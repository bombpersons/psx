//! Step replay: every trace under `mechanics/<id>/traces/` run through the
//! mechanic's own `step`, with no emulator, and compared with what the
//! original did. This proves the reference: that the rules as written
//! reproduce the original. psx's `docs/mechanics.md` describes it.
//!
//! `cargo test -p replay` runs it. The conversions between each mechanic's
//! structs and its traces are generated from the mechanic's source by
//! `build.rs`; nothing here is edited per mechanic but `Cargo.toml`.

include!(concat!(env!("OUT_DIR"), "/replay.rs"));

#[cfg(test)]
mod tests {
    #[test]
    fn traces() {
        let root = std::path::Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/.."));
        psx_mechanics::replay::run(root, &super::mechanics());
    }
}
