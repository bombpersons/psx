//! The machine's hardware as debug_show hardware gives it (psx
//! development: what the native core reached and which hardware it is
//! still missing), and the WAV files run and xa write.

use std::path::Path;

use psx_core::disasm::disasm;
use psx_core::{Machine, Width};
use serde_json::{Value, json};

pub(crate) fn write_wav(path: &Path, samples: &[i16]) -> std::io::Result<()> {
    let data_len = (samples.len() * 2) as u32;
    let mut b = wav_header(data_len);
    b.reserve(data_len as usize);
    for s in samples {
        b.extend_from_slice(&s.to_le_bytes());
    }
    std::fs::write(path, b)
}

/// The 44-byte header of a 44.1 kHz stereo 16-bit WAV holding `data_len` bytes.
fn wav_header(data_len: u32) -> Vec<u8> {
    let mut b = Vec::with_capacity(44);
    b.extend_from_slice(b"RIFF");
    b.extend_from_slice(&(36 + data_len).to_le_bytes());
    b.extend_from_slice(b"WAVEfmt ");
    b.extend_from_slice(&16u32.to_le_bytes());
    b.extend_from_slice(&1u16.to_le_bytes()); // PCM
    b.extend_from_slice(&2u16.to_le_bytes()); // stereo
    b.extend_from_slice(&44_100u32.to_le_bytes());
    b.extend_from_slice(&(44_100u32 * 4).to_le_bytes());
    b.extend_from_slice(&4u16.to_le_bytes());
    b.extend_from_slice(&16u16.to_le_bytes());
    b.extend_from_slice(b"data");
    b.extend_from_slice(&data_len.to_le_bytes());
    b
}

/// A WAV written as the samples arrive; the header's sizes are filled in by
/// [`WavStream::finish`].
pub(crate) struct WavStream {
    file: std::io::BufWriter<std::fs::File>,
    pub samples: u64,
    pub peak: u16,
}

impl WavStream {
    pub fn create(path: &Path) -> std::io::Result<Self> {
        use std::io::Write;
        let mut file = std::io::BufWriter::new(std::fs::File::create(path)?);
        file.write_all(&wav_header(0))?;
        Ok(WavStream { file, samples: 0, peak: 0 })
    }

    /// Interleaved L, R samples.
    pub fn push(&mut self, samples: &[i16]) -> std::io::Result<()> {
        use std::io::Write;
        for s in samples {
            self.file.write_all(&s.to_le_bytes())?;
            self.peak = self.peak.max(s.unsigned_abs());
        }
        self.samples += samples.len() as u64;
        Ok(())
    }

    pub fn finish(self) -> std::io::Result<()> {
        use std::io::{Seek, Write};
        let mut file = self.file.into_inner().map_err(|e| e.into_error())?;
        let data_len = u32::try_from(self.samples * 2).unwrap_or(u32::MAX);
        file.seek(std::io::SeekFrom::Start(0))?;
        file.write_all(&wav_header(data_len))
    }
}

/// Where the CPU spends `fields` fields from here and what it polls: run
/// on `m`, which the caller throws away.
pub(crate) fn hot(m: &mut Machine, fields: u64) -> Value {
    m.bus.log_io = true;
    let mut pcs: std::collections::HashMap<u32, u64> = Default::default();
    let mut io: std::collections::HashMap<(u32, bool), u64> = Default::default();
    let target = m.bus.frames + fields;
    let mut steps = 0u64;
    // A machine that never reaches a field's end still answers.
    while m.bus.frames < target && steps < 200_000_000 {
        let info = m.step();
        *pcs.entry(info.pc).or_default() += 1;
        for a in m.bus.take_io_log() {
            *io.entry((a.addr, a.write)).or_default() += 1;
        }
        steps += 1;
    }
    m.bus.log_io = false;
    let mut hot: Vec<_> = pcs.into_iter().collect();
    hot.sort_by_key(|e| std::cmp::Reverse(e.1));
    let mut hot_io: Vec<_> = io.into_iter().collect();
    hot_io.sort_by_key(|e| std::cmp::Reverse(e.1));
    json!({
        "fields": m.bus.frames + fields - target, "instructions": steps,
        "hot_pcs": hot.iter().take(12).map(|(pc, n)| json!({"pc": format!("0x{pc:08X}"), "count": n, "asm": disasm(m.bus.read(*pc, Width::Word).unwrap_or(0), *pc)})).collect::<Vec<_>>(),
        "io": hot_io.iter().take(12).map(|((a, w), n)| json!({"addr": format!("0x{a:08X}"), "write": w, "count": n})).collect::<Vec<_>>(),
    })
}

