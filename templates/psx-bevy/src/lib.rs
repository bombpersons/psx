//! The game, as a library: `src/main.rs` opens the window and adds
//! [`GamePlugin`]; tests in `tests/` drive the same plugins headless
//! ([`testing`]).
//!
//! Layout: one plugin per game feature (player, stage, enemies, HUD...),
//! listed in [`GamePlugin`]. Rules plugins stay free of rendering so they run
//! in tests. The template's own modules are [`psx`] (the PS1 look),
//! [`shell`] (logic ticks and input), [`controller`], [`viewer`] (the
//! `--scene` capture viewer), [`model_viewer`] (`--model`) and, with the `dev-mcp` feature, `dev_mcp`.
pub mod controller;
pub mod controls;
pub mod demo;
#[cfg(feature = "dev-mcp")]
pub mod dev_mcp;
pub mod model_viewer;
pub mod psx;
pub mod shell;
pub mod testing;
pub mod viewer;

use bevy::prelude::*;
use psx::Region;
use shell::TickRate;

/// Window title.
pub const TITLE: &str = "PSX game template — arrows, Z/X/C/S, Q/E, Enter";

/// The original game's video region. It sets the display rate and, with
/// [`FIELDS_PER_TICK`], the logic tick rate. Mega Man 8 and Croc (PAL) use
/// `Region::Pal`. `--region ntsc|pal` overrides it at run time.
pub const REGION: Region = Region::Ntsc;

/// Video fields per logic tick: 1 when the original's main loop runs every
/// vblank, 2 when it waits for two (Croc PAL: 25 Hz logic on a 50 Hz display).
pub const FIELDS_PER_TICK: u32 = 1;

/// The logic tick rate for a region.
pub fn tick_rate(region: Region) -> TickRate {
    TickRate::every_nth_field(region, FIELDS_PER_TICK)
}

/// The whole game: add each feature's plugin here.
pub struct GamePlugin;

impl Plugin for GamePlugin {
    fn build(&self, app: &mut App) {
        app.add_plugins(demo::DemoPlugin);
    }
}

/// What game code usually needs from the template.
pub mod prelude {
    pub use crate::controller::{Button, ControllerState};
    pub use crate::shell::{GameClock, GameSystems, TickRate};
}
