//! PlayStation asset formats: TIM textures, TMD models, VAB/VH+VB sound banks,
//! VAG samples and SPU-ADPCM, plus a signature scanner that finds them in
//! archives and RAM dumps, and TMD -> glTF export.
//!
//! `tim.rs` and `tmd.rs` are dependency-free and copied verbatim into the Bevy
//! template's asset loaders (`templates/psx-bevy/src/psx/formats/`).

pub mod adpcm;
pub mod archive;
pub mod gltf;
pub mod model_document;
pub mod scan;
pub mod tim;
pub mod tmd;
pub mod upload;
pub mod vab;

pub use scan::{scan, Hit, Info, Kind};
pub use tim::{Tim, TimAlpha, Vram};
pub use tmd::Tmd;
pub use vab::{Vab, Vag};

#[cfg(test)]
mod tests {
    /// The template's copies of the shared parsers must not drift.
    #[test]
    fn template_copies_match() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        for f in ["tim.rs", "tmd.rs"] {
            let ours = std::fs::read_to_string(root.join("src").join(f)).unwrap();
            let theirs = std::fs::read_to_string(root.join("../../templates/psx-bevy/psx-bevy/src/psx/formats").join(f)).unwrap();
            assert!(ours == theirs, "templates/psx-bevy/psx-bevy/src/psx/formats/{f} differs from crates/psx-assets/src/{f}; copy it over");
        }
    }
}
