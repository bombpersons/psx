//! `psx-oracle lockstep`: the native core and Mednafen side by side from the
//! BIOS reset vector (no disc), compared before every instruction: PC, the
//! instruction word, r1-r31, HI, LO, the delayed load in flight, COP0's SR,
//! Cause (but IP2), EPC and BadVaddr, and after each COP2 instruction or LWC2
//! the GTE's 64 registers.
//!
//! The two models' timing differs (the core's CPU costs are flat), so two
//! things Mednafen knows are adopted rather than counted as divergence:
//! - the value of every load from I/O space, which depends on device timing
//!   (applied to the destination register once the load retires, and stored
//!   into the core's I/O register file);
//! - interrupts: the core's interrupt line (Cause.IP2) follows Mednafen's,
//!   and when Mednafen enters an exception vector and the core did not, the
//!   core takes an external interrupt at the same point;
//! - the interlaced field being displayed, whose lines the GPU doesn't draw.
//!
//! Adoption counts are reported; anything else that differs stops the run.
//!
//! At every vblank, main RAM, the scratchpad, VRAM and SPU RAM are compared
//! too. Those differences are recorded, not fatal: the CPUs agree, so they
//! come from a device (DMA, the GPU or the SPU), possibly only from when its
//! work lands.
use std::cell::RefCell;
use std::collections::VecDeque;
use std::path::Path;
use std::rc::Rc;
use std::time::Instant;

use psx_core::disasm::{disasm, reg};
use psx_core::{Machine, Width};
use serde_json::{Value, json};

use crate::ffi::{self, Mednafen, Region};

fn h(v: u32) -> String {
    format!("0x{v:08X}")
}

const VECTORS: [u32; 2] = [0x8000_0080, 0xBFC0_0180];

struct State {
    m: Machine,
    steps: u64,
    context: usize,
    done: u64,
    history: VecDeque<Value>,
    /// Registers an I/O load wrote (or will write once it retires).
    io_dest: [Option<(u32, Width)>; 32],
    io_injected: u64,
    irq_injected: u64,
    /// The last instruction was COP2 or LWC2, so the GTE may have changed.
    gte_changed: bool,
    divergence: Value,
    stop: bool,
}

