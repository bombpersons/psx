//! Level data in the engine's own format: what a level places and which
//! triggers it has. Importers in `tools/` write it from the original's
//! tables; people can write it by hand (RON).
//!
//! Extend it with what the original's levels actually hold (geometry,
//! collision, rooms, cameras), keeping units normal (metres, radians) and
//! conversions in the importer.
use crate::objects::{Placement, spawn_placement};
use crate::triggers::{ActionCall, TriggerVolume};
use bevy::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct LevelData {
    #[serde(default)]
    pub objects: Vec<Placement>,
    #[serde(default)]
    pub triggers: Vec<TriggerPlacement>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TriggerPlacement {
    pub position: Vec3,
    pub half_extents: Vec3,
    pub call: ActionCall,
    #[serde(default)]
    pub once: bool,
}

/// A command that spawns a level's objects (through the type registry) and
/// trigger volumes: `commands.queue(SpawnLevel(level))`.
pub struct SpawnLevel(pub LevelData);

impl Command for SpawnLevel {
    type Out = ();

    fn apply(self, world: &mut World) {
        for placement in &self.0.objects {
            spawn_placement(world, placement);
        }
        for trigger in self.0.triggers {
            world.spawn((
                Name::new("Trigger"),
                Transform::from_translation(trigger.position),
                TriggerVolume {
                    half_extents: trigger.half_extents,
                    call: trigger.call,
                    once: trigger.once,
                    inside: false,
                    fired: false,
                },
            ));
        }
    }
}
