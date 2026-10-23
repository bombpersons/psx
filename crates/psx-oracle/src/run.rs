//! `psx-oracle run`: a bounded Mednafen run and its artifacts.
use std::cell::RefCell;
use std::fmt::Write as _;
use std::path::Path;
use std::rc::Rc;

use serde_json::{Value, json};

use crate::ffi::{self, Mednafen, Region};
use crate::{RunJob, write_png};

/// Write watch hits kept in writes.jsonl.
const MAX_HITS: usize = 10_000;

fn h(v: u32) -> String {
    format!("0x{v:08X}")
}

pub fn run(job: &RunJob, dir: &Path) -> Result<Value, String> {
    let start = std::time::Instant::now();
    std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let bios = std::fs::read(&job.bios).map_err(|e| format!("{}: {e}", job.bios.display()))?;
    let region = Region::of_bios(&bios);
    let base = dir.join("mednafen");
    let mut emu = Mednafen::open(&base, &job.bios, job.media.as_deref(), region, job.dualshock, job.verbose)?;
    if let Some(state) = &job.from_state {
        emu.load_state(state)?;
    }
    let first_vblank = emu.vblanks();
    let frame_now = Rc::new(RefCell::new(0u32));
    let hits: Rc<RefCell<(u64, Vec<Value>)>> = Default::default();
    if !job.watches.is_empty() {
        let (watches, hits, frame_now) = (job.watches.clone(), hits.clone(), frame_now.clone());
        emu.write_watches(&watches.clone(), move |pc, _| {
            if let Some(hit) = store_hit(pc, &watches, *frame_now.borrow()) {
                let mut h = hits.borrow_mut();
                h.0 += 1;
                if h.1.len() < MAX_HITS {
                    h.1.push(hit);
                }
            }
        });
    }

    let mut frame = ffi::Frame::default();
    let mut cycles = 0i64;
    let mut log = String::from("frame,cycles,width,height,interlaced\n");
    let mut inputs = job.inputs.iter().peekable();
    let (mut buttons, mut sticks) = (0u16, [0x80u8; 4]);
    for n in 0..job.frames {
        while let Some(i) = inputs.next_if(|i| i.frame <= n) {
            (buttons, sticks) = (i.buttons, i.sticks);
        }
        emu.set_pad(buttons, sticks);
        *frame_now.borrow_mut() = n;
        frame = emu.frame(n + 1 == job.frames);
        cycles += frame.master_cycles;
        if job.frame_log {
            let f = &frame;
            let _ = writeln!(log, "{n},{},{},{},{}", f.master_cycles, f.line_width, f.h, f.interlaced);
        }
    }

    let write = |name: &str, bytes: &[u8]| {
        let path = dir.join(name);
        std::fs::write(&path, bytes).map_err(|e| format!("{}: {e}", path.display()))
    };
    let vram = emu.read("gpu", 0, 1 << 20)?;
    write("ram.bin", &emu.read("ram", 0, 2 << 20)?)?;
    write("scratch.bin", &emu.read("cpu", 0x1F80_0000, 1024)?)?;
    write("vram.bin", &vram)?;
    write("spu.bin", &emu.read("spu", 0, 512 << 10)?)?;
    if job.frame_log {
        write("frames.csv", log.as_bytes())?;
    }
    let (w, hgt, rgba) = emu.screen(&frame);
    write_png(&dir.join("screen.png"), w, hgt, &rgba)?;
    let mut v = psx_gpu::Vram::default();
    v.load_bytes(&vram);
    write_png(&dir.join("vram.png"), 1024, 512, &v.rgba15(0, 0, 1024, 512))?;
    if job.save_state {
        emu.save_state(&dir.join("state.mcs"))?;
    }
    let hits = hits.borrow();
    if !job.watches.is_empty() {
        let lines: String = hits.1.iter().map(|hit| format!("{hit}\n")).collect();
        write("writes.jsonl", lines.as_bytes())?;
    }
    let r = ffi::regs();
    let reached = (emu.vblanks() - first_vblank) as u32;
    let report = json!({
        "status": if reached >= job.frames { "complete" } else { "partial" },
        "run_dir": dir,
        "frames": {"requested": job.frames, "reached": reached},
        "region": region.name(),
        // Mednafen's master clock is the CPU clock (33.8688 MHz).
        "cpu_cycles": cycles,
        "registers": {"pc": h(r.pc), "sr": h(r.sr), "cause": h(r.cause), "epc": h(r.epc),
                      "sp": h(r.gpr[29]), "ra": h(r.gpr[31])},
        "display": {"x": frame.x, "y": frame.y, "width": frame.line_width, "height": frame.h,
                    "interlaced": frame.interlaced != 0, "field": frame.field},
        "screen": {"width": w, "height": hgt},
        "watch": {"hits": hits.0, "recorded": hits.1.len(), "first": hits.1.iter().take(64).collect::<Vec<_>>()},
        "identity": {"backend": "mednafen", "version": ffi::version(), "changes": "psx-oracle (crates/psx-oracle/README.md)"},
        "wall_ms": start.elapsed().as_millis() as u64,
    });
    write("result.json", serde_json::to_string_pretty(&report).unwrap().as_bytes())?;
    Ok(report)
}

/// The store about to run at `pc`, if it writes into a watched range.
fn store_hit(pc: u32, watches: &[(u32, u32)], frame: u32) -> Option<Value> {
    let instr = ffi::peek(pc, 4);
    let width = match instr >> 26 {
        0x28 => 1,        // sb
        0x29 => 2,        // sh
        0x2B => 4,        // sw
        0x2A | 0x2E => 4, // swl, swr: up to a word
        _ => return None,
    };
    let r = ffi::regs();
    // With the cache isolated (SR bit 16), stores go to the cache, not memory.
    if r.sr & 0x1_0000 != 0 {
        return None;
    }
    let (rs, rt) = ((instr >> 21 & 31) as usize, (instr >> 16 & 31) as usize);
    let address = r.gpr[rs].wrapping_add(instr as u16 as i16 as u32);
    let (start, end) = if matches!(instr >> 26, 0x2A | 0x2E) {
        (address & !3, (address & !3) + 4)
    } else {
        (address, address + width)
    };
    let phys = |a: u32| a & 0x1FFF_FFFF;
    let (lo, hi) = (phys(start), phys(start) + (end - start));
    let watched = watches.iter().find(|&&(a, w)| lo < a + w && a < hi)?;
    let old = (phys(address) < 0x80_0000).then(|| ffi::peek(0x8000_0000 | (phys(address) & 0x1F_FFFC), 4));
    Some(json!({
        "frame": frame,
        "address": h(phys(address)),
        "width": width,
        "watch": h(watched.0),
        "writer": "cpu",
        "pc": h(pc),
        "ra": h(r.gpr[31]),
        "sp": h(r.gpr[29]),
        "value": h(r.gpr[rt]),
        "old_word": old.map(h),
    }))
}
