//! Offline importers: files extracted from the disc with `psx` (for example
//! `psx disc extract`) -> engine-native assets under `game/assets/`.
//!
//! The original's raw formats (fixed-point units, packed bitfields, offsets
//! into word arrays, CLUT/VRAM layouts) live only here. Each importer converts
//! exactly into the engine's format and records where every output came from
//! with [`Provenance`], so a reader can check an asset against the disc file.
//! Validate structurally: every byte of the source accounted for, every
//! reference in range.
//!
//! cargo run -p import -- provenance <source> <output>
use serde::Serialize;
use std::path::{Path, PathBuf};

/// Where one output came from: written as `<output>.provenance.json`.
#[derive(Serialize)]
pub struct Provenance {
    /// The source file, as named on the disc or by `psx` extraction.
    pub source: String,
    pub source_size: u64,
    /// FNV-1a 64 of the source bytes (dependency-free; enough to notice a
    /// different disc revision).
    pub source_fnv1a64: String,
    pub importer: String,
}

impl Provenance {
    pub fn of(source: &Path, importer: &str) -> std::io::Result<Self> {
        let bytes = std::fs::read(source)?;
        Ok(Self {
            source: source.display().to_string(),
            source_size: bytes.len() as u64,
            source_fnv1a64: format!("{:016x}", fnv1a64(&bytes)),
            importer: importer.to_string(),
        })
    }

    /// Writes this next to `output`.
    pub fn write_beside(&self, output: &Path) -> std::io::Result<PathBuf> {
        let mut name = output.file_name().unwrap_or_default().to_os_string();
        name.push(".provenance.json");
        let path = output.with_file_name(name);
        std::fs::write(&path, serde_json::to_vec_pretty(self).map_err(std::io::Error::other)?)?;
        Ok(path)
    }
}

pub fn fnv1a64(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |hash, &b| (hash ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3))
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.iter().map(String::as_str).collect::<Vec<_>>().as_slice() {
        // Add one subcommand per importer, e.g. ["levels", src, out] => levels::import(src, out).
        ["provenance", source, output] => {
            let written = Provenance::of(Path::new(source), "provenance")
                .and_then(|p| p.write_beside(Path::new(output)))
                .unwrap_or_else(|e| fail(&e.to_string()));
            println!("{}", written.display());
        }
        _ => fail("usage: import provenance <source> <output>"),
    }
}

fn fail(message: &str) -> ! {
    eprintln!("{message}");
    std::process::exit(2)
}

#[cfg(test)]
mod tests {
    #[test]
    fn fnv1a64_matches_reference_values() {
        assert_eq!(super::fnv1a64(b""), 0xcbf29ce484222325);
        assert_eq!(super::fnv1a64(b"a"), 0xaf63dc4c8601ec8c);
    }
}
