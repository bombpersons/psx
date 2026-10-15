//! Mega Man 8 (PAL) movement study: Mega Man's walk and jump rebuilt as Bevy
//! systems from measurements of the original (research/case-study-megaman8.md
//! in the psx repository).
//!
//! Built from the psx-bevy template: `src/main.rs` opens the window and adds
//! [`GamePlugin`]; [`psx`], [`shell`], [`controller`], [`testing`], [`viewer`],
//! [`model_viewer`] (`--model`) and `dev_mcp` are the template's modules.
pub mod controller;
#[cfg(feature = "dev-mcp")]
pub mod dev_mcp;
pub mod model_viewer;
pub mod player;
pub mod psx;
pub mod shell;
pub mod stage;
pub mod testing;
pub mod viewer;

use bevy::prelude::*;
use psx::Region;
use shell::TickRate;

/// Window title.
pub const TITLE: &str = "Mega Man 8 movement — arrows walk, X jumps";

/// The PAL release (SLES-01064).
pub const REGION: Region = Region::Pal;

/// The game logic runs once per PAL field (50 Hz): the per-tick 16.16 speeds
/// in `player::movement` are per field.
pub const FIELDS_PER_TICK: u32 = 1;

/// The logic tick rate for a region.
pub fn tick_rate(region: Region) -> TickRate {
    TickRate::every_nth_field(region, FIELDS_PER_TICK)
}

/// The whole game.
pub struct GamePlugin;

impl Plugin for GamePlugin {
    fn build(&self, app: &mut App) {
        app.add_plugins((stage::StagePlugin, player::PlayerPlugin));
    }
}

/// What game code usually needs from the template.
pub mod prelude {
    pub use crate::controller::{Button, ControllerState};
    pub use crate::shell::{GameClock, GameSystems, TickRate};
}