impl State {
    /// Before Mednafen runs the instruction at `pc`.
    fn check(&mut self, pc: u32) {
        if self.stop {
            return;
        }
        if self.done >= self.steps {
            self.stop = true;
            return;
        }
        let r = ffi::regs();
        let m = &mut self.m;
        // The interrupt line (Cause.IP2) follows Mednafen's, so the core sees
        // the same pending interrupt and takes it when Mednafen does.
        m.forced_irq = r.cause & 0x400 != 0;
        // So is the field the GPU skips in 480-line interlace, which follows
        // the video timing.
        m.bus.gpu.readout_field = ffi::gpu_readout_field();
        if pc != m.cpu.pc && VECTORS.contains(&pc) && !VECTORS.contains(&m.cpu.pc) {
            m.force_interrupt();
            self.irq_injected += 1;
        }
        // The delayed load in flight: register (0 = none) and value.
        let theirs = if r.load_reg == 0 || r.load_reg >= 32 { (0, 0) } else { (r.load_reg as usize, r.load_value) };
        let ours = match m.cpu.pending_load() {
            (0, _) => (0, 0),
            l => l,
        };
        let mut problems = Vec::new();
        if pc != m.cpu.pc {
            problems.push(json!({"what": "pc", "core": h(m.cpu.pc), "mednafen": h(pc)}));
        } else {
            let (word, ours_word) = (ffi::peek(pc, 4), m.bus.read(pc, Width::Word).unwrap_or(0));
            if word != ours_word {
                problems.push(json!({"what": "instruction", "core": h(ours_word), "mednafen": h(word)}));
            }
        }
        if problems.is_empty() {
            for i in 1..32 {
                if m.cpu.regs[i] == r.gpr[i] {
                    if ours.0 != i {
                        self.io_dest[i] = None;
                    }
                    continue;
                }
                match self.io_dest[i].take() {
                    // A value read from a device: take Mednafen's.
                    Some((addr, w)) => {
                        m.cpu.regs[i] = r.gpr[i];
                        m.bus.poke_io(addr, w, r.gpr[i]);
                        self.io_injected += 1;
                    }
                    None => {
                        problems.push(json!({"what": reg(i as u32), "core": h(m.cpu.regs[i]), "mednafen": h(r.gpr[i])}))
                    }
                }
            }
            if m.cpu.hi != r.hi {
                problems.push(json!({"what": "hi", "core": h(m.cpu.hi), "mednafen": h(r.hi)}));
            }
            if m.cpu.lo != r.lo {
                problems.push(json!({"what": "lo", "core": h(m.cpu.lo), "mednafen": h(r.lo)}));
            }
            // An I/O load still in flight may carry a different value; it is
            // compared (and adopted) once it retires.
            let io_in_flight = ours.0 != 0 && self.io_dest[ours.0].is_some();
            if ours.0 != theirs.0 || (ours.1 != theirs.1 && !io_in_flight) {
                problems.push(json!({"what": "delayed load", "core": [reg(ours.0 as u32), h(ours.1)],
                                     "mednafen": [reg(theirs.0 as u32), h(theirs.1)]}));
            }
            use psx_core::cpu::cop0;
            let c = &m.cpu.cop0;
            for (what, core, theirs) in [
                ("sr", c[cop0::SR], r.sr),
                ("cause", c[cop0::CAUSE] & !0x400, r.cause & !0x400),
                ("epc", c[cop0::EPC], r.epc),
                ("badvaddr", c[cop0::BADVADDR], r.badvaddr),
            ] {
                if core != theirs {
                    problems.push(json!({"what": what, "core": h(core), "mednafen": h(theirs)}));
                }
            }
            if self.gte_changed {
                let theirs = ffi::gte();
                for (i, &v) in theirs.iter().enumerate() {
                    let core = if i < 32 { m.cpu.gte.read_data(i) } else { m.cpu.gte.read_ctrl(i - 32) };
                    if core != v {
                        let name = if i < 32 { format!("gte data {i}") } else { format!("gte ctrl {}", i - 32) };
                        problems.push(json!({"what": name, "core": h(core), "mednafen": h(v)}));
                    }
                }
            }
        }
        if !problems.is_empty() {
            self.divergence = json!({
                "step": self.done,
                "pc": h(pc),
                "mednafen_asm": disasm(ffi::peek(pc, 4), pc),
                "core_asm": disasm(m.bus.read(m.cpu.pc, Width::Word).unwrap_or(0), m.cpu.pc),
                "problems": problems,
                "mednafen": {"sr": h(r.sr), "cause": h(r.cause), "epc": h(r.epc)},
                "core": {"sr": h(m.cpu.cop0[12]), "cause": h(m.cpu.cop0[13]), "epc": h(m.cpu.cop0[14])},
                "recent": self.history.iter().cloned().collect::<Vec<_>>(),
            });
            self.stop = true;
            return;
        }
        let instruction = m.bus.read(pc, Width::Word).unwrap_or(0);
        self.history.push_back(json!({"step": self.done, "pc": h(pc), "asm": disasm(instruction, pc)}));
        if self.history.len() > self.context {
            self.history.pop_front();
        }
        let info = m.step();
        self.gte_changed = matches!(instruction >> 26, 0x12 | 0x32);
        if let Some((addr, rt)) = info.io_load {
            let w = match instruction >> 26 {
                0x20 | 0x24 => Width::Byte,
                0x21 | 0x25 => Width::Half,
                _ => Width::Word,
            };
            if rt != 0 {
                self.io_dest[rt as usize] = Some((addr & 0x1FFF_FFFF, w));
            }
        }
        self.done += 1;
    }
}

