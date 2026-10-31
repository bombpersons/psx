//! Croc, the player: a [`Character`] whose controller is the pad.
//!
//! In the original Croc is a script (his "strat") run by the object loop
//! 0x8002C790 once per logic tick, with the arithmetic in C opcodes (MOVE
//! 0x80043234, TURN 0x8004425C, JUMP 0x80028688). Here his abilities are
//! modules, each with its own plugin and system sets inside the tick's
//! phases ([`TickPhase`]); [`CrocPlugin`] adds them:
//!
//! - [`running`]: ground movement and turning (F-0001)
//! - [`jump`]: the jump and its landing (F-0002)
//! - [`stomp`]: the stomp (F-0003)
//!
//! The rise and fall they start are the body's ([`crate::character`]).
pub mod jump;
pub mod running;
pub mod stomp;

use bevy::prelude::*;
use psx_bevy::scenario::{Parameters, ScenarioApp};
use serde::Deserialize;

use crate::character::{Character, Facing, Position, Vertical};
use crate::observed::Observed;
use crate::tick::TickPhase;
use crate::world::{Floor, Level, Placement, PlacementApp};
use jump::JumpState;
use running::Motion;

/// Croc. The parts each ability keeps are its own components.
#[derive(Component, Reflect, Default)]
#[reflect(Component)]
#[require(Character, Name::new("Croc"), Action, Motion, JumpState)]
pub struct Croc;

/// What Croc is doing: his script's global 0, which its main switch
/// (script 0x221B) dispatches on. Only the actions the port has are here.
#[derive(Reflect, Default, Clone, Copy, Debug, PartialEq, Eq)]
pub enum ActionKind {
    /// Moving freely, on the ground or in a jump.
    #[default]
    Free,
    /// Stomping, at a step.
    Stomp(stomp::Stomp),
}

/// The action running, with its own progress, and a change to it. An
/// action started inside a tick (a stomp from a jump) runs from the next
/// one: the script's switch is taken once a tick, at [`TickPhase::Latch`].
#[derive(Component, Reflect, Default, Clone, Copy, Debug)]
#[reflect(Component)]
pub struct Action {
    pub current: ActionKind,
    /// The action that takes over at the next latch, if it changes.
    pub next: Option<ActionKind>,
}

impl Action {
    /// Whether this tick runs the free action's control.
    pub fn is_free(&self) -> bool {
        self.current == ActionKind::Free
    }

    /// Runs `kind` from the next tick.
    pub fn start(&mut self, kind: ActionKind) {
        self.next = Some(kind);
    }

    /// The action the next tick runs.
    pub fn upcoming(&self) -> ActionKind {
        self.next.unwrap_or(self.current)
    }
}

/// The placement kind that spawns Croc.
pub const PLACEMENT: &str = "player";

/// Croc and his abilities.
pub struct CrocPlugin;

impl Plugin for CrocPlugin {
    fn build(&self, app: &mut App) {
        app.add_plugins((running::RunningPlugin, jump::JumpPlugin, stomp::StompPlugin))
            .add_systems(FixedUpdate, latch.in_set(TickPhase::Latch))
            .register_placement(PLACEMENT, spawn)
            .register_scenario("flat_floor", flat_floor)
            .observe(|w: &mut World| {
                let Some((position, facing, vertical, action)) =
                    w.query_filtered::<(&Position, &Facing, &Vertical, &Action), With<Croc>>().iter(w).next()
                else {
                    return Observed::none();
                };
                Observed {
                    croc_x: Some(position.x),
                    croc_y: Some(position.y),
                    croc_z: Some(position.z),
                    croc_angle: Some(facing.units()),
                    croc_on_ground: Some(vertical.on_ground()),
                    croc_vertical_speed: Some(vertical.speed),
                    croc_jump_height_left: Some(vertical.height_left),
                    // The glossary's numbers are the script's global 0.
                    croc_action: Some(match action.upcoming() {
                        ActionKind::Free => 1,
                        ActionKind::Stomp(_) => 4,
                    }),
                    ..Observed::none()
                }
            });
    }
}

/// The parameters of a `flat_floor` scenario (`scenarios/*.toml`): a
/// floor, and Croc on it.
#[derive(Deserialize)]
struct FlatFloor {
    floor_y: i32,
    start_x: i32,
    start_z: i32,
    /// 4096 to the turn.
    start_angle: i32,
}

/// Scenarios of kind `flat_floor`: a level of the floor and Croc's placement.
fn flat_floor(app: &mut App, p: &Parameters) -> Result<(), String> {
    let p: FlatFloor =
        serde_json::from_value(serde_json::Value::Object(p.clone())).map_err(|e| format!("flat_floor: {e}"))?;
    let level = Level {
        floor: Floor { height: p.floor_y },
        placements: vec![Placement {
            kind: PLACEMENT.into(),
            position: IVec3::new(p.start_x, p.floor_y, p.start_z),
            facing: Facing::from_units(p.start_angle),
        }],
    };
    level.build(app.world_mut())
}

fn spawn(world: &mut World, placement: &Placement) -> Entity {
    world.spawn((Croc, Position(placement.position), placement.facing)).id()
}

fn latch(mut q: Query<&mut Action>) {
    for mut action in &mut q {
        if let Some(next) = action.next.take() {
            action.current = next;
        }
    }
}
