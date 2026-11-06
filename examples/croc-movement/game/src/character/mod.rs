//! A character's body: where it is and faces, the floor under it, its
//! animation clock and its vertical motion. Nothing here is any one
//! character's: the player and anything else that stands, jumps or falls
//! has a [`Character`], and its controller drives it.
//!
//! In the original these are the object's own fields and the object loop's
//! work (0x8002C790), run for every object before its script.
pub mod vertical;

use psx_fixed::{Angle, Exact};
use serde::Deserialize;

use crate::config::Tuning;
pub use vertical::{Vertical, VerticalState};

/// A body.
#[derive(Clone, Copy, Debug)]
pub struct Character {
    pub position: Position,
    pub facing: Facing,
    pub vertical: Vertical,
    pub ground: Ground,
    pub clip: Clip,
    /// Whether what lifts a rising body still pushes. Let go, a rise near
    /// its top is cut short. The controller sets it each tick; a body with
    /// no controller keeps it held, so its rise is never cut.
    pub rise_held: bool,
    /// The body landed this tick (+0x174 bit 0x1000): set by the fall that
    /// lands and cleared by the next tick's physics (0x8002CCD0), so the
    /// tick's reactions can handle it.
    pub landed: bool,
}

impl Default for Character {
    fn default() -> Self {
        Character {
            position: Position::default(),
            facing: Facing::default(),
            vertical: Vertical::default(),
            ground: Ground::default(),
            clip: Clip::default(),
            rise_held: true,
            landed: false,
        }
    }
}

/// Where a character is, in world units (y up; see [`crate::world`]).
#[derive(Deserialize, Default, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(from = "[i32; 3]")]
pub struct Position {
    pub x: i32,
    pub y: i32,
    pub z: i32,
}

impl From<[i32; 3]> for Position {
    fn from([x, y, z]: [i32; 3]) -> Self {
        Position { x, y, z }
    }
}

/// An angle as the original keeps a facing: 4096 to the turn, wrapping.
pub type Heading = Angle<Exact, u16, 12>;

/// The way a character faces: 0 facing +z, growing to the left (the
/// object's +0x36A, 12 bits). Forward is (sin, cos) of it.
#[derive(Default, Clone, Copy, Debug, PartialEq)]
pub struct Facing(pub Heading);

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

    /// As radians about y, for drawing.
    pub fn radians(self) -> f32 {
        self.0.to_radians_f32()
    }
}

/// The floor under a character, as collision last found it (object +0x38).
/// Collision runs at the end of a tick, so a tick's rules see last tick's
/// floor, as the original's do.
#[derive(Default, Clone, Copy, Debug)]
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
#[derive(Default, Clone, Copy, Debug)]
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

    /// One tick of the animation.
    pub fn advance(&mut self) {
        if !self.playing {
            return;
        }
        if self.frame + 1 >= self.frames {
            self.playing = false;
        } else {
            self.frame += 1;
        }
    }
}

/// Design numbers of a body, read from `assets/character/body.ron`.
#[derive(Deserialize, Clone, Debug)]
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

/// Feet follow a floor less than `step` below them or anywhere above them
/// (see [`BodyTuning::step`]), even in a jump: Croc's script prelude (sub
/// 0x1923, every tick before its switch). Walking off a higher ledge into a
/// fall, the rest of the sub, has no ledge to happen on the flat floor and
/// is not ported.
pub fn follow_floor(tuning: &BodyTuning, body: &mut Character) {
    if body.ground.clearance(body.position.y) < tuning.step {
        body.position.y = body.ground.height;
    }
}
