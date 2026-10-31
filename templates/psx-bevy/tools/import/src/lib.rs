//! Importers: the player's disc -> the port's assets under
//! `game/assets/imported/`. The game runs [`import`] itself on first start
//! when it finds the disc (`game/src/disc.rs`); `cargo run -p import -- DISC`
//! runs the same code by hand.
//!
//! The original's raw formats (fixed-point units, packed bitfields, offsets
//! into word arrays, CLUT/VRAM layouts) live only here. Each importer converts
//! exactly into the port's format and records where every output came from
//! with [`Provenance`], so a reader can check an asset against the disc file.
//! Validate structurally: every byte of the source accounted for, every
//! reference in range. Decoders for standard PS1 formats (TIM, TMD, VAB/VAG)
//! are in [`psx_assets`].
pub use psx_assets;
pub use psx_media;

use psx_media::Disc;
use psx_media::iso9660::{Entry, FileSystem};
use psx_media::system_cnf::{DEFAULT_BOOT, SystemCnf};
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::sync::{Arc, Mutex};

/// The original game's name, for the "put your disc next to the game" screen.
pub const GAME: &str = "the original game";

/// Accepted discs, by the boot executable named in `SYSTEM.CNF`
/// (`SLUS_009.07`, `SCES_012.37`...): every region and revision the importer
/// supports. Empty accepts any PlayStation disc.
pub const SERIALS: &[&str] = &[];

/// Bump whenever [`import`]'s output changes: players' older imports are
/// redone on the next start.
pub const VERSION: u32 = 1;

/// Converts the disc into files under `out` (an empty folder that becomes
/// `game/assets/imported/`). Report progress as it goes.
///
/// The template's version records which disc it was given (`disc.ron`), to
/// show the protocol working; replace it with the game's importers.
pub fn import(disc: &DiscReader, out: &Path, progress: &ImportProgress) -> Result<(), String> {
    progress.set(0.5, "Disc information");
    let info = DiscInfo {
        volume_id: disc.fs.volume.volume_id.trim().to_string(),
        serial: disc.serial.clone(),
        files: disc.entries().iter().filter(|e| !e.is_dir).count(),
    };
    let text = ron::ser::to_string_pretty(&info, Default::default()).map_err(|e| e.to_string())?;
    std::fs::write(out.join(DiscInfo::PATH.trim_start_matches("imported/")), text).map_err(|e| e.to_string())?;
    Ok(())
}

/// What the template's [`import`] writes: `imported/disc.ron`.
#[derive(Serialize, Deserialize, Debug, PartialEq)]
pub struct DiscInfo {
    pub volume_id: String,
    pub serial: String,
    pub files: usize,
}

impl DiscInfo {
    /// Under the game's assets.
    pub const PATH: &str = "imported/disc.ron";
}

/// A disc image opened for import.
pub struct DiscReader {
    pub path: std::path::PathBuf,
    pub disc: Disc,
    pub fs: FileSystem,
    /// The boot executable's name (`SLUS_009.07`), or `PSX.EXE` without `SYSTEM.CNF`.
    pub serial: String,
}

impl DiscReader {
    /// Opens a `.cue`, `.chd`, `.iso` or bare `.bin` and reads its file system.
    pub fn open(path: &Path) -> Result<Self, String> {
        let error = |e: psx_media::MediaError| format!("{}: {e}", path.display());
        let disc = Disc::open(path).map_err(error)?;
        let fs = FileSystem::read(&disc).map_err(error)?;
        let boot = match fs.find("/SYSTEM.CNF") {
            Some(entry) => SystemCnf::parse(&fs.read_file(&disc, entry).map_err(error)?).boot,
            None => None,
        };
        let serial = serial_of(&boot.unwrap_or_else(|| DEFAULT_BOOT.into()));
        Ok(Self { path: path.to_path_buf(), disc, fs, serial })
    }

    /// Whether this is a disc [`SERIALS`] accepts.
    pub fn accepted(&self) -> bool {
        SERIALS.is_empty() || SERIALS.iter().any(|s| s.eq_ignore_ascii_case(&self.serial))
    }

