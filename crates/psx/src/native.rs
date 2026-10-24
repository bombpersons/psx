//! `psx native`: run the native core on its own (no oracle) and report what
//! it reached and which hardware it is still missing.

use std::path::Path;
use std::time::Instant;

use crate::Error;
use crate::commands::{Ctx, Op, Outcome};
use psx_core::disasm::disasm;
use psx_core::{Machine, Width};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{Value, json};
use std::path::PathBuf;
use std::time::Duration;

fn deadline(timeout_s: Option<u64>) -> Option<Instant> {
    timeout_s.map(|s| Instant::now() + Duration::from_secs(s))
}
fn d_native_frames() -> u64 {
    300
}
fn d_max_steps() -> u64 {
    20_000_000_000
}
fn d_tail() -> usize {
    48
}
fn d_pad() -> String {
    "digital".into()
}

#[derive(clap::Args, Deserialize, JsonSchema)]
pub struct NativeArgs {
    /// BIOS image, e.g. bios/scph7502.bin.
    #[arg(long)]
    pub bios: Option<PathBuf>,
    /// Disc image to insert (CUE/BIN/ISO/CHD); omit for an empty drive.
    #[arg(long)]
    pub disc: Option<PathBuf>,
    /// Frames to run from power-on.
    #[arg(long, default_value_t = d_native_frames())]
    #[serde(default = "d_native_frames")]
    pub frames: u64,
    /// Instruction budget.
    #[arg(long, default_value_t = d_max_steps())]
    #[serde(default = "d_max_steps")]
    pub max_steps: u64,
    /// Write <PREFIX>.screen.png and <PREFIX>.vram.png at the end.
    #[arg(long)]
    pub dump: Option<PathBuf>,
    /// Stop when the PC reaches this hex address and report the tail.
    #[arg(long = "break")]
    #[serde(rename = "break")]
    pub brk: Option<String>,
    /// Instructions of history kept for --break.
    #[arg(long, default_value_t = d_tail())]
    #[serde(default = "d_tail")]
    pub tail: usize,
    /// Report every change of this RAM word (hex address) with the responsible instruction.
    #[arg(long)]
    pub watch: Option<String>,
    /// Start recording --watch changes at this frame.
    #[arg(long, default_value_t = 0)]
    #[serde(default)]
    pub watch_after: u64,
    /// JSON `[{"frame": n, "buttons": ["CROSS"]}]` (frames from power-on).
    #[arg(long)]
    pub replay: Option<PathBuf>,
    /// With --dump: also save the screen every N frames.
    #[arg(long)]
    pub snapshot_every: Option<u64>,
    /// Capture the SPU output to a WAV file.
    #[arg(long)]
    pub wav: Option<PathBuf>,
    /// Controller in port 1: `digital` (default) or `dualshock`.
    #[arg(long, default_value_t = d_pad())]
    #[serde(default = "d_pad")]
    pub pad: String,
    /// Wall-clock limit in seconds; stops with status "timeout" (default: none).
    #[arg(long)]
    pub timeout: Option<u64>,
}

impl Op for NativeArgs {
    fn exec(self, ctx: &Ctx) -> Outcome {
        let resolved = match crate::context::resolve(&ctx.root, self.disc, self.bios, None, false) {
            Ok(r) => r,
            Err(e) => return e.into(),
        };
        if let Some(e) = resolved.setup_error() {
            return e.into();
        }
        let bios = resolved.bios.unwrap();
        let hex = |b: &Option<String>| b.as_deref().and_then(psx_media::memmap::parse_hex);
        Outcome::report(run(Options {
            bios: &bios,
            disc: resolved.media.as_deref(),
            frames: self.frames,
            max_steps: self.max_steps,
            dump: self.dump.as_deref(),
            brk: hex(&self.brk),
            tail: self.tail,
            watch: hex(&self.watch),
            watch_after: self.watch_after,
            replay: self.replay.as_deref(),
            snapshot_every: self.snapshot_every,
            wav: self.wav.as_deref(),
            pad: &self.pad,
            deadline: deadline(self.timeout),
        }))
    }
}

pub struct Options<'a> {
    pub bios: &'a Path,
    pub disc: Option<&'a Path>,
    pub frames: u64,
    pub max_steps: u64,
    pub dump: Option<&'a Path>,
    /// Stop at this PC and report the last `tail` instructions.
    pub brk: Option<u32>,
    pub tail: usize,
    /// Report changes of this RAM word from frame `watch_after` on.
    pub watch: Option<u32>,
    pub watch_after: u64,
    /// `[{frame, buttons: [names]}]`: held buttons from each frame on (frames from power-on).
    pub replay: Option<&'a Path>,
    /// Save the screen every N frames as <dump>.fNNNNN.png.
    pub snapshot_every: Option<u64>,
    /// Capture the SPU mix to a 44.1 kHz stereo WAV.
    pub wav: Option<&'a Path>,
    /// `digital` or `dualshock`.
    pub pad: &'a str,
    /// Stop with status "timeout" once this wall-clock instant passes.
    pub deadline: Option<Instant>,
}

