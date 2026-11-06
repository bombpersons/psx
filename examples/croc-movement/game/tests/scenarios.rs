//! Scenarios: the port against the original in situations both reach,
//! tick by tick (psx_scenario's runner). Each recording psx made of the
//! original (`scenarios/<id>/<name>.jsonl`) is replayed with its pad in a
//! fresh `Game`, and the scenario's quantities compared. `psx port check`
//! runs this test and records the verdicts; it is also the quick check
//! while working on a feature.
use psx_scenario::Scenarios;

#[test]
fn scenarios() {
    Scenarios::new(concat!(env!("CARGO_MANIFEST_DIR"), "/.."), game::Game::headless).run();
}
