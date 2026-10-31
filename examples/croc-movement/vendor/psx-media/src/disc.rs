//! Disc geometry and sector access over CUE/BIN, bare raw BIN, cooked ISO and CHD.
//!
//! LBA here is the logical block address used by ISO9660 and the CD-ROM
//! controller's `Setloc` after subtracting the 150-sector lead-in: LBA 0 is
//! the first sector of track 1's INDEX 01 on a normal PS1 image.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use sha2::{Digest, Sha256};

use crate::cue::{CueSheet, TrackMode};
use crate::error::{MediaError, Result, malformed};

pub const SYNC: [u8; 12] = [0, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0];
pub const LEAD_IN_SECTORS: u32 = 150;

#[derive(Clone, Debug, serde::Serialize)]
pub struct ImageFile {
    pub name: String,
    #[serde(skip)]
    pub path: PathBuf,
    pub bytes: u64,
}

#[derive(Clone, Debug, serde::Serialize)]
pub struct Track {
    pub number: u8,
    pub mode: TrackMode,
    /// Index into [`Disc::files`].
    pub file: usize,
    /// Sector offset inside the file of the first stored sector (INDEX 00 if
    /// present); for a CHD, the frame number inside the CHD.
    pub file_sector: u32,
    /// LBA of the first stored sector.
    pub first_lba: u32,
    /// LBA of INDEX 01, where the track's content starts.
    pub start_lba: u32,
    /// Stored sectors, counted from `first_lba`.
    pub sectors: u32,
    /// `PREGAP` sectors implied by the CUE but absent from the file.
    pub unstored_pregap: u32,
}

impl Track {
    pub fn contains(&self, lba: u32) -> bool {
        lba >= self.first_lba && lba < self.first_lba + self.sectors
    }
}

/// Which part of a CD-XA Mode 2 sector holds user data.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SectorForm {
    /// Mode 1 or cooked: 2048 bytes.
    Mode1,
    /// Mode 2 Form 1: 2048 bytes with EDC/ECC.
    Form1,
    /// Mode 2 Form 2: 2324 bytes (XA audio, STR video, some streams).
    Form2,
}

#[derive(Clone, Debug)]
pub struct DataSector {
    pub lba: u32,
    pub form: SectorForm,
    /// CD-XA subheader `[file, channel, submode, coding]` (Mode 2 only).
    pub subheader: Option<[u8; 4]>,
    pub data: Vec<u8>,
}

pub struct Disc {
    pub source: PathBuf,
    pub files: Vec<ImageFile>,
    pub tracks: Vec<Track>,
    store: Store,
    /// Subchannel Q overrides (LibCrypt) by LBA, from an `.sbi`/`.lsd` beside the image.
    pub subq: std::collections::BTreeMap<u32, crate::subq::SubQ>,
    pub subq_source: Option<PathBuf>,
}

/// Where sector bytes come from.
enum Store {
    /// One open handle per [`Disc::files`] entry.
    Files(Mutex<Vec<File>>),
    Chd(Box<crate::chd::ChdImage>),
}

impl std::fmt::Debug for Disc {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Disc")
            .field("source", &self.source)
            .field("files", &self.files)
            .field("tracks", &self.tracks)
            .finish()
    }
}

/// Disc image extensions [`Disc::open`] accepts, lowercase.
pub const IMAGE_EXTENSIONS: &[&str] = &["cue", "bin", "img", "iso", "chd"];

/// Whether `path` has a disc image extension (case-insensitive).
pub fn is_image_path(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| IMAGE_EXTENSIONS.contains(&e.to_ascii_lowercase().as_str()))
}

fn file_len(path: &Path) -> Result<u64> {
    std::fs::metadata(path).map(|m| m.len()).map_err(|e| MediaError::io(path, e))
}

