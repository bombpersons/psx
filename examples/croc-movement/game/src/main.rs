//! The executable. Window, renderer and command line come from psx-bevy's
//! launcher (`psx_bevy::launcher`, see its docs for the flags); the game
//! itself is the library in `src/lib.rs` (crate `game`).
use bevy::app::PluginGroup;
use psx_bevy::launcher::{self, Game};

fn main() {
    // Started outside cargo (an MCP client), a development build reads
    // this crate's assets/.
    #[cfg(debug_assertions)]
    psx_bevy::assets::development_base(env!("CARGO_MANIFEST_DIR"));
    launcher::run(Game {
        name: env!("CARGO_BIN_NAME"),
        title: game::TITLE,
        region: game::REGION,
        fields_per_tick: game::FIELDS_PER_TICK,
        screen: game::SCREEN,
        add: |app| {
            // Imports the game's data from the player's disc on first start.
            app.add_plugins(game::GamePlugins.set(game::disc::GameDataPlugin::from_args()));
        },
    });
}