/// True every 2^20 steps once `deadline` has passed (cheap enough for the step loops).
fn late(deadline: Option<Instant>, steps: u64) -> bool {
    steps.is_multiple_of(1 << 20) && deadline.is_some_and(|d| Instant::now() >= d)
}

pub(crate) fn write_wav(path: &Path, samples: &[i16]) -> std::io::Result<()> {
    let data_len = (samples.len() * 2) as u32;
    let mut b = Vec::with_capacity(44 + data_len as usize);
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
    for s in samples {
        b.extend_from_slice(&s.to_le_bytes());
    }
    std::fs::write(path, b)
}

/// Run until `target` frames, applying replay events and periodic snapshots.
fn run_with_input(
    m: &mut Machine,
    target: u64,
    max_steps: u64,
    events: &[crate::replay::Input],
    snap: Option<(u64, &Path)>,
    deadline: Option<Instant>,
) -> u64 {
    let mut steps = 0;
    let mut next = 0;
    let mut last_frame = u64::MAX;
    while m.bus.frames < target && steps < max_steps && !late(deadline, steps) {
        let f = m.bus.frames;
        if f != last_frame {
            last_frame = f;
            while next < events.len() && events[next].frame <= f {
                crate::replay::apply(m, &events[next]);
                next += 1;
            }
            if let Some((every, prefix)) = snap {
                if f > 0 && f.is_multiple_of(every) {
                    let (w, h, rgba) = m.bus.gpu.display_rgba();
                    let p = std::path::PathBuf::from(format!("{}.f{f:05}.png", prefix.display()));
                    let _ = crate::run::write_png(&p, w, h, &rgba, png::ColorType::Rgba);
                }
            }
        }
        // The rest of this frame needs no per-instruction work.
        steps += m.run_frame_steps(max_steps - steps);
    }
    steps
}

