//! The world: levels (`assets/levels/*.ron`), what they place, and the
//! floor collision finds under each character.
//!
//! World units are the original's: integers, y up, about 256 to the metre
//! (Croc stands about 300 tall). Rules work in them; [`METRES_PER_UNIT`] is
//! for drawing.
//!
//! For now a level's geometry is a stand-in the features are tested in: a
//! flat floor at one height (the original's levels are a feature of their
//! own). Collision gives every character the floor under it at the end of
//! each tick, as the original's (0x8002D410 -> 0x80031F64, writing the
//! object's +0x38) does for the next tick's rules.
use serde::Deserialize;

use crate::character::{Character, Facing, Heading, Position};
use crate::config::load_ron;
use crate::croc::{self, Croc};

/// Metres per world unit, for drawing only.
pub const METRES_PER_UNIT: f32 = 1.0 / 256.0;

/// What is in the world: its floors and the characters on them.
#[derive(Clone, Debug, Default)]
pub struct World {
    pub floors: Vec<Floor>,
    pub croc: Option<Croc>,
}

/// A level: its floor and what it places on it.
#[derive(Deserialize, Clone, Debug)]
pub struct Level {
    pub floor: Floor,
    pub placements: Vec<Placement>,
}

/// Something a level places, spawned by its kind ([`Level::build`]).
#[derive(Deserialize, Clone, Debug)]
pub struct Placement {
    pub kind: String,
    /// World units.
    pub position: Position,
    /// Degrees, in whole 1/4096 turns.
    #[serde(default, deserialize_with = "facing_degrees")]
    pub facing: Facing,
}

fn facing_degrees<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Facing, D::Error> {
    Heading::deserialize(d).map(|h| Facing(h.normalized()))
}

/// A flat floor at a height, world units: the level's geometry until real
/// levels are imported.
#[derive(Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub struct Floor {
    pub height: i32,
}

impl Floor {
    /// The floor's height under `_at`, if it is under it (a flat floor is
    /// under everything).
    pub fn height_under(&self, _at: Position) -> Option<i32> {
        Some(self.height)
    }
}

/// The floor under a point: the highest under it. Which floor is a
/// character's when several are (so a high one cannot lift it) is not
/// ported: there is one flat floor.
pub fn height_under(floors: &[Floor], at: Position) -> Option<i32> {
    floors.iter().filter_map(|f| f.height_under(at)).max()
}

impl Level {
    /// The game's start: 1-1's start, flat.
    pub const START: &str = "levels/start.ron";

    /// The game's start, read as the game reads it.
    pub fn start() -> Result<Level, String> {
        load_ron(Self::START)
    }

    /// Builds it: the floor, then each placement. Fails on a placement of
    /// a kind the port does not have.
    pub fn build(&self) -> Result<World, String> {
        let mut world = World { floors: vec![self.floor], croc: None };
        for placement in &self.placements {
            match placement.kind.as_str() {
                croc::PLACEMENT => world.croc = Some(Croc::new(placed(&world.floors, placement))),
                kind => return Err(format!("placement kind {kind}: the port has none")),
            }
        }
        Ok(world)
    }
}

/// A body placed in the world finds the floor under it at once, as
/// collision would at the end of a tick: its first tick's rules see it.
fn placed(floors: &[Floor], placement: &Placement) -> Character {
    let mut body = Character { position: placement.position, facing: placement.facing, ..Default::default() };
    collide(floors, &mut body);
    body
}

/// Collision: the floor under a character, for the next tick's rules.
pub fn collide(floors: &[Floor], body: &mut Character) {
    if let Some(height) = height_under(floors, body.position) {
        body.ground.height = height;
    }
}
