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

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::io::Write;

use crate::bus::Bus;
use crate::cpu::{Cpu, StepInfo, cop0};
use crate::disasm::{disasm, reg};

const RAM_WORDS: usize = 0x20_0000 / 4;
const BIOS_WORDS: usize = 0x8_0000 / 4;
/// RAM a code page holds, in bytes.
pub const CODE_PAGE: usize = 0x1000;
/// Most bytes of code page copies a trace keeps.
pub const MAX_CODE_BYTES: usize = 64 << 20;
/// Calls deep enough to remember where each returns to.
const MAX_RETURNS: usize = 1024;
const JR_RA: u32 = 0x03E0_0008;
/// A frame no `jr ra` returns to (an exception's, or a landing's).
const NO_RETURN: u32 = u32::MAX;
/// The owner of words run with no call under way ([`Tracer::owners`] index).
pub const OWNER_TOP: u32 = 0;
/// Most values kept of a word's owners or `sp` (see [`Seen`]).
const MAX_SEEN: usize = 16;

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
    /// The moment of the first call: the number of the latest copy taken then.
    pub first_moment: u32,
    /// The moments a call to it ran in, from the call until it returned,
    /// as `[from, to]` spans of moments, in order.
    pub moments: Vec<(u32, u32)>,
    pub last_frame: u64,
    /// Call sites (PC of the jal/jalr) with counts, keyed by the code
    /// page copy the call ran from (`NO_COPY` when none holds it) and pc.
    pub callers: BTreeMap<(u32, u32), u64>,
}

/// The copy of a word no code page copy holds (past `MAX_CODE_BYTES`, or
/// run from the instruction cache with no copy holding it).
pub const NO_COPY: u32 = u32::MAX;

/// How a word ran.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Ran {
    pub count: u64,
    /// The moment it first ran.
    pub first: u32,
    /// It continues the code before it: it ran straight on from the word
    /// before (which was no jump's delay slot, though it may be a branch's:
    /// a branch taken or not goes on there), or a call two words before
    /// returned to it.
    pub continues: bool,
}

impl Ran {
    fn add(&mut self, o: Ran) {
        self.first = if self.count == 0 { o.first } else { self.first.min(o.first) };
        self.count += o.count;
        self.continues |= o.continues;
    }
}

/// Adds `moment` (no earlier than the last) to spans of moments.
fn at(moments: &mut Vec<(u32, u32)>, moment: u32) {
    match moments.last_mut() {
        Some(l) if moment <= l.1.saturating_add(1) => l.1 = l.1.max(moment),
        _ => moments.push((moment, moment)),
    }
}

/// A page's latest copy: its number, bytes, how each word ran, and the
/// owner each word last ran for (`u32::MAX` before it ran).
type Latest = (u32, Box<[u8]>, Box<[Ran]>, Box<[u32]>);

/// What a word ran inside: the frame on top of the trace's call stack.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Owner {
    /// No call under way since tracing started.
    Top,
    /// A `jr ra` to an address the stack did not hold, by where it landed
    /// `(copy, address)`: what runs under it ran inside no call the stack
    /// knows. See [`Tracer::landing`] for what the run says of it.
    Landed(u32, u32),
    /// A call to `(copy, address)`, as [`Tracer::calls`] keys it.
    Call(u32, u32),
    /// An exception, by its handler's first word `(copy, address)`.
    Exception(u32, u32),
}

/// A call or exception under way.
#[derive(Clone, Copy, Debug)]
struct Frame {
    /// Where a call returns to (physical), or `NO_RETURN`.
    ret: u32,
    /// Its owner (an [`Tracer::owners`] index), once its first word ran.
    owner: Option<u32>,
    /// `sp` when its first word ran.
    sp: u32,
    /// An exception's EPC.
    epc: Option<u32>,
    /// An exception's: the word that ran before it was taken.
    last: Option<(u32, Option<u32>, u32)>,
}

/// Values seen of something, up to `MAX_SEEN`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Seen {
    pub values: Vec<u32>,
    /// More were seen than kept.
    pub more: bool,
}

impl Seen {
    fn add(&mut self, v: u32) {
        if !self.values.contains(&v) {
            if self.values.len() < MAX_SEEN {
                self.values.push(v);
            } else {
                self.more = true;
            }
        }
    }
}

