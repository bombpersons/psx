//! Croc: Legend of the Gobbos (PAL) movement study: Croc's walk, turn and
//! jump rebuilt as Bevy systems from measurements of the original
//! (research/case-study-croc.md in the psx repository).
//!
//! Built from the psx-bevy template: `src/main.rs` opens the window and adds
//! [`GamePlugin`]; [`psx`], [`shell`], [`controller`], [`testing`], [`viewer`]
//! and `dev_mcp` are the template's modules.
pub mod controller;
pub mod croc;
#[cfg(feature = "dev-mcp")]
pub mod dev_mcp;
pub mod psx;
pub mod shell;
pub mod testing;
pub mod viewer;

use bevy::prelude::*;
use psx::Region;
use shell::TickRate;

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

/// The whole game.
pub struct GamePlugin;

impl Plugin for GamePlugin {
    fn build(&self, app: &mut App) {
        app.add_plugins(croc::CrocPlugin);
    }
}

/// What game code usually needs from the template.
pub mod prelude {
    pub use crate::controller::{Button, ControllerState};
    pub use crate::shell::{GameClock, GameSystems, TickRate};
}
