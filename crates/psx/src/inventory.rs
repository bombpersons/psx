//! `psx inventory`: every function of every executable on the disc and of
//! the overlays RAM snapshots hold, with instruction counts and basic
//! blocks, and the disc's file list. The ledger's denominator.
//!
//! Functions are found by psx's own analysis, not Ghidra's: from the entry
//! point, every `jal` in a function already found, the call targets a traced
//! run recorded (calls through pointers included), and pointer words, in the
//! executable and in RAM snapshots where it is resident, that point at the
//! start of code no function covers yet (the handlers a table dispatches to,
//! which static analysis from the entry alone misses). Gaps no function
//! covers are listed, with how many of their words decode as instructions,
//! so they can be classified.
use crate::Error;
use crate::code::Code;
use crate::commands::{Ctx, Op, Outcome};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

#[derive(clap::Args, Clone, Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct InventoryArgs {
    /// Disc image (default protocol.toml's).
    #[serde(default)]
    pub media: Option<PathBuf>,
    /// Run directory whose RAM (and trace) adds runtime tables and overlays. Repeatable.
    #[arg(long = "ram")]
    #[serde(default)]
    pub ram: Vec<PathBuf>,
    /// Output (default notes/inventory.json beside protocol.toml, else .psx/).
    #[arg(long)]
    #[serde(default)]
    pub out: Option<PathBuf>,
}

const JR_RA: u32 = 0x03E0_0008;

/// The name a BIOS ROM is matched under, among the disc's files.
const BIOS: &str = "BIOS ";
/// Longest function the analysis follows, in instructions.
const MAX_FUNCTION: u32 = 0x4000;

/// An R3000A instruction psx's core executes (coprocessor 1 and 3, and
/// reserved encodings, are not instructions here).
fn valid(i: u32) -> bool {
    let op = i >> 26;
    match op {
        0x00 => matches!(
            i & 0x3F,
            0x00 | 0x02 | 0x03 | 0x04 | 0x06 | 0x07 | 0x08 | 0x09 | 0x0C | 0x0D | 0x10..=0x13 | 0x18..=0x1B | 0x20..=0x27 | 0x2A | 0x2B
        ),
        0x01 => matches!((i >> 16) & 0x1F, 0x00 | 0x01 | 0x10 | 0x11),
        0x02..=0x0F | 0x20..=0x26 | 0x28..=0x2B | 0x2E | 0x32 | 0x3A => true,
        0x10 | 0x12 => true,
        _ => false,
    }
}

/// Code a compiler emits: a valid instruction that does not write the zero
/// register (other than `nop`, and loads into it: a read a hardware register
/// needs to sync). Tables of data often decode as valid instructions, rarely
/// as sensible ones.
fn sensible(i: u32) -> bool {
    if !valid(i) {
        return false;
    }
    if i == 0 {
        return true;
    }
    let op = i >> 26;
    let (rt, rd) = ((i >> 16) & 31, (i >> 11) & 31);
    match op {
        // ALU and shifts write rd; mult/div, jr, syscall and break write
        // none. Shifting the zero register by a constant gives zero.
        0x00 => {
            let funct = i & 0x3F;
            !(rd == 0 && !matches!(funct, 0x08 | 0x0C | 0x0D | 0x11 | 0x13 | 0x18..=0x1B))
                && !(matches!(funct, 0x00 | 0x02 | 0x03) && rt == 0)
        }
        0x08..=0x0F => rt != 0,
        _ => true,
    }
}

fn jump_target(pc: u32, i: u32) -> u32 {
    (pc.wrapping_add(4) & 0xF000_0000) | ((i & 0x03FF_FFFF) << 2)
}

fn branch_target(pc: u32, i: u32) -> Option<u32> {
    let op = i >> 26;
    let is_branch = matches!(op, 0x04..=0x07) || (op == 0x01 && matches!((i >> 16) & 0x1F, 0x00 | 0x01 | 0x10 | 0x11));
    is_branch.then(|| pc.wrapping_add(4).wrapping_add(((i & 0xFFFF) as i16 as i32 as u32) << 2))
}

/// `jr`, `j` or a branch: the instruction after its delay slot starts a
/// block. Calls return, so they do not end one.
fn ends_block(i: u32) -> bool {
    branch_target(0, i).is_some() || i >> 26 == 0x02 || (i >> 26 == 0 && i & 0x3F == 0x08)
}

fn is_prologue(i: u32) -> bool {
    i >> 16 == 0x27BD && (i & 0x8000) != 0
}

/// One function: `[start, end)`.
#[derive(Clone, Debug)]
struct Function {
    start: u32,
    end: u32,
    found_by: BTreeSet<&'static str>,
    /// How the extent ended: return, tail_jump, next_function, invalid, limit.
    ends: &'static str,
}

/// Code at fixed addresses (an executable, or one RAM snapshot's overlay
/// areas), its functions and the seeds still to follow.
struct Analysis<'a> {
    code: &'a Code,
    /// Addresses functions may start at (an overlay excludes resident code).
    allowed: Box<dyn Fn(u32) -> bool + 'a>,
    /// Where pointers and prologues are looked for: an executable's whole
    /// range, or (for an overlay) the span of what calls and traces found.
    region: Option<(u32, u32)>,
    functions: BTreeMap<u32, Function>,
    queue: Vec<(u32, &'static str)>,
    /// Pointer targets inside functions: switch cases, block leaders.
    leaders: BTreeSet<u32>,
}

impl<'a> Analysis<'a> {
    fn new(code: &'a Code, region: Option<(u32, u32)>, allowed: Box<dyn Fn(u32) -> bool + 'a>) -> Self {
        Self { code, allowed, region, functions: BTreeMap::new(), queue: Vec::new(), leaders: BTreeSet::new() }
    }

    fn search(&self) -> Option<(u32, u32)> {
        self.region.or_else(|| self.span())
    }

    fn seed(&mut self, at: u32, how: &'static str) {
        if at.is_multiple_of(4) && self.code.word(at).is_some() && (self.allowed)(at) {
            self.queue.push((at, how));
        }
    }

    fn containing(&self, at: u32) -> Option<&Function> {
        self.functions.range(..=at).next_back().map(|(_, f)| f).filter(|f| at < f.end)
    }

