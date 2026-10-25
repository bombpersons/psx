//! Observation hooks for analysis runs. Everything here is off by default and
//! costs one branch per store when no watch is armed.
//!
//! Unlike the Redux backend (Lua breakpoints, decoded store values, inferred
//! CD sectors), these records come from the emulated hardware itself:
//! - CPU writes carry the storing instruction's PC, width, old and new value;
//! - DMA writes carry the channel and the whole transferred range;
//! - CD transfers carry the exact LBA and FIFO offset the drive delivered.

use std::collections::HashMap;
use std::hash::{BuildHasherDefault, Hasher};

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
        SpuLog { max, ..Default::default() }
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
        let upload = SpuUpload { address, bytes: 2, source: "cpu", ram: None };
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

/// Stores into a range counted rather than recorded (a game's tick counter):
/// from `pc` only when it is set, compared by physical address.
#[derive(Clone, Debug)]
pub struct Counter {
    pub range: (u32, u32),
    pub pc: Option<u32>,
    pub count: u64,
}

/// Every CPU store into main RAM, counted per storing PC, address and width,
/// with each pair's stores per field folded into a signature: pairs stored
/// in the same fields, as many times, share one. The code a game's tick runs
/// shares the tick's (finding the tick, `--find-tick`).
#[derive(Default)]
pub struct Census {
    pub pairs: HashMap<u64, Pair, BuildHasherDefault<Mix>>,
    touched: Vec<u64>,
    /// Fields ended so far.
    pub fields: u32,
}

/// One storing PC, address and width in a [`Census`].
#[derive(Clone, Debug, Default)]
pub struct Pair {
    now: u32,
    pub signature: u64,
    pub stores: u64,
    /// Fields with 1, 2, 3, and 4 or more stores (the rest had none).
    pub by_count: [u32; 4],
    /// New value minus old, masked to the width, at the first store.
    pub first_step: u32,
    /// Stores that added `first_step`, and stores that changed the value.
    pub same_step: u64,
    pub changes: u64,
    /// The value before the first store and after the last: a counter only
    /// this pair stores moves `first_step` a store between them.
    pub first_old: u32,
    pub last: u32,
}

impl Census {
    /// The key's (physical address, width, PC).
    pub fn parts(key: u64) -> (u32, u32, u32) {
        ((key & 0x1F_FFFF) as u32, ((key >> 24) & 0xFF) as u32, (key >> 32) as u32)
    }

    fn store(&mut self, addr: u32, width: u32, pc: u32, value: u32, old: u32) {
        let key = (u64::from(pc & 0x1FFF_FFFF) << 32) | (u64::from(width) << 24) | u64::from(addr & 0x1F_FFFF);
        let mask = u32::MAX >> (32 - 8 * width.clamp(1, 4));
        let step = value.wrapping_sub(old) & mask;
        let p = self.pairs.entry(key).or_default();
        if p.now == 0 {
            self.touched.push(key);
        }
        if p.stores == 0 {
            p.first_step = step;
            p.first_old = old & mask;
        }
        p.last = value & mask;
        p.now += 1;
        p.stores += 1;
        p.same_step += u64::from(step == p.first_step);
        p.changes += u64::from(step != 0);
    }

    /// A field boundary: fold this field's counts into the signatures.
    pub fn end_field(&mut self) {
        let f = u64::from(self.fields);
        for key in self.touched.drain(..) {
            let p = self.pairs.get_mut(&key).expect("touched pair");
            let n = std::mem::take(&mut p.now);
            p.signature = mix(p.signature ^ (f << 32 | u64::from(n)));
            p.by_count[(n as usize).min(4) - 1] += 1;
        }
        self.fields += 1;
    }
}

