//! Instruction tracing for analysis runs: a windowed per-instruction log with
//! the data each instruction produced, executed-code coverage, and call
//! targets. Driven from the run loop with [`Tracer::after_step`]; costs
//! nothing when no tracer exists.
//!
//! Trace line format (one instruction):
//! `frame cycle pc word  disassembly  ; effects` (cycle = system clock after the instruction)
//! where effects are the GPR (or HI/LO) the instruction wrote with its new
//! value, and any data access as `[addr]=value` (store) or `[addr]=>value`
//! (load; the register receives it after the delay slot). Calls add a
//! `-> target name (from caller)` line before the callee's first instruction
//! (when either side is in a traced range);
//! interrupts and exceptions add `!!` lines.

use std::collections::BTreeMap;
use std::io::Write;

use crate::bus::Bus;
use crate::cpu::{Cpu, StepInfo};
use crate::disasm::{disasm, reg};

const RAM_WORDS: usize = 0x20_0000 / 4;
const BIOS_WORDS: usize = 0x8_0000 / 4;

/// Names a code address given the current main RAM (for residency checks).
pub type Namer = Box<dyn Fn(u32, &[u8]) -> Option<String>>;

#[derive(Clone, Debug, Default)]
pub struct TraceConfig {
    /// Absolute frame (vblank count) at which tracing may start.
    pub from_frame: u64,
    /// Start at the first execution of this PC (after `from_frame`).
    pub start_pc: Option<u32>,
    /// Stop after this many frames of tracing.
    pub frames: Option<u64>,
    /// Physical `[start, end)` ranges whose instructions are written as lines; empty = all.
    pub ranges: Vec<(u32, u32)>,
    /// Cap on written instruction lines (coverage and calls continue past it).
    pub max_lines: u64,
}

#[derive(Clone, Debug, Default, serde::Serialize)]
pub struct CallStat {
    pub count: u64,
    pub first_frame: u64,
    pub last_frame: u64,
    /// Call sites (PC of the jal/jalr) with counts.
    pub callers: BTreeMap<u32, u64>,
}

pub struct Tracer {
    pub cfg: TraceConfig,
    out: Box<dyn Write>,
    namer: Option<Namer>,
    state: Window,
    /// Frame and cycle at which tracing started / stopped.
    pub started: Option<(u64, u64)>,
    pub stopped: Option<(u64, u64)>,
    /// Instructions executed inside the window.
    pub instructions: u64,
    pub lines: u64,
    /// In-range instructions not written because of `max_lines`.
    pub lines_dropped: u64,
    pub interrupts: u64,
    pub exceptions: BTreeMap<u8, u64>,
    ram_cover: Vec<u32>,
    bios_cover: Vec<u32>,
    pub calls: BTreeMap<u32, CallStat>,
    /// Steps until the callee's first instruction, and the call site.
    pending_call: Option<(u8, u32)>,
    io_error: Option<std::io::Error>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Window {
    Waiting,
    Active,
    Done,
}

/// The GPR an instruction writes without a load delay (loads and
/// MFCz/CFCz deliver after the delay slot and are shown as data accesses).
fn direct_dest(i: u32) -> Option<usize> {
    let rt = ((i >> 16) & 31) as usize;
    let rd = ((i >> 11) & 31) as usize;
    match i >> 26 {
        0x00 => match i & 0x3F {
            0x08 | 0x0C | 0x0D | 0x11 | 0x13 | 0x18..=0x1B => None,
            _ => Some(rd),
        },
        0x01 => (rt & 0x1E == 0x10).then_some(31),
        0x03 => Some(31),
        0x08..=0x0F => Some(rt),
        _ => None,
    }
    .filter(|&r| r != 0)
}

pub fn exception_name(code: u8) -> &'static str {
    match code {
        0x0 => "interrupt",
        0x4 => "load_address_error",
        0x5 => "store_address_error",
        0x6 => "instruction_bus_error",
        0x7 => "data_bus_error",
        0x8 => "syscall",
        0x9 => "break",
        0xA => "reserved_instruction",
        0xB => "coprocessor_unusable",
        0xC => "overflow",
        _ => "unknown",
    }
}

fn writes_hilo(i: u32) -> bool {
    i >> 26 == 0 && matches!(i & 0x3F, 0x11 | 0x13 | 0x18..=0x1B)
}

fn is_call(i: u32) -> bool {
    i >> 26 == 0x03 || (i >> 26 == 0 && i & 0x3F == 0x09) || (i >> 26 == 1 && (i >> 16) & 0x1E == 0x10)
}

impl Tracer {
    pub fn new(cfg: TraceConfig, out: Box<dyn Write>, namer: Option<Namer>) -> Self {
        Tracer {
            cfg,
            out,
            namer,
            state: Window::Waiting,
            started: None,
            stopped: None,
            instructions: 0,
            lines: 0,
            lines_dropped: 0,
            interrupts: 0,
            exceptions: BTreeMap::new(),
            ram_cover: vec![0; RAM_WORDS],
            bios_cover: vec![0; BIOS_WORDS],
            calls: BTreeMap::new(),
            pending_call: None,
            io_error: None,
        }
    }

    fn in_range(&self, phys: u32) -> bool {
        self.cfg.ranges.is_empty() || self.cfg.ranges.iter().any(|&(s, e)| phys >= s && phys < e)
    }

    fn write(&mut self, line: std::fmt::Arguments) {
        if self.io_error.is_none() {
            if let Err(e) = self.out.write_fmt(line) {
                self.io_error = Some(e);
            }
        }
    }

