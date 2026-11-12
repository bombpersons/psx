//! Scenarios: the port against the original in situations both reach,
//! tick by tick (psx_scenario's runner). Each recording psx made of the
//! original (`scenarios/<id>/<name>.jsonl`) is replayed with its pad in a
//! fresh `Game`, and the scenario's quantities compared: the quick check
//! while working. The plays goal.toml holds out are not run here: `psx
//! port check` runs every scenario against the game's program (`--psx`),
//! held-out plays too, and records the verdicts.
use psx_scenario::Scenarios;

#[test]
fn scenarios() {
    Scenarios::new(concat!(env!("CARGO_MANIFEST_DIR"), "/.."), game::Game::headless).run();
}