/// What the run says of a landing (an [`Owner::Landed`]) at the address R:
/// facts that let a reader guess whose code resumed there.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Landing {
    /// The copy of R's page those returns were in: the landing's, or the
    /// nearest before it in the page's history that has any (a page is
    /// copied again when anything in it changes, data beside code too).
    pub returned_copy: Option<u32>,
    /// The owners R ran inside after a call two words before returned to
    /// it through the stack in that copy (empty: never returned to so).
    pub returned_inside: Seen,
    /// `sp` there after those returns.
    pub returned_sp: Seen,
    /// `sp` at each landing.
    pub landed_sp: Seen,
}

/// How the trace's call stack kept its place.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct StackStats {
    /// `jr ra` to an address no frame held.
    pub returns_not_held: u64,
    /// Frames pushed out by `MAX_RETURNS`.
    pub frames_dropped: u64,
    /// `rfe` returns to neither EPC nor the word after it (a switch to
    /// other code, as between threads).
    pub exceptions_left_elsewhere: u64,
    /// `rfe` with no exception frame on the stack.
    pub rfe_without_exception: u64,
}

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
    /// Executions of copies no longer their page's latest, and the moment
    /// each word first ran: `(copy, word index)`.
    counts: BTreeMap<(u32, u32), Ran>,
    /// Copies not kept past `MAX_CODE_BYTES`.
    pub code_pages_dropped: u64,
    /// Executions credited to an earlier copy of a page because RAM no
    /// longer held the instruction (it ran from the instruction cache).
    pub ran_from_cache: u64,
    /// Executions no copy holds, and the moment each first ran: `(word
    /// index, instruction)`.
    uncopied: BTreeMap<(u32, u32), Ran>,
    bios_cover: Vec<u32>,
    /// Call targets keyed by the copy the callee's first instruction ran
    /// from (`NO_COPY` outside RAM or when none holds it) and its address.
    pub calls: BTreeMap<(u32, u32), CallStat>,
    /// Steps until the callee's first instruction, the call site and its copy.
    pending_call: Option<(u8, u32, u32)>,
    /// The physical pc that ran last, the jump or branch whose delay slot
    /// it was, and its instruction.
    last: Option<(u32, Option<u32>, u32)>,
    /// The calls and exceptions under way, most recent last.
    returns: Vec<Frame>,
    /// Each word's owners: what ran it, indexed by `owner_ids`.
    pub owners: Vec<Owner>,
    owner_ids: HashMap<Owner, u32>,
    /// Per owner, the words run inside it as `(copy, address)` (`NO_COPY`
    /// and the KSEG1 address for the BIOS ROM).
    bodies: Vec<BTreeSet<(u32, u32)>>,
    /// Per owner, the words entered by a jump (`j`, or `jr` but a return)
    /// and whether `sp` was back to its value at the owner's first word:
    /// bit 0 seen back (the frame torn down), bit 1 seen not.
    jumps: Vec<BTreeMap<(u32, u32), u8>>,
    /// Per word (`(copy, address)`) a call two words before returned to
    /// through the stack: the owners it ran inside then, and `sp`.
    returned_to: HashMap<(u32, u32), (Seen, Seen)>,
    /// Per landing owner, `sp` at each landing.
    landed_sp: HashMap<u32, Seen>,
    /// The owner each BIOS ROM word last ran for.
    bios_owner: Vec<u32>,
    pub stack: StackStats,
    /// An exception was taken: its EPC, for the frame its handler's first
    /// word pushes.
    pending_exception: Option<u32>,
    /// `rfe` ran: the next word leaves the exception frame.
    leaving_exception: bool,
    /// `sp` after the step before.
    last_sp: u32,
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

/// A conditional branch: taken or not, the CPU after its delay slot is at
/// its target or the word after the slot, the code it is part of.
fn is_branch(i: u32) -> bool {
    has_delay_slot(i) && !matches!(i >> 26, 0x00 | 0x02 | 0x03)
}

/// A word's address as owners list it: KSEG0 for RAM (mirrors folded),
/// KSEG1 for the BIOS ROM.
fn word_addr(phys: u32) -> u32 {
    match phys {
        0..=0x7F_FFFF => 0x8000_0000 + (phys & 0x1F_FFFC),
        0x1FC0_0000..=0x1FC7_FFFF => 0xA000_0000 + phys,
        _ => phys,
    }
}

fn is_rfe(i: u32) -> bool {
    i >> 25 == 0x21 && i & 0x3F == 0x10
}

