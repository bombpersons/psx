//! Croc, the player: a [`Character`] whose controller is the pad.
//!
//! In the original Croc is a script (his "strat") run by the object loop
//! 0x8002C790 once per logic tick, with the arithmetic in C opcodes (MOVE
//! 0x80043234, TURN 0x8004425C, JUMP 0x80028688). Here his abilities are
//! modules, each with its own state and steps, which [`crate::Game::tick`]
//! runs in the tick's order:
//!
//! - [`running`]: ground movement and turning
//! - [`jump`]: the jump and its landing
//! - [`stomp`]: the stomp
//!
//! The rise and fall they start are the body's ([`crate::character`]).
pub mod jump;
pub mod running;
pub mod stomp;

use psx_scenario::Parameters;
use serde::Deserialize;

use crate::character::{Character, Facing, Position};
use crate::observed::Observed;
use crate::world::{Floor, Level, Placement};
use jump::JumpState;
use running::{Gait, Motion};

/// Croc: his body, and the parts each ability keeps.
#[derive(Clone, Copy, Debug, Default)]
pub struct Croc {
    pub body: Character,
    pub action: Action,
    pub motion: Motion,
    pub jump: JumpState,
}

impl Croc {
    pub fn new(body: Character) -> Self {
        Croc { body, ..Default::default() }
    }
}

/// What Croc is doing: his script's global 0, which its main switch
/// (script 0x221B) dispatches on. Only the actions the port has are here.
#[derive(Default, Clone, Copy, Debug, PartialEq, Eq)]
pub enum ActionKind {
    /// Moving freely, on the ground or in a jump.
    #[default]
    Free,
    /// Stomping, at a step.
    Stomp(stomp::Stomp),
}

/// The action running, with its own progress, and a change to it. An
/// action started inside a tick (a stomp from a jump) runs from the next
/// one: the script's switch is taken once a tick, at [`Action::latch`].
#[derive(Default, Clone, Copy, Debug)]
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

    /// The action chosen last tick takes over.
    pub fn latch(&mut self) {
        if let Some(next) = self.next.take() {
            self.current = next;
        }
    }
}

/// The placement kind that is Croc.
pub const PLACEMENT: &str = "player";

/// The scenario kind of a flat floor with Croc on it.
pub const FLAT_FLOOR: &str = "flat_floor";

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

/// A `flat_floor` scenario's level: the floor and Croc's placement.
pub fn flat_floor(p: &Parameters) -> Result<Level, String> {
    let p: FlatFloor =
        serde_json::from_value(serde_json::Value::Object(p.clone())).map_err(|e| format!("flat_floor: {e}"))?;
    Ok(Level {
        floor: Floor { height: p.floor_y },
        placements: vec![Placement {
            kind: PLACEMENT.into(),
            position: Position { x: p.start_x, y: p.floor_y, z: p.start_z },
            facing: Facing::from_units(p.start_angle),
        }],
    })
}

/// Croc's quantities, as the glossary names them.
pub fn observe(croc: &Croc) -> Observed {
    let Croc { body, action, motion, .. } = croc;
    Observed {
        croc_x: Some(body.position.x),
        croc_y: Some(body.position.y),
        croc_z: Some(body.position.z),
        croc_angle: Some(body.facing.units()),
        croc_on_ground: Some(body.vertical.on_ground()),
        croc_vertical_speed: Some(body.vertical.speed),
        croc_jump_height_left: Some(body.vertical.height_left),
        // The glossary's numbers are the script's global 0.
        croc_action: Some(match action.upcoming() {
            ActionKind::Free => 1,
            ActionKind::Stomp(_) => 4,
        }),
        croc_speed_x: Some(motion.speed_x.bits()),
        croc_speed_z: Some(motion.speed_z.bits()),
        croc_turn_rate: Some(motion.turn_rate.bits()),
        croc_gait: Some(match motion.gait {
            Gait::Standing => glossary::Gait::Standing,
            Gait::Walking => glossary::Gait::Walking,
            Gait::Running => glossary::Gait::Running,
            Gait::Backing => glossary::Gait::Backing,
        }),
    }
}