/// The hardware now: video mode, the CD controller's recent commands,
/// the SPU, unhandled I/O reads, interrupts and the stall warnings.
pub(crate) fn hardware(m: &mut Machine, disc: Option<&Path>) -> Value {
    let mut unhandled: Vec<(u32, u64)> = m.bus.unhandled_reads.iter().map(|(a, n)| (*a, *n)).collect();
    unhandled.sort_by_key(|e| std::cmp::Reverse(e.1));
    json!({
            "warnings": crate::stall::warnings(m),
            "video": {"region":if m.bus.pal() { "PAL" } else { "NTSC" },"mode":m.bus.gpu.display.mode,
                "cycles_per_vblank":m.bus.frame_cycles(),"width":m.bus.gpu.display.width(),"height":m.bus.gpu.display.height(),
                "interlaced":m.bus.gpu.display.interlaced_480()},
            "cycles": m.bus.now,
            "unhandled_io_reads_top": unhandled.iter().take(15).map(|(a, n)| json!({"addr": format!("0x{a:08X}"), "reads": n})).collect::<Vec<_>>(),
            "unmodelled_dma": m.bus.unmodelled_dma.len(),
            "recent_dma": m.bus.recent_dma.iter().map(|d| format!("ch{} madr={:08X} bcr={:08X} chcr={:08X}", d.channel, d.madr, d.bcr, d.chcr)).collect::<Vec<_>>(),
            "spu": {"active_voices": m.bus.spu.active_voices(), "main_volume": [m.bus.spu.read16(0x180), m.bus.spu.read16(0x182)], "main_volume_current": [m.bus.spu.read16(0x1B8), m.bus.spu.read16(0x1BA)], "spucnt": m.bus.spu.read16(0x1AA),
                "voices_keyed": m.bus.spu.keyed_on, "noise_voices_seen": m.bus.spu.noise_seen, "pmon_voices_seen": m.bus.spu.pmon_seen, "irqs": m.bus.spu.irqs, "reserved_adpcm_blocks": m.bus.spu.reserved_blocks,
                "reverb": {"master": m.bus.spu.read16(0x1AA) & 0x80 != 0, "cd": m.bus.spu.read16(0x1AA) & 4 != 0,
                    "eon": m.bus.spu.read16(0x198) as u32 | (m.bus.spu.read16(0x19A) as u32) << 16,
                    "work_area": (m.bus.spu.read16(0x1A2) as u32) << 3, "out": [m.bus.spu.read16(0x184) as i16, m.bus.spu.read16(0x186) as i16]}},
            "cdrom": {"command_history": m.bus.cdrom.recent.iter().rev().take(24).rev().map(|r| json!({
                    "frame":r.frame,"cycle":r.time,"last_frame":r.last_frame,"last_cycle":r.last_time,
                    "command":r.command,"command_hex":format!("0x{:02X}",r.command),"params":r.params,"interrupts":r.ints,"repeats":r.repeats
                })).collect::<Vec<_>>(), "disc": disc, "sectors_delivered": m.bus.cdrom.sectors_delivered, "xa_sectors": m.bus.cdrom.xa_sectors, "unknown_commands": m.bus.cdrom.unknown_commands},
            "interrupts": {
                "i_stat": format!("0x{:08X}", m.bus.io_word(psx_core::bus::I_STAT)),
                "i_mask": format!("0x{:08X}", m.bus.io_word(psx_core::bus::I_MASK)),
                "sr": format!("0x{:08X}", m.cpu.cop0[12]), "cause": format!("0x{:08X}", m.cpu.cop0[13]), "epc": format!("0x{:08X}", m.cpu.cop0[14]),
            },
            "vector_0x80": (0..4).map(|i| format!("{:08X}", word(m, 0x80 + 4 * i))).collect::<Vec<_>>(),
            "timing_model": "Mednafen's video, CPU, DMA, CD-ROM, GPU command, timer, controller port, SPU and MDEC timing",
    })
}

fn word(m: &Machine, at: usize) -> u32 {
    m.bus.ram.get(at..at + 4).map_or(0, |b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
}
