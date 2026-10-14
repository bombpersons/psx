//! Observation hooks for analysis runs. Everything here is off by default and
//! costs one branch per store when no watch is armed.
//!
//! Unlike the Redux backend (Lua breakpoints, decoded store values, inferred
//! CD sectors), these records come from the emulated hardware itself:
//! - CPU writes carry the storing instruction's PC, width, old and new value;
//! - DMA writes carry the channel and the whole transferred range;
//! - CD transfers carry the exact LBA and FIFO offset the drive delivered.

use serde::Serialize;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Writer {
    Cpu,
    Dma,
}

#[derive(Clone, Debug, Serialize)]
pub struct WriteHit {
    pub frame: u64,
    pub cycle: u64,
    pub writer: Writer,
    /// Virtual address of a CPU store, or start of a DMA transfer.
    pub address: u32,
    /// Store width, or DMA transfer length in bytes.
    pub width: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pc: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value: Option<u32>,
    /// Value at the stored location before the write (CPU stores).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub old: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dma_channel: Option<u32>,
}

#[derive(Clone, Debug, Serialize)]
pub struct CdTransfer {
    pub frame: u64,
    pub cycle: u64,
    /// Sector whose data was in the FIFO (no inference involved).
    pub lba: u32,
    /// Offset inside that sector's FIFO payload.
    pub fifo_offset: u32,
    pub bytes: u32,
    /// 2048 or 2340 (Setmode bit 5).
    pub sector_size: u32,
    pub ram: u32,
    /// PC of the store that started the DMA.
    pub pc: u32,
}

#[derive(Clone, Debug, Serialize)]
pub struct BiosCall {
    pub frame: u64,
    pub cycle: u64,
    /// "A0", "B0" or "C0".
    pub table: &'static str,
    pub function: u32,
    pub args: [u32; 4],
    pub ra: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
}

#[derive(Default)]
pub struct Observer {
    /// Physical RAM ranges `[start, end)` to watch.
    pub watches: Vec<(u32, u32)>,
    pub hits: Vec<WriteHit>,
    pub hit_count: u64,
    pub max_hits: usize,
    pub cd: Option<Vec<CdTransfer>>,
    pub bios: Option<Vec<BiosCall>>,
    /// Every BIOS call counted by "<table>_<function>" (decimal), recorded or not.
    pub bios_counts: std::collections::BTreeMap<String, u64>,
}

/// Frequent BIOS calls that are counted but not recorded individually:
/// B(0Bh) TestEvent, B(17h) ReturnFromException, B(3Dh)/A(3Ch) putchar.
pub fn bios_call_is_quiet(table: &str, function: u32) -> bool {
    matches!((table, function), ("B0", 0x0B) | ("B0", 0x17) | ("B0", 0x3D) | ("A0", 0x3C))
}

impl Observer {
    pub fn watch(&mut self, addr: u32, width: u32) {
        let p = addr & 0x1F_FFFF;
        self.watches.push((p, p + width.max(1)));
        if self.max_hits == 0 {
            self.max_hits = 10_000;
        }
    }

    #[inline]
    pub fn active(&self) -> bool {
        !self.watches.is_empty()
    }

    pub fn overlaps(&self, phys: u32, len: u32) -> bool {
        let (a, b) = (phys & 0x1F_FFFF, (phys & 0x1F_FFFF) + len);
        self.watches.iter().any(|&(s, e)| a < e && b > s)
    }

    pub fn record(&mut self, hit: WriteHit) {
        self.hit_count += 1;
        if self.hits.len() < self.max_hits {
            self.hits.push(hit);
        }
    }
}
