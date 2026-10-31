//! The world: levels (`assets/levels/*.ron`), what they place, and the
//! floor collision finds under each character.
//!
//! World units are the original's: integers, y up, about 256 to the metre
//! (Croc stands about 300 tall). Rules work in them; [`to_metres`] is for
//! rendering.
//!
//! For now a level's geometry is a stand-in the features are tested in: a
//! flat floor at one height (the original's levels are a feature of their
//! own). Collision gives every character the floor under it at the end of
//! each tick, as the original's (0x8002D410 -> 0x80031F64, writing the
//! object's +0x38) does for the next tick's rules.
use bevy::ecs::system::SystemParam;
use bevy::prelude::*;
use serde::Deserialize;
use std::collections::HashMap;

use crate::character::{Facing, Ground, Heading, Position};
use crate::config::load_ron;
use crate::disc::GameData;
use crate::tick::TickPhase;

/// Metres per world unit, for rendering only.
pub const METRES_PER_UNIT: f32 = 1.0 / 256.0;

/// A world position in metres.
pub fn to_metres(p: IVec3) -> Vec3 {
    p.as_vec3() * METRES_PER_UNIT
}

/// A level: its floor and what it places on it.
#[derive(Deserialize, Clone, Debug)]
pub struct Level {
    pub floor: Floor,
    pub placements: Vec<Placement>,
}

/// Something a level places, spawned by the function registered for its
/// kind ([`PlacementApp::register_placement`]).
#[derive(Deserialize, Clone, Debug)]
pub struct Placement {
    pub kind: String,
    /// World units.
    pub position: IVec3,
    /// Degrees, in whole 1/4096 turns.
    #[serde(default, deserialize_with = "facing_degrees")]
    pub facing: Facing,
}

fn facing_degrees<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Facing, D::Error> {
    Heading::deserialize(d).map(|h| Facing(h.normalized()))
}

/// A flat floor at a height, world units: the level's geometry until real
/// levels are imported.
#[derive(Component, Deserialize, Reflect, Clone, Copy, Debug)]
#[reflect(Component)]
pub struct Floor {
    pub height: i32,
}

impl Floor {
    /// The floor's height under `_at`, if it is under it (a flat floor is
    /// under everything).
    pub fn height_under(&self, _at: IVec3) -> Option<i32> {
        Some(self.height)
    }
}

/// The world's floors, as collision asks them.
#[derive(SystemParam)]
pub struct Floors<'w, 's> {
    floors: Query<'w, 's, &'static Floor>,
}

impl Floors<'_, '_> {
    /// The floor under a point: the highest under it. Which floor is a
    /// character's when several are (so a high one cannot lift it) is not
    /// ported: there is one flat floor.
    pub fn height_under(&self, at: IVec3) -> Option<i32> {
        self.floors.iter().filter_map(|f| f.height_under(at)).max()
    }
}

/// Spawns a placement of a kind.
pub type Spawn = fn(&mut World, &Placement) -> Entity;

/// What each placement kind spawns.
#[derive(Resource, Default)]
pub struct PlacementKinds(HashMap<String, Spawn>);

pub trait PlacementApp {
    /// Placements of `kind` are spawned by `spawn`.
    fn register_placement(&mut self, kind: &str, spawn: Spawn) -> &mut Self;
}

impl PlacementApp for App {
    fn register_placement(&mut self, kind: &str, spawn: Spawn) -> &mut Self {
        let mut kinds = self.world_mut().get_resource_or_insert_with(PlacementKinds::default);
        assert!(kinds.0.insert(kind.to_string(), spawn).is_none(), "placement kind {kind} registered twice");
        self
    }
}

impl Level {
    /// The game's start: 1-1's start, flat.
    pub const START: &str = "levels/start.ron";

    /// The game's start, read as the game reads it (layered asset folders
    /// included).
    pub fn start() -> Result<Level, String> {
        load_ron(Self::START)
    }

    /// Builds it: the floor, then each placement. Fails on a placement of
    /// a kind nothing registered.
    pub fn build(&self, world: &mut World) -> Result<(), String> {
        world.spawn((Name::new("Floor"), self.floor));
        for placement in &self.placements {
            let spawn = world
                .get_resource::<PlacementKinds>()
                .and_then(|kinds| kinds.0.get(&placement.kind).copied())
                .ok_or_else(|| format!("placement kind {}: nothing registered", placement.kind))?;
            spawn(world, placement);
        }
        Ok(())
    }
}

pub struct WorldPlugin;

impl Plugin for WorldPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<PlacementKinds>()
            .add_systems(FixedUpdate, collide.in_set(TickPhase::Collide))
            .add_observer(find_floor)
            .add_systems(OnEnter(GameData::Ready), start);
    }
}

fn start(world: &mut World) {
    let level = Level::start().unwrap_or_else(|e| panic!("start level: {e}"));
    level.build(world).unwrap_or_else(|e| panic!("start level: {e}"));
}

/// A body placed in the world finds the floor under it at once, as
/// collision would at the end of a tick: its first tick's rules see it.
fn find_floor(add: On<Add<Ground>>, floors: Floors, mut q: Query<(&Position, &mut Ground)>) {
    if let Ok((position, mut ground)) = q.get_mut(add.entity)
        && let Some(height) = floors.height_under(position.0)
    {
        ground.height = height;
    }
}

fn collide(floors: Floors, mut q: Query<(&Position, &mut Ground)>) {
    for (position, mut ground) in &mut q {
        if let Some(height) = floors.height_under(position.0) {
            ground.height = height;
        }
    }
}
