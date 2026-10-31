fn main() {
    // The glue between the glossary's `Quantities` and psx: generated from
    // glossary/src/lib.rs.
    psx_scenario::rust::write_glossary_glue(concat!(env!("CARGO_MANIFEST_DIR"), "/.."));
}