    /// A file's bytes by ISO9660 path (`/DATA/LEVEL1.DAT`).
    pub fn read(&self, path: &str) -> Result<Vec<u8>, String> {
        let entry = self.fs.entry(path).map_err(|e| e.to_string())?;
        self.fs.read_file(&self.disc, entry).map_err(|e| format!("{path}: {e}"))
    }

    /// Every file and directory on the disc.
    pub fn entries(&self) -> &[Entry] {
        &self.fs.entries
    }
}

/// `cdrom:\SLUS_009.07;1` -> `SLUS_009.07`.
pub fn serial_of(boot: &str) -> String {
    let name = boot.rsplit(['\\', '/', ':']).next().unwrap_or(boot);
    name.split(';').next().unwrap_or(name).to_ascii_uppercase()
}

/// Builds tiny ISO images for tests: `test_disc::iso("VOL", &[("SYSTEM.CNF", b"BOOT = ...")])`.
pub mod test_disc {
    const SECTOR: usize = 2048;

    fn both32(out: &mut [u8], v: u32) {
        out[..4].copy_from_slice(&v.to_le_bytes());
        out[4..8].copy_from_slice(&v.to_be_bytes());
    }

    fn record(lba: u32, size: u32, dir: bool, name: &[u8]) -> Vec<u8> {
        let len = (33 + name.len() + 1) & !1;
        let mut r = vec![0u8; len];
        r[0] = len as u8;
        both32(&mut r[2..10], lba);
        both32(&mut r[10..18], size);
        r[18..25].copy_from_slice(&[99, 1, 1, 0, 0, 0, 0]);
        r[25] = if dir { 2 } else { 0 };
        r[28..30].copy_from_slice(&1u16.to_le_bytes());
        r[30..32].copy_from_slice(&1u16.to_be_bytes());
        r[32] = name.len() as u8;
        r[33..33 + name.len()].copy_from_slice(name);
        r
    }

    /// A Mode 1 (`.iso`) image with these files in its root directory.
    pub fn iso(volume_id: &str, files: &[(&str, &[u8])]) -> Vec<u8> {
        let mut lba = 19u32;
        let mut placed = Vec::new();
        for (name, bytes) in files {
            placed.push((format!("{name};1"), lba, *bytes));
            lba += bytes.len().div_ceil(SECTOR).max(1) as u32;
        }
        let mut image = vec![0u8; lba as usize * SECTOR];
        let pvd = &mut image[16 * SECTOR..17 * SECTOR];
        pvd[0] = 1;
        pvd[1..6].copy_from_slice(b"CD001");
        pvd[6] = 1;
        let mut id = [b' '; 32];
        id[..volume_id.len().min(32)].copy_from_slice(&volume_id.as_bytes()[..volume_id.len().min(32)]);
        pvd[40..72].copy_from_slice(&id);
        both32(&mut pvd[80..88], lba);
        pvd[128..130].copy_from_slice(&(SECTOR as u16).to_le_bytes());
        pvd[130..132].copy_from_slice(&(SECTOR as u16).to_be_bytes());
        pvd[156..190].copy_from_slice(&record(18, SECTOR as u32, true, &[0]));
        image[17 * SECTOR] = 255;
        image[17 * SECTOR + 1..17 * SECTOR + 6].copy_from_slice(b"CD001");
        let mut dir = record(18, SECTOR as u32, true, &[0]);
        dir.extend(record(18, SECTOR as u32, true, &[1]));
        for (name, lba, bytes) in &placed {
            dir.extend(record(*lba, bytes.len() as u32, false, name.as_bytes()));
            let at = *lba as usize * SECTOR;
            image[at..at + bytes.len()].copy_from_slice(bytes);
        }
        image[18 * SECTOR..18 * SECTOR + dir.len()].copy_from_slice(&dir);
        image
    }
}

/// How far an import is, shared with the game's progress screen.
#[derive(Clone, Default)]
pub struct ImportProgress(Arc<Mutex<(f32, String)>>);

impl ImportProgress {
    /// `fraction` in 0..=1 and what is being imported now.
    pub fn set(&self, fraction: f32, step: impl Into<String>) {
        *self.0.lock().unwrap() = (fraction.clamp(0.0, 1.0), step.into());
    }

