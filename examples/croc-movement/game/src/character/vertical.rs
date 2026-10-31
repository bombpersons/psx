//! Rising and falling: the object loop's vertical motion (rise 0x8002D8D4,
//! fall 0x8002D810), run for every character each tick before its
//! controller, whatever put it in the air. Croc's jump starts a rise, his
//! stomp a fall at its own speed; walking off a ledge will start one too.
//!
//! Not ported: falling underwater (0x8002D824, level+0x6E == 1: v >> 4),
//! walking off a ledge into a fall (script 0x1956: no ledge on the flat
//! floor), and falling to death.
use bevy::prelude::*;
use serde::Deserialize;

use super::{Ground, Position};
use crate::config::{Tuning, TuningPlugin};
use crate::tick::TickPhase;

/// Design numbers of the rise and the fall, read from
/// `assets/character/vertical.ron`.
#[derive(Resource, Deserialize, Clone, Debug)]
pub struct VerticalTuning {
    /// With less height left than this the rise slows (700, 0x8002D8D4).
    pub slow_below: i32,
    /// Each slowing tick the rise loses this on top of a quarter of its
    /// speed (8, 0x8002D8D4).
    pub rise_drag: i32,
    /// With less height left than this, a rise no longer held halves each
    /// tick (1400, 0x8002D8D4).
    pub release_below: i32,
    /// The rise ends when less height than this is left (4, 0x8002D8D4)...
    pub apex_height: i32,
    /// ...or when it is slower than this (4, 0x8002D8D4).
    pub apex_speed: i32,
    /// Each tick the fall gains this on top of a quarter of its speed
    /// (8, 0x8002D810).
    pub fall_gain: i32,
    /// The fastest fall, world units per tick (0x240, 0x8002D810).
    pub max_fall: i32,
}

impl VerticalTuning {
    pub const PATH: &str = "character/vertical.ron";
}

impl Tuning for VerticalTuning {
    fn validate(&self) -> Result<(), String> {
        // A rise that cannot reach its apex, or a fall from the apex (speed
        // 0) that never speeds up, never ends.
        let positive = [
            ("apex_height", self.apex_height),
            ("apex_speed", self.apex_speed),
            ("fall_gain", self.fall_gain),
            ("max_fall", self.max_fall),
        ];
        if let Some((name, _)) = positive.iter().find(|(_, v)| *v <= 0) {
            return Err(format!("{name}: above 0"));
        }
        if self.rise_drag < 0 {
            return Err("rise_drag: not below 0".into());
        }
        Ok(())
    }
}

/// Where a body is vertically: the object's flags +0x174 (0x200 rising,
/// 0x400 falling) and +0x178 bit 1 (standing). Neither rising nor falling
/// and off the ground is a hang (Croc's stomp's pause in mid-air).
#[derive(Reflect, Default, Clone, Copy, Debug, PartialEq, Eq)]
pub enum VerticalState {
    #[default]
    Grounded,
    Rising,
    Falling,
    Hanging,
}

/// A body's vertical motion. The speed and the height left are kept across
/// states, as the original keeps its fields (+0x150, +0x14C): a hang keeps
/// the speed it stopped at, and a drop from it the height left of the jump.
#[derive(Component, Reflect, Default, Clone, Copy, Debug)]
#[reflect(Component)]
#[require(RiseHeld)]
pub struct Vertical {
    pub state: VerticalState,
    /// World units per tick: upward while rising, downward while falling.
    pub speed: i32,
    /// How much higher the rise may still go, world units.
    pub height_left: i32,
}

impl Vertical {
    pub fn on_ground(&self) -> bool {
        self.state == VerticalState::Grounded
    }

    /// Rising or falling. A hang is neither, so this is not "off the
    /// ground": that is `!on_ground()`.
    pub fn rising_or_falling(&self) -> bool {
        matches!(self.state, VerticalState::Rising | VerticalState::Falling)
    }

