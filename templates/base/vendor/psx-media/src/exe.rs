//! PS-X EXE executables (psx-spx "CDROM File Formats").
//!
//! The BIOS loader copies `text_size` bytes from file offset 0x800 to
//! `text_addr`, zero-fills BSS if requested, sets GP/SP and jumps to `pc`.
//! That is the documented convention; a game may load further code (overlays)
//! its own way, which only runtime evidence can show.

/// Executable extensions treated as PS-X EXEs rather than disc images, lowercase.
pub const EXE_EXTENSIONS: &[&str] = &["exe", "ps-exe", "psx"];

/// Whether `path` has a PS-X EXE extension (case-insensitive).
pub fn is_exe_path(path: &std::path::Path) -> bool {
    path.extension().and_then(|e| e.to_str()).is_some_and(|e| EXE_EXTENSIONS.contains(&e.to_ascii_lowercase().as_str()))
}

use crate::error::{Result, malformed};
use crate::le::u32le;
use crate::memmap::{self, Region};

pub const HEADER_SIZE: usize = 0x800;
pub const MAGIC: &[u8; 8] = b"PS-X EXE";

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct PsxExe {
    pub pc: u32,
    pub gp: u32,
    pub text_addr: u32,
    pub text_size: u32,
    pub data_addr: u32,
    pub data_size: u32,
    pub bss_addr: u32,
    pub bss_size: u32,
    pub sp_base: u32,
    pub sp_offset: u32,
    /// ASCII region marker at 0x4C, e.g. "Sony Computer Entertainment Inc. for North America area".
    pub marker: String,
    /// File length; may exceed `0x800 + text_size` (trailing padding/data).
    pub file_size: usize,
    pub warnings: Vec<String>,
}

impl PsxExe {
    pub fn parse(bytes: &[u8]) -> Result<Self> {
        if bytes.len() < HEADER_SIZE || &bytes[..8] != MAGIC {
            return malformed("not a PS-X EXE (missing \"PS-X EXE\" header)");
        }
        let marker_bytes = &bytes[0x4C..HEADER_SIZE];
        let end = marker_bytes.iter().position(|&b| b == 0).unwrap_or(marker_bytes.len());
        let exe = PsxExe {
            pc: u32le(bytes, 0x10),
            gp: u32le(bytes, 0x14),
            text_addr: u32le(bytes, 0x18),
            text_size: u32le(bytes, 0x1C),
            data_addr: u32le(bytes, 0x20),
            data_size: u32le(bytes, 0x24),
            bss_addr: u32le(bytes, 0x28),
            bss_size: u32le(bytes, 0x2C),
            sp_base: u32le(bytes, 0x30),
            sp_offset: u32le(bytes, 0x34),
            marker: String::from_utf8_lossy(&marker_bytes[..end]).into_owned(),
            file_size: bytes.len(),
            warnings: Vec::new(),
        };
        Ok(exe.validated())
    }

    fn validated(mut self) -> Self {
        let mut w = Vec::new();
        if HEADER_SIZE + self.text_size as usize > self.file_size {
            w.push(format!(
                "text_size 0x{:X} exceeds file payload 0x{:X}; loader would read past end",
                self.text_size,
                self.file_size - HEADER_SIZE
            ));
        }
        if !self.text_size.is_multiple_of(0x800) {
            w.push(format!("text_size 0x{:X} is not a multiple of 0x800", self.text_size));
        }
        if memmap::resolve(self.text_addr).region != Region::MainRam {
            w.push(format!("text_addr 0x{:08X} is not in main RAM", self.text_addr));
        }
        if !self.text_range().contains(&self.pc) {
            w.push(format!("entry pc 0x{:08X} lies outside the loaded text", self.pc));
        }
        self.warnings = w;
        self
    }

    pub fn text_range(&self) -> std::ops::Range<u32> {
        self.text_addr..self.text_addr.wrapping_add(self.text_size)
    }

    /// Initial stack pointer, or `None` when the header leaves SP to the BIOS default.
    pub fn initial_sp(&self) -> Option<u32> {
        (self.sp_base != 0).then(|| self.sp_base.wrapping_add(self.sp_offset))
    }

    /// The bytes the loader copies to `text_addr`.
    pub fn payload<'a>(&self, bytes: &'a [u8]) -> &'a [u8] {
        let end = (HEADER_SIZE + self.text_size as usize).min(bytes.len());
        &bytes[HEADER_SIZE..end]
    }

    pub fn file_offset_to_addr(&self, offset: usize) -> Option<u32> {
        (offset >= HEADER_SIZE && offset < HEADER_SIZE + self.text_size as usize)
            .then(|| self.text_addr.wrapping_add((offset - HEADER_SIZE) as u32))
    }

    /// Accepts any RAM alias (KUSEG/KSEG0/KSEG1) of a loaded address.
    pub fn addr_to_file_offset(&self, addr: u32) -> Option<usize> {
        let want = memmap::resolve(addr);
        let base = memmap::resolve(self.text_addr);
        if want.region != Region::MainRam || base.region != Region::MainRam || want.offset < base.offset {
            return None;
        }
        let delta = want.offset - base.offset;
        (delta < self.text_size).then(|| HEADER_SIZE + delta as usize)
    }

    /// Region implied by the marker text, if recognisable.
    pub fn region_hint(&self) -> Option<&'static str> {
        let m = self.marker.to_ascii_lowercase();
        if m.contains("north america") {
            Some("NTSC-U")
        } else if m.contains("europe") {
            Some("PAL")
        } else if m.contains("japan") {
            Some("NTSC-J")
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Vec<u8> {
        let mut b = vec![0u8; HEADER_SIZE + 0x1000];
        b[..8].copy_from_slice(MAGIC);
        b[0x10..0x14].copy_from_slice(&0x8001_0100u32.to_le_bytes());
        b[0x18..0x1C].copy_from_slice(&0x8001_0000u32.to_le_bytes());
        b[0x1C..0x20].copy_from_slice(&0x1000u32.to_le_bytes());
        b[0x30..0x34].copy_from_slice(&0x801F_FF00u32.to_le_bytes());
        let m = b"Sony Computer Entertainment Inc. for North America area";
        b[0x4C..0x4C + m.len()].copy_from_slice(m);
        b
    }

    #[test]
    fn maps_offsets_and_aliases() {
        let bytes = sample();
        let exe = PsxExe::parse(&bytes).unwrap();
        assert!(exe.warnings.is_empty(), "{:?}", exe.warnings);
        assert_eq!(exe.region_hint(), Some("NTSC-U"));
        assert_eq!(exe.file_offset_to_addr(0x800), Some(0x8001_0000));
        assert_eq!(exe.file_offset_to_addr(0x7FF), None);
        assert_eq!(exe.addr_to_file_offset(0xA001_0010), Some(0x810));
        assert_eq!(exe.addr_to_file_offset(0x8001_1000), None);
        assert_eq!(exe.payload(&bytes).len(), 0x1000);
    }

    #[test]
    fn warns_on_truncated_payload() {
        let mut bytes = sample();
        bytes.truncate(HEADER_SIZE + 0x800);
        let exe = PsxExe::parse(&bytes).unwrap();
        assert!(exe.warnings.iter().any(|w| w.contains("exceeds file payload")));
    }
}
