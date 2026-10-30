//! Capture's read audit and branch coverage (docs/mechanics.md, "Capturing
//! on the PlayStation"): what the step reads that the binding does not
//! account for, and which of its branches the recording took each way.
//!
//! Only the step's own instructions count: not a world call's (its reads
//! are the world's), and not an exception handler's (from the exception to
//! its `rfe`). A load counts once the step uses what it loaded: reads the
//! register, stores it, or passes it to the world. An epilogue restoring the
//! caller's registers reads nothing.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use psx_core::Machine;
use psx_core::cpu::StepInfo;
use psx_gte::op;
use psx_mechanics::trace::{Audit, Coverage, Finding};
use serde_json::{Value as Json, json};

use super::capture::Regs;
use super::{Location, Reg, SCRATCHPAD};
use crate::run::fold;

/// Findings and missed branches listed in the result; the header has all.
const LISTED: usize = 30;
/// Instructions the static walk of the step's code visits at most.
const WALK_LIMIT: usize = 1_000_000;
/// `rfe`: an exception handler returns.
const RFE: u32 = 0x4200_0010;

/// zero, gp, sp and ra: the ABI's, never a hidden input.
const ABI: u64 = 1 | 1 << 28 | 1 << 29 | 1 << 31;

/// Where a value the step reads came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
enum From {
    /// From before the step: a hidden input unless the binding accounts for it.
    Before,
    /// The step wrote it.
    Step,
    /// A world call (the plan's call index) changed it, outside its response.
    Call(usize),
}

