//! This game, as a library: `src/main.rs` launches it; tests in `tests/`
//! drive the same plugins headless (`engine::testing`).
//!
//! Layout: one plugin per game feature (player, stage, enemies, HUD...),
//! listed in [`GamePlugins`]. Per-type controllers register with the engine's
//! object type registry; rules plugins stay free of rendering so they run in
//! tests. Reusable machinery belongs in `engine/` (see its AGENTS.md).
//!
//! Mods are crates built on this library (see `mods/example`): keep what a
//! mod would reuse `pub`, and each feature plugin usable on its own.
pub mod demo;
pub mod disc;

use bevy::app::PluginGroupBuilder;
use bevy::prelude::*;
use engine::prelude::*;

/// Window title.
pub const TITLE: &str = "PSX game template — arrows, Z/X/C/S, Q/E, Enter";

/// The original game's video region. It sets the display rate and, with
/// [`FIELDS_PER_TICK`], the logic tick rate. Mega Man 8 and Croc (PAL) use
/// `Region::Pal`. `--region ntsc|pal` overrides it at run time.
pub const REGION: Region = Region::Ntsc;

/// Video fields per logic tick: 1 when the original's main loop runs every
/// vblank, 2 when it waits for two (Croc PAL: 25 Hz logic on a 50 Hz display).
pub const FIELDS_PER_TICK: u32 = 1;

/// The original's display size in pixels: the PS1 screen target and, at 3x,
/// the window. Most games show 320x240; check the original's display mode.
pub const SCREEN: UVec2 = UVec2::new(320, 240);

/// The logic tick rate for a region.
pub fn tick_rate(region: Region) -> TickRate {
    TickRate::every_nth_field(region, FIELDS_PER_TICK)
}

/// The whole game: the engine, then each feature's plugin. A group, so a
/// crate built on the game can drop or swap one feature:
/// `GamePlugins.build().disable::<demo::DemoPlugin>()`.
pub struct GamePlugins;

impl PluginGroup for GamePlugins {
    fn build(self) -> PluginGroupBuilder {
        PluginGroupBuilder::start::<Self>()
            .add(EnginePlugin)
            .add(disc::GameDataPlugin::default())
            .add(demo::player::PlayerPlugin)
            .add(demo::SpinnerPlugin)
            .add(demo::DemoPlugin)
    }
}