    pub fn after_step(&mut self, info: &StepInfo, cpu: &Cpu, bus: &Bus) {
        match self.state {
            Window::Done => return,
            Window::Waiting => {
                if bus.frames < self.cfg.from_frame
                    || self.cfg.start_pc.is_some_and(|p| p & 0x1FFF_FFFF != info.pc & 0x1FFF_FFFF)
                {
                    return;
                }
                self.state = Window::Active;
                self.started = Some((bus.frames, bus.time()));
            }
            Window::Active => {}
        }
        if let (Some(n), Some((f0, _))) = (self.cfg.frames, self.started) {
            if bus.frames >= f0 + n {
                self.stop(bus);
                return;
            }
        }
        let pc = info.pc;
        let phys = pc & 0x1FFF_FFFF;
        let shown = self.in_range(phys);
        let room = self.lines < self.cfg.max_lines;

        // Call bookkeeping: the callee's first step is two after the call.
        if let Some((n, site)) = self.pending_call {
            if n <= 1 {
                self.pending_call = None;
                let c =
                    self.calls.entry(pc).or_insert_with(|| CallStat { first_frame: bus.frames, ..Default::default() });
                c.count += 1;
                c.last_frame = bus.frames;
                if c.callers.len() < 64 || c.callers.contains_key(&site) {
                    *c.callers.entry(site).or_default() += 1;
                }
                if (shown || self.in_range(site & 0x1FFF_FFFF)) && room {
                    let name =
                        self.namer.as_ref().and_then(|n| n(pc, &bus.ram)).map(|n| format!(" {n}")).unwrap_or_default();
                    self.write(format_args!("-> {pc:08x}{name} (from {site:08x})\n"));
                }
            } else {
                self.pending_call = Some((n - 1, site));
            }
        }

        if let Some(e) = info.exception {
            if e == 0 {
                self.interrupts += 1;
                if shown && room {
                    self.write(format_args!("!! {} {} interrupt before {pc:08x}\n", bus.frames, bus.time()));
                }
                return; // the instruction did not execute
            }
            *self.exceptions.entry(e).or_default() += 1;
            if shown && room {
                let name = exception_name(e);
                self.write(format_args!("!! {} {} exception {name} at {pc:08x}\n", bus.frames, bus.time()));
            }
        }

        self.instructions += 1;
        match phys {
            0..=0x7F_FFFF => {
                let w = &mut self.ram_cover[(phys as usize & 0x1F_FFFF) / 4];
                *w = w.saturating_add(1);
            }
            0x1FC0_0000..=0x1FC7_FFFF => {
                let w = &mut self.bios_cover[(phys as usize - 0x1FC0_0000) / 4];
                *w = w.saturating_add(1);
            }
            _ => {}
        }
        let i = info.instruction;
        if info.exception.is_none() && is_call(i) {
            self.pending_call = Some((2, pc));
        }
        if !shown {
            return;
        }
        if !room {
            self.lines_dropped += 1;
            return;
        }
        self.lines += 1;
        let mut fx = String::new();
        if info.exception.is_none() {
            if let Some(r) = direct_dest(i) {
                fx += &format!(" {}={:08x}", reg(r as u32), cpu.regs[r]);
            }
            if writes_hilo(i) {
                fx += &format!(" hi={:08x} lo={:08x}", cpu.hi, cpu.lo);
            }
        }
        if let Some(m) = info.mem {
            let v = match m.width {
                1 => format!("{:02x}", m.value & 0xFF),
                2 => format!("{:04x}", m.value & 0xFFFF),
                _ => format!("{:08x}", m.value),
            };
            fx += &format!(" [{:08x}]{}{v}", m.addr, if m.store { "=" } else { "=>" });
        }
        let sep = if fx.is_empty() { "" } else { "  ;" };
        let text = disasm(i, pc);
        self.write(format_args!("{} {} {pc:08x} {i:08x}  {text:<28}{sep}{fx}\n", bus.frames, bus.time()));
    }

    fn stop(&mut self, bus: &Bus) {
        if self.state == Window::Active {
            self.stopped = Some((bus.frames, bus.time()));
        }
        self.state = Window::Done;
    }

    /// Flush the trace; returns the first write error, if any.
    pub fn finish(&mut self, bus: &Bus) -> std::io::Result<()> {
        if self.state == Window::Active {
            self.stop(bus);
        }
        if let Some(e) = self.io_error.take() {
            return Err(e);
        }
        self.out.flush()
    }

    /// Executed code as `(start, end, instructions, executions)` runs of
    /// consecutive executed words, in KSEG0/KSEG1 addresses.
    pub fn coverage(&self) -> Vec<(u32, u32, u32, u64)> {
        let mut out = Vec::new();
        for (cover, base) in [(&self.ram_cover, 0x8000_0000u32), (&self.bios_cover, 0xBFC0_0000)] {
            let mut run: Option<(usize, u64)> = None;
            for (k, &n) in cover.iter().chain(std::iter::once(&0)).enumerate() {
                match (n, run) {
                    (0, Some((s, total))) => {
                        out.push((base + 4 * s as u32, base + 4 * k as u32, (k - s) as u32, total));
                        run = None;
                    }
                    (0, None) => {}
                    (n, Some((s, total))) => run = Some((s, total + n as u64)),
                    (n, None) => run = Some((k, n as u64)),
                }
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn destinations() {
        assert_eq!(direct_dest(0x2508_0001), Some(8)); // addiu t0,t0,1
        assert_eq!(direct_dest(0x0C00_0000), Some(31)); // jal
        assert_eq!(direct_dest(0x8C82_0010), None); // lw (delayed)
        assert_eq!(direct_dest(0x0000_0000), None); // nop writes r0
        assert!(writes_hilo(0x0085_001A)); // div a0,a1
        assert!(is_call(0x0040_F809)); // jalr v0
        assert!(!is_call(0x03E0_0008)); // jr ra
    }
}