/// What the binding accounts for, besides the reads capture takes itself.
pub struct Accounts<'a> {
    /// Memory parts, read with the step's latched registers: the state,
    /// inputs, params, explained reads, and the calls' requests and responses.
    pub memory: Vec<(&'a Location, u32)>,
    /// PCs whose loads are explained wherever they read.
    pub readers: HashSet<u32>,
    /// Registers whose value from before the step is accounted for: bases,
    /// filters, register inputs and params, explained registers.
    pub registers: u64,
    /// GTE registers likewise (0-31 data, 32-63 control).
    pub gte: u64,
    /// The binding's base registers and sp, for describing findings.
    pub bases: Vec<Reg>,
    /// Each call's `<interface>.<call>`, by the plan's call index.
    pub calls: Vec<String>,
    /// Interface entry PCs: the world's code, not the step's.
    pub entries: HashSet<u32>,
    /// The end PC, where the step's code stops.
    pub end: Option<u32>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
enum Key {
    Mem { pc: u32, from: From },
    Reg { pc: u32, reg: u8, from: From },
    Gte { pc: u32, reg: u8, from: From, by: &'static str },
}

impl Key {
    fn pc(&self) -> u32 {
        match *self {
            Key::Mem { pc, .. } | Key::Reg { pc, .. } | Key::Gte { pc, .. } => pc,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Same<T> {
    Unset,
    One(T),
    Varies,
}

impl<T: PartialEq + Copy> Same<T> {
    fn add(&mut self, v: T) {
        *self = match *self {
            Same::Unset => Same::One(v),
            Same::One(a) if a == v => Same::One(a),
            _ => Same::Varies,
        }
    }

    fn merge(&mut self, o: Same<T>) {
        match o {
            Same::Unset => {}
            Same::One(v) => self.add(v),
            Same::Varies => *self = Same::Varies,
        }
    }

    fn one(&self) -> Option<T> {
        if let Same::One(v) = self { Some(*v) } else { None }
    }
}

/// One finding's reads, over a step or the recording.
#[derive(Clone, Debug)]
struct Seen {
    steps: u64,
    /// Memory: the unaccounted bytes' addresses, lowest and highest.
    lo: u32,
    hi: u32,
    addr: Same<u32>,
    bytes: Same<u32>,
    /// The address less each base register as latched.
    rel: Vec<Same<i64>>,
    region: &'static str,
}

impl Seen {
    fn new(bases: usize) -> Seen {
        Seen {
            steps: 0,
            lo: u32::MAX,
            hi: 0,
            addr: Same::Unset,
            bytes: Same::Unset,
            rel: vec![Same::Unset; bases],
            region: "",
        }
    }

    fn merge(&mut self, o: &Seen) {
        self.steps += 1;
        self.lo = self.lo.min(o.lo);
        self.hi = self.hi.max(o.hi);
        self.addr.merge(o.addr);
        self.bytes.merge(o.bytes);
        for (a, b) in self.rel.iter_mut().zip(&o.rel) {
            a.merge(*b);
        }
        self.region = o.region;
    }
}

/// An unaccounted load whose value the step has not used yet.
#[derive(Clone, Debug)]
struct Pend {
    key: Key,
    addr: u32,
    bytes: u32,
    rel: Vec<i64>,
    region: &'static str,
}

/// A call under way, and what its instructions wrote.
struct Entered {
    call: usize,
    sp: u32,
    regs: u64,
    gte: u64,
    /// Stack words holding a register the call saved before writing it.
    saved: HashMap<u32, u8>,
}

/// The read audit and branch coverage of one recording.
pub struct Tracker<'a> {
    acc: &'a Accounts<'a>,
    // The step under way.
    latched: Regs,
    regs: [From; 34],
    gte: [From; 64],
    accounted: Vec<(u32, u32)>,
    written: HashSet<u32>,
    by_call: HashMap<u32, usize>,
    /// Stack words holding a register saved before the step wrote it, or
    /// before it used what it loaded.
    saved: HashMap<u32, (u8, From, Vec<Pend>)>,
    /// Registers holding an unaccounted load not yet used.
    taint: HashMap<u8, Vec<Pend>>,
    entered: Option<Entered>,
    depth: u32,
    step: HashMap<Key, Seen>,
    // The recording.
    found: BTreeMap<Key, Seen>,
    steps: u64,
    branch: Option<(u32, u32)>,
    outcomes: HashSet<(u32, bool)>,
    roots: BTreeSet<u32>,
    code: Option<Vec<u8>>,
    begin: u32,
}

/// Main RAM through any of its mirrors, or the scratchpad.
fn memory(addr: u32) -> bool {
    let p = fold(addr);
    p < 0x20_0000 || SCRATCHPAD.contains(&p)
}

fn bit(n: impl Into<u32>) -> u64 {
    1 << n.into()
}

impl<'a> Tracker<'a> {
    pub fn new(acc: &'a Accounts<'a>, begin: u32) -> Tracker<'a> {
        Tracker {
            acc,
            latched: [0; 34],
            regs: [From::Before; 34],
            gte: [From::Before; 64],
            accounted: Vec::new(),
            written: HashSet::new(),
            by_call: HashMap::new(),
            saved: HashMap::new(),
            taint: HashMap::new(),
            entered: None,
            depth: 0,
            step: HashMap::new(),
            found: BTreeMap::new(),
            steps: 0,
            branch: None,
            outcomes: HashSet::new(),
            roots: BTreeSet::new(),
            code: None,
            begin,
        }
    }

    /// A step begins, with the registers as latched there.
    pub fn open(&mut self, m: &Machine, latched: &Regs) {
        self.latched = *latched;
        self.regs = [From::Before; 34];
        self.gte = [From::Before; 64];
        self.written.clear();
        self.by_call.clear();
        self.saved.clear();
        self.taint.clear();
        self.entered = None;
        self.depth = 0;
        self.step.clear();
        self.branch = None;
        self.accounted = self
            .acc
            .memory
            .iter()
            .filter_map(|(at, n)| {
                let a = match at {
                    Location::Address { address } => *address,
                    Location::Base { base, offset } => latched[*base as usize].wrapping_add(*offset as u32),
                    _ => return None,
                };
                let p = fold(a);
                Some((p, p + n))
            })
            .collect();
        if self.code.is_none() {
            self.code = Some(m.bus.ram.to_vec());
        }
    }

    /// The step ends: its findings count.
    pub fn close(&mut self) {
        for (k, s) in self.step.drain() {
            self.found.entry(k).or_insert_with(|| Seen::new(s.rel.len())).merge(&s);
        }
        self.steps += 1;
    }

    fn note(&mut self, key: Key) -> &mut Seen {
        let n = self.acc.bases.len();
        self.step.entry(key).or_insert_with(|| Seen::new(n))
    }

    fn register(&mut self, pc: u32, r: u8) {
        match self.regs[r as usize] {
            _ if bit(r) & ABI != 0 => {}
            From::Step => {}
            From::Before if bit(r) & self.acc.registers != 0 => {}
            from => {
                self.note(Key::Reg { pc, reg: r, from });
            }
        }
    }

    fn gte_reads(&mut self, pc: u32, bad: Vec<(u8, From)>, by: &'static str) {
        for (reg, from) in bad {
            if from == From::Before && bit(reg) & self.acc.gte != 0 {
                continue;
            }
            self.note(Key::Gte { pc, reg, from, by });
        }
    }

    /// A load of `width` bytes at `addr` by the instruction at `pc`, used.
    fn load(&mut self, pc: u32, addr: u32, width: u32) {
        for p in self.unaccounted(pc, addr, width) {
            self.commit(p);
        }
    }

    /// What a load reads that the binding does not account for.
    fn unaccounted(&self, pc: u32, addr: u32, width: u32) -> Vec<Pend> {
        if self.acc.readers.contains(&pc) {
            return Vec::new();
        }
        let p = fold(addr);
        // Main RAM through any of its mirrors, as the address it mirrors.
        let addr = if p < 0x20_0000 { 0x8000_0000 | p } else { addr };
        let region = if memory(addr) {
            ""
        } else if (0x1F80_1000..0x1F80_3000).contains(&p) {
            "a hardware register"
        } else if (0x1FC0_0000..0x1FC8_0000).contains(&p) {
            "the BIOS ROM"
        } else {
            "outside memory"
        };
        let mut bytes: Vec<(u32, From)> = Vec::new();
        if region.is_empty() {
            for k in 0..width {
                let b = fold(addr.wrapping_add(k));
                if self.written.contains(&b) || self.accounted.iter().any(|&(s, e)| (s..e).contains(&b)) {
                    continue;
                }
                bytes.push((k, self.by_call.get(&b).map_or(From::Before, |&c| From::Call(c))));
            }
        } else {
            // Hardware gives a new value each read: only `explained` covers it.
            for k in 0..width {
                let b = fold(addr.wrapping_add(k));
                if !self.accounted.iter().any(|&(s, e)| (s..e).contains(&b)) {
                    bytes.push((k, From::Before));
                }
            }
        }
        let froms: BTreeSet<From> = bytes.iter().map(|b| b.1).collect();
        froms
            .into_iter()
            .map(|from| {
                let ks: Vec<u32> = bytes.iter().filter(|b| b.1 == from).map(|b| b.0).collect();
                let (k0, k1) = (ks[0], ks[ks.len() - 1]);
                let a = addr.wrapping_add(k0);
                let rel =
                    self.acc.bases.iter().map(|b| a.wrapping_sub(self.latched[*b as usize]) as i32 as i64).collect();
                Pend { key: Key::Mem { pc, from }, addr: a, bytes: k1 - k0 + 1, rel, region }
            })
            .collect()
    }

    fn commit(&mut self, p: Pend) {
        let s = self.note(p.key);
        s.steps = 1;
        s.lo = s.lo.min(p.addr);
        s.hi = s.hi.max(p.addr.wrapping_add(p.bytes - 1));
        s.addr.add(p.addr);
        s.bytes.add(p.bytes);
        for (x, r) in s.rel.iter_mut().zip(p.rel) {
            x.add(r);
        }
        s.region = p.region;
    }

    /// A bound location read at `pc` (a call's request at entry).
    fn location(&mut self, pc: u32, at: &Location, n: u32) {
        match at {
            Location::Address { address } => self.load(pc, *address, n),
            Location::Base { base, offset } => {
                self.load(pc, self.latched[*base as usize].wrapping_add(*offset as u32), n)
            }
            Location::Register { register } => {
                let r = *register as u8;
                for p in self.taint.remove(&r).unwrap_or_default() {
                    self.commit(p);
                }
                self.register(pc, r);
            }
            Location::Gte { gte } => {
                let from = self.gte[*gte as usize];
                if from != From::Step {
                    self.gte_reads(pc, vec![(*gte, from)], "the call's request");
                }
            }
            Location::Load { .. } | Location::Pointer { .. } => {}
        }
    }

    /// Call `call` is entered at `pc` with stack pointer `sp`: its request's
    /// parts are what the step passes it.
    pub fn entry(&mut self, pc: u32, call: usize, request: &[(&Location, u32)], sp: u32) {
        for (at, n) in request {
            self.location(pc, at, *n);
        }
        self.entered = Some(Entered { call, sp, regs: 0, gte: 0, saved: HashMap::new() });
    }

    /// The call returns: the registers it wrote, outside its response, are its.
    /// `held`: the physical bytes a used mechanic's response holds, which
    /// the step has from then on.
    pub fn returned(&mut self, response: &[(&Location, u32)], held: &[(u32, u32)]) {
        for &(s, e) in held {
            for b in s..e {
                self.by_call.remove(&b);
                self.written.insert(b);
            }
        }
        let Some(e) = self.entered.take() else { return };
        for r in 0..34 {
            if e.regs & bit(r as u32) != 0 {
                self.regs[r] = From::Call(e.call);
                self.taint.remove(&(r as u8));
            }
        }
        for g in 0..64 {
            if e.gte & bit(g as u32) != 0 {
                self.gte[g] = From::Call(e.call);
            }
        }
        for (at, _) in response {
            match at {
                Location::Register { register } => self.regs[*register as usize] = From::Step,
                Location::Gte { gte } => self.gte[*gte as usize] = From::Step,
                _ => {}
            }
        }
    }

    /// After every instruction while a step is open. `call`: the world call
    /// it belongs to. `inline`: an inline read's load, which the binding
    /// accounts for.
    pub fn observe(&mut self, m: &Machine, info: &StepInfo, call: Option<usize>, inline: bool) {
        if info.exception.is_some() {
            self.depth += 1;
            self.branch = None;
            return;
        }
        if self.depth > 0 {
            if info.instruction == RFE {
                self.depth -= 1;
            }
            return;
        }
        let (pc, i) = (info.pc, info.instruction);
        let (mut reads, writes) = cpu_use(i);
        let rt = ((i >> 16) & 31) as u8;
        if let Some(c) = call {
            self.world(m, info, c, writes, rt);
            return;
        }
        let op = i >> 26;
        let store = info.mem.filter(|a| a.store);
        // A register saved to the step's own stack frame is not used.
        if let Some(a) = store {
            let sp = m.cpu.regs[29];
            let r = rt as usize;
            if (sp..self.latched[29]).contains(&a.addr) && (self.regs[r] != From::Step || self.taint.contains_key(&rt))
            {
                reads &= !bit(rt);
                let pends = self.taint.get(&rt).cloned().unwrap_or_default();
                self.saved.insert(fold(a.addr), (rt, self.regs[r], pends));
            }
        }
        // lwl/lwr merge into rt: that is not a use of what it held.
        let merging = matches!(op, 0x22 | 0x26);
        for r in 0..34u8 {
            if reads & bit(r) != 0 {
                if !(merging && r == rt) {
                    for p in self.taint.remove(&r).unwrap_or_default() {
                        self.commit(p);
                    }
                }
                self.register(pc, r);
            }
        }
        let mut loaded = Vec::new();
        match info.mem {
            Some(a) if a.store => {
                if memory(a.addr) {
                    for k in 0..a.width as u32 {
                        self.written.insert(fold(a.addr.wrapping_add(k)));
                    }
                }
            }
            Some(a) if !inline => loaded = self.unaccounted(pc, a.addr, a.width as u32),
            _ => {}
        }
        let held = if merging { self.taint.remove(&rt) } else { None };
        for r in 1..34u8 {
            if writes & bit(r) != 0 {
                self.regs[r as usize] = From::Step;
                self.taint.remove(&r);
            }
        }
        if op == 0x32 {
            // lwc2: the GTE uses what it loads.
            loaded.drain(..).for_each(|p| self.commit(p));
        }
        if writes & bit(rt) != 0 && info.mem.is_some_and(|a| !a.store) {
            let mut pends = held.unwrap_or_default();
            pends.extend(loaded);
            if !pends.is_empty() {
                self.taint.insert(rt, pends);
            }
        }
        // A saved register restored: it is as it was.
        if let Some(a) = info.mem.filter(|a| !a.store && op == 0x23) {
            if let Some((r, from, pends)) = self.saved.get(&fold(a.addr)) {
                if *r == rt {
                    self.regs[rt as usize] = *from;
                    if !pends.is_empty() {
                        self.taint.insert(rt, pends.clone());
                    }
                }
            }
        }
        if matches!(i >> 26, 0x12 | 0x32 | 0x3A) {
            let mut g = GteFlow { st: self.gte, bad: Vec::new() };
            let by = gte_flow(i, &mut g);
            self.gte = g.st;
            self.gte_reads(pc, g.bad, by);
        }
        self.branches(pc, i, m.cpu.pc);
    }

    /// An instruction of call `c`: what it writes is the call's, except a
    /// register it restores from where it saved it in its own stack frame.
    fn world(&mut self, m: &Machine, info: &StepInfo, c: usize, writes: u64, rt: u8) {
        if let Some(a) = info.mem.filter(|a| a.store) {
            for k in 0..a.width as u32 {
                self.by_call.insert(fold(a.addr.wrapping_add(k)), c);
            }
        }
        let Some(e) = self.entered.as_mut() else { return };
        if let Some(a) = info.mem.filter(|a| a.store) {
            if (m.cpu.regs[29]..e.sp).contains(&a.addr) && e.regs & bit(rt) == 0 {
                e.saved.insert(fold(a.addr), rt);
            }
        }
        e.regs |= writes;
        if let Some(a) = info.mem.filter(|a| !a.store && info.instruction >> 26 == 0x23) {
            if e.saved.get(&fold(a.addr)) == Some(&rt) {
                e.regs &= !bit(rt);
            }
        }
        if matches!(info.instruction >> 26, 0x12 | 0x32 | 0x3A) {
            let mut g = GteFlow { st: [From::Before; 64], bad: Vec::new() };
            gte_flow(info.instruction, &mut g);
            for (n, f) in g.st.iter().enumerate() {
                if *f == From::Step {
                    e.gte |= bit(n as u32);
                }
            }
        }
    }

    fn branches(&mut self, pc: u32, i: u32, next: u32) {
        if let Some((b, bi)) = self.branch.take() {
            if pc == b.wrapping_add(4) {
                match flow(b, bi) {
                    Flow::Branch(t) => {
                        self.outcomes.insert((b, next == t));
                        if t == b.wrapping_add(8) {
                            // Both ways go on to the same instruction.
                            self.outcomes.insert((b, false));
                        }
                    }
                    Flow::JumpReg | Flow::CallReg => {
                        self.roots.insert(next);
                    }
                    _ => {}
                }
            }
        }
        if matches!(flow(pc, i), Flow::Branch(_) | Flow::JumpReg | Flow::CallReg) {
            self.branch = Some((pc, i));
        }
    }

    /// The audit and coverage for the header, and the report for the result.
    pub fn finish(self) -> (Audit, Coverage, Json) {
        let mut unaccounted = Vec::new();
        let mut draft = Drafts::default();
        for (k, s) in &self.found {
            let (at, note) = self.describe(k, s, &mut draft);
            unaccounted.push(Finding { at, note: Some(note) });
        }
        let audit = Audit { unaccounted };

        // Coverage: the step's code, walked from its begin and from every
        // register jump it took, against the branches it took.
        let ram = self.code.as_deref().unwrap_or_default();
        let roots = std::iter::once(self.begin).chain(self.roots.iter().copied());
        let walk = walk(ram, roots, &self.acc.entries, self.acc.end);
        let mut branches = walk.branches.clone();
        for (b, _) in &self.outcomes {
            if !branches.contains_key(b) {
                if let Some(Flow::Branch(t)) = word(ram, *b).map(|i| flow(*b, i)) {
                    branches.insert(*b, t);
                }
            }
        }
        let mut missed = Vec::new();
        let mut reached = 0;
        for b in branches.keys() {
            let (t, n) = (self.outcomes.contains(&(*b, true)), self.outcomes.contains(&(*b, false)));
            reached += t as u64 + n as u64;
            let note = match (t, n) {
                (true, true) => continue,
                (false, false) => "never ran",
                (true, false) => "never fell through",
                (false, true) => "never taken",
            };
            let mut at = serde_json::Map::new();
            at.insert("pc".into(), json!(hex(*b)));
            missed.push(Finding { at, note: Some(note.into()) });
        }
        let all = branches
            .keys()
            .map(|b| {
                let mut at = serde_json::Map::new();
                at.insert("pc".into(), json!(hex(*b)));
                at
            })
            .collect();
        let coverage = Coverage { reached, total: 2 * branches.len() as u64, missed, branches: all };

        let report = json!({
            "audit": {
                "clear": audit.unaccounted.is_empty(),
                "unaccounted": audit.unaccounted.len(),
                "listed": audit.unaccounted.iter().take(LISTED).collect::<Vec<_>>(),
                "draft": draft.toml(),
            },
            "coverage": {
                "reached": coverage.reached,
                "total": coverage.total,
                "missed": coverage.missed.len(),
                "listed": coverage.missed.iter().take(LISTED).collect::<Vec<_>>(),
                "instructions": walk.instructions,
                "walk_truncated": walk.truncated,
            },
        });
        (audit, coverage, report)
    }

    fn describe(&self, k: &Key, s: &Seen, draft: &mut Drafts) -> (serde_json::Map<String, Json>, String) {
        let mut at = serde_json::Map::new();
        at.insert("pc".into(), json!(hex(k.pc())));
        let steps = format!("{} of {} steps", s.steps, self.steps);
        let from = |f: &From, what: &str| match f {
            From::Call(c) => format!(", {what} by the call {} (not in its response)", self.acc.calls[*c]),
            _ => String::new(),
        };
        let note = match k {
            Key::Mem { pc, from: f } => {
                match s.addr.one() {
                    Some(a) => at.insert("address".into(), json!(hex(a))),
                    None => at.insert("addresses".into(), json!(format!("{}-{}", hex(s.lo), hex(s.hi)))),
                };
                // A base and offset when one holds and says more than the
                // address: a small offset, or an address that moves.
                let base = self.acc.bases.iter().zip(&s.rel).find_map(|(b, r)| {
                    r.one().map(|o| (*b, o)).filter(|(_, o)| (0..0x1_0000).contains(o) || s.addr.one().is_none())
                });
                if let Some((b, o)) = base {
                    at.insert("base".into(), json!(b.name()));
                    at.insert("offset".into(), json!(signed_hex(o)));
                }
                let bytes = match s.bytes.one() {
                    Some(1) => "1 byte".into(),
                    Some(n) => format!("{n} bytes"),
                    None => "bytes of varying width".into(),
                };
                let region = if s.region.is_empty() {
                    String::new()
                } else {
                    format!(", {}: bind it as an inline read if the result depends on it, else explain it", s.region)
                };
                if !s.region.is_empty() {
                    let (a, n) = s.addr.one().map_or((s.lo, s.hi - s.lo + 1), |a| (a, s.bytes.one().unwrap_or(4)));
                    draft.add(format!("{{ address = {} }}", hex(a)), n, *pc, HARDWARE);
                } else {
                    let width = s.bytes.one().unwrap_or(s.hi - s.lo + 1);
                    let loc = match (base, s.addr.one()) {
                        (Some((b, o)), _) => Some(format!("{{ base = \"{}\", offset = {} }}", b.name(), toml_int(o))),
                        (_, Some(a)) => Some(format!("{{ address = {} }}", hex(a))),
                        _ => None,
                    };
                    match loc {
                        Some(l) => draft.add(l, width, *pc, ""),
                        None => draft.add(
                            format!("{{ address = {} }}", hex(s.lo)),
                            s.hi - s.lo + 1,
                            *pc,
                            ": the range these steps read; a table may reach further",
                        ),
                    }
                }
                format!("{bytes}, {steps}{region}{}", from(f, "written"))
            }
            Key::Reg { pc, reg, from: f } => {
                let name = reg_name(*reg);
                at.insert("register".into(), json!(name));
                draft.add(format!("{{ register = \"{name}\" }}"), 4, *pc, "");
                match f {
                    From::Before => format!("read before the step wrote it, {steps}"),
                    _ => format!("{steps}{}", from(f, "changed")),
                }
            }
            Key::Gte { pc, reg, from: f, by } => {
                at.insert("gte".into(), json!(reg));
                draft.add(format!("{{ gte = {reg} }}"), 4, *pc, "");
                let what = format!("{} ({by})", GTE_NAMES[*reg as usize]);
                match f {
                    From::Before => format!("{what}, read before the step wrote it, {steps}"),
                    _ => format!("{what}, {steps}{}", from(f, "changed")),
                }
            }
        };
        (at, note)
    }
}

fn reg_name(r: u8) -> String {
    match r {
        32 => "hi".into(),
        33 => "lo".into(),
        r => psx_core::disasm::reg(r as u32).to_string(),
    }
}

fn hex(v: u32) -> String {
    format!("{v:#010x}")
}

fn signed_hex(v: i64) -> String {
    if v < 0 { format!("-{:#x}", -v) } else { format!("{v:#x}") }
}

/// TOML has no signed hex.
fn toml_int(v: i64) -> String {
    if v < 0 { v.to_string() } else { format!("{v:#x}") }
}

/// The draft's note for a read outside memory.
const HARDWARE: &str = "; or bind it as an inline read if the result depends on it";

/// A draft of `capture.explained`, one entry per place.
#[derive(Default)]
struct Drafts {
    /// Location -> bytes, reading PCs, and a note.
    entries: BTreeMap<String, (u32, BTreeSet<u32>, &'static str)>,
}

impl Drafts {
    fn add(&mut self, at: String, bytes: u32, pc: u32, note: &'static str) {
        let e = self.entries.entry(at).or_insert((bytes, BTreeSet::new(), note));
        e.0 = e.0.max(bytes);
        e.1.insert(pc);
    }

    fn toml(&self) -> String {
        if self.entries.is_empty() {
            return String::new();
        }
        let mut out = String::from("explained = [\n");
        for (at, (bytes, pcs, note)) in &self.entries {
            let pcs: Vec<String> = pcs.iter().map(|p| hex(*p)).collect();
            let bytes = if *bytes < 16 { bytes.to_string() } else { format!("{bytes:#x}") };
            out += &format!("  {{ at = {at}, bytes = {bytes}, why = \"\" }},  # read at {}{note}\n", pcs.join(", "));
        }
        out + "]\n"
    }
}

/// CPU registers an instruction reads and writes, as bits (r0-r31, hi 32,
/// lo 33).
fn cpu_use(i: u32) -> (u64, u64) {
    let (rs, rt, rd) = ((i >> 21) & 31, (i >> 16) & 31, (i >> 11) & 31);
    const HI: u64 = 1 << 32;
    const LO: u64 = 1 << 33;
    let b = |r: u32| 1u64 << r;
    let (r, w) = match i >> 26 {
        0x00 => match i & 0x3F {
            0x00 | 0x02 | 0x03 => (b(rt), b(rd)),
            0x04 | 0x06 | 0x07 => (b(rs) | b(rt), b(rd)),
            0x08 => (b(rs), 0),
            0x09 => (b(rs), b(rd)),
            0x10 => (HI, b(rd)),
            0x11 => (b(rs), HI),
            0x12 => (LO, b(rd)),
            0x13 => (b(rs), LO),
            0x18..=0x1B => (b(rs) | b(rt), HI | LO),
            0x20..=0x27 | 0x2A | 0x2B => (b(rs) | b(rt), b(rd)),
            _ => (0, 0),
        },
        0x01 => (b(rs), if rt & 0x1E == 0x10 { b(31) } else { 0 }),
        0x03 => (0, b(31)),
        0x04 | 0x05 => (b(rs) | b(rt), 0),
        0x06 | 0x07 => (b(rs), 0),
        0x0F => (0, b(rt)),
        0x08..=0x0E => (b(rs), b(rt)),
        // mfc0/cfc0, mfc2/cfc2 write rt; mtc0/ctc0, mtc2/ctc2 read it.
        0x10 | 0x12 => match rs {
            0x00 | 0x02 => (0, b(rt)),
            0x04 | 0x06 => (b(rt), 0),
            _ => (0, 0),
        },
        // lwl/lwr merge into rt.
        0x22 | 0x26 => (b(rs) | b(rt), b(rt)),
        0x20..=0x26 => (b(rs), b(rt)),
        0x28..=0x2E => (b(rs) | b(rt), 0),
        0x32 | 0x3A => (b(rs), 0),
        _ => (0, 0),
    };
    (r & !1, w & !1)
}

/// Where control goes after an instruction.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Flow {
    Next,
    /// A conditional branch to this target.
    Branch(u32),
    /// Always to this target (`b`, `j`).
    Jump(u32),
    /// Never taken: on to the next.
    Never,
    /// A call to this target (`jal`, `bal`).
    Call(u32),
    /// `jr ra`.
    Return,
    /// `jr` through another register.
    JumpReg,
    /// `jalr`.
    CallReg,
}

fn flow(pc: u32, i: u32) -> Flow {
    let (rs, rt) = ((i >> 21) & 31, (i >> 16) & 31);
    let target = pc.wrapping_add(4).wrapping_add(((i & 0xFFFF) as i16 as i32 as u32) << 2);
    let jump = (pc.wrapping_add(4) & 0xF000_0000) | ((i & 0x03FF_FFFF) << 2);
    match i >> 26 {
        0x00 => match i & 0x3F {
            0x08 if rs == 31 => Flow::Return,
            0x08 => Flow::JumpReg,
            0x09 => Flow::CallReg,
            _ => Flow::Next,
        },
        0x01 if rs == 0 && rt & 1 == 1 && rt & 0x1E == 0x10 => Flow::Call(target),
        0x01 if rs == 0 && rt & 1 == 1 => Flow::Jump(target),
        0x01 if rs == 0 => Flow::Never,
        0x01 => Flow::Branch(target),
        0x02 => Flow::Jump(jump),
        0x03 => Flow::Call(jump),
        0x04 if rs == rt => Flow::Jump(target),
        0x05 if rs == rt => Flow::Never,
        0x06 if rs == 0 => Flow::Jump(target),
        0x07 if rs == 0 => Flow::Never,
        0x04..=0x07 => Flow::Branch(target),
        _ => Flow::Next,
    }
}

/// The instruction at `pc` in a copy of main RAM, when it is main RAM above
/// the kernel (the first 64 KB).
fn word(ram: &[u8], pc: u32) -> Option<u32> {
    let p = fold(pc);
    if !(0x1_0000..0x20_0000).contains(&p) || pc & 3 != 0 {
        return None;
    }
    ram.get(p as usize..p as usize + 4).map(|w| u32::from_le_bytes(w.try_into().unwrap()))
}

struct Walk {
    /// Conditional branch -> target.
    branches: BTreeMap<u32, u32>,
    instructions: usize,
    truncated: bool,
}

/// Every instruction reachable from `roots` by branches, jumps and calls,
/// leaving out the world's entries and stopping at the end PC and at returns.
fn walk(ram: &[u8], roots: impl Iterator<Item = u32>, entries: &HashSet<u32>, end: Option<u32>) -> Walk {
    let mut todo: Vec<u32> = roots.filter(|r| !entries.contains(r)).collect();
    let mut seen: HashSet<u32> = HashSet::new();
    let mut branches = BTreeMap::new();
    let mut truncated = false;
    while let Some(mut pc) = todo.pop() {
        loop {
            if Some(pc) == end || !seen.insert(pc) {
                break;
            }
            if seen.len() >= WALK_LIMIT {
                truncated = true;
                todo.clear();
                break;
            }
            let Some(i) = word(ram, pc) else { break };
            match flow(pc, i) {
                Flow::Next => pc = pc.wrapping_add(4),
                Flow::Branch(t) => {
                    branches.insert(pc, t);
                    todo.push(t);
                    pc = pc.wrapping_add(8);
                }
                Flow::Jump(t) => {
                    todo.push(t);
                    break;
                }
                Flow::Never | Flow::CallReg => pc = pc.wrapping_add(8),
                Flow::Call(t) => {
                    if !entries.contains(&t) {
                        todo.push(t);
                    }
                    pc = pc.wrapping_add(8);
                }
                Flow::Return | Flow::JumpReg => break,
            }
        }
    }
    Walk { branches, instructions: seen.len(), truncated }
}

/// GTE registers' provenance through one instruction.
struct GteFlow {
    st: [From; 64],
    bad: Vec<(u8, From)>,
}

impl GteFlow {
    fn r(&mut self, regs: impl IntoIterator<Item = u8>) {
        for n in regs {
            let f = self.st[n as usize];
            if f != From::Step && !self.bad.contains(&(n, f)) {
                self.bad.push((n, f));
            }
        }
    }

    fn w(&mut self, regs: impl IntoIterator<Item = u8>) {
        for n in regs {
            self.st[n as usize] = From::Step;
        }
    }

    /// A FIFO push: each entry takes the next one's, the last is new.
    fn push(&mut self, fifo: &[u8]) {
        for k in 0..fifo.len() - 1 {
            self.st[fifo[k] as usize] = self.st[fifo[k + 1] as usize];
        }
        self.st[fifo[fifo.len() - 1] as usize] = From::Step;
    }
}

const V: [[u8; 2]; 3] = [[0, 1], [2, 3], [4, 5]];
const RGBC: u8 = 6;
const OTZ: u8 = 7;
const IR0: u8 = 8;
const IR: [u8; 3] = [9, 10, 11];
const SXY: [u8; 3] = [12, 13, 14];
const SZ: [u8; 4] = [16, 17, 18, 19];
const RGB: [u8; 3] = [20, 21, 22];
const MAC0: u8 = 24;
const MAC: [u8; 3] = [25, 26, 27];
const RT: [u8; 5] = [32, 33, 34, 35, 36];
const TR: [u8; 3] = [37, 38, 39];
const LLM: [u8; 5] = [40, 41, 42, 43, 44];
const BK: [u8; 3] = [45, 46, 47];
const LCM: [u8; 5] = [48, 49, 50, 51, 52];
const FC: [u8; 3] = [53, 54, 55];
const FLAG: u8 = 63;

const GTE_NAMES: [&str; 64] = [
    "VXY0", "VZ0", "VXY1", "VZ1", "VXY2", "VZ2", "RGBC", "OTZ", "IR0", "IR1", "IR2", "IR3", "SXY0", "SXY1", "SXY2",
    "SXYP", "SZ0", "SZ1", "SZ2", "SZ3", "RGB0", "RGB1", "RGB2", "RES1", "MAC0", "MAC1", "MAC2", "MAC3", "IRGB", "ORGB",
    "LZCS", "LZCR", "RT11RT12", "RT13RT21", "RT22RT23", "RT31RT32", "RT33", "TRX", "TRY", "TRZ", "L11L12", "L13L21",
    "L22L23", "L31L32", "L33", "RBK", "GBK", "BBK", "LR1LR2", "LR3LG1", "LG2LG3", "LB1LB2", "LB3", "RFC", "GFC", "BFC",
    "OFX", "OFY", "H", "DQA", "DQB", "ZSF3", "ZSF4", "FLAG",
];

/// A data register read as `mfc2`/`swc2` reads it.
fn data_read(g: &mut GteFlow, n: u8) {
    match n {
        15 => g.r([SXY[2]]),
        28 | 29 => g.r(IR),
        n => g.r([n]),
    }
}

/// A data register written as `mtc2`/`lwc2` writes it.
fn data_write(g: &mut GteFlow, n: u8) {
    match n {
        15 => g.push(&SXY),
        28 => g.w(IR),
        29 | 31 => {}
        30 => g.w([30, 31]),
        n => g.w([n]),
    }
}

/// The GTE registers an instruction reads and writes, in order, as psx-gte
/// runs it. Returns what did it, for the findings.
fn gte_flow(i: u32, g: &mut GteFlow) -> &'static str {
    let (rs, rt, rd) = ((i >> 21) & 31, ((i >> 16) & 31) as u8, ((i >> 11) & 31) as u8);
    match i >> 26 {
        0x32 => {
            data_write(g, rt);
            return "lwc2";
        }
        0x3A => {
            data_read(g, rt);
            return "swc2";
        }
        _ => {}
    }
    if i & (1 << 25) == 0 {
        return match rs {
            0x00 => {
                data_read(g, rd);
                "mfc2"
            }
            0x02 => {
                g.r([32 + rd]);
                "cfc2"
            }
            0x04 => {
                data_write(g, rd);
                "mtc2"
            }
            0x06 => {
                g.w([32 + rd]);
                "ctc2"
            }
            _ => "cop2",
        };
    }
    gte_command(i, g)
}

fn vertex(g: &mut GteFlow, vi: usize) {
    if vi < 3 { g.r(V[vi]) } else { g.r(IR) }
}

fn mac_ir(g: &mut GteFlow) {
    g.w(MAC);
    g.w(IR);
}

fn rtp(g: &mut GteFlow, vi: usize, last: bool) {
    g.r(RT);
    vertex(g, vi);
    g.r(TR);
    mac_ir(g);
    g.push(&SZ);
    g.r([58, 56, 57]);
    g.push(&SXY);
    if last {
        g.r([59, 60]);
        g.w([MAC0, IR0]);
    }
}

fn light(g: &mut GteFlow, vi: usize) {
    g.r(LLM);
    vertex(g, vi);
    mac_ir(g);
}

fn color_matrix(g: &mut GteFlow) {
    g.r(LCM);
    g.r(IR);
    g.r(BK);
    mac_ir(g);
}

fn depth_cue(g: &mut GteFlow) {
    g.r([IR0]);
    g.r(FC);
    mac_ir(g);
}

fn depth_cue_color(g: &mut GteFlow) {
    g.r([RGBC]);
    g.r(IR);
    depth_cue(g);
}

fn color_apply(g: &mut GteFlow) {
    g.r([RGBC]);
    g.r(IR);
    mac_ir(g);
}

fn push_color(g: &mut GteFlow) {
    g.r(MAC);
    g.r([RGBC]);
    g.push(&RGB);
}

fn gte_command(cmd: u32, g: &mut GteFlow) -> &'static str {
    let name = match op::alias(cmd) {
        op::RTPS => "RTPS",
        op::RTPT => "RTPT",
        op::NCLIP => "NCLIP",
        op::OP => "OP",
        op::DPCS => "DPCS",
        op::INTPL => "INTPL",
        op::MVMVA => "MVMVA",
        op::NCDS => "NCDS",
        op::NCDT => "NCDT",
        op::CDP => "CDP",
        op::NCCS => "NCCS",
        op::NCCT => "NCCT",
        op::CC => "CC",
        op::NCS => "NCS",
        op::NCT => "NCT",
        op::SQR => "SQR",
        op::DCPL => "DCPL",
        op::DPCT => "DPCT",
        op::AVSZ3 => "AVSZ3",
        op::AVSZ4 => "AVSZ4",
        op::GPF => "GPF",
        op::GPL => "GPL",
        _ => {
            g.w([FLAG]);
            return "a GTE command";
        }
    };
    g.w([FLAG]);
    match op::alias(cmd) {
        op::RTPS => rtp(g, 0, true),
        op::RTPT => (0..3).for_each(|v| rtp(g, v, v == 2)),
        op::NCLIP => {
            g.r(SXY);
            g.w([MAC0]);
        }
        op::OP => {
            g.r([RT[0], RT[2], RT[4]]);
            g.r(IR);
            mac_ir(g);
        }
        op::DPCS => {
            g.r([RGBC]);
            depth_cue(g);
            push_color(g);
        }
        op::INTPL => {
            g.r(IR);
            depth_cue(g);
            push_color(g);
        }
        op::MVMVA => {
            match cmd >> 17 & 3 {
                0 => g.r(RT),
                1 => g.r(LLM),
                2 => g.r(LCM),
                _ => g.r([RGBC, RT[1], RT[2], IR0]),
            }
            vertex(g, (cmd >> 15 & 3) as usize);
            match cmd >> 13 & 3 {
                0 => g.r(TR),
                1 => g.r(BK),
                2 => g.r(FC),
                _ => {}
            }
            mac_ir(g);
        }
        op::NCDS | op::NCDT => {
            let n = if op::alias(cmd) == op::NCDS { 1 } else { 3 };
            for v in 0..n {
                light(g, v);
                color_matrix(g);
                depth_cue_color(g);
                push_color(g);
            }
        }
        op::CDP => {
            color_matrix(g);
            depth_cue_color(g);
            push_color(g);
        }
        op::NCCS | op::NCCT => {
            let n = if op::alias(cmd) == op::NCCS { 1 } else { 3 };
            for v in 0..n {
                light(g, v);
                color_matrix(g);
                color_apply(g);
                push_color(g);
            }
        }
        op::CC => {
            color_matrix(g);
            color_apply(g);
            push_color(g);
        }
        op::NCS | op::NCT => {
            let n = if op::alias(cmd) == op::NCS { 1 } else { 3 };
            for v in 0..n {
                light(g, v);
                color_matrix(g);
                push_color(g);
            }
        }
        op::SQR => {
            g.r(IR);
            mac_ir(g);
        }
        op::DCPL => {
            depth_cue_color(g);
            push_color(g);
        }
        op::DPCT => {
            for _ in 0..3 {
                g.r([RGB[0]]);
                depth_cue(g);
                push_color(g);
            }
        }
        op::AVSZ3 => {
            g.r([SZ[1], SZ[2], SZ[3], 61]);
            g.w([MAC0, OTZ]);
        }
        op::AVSZ4 => {
            g.r(SZ);
            g.r([62]);
            g.w([MAC0, OTZ]);
        }
        op::GPF => {
            g.r([IR0]);
            g.r(IR);
            mac_ir(g);
            push_color(g);
        }
        op::GPL => {
            g.r([IR0]);
            g.r(IR);
            g.r(MAC);
            mac_ir(g);
            push_color(g);
        }
        _ => unreachable!(),
    }
    name
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gte(i: u32, known: &[u8]) -> (Vec<u8>, &'static str) {
        let mut g = GteFlow { st: [From::Before; 64], bad: Vec::new() };
        g.w(known.iter().copied());
        let by = gte_flow(i, &mut g);
        (g.bad.iter().map(|b| b.0).collect(), by)
    }

    #[test]
    fn gte_reads_follow_the_command() {
        // RTPS with V0 loaded: the matrix, translation and projection were set by others.
        let (bad, by) = gte(0x4A18_0001, &[0, 1]);
        assert_eq!(by, "RTPS");
        assert_eq!(bad, vec![32, 33, 34, 35, 36, 37, 38, 39, 58, 56, 57, 59, 60]);
        // AVSZ3 after three RTPS: SZ1-3 are the step's, ZSF3 is not.
        let mut g = GteFlow { st: [From::Before; 64], bad: Vec::new() };
        g.w((0..64).filter(|&n| n != 61 && !(16..20).contains(&n)));
        for _ in 0..3 {
            gte_flow(0x4A18_0001, &mut g);
        }
        g.bad.clear();
        gte_flow(0x4B58_002D, &mut g);
        assert_eq!(g.bad.iter().map(|b| b.0).collect::<Vec<_>>(), vec![61]);
        // mfc2 of SXYP reads SXY2; cfc2 reads a control register.
        assert_eq!(gte(0x4808_7800, &[]).0, vec![14]);
        assert_eq!(gte(0x4848_0000, &[]).0, vec![32]);
    }

    #[test]
    fn instruction_uses() {
        // addu t0, a0, a1
        assert_eq!(cpu_use(0x0085_4021), (1 << 4 | 1 << 5, 1 << 8));
        // sw ra, 0(sp): reads sp and ra.
        assert_eq!(cpu_use(0xAFBF_0000), (1 << 29 | 1 << 31, 0));
        // mflo v0
        assert_eq!(cpu_use(0x0000_1012), (1 << 33, 1 << 2));
        // bgezal a0 links.
        assert_eq!(cpu_use(0x0491_0003), (1 << 4, 1 << 31));
        // bal (bgezal zero) is a call; b (beq zero, zero) a jump.
        assert_eq!(flow(0x100, 0x0411_0003), Flow::Call(0x110));
        assert_eq!(flow(0x100, 0x1000_0003), Flow::Jump(0x110));
        assert_eq!(flow(0x100, 0x1080_0003), Flow::Branch(0x110));
    }
}
