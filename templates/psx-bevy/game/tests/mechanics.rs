//! Step replay: each mechanic's port against the traces captured from the
//! original (`mechanics/<id>/traces/`), with no emulator
//! (`psx_bevy::replay`). `psx port check` runs this test and records the
//! verdicts; it is also the quick check while working on a step.
//!
//! Register an adapter per mechanic: `.exact::<Adapter>()` for an exact
//! mechanic (it also runs the f32 drift report), `.faithful::<Adapter,
//! Float>()` for a faithful one. Mechanics with traces and no adapter are
//! listed, not failed; a feature's replay verdict fails until each of its
//! mechanics has one.
use psx_bevy::replay::Replays;
use std::path::Path;

#[test]
fn mechanics() {
    Replays::new(Path::new(env!("CARGO_MANIFEST_DIR")).join("..")).run();
}