impl Disc {
    /// Open a `.cue`, a bare raw `.bin` (single data track), a cooked `.iso` or a CD `.chd`.
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        let mut disc = Self::open_image(path)?;
        if let Some((src, map)) = crate::subq::load_beside(path)? {
            disc.subq = map;
            disc.subq_source = Some(src);
        }
        Ok(disc)
    }

    fn open_image(path: &Path) -> Result<Self> {
        let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
        match ext.as_str() {
            "cue" => Self::open_cue(path),
            "iso" => Self::open_single(path, TrackMode::Mode1Cooked),
            "chd" => Self::open_chd(path),
            "bin" | "img" => {
                let mut head = [0u8; 16];
                let mut f = File::open(path).map_err(|e| MediaError::io(path, e))?;
                f.read_exact(&mut head).map_err(|e| MediaError::io(path, e))?;
                if head[..12] != SYNC {
                    return Err(MediaError::Unsupported("bare .bin without CD sync pattern; supply its .cue".into()));
                }
                let mode = if head[15] == 1 { TrackMode::Mode1Raw } else { TrackMode::Mode2Raw };
                Self::open_single(path, mode)
            }
            other => Err(MediaError::Unsupported(format!("disc image extension .{other}"))),
        }
    }

    fn open_single(path: &Path, mode: TrackMode) -> Result<Self> {
        let bytes = file_len(path)?;
        let ss = mode.sector_size();
        if bytes % ss != 0 {
            return malformed(format!("{} is not a whole number of {ss}-byte sectors", path.display()));
        }
        let handle = File::open(path).map_err(|e| MediaError::io(path, e))?;
        Ok(Disc {
            source: path.to_path_buf(),
            files: vec![ImageFile { name: file_name(path), path: path.to_path_buf(), bytes }],
            tracks: vec![Track {
                number: 1,
                mode,
                file: 0,
                file_sector: 0,
                first_lba: 0,
                start_lba: 0,
                sectors: (bytes / ss) as u32,
                unstored_pregap: 0,
            }],
            store: Store::Files(Mutex::new(vec![handle])),
            subq: Default::default(),
            subq_source: None,
        })
    }

    fn open_cue(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path).map_err(|e| MediaError::io(path, e))?;
        let sheet = CueSheet::parse(&text)?;
        let dir = path.parent().unwrap_or(Path::new("."));

        let mut files = Vec::new();
        let mut handles = Vec::new();
        let mut tracks = Vec::new();
        let mut lba: u32 = 0;
        for (fi, cf) in sheet.files.iter().enumerate() {
            let fpath = dir.join(&cf.name);
            let bytes = file_len(&fpath)?;
            let ss = match cf.tracks.first() {
                Some(t) => t.mode.sector_size(),
                None => continue,
            };
            if cf.tracks.iter().any(|t| t.mode.sector_size() != ss) {
                return Err(MediaError::Unsupported(format!("mixed sector sizes in {}", cf.name)));
            }
            if bytes % ss != 0 {
                return malformed(format!("{} is not a whole number of {ss}-byte sectors", cf.name));
            }
            let file_sectors = (bytes / ss) as u32;
            let mut shift = 0;
            for (i, t) in cf.tracks.iter().enumerate() {
                shift += t.pregap;
                let first = t.first_stored();
                let end = cf.tracks.get(i + 1).map_or(file_sectors, |n| n.first_stored());
                if end < first || end > file_sectors {
                    return malformed(format!("track {} lies outside {}", t.number, cf.name));
                }
                tracks.push(Track {
                    number: t.number,
                    mode: t.mode,
                    file: fi,
                    file_sector: first,
                    first_lba: lba + shift + first,
                    start_lba: lba + shift + t.index(1).unwrap_or(first),
                    sectors: end - first,
                    unstored_pregap: t.pregap,
                });
            }
            lba += shift + file_sectors;
            handles.push(File::open(&fpath).map_err(|e| MediaError::io(&fpath, e))?);
            files.push(ImageFile { name: cf.name.clone(), path: fpath, bytes });
        }
        Ok(Disc {
            source: path.to_path_buf(),
            files,
            tracks,
            store: Store::Files(Mutex::new(handles)),
            subq: Default::default(),
            subq_source: None,
        })
    }

    fn open_chd(path: &Path) -> Result<Self> {
        let (image, chd_tracks) = crate::chd::ChdImage::open(path)?;
        let mut tracks = Vec::new();
        let mut lba = 0;
        for t in &chd_tracks {
            // Track 1's pregap is the 2-second lead-in, which LBA 0 already
            // follows: it takes no LBAs, and stored frames of it are skipped.
            let lead_in = t.number == 1;
            let skip = if lead_in && t.pregap_stored { t.pregap } else { 0 };
            let unstored = if t.pregap_stored || lead_in { 0 } else { t.pregap };
            let stored_pregap = if t.pregap_stored { t.pregap - skip } else { 0 };
            lba += unstored;
            tracks.push(Track {
                number: t.number,
                mode: t.mode,
                file: 0,
                file_sector: t.chd_frame + skip,
                first_lba: lba,
                start_lba: lba + stored_pregap,
                sectors: t.frames - skip,
                unstored_pregap: unstored,
            });
            lba += t.frames - skip;
        }
        let files = vec![ImageFile { name: file_name(path), path: path.to_path_buf(), bytes: file_len(path)? }];
        Ok(Disc {
            source: path.to_path_buf(),
            files,
            tracks,
            store: Store::Chd(Box::new(image)),
            subq: Default::default(),
            subq_source: None,
        })
    }

    pub fn track_for_lba(&self, lba: u32) -> Option<&Track> {
        self.tracks.iter().find(|t| t.contains(lba))
    }

    /// Total addressable sectors (end of the last stored sector).
    pub fn end_lba(&self) -> u32 {
        self.tracks.iter().map(|t| t.first_lba + t.sectors).max().unwrap_or(0)
    }

    /// Where a sector's stored bytes start: (index into [`Disc::files`], byte
    /// offset in that file). `None` for a CHD, whose sectors are compressed.
    pub fn image_offset(&self, lba: u32) -> Option<(usize, u64)> {
        let t = self.track_for_lba(lba)?;
        match self.store {
            Store::Files(_) => Some((t.file, (t.file_sector + (lba - t.first_lba)) as u64 * t.mode.sector_size())),
            Store::Chd(_) => None,
        }
    }

    /// Read the stored bytes of one sector exactly as they appear in the image.
    pub fn read_raw(&self, lba: u32) -> Result<(Vec<u8>, TrackMode)> {
        let t = self
            .track_for_lba(lba)
            .ok_or_else(|| MediaError::NotFound(format!("LBA {lba} is not stored in this image")))?;
        let sector = t.file_sector + (lba - t.first_lba);
        let path = &self.files[t.file].path;
        match &self.store {
            Store::Files(handles) => {
                let ss = t.mode.sector_size();
                let mut buf = vec![0u8; ss as usize];
                let mut handles = handles.lock().unwrap_or_else(|p| p.into_inner());
                let h = &mut handles[t.file];
                h.seek(SeekFrom::Start(sector as u64 * ss)).map_err(|e| MediaError::io(path, e))?;
                h.read_exact(&mut buf).map_err(|e| MediaError::io(path, e))?;
                Ok((buf, t.mode))
            }
            Store::Chd(chd) => Ok((chd.read_frame(path, sector, t.mode)?, t.mode)),
        }
    }

    /// Read a data sector's user data, honouring Mode 2 Form 1/Form 2.
    pub fn read_data(&self, lba: u32) -> Result<DataSector> {
        let (raw, mode) = self.read_raw(lba)?;
        match mode {
            TrackMode::Audio => Err(MediaError::NotFound(format!("LBA {lba} is in an audio track"))),
            TrackMode::Mode1Cooked => Ok(DataSector { lba, form: SectorForm::Mode1, subheader: None, data: raw }),
            TrackMode::Mode1Raw | TrackMode::Mode2Raw => {
                if raw[..12] != SYNC {
                    return malformed(format!("LBA {lba} has no sync pattern"));
                }
                match raw[15] {
                    1 => Ok(DataSector {
                        lba,
                        form: SectorForm::Mode1,
                        subheader: None,
                        data: raw[16..16 + 2048].to_vec(),
                    }),
                    2 => {
                        let sub = [raw[16], raw[17], raw[18], raw[19]];
                        let (form, len) =
                            if sub[2] & 0x20 != 0 { (SectorForm::Form2, 2324) } else { (SectorForm::Form1, 2048) };
                        Ok(DataSector { lba, form, subheader: Some(sub), data: raw[24..24 + len].to_vec() })
                    }
                    m => malformed(format!("LBA {lba} has sector mode {m}")),
                }
            }
        }
    }

    /// SHA-256 of a track's stored sectors as they read back (for a CHD, after
    /// decompression), so a track hashes the same in every image format.
    pub fn track_sha256(&self, t: &Track) -> Result<String> {
        let mut hasher = Sha256::new();
        for lba in t.first_lba..t.first_lba + t.sectors {
            hasher.update(self.read_raw(lba)?.0);
        }
        Ok(hex(&hasher.finalize()))
    }

    /// Write the disc into `dir` as `STEM (Track NN).bin` files and `STEM.cue`
    /// (plus `STEM.sbi` when LibCrypt data was loaded), for tools that only read
    /// CUE/BIN. Reopening the `.cue` gives the same tracks and sectors.
    pub fn write_bin_cue(&self, dir: &Path, stem: &str) -> Result<PathBuf> {
        use std::fmt::Write as _;
        use std::io::Write as _;
        let msf = |n: u32| format!("{:02}:{:02}:{:02}", n / 4500, n / 75 % 60, n % 75);
        let mut cue = String::new();
        for t in &self.tracks {
            let name = format!("{stem} (Track {:02}).bin", t.number);
            let path = dir.join(&name);
            let mut out = std::io::BufWriter::new(File::create(&path).map_err(|e| MediaError::io(&path, e))?);
            for lba in t.first_lba..t.first_lba + t.sectors {
                out.write_all(&self.read_raw(lba)?.0).map_err(|e| MediaError::io(&path, e))?;
            }
            out.flush().map_err(|e| MediaError::io(&path, e))?;
            let _ = writeln!(cue, "FILE \"{name}\" BINARY\n  TRACK {:02} {}", t.number, t.mode.cue_name());
            if t.unstored_pregap > 0 {
                let _ = writeln!(cue, "    PREGAP {}", msf(t.unstored_pregap));
            }
            if t.start_lba > t.first_lba {
                let _ = writeln!(cue, "    INDEX 00 00:00:00");
            }
            let _ = writeln!(cue, "    INDEX 01 {}", msf(t.start_lba - t.first_lba));
        }
        if let Some(src) = &self.subq_source {
            let sbi = dir.join(format!("{stem}.{}", src.extension().and_then(|e| e.to_str()).unwrap_or("sbi")));
            std::fs::copy(src, &sbi).map_err(|e| MediaError::io(&sbi, e))?;
        }
        let cue_path = dir.join(format!("{stem}.cue"));
        std::fs::write(&cue_path, cue).map_err(|e| MediaError::io(&cue_path, e))?;
        Ok(cue_path)
    }

    /// Read `count` consecutive 2048-byte Form 1 payloads (file system view).
    pub fn read_form1(&self, lba: u32, count: u32) -> Result<Vec<u8>> {
        let mut out = Vec::with_capacity(count as usize * 2048);
        for i in 0..count {
            let s = self.read_data(lba + i)?;
            out.extend_from_slice(&s.data[..2048]);
        }
        Ok(out)
    }
}

