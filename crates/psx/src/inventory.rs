//! `psx inventory`: every function of every executable on the disc and of
//! the code traced runs held in RAM, with instruction counts and basic
//! blocks, and the disc's file list. The ledger's denominator.
//!
//! Functions are found by psx's own analysis, not Ghidra's: from the entry
//! point, every `jal` in a function already found, the call targets a traced
//! run recorded (calls through pointers included), and pointer words, in the
//! executable and in RAM snapshots where it is resident, that point at the
//! start of code no function covers yet (the handlers a table dispatches to,
//! which static analysis from the entry alone misses). Code no executable
//! holds is found the same way in each moment of a traced run's code page
//! history, seeded by what had run then; a function is its start and the
//! sha256 of its bytes. Gaps no executable's function covers are listed,
//! with how many of their words decode as instructions, so they can be
//! classified.
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
    /// Its start is the trace's: a call it saw, or the entry.
    fact: bool,
}

/// Seeds the trace or the executable vouches for (`ran`: a function a fact
/// started, found again where the same bytes are). Every other seed (a call
/// or jump in code, a pointer, a prologue, where a stretch of executed code
/// starts: the trace saw it run, not where its function starts) is a
/// guess: guesses wait until the facts are followed, and never split a
/// function a fact started.
fn is_fact(how: &str) -> bool {
    matches!(how, "entry" | "trace_call" | "ran")
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
    /// Guessed seeds, waiting until `guesses` lets them in.
    later: Vec<(u32, &'static str)>,
    guessing: bool,
    /// Words the trace saw continue the code before them (run straight on
    /// into, or returned to by a call two words before) and no call into.
    continues: BTreeSet<u32>,
    /// Where a stretch of executed code may start a function: code read in
    /// the RAM it first ran with (all of it, when unset).
    fresh: Option<BTreeSet<u32>>,
    /// The functions running in this very code (all, when unset): the
    /// starts of the calls running then, and code first run then. The
    /// calls in any other function are not followed, as what they call
    /// may be other code by now.
    live: Option<BTreeSet<u32>>,
    /// Pointer targets inside functions: switch cases, block leaders.
    leaders: BTreeSet<u32>,
}

impl<'a> Analysis<'a> {
    fn new(code: &'a Code, region: Option<(u32, u32)>, allowed: Box<dyn Fn(u32) -> bool + 'a>) -> Self {
        Self {
            code,
            allowed,
            region,
            functions: BTreeMap::new(),
            queue: Vec::new(),
            later: Vec::new(),
            guessing: false,
            continues: BTreeSet::new(),
            fresh: None,
            live: None,
            leaders: BTreeSet::new(),
        }
    }

    /// Lets the guessed seeds in, once the facts are followed.
    fn guesses(&mut self) {
        self.guessing = true;
        self.drain();
    }

    fn search(&self) -> Option<(u32, u32)> {
        self.region.or_else(|| self.span())
    }

    fn seed(&mut self, at: u32, how: &'static str) {
        if !at.is_multiple_of(4) || self.code.word(at).is_none() || !(self.allowed)(at) || self.continues.contains(&at)
        {
            return;
        }
        if is_fact(how) {
            self.queue.push((at, how));
        } else {
            self.later.push((at, how));
        }
    }

    fn containing(&self, at: u32) -> Option<&Function> {
        self.functions.range(..=at).next_back().map(|(_, f)| f).filter(|f| at < f.end)
    }

    /// The function's extent from `start`: forward until a `jr ra` or tail
    /// `j` that no branch jumps past and the trace saw nothing continue,
    /// the next known function, an invalid word or the limit.
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
            // The trace saw the code after it continue it: not its end.
            if (i == JR_RA || tail) && reach <= pc + 4 && !self.continues.contains(&(pc + 8)) {
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
        loop {
            let Some((at, how)) = self.queue.pop().or_else(|| if self.guessing { self.later.pop() } else { None })
            else {
                return;
            };
            if let Some(f) = self.functions.get_mut(&at) {
                f.found_by.insert(how);
                f.fact |= is_fact(how);
                continue;
            }
            // A seed inside a function found earlier splits it (no seed is
            // a word the trace saw continue the code before it); a pointer
            // there is a block in it.
            if how == "pointer" && self.containing(at).is_some() {
                self.leaders.insert(at);
                continue;
            }
            let mut f = Function { start: at, end: at, found_by: BTreeSet::from([how]), ends: "", fact: is_fact(how) };
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
            let live = how == "executed" || self.live.as_ref().is_none_or(|l| l.contains(&at));
            let mut pc = if live { f.start } else { f.end };
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
    /// stretch of executed words this code holds starts, when it decodes
    /// to an end, a guess followed at once (after the facts, which come
    /// first). `ran` is what ran from this very code.
    fn executed(&mut self, ran: &BTreeSet<(u32, u32)>) {
        self.drain();
        self.guessing = true;
        let held: Vec<u32> = ran.iter().filter(|&&(a, w)| self.code.word(a) == Some(w)).map(|&(a, _)| a).collect();
        let starts: Vec<u32> =
            held.iter().enumerate().filter(|&(k, &a)| k == 0 || held[k - 1] + 4 != a).map(|(_, &a)| a).collect();
        for a in starts {
            if self.fresh.as_ref().is_none_or(|f| f.contains(&a))
                && self.containing(a).is_none()
                && (self.allowed)(a)
                && self.extent(a).1 != "invalid"
            {
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

/// Moments as `[from, to]` spans, in order.
type Spans = Vec<(u32, u32)>;

/// Whether `spans` hold moment `c`.
fn during(spans: &Spans, c: u32) -> bool {
    let k = spans.partition_point(|s| s.1 < c);
    spans.get(k).is_some_and(|s| s.0 <= c)
}

/// A traced run's timeline: each code page copy (code.bin, whole), the
/// words run from it (executed.bin), with the moment each first ran (a
/// moment is the number of the latest copy then), and the call targets in
/// it (calls.json), with the moments a call to each ran in, from the call
/// until it returned. At any moment, the copy each page held is RAM's code
/// then.
struct Timeline {
    copies: Vec<(u64, u32, Vec<u8>)>,
    /// Per copy: `(address, instruction, moment, continues the code before
    /// it)`.
    ran: Vec<Vec<(u32, u32, u32, bool)>>,
    /// Per copy: `(address, moments)`.
    calls: Vec<Vec<(u32, Spans)>>,
}

/// A run's timeline: none for a run without a trace; an error for one
/// recorded before run --trace kept each code page's history and the
/// moments each call ran in.
fn timeline(run: &Path) -> crate::Result<Option<Timeline>> {
    let path = run.join("executed.bin");
    let e = match std::fs::read(&path) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(Error::io(&path, e)),
    };
    let again = || {
        Error::invalid(format!(
            "{}: recorded before run --trace kept each code page's history and when code ran; record it again with run --trace",
            run.display()
        ))
    };
    let body = e.strip_prefix(b"PSXEXEC4").ok_or_else(again)?;
    let copies = code_pages(run).ok_or_else(again)?;
    let mut ran = vec![Vec::new(); copies.len()];
    for r in body.as_chunks::<28>().0 {
        let u = |a: usize| u32::from_le_bytes(r[a..a + 4].try_into().unwrap());
        if let Some(v) = ran.get_mut(u(0) as usize) {
            v.push((u(4), u(8), u(12), u(16) & 1 != 0));
        }
    }
    let mut calls = vec![Vec::new(); copies.len()];
    let text = std::fs::read_to_string(run.join("calls.json")).unwrap_or_default();
    let v: Value = serde_json::from_str(&text).unwrap_or_default();
    for c in v.as_array().into_iter().flatten() {
        let (Some(k), Some(a)) = (c["copy"].as_u64(), c["address"].as_u64()) else { continue };
        if c.get("moments").is_none() {
            return Err(again());
        }
        let spans = c["moments"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|s| Some((s[0].as_u64()? as u32, s[1].as_u64()? as u32)))
            .collect();
        if let Some(v) = calls.get_mut(k as usize) {
            v.push((0x8000_0000 | (a as u32 & 0x1F_FFFF), spans));
        }
    }
    Ok(Some(Timeline { copies, ran, calls }))
}

impl Timeline {
    /// Each word the trace saw continue the code before it, with the words
    /// that say so as its copy holds them (the word, and the two before it
    /// when in the copy): the same bytes there continue it wherever they
    /// are.
    fn continuing(&self) -> Vec<(u32, [Option<u32>; 3])> {
        let mut out = Vec::new();
        for (c, (_, page, bytes)) in self.copies.iter().enumerate() {
            let word = |x: u32| {
                let k = x.checked_sub(*page)? as usize;
                bytes.get(k..k + 4).map(|b| u32::from_le_bytes(b.try_into().unwrap()))
            };
            for r in self.ran[c].iter().filter(|r| r.3) {
                out.push((r.0, [word(r.0.wrapping_sub(8)), word(r.0.wrapping_sub(4)), word(r.0)]));
            }
        }
        out
    }
}

/// Whether code `word` reads holds the words a continuing fact was seen
/// with.
fn holds(word: impl Fn(u32) -> Option<u32>, x: u32, seen: &[Option<u32>; 3]) -> bool {
    [x.wrapping_sub(8), x.wrapping_sub(4), x].iter().zip(seen).all(|(&a, w)| w.is_none() || word(a) == *w)
}

/// A traced run's code page copies (code.bin), each whole: `(frame, KSEG0
/// page, bytes)`. None for a code.bin from before page history.
fn code_pages(run: &Path) -> Option<Vec<(u64, u32, Vec<u8>)>> {
    let b = std::fs::read(run.join("code.bin")).ok()?;
    let mut rest = b.strip_prefix(b"PSXCODE2")?;
    let u = |r: &[u8], a: usize| u32::from_le_bytes(r[a..a + 4].try_into().unwrap());
    let mut out: Vec<(u64, u32, Vec<u8>)> = Vec::new();
    // A later copy of a page is the words changed since the copy before.
    while rest.len() >= 20 {
        let frame = u64::from_le_bytes(rest[0..8].try_into().unwrap());
        let (addr, before, n) = (u(rest, 8), u(rest, 12), u(rest, 16) as usize);
        let body = rest.get(20..20 + n)?;
        let bytes = match out.get(before as usize) {
            None => body.to_vec(),
            Some((_, _, b)) => {
                let mut b = b.clone();
                for r in body.as_chunks::<8>().0 {
                    let o = u(r, 0) as usize;
                    b.get_mut(o..o + 4)?.copy_from_slice(&r[4..8]);
                }
                b
            }
        };
        out.push((frame, addr, bytes));
        rest = &rest[20 + n..];
    }
    Some(out)
}

const PAGE: u32 = 0x1000;

/// The executable whose code a page holds: at least half the page's words
/// in its range are its.
fn resident(exes: &[Exe], page: u32, bytes: &[u8]) -> Option<usize> {
    exes.iter().position(|e| {
        let (lo, hi) = (page.max(e.lo), (page + bytes.len() as u32).min(e.hi()));
        if lo >= hi {
            return false;
        }
        let same = (lo..hi)
            .step_by(4)
            .filter(|&a| bytes[(a - page) as usize..][..4] == e.payload[(a - e.lo) as usize..][..4])
            .count();
        2 * same as u32 >= (hi - lo) / 4
    })
}

/// RAM at one moment: its bytes (all ones where no code is known) and,
/// per page, whether code is known there and the executable it holds.
struct Moment {
    ram: Vec<u8>,
    known: Vec<bool>,
    owner: Vec<Option<usize>>,
    /// Words that continue the code before them (see `Analysis::continues`).
    continues: BTreeSet<u32>,
    /// Words that first ran at this moment (see `Analysis::fresh`).
    fresh: Option<BTreeSet<u32>>,
    /// Calls running at this moment (see `Analysis::live`).
    live: Option<BTreeSet<u32>>,
}

impl Moment {
    fn word(&self, x: u32) -> Option<u32> {
        let o = (x & 0x1F_FFFF) as usize;
        self.ram.get(o..o + 4).map(|b| u32::from_le_bytes(b.try_into().unwrap()))
    }

    /// `x` holds the code of the executable its page holds.
    fn held(&self, exes: &[Exe], x: u32) -> bool {
        let Some(i) = self.owner.get(((x & 0x1F_FFFF) / PAGE) as usize).copied().flatten() else { return false };
        let (e, o) = (&exes[i], (x & 0x1F_FFFF) as usize);
        x >= e.lo && x < e.hi() && {
            let k = (x - e.lo) as usize;
            e.payload.get(k..k + 4) == self.ram.get(o..o + 4)
        }
    }

    /// Where a moment's own code may start: game RAM (below 0x80010000 is
    /// the BIOS kernel's) that holds known code and not an executable's.
    fn allowed(&self, exes: &[Exe], x: u32) -> bool {
        (0x8001_0000..0x8020_0000).contains(&x) && self.known[((x & 0x1F_FFFF) / PAGE) as usize] && !self.held(exes, x)
    }
}

/// One moment's functions, found as in an executable: seeded by `seeds`
/// and the words `ran` then, then pointers into code and prologues around
/// each cluster of what they found. Per cluster: its extent and the rows
/// of its functions.
fn moment_functions(
    m: &Moment,
    exes: &[Exe],
    seeds: &[(u32, &'static str)],
    ran: &BTreeSet<(u32, u32)>,
) -> Vec<(u32, u32, Vec<Value>)> {
    let code = Code { base: 0x8000_0000, bytes: m.ram.clone(), origin: String::new(), gp: None, exe: None };
    let mut a = Analysis::new(&code, None, Box::new(|x| m.allowed(exes, x)));
    a.continues = m.continues.clone();
    a.fresh = m.fresh.clone();
    a.live = m.live.clone();
    for &(t, how) in seeds {
        a.seed(t, how);
    }
    a.drain();
    a.executed(ran);
    a.guesses();
    // Words that could point at game code: the only ones pointers() reads.
    let pointers: Vec<(u32, u32)> = (0..m.ram.len() as u32 / 4)
        .filter(|&k| m.known[(4 * k / PAGE) as usize])
        .map(|k| (0x8000_0000 + 4 * k, m.word(0x8000_0000 + 4 * k).unwrap_or(0)))
        .filter(|&(_, w)| (0x8001_0000..0x8020_0000).contains(&w) && w % 4 == 0)
        .collect();
    for _ in 0..4 {
        let before = a.functions.len();
        for (lo, hi) in clusters(&a) {
            a.region = Some((lo, hi));
            while a.pointers(&pointers) > 0 {}
            a.prologues();
        }
        if a.functions.len() == before {
            break;
        }
    }
    clusters(&a)
        .into_iter()
        .map(|(lo, hi)| {
            let (rows, _) = a.report_in(lo, hi, &|_| true);
            (lo, hi, rows)
        })
        .collect()
}

/// A function: its start and the sha256 of its bytes.
type Key = (u32, String);
/// A disc file, the address it was loaded at and the extent of it RAM held.
type FileAt = (String, u32, u32, u32);
/// loaded_from's answer: the file, and the extent of it RAM holds.
type Loaded = (Value, u32, u32);

/// A function found in RAM: its row, bytes, the file naming it and the
/// name of the code it was first found in.
struct Found {
    row: Value,
    bytes: Vec<u8>,
    /// The file, the address it was loaded at and the extent RAM held of it.
    file: Option<FileAt>,
    first: String,
}

impl Found {
    /// Started by a fact in some moment.
    fn fact(&self) -> bool {
        self.row["found_by"].as_array().into_iter().flatten().any(|h| h.as_str().is_some_and(is_fact))
    }
}

/// Each moment's functions, by start and sha256 (the same function in
/// many moments, runs or snapshots is one; other code loaded at its
/// address is another), named by the file their cluster was loaded from
/// when their bytes are that file's.
struct Finder<'a> {
    exes: &'a [Exe],
    /// Files code is named after: the disc's, the executables' and the BIOS ROMs.
    files: &'a [(String, Vec<u8>)],
    /// The executables' functions: `(start, end, calls in it)`.
    exe_functions: Vec<Vec<(u32, u32, Vec<u32>)>>,
    /// The executables' functions by start and sha256, not listed again.
    exe_keys: BTreeSet<(u32, String)>,
    found: BTreeMap<(u32, String), Found>,
    /// Which file a cluster's first 64 bytes were loaded from.
    named: BTreeMap<(u32, Vec<u8>), Option<Loaded>>,
    /// Functions found before, by start and bytes: a start in every moment
    /// holding the same bytes there, so the same code is split the same way
    /// whichever moment it is found in.
    known: Vec<(u32, Vec<u8>, bool)>,
}

impl Finder<'_> {
    /// Calls from the executables' functions a moment holds whole.
    fn exe_calls(&self, m: &Moment) -> Vec<(u32, &'static str)> {
        let mut out = Vec::new();
        for fs in &self.exe_functions {
            for (s, e, calls) in fs {
                if (*s..*e).step_by(4).all(|x| m.held(self.exes, x)) {
                    out.extend(calls.iter().map(|&t| (t, "call_from_executable")));
                }
            }
        }
        out
    }

    /// Finds a moment's functions and keeps the new ones; `when` names
    /// code no file holds.
    fn take(&mut self, m: &Moment, seeds: &[(u32, &'static str)], ran: &BTreeSet<(u32, u32)>, when: &str) {
        let mut seeds = seeds.to_vec();
        for (s, b, fact) in &self.known {
            let o = (s & 0x1F_FFFF) as usize;
            if m.ram.get(o..o + b.len()) == Some(&b[..]) && m.allowed(self.exes, *s) {
                seeds.push((*s, if *fact { "ran" } else { "same_code" }));
            }
        }
        for (lo, hi, rows) in moment_functions(m, self.exes, &seeds, ran) {
            let mut file = None;
            for row in rows {
                let (s, e) = (parse_hex(&row["entry"]), parse_hex(&row["end"]));
                let bytes = m.ram[(s & 0x1F_FFFF) as usize..(e & 0x1F_FFFF) as usize].to_vec();
                let key = (s, crate::publish::sha256(&bytes));
                if self.exe_keys.contains(&key) {
                    continue;
                }
                if let Some(f) = self.found.get_mut(&key) {
                    let mut by: BTreeSet<String> = f.row["found_by"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .chain(row["found_by"].as_array().into_iter().flatten())
                        .filter_map(|v| v.as_str().map(String::from))
                        .collect();
                    f.row["found_by"] = json!(std::mem::take(&mut by));
                    continue;
                }
                // The file the cluster was loaded from, once per cluster.
                let file = file.get_or_insert_with(|| {
                    let o = (lo & 0x1F_FFFF) as usize;
                    let head = m.ram[o..(o + 64).min((hi & 0x1F_FFFF) as usize)].to_vec();
                    self.named.entry((lo, head)).or_insert_with(|| loaded_from(&m.ram, lo, hi, self.files)).clone()
                });
                let file = file.as_ref().and_then(|(f, flo, fhi)| {
                    let path = f["path"].as_str()?;
                    let at = parse_hex(&f["loads_at"]);
                    let (_, b) = self.files.iter().find(|c| c.0 == path)?;
                    let k = s.checked_sub(at)? as usize;
                    (b.get(k..k + bytes.len()) == Some(&bytes[..])).then(|| (path.to_string(), at, *flo, *fhi))
                });
                let first = format!("code at {}, {when}", hex(lo));
                self.found.insert(key, Found { row, bytes, file, first });
            }
        }
    }
}

/// Finds the functions of each moment of a run's timeline (after each
/// copy), seeded by the calls into the copies it holds and the code run
/// from them at that moment.
fn take_timeline(f: &mut Finder, t: &Timeline) {
    let owner: Vec<Option<usize>> = t.copies.iter().map(|(_, p, b)| resident(f.exes, *p, b)).collect();
    let continuing = t.continuing();
    let mut now: BTreeMap<u32, usize> = BTreeMap::new();
    for (c, (frame, page, _)) in t.copies.iter().enumerate() {
        now.insert(*page, c);
        let mut m = Moment {
            ram: vec![0xFF; 0x20_0000],
            known: vec![false; 0x200],
            owner: vec![None; 0x200],
            continues: BTreeSet::new(),
            fresh: Some(BTreeSet::new()),
            live: Some(BTreeSet::new()),
        };
        let mut called = BTreeSet::new();
        let (mut ran, mut seeds) = (BTreeSet::new(), Vec::new());
        // The calls running at this moment ran with RAM as it is now, and
        // only theirs are followed here: other code a copy holds may run
        // with other code beside it, in other moments. What ran from a copy
        // is a fact about its bytes whenever it ran, and code seen to
        // continue the code before it does wherever the same bytes are.
        // Where code nothing called starts is read only in the RAM it first
        // ran with: the moment it first ran.
        let c = c as u32;
        for &k in now.values() {
            let o = (t.copies[k].1 & 0x1F_FFFF) as usize;
            m.ram[o..o + PAGE as usize].copy_from_slice(&t.copies[k].2);
            m.known[o / PAGE as usize] = true;
            m.owner[o / PAGE as usize] = owner[k];
            ran.extend(t.ran[k].iter().map(|r| (r.0, r.1)));
            m.fresh.get_or_insert_default().extend(t.ran[k].iter().filter(|r| r.2 == c).map(|r| r.0));
            called.extend(t.calls[k].iter().filter(|r| during(&r.1, c)).map(|r| r.0));
        }
        m.live = Some(called.clone());
        let known = |x: u32| m.known.get(((x & 0x1F_FFFF) / PAGE) as usize) == Some(&true);
        let continues = continuing
            .iter()
            .filter(|(x, seen)| known(*x) && holds(|y| known(y).then(|| m.word(y)).flatten(), *x, seen))
            .map(|(x, _)| *x)
            .collect();
        m.continues = continues;
        seeds.extend(called.iter().map(|&x| (x, "trace_call")));
        m.continues.retain(|x| !called.contains(x));
        seeds.extend(f.exe_calls(&m));
        f.take(&m, &seeds, &ran, &format!("frame {frame}"));
    }
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

        // The runs: each traced run's timeline, and each run's RAM at its end.
        // The runs read, for protocol to tell a corpus run the inventory has
        // not seen (or that was recorded again) from one it has.
        let mut inputs = Vec::new();
        let mut runs: Vec<(PathBuf, Option<Timeline>, Vec<u8>)> = Vec::new();
        for r in &self.ram {
            let dir = at(r);
            let tl = timeline(&dir)?;
            let mut ram = crate::ram::load(&dir)?.bytes;
            ram.resize(0x20_0000, 0);
            let executed = std::fs::read(dir.join("executed.bin")).ok().map(|b| crate::publish::sha256(&b));
            inputs.push(json!({"run": dir, "executed_sha256": executed}));
            runs.push((dir, tl, ram));
        }
        // The executable each copy holds.
        let owners: Vec<Vec<Option<usize>>> = runs
            .iter()
            .map(|(_, tl, _)| tl.iter().flat_map(|t| &t.copies).map(|(_, p, b)| resident(&exes, *p, b)).collect())
            .collect();

        let mut executables = Vec::new();
        let mut exe_functions = Vec::new();
        let mut exe_keys = BTreeSet::new();
        let mut totals = (0u64, 0u64, 0u64);
        for (i, exe) in exes.iter().enumerate() {
            let code =
                Code { base: exe.lo, bytes: exe.payload.clone(), origin: exe.path.clone(), gp: exe.gp, exe: None };
            let (lo, hi) = (exe.lo, exe.hi());
            let mut a = Analysis::new(&code, Some((lo, hi)), Box::new(move |x| x >= lo && x < hi));
            // What ran and was called in the pages holding its code.
            let (mut ran, mut called) = (BTreeSet::new(), BTreeSet::new());
            for ((_, tl, _), owner) in runs.iter().zip(&owners) {
                let Some(tl) = tl else { continue };
                for (c, (_, page, bytes)) in tl.copies.iter().enumerate().filter(|&(c, _)| owner[c] == Some(i)) {
                    ran.extend(tl.ran[c].iter().filter(|r| code.word(r.0) == Some(r.1)).map(|r| (r.0, r.1)));
                    for &(x, _) in &tl.calls[c] {
                        let k = (x - page) as usize;
                        if bytes.get(k..k + 4).map(|b| u32::from_le_bytes(b.try_into().unwrap())) == code.word(x) {
                            called.insert(x);
                        }
                    }
                }
            }
            for (_, tl, _) in &runs {
                let Some(tl) = tl else { continue };
                a.continues.extend(
                    tl.continuing().iter().filter(|(x, seen)| holds(|y| code.word(y), *x, seen)).map(|(x, _)| *x),
                );
            }
            a.continues.retain(|x| !called.contains(x));
            a.seed(exe.entry, "entry");
            for &x in &called {
                a.seed(x, "trace_call");
            }
            a.drain();
            a.executed(&ran);
            a.guesses();
            let held: Vec<&PathBuf> =
                runs.iter().filter(|(_, _, ram)| exe.resident(ram) >= 0.9).map(|(d, _, _)| d).collect();
            let mut pointer_words = words(&exe.payload, exe.lo);
            for (_, _, ram) in runs.iter().filter(|(_, _, ram)| exe.resident(ram) >= 0.9) {
                pointer_words.extend(words(ram, 0x8000_0000));
            }
            // Until no new function is found: each round's functions may
            // call more, and widen the span pointers may land in.
            while a.pointers(&pointer_words) > 0 {}
            a.prologues();
            while a.pointers(&pointer_words) > 0 {}
            let (functions, rest) = a.report();
            add(&mut totals, &rest["totals"]);
            exe_functions.push(
                a.functions
                    .values()
                    .map(|f| {
                        let calls = (f.start..f.end)
                            .step_by(4)
                            .filter_map(|pc| code.word(pc).filter(|w| w >> 26 == 3).map(|w| jump_target(pc, w)))
                            .collect();
                        (f.start, f.end, calls)
                    })
                    .collect::<Vec<_>>(),
            );
            exe_keys.extend(a.functions.values().map(|f| {
                (f.start, crate::publish::sha256(&exe.payload[(f.start - lo) as usize..(f.end - lo) as usize]))
            }));
            executables.push(json!({
                "path": exe.path, "boot": exe.boot, "loads_at": hex(exe.lo), "end": hex(exe.hi()),
                "entry": hex(exe.entry),
                "snapshots": held,
                "totals": rest["totals"], "gaps": rest["gaps"], "functions": functions,
            }));
        }

        // Code no executable holds, found in each moment of each run's
        // timeline, then in each run's RAM at its end (seeded by the
        // functions found in it still there). Named by the files code is
        // loaded from: the executables' too.
        for e in &exes {
            contents.push((e.path.clone(), e.payload.clone()));
        }
        let mut finder = Finder {
            exes: &exes,
            files: &contents,
            exe_functions,
            exe_keys,
            found: BTreeMap::new(),
            named: BTreeMap::new(),
            known: Vec::new(),
        };
        // Again until nothing changes, each time with every start found for
        // the code each moment holds, in whichever moment (a call there may
        // come only in another): the same code is split the same way.
        let mut before: Option<BTreeSet<Key>> = None;
        for _ in 0..8 {
            let now: BTreeSet<Key> = finder.found.keys().cloned().collect();
            if before.as_ref() == Some(&now) {
                break;
            }
            before = Some(now);
            finder.known = finder.found.iter().map(|((s, _), f)| (*s, f.bytes.clone(), f.fact())).collect();
            finder.found.clear();
            for (_, tl, _) in &runs {
                if let Some(tl) = tl {
                    take_timeline(&mut finder, tl);
                }
            }
            for (dir, tl, ram) in &runs {
                let pages = ram.len() / PAGE as usize;
                // A traced run's RAM at its end holds its last moment's code,
                // and the calls running then.
                let live = tl.as_ref().map(|tl| {
                    let last = tl.copies.len().saturating_sub(1) as u32;
                    let word = |b: &[u8], o: usize| b.get(o..o + 4).map(|w| w.to_vec());
                    let mut live = BTreeSet::new();
                    for (k, (_, page, bytes)) in tl.copies.iter().enumerate() {
                        for (x, _) in tl.calls[k].iter().filter(|r| during(&r.1, last)) {
                            if word(bytes, (x - page) as usize) == word(ram, (x & 0x1F_FFFF) as usize) {
                                live.insert(*x);
                            }
                        }
                    }
                    live
                });
                let owner = (0..pages)
                    .map(|p| resident(&exes, 0x8000_0000 + p as u32 * PAGE, &ram[p * PAGE as usize..][..PAGE as usize]))
                    .collect();
                let m = Moment {
                    ram: ram.clone(),
                    known: vec![true; pages],
                    owner,
                    continues: BTreeSet::new(),
                    fresh: None,
                    live,
                };
                let seeds = finder.exe_calls(&m);
                // The functions found in the run's moments that its RAM still holds.
                let mut more: Vec<(u32, Vec<u8>, bool)> =
                    finder.found.iter().map(|((s, _), f)| (*s, f.bytes.clone(), f.fact())).collect();
                more.retain(|x| !finder.known.contains(x));
                finder.known.extend(more);
                let name = dir.file_name().unwrap_or_default().to_string_lossy();
                finder.take(&m, &seeds, &BTreeSet::new(), &format!("end of run {name}"));
            }
        }
        let Finder { mut found, exe_keys, .. } = finder;
        // A function cut off where its moment knew no code is the start of
        // one found whole in another.
        let cut: Vec<Key> = found
            .iter()
            .filter(|(k, f)| {
                f.row["ends"] == "invalid"
                    && found
                        .range((k.0, String::new())..(k.0 + 1, String::new()))
                        .any(|(j, g)| j != *k && g.bytes.len() > f.bytes.len() && g.bytes.starts_with(&f.bytes))
            })
            .map(|(k, _)| k.clone())
            .collect();
        for k in cut {
            found.remove(&k);
        }

        // Each file code was loaded from is analysed over the extent RAM
        // held of it, for the functions no run reached.
        let mut groups: BTreeMap<String, (Option<FileAt>, Vec<Key>)> = BTreeMap::new();
        for (key, f) in &found {
            let name = match &f.file {
                Some((p, at, _, _)) => format!("{} {p}", hex(*at)),
                None => f.first.clone(),
            };
            let g = groups.entry(name).or_insert_with(|| (f.file.clone(), Vec::new()));
            if let (Some(g), Some(f)) = (&mut g.0, &f.file) {
                (g.2, g.3) = (g.2.min(f.2), g.3.max(f.3));
            }
            g.1.push(key.clone());
        }
        for (name, (file, keys)) in groups.iter_mut() {
            let Some((path, _, lo, hi)) = file.clone() else { continue };
            if path.starts_with(BIOS) || exes.iter().any(|e| e.path == path) {
                continue;
            }
            let Some(bytes) = contents.iter().find(|c| c.0 == path).map(|c| &c.1) else { continue };
            let at = file.as_ref().map(|f| f.1).unwrap_or(lo);
            let span = &bytes[(lo - at) as usize..((hi - at) as usize).min(bytes.len())];
            let code = Code { base: lo, bytes: span.to_vec(), origin: path.clone(), gp: None, exe: None };
            let mut a = Analysis::new(&code, Some((lo, hi)), Box::new(move |x| x >= lo.max(0x8001_0000) && x < hi));
            let starts: BTreeSet<u32> = keys.iter().map(|k| k.0).collect();
            for k in keys.iter() {
                a.seed(k.0, if found[k].fact() { "ran" } else { "same_code" });
            }
            a.drain();
            a.guesses();
            let ws = words(&code.bytes, lo);
            while a.pointers(&ws) > 0 {}
            a.prologues();
            while a.pointers(&ws) > 0 {}
            for row in a.report().0 {
                let (s, e) = (parse_hex(&row["entry"]), parse_hex(&row["end"]));
                if starts.contains(&s) {
                    continue;
                }
                let b = code.bytes[(s - lo) as usize..(e - lo) as usize].to_vec();
                let key = (s, crate::publish::sha256(&b));
                if found.contains_key(&key) || exe_keys.contains(&key) {
                    continue;
                }
                found.insert(key.clone(), Found { row, bytes: b, file: file.clone(), first: name.clone() });
                keys.push(key);
            }
        }

        // Code from a BIOS ROM (the shell it copies into RAM at power-on) is
        // listed apart, under bios_code.
        let mut overlays: Vec<Value> = Vec::new();
        let mut bios_code: Vec<Value> = Vec::new();
        for (name, (file, mut keys)) in groups {
            keys.sort();
            let (mut instructions, mut blocks, mut pointers) = (0u64, 0u64, 0u64);
            let mut functions = Vec::new();
            for key in &keys {
                let f = &found[key];
                let mut row = f.row.clone();
                row["sha256"] = json!(key.1);
                instructions += row["instructions"].as_u64().unwrap_or(0);
                blocks += row["blocks"].as_u64().unwrap_or(0);
                pointers += row["found_by"].as_array().is_some_and(|b| b.iter().all(|h| h == "pointer")) as u64;
                // Its bytes, for coverage to tell it from code loaded over it.
                let keep = overlay_bytes(&ctx.root, &key.1);
                if let Some(d) = keep.parent() {
                    std::fs::create_dir_all(d).map_err(|e| Error::io(d, e))?;
                }
                std::fs::write(&keep, &f.bytes).map_err(|e| Error::io(&keep, e))?;
                functions.push(row);
            }
            let start = keys.first().map(|k| k.0).unwrap_or(0);
            let end = keys.iter().map(|k| parse_hex(&found[k].row["end"])).max().unwrap_or(start);
            let t = json!({"functions": functions.len(), "instructions": instructions, "blocks": blocks,
                "found_only_through_pointers": pointers});
            let entry = json!({
                "name": name, "start": hex(start), "end": hex(end),
                "file": file.as_ref().map(|(p, at, lo, hi)| json!({"path": p, "loads_at": hex(*at),
                    "in_ram": {"start": hex(*lo), "end": hex(*hi)}})),
                "totals": t, "functions": functions,
            });
            if file.as_ref().is_some_and(|f| f.0.starts_with(BIOS)) {
                bios_code.push(entry);
            } else {
                add(&mut totals, &t);
                overlays.push(entry);
            }
        }
        overlays.sort_by_key(|o| parse_hex(&o["start"]));
        bios_code.sort_by_key(|o| parse_hex(&o["start"]));

        let out = self.out.as_ref().map(|p| at(p)).unwrap_or_else(|| default_out(ctx));
        let summary = json!({
            "functions": totals.0, "instructions": totals.1, "blocks": totals.2,
            "executables": exes.len(), "overlays": overlays.len(),
            "disc_files": files.len(),
        });
        let previous = std::fs::read_to_string(&out)
            .ok()
            .and_then(|t| serde_json::from_str::<Value>(&t).ok())
            .map(|v| v["totals"].clone());
        let inventory = json!({
            "inventory": 2, "made": crate::publish::utc_now(), "psx": crate::context::version(),
            "media": media, "boot": boot, "totals": summary,
            "how": "functions from each executable's entry, calls in functions found, traced call targets and pointers into code (in the executable and in snapshots holding it); code no executable holds is found in each moment of each traced run's code pages (RAM's code as it was then, seeded by what ran and was called in that moment, with calls followed only from code that ran in it) and in each run's RAM at its end; a function is its start and the sha256 of its bytes, listed once however many moments held it; overlays group them by the file their bytes are loaded from (analysed over the extent RAM held of it), else by the code they were first found in; code from the BIOS ROM, the shell, is listed under bios_code instead; gaps are what no executable's function covers, with how many words decode as instructions",
            "executables": executables, "overlays": overlays, "bios_code": bios_code, "disc": {"files": files},
            "ram": inputs,
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
        // The call in it is a guess, followed once the facts are.
        assert_eq!(a.functions.keys().copied().collect::<Vec<_>>(), [0x8001_0000]);
        a.guesses();
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
    fn page_history_is_read_whole_and_old_runs_are_refused() {
        let dir = std::env::temp_dir().join(format!("psx-page-history-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut code = b"PSXCODE2".to_vec();
        let mut copy = |frame: u64, before: u32, body: &[u8]| {
            code.extend(frame.to_le_bytes());
            code.extend(0x8010_0000u32.to_le_bytes());
            code.extend(before.to_le_bytes());
            code.extend((body.len() as u32).to_le_bytes());
            code.extend(body);
        };
        let changed = |o: u32, w: u32| [o.to_le_bytes(), w.to_le_bytes()].concat();
        copy(10, u32::MAX, &[0; 0x1000]);
        copy(11, 0, &changed(0x100, 7));
        copy(12, 1, &changed(8, 0xBBBB));
        std::fs::write(dir.join("code.bin"), code).unwrap();
        let mut executed = b"PSXEXEC4".to_vec();
        for (c, w, continues) in [(0u32, 0u32, 0), (2, 0xBBBB, 1), (u32::MAX, 5, 0)] {
            for v in [c, 0x8010_0008, w, c.min(2), continues] {
                executed.extend(v.to_le_bytes());
            }
            executed.extend(1u64.to_le_bytes());
        }
        std::fs::write(dir.join("executed.bin"), &executed).unwrap();
        std::fs::write(
            dir.join("calls.json"),
            r#"[{"address": 2148532232, "copy": 1, "moments": [[2, 2]]}, {"address": 0, "copy": null}]"#,
        )
        .unwrap();
        let t = timeline(&dir).unwrap().unwrap();
        assert_eq!(t.copies.len(), 3);
        assert_eq!((t.copies[2].2[0x100], &t.copies[2].2[8..12]), (7, &0xBBBBu32.to_le_bytes()[..]));
        assert_eq!(t.ran, [vec![(0x8010_0008, 0, 0, false)], vec![], vec![(0x8010_0008, 0xBBBB, 2, true)]]);
        assert_eq!(t.calls, [vec![], vec![(0x8010_0008, vec![(2, 2)])], vec![]]);
        std::fs::write(dir.join("code.bin"), b"PSXCODE1").unwrap();
        let old = timeline(&dir).err().map(|e| e.to_string()).unwrap_or_default();
        assert!(old.contains("record it again"), "{old}");
        std::fs::remove_file(dir.join("code.bin")).unwrap();
        std::fs::remove_file(dir.join("executed.bin")).unwrap();
        assert!(timeline(&dir).unwrap().is_none(), "a run without a trace is a snapshot");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn code_loaded_over_code_is_another_function_and_old_calls_stay_with_the_old_code() {
        const ADDIU: u32 = 0x2402_0001; // addiu v0, zero, 1
        let page = |ws: &[u32]| {
            let mut b = vec![0u8; 0x1000];
            for (k, w) in ws.iter().enumerate() {
                b[4 * k..4 * k + 4].copy_from_slice(&w.to_le_bytes());
            }
            b
        };
        // First a function at 0 and one at 0x10, called; then code loaded
        // over both, one function from 0 to 0x20; then a variable beside it.
        let first = page(&[PROLOGUE, NOP, JR_RA, NOP, JR_RA, NOP]);
        let second = page(&[PROLOGUE, ADDIU, ADDIU, ADDIU, ADDIU, ADDIU, JR_RA, NOP]);
        let mut third = second.clone();
        third[0x800] = 9;
        let base = 0x8010_0000;
        let ran = |n: u32, ws: &[u8], m: u32| -> Vec<(u32, u32, u32, bool)> {
            (0..n)
                .map(|k| (base + 4 * k, u32::from_le_bytes(ws[4 * k as usize..][..4].try_into().unwrap()), m, false))
                .collect()
        };
        let t = Timeline {
            ran: vec![[ran(4, &first, 0), ran(6, &first, 0)[4..].to_vec()].concat(), ran(8, &second, 1), vec![]],
            copies: vec![(10, base, first), (20, base, second), (30, base, third)],
            calls: vec![vec![(base + 0x10, vec![(0, 0)])], vec![], vec![]],
        };
        let mut f = Finder {
            exes: &[],
            files: &[],
            exe_functions: Vec::new(),
            exe_keys: BTreeSet::new(),
            found: BTreeMap::new(),
            named: BTreeMap::new(),
            known: Vec::new(),
        };
        take_timeline(&mut f, &t);
        let got: Vec<(u32, u32, String)> =
            f.found.values().map(|x| (parse_hex(&x.row["entry"]), parse_hex(&x.row["end"]), x.first.clone())).collect();
        assert_eq!(
            got.iter().map(|g| (g.0, g.1)).collect::<BTreeSet<_>>(),
            BTreeSet::from([(base, base + 0x10), (base + 0x10, base + 0x18), (base, base + 0x20)]),
            "{got:?}"
        );
        assert!(got.iter().any(|g| g.1 == base + 0x20 && g.2 == "code at 80100000, frame 20"));
    }

    #[test]
    fn code_is_seeded_only_from_the_moment_it_first_ran() {
        // A function starts at the end of one page and runs on into the
        // next. The second page's copy is taken first (other code ran in it);
        // the first page is reloaded later, and only then does the
        // function run, across both.
        let (a, b) = (0x8010_0000u32, 0x8010_1000u32);
        let mut page_a = vec![0xFFu8; 0x1000];
        page_a[0xFF8..0xFFC].copy_from_slice(&PROLOGUE.to_le_bytes());
        page_a[0xFFC..].copy_from_slice(&NOP.to_le_bytes());
        let mut page_b = vec![0u8; 0x1000];
        for (k, w) in [NOP, JR_RA, NOP].iter().enumerate() {
            page_b[4 * k..4 * k + 4].copy_from_slice(&w.to_le_bytes());
        }
        let t = Timeline {
            copies: vec![(10, b, page_b), (20, a, page_a)],
            ran: vec![
                vec![(b, NOP, 1, true), (b + 4, JR_RA, 1, true), (b + 8, NOP, 1, true)],
                vec![(a + 0xFF8, PROLOGUE, 1, false), (a + 0xFFC, NOP, 1, true)],
            ],
            calls: vec![vec![], vec![(a + 0xFF8, vec![(1, 1)])]],
        };
        let mut f = Finder {
            exes: &[],
            files: &[],
            exe_functions: Vec::new(),
            exe_keys: BTreeSet::new(),
            found: BTreeMap::new(),
            named: BTreeMap::new(),
            known: Vec::new(),
        };
        take_timeline(&mut f, &t);
        let got: Vec<(u32, u32)> =
            f.found.values().map(|x| (parse_hex(&x.row["entry"]), parse_hex(&x.row["end"]))).collect();
        assert_eq!(got, [(a + 0xFF8, b + 0xC)], "no function starts at the second page");
    }

    #[test]
    fn code_is_followed_only_in_the_moments_it_ran_in() {
        // A function at the end of one page runs on into the next, and is
        // called with both pages loaded. Later other code is loaded into
        // the second page, beside the same first page, and nothing runs
        // the function then: it is not read again across the new code.
        let (a, b) = (0x8010_0000u32, 0x8010_1000u32);
        let page = |at: usize, ws: &[u32]| {
            let mut p = vec![0u8; 0x1000];
            for (k, w) in ws.iter().enumerate() {
                p[at + 4 * k..at + 4 * k + 4].copy_from_slice(&w.to_le_bytes());
            }
            p
        };
        const ADDIU: u32 = 0x2402_0001;
        let t = Timeline {
            copies: vec![
                (10, a, page(0xFF8, &[PROLOGUE, NOP])),
                (11, b, page(0, &[NOP, JR_RA, NOP])),
                (20, b, page(0, &[JR_RA, NOP, ADDIU, JR_RA, NOP])),
            ],
            ran: vec![
                vec![(a + 0xFF8, PROLOGUE, 1, false), (a + 0xFFC, NOP, 1, true)],
                vec![(b, NOP, 1, true), (b + 4, JR_RA, 1, true), (b + 8, NOP, 1, true)],
                vec![],
            ],
            calls: vec![vec![(a + 0xFF8, vec![(1, 1)])], vec![], vec![]],
        };
        let mut f = Finder {
            exes: &[],
            files: &[],
            exe_functions: Vec::new(),
            exe_keys: BTreeSet::new(),
            found: BTreeMap::new(),
            named: BTreeMap::new(),
            known: Vec::new(),
        };
        take_timeline(&mut f, &t);
        let got: Vec<(u32, u32)> =
            f.found.values().map(|x| (parse_hex(&x.row["entry"]), parse_hex(&x.row["end"]))).collect();
        assert_eq!(got, [(a + 0xFF8, b + 0xC)]);
    }

    #[test]
    fn code_resumed_by_a_jump_stays_a_guess() {
        // A function is called, stores where to go on, and jumps away
        // (a+4); in a later moment code resumes there (a+0x10) with no
        // call. Nothing ran straight on into it, so where that stretch
        // starts is a guess beside the function the call started.
        let a = 0x8010_0000u32;
        const J_18: u32 = 0x0804_0006; // j 80100018
        let ws = [PROLOGUE, J_18, NOP, NOP, NOP, NOP, JR_RA, NOP];
        let mut p = vec![0u8; 0x1000];
        for (k, w) in ws.iter().enumerate() {
            p[4 * k..4 * k + 4].copy_from_slice(&w.to_le_bytes());
        }
        let r = |k: u32, m: u32, c: bool| (a + 4 * k, ws[k as usize], m, c);
        let t = Timeline {
            copies: vec![(10, a, p), (20, 0x8011_0000, vec![0; 0x1000])],
            ran: vec![
                vec![
                    r(0, 0, false),
                    r(1, 0, true),
                    r(2, 0, true),
                    r(4, 1, false),
                    r(5, 1, true),
                    r(6, 0, false),
                    r(7, 0, true),
                ],
                vec![],
            ],
            calls: vec![vec![(a, vec![(0, 0)])], vec![]],
        };
        let mut f = Finder {
            exes: &[],
            files: &[],
            exe_functions: Vec::new(),
            exe_keys: BTreeSet::new(),
            found: BTreeMap::new(),
            named: BTreeMap::new(),
            known: Vec::new(),
        };
        take_timeline(&mut f, &t);
        let got = |f: &Finder| -> BTreeSet<(u32, u32, bool)> {
            f.found.values().map(|x| (parse_hex(&x.row["entry"]), parse_hex(&x.row["end"]), x.fact())).collect()
        };
        assert_eq!(got(&f), BTreeSet::from([(a, a + 0x20, true), (a + 0x10, a + 0x20, false)]));
    }

    #[test]
    fn a_guess_never_cuts_code_the_trace_started() {
        // Code loaded at b is called and runs, skipping b+8. Beside it, old
        // code still in RAM (it ran before b was loaded) calls b+8, meant for
        // what was there then.
        let (a, b) = (0x8010_0000u32, 0x8010_1000u32);
        const ADDIU: u32 = 0x2402_0001;
        let page = |ws: &[u32]| {
            let mut p = vec![0u8; 0x1000];
            for (k, w) in ws.iter().enumerate() {
                p[4 * k..4 * k + 4].copy_from_slice(&w.to_le_bytes());
            }
            p
        };
        let jal = 0x0C00_0000 | ((b + 8) >> 2 & 0x03FF_FFFF);
        let new = [0x1000_0002, NOP, ADDIU, JR_RA, NOP]; // b +0xC
        let t = Timeline {
            copies: vec![(10, a, page(&[jal, NOP, JR_RA, NOP])), (20, b, page(&new))],
            ran: vec![
                vec![(a, jal, 0, false), (a + 4, NOP, 0, true), (a + 8, JR_RA, 0, false), (a + 0xC, NOP, 0, true)],
                vec![
                    (b, new[0], 1, false),
                    (b + 4, NOP, 1, true),
                    (b + 0xC, JR_RA, 1, false),
                    (b + 0x10, NOP, 1, true),
                ],
            ],
            calls: vec![vec![(a, vec![(0, 0)])], vec![(b, vec![(1, 1)])]],
        };
        let mut f = Finder {
            exes: &[],
            files: &[],
            exe_functions: Vec::new(),
            exe_keys: BTreeSet::new(),
            found: BTreeMap::new(),
            named: BTreeMap::new(),
            known: Vec::new(),
        };
        take_timeline(&mut f, &t);
        let got: BTreeSet<(u32, u32)> =
            f.found.values().map(|x| (parse_hex(&x.row["entry"]), parse_hex(&x.row["end"]))).collect();
        assert_eq!(got, BTreeSet::from([(a, a + 0x10), (b, b + 0x14)]));
        // Nor does a call land where the trace ran straight on.
        let c = code(&[PROLOGUE, NOP, JR_RA, NOP]);
        let mut x = Analysis::new(&c, None, Box::new(|_| true));
        x.continues.insert(0x8001_0004);
        x.seed(0x8001_0004, "trace_call");
        x.drain();
        assert!(x.functions.is_empty());
    }
}