    /// Opcode 0x22, JUMP (0x80028688): rise from the next tick, at most
    /// `height` higher, at `speed` (+0x14C = height, store 0x800286B0;
    /// +0x150 = speed, store 0x800286B8; rising set, falling and standing
    /// cleared). y does not move this tick.
    pub fn jump(&mut self, height: i32, speed: i32) {
        self.height_left = height;
        self.speed = speed;
        self.state = VerticalState::Rising;
    }

    /// Opcode 0x76 (0x80028734): neither rising nor falling, and off the
    /// ground: the body hangs where it is.
    pub fn hang(&mut self) {
        self.state = VerticalState::Hanging;
    }

    /// Opcode 0x73 (0x800286F4): fall from here at `speed`.
    pub fn fall(&mut self, speed: i32) {
        self.state = VerticalState::Falling;
        self.speed = speed;
    }
}

/// Whether what lifts a rising body still pushes. Let go, a rise near its
/// top is cut short. The controller sets it in [`TickPhase::Input`]; a body
/// with no controller keeps it held, so its rise is never cut.
#[derive(Component, Reflect, Clone, Copy, Debug)]
#[reflect(Component)]
pub struct RiseHeld(pub bool);

impl Default for RiseHeld {
    fn default() -> Self {
        RiseHeld(true)
    }
}

/// The body landed this tick (+0x174 bit 0x1000): present from the fall
/// that lands until the next tick's physics (0x8002CCD0 clears it), so
/// [`TickPhase::Reactions`] can handle it.
#[derive(Component, Reflect, Default, Clone, Copy, Debug)]
#[reflect(Component)]
pub struct Landed;

pub(super) struct VerticalPlugin;

impl Plugin for VerticalPlugin {
    fn build(&self, app: &mut App) {
        app.add_plugins(TuningPlugin::<VerticalTuning>::new(VerticalTuning::PATH))
            .add_systems(FixedUpdate, move_vertically.in_set(TickPhase::Physics));
    }
}

/// Rising or falling, each tick before the controller: one or the other,
/// the fall tested first (0x8002CCF0..0x8002CD24), so a rise that peaks
/// this tick falls from the next.
fn move_vertically(
    mut commands: Commands,
    tuning: Res<VerticalTuning>,
    landed: Query<Entity, With<Landed>>,
    mut q: Query<(Entity, &mut Vertical, &mut Position, &Ground, &RiseHeld)>,
) {
    for entity in &landed {
        commands.entity(entity).remove::<Landed>();
    }
    for (entity, mut v, mut position, ground, held) in &mut q {
        match v.state {
            VerticalState::Falling => {
                if fall(&tuning, &mut v, &mut position, ground) {
                    commands.entity(entity).insert(Landed);
                }
            }
            VerticalState::Rising => rise(&tuning, held.0, &mut v, &mut position),
            VerticalState::Grounded | VerticalState::Hanging => {}
        }
    }
}

/// The rise (0x8002D8D4).
fn rise(tuning: &VerticalTuning, held: bool, v: &mut Vertical, position: &mut Position) {
    position.y += v.speed;
    v.height_left -= v.speed;
    if v.height_left < tuning.slow_below {
        v.speed = v.speed - tuning.rise_drag - (v.speed >> 2);
    }
    if v.height_left < tuning.release_below && !held {
        v.speed -= v.speed >> 1;
    }
    if v.height_left < tuning.apex_height || v.speed < tuning.apex_speed {
        v.height_left = 0;
        v.speed = 0;
        v.state = VerticalState::Falling;
    }
}

/// The fall (0x8002D810): whether it landed, the feet reaching the floor.
fn fall(tuning: &VerticalTuning, v: &mut Vertical, position: &mut Position, ground: &Ground) -> bool {
    position.y -= v.speed;
    v.speed = (v.speed + tuning.fall_gain + (v.speed >> 2)).min(tuning.max_fall);
    if position.y > ground.height {
        return false;
    }
    position.y = ground.height;
    v.state = VerticalState::Grounded;
    v.speed = 0;
    true
}
