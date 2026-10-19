//! The engine: the reusable machinery this game runs on, cut along the lines
//! the original draws between data it interprets and code it runs per object
//! type. A different game should be able to use this crate without the
//! original's content and without this game's rules (`examples/sandbox.rs`
//! proves it). See `AGENTS.md` for what belongs here.
//!
//! It builds on the template's crate, [`psx_bevy`] (PS1 look, ticks, input,
//! controls and display menus, testing, developer MCP), re-exported here so
//! games depend on `engine` only.
//!
//! The skeleton is deliberately small. Rename and reshape [`schedule`],
//! [`objects`], [`triggers`] and [`level`] to match `docs/engine-model.md`:
//! they are starting points, not a design to fit the original into.
pub mod config;
pub mod level;
pub mod objects;
pub mod schedule;
pub mod triggers;

pub use psx_bevy;
#[cfg(feature = "dev-mcp")]
pub use psx_bevy::dev_mcp;
pub use psx_bevy::{
    controller, controls, display, launcher, model_viewer, pointer, psx, settings, shell, testing, viewer,
};

use bevy::prelude::*;

/// Schedule sets, the object type and action registries, trigger detection
/// and dispatch. Add it once, before the game's plugins.
pub struct EnginePlugin;

impl Plugin for EnginePlugin {
    fn build(&self, app: &mut App) {
        app.add_plugins((schedule::SchedulePlugin, objects::ObjectsPlugin, triggers::TriggersPlugin));
    }
}

/// What game code usually needs from the engine.
pub mod prelude {
    pub use crate::EnginePlugin;
    pub use crate::config::{TuningPlugin, load_ron};
    pub use crate::level::{LevelData, SpawnLevel};
    pub use crate::objects::{ObjectType, ObjectTypeAppExt, ObjectTypeId, ObjectTypeInfo, Placement};
    pub use crate::schedule::EngineSet;
    pub use crate::triggers::{ActionAppExt, ActionCall, ActionId, FireAction, TriggerActivator, TriggerVolume};
    pub use psx_bevy::prelude::*;
}
