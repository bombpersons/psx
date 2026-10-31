//! A character's body: where it is and faces, the floor under it, its
//! animation clock and its vertical motion. Nothing here is any one
//! character's: the player and anything else that stands, jumps or falls
//! carry [`Character`], and their controllers drive it.
//!
//! In the original these are the object's own fields and the object loop's
//! work (0x8002C790), run for every object before its script.
pub mod vertical;

use bevy::prelude::*;
use psx_bevy::prelude::*;
use serde::Deserialize;

use crate::config::{Tuning, TuningPlugin};
use crate::tick::TickPhase;
pub use vertical::{Landed, RiseHeld, Vertical, VerticalState};

/// Something with a body.
#[derive(Component, Reflect, Default)]
#[reflect(Component)]
#[require(Position, Facing, Vertical, Ground, Clip)]
pub struct Character;

/// Where a character is, in world units (y up; see [`crate::world`]).
#[derive(Component, Reflect, Default, Clone, Copy, Debug, PartialEq, Eq, Deref, DerefMut)]
#[reflect(Component)]
pub struct Position(pub IVec3);

/// An angle as the original keeps a facing: 4096 to the turn, wrapping.
pub type Heading = Angle<Exact, u16, 12>;

/// The way a character faces: 0 facing +z, growing to the left (the
/// object's +0x36A, 12 bits). Forward is (sin, cos) of it.
#[derive(Component, Reflect, Default, Clone, Copy, Debug, PartialEq)]
#[reflect(Component)]
pub struct Facing(#[reflect(ignore)] pub Heading);

impl Facing {
    /// A facing of `units` (4096 to the turn), kept to one turn.
    pub fn from_units(units: i32) -> Self {
        Facing(Heading::from_units(units as i64).normalized())
    }

    /// Its units, 0..4096.
    pub fn units(self) -> u16 {
        self.0.turns().bits()
    }

    /// Turned by `units`, kept to one turn as the original's `& 0xFFF` keeps it.
    pub fn turned(self, units: i32) -> Self {
        Facing((self.0 + Heading::from_units(units as i64)).normalized())
    }

    /// As a rotation about y, for rendering.
    pub fn to_rotation(self) -> Quat {
        Quat::from_rotation_y(self.0.to_radians_f32())
    }
}

/// The floor under a character, as collision last found it (object +0x38).
/// Collision runs at the end of a tick, so a tick's rules see last tick's
/// floor, as the original's do.
#[derive(Component, Reflect, Default, Clone, Copy, Debug)]
#[reflect(Component)]
pub struct Ground {
    pub height: i32,
}

impl Ground {
    /// How far above the floor a height is.
    pub fn clearance(&self, y: i32) -> i32 {
        y - self.height
    }
}

/// The animation playing, as far as the rules wait on it: its length in
/// frames and how far it is. The original advances it one frame a tick at
/// the start of the tick (0x8002C790) and ends it when the next frame would
/// be past the last.
#[derive(Component, Reflect, Default, Clone, Copy, Debug)]
#[reflect(Component)]
pub struct Clip {
    pub frames: u16,
    pub frame: u16,
    pub playing: bool,
}

impl Clip {
    pub fn play(frames: u16) -> Self {
        Clip { frames, frame: 0, playing: true }
    }

    pub fn stop(&mut self) {
        self.playing = false;
    }
}

/// Design numbers of a body, read from `assets/character/body.ron`.
#[derive(Resource, Deserialize, Clone, Debug)]
pub struct BodyTuning {
    /// Feet snap to a floor less than this far below them, and to any floor
    /// above them however high: Croc's script tests only `y - floor < 0x83`
    /// (script 0x1923). Which floor is a character's (so a high one cannot
    /// lift it) is collision's choice, not ported yet: the port has one flat
    /// floor.
    pub step: i32,
}

impl BodyTuning {
    pub const PATH: &str = "character/body.ron";
}

impl Tuning for BodyTuning {
    fn validate(&self) -> Result<(), String> {
        if self.step < 0 { Err("step: not below 0".into()) } else { Ok(()) }
    }
}

/// Characters' bodies: the animation clock, the vertical motion and the
/// feet following the floor.
pub struct CharacterPlugin;

impl Plugin for CharacterPlugin {
    fn build(&self, app: &mut App) {
        app.add_plugins((TuningPlugin::<BodyTuning>::new(BodyTuning::PATH), vertical::VerticalPlugin))
            .add_systems(FixedUpdate, (animate.in_set(TickPhase::Animate), follow_floor.in_set(TickPhase::Ground)));
    }
}

fn animate(mut q: Query<&mut Clip>) {
    for mut clip in &mut q {
        if !clip.playing {
            continue;
        }
        if clip.frame + 1 >= clip.frames {
            clip.playing = false;
        } else {
            clip.frame += 1;
        }
    }
}

/// Feet follow a floor less than `step` below them or anywhere above them
/// (see [`BodyTuning::step`]), even in a jump: Croc's script prelude (sub
/// 0x1923, every tick before its switch). Walking off a higher ledge into a
/// fall, the rest of the sub, has no ledge to happen on the flat floor and
/// is not ported.
fn follow_floor(tuning: Res<BodyTuning>, mut q: Query<(&mut Position, &Ground)>) {
    for (mut position, ground) in &mut q {
        if ground.clearance(position.y) < tuning.step {
            position.y = ground.height;
        }
    }
}