    pub fn get(&self) -> (f32, String) {
        self.0.lock().unwrap().clone()
    }
}

use std::path::PathBuf;

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
    /// For a file read from the disc, named as [`DiscReader::read`] takes it
    /// (`/DATA/LEVEL1.DAT`): psx counts a disc file imported by this name.
    pub fn of_disc(path: &str, bytes: &[u8], importer: &str) -> Self {
        Self {
            source: path.to_string(),
            source_size: bytes.len() as u64,
            source_fnv1a64: format!("{:016x}", fnv1a64(bytes)),
            importer: importer.to_string(),
        }
    }

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

/// Marks a test as checking the import of the disc file `path` against
/// what the game built from it (its RAM after loading, its VRAM uploads, a
/// GPU capture: evidence from psx). `psx port check` counts the file
/// checked when the test passes: `let _checked = import::checking("/DATA/LEVEL1.DAT");`
/// at its start.
pub fn checking(path: &str) -> Checking {
    Checking(path.to_string())
}

/// See [`checking`]: recorded when dropped, unless the test panicked.
pub struct Checking(String);

impl Drop for Checking {
    fn drop(&mut self) {
        if std::thread::panicking() {
            return;
        }
        if let Some(report) = std::env::var_os("PSX_IMPORT_REPORT") {
            use std::io::Write;
            let file = std::fs::OpenOptions::new().create(true).append(true).open(report);
            if let Ok(mut f) = file {
                let _ = writeln!(f, "{}", self.0);
            }
        }
    }
}

pub fn fnv1a64(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |hash, &b| (hash ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3))
}

#[cfg(test)]
mod tests {
    #[test]
    fn fnv1a64_matches_reference_values() {
        assert_eq!(super::fnv1a64(b""), 0xcbf29ce484222325);
        assert_eq!(super::fnv1a64(b"a"), 0xaf63dc4c8601ec8c);
    }

    #[test]
    fn opens_a_disc_and_imports_its_info() {
        let dir = std::env::temp_dir().join(format!("import-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let image = dir.join("game.iso");
        let cnf = b"BOOT = cdrom:\\SLUS_123.45;1\r\nTCB = 4\r\n";
        let files: [(&str, &[u8]); 2] = [("SYSTEM.CNF", cnf), ("SLUS_123.45", &[0; 3000])];
        std::fs::write(&image, super::test_disc::iso("MY_GAME", &files)).unwrap();
        let disc = super::DiscReader::open(&image).unwrap();
        assert_eq!(disc.serial, "SLUS_123.45");
        assert_eq!(disc.read("/slus_123.45;1").unwrap().len(), 3000);
        let out = dir.join("out");
        std::fs::create_dir_all(&out).unwrap();
        super::import(&disc, &out, &super::ImportProgress::default()).unwrap();
        let info: super::DiscInfo = ron::from_str(&std::fs::read_to_string(out.join("disc.ron")).unwrap()).unwrap();
        assert_eq!(info, super::DiscInfo { volume_id: "MY_GAME".into(), serial: "SLUS_123.45".into(), files: 2 });
    }

    #[test]
    fn a_passing_check_is_recorded_for_psx() {
        let report = std::env::temp_dir().join(format!("import-checked-{}", std::process::id()));
        // Only psx port check sets the variable; this test sets it for itself.
        unsafe { std::env::set_var("PSX_IMPORT_REPORT", &report) };
        drop(super::checking("/DATA/LEVEL1.DAT"));
        let failed = std::thread::spawn(|| {
            let _checked = super::checking("/DATA/LEVEL2.DAT");
            panic!("the import differs from the game's RAM");
        });
        assert!(failed.join().is_err());
        assert_eq!(std::fs::read_to_string(&report).unwrap(), "/DATA/LEVEL1.DAT\n");
        std::fs::remove_file(report).unwrap();
    }

    #[test]
    fn serial_from_boot_line() {
        assert_eq!(super::serial_of("cdrom:\\SLUS_009.07;1"), "SLUS_009.07");
        assert_eq!(super::serial_of("cdrom:PSX.EXE;1"), "PSX.EXE");
    }
}
