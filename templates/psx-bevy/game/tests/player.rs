//! Gameplay tests: run the demo's rules headless for exact ticks with scripted
//! buttons. For your game, compare rules against recordings of the original
//! with the case runner (`tests/cases.rs`).
use bevy::prelude::*;
use game::demo::player::{GROUND_Y, Hop, Player, PlayerPlugin, PlayerTuning};
use psx_bevy::controller::Button;
use psx_bevy::testing::{TickApp, headless_app};

fn app() -> (App, Entity) {
    let mut app = headless_app(game::tick_rate(game::REGION));
    app.add_plugins(PlayerPlugin);
    let player = app.world_mut().spawn(Player).id();
    (app, player)
}

fn tuning(app: &App) -> PlayerTuning {
    app.world().resource::<PlayerTuning>().clone()
}

fn position(app: &App, player: Entity) -> Vec3 {
    app.world().get::<Transform>(player).unwrap().translation
}

#[test]
fn walks_at_walk_speed() {
    let (mut app, player) = app();
    let ticks = 30;
    app.run_ticks(ticks, &[Button::Right]);
    let seconds = ticks as f32 / game::tick_rate(game::REGION).hz as f32;
    assert!((position(&app, player).x - tuning(&app).walk_speed * seconds).abs() < 1e-3);
}

#[test]
fn a_one_tick_tap_hops_once() {
    let (mut app, player) = app();
    app.run_tick(&[Button::Cross]);
    app.run_ticks(5, &[]);
    assert!(matches!(app.world().get::<Hop>(player), Some(Hop::Airborne { .. })));

    // Apex of a ballistic hop: v²/2g above the floor, within one tick's motion.
    let mut apex = GROUND_Y;
    let mut airborne_ticks = 6;
    while app.world().get::<Hop>(player) != Some(&Hop::Grounded) {
        app.run_tick(&[]);
        apex = apex.max(position(&app, player).y);
        airborne_ticks += 1;
        assert!(airborne_ticks < 200, "never landed");
    }
    let PlayerTuning { hop_speed, gravity, .. } = tuning(&app);
    let expected = hop_speed * hop_speed / (2.0 * gravity);
    assert!((apex - GROUND_Y - expected).abs() < 0.1, "apex {apex}");
    assert_eq!(position(&app, player).y, GROUND_Y);
}

#[test]
fn holding_cross_does_not_hop_again() {
    let (mut app, player) = app();
    // Hold Cross through the whole hop and the landing.
    app.run_ticks(60, &[Button::Cross]);
    assert_eq!(app.world().get::<Hop>(player), Some(&Hop::Grounded));
    // A new press hops again.
    app.run_tick(&[]);
    app.run_tick(&[Button::Cross]);
    assert!(matches!(app.world().get::<Hop>(player), Some(Hop::Airborne { .. })));
}

#[test]
fn the_demo_level_spawns_its_rules_entities() {
    use game::demo::DemoLevel;
    use game::demo::spinner::Spinner;
    let mut app = headless_app(game::tick_rate(game::REGION));
    app.add_plugins(PlayerPlugin);
    let level: DemoLevel = game::config::load_ron(DemoLevel::PATH).unwrap();
    level.spawn(&mut app.world_mut().commands());
    app.run_tick(&[]);
    let players = app.world_mut().query_filtered::<&Transform, With<Player>>().iter(app.world()).count();
    let spinners = app.world_mut().query::<&Spinner>().iter(app.world()).count();
    assert_eq!((players, spinners), (1, level.spinners.len()));
}