    /// The function's extent from `start`: forward until a `jr ra` or tail
    /// `j` that no branch jumps past, the next known function, an invalid
    /// word or the limit.
    fn extent(&self, start: u32) -> (u32, &'static str) {
        let next = self.functions.range(start + 1..).next().map(|(&a, _)| a).unwrap_or(u32::MAX);
        let (mut pc, mut reach, mut zeros) = (start, start, 0);
        for _ in 0..MAX_FUNCTION {
            if pc >= next {
                return (next, "next_function");
            }
            let Some(i) = self.code.word(pc) else { return (pc, "image_end") };
            if !sensible(i) {
                return (pc, "invalid");
            }
            // Eight nops in a row are padding or memory not loaded yet.
            zeros = if i == 0 { zeros + 1 } else { 0 };
            if zeros == 8 {
                return (pc - 28, "invalid");
            }
            if let Some(t) = branch_target(pc, i) {
                if t > reach && t < next {
                    reach = t;
                }
            }
            let mut tail = false;
            if i >> 26 == 0x02 {
                let t = jump_target(pc, i);
                if t > pc + 4
                    && t < next
                    && !self.functions.contains_key(&t)
                    && t.wrapping_sub(start) < 4 * MAX_FUNCTION
                {
                    reach = reach.max(t);
                } else {
                    tail = true;
                }
            }
            if (i == JR_RA || tail) && reach <= pc + 4 {
                if !self.code.word(pc + 4).is_some_and(sensible) {
                    return (pc, "invalid");
                }
                return (pc + 8, if tail { "tail_jump" } else { "return" });
            }
            pc += 4;
        }
        (pc, "limit")
    }

    /// Follows the queue: each new function's extent, and the calls and
    /// tail jumps in it.
    fn drain(&mut self) {
        while let Some((at, how)) = self.queue.pop() {
            if let Some(f) = self.functions.get_mut(&at) {
                f.found_by.insert(how);
                continue;
            }
            // A seed inside a function found earlier splits it only when
            // it is a call target; a pointer there is a block in it.
            if how == "pointer" && self.containing(at).is_some() {
                self.leaders.insert(at);
                continue;
            }
            let mut f = Function { start: at, end: at, found_by: BTreeSet::from([how]), ends: "" };
            self.functions.insert(at, f.clone());
            (f.end, f.ends) = self.extent(at);
            // The function before may have run into this one.
            if let Some((&p, _)) = self.functions.range(..at).next_back() {
                let (end, ends) = self.extent(p);
                let prev = self.functions.get_mut(&p).unwrap();
                (prev.end, prev.ends) = (end, ends);
            }
            if f.end <= f.start {
                self.functions.remove(&at);
                continue;
            }
            let mut pc = f.start;
            while pc < f.end {
                let i = self.code.word(pc).unwrap_or(0);
                match i >> 26 {
                    0x03 => self.seed(jump_target(pc, i), "call"),
                    0x02 if f.ends == "tail_jump" && pc + 8 == f.end => self.seed(jump_target(pc, i), "tail_jump"),
                    _ => {}
                }
                pc += 4;
            }
            self.functions.insert(at, f);
        }
    }

    /// The span functions cover, from the first start to the last end.
    fn span(&self) -> Option<(u32, u32)> {
        Some((*self.functions.keys().next()?, self.functions.values().map(|f| f.end).max()?))
    }

    /// Pointers into the code: a target inside a function is a block
    /// leader; one in a gap that starts like a function and decodes to a
    /// return is a function.
    fn pointers(&mut self, words: &[(u32, u32)]) -> usize {
        let Some((lo, hi)) = self.search() else { return 0 };
        let before = self.functions.len();
        let targets: BTreeSet<u32> =
            words.iter().map(|&(_, w)| w).filter(|&w| w % 4 == 0 && w >= lo && w < hi).collect();
        for t in targets {
            if !(self.allowed)(t) {
                continue;
            }
            if self.containing(t).is_some() {
                self.leaders.insert(t);
                continue;
            }
            if self.starts_function(t) {
                self.seed(t, "pointer");
                self.drain();
            }
        }
        self.functions.len() - before
    }

    /// Starts like a function: a stack prologue, or just after a return's
    /// delay slot or padding, and decodes to a return.
    fn starts_function(&self, t: u32) -> bool {
        let Some(i) = self.code.word(t) else { return false };
        let boundary = is_prologue(i) || i != 0 && self.after_end(t);
        if !boundary || !sensible(i) {
            return false;
        }
        // A tail jump lands on a function already known: tables of data
        // decode as jumps to anywhere.
        match self.extent(t) {
            (_, "return") => true,
            (end, "tail_jump") => {
                let w = self.code.word(end - 8).unwrap_or(0);
                self.functions.contains_key(&jump_target(end - 8, w))
            }
            _ => false,
        }
    }

    /// `t` follows the end of a function: back over zero padding to a
    /// delay slot whose jump (`jr ra` or a tail `j`) ends that function.
    fn after_end(&self, t: u32) -> bool {
        let mut a = t.wrapping_sub(4);
        let mut padding = 0;
        while self.code.word(a) == Some(0) && padding < 64 {
            a = a.wrapping_sub(4);
            padding += 1;
        }
        // `a` is the jump, unless its delay slot was not a nop. A `jr` on
        // another register is a switch, whose cases follow it.
        let ends = |w: u32| w >> 26 == 0x02 || w == JR_RA;
        match (self.code.word(a), self.code.word(a.wrapping_sub(4))) {
            (Some(w), _) if padding > 0 && ends(w) => true,
            (_, Some(w)) => ends(w) && self.code.word(t.wrapping_sub(4)).is_some_and(|d| !ends(d)),
            _ => false,
        }
    }

    /// Gaps between functions: a stack prologue, or code just after a
    /// return's delay slot (and any padding), that decodes to a return is a
    /// function (leaf handlers have no prologue); what is left is listed.
    fn prologues(&mut self) {
        let Some((lo, hi)) = self.search() else { return };
        let mut at = lo;
        while at < hi {
            if let Some(f) = self.containing(at) {
                at = f.end;
                continue;
            }
            if (self.allowed)(at) && self.starts_function(at) {
                let how = if self.code.word(at).is_some_and(is_prologue) { "prologue" } else { "after_return" };
                self.seed(at, how);
                self.drain();
                continue;
            }
            at += 4;
        }
    }