fn file_name(path: &Path) -> String {
    path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
}

/// Streaming SHA-256 of a whole file, hex encoded.
pub fn sha256_file(path: &Path) -> Result<String> {
    let mut f = File::open(path).map_err(|e| MediaError::io(path, e))?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = f.read(&mut buf).map_err(|e| MediaError::io(path, e))?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hex(&hasher.finalize()))
}

pub fn sha256_bytes(bytes: &[u8]) -> String {
    hex(&Sha256::digest(bytes))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sector(lba: u32, mode: u8) -> Vec<u8> {
        let mut s = vec![(lba % 251) as u8; 2352];
        s[..12].copy_from_slice(&SYNC);
        s[15] = mode;
        s
    }

    #[test]
    fn bin_cue_round_trip() {
        let dir = std::env::temp_dir().join(format!("psx-media-bincue-{}", std::process::id()));
        let out = dir.join("out");
        std::fs::create_dir_all(&out).unwrap();
        // Data track, then an audio track with a stored INDEX 00 and one with a PREGAP.
        let bin: Vec<u8> = (0..30).flat_map(|i| if i < 10 { sector(i, 2) } else { vec![i as u8; 2352] }).collect();
        std::fs::write(dir.join("g.bin"), bin).unwrap();
        std::fs::write(
            dir.join("g.cue"),
            "FILE \"g.bin\" BINARY\n  TRACK 01 MODE2/2352\n    INDEX 01 00:00:00\n  TRACK 02 AUDIO\n    INDEX 00 00:00:10\n    INDEX 01 00:00:12\n  TRACK 03 AUDIO\n    PREGAP 00:00:05\n    INDEX 01 00:00:20\n",
        )
        .unwrap();
        let disc = Disc::open(dir.join("g.cue")).unwrap();
        let copy = Disc::open(disc.write_bin_cue(&out, "g").unwrap()).unwrap();
        let geometry = |d: &Disc| {
            d.tracks
                .iter()
                .map(|t| (t.number, t.mode, t.first_lba, t.start_lba, t.sectors, t.unstored_pregap))
                .collect::<Vec<_>>()
        };
        assert_eq!(geometry(&copy), geometry(&disc));
        assert_eq!(copy.files.len(), 3);
        for (a, b) in disc.tracks.iter().zip(&copy.tracks) {
            assert_eq!(disc.track_sha256(a).unwrap(), copy.track_sha256(b).unwrap());
        }
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
