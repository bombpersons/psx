//! The port, as a library: `src/main.rs` launches it; tests in `tests/`
//! drive the same plugins headless (`psx_bevy::testing`).
//!
//! Layout: one plugin per catalogue feature, named after it and listed in
//! [`GamePlugins`]. Logic stays free of rendering so it runs in tests.
pub mod config;
pub mod demo;
pub mod disc;

/// `Observed`: the glossary's quantities as the port has them, which the
/// port fills for the scenarios (`psx_bevy::scenario`).
/// Generated from `glossary/src/lib.rs` by `build.rs`.
pub mod observed {
    include!(concat!(env!("OUT_DIR"), "/observed.rs"));
}

use bevy::app::PluginGroupBuilder;
use bevy::prelude::*;
use psx_bevy::prelude::*;

/// Window title.
pub const TITLE: &str = "PSX game template — arrows, Z/X/C/S, Q/E, Enter";

/// The original game's video region. It sets the display rate and, with
/// [`FIELDS_PER_TICK`], the logic tick rate. A European release uses
/// `Region::Pal`. `--region ntsc|pal` overrides it at run time.
pub const REGION: Region = Region::Ntsc;

/// Video fields per logic tick: 1 when the original's main loop runs every
/// vblank, 2 when it waits for two (PAL: 25 Hz logic on a 50 Hz display).
pub const FIELDS_PER_TICK: u32 = 1;

/// The original's display size in pixels: the PS1 screen target and, at 3x,
/// the window. Most games show 320x240; check the original's display mode.
pub const SCREEN: UVec2 = UVec2::new(320, 240);

/// The logic tick rate for a region.
pub fn tick_rate(region: Region) -> TickRate {
    TickRate::every_nth_field(region, FIELDS_PER_TICK)
}

/// The whole game's plugins. A group, so a test or sandbox can drop or swap
/// one: `GamePlugins.build().disable::<demo::DemoPlugin>()`.
pub struct GamePlugins;

impl PluginGroup for GamePlugins {
    fn build(self) -> PluginGroupBuilder {
        PluginGroupBuilder::start::<Self>()
            .add(disc::GameDataPlugin::default())
            .add(demo::player::PlayerPlugin)
            .add(demo::spinner::SpinnerPlugin)
            .add(demo::DemoPlugin)
    }
}
