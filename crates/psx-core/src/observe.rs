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

/// One `--trace-spu` record: a voice key on/off, a voice's first END block
/// after key on, or a transfer into sound RAM.
#[derive(Clone, Debug, Serialize)]
pub struct SpuEvent {
    pub frame: u64,
    pub cycle: u64,
    /// "key_on", "key_off", "end" or "upload".
    pub event: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub voice: Option<u32>,
    /// The storing instruction (key on/off, CPU uploads, the store that started a DMA).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pc: Option<u32>,
    #[serde(flatten)]
    pub key_on: Option<KeyOn>,
    #[serde(flatten)]
    pub upload: Option<SpuUpload>,
}

/// Voice settings at key on, and the sample it starts.
#[derive(Clone, Debug, Serialize)]
pub struct KeyOn {
    /// Sample start in sound RAM (bytes).
    pub start: u32,
    /// Raw pitch: 0x1000 plays the sample at 44.1 kHz.
    pub pitch: u16,
    pub rate_hz: f64,
    /// Left/right volume as a fraction of full scale; `None` for a sweep.
    pub volume: [Option<f64>; 2],
    pub volume_raw: [u16; 2],
    pub adsr: u32,
    pub noise: bool,
    pub pitch_modulated: bool,
    /// SHA-256 of the ADPCM bytes from `start` through the first END block,
    /// as VAB/VAG extraction reports per waveform (`adpcm_sha256`).
    pub sample_sha256: String,
    pub sample_bytes: u32,
    /// The END block also repeats (a looping sample).
    pub looped: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct SpuUpload {
    /// Destination in sound RAM (bytes).
    pub address: u32,
    pub bytes: u32,
    /// "dma" (channel 4) or "cpu" (manual FIFO writes, merged while contiguous).
    pub source: &'static str,
    /// Main RAM source of a DMA transfer.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ram: Option<u32>,
}

/// The `--trace-spu` log. Counts include events beyond the recording budget.
#[derive(Default)]
pub struct SpuLog {
    pub events: Vec<SpuEvent>,
    pub counts: std::collections::BTreeMap<&'static str, u64>,
    pub max: usize,
    /// Voices keyed on during the trace whose first END block is not yet
    /// reported. Silent voices keep walking sound RAM and set ENDX too, so
    /// voices not keyed on while tracing report no END.
    pub awaiting_end: u32,
    /// The last recorded event is a CPU upload ending at this address.
    cpu_upload_end: Option<u32>,
}

impl SpuLog {
    pub fn new(max: usize) -> Self {
        SpuLog {
            max,
            ..Default::default()
        }
    }

    pub fn record(&mut self, event: SpuEvent) {
        self.cpu_upload_end = None;
        *self.counts.entry(event.event).or_default() += 1;
        if self.events.len() < self.max {
            self.events.push(event);
        }
    }

    /// One halfword written through the transfer FIFO, merged into the last
    /// event while the CPU keeps writing consecutive addresses.
    pub fn cpu_upload(&mut self, frame: u64, cycle: u64, pc: u32, address: u32) {
        if self.cpu_upload_end == Some(address) {
            if let Some(upload) = self.events.last_mut().and_then(|e| e.upload.as_mut()) {
                upload.bytes += 2;
                self.cpu_upload_end = Some(address + 2);
                return;
            }
        }
        let upload = SpuUpload {
            address,
            bytes: 2,
            source: "cpu",
            ram: None,
        };
        let recorded = self.events.len() < self.max;
        self.record(SpuEvent {
            frame,
            cycle,
            event: "upload",
            voice: None,
            pc: Some(pc),
            key_on: None,
            upload: Some(upload),
        });
        // Merging needs the event in the log; unrecorded halfwords count individually.
        self.cpu_upload_end = recorded.then_some(address + 2);
    }
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
    pub spu: Option<SpuLog>,
    /// Every BIOS call counted by "<table>_<function>" (decimal), recorded or not.
    pub bios_counts: std::collections::BTreeMap<String, u64>,
}

/// Frequent BIOS calls that are counted but not recorded individually:
/// B(0Bh) TestEvent, B(17h) ReturnFromException, B(3Dh)/A(3Ch) putchar.
pub fn bios_call_is_quiet(table: &str, function: u32) -> bool {
    matches!(
        (table, function),
        ("B0", 0x0B) | ("B0", 0x17) | ("B0", 0x3D) | ("A0", 0x3C)
    )
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