/// The differing byte ranges of `a` and `b` (at most 8, merged across gaps
/// under 16 bytes) and the count of differing bytes.
fn ranges(a: &[u8], b: &[u8], base: u32) -> (u64, Vec<Value>) {
    let (mut count, mut out) = (0u64, Vec::new());
    let mut open: Option<(usize, usize)> = None;
    for (i, _) in a.iter().zip(b).enumerate().filter(|(_, (x, y))| x != y) {
        count += 1;
        open = match open {
            Some((s, e)) if i < e + 16 => Some((s, i + 1)),
            Some(r) => {
                out.push(r);
                Some((i, i + 1))
            }
            None => Some((i, i + 1)),
        };
    }
    out.extend(open);
    let shown = out.iter().take(8).map(|&(s, e)| json!({"at": h(base + s as u32), "bytes": e - s})).collect();
    (count, shown)
}

/// Memory compared at each vblank: name, Mednafen's address space and
/// start, the core's bytes, and the address shown for offset 0.
const MEMORIES: [(&str, &str, u32, u32); 4] =
    [("ram", "ram", 0, 0), ("scratchpad", "cpu", 0x1F80_0000, 0x1F80_0000), ("vram", "gpu", 0, 0), ("spu_ram", "spu", 0, 0)];

#[derive(Default)]
struct MemoryDiff {
    frames: u64,
    /// VRAM differences while Mednafen's GPU was still busy (not counted).
    busy: u64,
    first: Value,
    last: Value,
}

fn core_memory(m: &Machine, name: &str) -> Vec<u8> {
    match name {
        "ram" => m.bus.ram.clone(),
        "scratchpad" => m.bus.scratch.to_vec(),
        "vram" => m.bus.gpu.vram.as_slice().iter().flat_map(|p| p.to_le_bytes()).collect(),
        _ => m.bus.spu.ram.clone(),
    }
}

fn compare_memory(emu: &Mednafen, m: &Machine, frame: u64, step: u64, diffs: &mut [MemoryDiff; 4]) {
    for ((name, space, start, base), d) in MEMORIES.iter().zip(diffs.iter_mut()) {
        let ours = core_memory(m, name);
        let Ok(theirs) = emu.read(space, *start, ours.len() as u32) else { continue };
        if ours == theirs {
            d.last = Value::Null;
            continue;
        }
        let (bytes, at) = ranges(&ours, &theirs, *base);
        // Mednafen's GPU takes time to draw; the core's draws at once.
        if *name == "vram" && ffi::gpu_busy() {
            d.busy += 1;
            continue;
        }
        d.frames += 1;
        d.last = json!({"frame": frame, "step": step, "bytes": bytes, "ranges": at});
        if d.first.is_null() {
            d.first = d.last.clone();
        }
    }
}

/// `<prefix>.{core,mednafen}.{ram,scratchpad,vram,spu_ram}.bin`, and the
/// core's screen and VRAM as PNGs.
fn dump_files(emu: &Mednafen, m: &Machine, prefix: &Path) -> Value {
    if let Some(parent) = prefix.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let file = |what: &str| std::path::PathBuf::from(format!("{}.{what}", prefix.display()));
    let mut out = serde_json::Map::new();
    let mut put = |what: String, ok: bool| {
        out.insert(what.clone(), if ok { json!(file(&what)) } else { Value::Null });
    };
    for (name, space, start, _) in MEMORIES {
        let ours = core_memory(m, name);
        put(format!("core.{name}.bin"), std::fs::write(file(&format!("core.{name}.bin")), &ours).is_ok());
        let theirs = emu.read(space, start, ours.len() as u32);
        let ok = theirs.is_ok_and(|b| std::fs::write(file(&format!("mednafen.{name}.bin")), b).is_ok());
        put(format!("mednafen.{name}.bin"), ok);
    }
    let (w, hgt, rgba) = m.bus.gpu.display_rgba();
    put("core.screen.png".into(), crate::write_png(&file("core.screen.png"), w, hgt, &rgba).is_ok());
    let vram = m.bus.gpu.vram.rgba15(0, 0, 1024, 512);
    put("core.vram.png".into(), crate::write_png(&file("core.vram.png"), 1024, 512, &vram).is_ok());
    Value::Object(out)
}