    /// Code a traced run executed that no function covers: where each
    /// stretch of executed words this code holds starts. Only a stretch of
    /// four words or a whole run of executed words, and only one that
    /// decodes to an end: code loaded over this code shares nops and
    /// returns with it.
    fn executed(&mut self, ran: &BTreeSet<(u32, u32)>) {
        let Some((lo, hi)) = self.search() else { return };
        let addrs: BTreeSet<u32> = ran.range((lo, 0)..(hi, 0)).map(|&(a, _)| a).collect();
        let held: Vec<u32> =
            ran.range((lo, 0)..(hi, 0)).filter(|&&(a, w)| self.code.word(a) == Some(w)).map(|&(a, _)| a).collect();
        let mut starts = Vec::new();
        let mut k = 0;
        while k < held.len() {
            let mut e = k + 1;
            while e < held.len() && held[e] == held[e - 1] + 4 {
                e += 1;
            }
            let (s, end) = (held[k], held[e - 1] + 4);
            let whole = !addrs.contains(&(s - 4)) && !addrs.contains(&end);
            if e - k >= 4 || whole {
                starts.push(s);
            }
            k = e;
        }
        for a in starts {
            if self.containing(a).is_none() && (self.allowed)(a) && self.extent(a).1 != "invalid" {
                self.seed(a, "executed");
                self.drain();
            }
        }
    }

    fn gaps(&self, lo: u32, hi: u32) -> Vec<Value> {
        let mut out = Vec::new();
        let mut at = lo;
        while at < hi {
            if let Some(f) = self.containing(at) {
                at = f.end;
                continue;
            }
            let start = at;
            while at < hi && self.containing(at).is_none() {
                at += 4;
            }
            let words: Vec<u32> = (start..at).step_by(4).filter_map(|a| self.code.word(a)).collect();
            if words.iter().all(|&w| w == 0) {
                continue;
            }
            let decodes = words.iter().filter(|&&w| w != 0 && sensible(w)).count();
            out.push(json!({"start": hex(start), "end": hex(at), "words": words.len(), "decode": decodes}));
        }
        out
    }

    fn report(&self) -> (Vec<Value>, Value) {
        let (lo, hi) = self.span().unwrap_or_default();
        self.report_in(lo, hi, &|_| true)
    }

    /// The functions starting in `[lo, hi)`, their totals and the gaps
    /// between them.
    fn report_in(&self, lo: u32, hi: u32, keep: &dyn Fn(u32) -> bool) -> (Vec<Value>, Value) {
        let mut rows = Vec::new();
        let (mut instructions, mut blocks, mut pointers) = (0u64, 0u64, 0usize);
        for f in self.functions.range(lo..hi).map(|(_, f)| f).filter(|f| keep(f.start)) {
            pointers += f.found_by.iter().all(|h| *h == "pointer") as usize;
            let mut leaders = BTreeSet::from([f.start]);
            let mut pc = f.start;
            while pc < f.end {
                let i = self.code.word(pc).unwrap_or(0);
                if let Some(t) = branch_target(pc, i) {
                    if t >= f.start && t < f.end {
                        leaders.insert(t);
                    }
                }
                if i >> 26 == 0x02 {
                    let t = jump_target(pc, i);
                    if t >= f.start && t < f.end {
                        leaders.insert(t);
                    }
                }
                if ends_block(i) && pc + 8 < f.end {
                    leaders.insert(pc + 8);
                }
                pc += 4;
            }
            leaders.extend(self.leaders.range(f.start..f.end));
            let n = (f.end - f.start) / 4;
            instructions += n as u64;
            blocks += leaders.len() as u64;
            rows.push(json!({
                "entry": hex(f.start), "end": hex(f.end), "instructions": n, "blocks": leaders.len(),
                "block_starts": leaders.iter().map(|&l| hex(l)).collect::<Vec<_>>(),
                "found_by": f.found_by.iter().collect::<Vec<_>>(), "ends": f.ends,
            }));
        }
        let gaps = self.gaps(lo, hi);
        let unclaimed: u64 = gaps.iter().map(|g| g["decode"].as_u64().unwrap_or(0)).sum();
        let totals = json!({
            "functions": rows.len(), "instructions": instructions, "blocks": blocks,
            "found_only_through_pointers": pointers,
            "unclaimed_gaps": gaps.len(), "unclaimed_decoding_words": unclaimed,
        });
        (rows, json!({"totals": totals, "gaps": gaps}))
    }
}

/// The disc file a cluster of code in RAM was loaded from: one whose bytes
/// hold the cluster's first 64 and which RAM holds around them. Gives the
/// file and the extent of it RAM still holds.
fn loaded_from(ram: &[u8], lo: u32, hi: u32, files: &[(String, Vec<u8>)]) -> Option<(Value, u32, u32)> {
    let o = (lo & 0x1F_FFFF) as usize;
    let needle = ram.get(o..(o + 64).min((hi & 0x1F_FFFF) as usize))?;
    if needle.iter().all(|&b| b == 0) {
        return None;
    }
    let mut best: Option<(usize, Value, u32, u32)> = None;
    for (path, bytes) in files {
        for off in memchr::memmem::find_iter(bytes, needle).take(8) {
            let Some(start) = o.checked_sub(off) else { continue };
            let n = bytes.len().min(ram.len() - start) / 4;
            if n == 0 || (start & 3) != 0 {
                continue;
            }
            let eq = |k: usize| ram[start + 4 * k..start + 4 * k + 4] == bytes[4 * k..4 * k + 4];
            // The extent: from the file's start, 4 KiB windows at least half
            // the same, around the cluster (a later load may have covered
            // the file's end, or the program its start).
            let window = 1024;
            let good = |w: usize| {
                let r = w * window..((w + 1) * window).min(n);
                r.clone().filter(|&k| eq(k)).count() * 2 >= r.len()
            };
            let windows = n.div_ceil(window);
            let here = off / 4 / window;
            if here >= windows || !good(here) {
                continue;
            }
            let (mut first, mut last) = (here, here);
            while first > 0 && good(first - 1) {
                first -= 1;
            }
            while last + 1 < windows && good(last + 1) {
                last += 1;
            }
            // Zeros match zeros anywhere: not at the extent's ends.
            let zeros = |w: usize| bytes[4 * w * window..(4 * (w + 1) * window).min(4 * n)].iter().all(|&b| b == 0);
            while first < here && zeros(first) {
                first += 1;
            }
            while last > here && zeros(last) {
                last -= 1;
            }
            let (a, b) = (first * window, ((last + 1) * window).min(n));
            let same = (a..b).filter(|&k| eq(k)).count();
            if best.as_ref().is_some_and(|x| x.0 >= same) {
                continue;
            }
            let at = 0x8000_0000 | start as u32;
            let file = json!({
                "path": path, "offset": off, "loads_at": hex(at), "file_words": n,
                "in_ram": {"start": hex(at + 4 * a as u32), "end": hex(at + 4 * b as u32), "words_same": same},
            });
            best = Some((same, file, at + 4 * a as u32, at + 4 * b as u32));
        }
    }
    best.map(|b| (b.1, b.2, b.3))
}

