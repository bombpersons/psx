//! CHD (MAME "compressed hunks of data") CD images, decoded with the `chd` crate.
//!
//! A CD CHD stores 2448-byte frames (2352 bytes of sector data, then 96 of
//! subcode), several to a hunk. Tracks are listed in `CHT2`/`CHTR` metadata and
//! each one is padded to a multiple of [`TRACK_PADDING`] frames. Audio is stored
//! big-endian and is swapped back to the little-endian PCM a `.bin` holds. The
//! CD codecs strip sync/EDC/ECC from data sectors; the crate rebuilds them, so
//! a data frame reads back as the same 2352 bytes as the `.bin` it came from.

use std::fs::File;
use std::path::Path;
use std::sync::Mutex;

use chd::Chd;
use chd::metadata::{KnownMetadata, MetadataTag};

use crate::cue::TrackMode;
use crate::error::{MediaError, Result};

const FRAME_BYTES: usize = 2448;
const TRACK_PADDING: u32 = 4;

/// One track as listed in the CHD's metadata.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ChdTrack {
    pub number: u8,
    pub mode: TrackMode,
    /// Frames stored for this track, including a stored pregap.
    pub frames: u32,
    /// Pregap frames before INDEX 01.
    pub pregap: u32,
    /// Whether the pregap's frames are in the file (`PGTYPE:V...`).
    pub pregap_stored: bool,
    /// First frame of the track inside the CHD.
    pub chd_frame: u32,
}

pub(crate) struct ChdImage {
    inner: Mutex<Cache>,
}

/// The CHD and its most recently decoded hunk (reads are mostly sequential).
struct Cache {
    chd: Chd<File>,
    compressed: Vec<u8>,
    hunk: Vec<u8>,
    hunk_number: Option<u32>,
    frames_per_hunk: u32,
}

fn chd_error(path: &Path, e: chd::Error) -> MediaError {
    match e {
        chd::Error::RequiresParent => {
            MediaError::Unsupported(format!("{}: CHDs with a parent are not supported", path.display()))
        }
        e => MediaError::Malformed(format!("{}: {e}", path.display())),
    }
}

impl ChdImage {
    pub fn open(path: &Path) -> Result<(Self, Vec<ChdTrack>)> {
        let file = File::open(path).map_err(|e| MediaError::io(path, e))?;
        let mut chd = Chd::open(file, None).map_err(|e| chd_error(path, e))?;
        if chd.header().has_parent() {
            return Err(chd_error(path, chd::Error::RequiresParent));
        }
        let hunk_bytes = chd.header().hunk_size() as usize;
        if chd.header().unit_bytes() as usize != FRAME_BYTES || !hunk_bytes.is_multiple_of(FRAME_BYTES) {
            return Err(MediaError::Unsupported(format!("{}: not a CD-ROM CHD", path.display())));
        }
        let mut texts = Vec::new();
        let refs = chd.metadata_refs().collect::<chd::Result<Vec<_>>>().map_err(|e| chd_error(path, e))?;
        for meta in refs {
            let tag = meta.metatag();
            if tag == KnownMetadata::CdRomTrack2 as u32 || tag == KnownMetadata::CdRomTrack as u32 {
                let m = meta.read(chd.inner()).map_err(|e| chd_error(path, e))?;
                texts.push(String::from_utf8_lossy(&m.value).trim_end_matches('\0').to_string());
            } else if KnownMetadata::is_cdrom(tag) {
                return Err(MediaError::Unsupported(format!(
                    "{}: legacy or GD-ROM CHD track metadata",
                    path.display()
                )));
            }
        }
        let tracks = parse_tracks(&texts)?;
        let total = chd.header().hunk_count() as u64 * (hunk_bytes / FRAME_BYTES) as u64;
        if tracks.last().is_some_and(|t| (t.chd_frame + t.frames) as u64 > total) {
            return Err(MediaError::Malformed(format!("{}: tracks extend past the end of the CHD", path.display())));
        }
        let cache = Cache {
            compressed: Vec::new(),
            hunk: chd.get_hunksized_buffer(),
            hunk_number: None,
            frames_per_hunk: (hunk_bytes / FRAME_BYTES) as u32,
            chd,
        };
        Ok((ChdImage { inner: Mutex::new(cache) }, tracks))
    }

