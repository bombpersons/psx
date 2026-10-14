//! Native save states: the complete machine (CPU, GTE, RAM, scratchpad, I/O
//! registers, DMA, timers, GPU + VRAM, CD-ROM drive, pad port, MDEC, SPU +
//! sound RAM) so a run split at any instruction continues exactly as an
//! unsplit one.
//!
//! File layout:
//! - `PSXSTATE` magic;
//! - little-endian `u32` header length and a JSON [`Header`] (readable without
//!   decoding the body: format, core version, BIOS and media identity);
//! - gzip-compressed postcard encoding of the machine.
//!
//! Not saved: the BIOS image and the disc (both are reopened from files and
//! checked against the header), and analysis state (watches, traces, logs,
//! captured audio), which belongs to the run, not the machine.

use std::io::{Read, Write};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::Machine;

pub const MAGIC: &[u8; 8] = b"PSXSTATE";
/// Bumped whenever the encoded machine changes shape; older states are rejected.
pub const FORMAT: u32 = 7;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MediaId {
    /// "exe" (side-loaded PS-X EXE: SHA-256 of the file) or "disc" (see [`disc_fingerprint`]).
    pub kind: String,
    pub sha256: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Header {
    pub format: u32,
    pub core_version: String,
    pub bios_sha256: String,
    pub media: Option<MediaId>,
    /// Vblanks and system clocks since power-on when the state was taken.
    pub frames: u64,
    pub cycles: u64,
}

#[derive(Debug)]
pub enum StateError {
    NotAState,
    Format(u32),
    Corrupt(String),
    BiosMismatch { expected: String, actual: String },
}

impl std::fmt::Display for StateError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StateError::NotAState => write!(f, "not a psx-core save state (PCSX-Redux states need --backend redux)"),
            StateError::Format(v) => write!(f, "save state format {v} is not supported (this core writes format {FORMAT})"),
            StateError::Corrupt(e) => write!(f, "corrupt save state: {e}"),
            StateError::BiosMismatch { expected, actual } => {
                write!(f, "state was saved with BIOS sha256 {expected}, but this BIOS is {actual}")
            }
        }
    }
}

impl std::error::Error for StateError {}

pub fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes).iter().map(|b| format!("{b:02x}")).collect()
}

