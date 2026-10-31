//! Croc: Legend of the Gobbos, ported: a library; `src/main.rs` launches
//! it, tests in `tests/` drive the same plugins headless
//! (`psx_bevy::testing`).
//!
//! Layout: [`LogicPlugins`] are the rules (no rendering, so they run in
//! tests): the tick's phases ([`tick`]), the world and its levels
//! ([`world`]), characters' bodies ([`character`]) and Croc, the player,
//! with his abilities ([`croc`]). [`scene`] and [`camera`] dress them.
pub mod camera;
pub mod character;
pub mod config;
pub mod croc;
pub mod disc;
pub mod scene;
pub mod tick;
pub mod world;

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
pub const TITLE: &str = "Croc: Legend of the Gobbos";

/// The original's video region: the PAL release (SLES-00593).
pub const REGION: Region = Region::Pal;

/// Video fields per logic tick: the main loop adds 128 per field to the
/// accumulator at 0x80075240 and runs a logic step per 256 (0x80013CA8),
/// so 25 steps a second on PAL's 50 fields.
pub const FIELDS_PER_TICK: u32 = 2;

/// The original's display size in pixels.
pub const SCREEN: UVec2 = UVec2::new(320, 240);

/// The logic tick rate for a region.
pub fn tick_rate(region: Region) -> TickRate {
    TickRate::every_nth_field(region, FIELDS_PER_TICK)
}

/// The game's rules: the tick, the world, bodies and Croc.
pub struct LogicPlugins;

impl PluginGroup for LogicPlugins {
    fn build(self) -> PluginGroupBuilder {
        PluginGroupBuilder::start::<Self>()
            .add(tick::TickPlugin)
            .add(world::WorldPlugin)
            .add(character::CharacterPlugin)
            .add(croc::CrocPlugin)
    }
}

/// The whole game's plugins. A group, so a test or sandbox can drop or swap
/// one.
pub struct GamePlugins;

impl PluginGroup for GamePlugins {
    fn build(self) -> PluginGroupBuilder {
        PluginGroupBuilder::start::<Self>()
            .add(disc::GameDataPlugin::default())
            .add_group(LogicPlugins)
            .add(scene::ScenePlugin)
    }
}