pub fn run(root: &Path, bios_path: &Path, steps: u64, context: usize, dump: Option<&Path>) -> (bool, Value) {
    let err = |code: &str, m: String| (false, json!({"error": {"code": code, "message": m}}));
    let bios = match std::fs::read(bios_path) {
        Ok(b) => b,
        Err(e) => return err("io_error", format!("{}: {e}", bios_path.display())),
    };
    let base = root.join(".psx/lockstep").join(std::process::id().to_string());
    let region = Region::of_bios(&bios);
    let mut emu = match Mednafen::open(&base, bios_path, None, region, false, false) {
        Ok(e) => e,
        Err(e) => return err("backend_unavailable", e),
    };
    let mut m = Machine::new(bios);
    // Interrupts and the displayed field come from Mednafen; device timing
    // is compared separately.
    m.bus.irq_sources = false;
    m.bus.external_field = true;
    let state = Rc::new(RefCell::new(State {
        m,
        steps,
        context,
        done: 0,
        history: VecDeque::with_capacity(context + 1),
        io_dest: [None; 32],
        io_injected: 0,
        irq_injected: 0,
        gte_changed: false,
        divergence: Value::Null,
        stop: false,
    }));
    let started = Instant::now();
    {
        let state = state.clone();
        emu.step_hook(move |pc, _| state.borrow_mut().check(pc));
    }
    // Every frame is a few hundred thousand instructions; this only bounds a
    // run that stops calling the hook.
    let mut frames = 0u64;
    let mut diffs: [MemoryDiff; 4] = Default::default();
    while !state.borrow().stop && frames < steps / 1000 + 100 {
        emu.frame(false);
        frames += 1;
        let s = state.borrow();
        compare_memory(&emu, &s.m, emu.vblanks(), s.done, &mut diffs);
    }
    emu.unhook();
    let secs = started.elapsed().as_secs_f64();
    let s = state.borrow();
    let mut dumped = Value::Null;
    if let Some(prefix) = dump {
        dumped = dump_files(&emu, &s.m, prefix);
    }
    let ok = s.divergence.is_null() && s.done >= steps;
    (
        ok,
        json!({
            "status": if !s.divergence.is_null() { "diverged" } else if s.done >= steps { "matched" } else { "stopped" },
            "steps_compared": s.done,
            "steps_requested": steps,
            "frames": emu.vblanks(),
            "final_pc": h(s.m.cpu.pc),
            "io_values_adopted": s.io_injected,
            "interrupts_injected": s.irq_injected,
            "core_dump": dumped,
            "divergence": s.divergence,
            "memory": MEMORIES.iter().zip(&diffs).map(|((name, ..), d)| (name.to_string(), json!({
                "frames_differing": d.frames,
                "frames_differing_while_gpu_busy": (*name == "vram").then_some(d.busy),
                "first": d.first,
                "at_end": d.last,
            }))).collect::<serde_json::Map<_, _>>(),
            "steps_per_second": (s.done as f64 / secs.max(1e-9)).round(),
            "compared": ["pc", "instruction word", "r1-r31", "hi", "lo", "delayed load",
                         "sr", "cause (but IP2)", "epc", "badvaddr", "gte after cop2/lwc2",
                         "ram, scratchpad, vram and spu ram at each vblank"],
            "oracle": {"backend": "mednafen", "version": ffi::version(), "region": region.name()},
        }),
    )
}
