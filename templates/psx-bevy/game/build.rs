fn main() {
    // `Observed`, the glossary's quantities as the port has them, for the
    // scenarios (psx_bevy::scenario): generated from glossary/src/lib.rs.
    psx_scenario::rust::write_observed(concat!(env!("CARGO_MANIFEST_DIR"), "/../glossary"));
}
