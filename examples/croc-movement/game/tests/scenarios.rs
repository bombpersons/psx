//! Scenarios: the port against the original in situations both reach,
//! tick by tick (psx_scenario's runner). Each recording psx made of the
//! original (`scenarios/<id>/<name>.jsonl`) is replayed with its pad in a
//! fresh `Game`, and the scenario's quantities compared: the quick check
//! while working on a feature. `psx port check` runs the same scenarios
//! against the game's program (`--psx`) and records the verdicts.
use psx_scenario::Scenarios;

#[test]
fn scenarios() {
    Scenarios::new(concat!(env!("CARGO_MANIFEST_DIR"), "/.."), game::Game::headless).run();
}
