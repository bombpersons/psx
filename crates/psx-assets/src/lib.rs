//! PlayStation asset formats: TIM textures, TMD models, VAB/VH+VB sound banks,
//! VAG samples and SPU-ADPCM, plus a signature scanner that finds them in
//! archives and RAM dumps, and TMD -> glTF export.
//!
//! `tim.rs` and `tmd.rs` are dependency-free and copied verbatim into
//! psx-bevy's asset loaders (`src/psx/formats/` in
//! <https://github.com/bombpersons/psx-bevy>).

pub mod adpcm;
pub mod archive;
pub mod gltf;
pub mod model_document;
pub mod scan;
pub mod tim;
pub mod tmd;
pub mod upload;
pub mod vab;

pub use scan::{Hit, Info, Kind, scan};
pub use tim::{Tim, TimAlpha, Vram};
pub use tmd::Tmd;
pub use vab::{Vab, Vag};

#[cfg(test)]
mod tests {
    /// psx-bevy's copies of the shared parsers must not drift. psx-bevy is its
    /// own repository: the test reads a checkout at `PSX_BEVY_DIR`, else one
    /// next to this repository, and is skipped when neither exists.
    #[test]
    fn psx_bevy_copies_match() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let bevy = std::env::var_os("PSX_BEVY_DIR")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| root.join("../../../psx-bevy"));
        if !bevy.join("src/psx/formats").is_dir() {
            eprintln!("skipped: no psx-bevy checkout at {} (set PSX_BEVY_DIR)", bevy.display());
            return;
        }
        for f in ["tim.rs", "tmd.rs"] {
            let ours = std::fs::read_to_string(root.join("src").join(f)).unwrap();
            let theirs = std::fs::read_to_string(bevy.join("src/psx/formats").join(f)).unwrap();
            assert!(
                ours == theirs,
                "psx-bevy's src/psx/formats/{f} differs from crates/psx-assets/src/{f}; copy it over"
            );
        }
    }
}