/// Identity of a disc image without hashing hundreds of megabytes: the track
/// layout plus the raw system area and volume descriptors (LBA 0..24) and
/// every 4096th sector after that.
pub fn disc_fingerprint(disc: &psx_media::Disc) -> String {
    let mut h = Sha256::new();
    h.update(disc_layout(disc).as_bytes());
    let end = disc.end_lba();
    let mut lba = 0;
    while lba < end {
        if let Ok((raw, _)) = disc.read_raw(lba) {
            h.update(lba.to_le_bytes());
            h.update(&raw);
        }
        lba = if lba < 24 { lba + 1 } else { (lba / 4096 + 1) * 4096 };
    }
    h.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

fn disc_layout(disc: &psx_media::Disc) -> String {
    disc.tracks
        .iter()
        .map(|t| format!("{} {:?} {} {} {} {};", t.number, t.mode, t.first_lba, t.start_lba, t.sectors, t.unstored_pregap))
        .chain(disc.subq.iter().map(|(lba, q)| format!("subq {lba} {q:?};")))
        .collect()
}

/// Read only the header of a state file.
pub fn read_header(bytes: &[u8]) -> Result<(Header, usize), StateError> {
    if bytes.len() < 12 || &bytes[..8] != MAGIC {
        return Err(StateError::NotAState);
    }
    let len = u32::from_le_bytes(bytes[8..12].try_into().unwrap()) as usize;
    let json = bytes.get(12..12 + len).ok_or_else(|| StateError::Corrupt("truncated header".into()))?;
    let header: Header = serde_json::from_slice(json).map_err(|e| StateError::Corrupt(e.to_string()))?;
    if header.format != FORMAT {
        return Err(StateError::Format(header.format));
    }
    Ok((header, 12 + len))
}

impl Machine {
    /// Encode the machine. `media` records what is inserted so a restore can
    /// check it reopened the same thing.
    pub fn save_state(&self, media: Option<MediaId>) -> Vec<u8> {
        let header = Header {
            format: FORMAT,
            core_version: env!("CARGO_PKG_VERSION").into(),
            bios_sha256: sha256_hex(&self.bus.bios),
            media,
            frames: self.bus.frames,
            cycles: self.bus.now,
        };
        let json = serde_json::to_vec(&header).expect("header serialises");
        let body = postcard::to_stdvec(self).expect("machine serialises");
        let mut out = Vec::with_capacity(body.len() / 4);
        out.extend_from_slice(MAGIC);
        out.extend_from_slice(&(json.len() as u32).to_le_bytes());
        out.extend_from_slice(&json);
        let mut gz = flate2::write::GzEncoder::new(out, flate2::Compression::fast());
        gz.write_all(&body).expect("in-memory write");
        gz.finish().expect("in-memory write")
    }

    /// Decode a state saved with [`Machine::save_state`]. The BIOS must be the
    /// one the state was saved with; the caller reinserts the disc (set
    /// `bus.cdrom.disc` directly: [`Machine::insert_disc`] resets the drive)
    /// after checking it against `header.media`.
    pub fn load_state(bytes: &[u8], bios: Vec<u8>) -> Result<(Header, Machine), StateError> {
        let (header, at) = read_header(bytes)?;
        let mut m = Machine::new(bios);
        let actual = sha256_hex(&m.bus.bios);
        if actual != header.bios_sha256 {
            return Err(StateError::BiosMismatch { expected: header.bios_sha256, actual });
        }
        let mut body = Vec::new();
        flate2::read::GzDecoder::new(&bytes[at..]).read_to_end(&mut body).map_err(|e| StateError::Corrupt(e.to_string()))?;
        let mut restored: Machine = postcard::from_bytes(&body).map_err(|e| StateError::Corrupt(e.to_string()))?;
        restored.bus.bios = std::mem::take(&mut m.bus.bios);
        restored.bus.after_restore();
        Ok((header, restored))
    }
}

/// Serde for fixed-size arrays longer than serde's built-in 32.
pub(crate) mod big_array {
    use serde::de::Error;
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    pub fn serialize<S: Serializer, T: Serialize, const N: usize>(a: &[T; N], s: S) -> Result<S::Ok, S::Error> {
        a.as_slice().serialize(s)
    }

    pub fn deserialize<'de, D: Deserializer<'de>, T: Deserialize<'de>, const N: usize>(d: D) -> Result<[T; N], D::Error> {
        let v = Vec::<T>::deserialize(d)?;
        let n = v.len();
        v.try_into().map_err(|_| D::Error::invalid_length(n, &"a fixed-size array"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A BIOS that counts in $t0 and stores it to RAM forever.
    fn counting_bios() -> Vec<u8> {
        let code = [0x2508_0001u32, 0xAC08_0000, 0x0BF0_0000, 0];
        code.iter().flat_map(|w| w.to_le_bytes()).collect()
    }

    #[test]
    fn split_run_equals_straight_run() {
        let mut straight = Machine::new(counting_bios());
        for _ in 0..200_000 {
            straight.step();
        }
        let saved = straight.save_state(None);
        let (header, mut resumed) = Machine::load_state(&saved, counting_bios()).unwrap();
        assert_eq!(header.cycles, straight.bus.now);
        for _ in 0..300_000 {
            straight.step();
            resumed.step();
        }
        assert_eq!(straight.cpu.regs[8], 500_000 / 4); // addiu, sw, j, delay slot
        assert_eq!(straight.save_state(None), resumed.save_state(None));
    }

    #[test]
    fn rejects_other_bios_and_garbage() {
        let saved = Machine::new(counting_bios()).save_state(None);
        assert!(matches!(Machine::load_state(&saved, vec![0; 16]), Err(StateError::BiosMismatch { .. })));
        assert!(matches!(Machine::load_state(b"\x0a\x1b garbage", counting_bios()), Err(StateError::NotAState)));
        let mut truncated = saved.clone();
        truncated.truncate(saved.len() - 100);
        assert!(matches!(Machine::load_state(&truncated, counting_bios()), Err(StateError::Corrupt(_))));
    }
}