/// Groups of functions with less than 4 KiB between one's end and the
/// next's start: one overlay each.
fn clusters(a: &Analysis) -> Vec<(u32, u32)> {
    cluster(a.functions.values().map(|f| (f.start, f.end)))
}

fn cluster(spans: impl Iterator<Item = (u32, u32)>) -> Vec<(u32, u32)> {
    let mut out: Vec<(u32, u32)> = Vec::new();
    for (start, end) in spans {
        match out.last_mut() {
            Some(c) if start < c.1 + 0x1000 => c.1 = c.1.max(end),
            _ => out.push((start, end)),
        }
    }
    out
}

fn parse_hex(v: &Value) -> u32 {
    v.as_str().and_then(|s| u32::from_str_radix(s, 16).ok()).unwrap_or(0)
}

fn hex(a: u32) -> String {
    format!("{a:08X}")
}

fn words(bytes: &[u8], base: u32) -> Vec<(u32, u32)> {
    bytes.as_chunks::<4>().0.iter().enumerate().map(|(i, w)| (base + 4 * i as u32, u32::from_le_bytes(*w))).collect()
}

/// A traced run's call targets (calls.json), through pointers included.
fn trace_calls(run: &Path) -> Vec<u32> {
    let Ok(text) = std::fs::read_to_string(run.join("calls.json")) else { return Vec::new() };
    let v: Value = serde_json::from_str(&text).unwrap_or_default();
    v.as_array().into_iter().flatten().filter_map(|c| c["address"].as_u64().map(|a| a as u32)).collect()
}

/// A snapshot: its run directory, RAM, the executables it holds (with the
/// share of each unchanged) and its trace's call targets, each with the
/// instruction it ran there when the run recorded executed.bin.
struct Snap {
    label: String,
    /// What its trace executed: `(address, instruction)`.
    ran: Vec<(u32, u32)>,
    ram: Vec<u8>,
    resident: Vec<(usize, f64)>,
    calls: Vec<(u32, Option<u32>)>,
}

/// Code a traced run executed that neither its RAM at the end nor an
/// executable holds: `(address, instruction)`, in address order.
fn lost_code(ram: &[u8], ran: &[(u32, u32, u64)], exes: &[Exe]) -> Vec<(u32, u32)> {
    let word = |b: &[u8], o: usize| b.get(o..o + 4).map(|w| u32::from_le_bytes(w.try_into().unwrap()));
    ran.iter()
        .filter(|&&(a, w, _)| {
            (0x8001_0000..0x8020_0000).contains(&a)
                && word(ram, (a & 0x1F_FFFF) as usize) != Some(w)
                && !exes.iter().any(|e| a >= e.lo && a < e.hi() && word(&e.payload, (a - e.lo) as usize) == Some(w))
        })
        .map(|&(a, w, _)| (a, w))
        .collect()
}

/// Where lost code was loaded from: the disc files holding runs of it, as
/// `(file, address its first byte was loaded at)`.
fn placements(lost: &[(u32, u32)], files: &[(String, Vec<u8>)]) -> Vec<(usize, u32)> {
    let mut out: Vec<(usize, u32)> = Vec::new();
    let holds = |&(f, at): &(usize, u32), a: u32, w: u32| {
        let o = a.wrapping_sub(at) as usize;
        files[f].1.get(o..o + 4).is_some_and(|b| u32::from_le_bytes(b.try_into().unwrap()) == w)
    };
    let mut k = 0;
    while k < lost.len() {
        let mut e = k + 1;
        while e < lost.len() && lost[e].0 == lost[e - 1].0 + 4 {
            e += 1;
        }
        let run = &lost[k..e];
        k = e;
        if run.len() < 4 || out.iter().any(|p| holds(p, run[0].0, run[0].1)) {
            continue;
        }
        let needle: Vec<u8> = run.iter().take(16).flat_map(|&(_, w)| w.to_le_bytes()).collect();
        'files: for (f, (_, bytes)) in files.iter().enumerate() {
            for off in memchr::memmem::find_iter(bytes, &needle).take(4) {
                let Some(at) = run[0].0.checked_sub(off as u32) else { continue };
                if off % 4 == 0 && at >= 0x8000_0000 {
                    out.push((f, at));
                    break 'files;
                }
            }
        }
    }
    out
}

struct Exe {
    path: String,
    boot: bool,
    lo: u32,
    payload: Vec<u8>,
    entry: u32,
    gp: Option<u32>,
}

impl Exe {
    fn hi(&self) -> u32 {
        self.lo + self.payload.len() as u32
    }

    /// The share of its words a RAM snapshot holds unchanged.
    fn resident(&self, ram: &[u8]) -> f64 {
        let o = (self.lo & 0x1F_FFFF) as usize;
        let Some(r) = ram.get(o..o + self.payload.len()) else { return 0.0 };
        let (same, all) = r
            .as_chunks::<4>()
            .0
            .iter()
            .zip(self.payload.as_chunks::<4>().0)
            .fold((0usize, 0usize), |(s, a), (x, y)| (s + (x == y) as usize, a + 1));
        if all == 0 { 0.0 } else { same as f64 / all as f64 }
    }
}

/// Where inventory keeps an overlay's bytes, by their sha256.
pub(crate) fn overlay_bytes(root: &Path, sha: &str) -> PathBuf {
    root.join(".psx/overlays").join(format!("{sha}.bin"))
}

pub(crate) fn default_out(ctx: &Ctx) -> PathBuf {
    if ctx.root.join("protocol.toml").is_file() {
        ctx.root.join("notes/inventory.json")
    } else {
        ctx.root.join(".psx/inventory.json")
    }
}

pub(crate) fn protocol_media(root: &Path) -> Option<PathBuf> {
    let text = std::fs::read_to_string(root.join("protocol.toml")).ok()?;
    let v: toml::Value = toml::from_str(&text).ok()?;
    Some(PathBuf::from(v.get("disc")?.get("media")?.as_str()?))
}

