//! Croc: Legend of the Gobbos (PAL) movement study: Croc's walk, turn and
//! jump rebuilt as Bevy systems from measurements of the original
//! (research/case-study-croc.md in the psx repository).
//!
//! Built from the psx template's workspace: `src/main.rs` launches the game
//! through `engine::launcher`; the PS1 look, ticks, input and testing come
//! from the template's crate via `engine`.
pub mod croc;

use bevy::prelude::*;
use engine::prelude::*;

/// Window title.
pub const TITLE: &str = "Croc movement — arrows walk/turn, X jump, Q/E sidestep (air: turn), S toggles PS1 look";

/// The PAL release (SLES-00593).
pub const REGION: Region = Region::Pal;

/// Croc's logic runs once per two vblanks (25 Hz on PAL) and samples the pad
/// at that rate too, so button edges match the original's.
pub const FIELDS_PER_TICK: u32 = croc::movement::VBLANKS_PER_TICK;

/// The logic tick rate for a region.
pub fn tick_rate(region: Region) -> TickRate {
    TickRate::every_nth_field(region, FIELDS_PER_TICK)
}

/// The whole game: the engine, then Croc.
pub struct GamePlugin;

impl Plugin for GamePlugin {
    fn build(&self, app: &mut App) {
        app.add_plugins((EnginePlugin, croc::CrocPlugin));
    }
}
