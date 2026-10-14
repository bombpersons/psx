//! `psx native`: run the native core on its own (no oracle) and report what
//! it reached and which hardware it is still missing.

use std::path::Path;
use std::time::Instant;

use psx_core::disasm::disasm;
use psx_core::{Machine, Width};
use serde_json::{json, Value};

pub fn run(bios_path: &Path, disc: Option<&Path>, frames: u64, max_steps: u64, dump: Option<&Path>, brk: Option<u32>, tail: usize, watch: Option<u32>, watch_after: u64) -> (bool, Value) {
    let bios = match std::fs::read(bios_path) {
        Ok(b) => b,
        Err(e) => return (false, json!({"error": {"code": "io_error", "message": format!("{}: {e}", bios_path.display())}})),
    };
    let mut m = Machine::new(bios);
    if let Some(d) = disc {
        match psx_media::Disc::open(d) {
            Ok(d) => m.insert_disc(Some(d)),
            Err(e) => return (false, json!({"error": {"code": e.code(), "message": e.to_string()}})),
        }
    }
    let started = Instant::now();
    // Optional breakpoint with an instruction tail.
    let mut ring: std::collections::VecDeque<(u32, u32, Option<u8>)> = std::collections::VecDeque::with_capacity(tail + 1);
    let mut steps = 0u64;
    let mut broke = Value::Null;
    if let Some(w) = watch {
        // Record every change of the watched word (first 32), with the instruction that ran.
        while m.bus.frames < watch_after && steps < max_steps {
            m.step();
            steps += 1;
        }
        let mut changes = Vec::new();
        let mut last = m.bus.read(w, Width::Word).unwrap_or(0);
        let mut dma_seen = m.bus.recent_dma.len();
        let target = m.bus.frames + frames;
        while m.bus.frames < target && steps < max_steps && changes.len() < 32 {
            let info = m.step();
            steps += 1;
            let now = m.bus.read(w, Width::Word).unwrap_or(0);
            if now != last {
                let dma = (m.bus.recent_dma.len() != dma_seen).then(|| m.bus.recent_dma.back().map(|d| format!("ch{} madr={:08X} bcr={:08X} chcr={:08X}", d.channel, d.madr, d.bcr, d.chcr))).flatten();
                changes.push(json!({"frame": m.bus.frames, "step": steps, "pc": format!("0x{:08X}", info.pc), "asm": disasm(info.instruction, info.pc),
                    "old": format!("0x{last:08X}"), "new": format!("0x{now:08X}"), "dma_this_step": dma}));
                last = now;
            }
            dma_seen = m.bus.recent_dma.len();
        }
        return (true, json!({"status": "watch", "addr": format!("0x{w:08X}"), "changes": changes, "frames_reached": m.bus.frames}));
    }
    if let Some(bp) = brk {
        let target = m.bus.frames + frames;
        while m.bus.frames < target && steps < max_steps {
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
            return (false, json!({"status": "breakpoint", "break": broke}));
        }
    }
    steps += m.run_frames(frames.saturating_sub(2).saturating_sub(m.bus.frames), max_steps);
    // Profile the last two frames: where the CPU spends time and what it polls.
    m.bus.log_io = true;
    let mut pcs: std::collections::HashMap<u32, u64> = Default::default();
    let mut io: std::collections::HashMap<(u32, bool), u64> = Default::default();
    let target = m.bus.frames + frames.min(2);
    while m.bus.frames < target && steps < max_steps {
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
        let ok2 = crate::run::write_png(&vram, 1024, 512, &m.bus.gpu.vram.rgba15(0, 0, 1024, 512), png::ColorType::Rgba).is_ok();
        dumped = json!({"screen": ok1.then_some(screen), "vram": ok2.then_some(vram)});
    }
    let pc = m.cpu.pc;
    let reached = m.bus.frames >= frames;
    (
        reached,
        json!({
            "status": if reached { "frames_reached" } else { "step_budget_exhausted" },
            "frames": {"requested": frames, "reached": m.bus.frames, "region": if m.bus.pal() { "PAL" } else { "NTSC" }},
            "instructions": steps,
            "cycles": m.bus.now,
            "final_pc": format!("0x{pc:08X}"),
            "final_asm": disasm(m.bus.read(pc, Width::Word).unwrap_or(0), pc),
            "unhandled_io_reads_top": unhandled.iter().take(15).map(|(a, n)| json!({"addr": format!("0x{a:08X}"), "reads": n})).collect::<Vec<_>>(),
            "unmodelled_dma": m.bus.unmodelled_dma.len(),
            "cdrom": {"disc": disc, "sectors_delivered": m.bus.cdrom.sectors_delivered, "unknown_commands": m.bus.cdrom.unknown_commands},
            "last_frames_profile": {
                "hot_pcs": hot.iter().take(12).map(|(pc, n)| json!({"pc": format!("0x{pc:08X}"), "count": n, "asm": disasm(m.bus.read(*pc, Width::Word).unwrap_or(0), *pc)})).collect::<Vec<_>>(),
                "io": hot_io.iter().take(12).map(|((a, w), n)| json!({"addr": format!("0x{a:08X}"), "write": w, "count": n})).collect::<Vec<_>>(),
                "i_stat": format!("0x{:08X}", m.bus.read(0x1F80_1070, Width::Word).unwrap_or(0)),
                "i_mask": format!("0x{:08X}", m.bus.read(0x1F80_1074, Width::Word).unwrap_or(0)),
                "sr": format!("0x{:08X}", m.cpu.cop0[12]),
            },
            "dump": dumped,
            "speed": {"instructions_per_second": (steps as f64 / secs.max(1e-9)).round(), "realtime_factor": ((m.bus.now as f64 / psx_core::bus::SYSCLK as f64) / secs.max(1e-9) * 100.0).round() / 100.0},
            "timing_model": "PCSX-Redux-like flat costs (2 cycles/instruction, +20 in BIOS ROM); not cycle-accurate",
        }),
    )
}