impl InventoryArgs {
    fn run(&self, ctx: &Ctx) -> crate::Result<Value> {
        use psx_media::disc::Disc;
        use psx_media::iso9660::FileSystem;
        let at = |p: &Path| if p.is_absolute() { p.to_path_buf() } else { ctx.root.join(p) };
        let media = match &self.media {
            Some(m) => at(m),
            None => protocol_media(&ctx.root).ok_or_else(|| {
                Error::invalid("give a disc image, or run in a repository whose protocol.toml names one")
            })?,
        };
        let disc = Disc::open(&media)?;
        let fs = FileSystem::read(&disc)?;
        let cnf = fs.find("/SYSTEM.CNF").and_then(|e| fs.read_file(&disc, e).ok());
        let boot = cnf
            .as_deref()
            .map(psx_media::system_cnf::SystemCnf::parse)
            .and_then(|c| c.boot)
            .unwrap_or_else(|| psx_media::system_cnf::DEFAULT_BOOT.into());
        let boot = crate::roles::boot_path(&boot);

        // The disc's files, and its executables.
        let mut files = Vec::new();
        let mut exes = Vec::new();
        let mut contents: Vec<(String, Vec<u8>)> = Vec::new();
        for e in fs.entries.iter().filter(|e| !e.is_dir) {
            let bytes = if e.is_streamed() { None } else { fs.read_file(&disc, e).ok() };
            let executable = bytes.as_deref().is_some_and(|b| b.starts_with(b"PS-X EXE"));
            files.push(json!({
                "path": e.path, "lba": e.lba, "size": e.size, "streamed": e.is_streamed(),
                "executable": executable, "boot": e.path.eq_ignore_ascii_case(&boot),
            }));
            let Some(bytes) = bytes else { continue };
            if executable {
                let exe = psx_media::PsxExe::parse(&bytes)?;
                exes.push(Exe {
                    path: e.path.clone(),
                    boot: e.path.eq_ignore_ascii_case(&boot),
                    lo: 0x8000_0000 | (exe.text_addr & 0x1F_FFFF),
                    payload: exe.payload(&bytes).to_vec(),
                    entry: exe.pc,
                    gp: (exe.gp != 0).then_some(exe.gp),
                });
            } else {
                contents.push((e.path.clone(), bytes));
            }
        }

        // The BIOS ROMs the runs booted: the shell they copy into RAM at
        // power-on (the logo) is the BIOS's code, not the game's.
        for r in &self.ram {
            let report: Option<Value> =
                std::fs::read_to_string(at(r).join("report.json")).ok().and_then(|t| serde_json::from_str(&t).ok());
            let Some(bios) = report.as_ref().and_then(|v| v["identity"]["bios"].as_str()).map(PathBuf::from) else {
                continue;
            };
            let name = format!("{BIOS}{}", bios.file_name().unwrap_or_default().to_string_lossy());
            if !contents.iter().any(|c| c.0 == name) {
                if let Ok(bytes) = std::fs::read(at(&bios)) {
                    contents.push((name, bytes));
                }
            }
        }

        // The snapshots: which executable each holds, and their traces.
        // Code a trace ran that is gone by the end (loaded over again) is
        // searched for in the disc's files; each file found is analysed as
        // if a snapshot held it alone, where the run ran it.
        let mut snaps: Vec<Snap> = Vec::new();
        for r in &self.ram {
            let dir = at(r);
            let snap = crate::ram::load(&dir)?;
            let resident: Vec<(usize, f64)> =
                exes.iter().enumerate().map(|(i, e)| (i, e.resident(&snap.bytes))).filter(|&(_, s)| s >= 0.9).collect();
            let ran = crate::coverage::executed(&dir).ok();
            // One entry per instruction a target ran as (code loaded over
            // code runs as several).
            let mut ran_at: BTreeMap<u32, Vec<u32>> = BTreeMap::new();
            for &(a, w, _) in ran.iter().flatten() {
                ran_at.entry(a).or_default().push(w);
            }
            let calls: Vec<(u32, Option<u32>)> = trace_calls(&dir)
                .into_iter()
                .flat_map(|t| match &ran {
                    None => vec![(t, None)],
                    Some(_) => ran_at
                        .get(&(0x8000_0000 | (t & 0x1F_FFFF)))
                        .map(|ws| ws.iter().map(|&w| (t, Some(w))).collect())
                        .unwrap_or_default(),
                })
                .collect();
            let lost = ran.as_deref().map(|r| lost_code(&snap.bytes, r, &exes)).unwrap_or_default();
            for (f, at) in placements(&lost, &contents) {
                let mut ram = vec![0u8; snap.bytes.len()];
                let o = (at & 0x1F_FFFF) as usize;
                let bytes = &contents[f].1;
                let n = bytes.len().min(ram.len().saturating_sub(o));
                ram[o..o + n].copy_from_slice(&bytes[..n]);
                let label = format!("{} (ran {} loaded at {})", dir.display(), contents[f].0, hex(at));
                let calls = calls
                    .iter()
                    .copied()
                    .filter(|&(t, w)| {
                        w.is_some_and(|w| {
                            let k = (t & 0x1F_FFFF) as usize;
                            ram.get(k..k + 4).is_some_and(|b| u32::from_le_bytes(b.try_into().unwrap()) == w)
                        })
                    })
                    .collect();
                snaps.push(Snap { label, ram, resident: Vec::new(), calls, ran: Vec::new() });
            }
            let ran = ran.unwrap_or_default().into_iter().map(|(a, w, _)| (a, w)).collect();
            snaps.push(Snap { label: dir.display().to_string(), ram: snap.bytes, resident, calls, ran });
        }

        // Traced call targets with the instruction run there: a seed in any
        // executable or snapshot holding that instruction there.
        let ran_calls: BTreeSet<(u32, u32)> =
            snaps.iter().flat_map(|s| s.calls.iter().filter_map(|&(t, w)| Some((t, w?)))).collect();
        let ran_words: BTreeSet<(u32, u32)> = snaps.iter().flat_map(|s| s.ran.iter().copied()).collect();

        let mut executables = Vec::new();
        // Each executable's functions, for the calls they make into overlays.
        let mut found: Vec<Vec<(u32, u32)>> = Vec::new();
        let mut totals = (0u64, 0u64, 0u64);
        for (i, exe) in exes.iter().enumerate() {
            let code =
                Code { base: exe.lo, bytes: exe.payload.clone(), origin: exe.path.clone(), gp: exe.gp, exe: None };
            let (lo, hi) = (exe.lo, exe.hi());
            let mut a = Analysis::new(&code, Some((lo, hi)), Box::new(move |x| x >= lo && x < hi));
            a.seed(exe.entry, "entry");
            a.drain();
            let held: Vec<&Snap> = snaps.iter().filter(|s| s.resident.iter().any(|&(j, _)| j == i)).collect();
            // Where the executable's code is what a run ran there; a trace
            // without executed.bin, in snapshots holding the executable.
            for &(t, w) in &ran_calls {
                if code.word(t) == Some(w) {
                    a.seed(t, "trace_call");
                }
            }
            for s in &held {
                for &(t, w) in &s.calls {
                    if w.is_none() {
                        a.seed(t, "trace_call");
                    }
                }
            }
            a.drain();
            let mut pointer_words = words(&exe.payload, exe.lo);
            for s in &held {
                pointer_words.extend(words(&s.ram, 0x8000_0000));
            }
            // Until no new function is found: each round's functions may
            // call more, and widen the span pointers may land in.
            while a.pointers(&pointer_words) > 0 {}
            a.prologues();
            a.executed(&ran_words);
            while a.pointers(&pointer_words) > 0 {}
            let (functions, rest) = a.report();
            add(&mut totals, &rest["totals"]);
            found.push(a.functions.values().map(|f| (f.start, f.end)).collect::<Vec<_>>());
            executables.push(json!({
                "path": exe.path, "boot": exe.boot, "loads_at": hex(exe.lo), "end": hex(exe.hi()),
                "entry": hex(exe.entry),
                "snapshots": held.iter().map(|s| s.label.clone()).collect::<Vec<_>>(),
                "totals": rest["totals"], "gaps": rest["gaps"], "functions": functions,
            }));
        }

        // Overlays: code a snapshot holds that is no executable's (RAM that
        // still equals an executable's bytes is that executable's), reached
        // by calls from the resident executables' code still in place, or recorded by
        // the snapshot's trace. Below 0x80010000 is the BIOS kernel's.
        let mut overlays: Vec<Value> = Vec::new();
        let mut bios_code: Vec<Value> = Vec::new();
        let mut seen: BTreeMap<String, usize> = BTreeMap::new();
        for Snap { label, ram, calls, resident, .. } in &snaps {
            let code = Code { base: 0x8000_0000, bytes: ram.clone(), origin: label.clone(), gp: None, exe: None };
            let mut theirs = vec![false; ram.len() / 4];
            for e in &exes {
                let o = (e.lo & 0x1F_FFFF) as usize;
                for (k, w) in e.payload.as_chunks::<4>().0.iter().enumerate() {
                    if ram.get(o + 4 * k..o + 4 * k + 4) == Some(&w[..]) {
                        theirs[o / 4 + k] = true;
                    }
                }
            }
            let held = |x: u32| theirs.get(((x & 0x1F_FFFF) / 4) as usize).copied().unwrap_or(true);
            let outside = |x: u32| (0x8001_0000..0x8020_0000).contains(&x) && !held(x);
            let mut a = Analysis::new(&code, None, Box::new(outside));
            // Calls from the executables the snapshot holds (executables
            // sharing library code at the same addresses look held there too).
            for &(i, _) in resident {
                for &(start, end) in &found[i] {
                    if !(start..end).step_by(4).all(held) {
                        continue;
                    }
                    for pc in (start..end).step_by(4) {
                        let w = code.word(pc).unwrap_or(0);
                        if w >> 26 == 3 {
                            a.seed(jump_target(pc, w), "call_from_executable");
                        }
                    }
                }
            }
            for &(t, w) in calls
                .iter()
                .filter(|c| c.1.is_none())
                .chain(&ran_calls.iter().map(|&(t, w)| (t, Some(w))).collect::<Vec<_>>())
            {
                if w.is_none_or(|w| code.word(t) == Some(w)) {
                    a.seed(t, "trace_call");
                }
            }
            a.drain();
            if a.functions.is_empty() {
                continue;
            }
            // Each overlay is searched on its own. Its extent is the disc file
            // RAM holds there, when one matches; else its cluster of functions.
            let ram_words = words(ram, 0x8000_0000);
            let mut regions: Vec<(u32, u32, Option<Value>)> = Vec::new();
            for _ in 0..4 {
                let before = a.functions.len();
                for (lo, hi) in clusters(&a) {
                    if regions.iter().any(|r| lo >= r.0 && lo < r.1) {
                        continue;
                    }
                    let found = loaded_from(ram, lo, hi, &contents);
                    let (rlo, rhi) = found.as_ref().map(|f| (f.1, f.2)).unwrap_or((lo, hi));
                    regions.retain(|r| !(r.0 >= rlo && r.0 < rhi && r.2.is_none()));
                    regions.push((rlo, rhi, found.map(|f| f.0)));
                }
                for &(lo, hi, _) in &regions {
                    a.region = Some((lo, hi));
                    while a.pointers(&ram_words) > 0 {}
                    a.prologues();
                    a.executed(&ran_words);
                }
                if a.functions.len() == before {
                    break;
                }
            }
            // A file's extent claims only the functions whose code is the
            // file's: others are code loaded over part of it, overlays of
            // their own (matched to a file of their own when one holds them).
            let mut foreign: BTreeSet<u32> = BTreeSet::new();
            for (lo, hi, file) in &regions {
                let Some(f) = file else { continue };
                let Some((_, bytes)) = contents.iter().find(|c| f["path"] == c.0.as_str()) else { continue };
                let at = parse_hex(&f["loads_at"]);
                for func in a.functions.range(lo..hi).map(|(_, f)| f) {
                    let n = ((func.end - func.start) / 4) as usize;
                    let same = (func.start..func.end)
                        .step_by(4)
                        .filter(|&x| {
                            let k = (x - at) as usize;
                            bytes
                                .get(k..k + 4)
                                .is_some_and(|b| code.word(x) == Some(u32::from_le_bytes(b.try_into().unwrap())))
                        })
                        .count();
                    if 2 * same < n {
                        foreign.insert(func.start);
                    }
                }
            }
            let mut regions: Vec<(u32, u32, Option<Value>, bool)> =
                regions.into_iter().map(|(lo, hi, f)| (lo, hi, f, false)).collect();
            for (lo, hi) in cluster(foreign.iter().map(|s| (*s, a.functions[s].end))) {
                // Its own file only when that file's extent is the cluster's.
                let found = loaded_from(ram, lo, hi, &contents).filter(|f| f.1 <= lo && f.2 >= hi);
                let (rlo, rhi) = found.as_ref().map(|f| (f.1, f.2)).unwrap_or((lo, hi));
                regions.push((rlo, rhi, found.map(|f| f.0), true));
            }
            regions.sort_by_key(|r| r.0);
            for (lo, hi, file, over) in regions {
                let (functions, rest) = a.report_in(lo, hi, &|s| foreign.contains(&s) == over);
                if functions.is_empty() {
                    continue;
                }
                let o = (lo & 0x1F_FFFF) as usize..(hi & 0x1F_FFFF) as usize;
                let sha = crate::publish::sha256(&ram[o.clone()]);
                let from_bios = file.as_ref().and_then(|f| f["path"].as_str()).is_some_and(|p| p.starts_with(BIOS));
                if from_bios {
                    // Kept apart, for coverage to credit; one entry per BIOS
                    // and address it was loaded at, over the widest extent.
                    let at = file.as_ref().map(|f| (f["path"].clone(), f["loads_at"].clone()));
                    let same = bios_code
                        .iter()
                        .position(|b| Some((b["file"]["path"].clone(), b["file"]["loads_at"].clone())) == at);
                    if let Some(k) = same {
                        let (blo, bhi) = (parse_hex(&bios_code[k]["start"]), parse_hex(&bios_code[k]["end"]));
                        if hi - lo <= bhi - blo {
                            if let Some(runs) = bios_code[k]["snapshots"].as_array_mut() {
                                runs.push(json!(label));
                            }
                            continue;
                        }
                        bios_code.remove(k);
                    }
                    let keep = overlay_bytes(&ctx.root, &sha);
                    if let Some(d) = keep.parent() {
                        std::fs::create_dir_all(d).map_err(|e| Error::io(d, e))?;
                    }
                    std::fs::write(&keep, &ram[o.clone()]).map_err(|e| Error::io(&keep, e))?;
                    bios_code.push(
                        json!({"start": hex(lo), "end": hex(hi), "sha256": sha, "file": file, "snapshots": [label]}),
                    );
                    continue;
                }
                if let Some(&k) = seen.get(&sha) {
                    if let Some(runs) = overlays[k]["snapshots"].as_array_mut() {
                        runs.push(json!(label));
                    }
                    continue;
                }
                add(&mut totals, &rest["totals"]);
                // Its bytes, for coverage to tell it from code loaded over it.
                let keep = overlay_bytes(&ctx.root, &sha);
                if let Some(d) = keep.parent() {
                    std::fs::create_dir_all(d).map_err(|e| Error::io(d, e))?;
                }
                std::fs::write(&keep, &ram[o.clone()]).map_err(|e| Error::io(&keep, e))?;
                seen.insert(sha.clone(), overlays.len());
                overlays.push(json!({
                    "start": hex(lo), "end": hex(hi), "sha256": sha, "file": file,
                    "snapshots": [label],
                    "totals": rest["totals"], "gaps": rest["gaps"], "functions": functions,
                }));
            }
        }

        let out = self.out.as_ref().map(|p| at(p)).unwrap_or_else(|| default_out(ctx));
        let summary = json!({
            "functions": totals.0, "instructions": totals.1, "blocks": totals.2,
            "executables": exes.len(), "overlays": overlays.len(), "disc_files": files.len(),
        });
        let previous = std::fs::read_to_string(&out)
            .ok()
            .and_then(|t| serde_json::from_str::<Value>(&t).ok())
            .map(|v| v["totals"].clone());
        let inventory = json!({
            "inventory": 1, "made": crate::publish::utc_now(), "psx": crate::context::version(),
            "media": media, "boot": boot, "totals": summary,
            "how": "functions from each executable's entry, calls in functions found, traced call targets and pointers into code (in the executable and in snapshots holding it); an overlay is code a snapshot holds outside its executables (code from the BIOS ROM, the shell, is listed under bios_code instead); gaps are what no function covers, with how many words decode as instructions",
            "executables": executables, "overlays": overlays, "bios_code": bios_code, "disc": {"files": files},
        });
        if let Some(d) = out.parent() {
            std::fs::create_dir_all(d).map_err(|e| Error::io(d, e))?;
        }
        let text = serde_json::to_string_pretty(&inventory).unwrap_or_default() + "\n";
        std::fs::write(&out, text).map_err(|e| Error::io(&out, e))?;
        let history = out.with_file_name("inventory_history.jsonl");
        let line =
            serde_json::to_string(&json!({"made": inventory["made"], "psx": inventory["psx"], "totals": summary}))
                .unwrap_or_default();
        use std::io::Write;
        std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&history)
            .and_then(|mut f| writeln!(f, "{line}"))
            .map_err(|e| Error::io(&history, e))?;

