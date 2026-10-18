//! Mega Man 8 (PAL) movement study: Mega Man's walk and jump rebuilt as Bevy
//! systems from measurements of the original (research/case-study-megaman8.md
//! in the psx repository).
//!
//! Built from the psx template's workspace: `src/main.rs` launches the game
//! through `engine::launcher`; the PS1 look, ticks, input and testing come
//! from the template's crate via `engine`.
pub mod player;
pub mod stage;

use bevy::prelude::*;
use engine::prelude::*;

/// Window title.
pub const TITLE: &str = "Mega Man 8 movement — arrows walk, X jumps";

/// The PAL release (SLES-01064).
pub const REGION: Region = Region::Pal;

/// The game logic runs once per PAL field (50 Hz): the per-tick 16.16 speeds
/// in `player::movement` are per field.
pub const FIELDS_PER_TICK: u32 = 1;

/// The PS1 screen size (and the window, at 3x). The template's default; not
/// yet checked against the original's display mode.
pub const SCREEN: UVec2 = UVec2::new(320, 240);

/// The logic tick rate for a region.
pub fn tick_rate(region: Region) -> TickRate {
    TickRate::every_nth_field(region, FIELDS_PER_TICK)
}

/// The whole game: the engine, then the stage and Mega Man.
pub struct GamePlugin;

impl Plugin for GamePlugin {
    fn build(&self, app: &mut App) {
        app.add_plugins((EnginePlugin, stage::StagePlugin, player::PlayerPlugin));
    }
}