/// A jump or branch: the word after it is its delay slot.
fn has_delay_slot(i: u32) -> bool {
    match i >> 26 {
        0x00 => matches!(i & 0x3F, 0x08 | 0x09),
        0x01..=0x07 => true,
        0x10..=0x13 => (i >> 21) & 0x1F == 0x08,
        _ => false,
    }
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
            last: None,
            returns: Vec::new(),
            owners: vec![Owner::Top],
            owner_ids: HashMap::from([(Owner::Top, OWNER_TOP)]),
            bodies: vec![BTreeSet::new()],
            jumps: vec![BTreeMap::new()],
            returned_to: HashMap::new(),
            landed_sp: HashMap::new(),
            bios_owner: vec![u32::MAX; BIOS_WORDS],
            stack: StackStats::default(),
            pending_exception: None,
            leaving_exception: false,
            last_sp: 0,
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

        let moment = self.moment();
        let sp_before = std::mem::replace(&mut self.last_sp, cpu.regs[29]);

        // Leaving an exception (the word after `rfe`) drops its frame and
        // what is above it. Back where it was taken, the code before is
        // the word before the one it interrupted.
        if std::mem::take(&mut self.leaving_exception) {
            match self.returns.iter().rposition(|f| f.epc.is_some()) {
                Some(k) => {
                    let f = self.returns[k];
                    let epc = f.epc.unwrap_or(0) & 0x1FFF_FFFF;
                    if phys == epc {
                        self.last = f.last;
                    } else if phys != epc.wrapping_add(4) {
                        self.stack.exceptions_left_elsewhere += 1;
                    }
                    self.returns.truncate(k);
                }
                None => self.stack.rfe_without_exception += 1,
            }
        }
        // An exception's handler: a frame of its own.
        if let Some(epc) = self.pending_exception.take() {
            let id = self.owner_id(Owner::Exception(copy, pc));
            self.push(Frame { ret: NO_RETURN, owner: Some(id), sp: sp_before, epc: Some(epc), last: self.last });
            // The CPU, not the word before, brought it here.
            self.last = None;
        }

        // Call bookkeeping: the callee's first step is two after the call.
        if let Some((n, site, site_copy)) = self.pending_call {
            if n <= 1 {
                self.pending_call = None;
                let id = self.owner_id(Owner::Call(copy, pc));
                if let Some(r) = self.returns.last_mut().filter(|r| r.ret == site.wrapping_add(8) & 0x1FFF_FFFF) {
                    r.owner = Some(id);
                    r.sp = sp_before;
                }
                let c = self.calls.entry((copy, pc)).or_insert_with(|| CallStat {
                    first_frame: bus.frames,
                    first_moment: moment,
                    ..Default::default()
                });
                c.count += 1;
                c.last_frame = bus.frames;
                at(&mut c.moments, moment);
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
            self.pending_exception = Some(cpu.cop0[cop0::EPC]);
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
        // Run straight on from the word before, not as a jump's delay
        // slot's successor (a jump may have landed there instead) though
        // maybe as a branch's, or returned to by `jr ra` from a call two
        // words before.
        let after = |l: u32| l.wrapping_add(4) == phys;
        let stepped = self.last.is_some_and(|(l, slot, _)| after(l) && slot.is_none_or(is_branch));
        let mut returned = false;
        // The jump (`j`, `jr`) whose delay slot ran last: this word is
        // where it went.
        let jump =
            self.last.and_then(|(_, slot, _)| slot).filter(|&j| j >> 26 == 0x02 || (j >> 26 == 0 && j & 0x3F == 0x08));
        if jump == Some(JR_RA) {
            let at = (copy, word_addr(phys));
            if let Some(k) = self.returns.iter().rposition(|r| r.ret == phys) {
                self.returns.truncate(k);
                returned = true;
                let owner = self.top().0;
                let (owners, sps) = self.returned_to.entry(at).or_default();
                owners.add(owner);
                sps.add(sp_before);
            } else {
                // What runs now ran inside no call the stack knows: a
                // landing, owned by where it landed. One landing ends the
                // landing it ran under.
                self.stack.returns_not_held += 1;
                let id = self.owner_id(Owner::Landed(at.0, at.1));
                self.landed_sp.entry(id).or_default().add(sp_before);
                let f = Frame { ret: NO_RETURN, owner: Some(id), sp: sp_before, epc: None, last: None };
                let on_landing = self
                    .returns
                    .last()
                    .and_then(|t| t.owner)
                    .is_some_and(|o| matches!(self.owners[o as usize], Owner::Landed(..)));
                match self.returns.last_mut() {
                    Some(t) if on_landing => *t = f,
                    _ => self.push(f),
                }
            }
        }
        let slot = self.last.and_then(|(l, _, i)| (after(l) && has_delay_slot(i)).then_some(i));
        self.last = Some((phys, slot, info.instruction));
        let (owner, owner_sp) = self.top();
        if info.exception.is_none() && is_call(info.instruction) {
            self.push(Frame { ret: phys.wrapping_add(8), owner: None, sp: 0, epc: None, last: None });
        }
        let continues = stepped || returned;
        let word = match phys {
            0..=0x7F_FFFF => {
                let ran = Ran { count: 1, first: moment, continues };
                let addr = 0x8000_0000 + 4 * k as u32;
                if copy == NO_COPY {
                    self.uncopied.entry((k as u32, info.instruction)).or_default().add(ran);
                    self.bodies[owner as usize].insert((copy, addr));
                } else {
                    match &mut self.page_copy[k / (CODE_PAGE / 4)] {
                        Some((c, _, w, o)) if *c == copy => {
                            w[k % (CODE_PAGE / 4)].add(ran);
                            if std::mem::replace(&mut o[k % (CODE_PAGE / 4)], owner) != owner {
                                self.bodies[owner as usize].insert((copy, addr));
                            }
                        }
                        _ => {
                            self.counts.entry((copy, k as u32)).or_default().add(ran);
                            self.bodies[owner as usize].insert((copy, addr));
                        }
                    }
                }
                self.ram_cover[k] = self.ram_cover[k].saturating_add(1);
                Some((copy, addr))
            }
            0x1FC0_0000..=0x1FC7_FFFF => {
                let j = (phys as usize - 0x1FC0_0000) / 4;
                self.bios_cover[j] = self.bios_cover[j].saturating_add(1);
                let addr = 0xA000_0000 + phys;
                if std::mem::replace(&mut self.bios_owner[j], owner) != owner {
                    self.bodies[owner as usize].insert((NO_COPY, addr));
                }
                Some((NO_COPY, addr))
            }
            _ => None,
        };
        // A word a jump entered, with whether the owner's frame was torn
        // down then (`sp` back to its value at the owner's first word).
        if let (Some(_), false, Some(w)) = (jump, returned, word) {
            if jump != Some(JR_RA) && owner != OWNER_TOP {
                let how = if sp_before == owner_sp { 1 } else { 2 };
                *self.jumps[owner as usize].entry(w).or_default() |= how;
            }
        }
        let i = info.instruction;
        if info.exception.is_none() && is_rfe(i) {
            self.leaving_exception = true;
        }
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

    /// The id of `o` in `owners`, added when new.
    fn owner_id(&mut self, o: Owner) -> u32 {
        if let Some(&id) = self.owner_ids.get(&o) {
            return id;
        }
        let id = self.owners.len() as u32;
        self.owners.push(o);
        self.owner_ids.insert(o, id);
        self.bodies.push(BTreeSet::new());
        self.jumps.push(BTreeMap::new());
        id
    }

    /// The owner of what runs now and `sp` at its first word: the top frame
    /// whose first word ran.
    fn top(&self) -> (u32, u32) {
        self.returns.iter().rev().find_map(|f| Some((f.owner?, f.sp))).unwrap_or((OWNER_TOP, 0))
    }

    fn push(&mut self, f: Frame) {
        if self.returns.len() == MAX_RETURNS {
            self.returns.remove(0);
            self.stack.frames_dropped += 1;
        }
        self.returns.push(f);
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
            Some((c, ..)) if writes == seen => return Some(*c),
            Some((c, b, ..)) if b[..] == bus.ram[p * CODE_PAGE..(p + 1) * CODE_PAGE] => return Some(*c),
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
            Some((c, b, ..)) => CopyData::Changed(
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
        // The calls not yet returned from run on into the new moment.
        for f in &self.returns {
            if let Some(Owner::Call(copy, addr)) = f.owner.map(|o| self.owners[o as usize]) {
                if let Some(call) = self.calls.get_mut(&(copy, addr)) {
                    at(&mut call.moments, c);
                }
            }
        }
        let fresh = (c, now.into(), vec![Ran::default(); CODE_PAGE / 4].into(), vec![u32::MAX; CODE_PAGE / 4].into());
        if let Some((old, _, w, _)) = self.page_copy[p].replace(fresh) {
            let k0 = (p * CODE_PAGE / 4) as u32;
            for (j, r) in w.iter().enumerate().filter(|(_, r)| r.count > 0) {
                self.counts.entry((old, k0 + j as u32)).or_default().add(*r);
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

    /// The moment now: the number of the latest copy taken (RAM's code is
    /// each page's latest copy).
    fn moment(&self) -> u32 {
        self.code_pages.len().saturating_sub(1) as u32
    }

    /// Each owner with the words run inside it, as `(copy, first, end)`
    /// ranges of consecutive words in one copy, and the words a jump
    /// entered as `(copy, address, how)` ([`Tracer::jumps`] bits).
    #[allow(clippy::type_complexity)]
    pub fn bodies(&self) -> Vec<(Owner, Vec<(u32, u32, u32)>, Vec<(u32, u32, u8)>)> {
        let mut out = Vec::new();
        for (id, &o) in self.owners.iter().enumerate() {
            let mut ranges: Vec<(u32, u32, u32)> = Vec::new();
            for &(c, a) in &self.bodies[id] {
                match ranges.last_mut() {
                    Some(r) if r.0 == c && r.2 == a => r.2 = a + 4,
                    _ => ranges.push((c, a, a + 4)),
                }
            }
            let jumps = self.jumps[id].iter().map(|(&(c, a), &h)| (c, a, h)).collect();
            out.push((o, ranges, jumps));
        }
        out
    }

    /// What the run says of landing owner `id` ([`Owner::Landed`]).
    pub fn landing(&self, id: u32) -> Option<Landing> {
        let Owner::Landed(mut c, a) = *self.owners.get(id as usize)? else { return None };
        let landed_sp = self.landed_sp.get(&id).cloned().unwrap_or_default();
        loop {
            if let Some((returned_inside, returned_sp)) = self.returned_to.get(&(c, a)).cloned() {
                return Some(Landing { returned_copy: Some(c), returned_inside, returned_sp, landed_sp });
            }
            match self.code_pages.get(c as usize).map(|p| &p.data) {
                Some(CopyData::Changed(before, _)) => c = *before,
                _ => return Some(Landing { landed_sp, ..Default::default() }),
            }
        }
    }

    /// Executed RAM words as `(copy, KSEG0 address, instruction word, how
    /// it ran)`, in copy then address order, then the words no copy holds
    /// (`NO_COPY`).
    pub fn executed(&self) -> Vec<(u32, u32, u32, Ran)> {
        let mut all = self.counts.clone();
        for (p, (c, _, w, _)) in self.page_copy.iter().enumerate().filter_map(|(p, e)| Some((p, e.as_ref()?))) {
            for (j, r) in w.iter().enumerate().filter(|(_, r)| r.count > 0) {
                all.entry((*c, (p * CODE_PAGE / 4 + j) as u32)).or_default().add(*r);
            }
        }
        let word = |c: u32, k: u32| self.copy_word(c, 4 * k as usize % CODE_PAGE);
        let mut out: Vec<_> = all.into_iter().map(|((c, k), r)| (c, 0x8000_0000 + 4 * k, word(c, k), r)).collect();
        out.extend(self.uncopied.iter().map(|(&(k, i), r)| (NO_COPY, 0x8000_0000 + 4 * k, i, *r)));
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
        assert_eq!(t.calls.values().map(|c| c.first_moment).collect::<Vec<_>>(), [1, 2]);
        // The first call never returns: it runs on into the moment after.
        assert_eq!(t.calls.values().map(|c| c.moments.clone()).collect::<Vec<_>>(), [vec![(1, 2)], vec![(2, 2)]]);
        let r = |count, first, continues| Ran { count, first, continues };
        assert_eq!(
            t.executed(),
            [
                (0, 0x8002_0000, JAL, r(2, 0, false)),
                (0, 0x8002_0004, 0, r(2, 0, true)),
                (1, 0x8001_0000, JR_RA, r(2, 1, false)),
                (2, 0x8001_0000, JR_RA, r(1, 2, false))
            ]
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
        // Each with the moment it first ran (the latest copy then).
        let r = |count, first| Ran { count, first, continues: false };
        assert_eq!(
            t.executed(),
            [(0, 0x8001_0000, 7, r(2, 0)), (2, 0x8001_0000, 9, r(1, 2)), (NO_COPY, 0x8001_0000, 5, r(1, 2))]
        );
    }

    #[test]
    fn a_word_that_continues_the_code_before_it_is_marked() {
        let mut bus = Bus::new(vec![]);
        bus.page_writes = vec![0; 0x20_0000 / CODE_PAGE];
        let cpu = Cpu::new();
        let mut t = Tracer::new(TraceConfig::default(), Box::new(std::io::sink()), None);
        const JAL_2: u32 = 0x0C00_8000; // jal 80020000
        const J_14: u32 = 0x0800_4005; // j 80010014
        const BNE: u32 = 0x1420_0003; // bne at,zero,+3
        const B_4C: u32 = 0x1000_0002; // b 8001004C
        // A call and its return, a jump, a `jr ra` to where no call
        // returns, a branch not taken and one taken.
        let code = [
            (0x8001_0000, JAL_2, false),
            (0x8001_0004, 0, true),
            (0x8002_0000, JR_RA, false),
            (0x8002_0004, 0, true),
            (0x8001_0008, J_14, true),
            (0x8001_000C, 0, true),
            (0x8001_0014, JR_RA, false),
            (0x8001_0018, 0, true),
            (0x8001_0030, 0, false),
            (0x8001_0034, BNE, true),
            (0x8001_0038, 0, true),
            (0x8001_003C, 0, true),
            (0x8001_0040, B_4C, true),
            (0x8001_0044, 0, true),
            (0x8001_004C, 0, false),
        ];
        for (pc, i, _) in code {
            put(&mut bus, pc, i);
        }
        for (pc, i, _) in code {
            ran(&mut t, &cpu, &bus, pc, i);
        }
        let got: BTreeMap<u32, bool> = t.executed().iter().map(|e| (e.1, e.3.continues)).collect();
        assert_eq!(got, code.iter().map(|&(pc, _, c)| (pc, c)).collect());
    }

    #[test]
    fn a_call_runs_in_each_moment_until_it_returns() {
        let mut bus = Bus::new(vec![]);
        bus.page_writes = vec![0; 0x20_0000 / CODE_PAGE];
        let cpu = Cpu::new();
        let mut t = Tracer::new(TraceConfig::default(), Box::new(std::io::sink()), None);
        // 80010000 calls 80020000, which calls 80030000 and returns; while
        // the first call runs, code runs in two new pages (moments 3, 4).
        const JAL_2: u32 = 0x0C00_8000; // jal 80020000
        const JAL_3: u32 = 0x0C00_C000; // jal 80030000
        let code = [
            (0x8001_0000, JAL_2),
            (0x8001_0004, 0),
            (0x8002_0000, JAL_3),
            (0x8002_0004, 0),
            (0x8003_0000, JR_RA),
            (0x8003_0004, 0),
            (0x8002_0008, 0),
            (0x8004_0000, 0),
            (0x8005_0000, 0),
            (0x8002_000C, JR_RA),
            (0x8002_0010, 0),
            (0x8001_0008, 0),
            (0x8006_0000, 0),
        ];
        for (pc, i) in code {
            put(&mut bus, pc, i);
        }
        for (pc, i) in code {
            ran(&mut t, &cpu, &bus, pc, i);
        }
        let got: Vec<(u32, Vec<(u32, u32)>)> = t.calls.iter().map(|(k, c)| (k.1, c.moments.clone())).collect();
        assert_eq!(got, [(0x8002_0000, vec![(1, 4)]), (0x8003_0000, vec![(2, 2)])]);
    }

    /// The addresses each owner ran, by owner address (copies aside).
    fn owned(t: &Tracer) -> BTreeMap<(&'static str, u32), Vec<u32>> {
        let mut out = BTreeMap::new();
        for (o, ranges, _) in t.bodies() {
            let key = match o {
                Owner::Top => ("top", 0),
                Owner::Landed(_, a) => ("landed", a),
                Owner::Call(_, a) => ("call", a),
                Owner::Exception(_, a) => ("exception", a),
            };
            let words: Vec<u32> = ranges.iter().flat_map(|r| (r.1..r.2).step_by(4)).collect();
            if !words.is_empty() {
                out.insert(key, words);
            }
        }
        out
    }

    #[test]
    fn each_word_is_owned_by_the_call_it_ran_inside() {
        let mut bus = Bus::new(vec![]);
        bus.page_writes = vec![0; 0x20_0000 / CODE_PAGE];
        let mut cpu = Cpu::new();
        let mut t = Tracer::new(TraceConfig::default(), Box::new(std::io::sink()), None);
        const JAL_2: u32 = 0x0C00_8000; // jal 80020000
        const JAL_3: u32 = 0x0C00_C000; // jal 80030000
        const JAL_5: u32 = 0x0C01_4000; // jal 80050000
        const J_4: u32 = 0x0801_0000; // j 80040000
        const PUSH: u32 = 0x27BD_FFF8; // addiu sp,sp,-8
        const POP: u32 = 0x27BD_0008; // addiu sp,sp,8
        const JR_K0: u32 = 0x0340_0008;
        // 80020000 calls 80030000, then tail-jumps to 80040000 with its
        // frame torn down; 80050000 jumps there with its frame up. The
        // return to 80010010 is interrupted; after the handler, a `jr ra`
        // goes where no call returns, and a call runs under it.
        let code = [
            (0x8001_0000, JAL_2, 0x100, None),
            (0x8001_0004, 0, 0x100, None),
            (0x8002_0000, PUSH, 0xF8, None),
            (0x8002_0004, JAL_3, 0xF8, None),
            (0x8002_0008, 0, 0xF8, None),
            (0x8003_0000, JR_RA, 0xF8, None),
            (0x8003_0004, 0, 0xF8, None),
            (0x8002_000C, POP, 0x100, None),
            (0x8002_0010, J_4, 0x100, None),
            (0x8002_0014, 0, 0x100, None),
            (0x8004_0000, JR_RA, 0x100, None),
            (0x8004_0004, 0, 0x100, None),
            (0x8001_0008, JAL_5, 0x100, None),
            (0x8001_000C, 0, 0x100, None),
            (0x8005_0000, PUSH, 0xF8, None),
            (0x8005_0004, J_4, 0xF8, None),
            (0x8005_0008, 0, 0xF8, None),
            (0x8004_0000, JR_RA, 0xF8, None),
            (0x8004_0004, 0, 0xF8, None),
            (0x8001_0010, 0, 0xF8, Some(0)),
            (0x8000_0080, 0, 0xF8, None),
            (0x8000_0084, JR_K0, 0xF8, None),
            (0x8000_0088, 0x4200_0010, 0xF8, None), // rfe
            (0x8001_0010, 0, 0xF8, None),
            (0x8001_0014, JR_RA, 0xF8, None),
            (0x8001_0018, 0, 0xF8, None),
            (0x8006_0000, 0, 0xF8, None),
            (0x8006_0004, JAL_3, 0xF8, None),
            (0x8006_0008, 0, 0xF8, None),
            (0x8003_0000, JR_RA, 0xF8, None),
            (0x8003_0004, 0, 0xF8, None),
            (0x8006_000C, 0, 0xF8, None),
        ];
        for (pc, i, _, _) in code {
            put(&mut bus, pc, i);
        }
        cpu.cop0[crate::cpu::cop0::EPC] = 0x8001_0010;
        for (pc, i, sp, exception) in code {
            cpu.regs[29] = sp;
            let info = StepInfo { pc, instruction: i, exception, io_load: None, mem: None };
            t.after_step(&info, &cpu, &bus);
        }
        let w = |r: std::ops::Range<u32>| r.step_by(4).collect::<Vec<u32>>();
        let tail = w(0x8004_0000..0x8004_0008);
        assert_eq!(
            owned(&t),
            BTreeMap::from([
                (("top", 0), w(0x8001_0000..0x8001_001C)),
                (("landed", 0x8006_0000), w(0x8006_0000..0x8006_0010)),
                (("call", 0x8002_0000), [w(0x8002_0000..0x8002_0018), tail.clone()].concat()),
                (("call", 0x8003_0000), w(0x8003_0000..0x8003_0008)),
                (("call", 0x8005_0000), [tail, w(0x8005_0000..0x8005_000C)].concat()),
                (("exception", 0x8000_0080), w(0x8000_0080..0x8000_008C)),
            ])
        );
        // The tail entered by a jump: from 80020000 with its frame torn
        // down, from 80050000 with it up.
        let jumps: Vec<(Owner, Vec<(u32, u8)>)> = t
            .bodies()
            .into_iter()
            .filter(|b| !b.2.is_empty())
            .map(|(o, _, j)| (o, j.iter().map(|j| (j.1, j.2)).collect()))
            .collect();
        let call = |a| t.owners.iter().copied().find(|o| matches!(*o, Owner::Call(_, x) if x == a)).unwrap();
        assert_eq!(jumps, [(call(0x8002_0000), vec![(0x8004_0000, 1)]), (call(0x8005_0000), vec![(0x8004_0000, 2)])]);
        assert_eq!(t.stack, StackStats { returns_not_held: 1, ..Default::default() });
        // The return the interrupt cut off continues the code before it.
        assert!(t.executed().iter().any(|e| e.1 == 0x8001_0010 && e.3.continues));
    }

    #[test]
    fn a_stack_deeper_than_kept_loses_its_oldest_frames() {
        let mut bus = Bus::new(vec![]);
        bus.page_writes = vec![0; 0x20_0000 / CODE_PAGE];
        let cpu = Cpu::new();
        let mut t = Tracer::new(TraceConfig::default(), Box::new(std::io::sink()), None);
        const JAL_2: u32 = 0x0C00_8000; // jal 80020000
        put(&mut bus, 0x8001_0000, JAL_2);
        put(&mut bus, 0x8002_0000, JAL_2);
        put(&mut bus, 0x8002_0008, JR_RA);
        // 80020000 calls itself: with the first call, one deeper than the
        // stack keeps.
        ran(&mut t, &cpu, &bus, 0x8001_0000, JAL_2);
        ran(&mut t, &cpu, &bus, 0x8001_0004, 0);
        for _ in 0..MAX_RETURNS {
            ran(&mut t, &cpu, &bus, 0x8002_0000, JAL_2);
            ran(&mut t, &cpu, &bus, 0x8002_0004, 0);
        }
        for _ in 0..MAX_RETURNS {
            ran(&mut t, &cpu, &bus, 0x8002_0008, JR_RA);
            ran(&mut t, &cpu, &bus, 0x8002_000C, 0);
        }
        ran(&mut t, &cpu, &bus, 0x8001_0008, 0);
        let s = StackStats { returns_not_held: 1, frames_dropped: 1, ..Default::default() };
        assert_eq!(t.stack, s);
        assert_eq!(owned(&t)[&("landed", 0x8001_0008)], [0x8001_0008]);
    }

    #[test]
    fn a_landing_keeps_what_the_run_says_of_where_it_landed() {
        let mut bus = Bus::new(vec![]);
        bus.page_writes = vec![0; 0x20_0000 / CODE_PAGE];
        let mut cpu = Cpu::new();
        let mut t = Tracer::new(TraceConfig::default(), Box::new(std::io::sink()), None);
        const JAL_2: u32 = 0x0C00_8000; // jal 80020000
        const JAL_3: u32 = 0x0C00_C000; // jal 80030000
        const JAL_4: u32 = 0x0C01_0000; // jal 80040000
        const PUSH: u32 = 0x27BD_FFF8; // addiu sp,sp,-8
        const POP: u32 = 0x27BD_0008; // addiu sp,sp,8
        // 80020000 calls 80030000 (as `setjmp`), which returns to
        // 8002000C. Later 80040000 (as `longjmp`) puts back `sp` and
        // `jr ra`s there, a return the stack does not hold.
        let code = [
            (0x8001_0000, JAL_2, 0x100),
            (0x8001_0004, 0, 0x100),
            (0x8002_0000, PUSH, 0xF8),
            (0x8002_0004, JAL_3, 0xF8),
            (0x8002_0008, 0, 0xF8),
            (0x8003_0000, JR_RA, 0xF8),
            (0x8003_0004, 0, 0xF8),
            (0x8002_000C, 0, 0xF8),
            (0x8002_0010, POP, 0x100),
            (0x8002_0014, JR_RA, 0x100),
            (0x8002_0018, 0, 0x100),
            (0x8001_0008, 0, 0x100),
            (0x8001_000C, JAL_4, 0x100),
            (0x8001_0010, 0, 0x100),
            (0x8004_0000, JR_RA, 0xF8),
            (0x8004_0004, 0, 0xF8),
            (0x8002_000C, 0, 0xF8),
            (0x8002_0010, POP, 0x100),
        ];
        for (pc, i, _) in code {
            put(&mut bus, pc, i);
        }
        for (pc, i, sp) in code {
            cpu.regs[29] = sp;
            ran(&mut t, &cpu, &bus, pc, i);
        }
        // The landing owns what ran after it; the call's owner keeps what
        // ran before.
        let o = owned(&t);
        assert_eq!(o[&("landed", 0x8002_000C)], [0x8002_000C, 0x8002_0010]);
        assert_eq!(o[&("call", 0x8002_0000)], (0x8002_0000..0x8002_001C).step_by(4).collect::<Vec<_>>());
        let landed = t.owners.iter().position(|o| matches!(o, Owner::Landed(..))).unwrap() as u32;
        let x = t.owners.iter().position(|o| matches!(*o, Owner::Call(_, 0x8002_0000))).unwrap() as u32;
        let seen = |v: Vec<u32>| Seen { values: v, more: false };
        assert_eq!(
            t.landing(landed),
            Some(Landing {
                returned_copy: Some(1),
                returned_inside: seen(vec![x]),
                returned_sp: seen(vec![0xF8]),
                landed_sp: seen(vec![0xF8])
            })
        );
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