        let per = |list: &[Value], key: &str| -> Vec<Value> {
            list.iter().map(|x| json!({key: x[key].clone(), "file": x.get("file").cloned().unwrap_or(Value::Null), "totals": x["totals"]})).collect()
        };
        Ok(json!({
            "out": out, "history": history, "totals": summary, "previous": previous,
            "executables": executables.iter().map(|x| json!({"path": x["path"], "boot": x["boot"], "totals": x["totals"]})).collect::<Vec<_>>(),
            "overlays": per(&overlays, "start"),
            "bios_code": bios_code.iter().map(|b| json!({"start": b["start"], "end": b["end"], "file": b["file"]["path"]})).collect::<Vec<_>>(),
            "next": "artifact_read the inventory for functions and gaps; keep it in the investigator's notes so the ledger can count against it",
        }))
    }
}

fn add(t: &mut (u64, u64, u64), v: &Value) {
    t.0 += v["functions"].as_u64().unwrap_or(0);
    t.1 += v["instructions"].as_u64().unwrap_or(0);
    t.2 += v["blocks"].as_u64().unwrap_or(0);
}

impl Op for InventoryArgs {
    fn exec(self, ctx: &Ctx) -> Outcome {
        Outcome::result(self.run(ctx))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn code(words: &[u32]) -> Code {
        Code {
            base: 0x8001_0000,
            bytes: words.iter().flat_map(|w| w.to_le_bytes()).collect(),
            origin: "test".into(),
            gp: None,
            exe: None,
        }
    }

    const NOP: u32 = 0;
    const PROLOGUE: u32 = 0x27BD_FFE8; // addiu sp, sp, -24
    fn jal(t: u32) -> u32 {
        0x0C00_0000 | ((t >> 2) & 0x03FF_FFFF)
    }
    fn beq_zero(offset_words: i16) -> u32 {
        0x1000_0000 | (offset_words as u16 as u32)
    }

    #[test]
    fn follows_calls_then_pointers_then_prologues() {
        // 0: main calls 0x20; 0x20: leaf; 0x30: a handler only a table
        // reaches; 0x40: a prologue no one reaches; 0x50: the table.
        let mut w = vec![NOP; 0x60 / 4];
        w[0] = PROLOGUE;
        w[1] = jal(0x8001_0020);
        w[2] = NOP;
        w[3] = beq_zero(2); // to 0x18
        w[4] = NOP;
        w[5] = NOP;
        w[6] = JR_RA;
        w[7] = NOP;
        w[8] = JR_RA; // 0x20 leaf
        w[9] = NOP;
        w[12] = JR_RA; // 0x30 handler (after padding)
        w[13] = NOP;
        w[16] = PROLOGUE; // 0x40
        w[17] = JR_RA;
        w[18] = NOP;
        w[20] = 0x8001_0030; // 0x50 table
        w[21] = 0x8001_0010; // a pointer into main: a block, not a function
        let c = code(&w);
        let (lo, hi) = (c.base, c.end());
        let mut a = Analysis::new(&c, Some((lo, hi)), Box::new(|_| true));
        a.seed(0x8001_0000, "entry");
        a.drain();
        assert_eq!(a.functions.keys().copied().collect::<Vec<_>>(), [0x8001_0000, 0x8001_0020]);
        assert_eq!(a.functions[&0x8001_0000].end, 0x8001_0020, "the branch past the first jr ra keeps it going");
        let ws = words(&c.bytes, c.base);
        assert_eq!(a.pointers(&ws), 1);
        assert_eq!(a.functions[&0x8001_0030].found_by, BTreeSet::from(["pointer"]));
        assert!(a.leaders.contains(&0x8001_0010));
        assert!(!a.functions.contains_key(&0x8001_0040));
        a.prologues();
        assert!(a.functions.contains_key(&0x8001_0040));
        let (rows, rest) = a.report();
        assert_eq!(rows[0]["instructions"], 8);
        // main: 0x00; 0x10, a pointer into it; 0x14 after the beq's slot; 0x18 its target.
        assert_eq!(rows[0]["blocks"], 4);
        assert_eq!(rest["totals"]["functions"], 4);
        assert_eq!(rest["totals"]["found_only_through_pointers"], 1);
    }

    #[test]
    fn overlays_are_matched_to_the_part_of_their_file_ram_still_holds() {
        // A 12 KiB file loaded at 0x80100000, its last 4 KiB since covered.
        let file: Vec<u8> = (0..12 * 1024u32).map(|i| (i.wrapping_mul(2654435761) >> 24) as u8).collect();
        let mut ram = vec![0u8; 0x20_0000];
        ram[0x10_0000..0x10_0000 + 8 * 1024].copy_from_slice(&file[..8 * 1024]);
        let files = vec![("/OTHER.BIN".to_string(), vec![1u8; 4096]), ("/OVL.BIN".to_string(), file)];
        let (f, lo, hi) = loaded_from(&ram, 0x8010_0400, 0x8010_0800, &files).unwrap();
        assert_eq!((f["path"].as_str(), f["loads_at"].as_str()), (Some("/OVL.BIN"), Some("80100000")));
        assert_eq!((lo, hi), (0x8010_0000, 0x8010_2000));
        assert!(loaded_from(&ram, 0x8018_0000, 0x8018_0100, &files).is_none());
    }

    #[test]
    fn invalid_words_end_a_function_and_data_is_a_gap() {
        let w = vec![PROLOGUE, NOP, 0xFFFF_FFFF, 0x1234_5678, JR_RA, NOP];
        let c = code(&w);
        let mut a = Analysis::new(&c, None, Box::new(|_| true));
        a.seed(0x8001_0000, "entry");
        a.drain();
        assert_eq!(a.functions[&0x8001_0000].ends, "invalid");
        assert!(!valid(0xFFFF_FFFF) && valid(JR_RA) && valid(PROLOGUE) && !valid(0x4400_0000));
        // srl zero, zero, 16 and addiu zero, ...: valid, never emitted.
        assert!(valid(0x0000_0402) && !sensible(0x0000_0402) && !sensible(0x2400_0001) && sensible(NOP));
        assert!(!sensible(0x0000_FE00), "sll ra, zero, 24");
        // A tail jump whose delay slot does not decode is no function, nor
        // one to a function no one knows.
        let c = code(&[JR_RA, NOP, 0x0801_000C, 0xFFFF_FFFF, JR_RA, NOP, 0x0800_4000, NOP]);
        let a = Analysis::new(&c, None, Box::new(|_| true));
        assert!(!a.starts_function(0x8001_0008));
        assert!(!a.starts_function(0x8001_0018));
        // A load into the zero register syncs with hardware: code. Eight
        // nops in a row are not.
        assert!(sensible(0x8C40_0000), "lw zero, 0(v0)");
        let mut w = vec![PROLOGUE];
        w.extend([NOP; 8]);
        w.extend([JR_RA, NOP]);
        let c = code(&w);
        let mut a = Analysis::new(&c, None, Box::new(|_| true));
        a.seed(0x8001_0000, "entry");
        a.drain();
        assert_eq!((a.functions[&0x8001_0000].end, a.functions[&0x8001_0000].ends), (0x8001_0004, "invalid"));
    }

    #[test]
    fn code_a_run_ran_and_lost_is_found_in_the_file_it_came_from() {
        // The file's code ran at 0x80100000 (its byte 8 there), then was
        // loaded over: RAM holds the nop, an executable the return.
        let file: Vec<u32> = vec![1, 2, PROLOGUE, 0x0001_2345, 0x0002_3456, JR_RA, NOP, 9];
        let files = vec![("/OVL.BIN".to_string(), file.iter().flat_map(|w| w.to_le_bytes()).collect::<Vec<u8>>())];
        let payload = JR_RA.to_le_bytes().to_vec();
        let exe = Exe { path: "/MAIN.EXE".into(), boot: true, lo: 0x8010_000C, payload, entry: 0, gp: None };
        let ran: Vec<(u32, u32, u64)> =
            file[2..7].iter().enumerate().map(|(k, &w)| (0x8010_0000 + 4 * k as u32, w, 1)).collect();
        let ram = vec![0u8; 0x20_0000];
        let lost = lost_code(&ram, &ran, &[exe]);
        assert_eq!(lost.len(), 3);
        assert_eq!(placements(&lost, &files), vec![], "too short a run to search for");
        let lost: Vec<(u32, u32)> = ran.iter().map(|&(a, w, _)| (a, w)).collect();
        assert_eq!(placements(&lost, &files), vec![(0, 0x800F_FFF8)]);
    }
}
