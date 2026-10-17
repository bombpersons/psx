//! Disc geometry and sector access over CUE/BIN, bare raw BIN and cooked ISO.
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
use crate::error::{malformed, MediaError, Result};

pub const SYNC: [u8; 12] = [
    0, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0,
];
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
    /// Sector offset inside the file of the first stored sector (INDEX 00 if present).
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
    handles: Mutex<Vec<File>>,
    /// Subchannel Q overrides (LibCrypt) by LBA, from an `.sbi`/`.lsd` beside the image.
    pub subq: std::collections::BTreeMap<u32, crate::subq::SubQ>,
    pub subq_source: Option<PathBuf>,
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

fn file_len(path: &Path) -> Result<u64> {
    std::fs::metadata(path)
        .map(|m| m.len())
        .map_err(|e| MediaError::io(path, e))
}

impl Disc {
    /// Open a `.cue`, a bare raw `.bin` (single data track) or a cooked `.iso`.
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
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        match ext.as_str() {
            "cue" => Self::open_cue(path),
            "iso" => Self::open_single(path, TrackMode::Mode1Cooked),
            "bin" | "img" => {
                let mut head = [0u8; 16];
                let mut f = File::open(path).map_err(|e| MediaError::io(path, e))?;
                f.read_exact(&mut head)
                    .map_err(|e| MediaError::io(path, e))?;
                if head[..12] != SYNC {
                    return Err(MediaError::Unsupported(
                        "bare .bin without CD sync pattern; supply its .cue".into(),
                    ));
                }
                let mode = if head[15] == 1 {
                    TrackMode::Mode1Raw
                } else {
                    TrackMode::Mode2Raw
                };
                Self::open_single(path, mode)
            }
            other => Err(MediaError::Unsupported(format!(
                "disc image extension .{other}"
            ))),
        }
    }

    fn open_single(path: &Path, mode: TrackMode) -> Result<Self> {
        let bytes = file_len(path)?;
        let ss = mode.sector_size();
        if bytes % ss != 0 {
            return malformed(format!(
                "{} is not a whole number of {ss}-byte sectors",
                path.display()
            ));
        }
        let handle = File::open(path).map_err(|e| MediaError::io(path, e))?;
        Ok(Disc {
            source: path.to_path_buf(),
            files: vec![ImageFile {
                name: file_name(path),
                path: path.to_path_buf(),
                bytes,
            }],
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
            handles: Mutex::new(vec![handle]),
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
                return Err(MediaError::Unsupported(format!(
                    "mixed sector sizes in {}",
                    cf.name
                )));
            }
            if bytes % ss != 0 {
                return malformed(format!(
                    "{} is not a whole number of {ss}-byte sectors",
                    cf.name
                ));
            }
            let file_sectors = (bytes / ss) as u32;
            let mut shift = 0;
            for (i, t) in cf.tracks.iter().enumerate() {
                shift += t.pregap;
                let first = t.first_stored();
                let end = cf
                    .tracks
                    .get(i + 1)
                    .map_or(file_sectors, |n| n.first_stored());
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
            files.push(ImageFile {
                name: cf.name.clone(),
                path: fpath,
                bytes,
            });
        }
        Ok(Disc {
            source: path.to_path_buf(),
            files,
            tracks,
            handles: Mutex::new(handles),
            subq: Default::default(),
            subq_source: None,
        })
    }

    pub fn track_for_lba(&self, lba: u32) -> Option<&Track> {
        self.tracks.iter().find(|t| t.contains(lba))
    }

    /// Total addressable sectors (end of the last stored sector).
    pub fn end_lba(&self) -> u32 {
        self.tracks
            .iter()
            .map(|t| t.first_lba + t.sectors)
            .max()
            .unwrap_or(0)
    }

    /// Read the stored bytes of one sector exactly as they appear in the image.
    pub fn read_raw(&self, lba: u32) -> Result<(Vec<u8>, TrackMode)> {
        let t = self.track_for_lba(lba).ok_or_else(|| {
            MediaError::NotFound(format!("LBA {lba} is not stored in this image"))
        })?;
        let ss = t.mode.sector_size();
        let offset = (t.file_sector as u64 + (lba - t.first_lba) as u64) * ss;
        let mut buf = vec![0u8; ss as usize];
        let mut handles = self.handles.lock().unwrap_or_else(|p| p.into_inner());
        let h = &mut handles[t.file];
        let path = &self.files[t.file].path;
        h.seek(SeekFrom::Start(offset))
            .map_err(|e| MediaError::io(path, e))?;
        h.read_exact(&mut buf)
            .map_err(|e| MediaError::io(path, e))?;
        Ok((buf, t.mode))
    }

    /// Read a data sector's user data, honouring Mode 2 Form 1/Form 2.
    pub fn read_data(&self, lba: u32) -> Result<DataSector> {
        let (raw, mode) = self.read_raw(lba)?;
        match mode {
            TrackMode::Audio => Err(MediaError::NotFound(format!(
                "LBA {lba} is in an audio track"
            ))),
            TrackMode::Mode1Cooked => Ok(DataSector {
                lba,
                form: SectorForm::Mode1,
                subheader: None,
                data: raw,
            }),
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
                        let (form, len) = if sub[2] & 0x20 != 0 {
                            (SectorForm::Form2, 2324)
                        } else {
                            (SectorForm::Form1, 2048)
                        };
                        Ok(DataSector {
                            lba,
                            form,
                            subheader: Some(sub),
                            data: raw[24..24 + len].to_vec(),
                        })
                    }
                    m => malformed(format!("LBA {lba} has sector mode {m}")),
                }
            }
        }
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
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
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
