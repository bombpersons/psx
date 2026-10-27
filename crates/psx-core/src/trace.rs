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
/// RAM a code page holds, in bytes.
pub const CODE_PAGE: usize = 0x1000;
/// Most bytes of code page copies a trace keeps.
pub const MAX_CODE_BYTES: usize = 64 << 20;

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
    /// Call sites (PC of the jal/jalr) with counts, keyed by the code
    /// page copy the call ran from (`NO_COPY` when none holds it) and pc.
    pub callers: BTreeMap<(u32, u32), u64>,
}

/// The copy of a word no code page copy holds (past `MAX_CODE_BYTES`, or
/// run from the instruction cache with no copy holding it).
pub const NO_COPY: u32 = u32::MAX;

/// A page's latest copy: its index, its bytes and each word's executions.
type Latest = (u32, Box<[u8]>, Box<[u32]>);

/// One copy of a code page.
pub struct CodeCopy {
    pub frame: u64,
    /// The page's KSEG0 address.
    pub addr: u32,
    pub data: CopyData,
}

/// A page's first copy is whole; each later one holds the words that
/// changed since the page's copy before it.
pub enum CopyData {
    Whole(Box<[u8]>),
    /// The copy before (an index), and `(word index, word)` in order.
    Changed(u32, Vec<(u16, u32)>),
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
    /// Each RAM page's history: a copy of it, numbered by its index, taken
    /// when code first runs in it and again whenever code runs in it after
    /// a store changed it.
    pub code_pages: Vec<CodeCopy>,
    code_bytes: usize,
    /// Per page, its latest copy with its bytes and the executions of each
    /// of its words, and the bus's store count for the page
    /// ([`Bus::page_writes`]) when last compared with it.
    page_copy: Vec<Option<Latest>>,
    page_seen: Vec<u32>,
    /// Executions of copies no longer their page's latest: `(copy, word
    /// index)`.
    counts: BTreeMap<(u32, u32), u64>,
    /// Copies not kept past `MAX_CODE_BYTES`.
    pub code_pages_dropped: u64,
    /// Executions credited to an earlier copy of a page because RAM no
    /// longer held the instruction (it ran from the instruction cache).
    pub ran_from_cache: u64,
    /// Executions no copy holds: `(word index, instruction)`.
    uncopied: BTreeMap<(u32, u32), u64>,
    bios_cover: Vec<u32>,
    /// Call targets keyed by the copy the callee's first instruction ran
    /// from (`NO_COPY` outside RAM or when none holds it) and its address.
    pub calls: BTreeMap<(u32, u32), CallStat>,
    /// Steps until the callee's first instruction, the call site and its copy.
    pending_call: Option<(u8, u32, u32)>,
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
            code_pages: Vec::new(),
            code_bytes: 0,
            page_copy: (0..0x20_0000 / CODE_PAGE).map(|_| None).collect(),
            page_seen: vec![0; 0x20_0000 / CODE_PAGE],
            counts: BTreeMap::new(),
            code_pages_dropped: 0,
            ran_from_cache: 0,
            uncopied: BTreeMap::new(),
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
        let interrupted = info.exception == Some(0);
        // The copy this instruction runs from (an interrupted one did not
        // run: its page's latest copy, for a call landing there).
        let k = (phys as usize & 0x1F_FFFF) / 4;
        let copy = match phys {
            0..=0x7F_FFFF if interrupted => self.page_now(k / (CODE_PAGE / 4), bus).unwrap_or(NO_COPY),
            0..=0x7F_FFFF => self.copy_for(k, info.instruction, bus),
            _ => NO_COPY,
        };

