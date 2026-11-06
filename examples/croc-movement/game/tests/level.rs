//! The start level loads and places what it lists.
use game::world::Level;

#[test]
fn the_start_places_croc_on_its_floor() {
    let level = Level::start().unwrap();
    let placed = level.placements.iter().find(|p| p.kind == game::croc::PLACEMENT).expect("the start places Croc");
    let game = game::Game::start().unwrap();
    let croc = game.world.croc.expect("Croc");
    assert_eq!(croc.body.position, placed.position);
    assert_eq!(croc.body.facing, placed.facing);
    assert_eq!(croc.body.ground.height, level.floor.height);
}