fn mix(mut x: u64) -> u64 {
    x = (x ^ (x >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    x ^ (x >> 31)
}

/// A fast hasher for the census's integer keys.
#[derive(Default)]
pub struct Mix(u64);

impl Hasher for Mix {
    fn finish(&self) -> u64 {
        mix(self.0)
    }
    fn write(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.0 = self.0.rotate_left(8) ^ u64::from(b);
        }
    }
    fn write_u64(&mut self, x: u64) {
        self.0 ^= x;
    }
}

#[derive(Default)]
pub struct Observer {
    /// Physical RAM ranges `[start, end)` to watch.
    pub watches: Vec<(u32, u32)>,
    pub counters: Vec<Counter>,
    pub census: Option<Box<Census>>,
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

    /// Count stores into `[addr, addr + width)`, from `pc` if given.
    pub fn count(&mut self, addr: u32, width: u32, pc: Option<u32>) -> usize {
        let p = addr & 0x1F_FFFF;
        self.counters.push(Counter { range: (p, p + width.max(1)), pc: pc.map(|pc| pc & 0x1FFF_FFFF), count: 0 });
        self.counters.len() - 1
    }

    #[inline]
    pub fn active(&self) -> bool {
        !self.watches.is_empty() || !self.counters.is_empty() || self.census.is_some()
    }

    pub fn overlaps(&self, phys: u32, len: u32) -> bool {
        if self.census.is_some() {
            return true;
        }
        let (a, b) = (phys & 0x1F_FFFF, (phys & 0x1F_FFFF) + len);
        self.watches.iter().chain(self.counters.iter().map(|c| &c.range)).any(|&(s, e)| a < e && b > s)
    }

    pub fn record(&mut self, hit: WriteHit) {
        let (a, b) = (hit.address & 0x1F_FFFF, (hit.address & 0x1F_FFFF) + hit.width);
        for c in &mut self.counters {
            if a < c.range.1 && b > c.range.0 && c.pc.is_none_or(|pc| hit.pc.is_some_and(|h| h & 0x1FFF_FFFF == pc)) {
                c.count += 1;
            }
        }
        if let (Some(c), Some(pc), Some(value), Some(old)) = (&mut self.census, hit.pc, hit.value, hit.old) {
            c.store(hit.address, hit.width, pc, value, old);
        }
        if !self.watches.iter().any(|&(s, e)| a < e && b > s) {
            return;
        }
        self.hit_count += 1;
        if self.hits.len() < self.max_hits {
            self.hits.push(hit);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store(address: u32, pc: Option<u32>) -> WriteHit {
        WriteHit {
            frame: 0,
            cycle: 0,
            writer: if pc.is_some() { Writer::Cpu } else { Writer::Dma },
            address,
            width: 4,
            pc,
            value: Some(1),
            old: Some(0),
            dma_channel: None,
        }
    }

    #[test]
    fn counters_count_stores_without_recording_them() {
        let mut o = Observer::default();
        let any = o.count(0x8008_7AA4, 4, None);
        let from = o.count(0x8008_7AA4, 4, Some(0x8001_DD64));
        assert!(o.active() && o.overlaps(0x0008_7AA4, 4));
        // KUSEG and KSEG0 name the same code and the same RAM.
        o.record(store(0x0008_7AA4, Some(0x0001_DD64)));
        o.record(store(0x8008_7AA4, Some(0x8002_0000)));
        o.record(store(0x8008_7AA2, None)); // a DMA over the counter
        o.record(store(0x8008_7AA8, Some(0x8001_DD64))); // next word
        assert_eq!((o.counters[any].count, o.counters[from].count), (3, 1));
        assert_eq!((o.hit_count, o.hits.len()), (0, 0));
        o.watch(0x8008_7AA8, 4);
        o.record(store(0x8008_7AA8, None));
        assert_eq!((o.hit_count, o.counters[any].count), (1, 3));
    }

    #[test]
    fn census_signs_pairs_by_the_fields_they_are_stored_in() {
        let mut o = Observer { census: Some(Default::default()), ..Default::default() };
        let put = |o: &mut Observer, addr: u32, pc: u32, old: u32, value: u32| {
            o.record(WriteHit { value: Some(value), old: Some(old), ..store(addr, Some(pc)) })
        };
        // A counter and a timer stored once a tick, ticks run 1, 0, 2;
        // a frame counter once a field.
        for (field, ticks) in [1u32, 0, 2].into_iter().enumerate() {
            for t in 0..ticks {
                put(&mut o, 0x8008_0000, 0x8001_0000, t, t + 1);
                put(&mut o, 0x0008_0010, 0x0001_0040, 50 - t, 48 - t);
            }
            put(&mut o, 0x8008_0020, 0x8001_0080, field as u32, field as u32 + 1);
            o.census.as_mut().unwrap().end_field();
        }
        let c = o.census.unwrap();
        let pair = |addr: u32, pc: u32| &c.pairs[&(u64::from(pc) << 32 | 4 << 24 | u64::from(addr))];
        let (counter, timer, frames) = (pair(0x8_0000, 0x1_0000), pair(0x8_0010, 0x1_0040), pair(0x8_0020, 0x1_0080));
        assert_eq!(c.fields, 3);
        assert_eq!(counter.signature, timer.signature);
        assert_ne!(counter.signature, frames.signature);
        assert_eq!((counter.stores, counter.by_count, counter.first_step, counter.same_step), (3, [1, 1, 0, 0], 1, 3));
        assert_eq!((timer.first_step, timer.same_step), (0xFFFF_FFFE, 3));
        // The counter restarts each field: its stores' steps are steady, its value is not.
        assert_eq!((counter.first_old, counter.last), (0, 2));
        assert_eq!(frames.by_count, [3, 0, 0, 0]);
    }
}