        // Call bookkeeping: the callee's first step is two after the call.
        if let Some((n, site, site_copy)) = self.pending_call {
            if n <= 1 {
                self.pending_call = None;
                let c = self
                    .calls
                    .entry((copy, pc))
                    .or_insert_with(|| CallStat { first_frame: bus.frames, ..Default::default() });
                c.count += 1;
                c.last_frame = bus.frames;
                if c.callers.len() < 64 || c.callers.contains_key(&(site_copy, site)) {
                    *c.callers.entry((site_copy, site)).or_default() += 1;
                }
                if (shown || self.in_range(site & 0x1FFF_FFFF)) && room {
                    let name =
                        self.namer.as_ref().and_then(|n| n(pc, &bus.ram)).map(|n| format!(" {n}")).unwrap_or_default();
                    self.write(format_args!("-> {pc:08x}{name} (from {site:08x})\n"));
                }
            } else {
                self.pending_call = Some((n - 1, site, site_copy));
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
                if copy == NO_COPY {
                    *self.uncopied.entry((k as u32, info.instruction)).or_default() += 1;
                } else {
                    match &mut self.page_copy[k / (CODE_PAGE / 4)] {
                        Some((c, _, n)) if *c == copy => {
                            let n = &mut n[k % (CODE_PAGE / 4)];
                            *n = n.saturating_add(1);
                        }
                        _ => *self.counts.entry((copy, k as u32)).or_default() += 1,
                    }
                }
                self.ram_cover[k] = self.ram_cover[k].saturating_add(1);
            }
            0x1FC0_0000..=0x1FC7_FFFF => {
                let w = &mut self.bios_cover[(phys as usize - 0x1FC0_0000) / 4];
                *w = w.saturating_add(1);
            }
            _ => {}
        }
        let i = info.instruction;
        if info.exception.is_none() && is_call(i) {
            self.pending_call = Some((2, pc, copy));
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

    /// Page `p`'s latest copy, after a new one is taken if a store reached
    /// the page since it was last compared and changed it (or none was).
    fn page_now(&mut self, p: usize, bus: &Bus) -> Option<u32> {
        let writes = bus.page_writes.get(p).copied().unwrap_or(0);
        let seen = std::mem::replace(&mut self.page_seen[p], writes);
        match &self.page_copy[p] {
            Some((c, _, _)) if writes == seen => return Some(*c),
            Some((c, b, _)) if b[..] == bus.ram[p * CODE_PAGE..(p + 1) * CODE_PAGE] => return Some(*c),
            _ => {}
        }
        self.take_copy(p, bus)
    }

    /// Copies page `p` as RAM holds it now, keeping only the words that
    /// changed when the page has a copy already.
    fn take_copy(&mut self, p: usize, bus: &Bus) -> Option<u32> {
        let now = &bus.ram[p * CODE_PAGE..(p + 1) * CODE_PAGE];
        let data = match &self.page_copy[p] {
            None => CopyData::Whole(now.into()),
            Some((c, b, _)) => CopyData::Changed(
                *c,
                (0..CODE_PAGE / 4)
                    .filter(|&j| b[4 * j..4 * j + 4] != now[4 * j..4 * j + 4])
                    .map(|j| (j as u16, u32::from_le_bytes(now[4 * j..4 * j + 4].try_into().unwrap())))
                    .collect(),
            ),
        };
        let size = match &data {
            CopyData::Whole(b) => b.len(),
            CopyData::Changed(_, w) => 6 * w.len(),
        };
        if self.code_bytes + size > MAX_CODE_BYTES {
            self.code_pages_dropped += 1;
            return None;
        }
        self.code_bytes += size;
        let c = self.code_pages.len() as u32;
        self.code_pages.push(CodeCopy { frame: bus.frames, addr: 0x8000_0000 + (p * CODE_PAGE) as u32, data });
        let fresh = (c, now.into(), vec![0; CODE_PAGE / 4].into());
        if let Some((old, _, n)) = self.page_copy[p].replace(fresh) {
            let k0 = (p * CODE_PAGE / 4) as u32;
            for (j, &n) in n.iter().enumerate().filter(|(_, n)| **n > 0) {
                *self.counts.entry((old, k0 + j as u32)).or_default() += n as u64;
            }
        }
        Some(c)
    }

    /// The word at byte `o` of copy `c`.
    pub fn copy_word(&self, mut c: u32, o: usize) -> u32 {
        loop {
            match &self.code_pages[c as usize].data {
                CopyData::Whole(b) => return u32::from_le_bytes(b[o..o + 4].try_into().unwrap()),
                CopyData::Changed(before, w) => match w.binary_search_by_key(&((o / 4) as u16), |e| e.0) {
                    Ok(i) => return w[i].1,
                    Err(_) => c = *before,
                },
            }
        }
    }

    /// The copy word `k` ran from as instruction `i`: its page's latest,
    /// a new one when RAM holds `i` there and the latest does not (a store
    /// the bus did not count), or, when RAM no longer holds it (the
    /// instruction cache did), the latest earlier copy that did.
    fn copy_for(&mut self, k: usize, i: u32, bus: &Bus) -> u32 {
        let p = 4 * k / CODE_PAGE;
        let o = 4 * k % CODE_PAGE;
        let Some(c) = self.page_now(p, bus) else { return NO_COPY };
        if self.copy_word(c, o) == i {
            return c;
        }
        if u32::from_le_bytes(bus.ram[4 * k..4 * k + 4].try_into().unwrap()) == i {
            return self.take_copy(p, bus).unwrap_or(NO_COPY);
        }
        let page = 0x8000_0000 + (p * CODE_PAGE) as u32;
        match (0..c).rev().find(|&e| self.code_pages[e as usize].addr == page && self.copy_word(e, o) == i) {
            Some(e) => {
                self.ran_from_cache += 1;
                e
            }
            None => NO_COPY,
        }
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

    /// Executed RAM words as `(copy, KSEG0 address, instruction word,
    /// executions)`, in copy then address order, then the words no copy
    /// holds (`NO_COPY`).
    pub fn executed(&self) -> Vec<(u32, u32, u32, u64)> {
        let mut all = self.counts.clone();
        for (p, (c, _, n)) in self.page_copy.iter().enumerate().filter_map(|(p, e)| Some((p, e.as_ref()?))) {
            for (j, &n) in n.iter().enumerate().filter(|(_, n)| **n > 0) {
                *all.entry((*c, (p * CODE_PAGE / 4 + j) as u32)).or_default() += n as u64;
            }
        }
        let word = |c: u32, k: u32| self.copy_word(c, 4 * k as usize % CODE_PAGE);
        let mut out: Vec<_> = all.into_iter().map(|((c, k), n)| (c, 0x8000_0000 + 4 * k, word(c, k), n)).collect();
        out.extend(self.uncopied.iter().map(|(&(k, i), &n)| (NO_COPY, 0x8000_0000 + 4 * k, i, n)));
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Runs instruction `i` at `pc` through the tracer.
    fn ran(t: &mut Tracer, cpu: &Cpu, bus: &Bus, pc: u32, i: u32) {
        let info = StepInfo { pc, instruction: i, exception: None, io_load: None, mem: None };
        t.after_step(&info, cpu, bus);
    }

    fn put(bus: &mut Bus, addr: u32, w: u32) {
        bus.write(addr, crate::bus::Width::Word, w).unwrap();
    }

    const JR_RA: u32 = 0x03E0_0008;
    const JAL: u32 = 0x0C00_4000; // jal 80010000

    #[test]
    fn code_loaded_over_code_runs_from_a_copy_of_its_own() {
        let mut bus = Bus::new(vec![]);
        bus.page_writes = vec![0; 0x20_0000 / CODE_PAGE];
        let cpu = Cpu::new();
        let mut t = Tracer::new(TraceConfig::default(), Box::new(std::io::sink()), None);
        // A call into a page holding `jr ra` at 80010000.
        put(&mut bus, 0x8001_0000, JR_RA);
        put(&mut bus, 0x8001_0004, 1);
        put(&mut bus, 0x8002_0000, JAL);
        for (pc, i) in [(0x8002_0000, JAL), (0x8002_0004, 0), (0x8001_0000, JR_RA)] {
            ran(&mut t, &cpu, &bus, pc, i);
        }
        // A store that leaves the page unchanged keeps its copy.
        put(&mut bus, 0x8001_0004, 1);
        ran(&mut t, &cpu, &bus, 0x8001_0000, JR_RA);
        assert_eq!(t.code_pages.len(), 2);
        // Other code loaded there with the same word where it is called.
        put(&mut bus, 0x8001_0004, 2);
        for (pc, i) in [(0x8002_0000, JAL), (0x8002_0004, 0), (0x8001_0000, JR_RA)] {
            ran(&mut t, &cpu, &bus, pc, i);
        }
        assert_eq!(t.code_pages.len(), 3);
        assert_eq!(t.code_pages[2].addr, 0x8001_0000);
        let calls: Vec<_> = t.calls.iter().map(|(&k, c)| (k, c.count, c.callers.clone())).collect();
        // The caller's page was copied first.
        let site = BTreeMap::from([((0, 0x8002_0000), 1)]);
        assert_eq!(calls, [((1, 0x8001_0000), 1, site.clone()), ((2, 0x8001_0000), 1, site)]);
        assert_eq!(
            t.executed(),
            [(0, 0x8002_0000, JAL, 2), (0, 0x8002_0004, 0, 2), (1, 0x8001_0000, JR_RA, 2), (2, 0x8001_0000, JR_RA, 1)]
        );
    }

    #[test]
    fn a_word_ram_no_longer_holds_ran_from_the_copy_that_did() {
        let mut bus = Bus::new(vec![]);
        bus.page_writes = vec![0; 0x20_0000 / CODE_PAGE];
        let cpu = Cpu::new();
        let mut t = Tracer::new(TraceConfig::default(), Box::new(std::io::sink()), None);
        put(&mut bus, 0x8001_0000, 7);
        ran(&mut t, &cpu, &bus, 0x8001_0000, 7);
        put(&mut bus, 0x8001_0000, 8);
        // The instruction cache still holds 7.
        ran(&mut t, &cpu, &bus, 0x8001_0000, 7);
        assert_eq!(t.code_pages.len(), 2);
        assert_eq!(t.ran_from_cache, 1);
        // A store the bus did not count is found by the word that ran.
        bus.ram[0x1_0000..0x1_0004].copy_from_slice(&9u32.to_le_bytes());
        ran(&mut t, &cpu, &bus, 0x8001_0000, 9);
        // And a word no copy holds is kept apart.
        ran(&mut t, &cpu, &bus, 0x8001_0000, 5);
        assert_eq!(t.executed(), [(0, 0x8001_0000, 7, 2), (2, 0x8001_0000, 9, 1), (NO_COPY, 0x8001_0000, 5, 1)]);
    }

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
