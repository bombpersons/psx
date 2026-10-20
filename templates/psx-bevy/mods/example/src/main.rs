//! The mod's executable: the game's launcher with the game's feature plugins
//! minus its demo scene, plus the mod's own (`src/lib.rs`).
use bevy::app::PluginGroup;
use engine::launcher::{self, Game};
use game::demo::DemoPlugin;

fn main() {
    // The game's assets, layered under this crate's own assets/ (which wins).
    engine::assets::add_root(example_mod::GAME_ASSETS);
    launcher::run(Game {
        name: env!("CARGO_PKG_NAME"),
        title: "Example mod — arrows, Z/X, Q/E",
        region: game::REGION,
        fields_per_tick: game::FIELDS_PER_TICK,
        screen: game::SCREEN,
        add: |app| {
            app.add_plugins((
                game::GamePlugins.build().disable::<DemoPlugin>(),
                example_mod::ModPlugin,
                example_mod::ScenePlugin,
            ));
        },
    });
}
