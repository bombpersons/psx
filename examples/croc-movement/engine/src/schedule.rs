//! The order work happens in within one logic tick.
//!
//! Game systems join these sets instead of ordering themselves ad hoc, so the
//! port keeps the original's update order. The four sets below are
//! placeholders: once `docs/engine-model.md` records how the original runs a
//! frame (which loops, in what order, what happens per field and per tick),
//! rename and reorder them to match, and cite the evidence here.
use bevy::prelude::*;
use psx_bevy::shell::GameSystems;

/// Phases of one logic tick, run in this order inside [`GameSystems::Game`].
#[derive(SystemSet, Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EngineSet {
    /// Per-type control code that reads the controller (usually the player).
    Control,
    /// Per-type update code for every other active object.
    Objects,
    /// Trigger detection, then action handlers.
    Triggers,
    /// Camera logic, when the original runs it per tick.
    Camera,
}

pub struct SchedulePlugin;

impl Plugin for SchedulePlugin {
    fn build(&self, app: &mut App) {
        app.configure_sets(
            FixedUpdate,
            (EngineSet::Control, EngineSet::Objects, EngineSet::Triggers, EngineSet::Camera)
                .chain()
                .in_set(GameSystems::Game),
        );
    }
}