    /// The stored bytes of CHD frame `frame` for a track of `mode`, laid out as in a `.bin`/`.iso`.
    pub fn read_frame(&self, path: &Path, frame: u32, mode: TrackMode) -> Result<Vec<u8>> {
        let mut c = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        let hunk = frame / c.frames_per_hunk;
        if c.hunk_number != Some(hunk) {
            c.hunk_number = None;
            let Cache { chd, compressed, hunk: buf, .. } = &mut *c;
            chd.hunk(hunk).and_then(|mut h| h.read_hunk_in(compressed, buf)).map_err(|e| chd_error(path, e))?;
            c.hunk_number = Some(hunk);
        }
        let at = (frame % c.frames_per_hunk) as usize * FRAME_BYTES;
        let mut out = c.hunk[at..at + mode.sector_size() as usize].to_vec();
        if mode == TrackMode::Audio {
            out.as_chunks_mut::<2>().0.iter_mut().for_each(|s| s.swap(0, 1));
        }
        Ok(out)
    }
}

fn field<'a>(text: &'a str, key: &str) -> Option<&'a str> {
    text.split_whitespace().find_map(|kv| kv.strip_prefix(key)?.strip_prefix(':'))
}

/// Tracks from `CHT2` (`TRACK:1 TYPE:MODE2_RAW SUBTYPE:NONE FRAMES:n PREGAP:n PGTYPE:.. ...`)
/// or `CHTR` (no pregap fields) entries.
pub(crate) fn parse_tracks(texts: &[String]) -> Result<Vec<ChdTrack>> {
    let bad = |t: &str, what: &str| MediaError::Malformed(format!("CHD track metadata {t:?}: {what}"));
    let mut tracks = Vec::new();
    let mut chd_frame = 0;
    for t in texts {
        let num =
            |key: &str| -> Result<u32> { field(t, key).ok_or_else(|| bad(t, key))?.parse().map_err(|_| bad(t, key)) };
        let number = num("TRACK")?;
        let mode = match field(t, "TYPE").ok_or_else(|| bad(t, "TYPE"))? {
            "MODE1_RAW" => TrackMode::Mode1Raw,
            "MODE1" => TrackMode::Mode1Cooked,
            "MODE2_RAW" => TrackMode::Mode2Raw,
            "AUDIO" => TrackMode::Audio,
            other => return Err(MediaError::Unsupported(format!("CHD track type {other}"))),
        };
        let frames = num("FRAMES")?;
        let pregap = if field(t, "PREGAP").is_some() { num("PREGAP")? } else { 0 };
        let pregap_stored = field(t, "PGTYPE").is_some_and(|p| p.starts_with('V'));
        if number as usize != tracks.len() + 1 || (pregap_stored && pregap > frames) {
            return Err(bad(t, "track out of order or pregap larger than the track"));
        }
        tracks.push(ChdTrack { number: number as u8, mode, frames, pregap, pregap_stored, chd_frame });
        chd_frame += frames.div_ceil(TRACK_PADDING) * TRACK_PADDING;
    }
    if tracks.is_empty() {
        return Err(MediaError::Unsupported("CHD has no CD-ROM track metadata".into()));
    }
    Ok(tracks)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_cht2() {
        let t = parse_tracks(&[
            "TRACK:1 TYPE:MODE2_RAW SUBTYPE:NONE FRAMES:1001 PREGAP:0 PGTYPE:MODE1 PGSUB:NONE POSTGAP:0".into(),
            "TRACK:2 TYPE:AUDIO SUBTYPE:NONE FRAMES:300 PREGAP:150 PGTYPE:VAUDIO PGSUB:NONE POSTGAP:0".into(),
        ])
        .unwrap();
        assert_eq!(
            t[0],
            ChdTrack {
                number: 1,
                mode: TrackMode::Mode2Raw,
                frames: 1001,
                pregap: 0,
                pregap_stored: false,
                chd_frame: 0
            }
        );
        assert_eq!(
            t[1],
            ChdTrack {
                number: 2,
                mode: TrackMode::Audio,
                frames: 300,
                pregap: 150,
                pregap_stored: true,
                chd_frame: 1004
            }
        );
        assert!(parse_tracks(&["TRACK:1 TYPE:MODE2_FORM1 SUBTYPE:NONE FRAMES:4".into()]).is_err());
        assert!(parse_tracks(&["TRACK:2 TYPE:AUDIO SUBTYPE:NONE FRAMES:4".into()]).is_err());
    }
}
