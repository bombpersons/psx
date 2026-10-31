//! Scenarios: the port against the original in situations both reach,
//! tick by tick (`psx_bevy::scenario`). Each recording psx made of the
//! original (`scenarios/<id>/<name>.jsonl`) is replayed with its pad, and
//! the scenario's quantities compared. `psx port check` runs this test and
//! records the verdicts; it is also the quick check while working on a
//! feature.
//!
//! `app` makes a fresh headless app with the game's rules: they register
//! the launchers for the scenario kinds and observe the quantities.
use bevy::prelude::*;
use psx_bevy::scenario::Scenarios;
use psx_bevy::testing::headless_app;
use std::path::Path;

fn app() -> App {
    let mut app = headless_app(game::tick_rate(game::REGION));
    app.add_plugins(game::LogicPlugins);
    app
}

#[test]
fn scenarios() {
    Scenarios::new(Path::new(env!("CARGO_MANIFEST_DIR")).join(".."), app).run();
}
