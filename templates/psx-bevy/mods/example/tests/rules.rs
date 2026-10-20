//! The mod's rules, headless: the game's plugins minus its scene, plus the
//! mod's, spawning the mod's own level.
use bevy::prelude::*;
use engine::objects::UnknownObjectTypes;
use engine::prelude::*;
use engine::testing::{TickApp, headless_app};
use example_mod::{Arena, Beacon, ModPlugin};
use game::demo::player::{Player, PlayerTuning};
use game::demo::{DemoPlugin, Spin};

fn app() -> App {
    engine::assets::add_root(example_mod::GAME_ASSETS);
    let mut app = headless_app(game::tick_rate(game::REGION));
    app.add_plugins((game::GamePlugins.build().disable::<DemoPlugin>(), ModPlugin));
    let arena: Arena = load_ron(Arena::PATH).unwrap();
    app.world_mut().commands().queue(SpawnLevel(arena.level));
    app.run_tick(&[]);
    app
}

#[test]
fn the_mods_level_spawns_every_type() {
    let mut app = app();
    let players = app.world_mut().query_filtered::<(), With<Player>>().iter(app.world()).count();
    let beacons = app.world_mut().query::<&Beacon>().iter(app.world()).count();
    assert_eq!((players, beacons), (1, 2));
    assert!(app.world().resource::<UnknownObjectTypes>().0.is_empty());
}

#[test]
fn the_replaced_spinner_turns_twice_as_fast() {
    let mut app = app();
    let spins: Vec<f32> = app.world_mut().query::<&Spin>().iter(app.world()).map(|s| s.0).collect();
    assert_eq!(spins, vec![2.0]);
}

#[test]
fn the_mods_tuning_file_replaces_the_games() {
    let app = app();
    assert_eq!(app.world().resource::<PlayerTuning>().walk_speed, 3.0);
}
