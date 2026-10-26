//! Case files replayed headless (`engine::testing::case`): one runner per
//! subject, which spawns it from the case's start and reads each compared
//! column back in the case's units, then one line per case.
//!
//! A port's cases are recordings of the original (in a clean room,
//! `specs/cases/`): point `load` at them. The demo's case in `tests/cases/` was
//! recorded from the demo itself, to show the form: a tap of Cross and a
//! walk to the right, with a tick that caught up (row 10 follows two ticks)
//! and a row without one (row 11), and coverage that splits its columns
//! between two specs.
//!
//! Each run is also written in the case's form to the target's temporary
//! folder (`write_run`), to compare with the original outside the tests,
//! with its verdict per spec: the record of which specs the port passes.
use bevy::prelude::*;
use engine::EnginePlugin;
use engine::testing::{Case, Start, headless_app};
use game::demo::player::{Hop, Player, PlayerPlugin};
use std::path::Path;

/// The specs the rules implement. A case's claims for any other spec are
/// listed as waiting for it, not compared.
const IMPLEMENTED: &[&str] = &["DEMO-0001", "DEMO-0002"];

fn load(name: &str) -> Case {
    Case::load(Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/cases").join(name)).unwrap()
}

/// The player's cases: the player starts as row 0 shows it.
fn player_case(name: &str) {
    let case = load(name);
    assert_eq!(case.start().unwrap(), Start::FirstRow, "{name}");
    let mut app = headless_app(game::tick_rate(game::REGION));
    app.add_plugins((EnginePlugin, PlayerPlugin));
    let at = |c: &str| case.first(c) as f32;
    let player = app.world_mut().spawn((Player, Transform::from_xyz(at("x"), at("y"), at("z")))).id();
    let report = case.runner().implementing(IMPLEMENTED).run(&mut app, |world, column| {
        let position = world.get::<Transform>(player)?.translation;
        let value = match column {
            "x" => position.x,
            "y" => position.y,
            "z" => position.z,
            "airborne" => f32::from(u8::from(matches!(world.get::<Hop>(player)?, Hop::Airborne { .. }))),
            _ => return None,
        };
        Some(f64::from(value))
    });
    report.write_run(Path::new(env!("CARGO_TARGET_TMPDIR")).join("cases")).unwrap();
    report.assert_passed();
}

macro_rules! cases {
    ($runner:ident: $($name:ident),* $(,)?) => {
        $(#[test] fn $name() { $runner(stringify!($name)); })*
    };
}

// DEMO-0001 (the hop), DEMO-0002 (walking)
cases!(player_case: demo_hop_walk);
