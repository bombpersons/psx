//! Gameplay tests: run the demo's rules headless for exact ticks with scripted
//! buttons. For your game, compare rules against the original with
//! `game::testing::trajectory`: replay a `psx run --replay` file, record the
//! values `psx run --sample` recorded, and `compare` every tick.
use bevy::prelude::*;
use game::controller::Button;
use game::demo::player::{GRAVITY, GROUND_Y, HOP_SPEED, Hop, Player, PlayerPlugin, WALK_SPEED};
use game::testing::{TickApp, headless_app};

fn app() -> (App, Entity) {
    let mut app = headless_app(game::tick_rate(game::REGION));
    app.add_plugins(PlayerPlugin);
    let player = app.world_mut().spawn(Player).id();
    (app, player)
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
    assert!((position(&app, player).x - WALK_SPEED * seconds).abs() < 1e-3);
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
    let expected = HOP_SPEED * HOP_SPEED / (2.0 * GRAVITY);
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
