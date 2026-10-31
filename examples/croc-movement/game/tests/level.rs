//! The start level loads and places what it lists, headless.
use bevy::prelude::*;
use game::character::{Facing, Ground, Position};
use game::croc::Croc;
use game::world::Level;
use psx_bevy::testing::headless_app;

#[test]
fn the_start_places_croc_on_its_floor() {
    let level = Level::start().unwrap();
    let placed = level.placements.iter().find(|p| p.kind == game::croc::PLACEMENT).expect("the start places Croc");
    let mut app = headless_app(game::tick_rate(game::REGION));
    app.add_plugins((game::disc::GameDataPlugin::default(), game::LogicPlugins));
    app.update();
    let world = app.world_mut();
    let (position, facing, ground) =
        world.query_filtered::<(&Position, &Facing, &Ground), With<Croc>>().single(world).expect("one Croc");
    assert_eq!(position.0, placed.position);
    assert_eq!(*facing, placed.facing);
    assert_eq!(ground.height, level.floor.height);
}
