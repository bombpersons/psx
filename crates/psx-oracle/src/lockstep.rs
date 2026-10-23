//! `psx-oracle lockstep`: the native core and Mednafen side by side from the
//! BIOS reset vector (no disc), compared before every instruction: PC, the
//! instruction word, r1-r31, HI, LO and the delayed load in flight.
//!
//! The two models' timing differs (the core's CPU costs are flat), so two
//! things Mednafen knows are adopted rather than counted as divergence:
//! - the value of every load from I/O space, which depends on device timing
//!   (applied to the destination register once the load retires, and stored
//!   into the core's I/O register file);
//! - interrupts: the core's interrupt line (Cause.IP2) follows Mednafen's,
//!   and when Mednafen enters an exception vector and the core did not, the
//!   core takes an external interrupt at the same point.
//!
//! Adoption counts are reported; anything else that differs stops the run.
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
    // Interrupts come from Mednafen; device timing is compared separately.
    m.bus.irq_sources = false;
    let state = Rc::new(RefCell::new(State {
        m,
        steps,
        context,
        done: 0,
        history: VecDeque::with_capacity(context + 1),
        io_dest: [None; 32],
        io_injected: 0,
        irq_injected: 0,
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
    while !state.borrow().stop && frames < steps / 1000 + 100 {
        emu.frame(false);
        frames += 1;
    }
    emu.unhook();
    let secs = started.elapsed().as_secs_f64();
    let s = state.borrow();
    let mut dumped = Value::Null;
    if let Some(prefix) = dump {
        if let Some(parent) = prefix.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let (w, hgt, rgba) = s.m.bus.gpu.display_rgba();
        let screen = std::path::PathBuf::from(format!("{}.screen.png", prefix.display()));
        let vram = std::path::PathBuf::from(format!("{}.vram.png", prefix.display()));
        let ok1 = crate::write_png(&screen, w, hgt, &rgba).is_ok();
        let ok2 = crate::write_png(&vram, 1024, 512, &s.m.bus.gpu.vram.rgba15(0, 0, 1024, 512)).is_ok();
        dumped = json!({"screen": ok1.then_some(screen), "vram": ok2.then_some(vram)});
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
            "steps_per_second": (s.done as f64 / secs.max(1e-9)).round(),
            "compared": ["pc", "instruction word", "r1-r31", "hi", "lo", "delayed load"],
            "oracle": {"backend": "mednafen", "version": ffi::version(), "region": region.name()},
        }),
    )
}
