//! Subchannel Q overrides for copy-protected discs (LibCrypt).
//!
//! LibCrypt-protected PAL discs have ~32 sectors whose subchannel Q data was
//! deliberately corrupted at mastering; the game seeks to them and compares
//! GetlocP responses. BIN/CUE rips do not carry subchannel data, so the
//! corrupted Q is distributed separately (redump.org) as:
//!
//! - `.sbi`: `"SBI\0"`, then records of BCD absolute MSF (3 bytes), a type
//!   byte, and data: type 1 = the 10 Q bytes; type 2 = relative MSF (3);
//!   type 3 = absolute MSF (3).
//! - `.lsd`: records of BCD absolute MSF (3 bytes) + 12 Q bytes (10 + CRC).
//!
//! Q layout: control/ADR, track, index, rel M S F, zero, abs M S F.
//!
//! The drive checks each sector's Q CRC and keeps its last *valid* Q when
//! one fails, so a LibCrypt sector makes GetlocP repeat the previous
//! position; the game derives its key from which sectors do that. `.sbi`
//! files carry no CRC (their entries are the bad sectors); `.lsd` CRCs are
//! checked.

use std::collections::BTreeMap;
use std::path::Path;

use crate::{MediaError, Result};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SubQ {
    /// The full 10-byte Q, and whether its CRC is valid.
    Full([u8; 10], bool),
    /// Only the relative MSF (Q bytes 3..6) differs.
    Relative([u8; 3]),
    /// Only the absolute MSF (Q bytes 7..10) differs.
    Absolute([u8; 3]),
}

impl SubQ {
    /// Whether a drive accepts this Q (CRC valid); `.sbi` entries never are.
    pub fn crc_valid(&self) -> bool {
        matches!(self, SubQ::Full(_, true))
    }

    /// Apply to a computed Q.
    pub fn apply(&self, q: &mut [u8; 10]) {
        match self {
            SubQ::Full(v, _) => *q = *v,
            SubQ::Relative(v) => q[3..6].copy_from_slice(v),
            SubQ::Absolute(v) => q[7..10].copy_from_slice(v),
        }
    }
}

/// Subchannel Q CRC: CRC-16/CCITT (poly 1021h, init 0), inverted, big-endian.
pub fn crc(q: &[u8; 10]) -> u16 {
    let mut c: u16 = 0;
    for &b in q {
        c ^= (b as u16) << 8;
        for _ in 0..8 {
            c = if c & 0x8000 != 0 { (c << 1) ^ 0x1021 } else { c << 1 };
        }
    }
    !c
}

fn unbcd(v: u8) -> u32 {
    (v >> 4) as u32 * 10 + (v & 15) as u32
}

fn lba_of(msf: &[u8]) -> Option<u32> {
    (unbcd(msf[0]) * 4500 + unbcd(msf[1]) * 75 + unbcd(msf[2])).checked_sub(150)
}

/// Parse `.sbi` or `.lsd` bytes into overrides keyed by LBA.
pub fn parse(bytes: &[u8], lsd: bool) -> Result<BTreeMap<u32, SubQ>> {
    let bad = |m: &str| MediaError::Malformed(format!("subchannel file: {m}"));
    let mut out = BTreeMap::new();
    if lsd {
        if !bytes.len().is_multiple_of(15) {
            return Err(bad("LSD size is not a multiple of 15"));
        }
        for r in bytes.as_chunks::<15>().0 {
            let lba = lba_of(&r[..3]).ok_or_else(|| bad("MSF before 00:02:00"))?;
            let q: [u8; 10] = r[3..13].try_into().unwrap();
            out.insert(lba, SubQ::Full(q, crc(&q) == u16::from_be_bytes([r[13], r[14]])));
        }
        return Ok(out);
    }
    if bytes.get(..4) != Some(b"SBI\0") {
        return Err(bad("missing SBI header"));
    }
    let mut i = 4;
    while i < bytes.len() {
        let rec = bytes.get(i..i + 4).ok_or_else(|| bad("truncated record"))?;
        let lba = lba_of(&rec[..3]).ok_or_else(|| bad("MSF before 00:02:00"))?;
        let (q, len) = match rec[3] {
            1 => (bytes.get(i + 4..i + 14).map(|d| SubQ::Full(d.try_into().unwrap(), false)), 10),
            2 => (bytes.get(i + 4..i + 7).map(|d| SubQ::Relative(d.try_into().unwrap())), 3),
            3 => (bytes.get(i + 4..i + 7).map(|d| SubQ::Absolute(d.try_into().unwrap())), 3),
            t => return Err(bad(&format!("unknown record type {t}"))),
        };
        out.insert(lba, q.ok_or_else(|| bad("truncated record"))?);
        i += 4 + len;
    }
    Ok(out)
}

/// Overrides from `<image>.sbi` or `<image>.lsd` next to the image, if present.
pub fn load_beside(image: &Path) -> Result<Option<(std::path::PathBuf, BTreeMap<u32, SubQ>)>> {
    for (ext, lsd) in [("sbi", false), ("SBI", false), ("lsd", true), ("LSD", true)] {
        let p = image.with_extension(ext);
        if p.is_file() {
            let bytes = std::fs::read(&p).map_err(|e| MediaError::io(&p, e))?;
            return Ok(Some((p, parse(&bytes, lsd)?)));
        }
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_sbi_records() {
        // 03:08:05 type 1 (full Q), then 03:08:06 type 3 (absolute MSF).
        let mut b = b"SBI\0".to_vec();
        b.extend([0x03, 0x08, 0x05, 1, 0x41, 0x01, 0x01, 0x03, 0x06, 0x05, 0x00, 0x03, 0x08, 0x05]);
        b.extend([0x03, 0x08, 0x06, 3, 0x03, 0x0A, 0x06]);
        let m = parse(&b, false).unwrap();
        let lba = 3 * 4500 + 8 * 75 + 5 - 150;
        assert_eq!(m.len(), 2);
        assert!(matches!(m[&lba], SubQ::Full(q, false) if q[4] == 0x06));
        let mut q = [0u8; 10];
        m[&(lba + 1)].apply(&mut q);
        assert_eq!(&q[7..], &[0x03, 0x0A, 0x06]);
    }

    #[test]
    fn parses_lsd_records() {
        let mut b = vec![0x03, 0x08, 0x05];
        b.extend([0x41, 0x01, 0x01, 0x03, 0x06, 0x05, 0x00, 0x03, 0x08, 0x05, 0xAA, 0xBB]);
        let m = parse(&b, true).unwrap();
        assert_eq!(m.len(), 1);
        assert!(!m.values().next().unwrap().crc_valid());
        // A record with its correct CRC is accepted.
        let q: [u8; 10] = b[3..13].try_into().unwrap();
        b[13..15].copy_from_slice(&crc(&q).to_be_bytes());
        assert!(parse(&b, true).unwrap().values().next().unwrap().crc_valid());
        assert!(parse(&b[..14], true).is_err());
    }
}
