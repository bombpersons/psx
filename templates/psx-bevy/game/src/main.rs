//! The executable. Window, renderer and command line come from the engine's
//! launcher (`engine::launcher`, see its docs for the flags); the game itself
//! is the library in `src/lib.rs` (crate `game`).
use bevy::app::PluginGroup;
use engine::launcher::{self, Game};

fn main() {
    launcher::run(Game {
        name: env!("CARGO_PKG_NAME"),
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