pub fn run(o: Options) -> crate::Result<(bool, Value)> {
    let Options {
        bios: bios_path,
        disc,
        frames,
        max_steps,
        dump,
        brk,
        tail,
        watch,
        watch_after,
        replay,
        snapshot_every,
        wav,
        pad,
        deadline,
    } = o;
    let events = replay.map(crate::replay::load).transpose().map_err(Error::invalid)?.unwrap_or_default();
    let bios = std::fs::read(bios_path).map_err(|e| Error::io(bios_path, e))?;
    let mut m = Machine::new(bios);
    m.bus.sio0.pad.kind = psx_core::pad::PadKind::from_name(pad)
        .ok_or_else(|| Error::invalid(format!("unknown pad {pad:?} (digital | dualshock)")))?;
    if let Some(d) = disc {
        m.insert_disc(Some(psx_media::Disc::open(d)?));
    }
    m.bus.spu.capture = wav.is_some();
    let started = Instant::now();
    // Optional breakpoint with an instruction tail.
    let mut ring: std::collections::VecDeque<(u32, u32, Option<u8>)> =
        std::collections::VecDeque::with_capacity(tail + 1);
    let mut steps = 0u64;
    let mut broke = Value::Null;
    if let Some(w) = watch {
        // Record every change of the watched word (first 32), with the instruction that ran.
        while m.bus.frames < watch_after && steps < max_steps && !late(deadline, steps) {
            m.step();
            steps += 1;
        }
        let mut changes = Vec::new();
        let mut last = m.bus.read(w, Width::Word).unwrap_or(0);
        let mut dma_seen = m.bus.recent_dma.len();
        let target = m.bus.frames + frames;
        while m.bus.frames < target && steps < max_steps && changes.len() < 32 && !late(deadline, steps) {
            let info = m.step();
            steps += 1;
            let now = m.bus.read(w, Width::Word).unwrap_or(0);
            if now != last {
                let dma = (m.bus.recent_dma.len() != dma_seen)
                    .then(|| {
                        m.bus.recent_dma.back().map(|d| {
                            format!("ch{} madr={:08X} bcr={:08X} chcr={:08X}", d.channel, d.madr, d.bcr, d.chcr)
                        })
                    })
                    .flatten();
                changes.push(json!({"frame": m.bus.frames, "step": steps, "pc": format!("0x{:08X}", info.pc), "asm": disasm(info.instruction, info.pc),
                    "old": format!("0x{last:08X}"), "new": format!("0x{now:08X}"), "dma_this_step": dma}));
                last = now;
            }
            dma_seen = m.bus.recent_dma.len();
        }
        let mut v = json!({"status": "watch", "addr": format!("0x{w:08X}"), "changes": changes, "frames_reached": m.bus.frames});
        if deadline.is_some_and(|d| Instant::now() >= d) {
            v["timed_out"] = json!(true);
        }
        return Ok((true, v));
    }
    if let Some(bp) = brk {
        let target = m.bus.frames + frames;
        while m.bus.frames < target && steps < max_steps && !late(deadline, steps) {
            if m.cpu.pc == bp {
                broke = json!({
                    "pc": format!("0x{bp:08X}"), "frame": m.bus.frames, "step": steps,
                    "tail": ring.iter().map(|(pc, i, e)| format!("{pc:08X}: {}{}", disasm(*i, *pc), e.map_or(String::new(), |e| format!("   <exception {e}>")))).collect::<Vec<_>>(),
                    "regs": (0..32).map(|r| format!("{}={:08X}", psx_core::disasm::reg(r), m.cpu.regs[r as usize])).collect::<Vec<_>>(),
                    "cop0": {"sr": format!("0x{:08X}", m.cpu.cop0[12]), "cause": format!("0x{:08X}", m.cpu.cop0[13]), "epc": format!("0x{:08X}", m.cpu.cop0[14])},
                    "recent_dma": m.bus.recent_dma.iter().map(|d| format!("ch{} madr={:08X} bcr={:08X} chcr={:08X}", d.channel, d.madr, d.bcr, d.chcr)).collect::<Vec<_>>(),
                    "vector_0x80": (0..4).map(|i| format!("{:08X}", m.bus.read(0x8000_0080 + 4 * i, Width::Word).unwrap_or(0))).collect::<Vec<_>>(),
                });
                break;
            }
            let info = m.step();
            if tail > 0 {
                ring.push_back((info.pc, info.instruction, info.exception));
                if ring.len() > tail {
                    ring.pop_front();
                }
            }
            steps += 1;
        }
        if !broke.is_null() {
            return Ok((false, json!({"status": "breakpoint", "break": broke})));
        }
    }
    let snap = snapshot_every.zip(dump);
    steps += run_with_input(&mut m, frames.saturating_sub(2), max_steps, &events, snap, deadline);
    // Profile the last two frames: where the CPU spends time and what it polls.
    m.bus.log_io = true;
    let mut pcs: std::collections::HashMap<u32, u64> = Default::default();
    let mut io: std::collections::HashMap<(u32, bool), u64> = Default::default();
    let target = m.bus.frames + frames.min(2);
    while m.bus.frames < target && steps < max_steps && !late(deadline, steps) {
        let info = m.step();
        *pcs.entry(info.pc).or_default() += 1;
        for a in m.bus.take_io_log() {
            *io.entry((a.addr, a.write)).or_default() += 1;
        }
        steps += 1;
    }
    let mut hot: Vec<_> = pcs.into_iter().collect();
    hot.sort_by_key(|e| std::cmp::Reverse(e.1));
    let mut hot_io: Vec<_> = io.into_iter().collect();
    hot_io.sort_by_key(|e| std::cmp::Reverse(e.1));
    let secs = started.elapsed().as_secs_f64();
    let mut unhandled: Vec<(u32, u64)> = m.bus.unhandled_reads.iter().map(|(a, n)| (*a, *n)).collect();
    unhandled.sort_by_key(|e| std::cmp::Reverse(e.1));
    let mut dumped = Value::Null;
    if let Some(prefix) = dump {
        if let Some(parent) = prefix.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let (w, h, rgba) = m.bus.gpu.display_rgba();
        let screen = std::path::PathBuf::from(format!("{}.screen.png", prefix.display()));
        let vram = std::path::PathBuf::from(format!("{}.vram.png", prefix.display()));
        let ok1 = crate::run::write_png(&screen, w, h, &rgba, png::ColorType::Rgba).is_ok();
        let ok2 =
            crate::run::write_png(&vram, 1024, 512, &m.bus.gpu.vram.rgba15(0, 0, 1024, 512), png::ColorType::Rgba)
                .is_ok();
        dumped = json!({"screen": ok1.then_some(screen), "vram": ok2.then_some(vram)});
    }
    let audio = match wav {
        Some(p) => {
            let samples = m.bus.spu.take_samples();
            let peak = samples.iter().map(|s| s.unsigned_abs()).max().unwrap_or(0);
            let ok = write_wav(p, &samples).is_ok();
            json!({"wav": ok.then_some(p), "seconds": samples.len() as f64 / 88_200.0, "peak": peak, "voices_keyed": m.bus.spu.keyed_on})
        }
        None => Value::Null,
    };
    let pc = m.cpu.pc;
    let reached = m.bus.frames >= frames;
    let timed_out = deadline.is_some_and(|d| Instant::now() >= d);
    Ok((
        reached,
        json!({
            "status": if reached { "frames_reached" } else if timed_out { "timeout" } else { "step_budget_exhausted" },
            "warnings": crate::stall::warnings(&m),
            "video": {"region_at_capture":if m.bus.pal() { "PAL" } else { "NTSC" },"mode":m.bus.gpu.display.mode,
                "cycles_per_vblank_at_capture":m.bus.frame_cycles(),"width":m.bus.gpu.display.width(),"height":m.bus.gpu.display.height(),
                "interlaced":m.bus.gpu.display.interlaced_480(),"note":"GPU mode at capture; disc region and earlier boot modes may differ."},
            "frames": {"requested": frames, "reached": m.bus.frames, "region": if m.bus.pal() { "PAL" } else { "NTSC" }},
            "instructions": steps,
            "cycles": m.bus.now,
            "final_pc": format!("0x{pc:08X}"),
            "final_asm": disasm(m.bus.read(pc, Width::Word).unwrap_or(0), pc),
            "unhandled_io_reads_top": unhandled.iter().take(15).map(|(a, n)| json!({"addr": format!("0x{a:08X}"), "reads": n})).collect::<Vec<_>>(),
            "unmodelled_dma": m.bus.unmodelled_dma.len(),
            "audio": audio,
            "spu": {"active_voices": m.bus.spu.active_voices(), "main_volume": [m.bus.spu.read16(0x180), m.bus.spu.read16(0x182)], "main_volume_current": [m.bus.spu.read16(0x1B8), m.bus.spu.read16(0x1BA)], "spucnt": m.bus.spu.read16(0x1AA),
                "noise_voices_seen": m.bus.spu.noise_seen, "pmon_voices_seen": m.bus.spu.pmon_seen, "irqs": m.bus.spu.irqs, "reserved_adpcm_blocks": m.bus.spu.reserved_blocks,
                "reverb": {"master": m.bus.spu.read16(0x1AA) & 0x80 != 0, "cd": m.bus.spu.read16(0x1AA) & 4 != 0,
                    "eon": m.bus.spu.read16(0x198) as u32 | (m.bus.spu.read16(0x19A) as u32) << 16,
                    "work_area": (m.bus.spu.read16(0x1A2) as u32) << 3, "out": [m.bus.spu.read16(0x184) as i16, m.bus.spu.read16(0x186) as i16]}},
            "cdrom": {"command_history": m.bus.cdrom.recent.iter().rev().take(24).rev().map(|r| json!({
                    "frame":r.frame,"cycle":r.time,"last_frame":r.last_frame,"last_cycle":r.last_time,
                    "command":r.command,"command_hex":format!("0x{:02X}",r.command),"params":r.params,"interrupts":r.ints,"repeats":r.repeats
                })).collect::<Vec<_>>(), "disc": disc, "sectors_delivered": m.bus.cdrom.sectors_delivered, "xa_sectors": m.bus.cdrom.xa_sectors, "unknown_commands": m.bus.cdrom.unknown_commands,
                "recent_commands": m.bus.cdrom.recent.iter().rev().take(24).rev().map(|r| format!("frame {} {:02X} {:02X?} -> INT{:?} x{}", r.frame.map_or_else(|| "unknown".into(), |f| f.to_string()), r.command, r.params, r.ints, r.repeats)).collect::<Vec<_>>()},
            "last_frames_profile": {
                "hot_pcs": hot.iter().take(12).map(|(pc, n)| json!({"pc": format!("0x{pc:08X}"), "count": n, "asm": disasm(m.bus.read(*pc, Width::Word).unwrap_or(0), *pc)})).collect::<Vec<_>>(),
                "io": hot_io.iter().take(12).map(|((a, w), n)| json!({"addr": format!("0x{a:08X}"), "write": w, "count": n})).collect::<Vec<_>>(),
                "i_stat": format!("0x{:08X}", m.bus.read(0x1F80_1070, Width::Word).unwrap_or(0)),
                "i_mask": format!("0x{:08X}", m.bus.read(0x1F80_1074, Width::Word).unwrap_or(0)),
                "sr": format!("0x{:08X}", m.cpu.cop0[12]),
            },
            "dump": dumped,
            "speed": {"instructions_per_second": (steps as f64 / secs.max(1e-9)).round(), "realtime_factor": ((m.bus.now as f64 / psx_core::bus::SYSCLK as f64) / secs.max(1e-9) * 100.0).round() / 100.0},
            "timing_model": "Mednafen's video, CPU, DMA, CD-ROM, GPU command, timer, controller port and SPU timing; the MDEC's timing differs",
        }),
    ))
}
