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

/// `jr` on any register: the code does not go on past its delay slot.
/// On another register than ra it is a jump to code the bytes do not say
/// (a stub into the BIOS, a switch's cases), taken as the end of the code
/// there: a function after it is listed on its own, where reading on would
/// hide it in the code before (a guess either way). Not `jr zero`: no
/// code jumps to address 0, and it is the integer 8, common in data.
fn is_jr(i: u32) -> bool {
    i & 0xFC1F_FFFF == 0x0000_0008 && i >> 21 != 0
}

/// A value the words before a `jr` give a register: a constant, an index
/// (the register and its write it was read from) times 4 plus a constant,
/// the word loaded from a table at such an address, or a test of an index
/// against a bound (`sltiu`).
#[derive(Clone, Copy, PartialEq)]
enum Val {
    Unknown,
    Const(u32),
    Scaled((u32, u32), u32),
    Case((u32, u32), u32),
    Test((u32, u32), u32),
}

/// The register an instruction writes, if any.
fn written(i: u32) -> Option<u32> {
    let (op, rs, rt, rd) = (i >> 26, (i >> 21) & 31, (i >> 16) & 31, (i >> 11) & 31);
    let r = match op {
        0x00 => match i & 0x3F {
            0x08 | 0x0C | 0x0D | 0x11 | 0x13 | 0x18..=0x1B => return None,
            _ => rd,
        },
        0x03 => 31,
        0x08..=0x0F | 0x20..=0x26 => rt,
        0x10 | 0x12 if rs == 0 || rs == 2 => rt,
        _ => return None,
    };
    (r != 0).then_some(r)
}

/// How many words before a `jr` its dispatch is read from.
const DISPATCH: u32 = 24;

/// How far a function's start and return are looked for from a `jr`.
const FRAME: u32 = 1024;

/// The registers a call keeps (`s0` to `s8`, as compiled code saves them).
fn saved(r: usize) -> bool {
    matches!(r, 16..=23 | 30)
}

/// What the dispatch reader knows: each register's value, how many times
/// each was written, and the bounds tests and branches gave indices.
struct Regs {
    vals: [Val; 32],
    writes: [u32; 32],
    bounds: BTreeMap<(u32, u32), u32>,
}

impl Regs {
    fn new(vals: [Val; 32]) -> Self {
        Regs { vals, writes: [0; 32], bounds: BTreeMap::new() }
    }

    fn step(&mut self, w: u32) {
        let (op, rs, rt) = (w >> 26, (w >> 21) & 31, (w >> 16) & 31);
        let imm = w & 0xFFFF;
        let simm = imm as i16 as i32 as u32;
        let src = |r: u32| (r, self.writes[r as usize]);
        let (a, b) = (self.vals[rs as usize], self.vals[rt as usize]);
        if matches!(op, 0x04 | 0x05) && rt == 0 {
            if let Val::Test(x, n) = a {
                self.bounds.insert(x, n);
            }
        }
        let v = match op {
            0x0F => Val::Const(imm << 16),
            0x09 => match a {
                Val::Const(c) => Val::Const(c.wrapping_add(simm)),
                Val::Scaled(x, c) => Val::Scaled(x, c.wrapping_add(simm)),
                _ => Val::Unknown,
            },
            0x0D => match a {
                Val::Const(c) => Val::Const(c | imm),
                _ => Val::Unknown,
            },
            0x0B if imm < 0x8000 => Val::Test(src(rs), imm),
            0x23 => match a {
                Val::Scaled(x, c) => Val::Case(x, c.wrapping_add(simm)),
                _ => Val::Unknown,
            },
            0x00 => match w & 0x3F {
                0x00 if (w >> 6) & 31 == 2 => Val::Scaled(src(rt), 0),
                0x21 => match (a, b) {
                    (Val::Scaled(x, c), Val::Const(d)) | (Val::Const(d), Val::Scaled(x, c)) => {
                        Val::Scaled(x, c.wrapping_add(d))
                    }
                    _ => Val::Unknown,
                },
                0x2B => match b {
                    Val::Const(n) => Val::Test(src(rs), n),
                    _ => Val::Unknown,
                },
                _ => Val::Unknown,
            },
            _ => Val::Unknown,
        };
        if let Some(d) = written(w) {
            self.writes[d as usize] += 1;
            self.vals[d as usize] = v;
        }
    }

    /// Reads `[from, to)` straight on. After a call's delay slot only the
    /// registers it keeps are known; after a jump's, only `init`.
    fn read(&mut self, code: &Code, from: u32, to: u32, init: &[Val; 32]) {
        let mut pc = from;
        while pc != to {
            let Some(w) = code.word(pc) else {
                self.vals = *init;
                pc = pc.wrapping_add(4);
                continue;
            };
            match code.word(pc.wrapping_sub(8)) {
                Some(j) if j >> 26 == 0x03 || j >> 26 == 0 && j & 0x3F == 0x09 => {
                    for r in (0..32).filter(|&r| !saved(r)) {
                        self.vals[r] = init[r];
                    }
                }
                Some(j) if j >> 26 == 0x02 || j >> 26 == 0 && j & 0x3F == 0x08 => self.vals = *init,
                _ => {}
            }
            self.step(w);
            pc = pc.wrapping_add(4);
        }
    }
}

/// The saved registers that hold one constant at `at` in the function
/// around it: from its start (the nearest stack prologue before, with no
/// return between) to `at` written once, to a constant, and from `at` to
/// its return written only to restore them from the stack. Compiled code
/// keeps a table's address so across calls and the cases of other
/// switches.
fn frame_constants(code: &Code, at: u32) -> [Val; 32] {
    let mut out = [Val::Unknown; 32];
    let Some(start) = (1..FRAME)
        .map(|k| at.wrapping_sub(4 * k))
        .take_while(|&p| code.word(p).is_some_and(|w| w != JR_RA))
        .find(|&p| code.word(p).is_some_and(is_prologue))
    else {
        return out;
    };
    let mut r = Regs::new([Val::Unknown; 32]);
    let mut once = [Val::Unknown; 32];
    let mut pc = start;
    while pc != at {
        let w = code.word(pc).unwrap_or(0);
        r.step(w);
        if let Some(d) = written(w).map(|d| d as usize).filter(|&d| saved(d)) {
            once[d] = if r.writes[d] == 1 { r.vals[d] } else { Val::Unknown };
        }
        pc = pc.wrapping_add(4);
    }
    let mut kept = [true; 32];
    for k in 1..FRAME {
        let Some(w) = code.word(at.wrapping_add(4 * k)) else { break };
        if let Some(d) = written(w).map(|d| d as usize) {
            kept[d] &= w >> 26 == 0x23 && (w >> 21) & 31 == 29;
        }
        if w == JR_RA {
            break;
        }
    }
    for d in (0..32).filter(|&d| saved(d) && kept[d]) {
        if let Val::Const(_) = once[d] {
            out[d] = once[d];
        }
    }
    out
}

/// The table a switch's `jr` at `at` jumps through and how many cases it
/// has, when the words before it say both, as compilers emit a switch:
/// an index tested against a bound (`sltiu`, then a branch on the test),
/// times 4, plus the table's address (`lui`, `addiu` or `ori`, or a `lw`
/// offset), loaded, jumped to. Read straight on, as if the code before ran
/// into it, knowing at first only the function's constants (see
/// `frame_constants`) when the words before do not say the table's
/// address. Anything else is not claimed.
fn dispatch(code: &Code, at: u32) -> Option<(u32, u32)> {
    let i = code.word(at)?;
    if !is_jr(i) || i == JR_RA {
        return None;
    }
    let reg = ((i >> 21) & 31) as usize;
    let from = at.wrapping_sub(4 * DISPATCH);
    let mut r = Regs::new([Val::Unknown; 32]);
    r.read(code, from, at, &[Val::Unknown; 32]);
    if r.vals[reg] == Val::Unknown {
        let init = frame_constants(code, at);
        r = Regs::new(init);
        r.read(code, from, at, &init);
    }
    match r.vals[reg] {
        Val::Case(x, table) => r.bounds.get(&x).filter(|&&n| (1..=1024).contains(&n)).map(|&n| (table, n)),
        _ => None,
    }
}

fn is_prologue(i: u32) -> bool {
    i >> 16 == 0x27BD && (i & 0x8000) != 0
}

/// One function: its entry and its chunks, `[start, end)` each, in
/// address order; `end` is the last chunk's end.
#[derive(Clone, Debug)]
struct Function {
    start: u32,
    end: u32,
    chunks: Vec<(u32, u32)>,
    found_by: BTreeSet<&'static str>,
    /// How the extent ended: return, register_jump, tail_jump,
    /// next_function, invalid, limit; `ran` for a body of what ran.
    ends: &'static str,
    /// Its start is the trace's: a call it saw, or the entry.
    fact: bool,
    /// A body of what ran (see `Fact`): what ran in it is never split or
    /// taken; a guessed start in what it did not run cuts that short.
    fixed: bool,
    /// For a body of what ran, the words that ran in it.
    ran: BTreeSet<u32>,
    /// Words in it a jump entered (see `Fact::jumps`).
    jumped: BTreeMap<u32, u8>,
    /// Guessed starts on code that ran in it: a call read from bytes
    /// landing there.
    candidates: BTreeSet<u32>,
    /// For shared code: the entries of the calls it ran inside.
    shared_by: BTreeSet<u32>,
    /// Its words run under a landing, guessed to be its (see `Body`).
    landed: u32,
}

impl Function {
    fn new(start: u32, how: &'static str) -> Self {
        Function {
            start,
            end: start,
            chunks: vec![(start, start)],
            found_by: BTreeSet::from([how]),
            ends: "",
            fact: is_fact(how),
            fixed: false,
            ran: BTreeSet::new(),
            jumped: BTreeMap::new(),
            candidates: BTreeSet::new(),
            shared_by: BTreeSet::new(),
            landed: 0,
        }
    }

    fn words(&self) -> impl Iterator<Item = u32> + '_ {
        self.chunks.iter().flat_map(|&(s, e)| (s..e).step_by(4))
    }
}

/// Seeds the trace or the executable vouches for. Every other seed (a call
/// or jump in code, a pointer, a prologue, where a stretch of executed code
/// no known call ran starts) is a guess: guesses wait until the facts are
/// followed, and never take a word a body of what ran holds.
fn is_fact(how: &str) -> bool {
    matches!(how, "entry" | "trace_call" | "exception" | "shared_code" | "jumped_into")
}

/// Seeds that, landing inside a body of what ran, are kept as a candidate
/// entry of it: a call or jump read from bytes, or a call the trace saw
/// whose own body is not here whole.
fn is_candidate(how: &str) -> bool {
    matches!(how, "call" | "tail_jump" | "call_from_executable" | "trace_call")
}

/// What one owner of the trace (owners.json) ran in one image: the address
/// of its entry and whether the entry is in this image, the words run
/// inside it here, of them those run under a landing guessed to be its,
/// and the words a jump entered (how: 1 frame torn down, 2 not, 3 both).
#[derive(Clone, Debug, Default)]
struct Body {
    entry: u32,
    here: bool,
    how: &'static str,
    words: BTreeSet<u32>,
    landed: BTreeSet<u32>,
    jumps: BTreeMap<u32, u8>,
    /// Running in this very code: its calls are followed.
    live: bool,
}

/// A function of what ran in one image: its entry, how it is known, the
/// words that ran in it, and from `Body` the rest.
#[derive(Clone, Debug, PartialEq)]
struct Fact {
    entry: u32,
    how: &'static str,
    words: BTreeSet<u32>,
    landed: u32,
    jumps: BTreeMap<u32, u8>,
    shared_by: BTreeSet<u32>,
    live: bool,
}

/// The functions of what ran in one image. A word run inside one owner
/// whose entry is here is that owner's. Words run inside more than one, or
/// only inside an owner whose entry is in another image, are split where
/// an owner's entry is; a piece starting at one is that owner's, any other
/// is a function of its own, entered at its first word: the word before it
/// is not in the piece, so each owner that ran it came in there or by a
/// jump.
fn facts(bodies: &[Body]) -> Vec<Fact> {
    let mut out: Vec<Fact> = Vec::new();
    let mut at: BTreeMap<u32, usize> = BTreeMap::new();
    for b in bodies.iter().filter(|b| b.here) {
        let k = *at.entry(b.entry).or_insert_with(|| {
            out.push(Fact {
                entry: b.entry,
                how: b.how,
                words: BTreeSet::new(),
                landed: 0,
                jumps: BTreeMap::new(),
                shared_by: BTreeSet::new(),
                live: false,
            });
            out.len() - 1
        });
        out[k].live |= b.live;
    }
    let mut owners: BTreeMap<u32, Vec<usize>> = BTreeMap::new();
    for (k, b) in bodies.iter().enumerate() {
        for &w in &b.words {
            owners.entry(w).or_default().push(k);
        }
    }
    let mut shared = Vec::new();
    for (&w, ks) in &owners {
        let entries: BTreeSet<u32> = ks.iter().map(|&k| bodies[k].entry).collect();
        match (entries.len(), bodies[ks[0]].here) {
            (1, true) => {
                let f = &mut out[at[&bodies[ks[0]].entry]];
                f.words.insert(w);
            }
            _ => shared.push(w),
        }
    }
    let mut pieces: Vec<Vec<u32>> = Vec::new();
    for w in shared {
        match pieces.last_mut() {
            Some(p) if *p.last().unwrap() + 4 == w && !at.contains_key(&w) => p.push(w),
            _ => pieces.push(vec![w]),
        }
    }
    for p in pieces {
        let ks: BTreeSet<usize> = p.iter().flat_map(|w| owners[w].iter().copied()).collect();
        let k = match at.get(&p[0]) {
            Some(&k) => k,
            None => {
                let entry = p[0];
                let here = ks.iter().any(|&k| bodies[k].here);
                out.push(Fact {
                    entry,
                    how: if here { "shared_code" } else { "jumped_into" },
                    words: BTreeSet::new(),
                    landed: 0,
                    jumps: BTreeMap::new(),
                    shared_by: ks.iter().map(|&k| bodies[k].entry).collect(),
                    live: ks.iter().any(|&k| bodies[k].live),
                });
                at.insert(entry, out.len() - 1);
                out.len() - 1
            }
        };
        out[k].words.extend(&p);
    }
    // What jumps entered, and what landings ran, in each.
    for f in &mut out {
        for b in bodies {
            for (&w, &how) in b.jumps.range(f.words.first().copied().unwrap_or(0)..) {
                if w != f.entry && f.words.contains(&w) {
                    *f.jumps.entry(w).or_default() |= how;
                }
            }
            if b.entry == f.entry && b.here {
                f.landed += b.landed.iter().filter(|w| f.words.contains(w)).count() as u32;
            }
        }
    }
    out.retain(|f| f.words.contains(&f.entry));
    out
}

/// Consecutive words as `[start, end)` spans.
fn spans(words: impl IntoIterator<Item = u32>) -> Vec<(u32, u32)> {
    let mut out: Vec<(u32, u32)> = Vec::new();
    for a in words {
        match out.last_mut() {
            Some(s) if s.1 == a => s.1 = a + 4,
            _ => out.push((a, a + 4)),
        }
    }
    out
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
    /// Each function's chunks: start to `(end, entry)`.
    claim: BTreeMap<u32, (u32, u32)>,
    queue: Vec<(u32, &'static str)>,
    /// Guessed seeds, waiting until `guesses` lets them in.
    later: Vec<(u32, &'static str)>,
    guessing: bool,
    /// Words the trace saw continue the code before them (run straight on
    /// into, or returned to by a call two words before) and no call into:
    /// reading code that did not run, the code after a return they follow
    /// is not its end.
    continues: BTreeSet<u32>,
    /// Where a stretch of executed code no known call ran may start a
    /// function: code read in the RAM it first ran with (all of it, when
    /// unset).
    fresh: Option<BTreeSet<u32>>,
    /// The functions running in this very code (all, when unset): the
    /// starts of the calls running then, and code first run then. The
    /// calls in any other function are not followed, as what they call
    /// may be other code by now.
    live: Option<BTreeSet<u32>>,
    /// Pointer targets inside functions: switch cases, block leaders.
    leaders: BTreeSet<u32>,
    /// Words that ran inside a call as this code holds them (by address,
    /// how many calls), where it may not hold the whole call as it ran: no
    /// guess starts on them or takes them (the call is found where it is
    /// whole).
    ran: Option<&'a BTreeMap<u32, u32>>,
    /// Switches (see `dispatch`): each `jr`'s table and the cases it
    /// holds, where the code goes on after the `jr`.
    switches: BTreeMap<u32, (u32, Vec<u32>)>,
    /// The words of those tables: not pointers to functions.
    tables: BTreeSet<u32>,
}

impl<'a> Analysis<'a> {
    fn new(code: &'a Code, region: Option<(u32, u32)>, allowed: Box<dyn Fn(u32) -> bool + 'a>) -> Self {
        Self {
            code,
            allowed,
            region,
            functions: BTreeMap::new(),
            claim: BTreeMap::new(),
            queue: Vec::new(),
            later: Vec::new(),
            guessing: false,
            continues: BTreeSet::new(),
            fresh: None,
            live: None,
            leaders: BTreeSet::new(),
            ran: None,
            switches: BTreeMap::new(),
            tables: BTreeSet::new(),
        }
        .with_switches()
    }

    /// Each switch in the code whose cases are all code near it.
    fn with_switches(mut self) -> Self {
        for at in (self.code.base..self.code.end()).step_by(4) {
            let Some((table, n)) = dispatch(self.code, at) else { continue };
            let cases: Option<Vec<u32>> = (0..n)
                .map(|k| {
                    self.code.word(table.wrapping_add(4 * k)).filter(|&t| {
                        t.is_multiple_of(4)
                            && t.abs_diff(at) < 4 * MAX_FUNCTION
                            && (self.allowed)(t)
                            && self.code.word(t).is_some_and(sensible)
                    })
                })
                .collect();
            if let Some(mut cases) = cases {
                self.tables.extend((0..n).map(|k| table.wrapping_add(4 * k)));
                self.leaders.extend(&cases);
                cases.sort_unstable();
                cases.dedup();
                self.switches.insert(at, (table, cases));
            }
        }
        self
    }

    fn ran(&self, at: u32) -> bool {
        self.ran.is_some_and(|r| r.contains_key(&at)) && (self.allowed)(at)
    }

    /// Lets the guessed seeds in, once the facts are followed.
    fn guesses(&mut self) {
        self.guessing = true;
        self.drain();
    }

    fn search(&self) -> Option<(u32, u32)> {
        self.region.or_else(|| self.span())
    }

    /// Keeps `f`, its chunks claimed in place of any it had.
    fn set(&mut self, mut f: Function) {
        self.unset(f.start);
        f.chunks.retain(|c| c.0 < c.1 || c.0 == f.start);
        f.end = f.chunks.iter().map(|c| c.1).max().unwrap_or(f.start);
        for &(s, e) in &f.chunks {
            self.claim.insert(s, (e, f.start));
        }
        self.functions.insert(f.start, f);
    }

    fn unset(&mut self, at: u32) -> Option<Function> {
        let f = self.functions.remove(&at)?;
        for c in &f.chunks {
            if self.claim.get(&c.0).is_some_and(|x| x.1 == at) {
                self.claim.remove(&c.0);
            }
        }
        Some(f)
    }

    /// The chunk holding `at`: `(start, end, entry)`.
    fn chunk(&self, at: u32) -> Option<(u32, u32, u32)> {
        self.claim.range(..=at).next_back().map(|(&s, &(e, f))| (s, e, f)).filter(|c| at < c.1)
    }

    fn containing(&self, at: u32) -> Option<&Function> {
        self.chunk(at).and_then(|c| self.functions.get(&c.2))
    }

    fn seed(&mut self, at: u32, how: &'static str) {
        if !at.is_multiple_of(4) || self.code.word(at).is_none() || !(self.allowed)(at) {
            return;
        }
        // On code that ran in a body of what ran: a candidate entry of it,
        // or a block.
        if let Some(f) = self.containing(at).filter(|f| f.fixed && f.start != at && f.ran.contains(&at)) {
            let s = f.start;
            let f = self.functions.get_mut(&s).unwrap();
            if is_candidate(how) {
                f.candidates.insert(at);
            } else if how == "pointer" {
                self.leaders.insert(at);
            }
            return;
        }
        if self.ran(at) && self.chunk(at).is_none() {
            return;
        }
        if is_fact(how) {
            self.queue.push((at, how));
        } else {
            self.later.push((at, how));
        }
    }

    /// The bodies of what ran, placed before anything is read from bytes:
    /// then each is widened by what it did not run (`fill`), and the calls
    /// in the live ones are followed.
    fn place(&mut self, facts: &[Fact]) {
        for x in facts {
            let mut f = Function::new(x.entry, x.how);
            (f.chunks, f.ends, f.fixed, f.ran) = (spans(x.words.iter().copied()), "ran", true, x.words.clone());
            (f.jumped, f.shared_by, f.landed) = (x.jumps.clone(), x.shared_by.clone(), x.landed);
            if self.functions.contains_key(&x.entry) || self.chunk(x.entry).is_some() {
                continue;
            }
            self.set(f);
        }
        let entries: Vec<u32> = facts.iter().map(|x| x.entry).filter(|e| self.functions.contains_key(e)).collect();
        for &e in &entries {
            let f = self.functions[&e].clone();
            let more = self.fill(&f);
            if !more.is_empty() {
                let mut f = self.unset(e).unwrap();
                f.chunks = spans(f.words().chain(more).collect::<BTreeSet<_>>());
                self.set(f);
            }
        }
        for x in facts.iter().filter(|x| entries.contains(&x.entry)) {
            if x.live || self.live.as_ref().is_none_or(|l| l.contains(&x.entry)) {
                let calls: Vec<u32> = self.functions[&x.entry]
                    .words()
                    .filter_map(|pc| self.code.word(pc).filter(|w| w >> 26 == 0x03).map(|w| jump_target(pc, w)))
                    .collect();
                for t in calls {
                    self.seed(t, "call");
                }
            }
        }
    }

    /// What a body of what ran did not run, read from bytes (a guess): the
    /// code between its words, and from its words along the branches and
    /// jumps their bytes say, over words no function holds, until a
    /// return, a tail jump, an invalid word, padding or the limit. A `j`
    /// forward to no known function is taken as inside it.
    fn fill(&self, f: &Function) -> BTreeSet<u32> {
        let have: BTreeSet<u32> = f.words().collect();
        let mut more = BTreeSet::new();
        let mut todo: Vec<u32> = have.iter().flat_map(|&w| self.next(f.start, w)).collect();
        // Code between what ran is read straight through, as an extent is:
        // cases of a switch are reached by a table, not by branches. What
        // ran inside other calls there (a `nop` of other code once loaded
        // here) is not theirs: this code ran on both sides of it.
        let runs = spans(have.iter().copied());
        let between: BTreeSet<u32> = runs.windows(2).flat_map(|w| (w[0].1..w[1].0).step_by(4)).collect();
        todo.extend(&between);
        while let Some(pc) = todo.pop() {
            if have.contains(&pc) || more.contains(&pc) || more.len() as u32 >= MAX_FUNCTION {
                continue;
            }
            if !(self.allowed)(pc)
                || self.chunk(pc).is_some()
                || self.functions.contains_key(&pc)
                || self.ran(pc) && !between.contains(&pc)
            {
                continue;
            }
            let Some(i) = self.code.word(pc) else { continue };
            if !sensible(i) || (i == 0 && (1..8).all(|k| self.code.word(pc + 4 * k) == Some(0))) {
                continue;
            }
            more.insert(pc);
            todo.extend(self.next(f.start, pc));
        }
        more
    }

    /// Where the code at `pc` goes on, by its bytes: after a delay slot,
    /// where its jump goes; else the next word.
    fn next(&self, start: u32, pc: u32) -> Vec<u32> {
        let Some(j) = self.code.word(pc.wrapping_sub(4)).filter(|&w| ends_block(w) || w >> 26 == 0x03) else {
            return vec![pc + 4];
        };
        let at = pc - 4;
        if is_jr(j) {
            return self.switches.get(&at).map(|s| s.1.clone()).unwrap_or_default();
        }
        if let Some(t) = branch_target(at, j) {
            // beq zero, zero is a plain branch.
            return if j >> 16 == 0x1000 { vec![t] } else { vec![t, pc + 4] };
        }
        if j >> 26 == 0x02 {
            let t = jump_target(at, j);
            let inside = t > pc && !self.functions.contains_key(&t) && t.wrapping_sub(start) < 4 * MAX_FUNCTION;
            return if inside { vec![t] } else { vec![] };
        }
        // jal and jalr return.
        vec![pc + 4]
    }

    /// The function's extent from `start`: forward until a `jr` or tail
    /// `j` that no branch jumps past and the trace saw nothing continue,
    /// the next known function, an invalid word or the limit.
    fn extent(&self, start: u32) -> (u32, &'static str) {
        let next = self.claim.range(start + 1..).next().map(|(&a, _)| a).unwrap_or(u32::MAX);
        let ran = self.ran.and_then(|r| {
            r.range(start + 1..start.saturating_add(4 * MAX_FUNCTION).min(next))
                .map(|(&a, _)| a)
                .find(|&a| (self.allowed)(a))
        });
        let next = next.min(ran.unwrap_or(u32::MAX));
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
            for &t in self.switches.get(&pc).map(|s| &s.1[..]).unwrap_or_default() {
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
            if (is_jr(i) && !self.switches.contains_key(&pc) || tail)
                && reach <= pc + 4
                && !self.continues.contains(&(pc + 8))
            {
                if !self.code.word(pc + 4).is_some_and(sensible) {
                    return (pc, "invalid");
                }
                let ends = if tail {
                    "tail_jump"
                } else if i == JR_RA {
                    "return"
                } else {
                    "register_jump"
                };
                return (pc + 8, ends);
            }
            pc += 4;
        }
        (pc, "limit")
    }

    /// Sets a guessed function's extent.
    fn widen(&mut self, at: u32) {
        let (end, ends) = self.extent(at);
        let mut f = self.unset(at).unwrap();
        (f.chunks, f.ends) = (vec![(at, end.max(at))], ends);
        self.set(f);
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
            // A seed inside a guessed function found earlier splits it; a
            // pointer there is a block in it.
            if how == "pointer" && self.containing(at).is_some() {
                self.leaders.insert(at);
                continue;
            }
            // In what a body of what ran did not run: the body is read
            // again from what ran, up to the new function.
            let body = self.containing(at).filter(|f| f.fixed).map(|f| (f.start, f.ran.contains(&at)));
            if body.is_some_and(|b| b.1) {
                continue;
            }
            if let Some((b, _)) = body {
                let mut f = self.unset(b).unwrap();
                f.chunks = spans(f.ran.iter().copied());
                self.set(f);
            }
            self.set(Function::new(at, how));
            self.widen(at);
            // The function before may have run into this one.
            if let Some((_, &(_, p))) = self.claim.range(..at).next_back() {
                if !self.functions[&p].fixed {
                    self.widen(p);
                }
            }
            let f = self.functions[&at].clone();
            if f.end <= f.start {
                self.unset(at);
            }
            if let Some((b, _)) = body {
                let g = self.functions[&b].clone();
                let more = self.fill(&g);
                let mut g = self.unset(b).unwrap();
                g.chunks = spans(g.ran.iter().copied().chain(more).collect::<BTreeSet<_>>());
                self.set(g);
            }
            if f.end <= f.start {
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
        }
    }

    /// The span functions cover, from the first start to the last end.
    fn span(&self) -> Option<(u32, u32)> {
        Some((*self.claim.keys().next()?, self.claim.values().map(|c| c.0).max()?))
    }

    /// Pointers into the code: a target inside a function is a block
    /// leader; one in a gap that starts like a function and decodes to a
    /// return is a function.
    fn pointers(&mut self, words: &[(u32, u32)]) -> usize {
        let Some((lo, hi)) = self.search() else { return 0 };
        let before = self.functions.len();
        let targets: BTreeSet<u32> = words
            .iter()
            .filter(|&&(a, w)| w % 4 == 0 && w >= lo && w < hi && !self.tables.contains(&a))
            .map(|&(_, w)| w)
            .collect();
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
        let Some(i) = self.code.word(t).filter(|_| !self.ran(t)) else { return false };
        let boundary = is_prologue(i) || i != 0 && self.after_end(t);
        if !boundary || !sensible(i) {
            return false;
        }
        // A tail jump lands on a function already known: tables of data
        // decode as jumps to anywhere.
        match self.extent(t) {
            (_, "return" | "register_jump") => true,
            (end, "tail_jump") => {
                let w = self.code.word(end - 8).unwrap_or(0);
                self.functions.contains_key(&jump_target(end - 8, w))
            }
            _ => false,
        }
    }

    /// `t` follows the end of a function: back over zero padding to a
    /// delay slot whose jump (`jr` or a tail `j`) ends that function.
    fn after_end(&self, t: u32) -> bool {
        let mut a = t.wrapping_sub(4);
        let mut padding = 0;
        while self.code.word(a) == Some(0) && padding < 64 {
            a = a.wrapping_sub(4);
            padding += 1;
        }
        // `a` is the jump, unless its delay slot was not a nop. A switch's
        // cases follow its `jr`.
        let ends = |a: u32, w: u32| w >> 26 == 0x02 || is_jr(w) && !self.switches.contains_key(&a);
        let d = t.wrapping_sub(4);
        match (self.code.word(a), self.code.word(a.wrapping_sub(4))) {
            (Some(w), _) if padding > 0 && ends(a, w) => true,
            (_, Some(w)) => ends(a.wrapping_sub(4), w) && self.code.word(d).is_some_and(|x| !ends(d, x)),
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
            if let Some(c) = self.chunk(at) {
                at = c.1;
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

    /// Code a traced run executed that no function covers and no known
    /// call ran: where each stretch of it this code holds starts, when it
    /// decodes to an end, a guess followed at once (after the facts, which
    /// come first). `ran` is what ran from this very code.
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
            if let Some(c) = self.chunk(at) {
                at = c.1;
                continue;
            }
            let start = at;
            while at < hi && self.chunk(at).is_none() {
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
            let inside = |t: u32| f.chunks.iter().any(|c| t >= c.0 && t < c.1);
            let mut leaders: BTreeSet<u32> = f.chunks.iter().map(|c| c.0).collect();
            leaders.insert(f.start);
            for pc in f.words() {
                let i = self.code.word(pc).unwrap_or(0);
                if let Some(t) = branch_target(pc, i).filter(|&t| inside(t)) {
                    leaders.insert(t);
                }
                if i >> 26 == 0x02 && inside(jump_target(pc, i)) {
                    leaders.insert(jump_target(pc, i));
                }
                if ends_block(i) && inside(pc + 8) {
                    leaders.insert(pc + 8);
                }
            }
            leaders.extend(self.leaders.iter().copied().filter(|&l| inside(l)));
            leaders.extend(f.candidates.iter().chain(f.jumped.keys()).copied().filter(|&l| inside(l)));
            let n = f.words().count();
            instructions += n as u64;
            blocks += leaders.len() as u64;
            let mut row = json!({
                "entry": hex(f.start), "end": hex(f.end), "instructions": n, "blocks": leaders.len(),
                "block_starts": leaders.iter().map(|&l| hex(l)).collect::<Vec<_>>(),
                "found_by": f.found_by.iter().collect::<Vec<_>>(), "ends": f.ends,
            });
            if f.chunks != [(f.start, f.end)] {
                row["chunks"] = json!(f.chunks.iter().map(|c| [hex(c.0), hex(c.1)]).collect::<Vec<_>>());
            }
            if !f.jumped.is_empty() {
                let how = |h: u8| match h {
                    1 => "frame_torn_down",
                    2 => "frame_kept",
                    _ => "both",
                };
                row["jumped_to"] = json!(f.jumped.iter().map(|(&a, &h)| json!([hex(a), how(h)])).collect::<Vec<_>>());
            }
            if !f.candidates.is_empty() {
                row["candidate_entries"] = json!(f.candidates.iter().map(|&a| hex(a)).collect::<Vec<_>>());
            }
            if !f.shared_by.is_empty() {
                row["shared_by"] = json!(f.shared_by.iter().map(|&a| hex(a)).collect::<Vec<_>>());
            }
            if f.landed > 0 {
                row["landed_words"] = json!(f.landed);
            }
            let switches: Vec<Value> = f
                .chunks
                .iter()
                .flat_map(|&(s, e)| self.switches.range(s..e))
                .map(|(&at, (table, cases))| json!({"jr": hex(at), "table": hex(*table), "cases": cases.len()}))
                .collect();
            if !switches.is_empty() {
                row["switches"] = json!(switches);
            }
            rows.push(row);
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

/// A report row's chunks: `chunks`, or `[entry, end)`.
fn row_chunks(row: &Value) -> Vec<(u32, u32)> {
    match row["chunks"].as_array() {
        Some(cs) => cs.iter().map(|c| (parse_hex(&c[0]), parse_hex(&c[1]))).collect(),
        None => vec![(parse_hex(&row["entry"]), parse_hex(&row["end"]))],
    }
}

/// The bytes of `chunks`, in order, from code `word` reads.
fn chunk_bytes(chunks: &[(u32, u32)], bytes: impl Fn(u32, u32) -> Vec<u8>) -> Vec<u8> {
    chunks.iter().flat_map(|&(s, e)| bytes(s, e)).collect()
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
    cluster(a.claim.iter().map(|(&s, &(e, _))| (s, e)))
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
/// moment is the number of the latest copy then), the call targets in it
/// (calls.json), with the moments a call to each ran in, from the call
/// until it returned, and what ran inside each call (owners.json). At any
/// moment, the copy each page held is RAM's code then.
struct Timeline {
    copies: Vec<(u64, u32, Vec<u8>)>,
    /// Per copy: `(address, instruction, moment, continues the code before
    /// it)`.
    ran: Vec<Vec<(u32, u32, u32, bool)>>,
    /// Per copy: `(address, moments)`.
    calls: Vec<Vec<(u32, Spans)>>,
    /// Per copy, per word: the copy that word has been the same since. A
    /// page's copies are in order, each with every word changed since the
    /// one before; a word none of them changed is the one that ran.
    ver: Vec<Vec<u32>>,
    /// What ran inside each call and exception of the trace.
    groups: Vec<Group>,
    /// Per copy: the words run in it inside a group.
    owned: Vec<BTreeSet<u32>>,
}

/// A call (or exception) of the trace, by its entry and the copy its entry
/// word has been the same since, with the words run inside it as
/// `(version, address)`: the calls to the same code, while none of it
/// changed, are one. Of its words, those run under a landing (a `jr ra` to
/// no return the trace held) whose resume point ran inside this call alone
/// after a return through the stack: a guess (`setjmp` returns twice), and
/// words that also ran inside a call the stack held are that call's.
#[derive(Clone, Debug, Default)]
struct Group {
    entry: u32,
    ver: u32,
    how: &'static str,
    words: Vec<(u32, u32)>,
    landed: BTreeSet<(u32, u32)>,
    jumps: BTreeMap<(u32, u32), u8>,
}

/// One record of owners.json, its words as `(copy, address)`.
#[derive(Clone, Debug, Default)]
struct OwnerRow {
    kind: String,
    entry: Option<(u32, u32)>,
    body: Vec<(u32, u32)>,
    jumps: Vec<(u32, u32, u8)>,
    returned_inside: Vec<usize>,
    seen_more: bool,
}

fn owner_rows(v: &Value) -> Vec<OwnerRow> {
    let ram = |a: u64| (0x8000_0000..0x8020_0000).contains(&a);
    let n = |v: &Value| v.as_u64().map(|x| x as u32);
    v.as_array()
        .into_iter()
        .flatten()
        .map(|o| OwnerRow {
            kind: o["owner"].as_str().unwrap_or_default().to_string(),
            entry: n(&o["copy"]).zip(n(&o["address"])),
            body: o["body"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|r| Some((n(&r[0])?, r[1].as_u64()?, r[2].as_u64()?)))
                .filter(|r| ram(r.1))
                .flat_map(|(c, a, e)| (a as u32..e as u32).step_by(4).map(move |x| (c, x)))
                .collect(),
            jumps: o["jumps"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|r| Some((n(&r[0])?, n(&r[1])?, r[2].as_u64()? as u8)))
                .collect(),
            returned_inside: o["returned_inside"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|x| x.as_u64().map(|x| x as usize))
                .collect(),
            seen_more: o["seen_more"].as_bool().unwrap_or(false),
        })
        .collect()
}

/// A run's timeline: none for a run without a trace; an error for one
/// recorded before run --trace kept each code page's history, the moments
/// each call ran in and what ran inside each.
fn timeline(run: &Path) -> crate::Result<Option<Timeline>> {
    let path = run.join("executed.bin");
    let e = match std::fs::read(&path) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(Error::io(&path, e)),
    };
    let again = || {
        Error::invalid(format!(
            "{}: recorded before run --trace kept each code page's history, when code ran and what ran inside each call; record it again with run --trace",
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
    let text = std::fs::read_to_string(run.join("owners.json")).map_err(|_| again())?;
    let owners: Value = serde_json::from_str(&text).map_err(|_| again())?;
    Ok(Some(Timeline::new(copies, ran, calls, &owner_rows(&owners))))
}

impl Timeline {
    fn new(
        copies: Vec<(u64, u32, Vec<u8>)>,
        ran: Vec<Vec<(u32, u32, u32, bool)>>,
        calls: Vec<Vec<(u32, Spans)>>,
        owners: &[OwnerRow],
    ) -> Self {
        let mut ver: Vec<Vec<u32>> = Vec::new();
        let mut last: BTreeMap<u32, usize> = BTreeMap::new();
        for (k, (_, page, bytes)) in copies.iter().enumerate() {
            let words = bytes.len() / 4;
            let v = match last.insert(*page, k) {
                Some(p) => (0..words)
                    .map(|j| {
                        let same = copies[p].2.get(4 * j..4 * j + 4) == bytes.get(4 * j..4 * j + 4);
                        if same { ver[p][j] } else { k as u32 }
                    })
                    .collect(),
                None => vec![k as u32; words],
            };
            ver.push(v);
        }
        let mut t = Timeline { copies, ran, calls, ver, groups: Vec::new(), owned: Vec::new() };
        t.owned = vec![BTreeSet::new(); t.copies.len()];
        // Calls and exceptions first, then landings guessed to be theirs.
        let mut key: BTreeMap<(&'static str, u32, u32), usize> = BTreeMap::new();
        let mut of_row: Vec<Option<usize>> = vec![None; owners.len()];
        for (r, o) in owners.iter().enumerate() {
            let how = match o.kind.as_str() {
                "call" => "trace_call",
                "exception" => "exception",
                _ => continue,
            };
            let Some((c, a)) = o.entry else { continue };
            let Some(v) = t.version(c, a) else { continue };
            let g = *key.entry((how, v, a)).or_insert_with(|| {
                t.groups.push(Group { entry: a, ver: v, how, words: vec![(v, a)], ..Default::default() });
                t.groups.len() - 1
            });
            of_row[r] = Some(g);
            for &(c, w) in &o.body {
                if let Some(v) = t.version(c, w) {
                    t.groups[g].words.push((v, w));
                    t.owned[c as usize].insert(w);
                }
            }
            for &(c, w, h) in &o.jumps {
                if let Some(v) = t.version(c, w) {
                    *t.groups[g].jumps.entry((v, w)).or_default() |= h;
                }
            }
        }
        let held: std::collections::HashSet<(u32, u32)> =
            t.groups.iter().flat_map(|g| g.words.iter().copied()).collect();
        for o in owners.iter().filter(|o| o.kind == "landed") {
            // Where it resumed ran inside one owner: a landing's, in turn.
            let mut at = o;
            let mut g = None;
            for _ in 0..8 {
                let [x] = at.returned_inside[..] else { break };
                if at.seen_more || x >= owners.len() {
                    break;
                }
                if owners[x].kind == "landed" {
                    at = &owners[x];
                    continue;
                }
                g = of_row[x];
                break;
            }
            let Some(g) = g else { continue };
            for &(c, w) in &o.body {
                let Some(v) = t.version(c, w) else { continue };
                if !held.contains(&(v, w)) {
                    t.groups[g].words.push((v, w));
                    t.groups[g].landed.insert((v, w));
                    t.owned[c as usize].insert(w);
                }
            }
        }
        for g in &mut t.groups {
            g.words.sort();
            g.words.dedup();
        }
        t
    }

    /// Word `a` as copy `k` holds it.
    fn word(&self, k: u32, a: u32) -> Option<u32> {
        let (_, page, bytes) = self.copies.get(k as usize)?;
        let o = a.checked_sub(*page)? as usize;
        bytes.get(o..o + 4).map(|b| u32::from_le_bytes(b.try_into().unwrap()))
    }

    /// The copy word `a` of copy `k` has been the same since.
    fn version(&self, k: u32, a: u32) -> Option<u32> {
        let (_, page, _) = self.copies.get(k as usize)?;
        let j = a.checked_sub(*page)? / 4;
        self.ver[k as usize].get(j as usize).copied()
    }

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

/// Which groups a moment holds whole: each of their words with the bytes
/// it ran as, by the copy each page holds then. The same code loaded again
/// at the same place (a later copy with the same bytes, after other code
/// was there) is the same code: what ran in either load is a body in both,
/// and the calls to it in both are one function.
struct Holding {
    /// Per page: `(group, word)` of the words on it.
    on: BTreeMap<u32, Vec<(usize, usize)>>,
    /// Per group: its words the moment does not hold as they ran.
    missing: Vec<usize>,
    /// Per address: the groups' words the moment holds as they ran there,
    /// on pages holding no executable's code.
    present: BTreeMap<u32, u32>,
}

impl Holding {
    fn new(t: &Timeline) -> Self {
        let mut on: BTreeMap<u32, Vec<(usize, usize)>> = BTreeMap::new();
        for (g, x) in t.groups.iter().enumerate() {
            for (k, &(_, a)) in x.words.iter().enumerate() {
                on.entry(a & !(PAGE - 1)).or_default().push((g, k));
            }
        }
        Holding { on, missing: t.groups.iter().map(|g| g.words.len()).collect(), present: BTreeMap::new() }
    }

    /// Page `page` now holds copy `new` in place of `old`; `exe` is
    /// whether a copy holds an executable's code.
    fn change(&mut self, t: &Timeline, page: u32, old: Option<usize>, new: usize, exe: impl Fn(usize) -> bool) {
        let Some(on) = self.on.get(&page) else { return };
        let counted = |c: usize| !exe(c) && page >= 0x8001_0000;
        for &(g, k) in on {
            let (v, a) = t.groups[g].words[k];
            let is = |c: usize| t.word(c as u32, a) == t.word(v, a);
            let (was, now) = (old.is_some_and(is), is(new));
            match (was, now) {
                (true, false) => self.missing[g] += 1,
                (false, true) => self.missing[g] -= 1,
                _ => {}
            }
            if was && old.is_some_and(counted) {
                let n = self.present.get_mut(&a).unwrap();
                *n -= 1;
                if *n == 0 {
                    self.present.remove(&a);
                }
            }
            if now && counted(new) {
                *self.present.entry(a).or_default() += 1;
            }
        }
    }

    fn whole(&self) -> impl Iterator<Item = usize> + '_ {
        self.missing.iter().enumerate().filter(|m| *m.1 == 0).map(|m| m.0)
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

/// One moment's functions, found as in an executable: the bodies of what
/// ran (`facts`), then seeded by `seeds` and the words `ran` then inside
/// no known call, then pointers into code and prologues around each
/// cluster of what they found. Per cluster: its extent and the rows of its
/// functions.
fn moment_functions(
    m: &Moment,
    exes: &[Exe],
    facts: &[Fact],
    seeds: &[(u32, &'static str)],
    ran: &BTreeSet<(u32, u32)>,
    held: Option<&BTreeMap<u32, u32>>,
    switches: &mut Switches,
) -> Vec<(u32, u32, Vec<Value>)> {
    let code = Code { base: 0x8000_0000, bytes: m.ram.clone(), origin: String::new(), gp: None, exe: None };
    let mut a = Analysis::new(&code, None, Box::new(|x| m.allowed(exes, x)));
    for ((at, words), (table, cases)) in switches.iter() {
        if !a.switches.contains_key(at)
            && dispatch_words(&code, *at) == *words
            && cases.iter().all(|&t| m.allowed(exes, t) && code.word(t).is_some_and(sensible))
        {
            a.leaders.extend(cases);
            a.switches.insert(*at, (*table, cases.clone()));
        }
    }
    for (&at, s) in &a.switches {
        switches.entry((at, dispatch_words(&code, at))).or_insert_with(|| s.clone());
    }
    a.continues = m.continues.clone();
    a.fresh = m.fresh.clone();
    a.live = m.live.clone();
    a.ran = held;
    a.place(facts);
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

/// A function found before: start, chunks, bytes and, for a body of what
/// ran, how it is known.
type Known = (u32, Vec<(u32, u32)>, Vec<u8>, Option<&'static str>);

impl Found {
    /// Started by a fact in some moment.
    fn fact(&self) -> bool {
        self.how().is_some()
    }

    /// How a body of what ran is known (see `is_fact`).
    fn how(&self) -> Option<&'static str> {
        let by: Vec<&str> = self.row["found_by"].as_array().into_iter().flatten().filter_map(|h| h.as_str()).collect();
        ["trace_call", "exception", "shared_code", "jumped_into", "entry"].into_iter().find(|h| by.contains(h))
    }

    fn known(&self, start: u32) -> Known {
        (start, row_chunks(&self.row), self.bytes.clone(), self.how())
    }
}

/// A body of what ran found before, placed whole: its chunks' words.
fn carried(start: u32, chunks: &[(u32, u32)], how: &'static str) -> Fact {
    Fact {
        entry: start,
        how,
        words: chunks.iter().flat_map(|&(s, e)| (s..e).step_by(4)).collect(),
        landed: 0,
        jumps: BTreeMap::new(),
        shared_by: BTreeSet::new(),
        live: false,
    }
}

/// Switches found in any moment, by their `jr` and the words their
/// dispatch is read from (`DISPATCH` before it, and it): their table and
/// cases. A moment holding the same words there has the same switch,
/// though it may not hold the page its table is in.
type Switches = BTreeMap<(u32, Vec<Option<u32>>), (u32, Vec<u32>)>;

fn dispatch_words(code: &Code, at: u32) -> Vec<Option<u32>> {
    (0..=DISPATCH).map(|k| code.word(at.wrapping_sub(4 * DISPATCH).wrapping_add(4 * k))).collect()
}

/// An executable's function: its chunks and the calls in it.
type ExeFunction = (Vec<(u32, u32)>, Vec<u32>);

/// Each moment's functions, by start and sha256 (the same function in
/// many moments, runs or snapshots is one; other code loaded at its
/// address is another), named by the file their cluster was loaded from
/// when their bytes are that file's.
struct Finder<'a> {
    exes: &'a [Exe],
    /// Files code is named after: the disc's, the executables' and the BIOS ROMs.
    files: &'a [(String, Vec<u8>)],
    /// The executables' functions: `(chunks, calls in it)`.
    exe_functions: Vec<Vec<ExeFunction>>,
    /// The executables' functions by start and sha256, not listed again.
    exe_keys: BTreeSet<(u32, String)>,
    found: BTreeMap<(u32, String), Found>,
    /// Which file a cluster's first 64 bytes were loaded from.
    named: BTreeMap<(u32, Vec<u8>), Option<Loaded>>,
    /// Functions found before: start, chunks, bytes and, for a body of
    /// what ran, how it is known. One read from bytes is a start in every
    /// moment holding the same bytes there, so the same code is split the
    /// same way whichever moment it is found in; a body of what ran is
    /// carried only into the RAM of a run without a trace.
    known: Vec<Known>,
    /// Switches found before (see `Switches`).
    switches: Switches,
}

impl Finder<'_> {
    /// Calls from the executables' functions a moment holds whole.
    fn exe_calls(&self, m: &Moment) -> Vec<(u32, &'static str)> {
        let mut out = Vec::new();
        for fs in &self.exe_functions {
            for (chunks, calls) in fs {
                if chunks.iter().flat_map(|&(s, e)| (s..e).step_by(4)).all(|x| m.held(self.exes, x)) {
                    out.extend(calls.iter().map(|&t| (t, "call_from_executable")));
                }
            }
        }
        out
    }

    /// Finds a moment's functions and keeps the new ones; `when` names
    /// code no file holds. `held` is the words that ran inside a call as
    /// the moment holds them (see `Analysis::ran`). With `carry` (the RAM
    /// of a run without a trace), the bodies of what ran found before whose
    /// bytes the moment holds are placed.
    #[allow(clippy::too_many_arguments)]
    fn take(
        &mut self,
        m: &Moment,
        facts: &[Fact],
        seeds: &[(u32, &'static str)],
        ran: &BTreeSet<(u32, u32)>,
        when: &str,
        held: Option<&BTreeMap<u32, u32>>,
        carry: bool,
    ) {
        let (mut seeds, mut facts) = (seeds.to_vec(), facts.to_vec());
        let ram = |s: u32, e: u32| m.ram.get((s & 0x1F_FFFF) as usize..(e & 0x1F_FFFF) as usize).map(<[u8]>::to_vec);
        for (s, chunks, b, how) in &self.known {
            let holds = chunks.iter().all(|&(x, y)| ram(x, y).is_some())
                && chunk_bytes(chunks, |x, y| ram(x, y).unwrap_or_default()) == *b;
            if !holds || !m.allowed(self.exes, *s) {
                continue;
            }
            match how {
                None => seeds.push((*s, "same_code")),
                Some(how) if carry => facts.push(carried(*s, chunks, how)),
                _ => {}
            }
        }
        for (lo, hi, rows) in moment_functions(m, self.exes, &facts, &seeds, ran, held, &mut self.switches) {
            let mut file = None;
            for row in rows {
                let s = parse_hex(&row["entry"]);
                let bytes = chunk_bytes(&row_chunks(&row), |x, y| ram(x, y).unwrap_or_default());
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
                    let in_file = |x: u32, y: u32| {
                        let k = x.checked_sub(at)? as usize;
                        b.get(k..k + (y - x) as usize).map(<[u8]>::to_vec)
                    };
                    let chunks = row_chunks(&row);
                    let same = chunks.iter().all(|&(x, y)| in_file(x, y).is_some())
                        && chunk_bytes(&chunks, |x, y| in_file(x, y).unwrap_or_default()) == bytes;
                    same.then(|| (path.to_string(), at, *flo, *fhi))
                });
                let first = format!("code at {}, {when}", hex(lo));
                self.found.insert(key, Found { row, bytes, file, first });
            }
        }
    }
}

/// Finds the functions of each moment of a run's timeline (after each
/// copy): the bodies of what ran inside the calls whose code it holds as
/// it ran, then seeded by the calls into the copies it holds and the code
/// run from them inside no known call at that moment.
fn take_timeline(f: &mut Finder, t: &Timeline) {
    let owner: Vec<Option<usize>> = t.copies.iter().map(|(_, p, b)| resident(f.exes, *p, b)).collect();
    let continuing = t.continuing();
    let mut now: BTreeMap<u32, usize> = BTreeMap::new();
    let mut holding = Holding::new(t);
    for (c, (frame, page, _)) in t.copies.iter().enumerate() {
        let old = now.insert(*page, c);
        holding.change(t, *page, old, c, |k| owner[k].is_some());
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
        // ran with: the moment it first ran. A word run inside a known call
        // is that call's, below.
        let c = c as u32;
        for &k in now.values() {
            let o = (t.copies[k].1 & 0x1F_FFFF) as usize;
            m.ram[o..o + PAGE as usize].copy_from_slice(&t.copies[k].2);
            m.known[o / PAGE as usize] = true;
            m.owner[o / PAGE as usize] = owner[k];
            ran.extend(t.ran[k].iter().filter(|r| !t.owned[k].contains(&r.0)).map(|r| (r.0, r.1)));
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
        // What ran inside each call (and exception) whose words this moment
        // holds all as they ran: in this moment's code, outside the
        // executables.
        let bodies: Vec<Body> = holding
            .whole()
            .map(|g| {
                let x = &t.groups[g];
                Body {
                    entry: x.entry,
                    here: m.allowed(f.exes, x.entry),
                    how: x.how,
                    words: x.words.iter().map(|w| w.1).filter(|&w| m.allowed(f.exes, w)).collect(),
                    landed: x.landed.iter().map(|w| w.1).collect(),
                    jumps: x.jumps.iter().map(|(w, &h)| (w.1, h)).collect(),
                    live: called.contains(&x.entry),
                }
            })
            .filter(|b| !b.words.is_empty())
            .collect();
        let facts = facts(&bodies);
        f.take(&m, &facts, &seeds, &ran, &format!("frame {frame}"), Some(&holding.present), false);
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
            // What ran and was called in the pages holding its code; what
            // ran inside each call (and exception), in its code.
            let (mut ran, mut called) = (BTreeSet::new(), BTreeSet::new());
            let mut bodies: BTreeMap<(bool, u32), Body> = BTreeMap::new();
            for ((_, tl, _), owner) in runs.iter().zip(&owners) {
                let Some(tl) = tl else { continue };
                let mine = |(v, a): (u32, u32)| owner[v as usize] == Some(i) && tl.word(v, a) == code.word(a);
                for g in &tl.groups {
                    let here = mine((g.ver, g.entry));
                    let words: Vec<u32> = g.words.iter().filter(|&&w| mine(w)).map(|w| w.1).collect();
                    if words.is_empty() {
                        continue;
                    }
                    let b = bodies.entry((here, g.entry)).or_insert_with(|| Body {
                        entry: g.entry,
                        here,
                        how: g.how,
                        live: true,
                        ..Default::default()
                    });
                    b.words.extend(words);
                    b.landed.extend(g.landed.iter().filter(|&&w| mine(w)).map(|w| w.1));
                    for (&w, &h) in g.jumps.iter().filter(|(&w, _)| mine(w)) {
                        *b.jumps.entry(w.1).or_default() |= h;
                    }
                }
                for (c, (_, page, bytes)) in tl.copies.iter().enumerate().filter(|&(c, _)| owner[c] == Some(i)) {
                    ran.extend(
                        tl.ran[c]
                            .iter()
                            .filter(|r| !tl.owned[c].contains(&r.0) && code.word(r.0) == Some(r.1))
                            .map(|r| (r.0, r.1)),
                    );
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
            let bodies: Vec<Body> = bodies.into_values().collect();
            a.place(&facts(&bodies));
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
                        let calls = f
                            .words()
                            .filter_map(|pc| code.word(pc).filter(|w| w >> 26 == 3).map(|w| jump_target(pc, w)))
                            .collect();
                        (f.chunks.clone(), calls)
                    })
                    .collect::<Vec<_>>(),
            );
            exe_keys.extend(a.functions.values().map(|f| {
                let bytes = chunk_bytes(&f.chunks, |s, e| exe.payload[(s - lo) as usize..(e - lo) as usize].to_vec());
                (f.start, crate::publish::sha256(&bytes))
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
            switches: BTreeMap::new(),
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
            finder.known = finder.found.iter().map(|((s, _), f)| f.known(*s)).collect();
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
                let name = dir.file_name().unwrap_or_default().to_string_lossy();
                let when = format!("end of run {name}");
                let Some(tl) = tl else {
                    // The bodies of what ran found in the other runs' moments
                    // that its RAM holds.
                    let mut more: Vec<Known> = finder.found.iter().map(|((s, _), f)| f.known(*s)).collect();
                    more.retain(|x| !finder.known.contains(x));
                    finder.known.extend(more);
                    finder.take(&m, &[], &seeds, &BTreeSet::new(), &when, None, true);
                    continue;
                };
                // A traced run's end is one more moment: what ran inside each
                // call whose words RAM holds all as they ran, and no guess
                // takes a word RAM holds as it ran inside a call.
                let (mut held, mut bodies): (BTreeMap<u32, u32>, Vec<Body>) = (BTreeMap::new(), Vec::new());
                for g in &tl.groups {
                    let mut whole = true;
                    for &(v, a) in &g.words {
                        let o = (a & 0x1F_FFFF) as usize;
                        let at = ram.get(o..o + 4).map(|b| u32::from_le_bytes(b.try_into().unwrap()));
                        let same = at.is_some() && tl.word(v, a) == at;
                        whole &= same;
                        if same && m.owner.get(o / PAGE as usize) == Some(&None) && a >= 0x8001_0000 {
                            *held.entry(a).or_default() += 1;
                        }
                    }
                    if whole {
                        bodies.push(Body {
                            entry: g.entry,
                            here: m.allowed(&exes, g.entry),
                            how: g.how,
                            words: g.words.iter().map(|w| w.1).filter(|&w| m.allowed(&exes, w)).collect(),
                            landed: g.landed.iter().map(|w| w.1).collect(),
                            jumps: g.jumps.iter().map(|(w, &h)| (w.1, h)).collect(),
                            live: m.live.as_ref().is_some_and(|l| l.contains(&g.entry)),
                        });
                    }
                }
                bodies.retain(|b| !b.words.is_empty());
                finder.take(&m, &facts(&bodies), &seeds, &BTreeSet::new(), &when, Some(&held), false);
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
            // The bodies of what ran as found; the rest read again.
            let inside = |c: &[(u32, u32)]| c.iter().all(|&(x, y)| x >= lo && y <= hi);
            let placed: Vec<Fact> = keys
                .iter()
                .filter_map(|k| Some(carried(k.0, &row_chunks(&found[k].row), found[k].how()?)))
                .filter(|x| inside(&spans(x.words.iter().copied())))
                .collect();
            a.place(&placed);
            for k in keys.iter().filter(|k| !found[*k].fact()) {
                a.seed(k.0, "same_code");
            }
            a.drain();
            a.guesses();
            let ws = words(&code.bytes, lo);
            while a.pointers(&ws) > 0 {}
            a.prologues();
            while a.pointers(&ws) > 0 {}
            for row in a.report().0 {
                let s = parse_hex(&row["entry"]);
                if starts.contains(&s) {
                    continue;
                }
                let b =
                    chunk_bytes(&row_chunks(&row), |x, y| code.bytes[(x - lo) as usize..(y - lo) as usize].to_vec());
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

    struct Tl {
        copies: Vec<(u64, u32, Vec<u8>)>,
        ran: Vec<Vec<(u32, u32, u32, bool)>>,
        calls: Vec<Vec<(u32, Spans)>>,
    }

    fn timeline_of(t: Tl) -> Timeline {
        Timeline::new(t.copies, t.ran, t.calls, &[])
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
        std::fs::write(dir.join("owners.json"), "[]").unwrap();
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
        let t = timeline_of(Tl {
            ran: vec![[ran(4, &first, 0), ran(6, &first, 0)[4..].to_vec()].concat(), ran(8, &second, 1), vec![]],
            copies: vec![(10, base, first), (20, base, second), (30, base, third)],
            calls: vec![vec![(base + 0x10, vec![(0, 0)])], vec![], vec![]],
        });
        let mut f = Finder {
            exes: &[],
            files: &[],
            exe_functions: Vec::new(),
            exe_keys: BTreeSet::new(),
            found: BTreeMap::new(),
            named: BTreeMap::new(),
            known: Vec::new(),
            switches: BTreeMap::new(),
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
        let t = timeline_of(Tl {
            copies: vec![(10, b, page_b), (20, a, page_a)],
            ran: vec![
                vec![(b, NOP, 1, true), (b + 4, JR_RA, 1, true), (b + 8, NOP, 1, true)],
                vec![(a + 0xFF8, PROLOGUE, 1, false), (a + 0xFFC, NOP, 1, true)],
            ],
            calls: vec![vec![], vec![(a + 0xFF8, vec![(1, 1)])]],
        });
        let mut f = Finder {
            exes: &[],
            files: &[],
            exe_functions: Vec::new(),
            exe_keys: BTreeSet::new(),
            found: BTreeMap::new(),
            named: BTreeMap::new(),
            known: Vec::new(),
            switches: BTreeMap::new(),
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
        let t = timeline_of(Tl {
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
        });
        let mut f = Finder {
            exes: &[],
            files: &[],
            exe_functions: Vec::new(),
            exe_keys: BTreeSet::new(),
            found: BTreeMap::new(),
            named: BTreeMap::new(),
            known: Vec::new(),
            switches: BTreeMap::new(),
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
        let t = timeline_of(Tl {
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
        });
        let mut f = Finder {
            exes: &[],
            files: &[],
            exe_functions: Vec::new(),
            exe_keys: BTreeSet::new(),
            found: BTreeMap::new(),
            named: BTreeMap::new(),
            known: Vec::new(),
            switches: BTreeMap::new(),
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
        let t = timeline_of(Tl {
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
        });
        let mut f = Finder {
            exes: &[],
            files: &[],
            exe_functions: Vec::new(),
            exe_keys: BTreeSet::new(),
            found: BTreeMap::new(),
            named: BTreeMap::new(),
            known: Vec::new(),
            switches: BTreeMap::new(),
        };
        take_timeline(&mut f, &t);
        let got: BTreeSet<(u32, u32)> =
            f.found.values().map(|x| (parse_hex(&x.row["entry"]), parse_hex(&x.row["end"]))).collect();
        assert_eq!(got, BTreeSet::from([(a, a + 0x10), (b, b + 0x14)]));
    }

    fn body(entry: u32, here: bool, words: &[u32], jumps: &[(u32, u8)]) -> Body {
        Body {
            entry,
            here,
            how: "trace_call",
            words: words.iter().copied().collect(),
            jumps: jumps.iter().copied().collect(),
            ..Default::default()
        }
    }

    #[test]
    fn code_run_inside_several_calls_is_split_where_their_entries_are() {
        let x = |o: u32| 0x8001_0000 + o;
        let r = |s: u32, e: u32| (x(s)..x(e)).step_by(4).collect::<Vec<_>>();
        let bodies = [
            // Two calls jumping into one tail.
            body(x(0x100), true, &[r(0x100, 0x108), r(0x200, 0x210)].concat(), &[(x(0x200), 1)]),
            body(x(0x180), true, &[r(0x180, 0x188), r(0x200, 0x210)].concat(), &[(x(0x200), 1)]),
            // A call one word before another's entry, running into it.
            body(x(0x300), true, &r(0x300, 0x310), &[]),
            body(x(0x304), true, &r(0x304, 0x310), &[]),
            // A call in another image jumping into this one.
            body(0x8000_9000, false, &r(0x400, 0x408), &[(x(0x400), 2)]),
        ];
        let got: BTreeMap<u32, (&str, Vec<u32>, Vec<u32>)> = facts(&bodies)
            .into_iter()
            .map(|f| (f.entry, (f.how, f.words.into_iter().collect(), f.shared_by.into_iter().collect())))
            .collect();
        let want = BTreeMap::from([
            (x(0x100), ("trace_call", r(0x100, 0x108), vec![])),
            (x(0x180), ("trace_call", r(0x180, 0x188), vec![])),
            (x(0x200), ("shared_code", r(0x200, 0x210), vec![x(0x100), x(0x180)])),
            (x(0x300), ("trace_call", r(0x300, 0x304), vec![])),
            (x(0x304), ("trace_call", r(0x304, 0x310), vec![])),
            (x(0x400), ("jumped_into", r(0x400, 0x408), vec![0x8000_9000])),
        ]);
        assert_eq!(got, want);
    }

    #[test]
    fn a_body_of_what_ran_is_widened_by_its_bytes_and_keeps_calls_into_what_ran_as_candidates() {
        const ADDIU: u32 = 0x2402_0001;
        // 0: a call that ran 0, 4, 8 and 0x14, skipping the block at 0xC
        // and stopping before its return (cut off); 0x20: another call.
        let c = code(&[PROLOGUE, 0x1480_0003, NOP, ADDIU, ADDIU, ADDIU, JR_RA, NOP, PROLOGUE, JR_RA, NOP]);
        let x = |o: u32| 0x8001_0000 + o;
        let mut a = Analysis::new(&c, None, Box::new(|_| true));
        let fact = |e: u32, ws: &[u32]| Fact {
            entry: x(e),
            how: "trace_call",
            words: ws.iter().map(|&w| x(w)).collect(),
            landed: 0,
            jumps: BTreeMap::new(),
            shared_by: BTreeSet::new(),
            live: true,
        };
        a.place(&[fact(0, &[0, 4, 8, 0x14]), fact(0x20, &[0x20, 0x24, 0x28])]);
        assert_eq!(a.functions[&x(0)].chunks, [(x(0), x(0x20))]);
        assert_eq!(a.functions[&x(0x20)].chunks, [(x(0x20), x(0x2C))]);
        // A call read from bytes onto code that ran in it is a candidate
        // entry.
        a.seed(x(0x14), "call");
        a.guesses();
        assert_eq!(a.functions.len(), 2);
        let (rows, _) = a.report();
        assert_eq!(rows[0]["candidate_entries"], json!(["80010014"]));
        assert_eq!((rows[0]["ends"].as_str(), rows[0]["instructions"].as_u64()), (Some("ran"), Some(8)));
        // One onto what it did not run starts a function there, and the
        // body is read up to it.
        a.seed(x(0x18), "call");
        a.drain();
        assert_eq!(a.functions[&x(0)].chunks, [(x(0), x(0x18))]);
        assert_eq!((a.functions[&x(0x18)].end, a.functions[&x(0x18)].ends), (x(0x20), "return"));
    }

    #[test]
    fn the_cases_of_a_switch_that_did_not_run_are_in_its_body() {
        // `jr v0` to cases by a table: the last case ran, the one after a
        // `j` did not, and nothing branches to it. Its word ran inside
        // another call, in other code once loaded here with the same word.
        const ADDIU: u32 = 0x2402_0001;
        let x = |o: u32| 0x8001_0000 + o;
        let c = code(&[PROLOGUE, 0x0040_0008, NOP, ADDIU, 0x0800_4008, NOP, ADDIU, ADDIU, JR_RA, NOP]);
        let other = BTreeMap::from([(x(0x18), 1)]);
        let mut a = Analysis::new(&c, None, Box::new(|_| true));
        a.ran = Some(&other);
        a.place(&[Fact {
            entry: x(0),
            how: "trace_call",
            words: [0, 4, 8, 0x1C, 0x20, 0x24].iter().map(|&w| x(w)).collect(),
            landed: 0,
            jumps: BTreeMap::new(),
            shared_by: BTreeSet::new(),
            live: true,
        }]);
        assert_eq!(a.functions[&x(0)].chunks, [(x(0), x(0x28))]);
    }

    #[test]
    fn a_switch_is_one_function_with_its_cases_and_its_table_seeds_nothing() {
        let x = |o: u32| 0x8001_0000 + o;
        let mut w = vec![
            PROLOGUE,
            0x2CA2_0003, // sltiu v0, a1, 3
            0x1040_000E, // beq v0, zero, 0x44 (the default)
            0x3C02_8001, // lui v0, 0x8001
            0x2442_0050, // addiu v0, v0, 0x50 (the table)
            0x0005_1880, // sll v1, a1, 2
            0x0062_1821, // addu v1, v1, v0
            0x8C62_0000, // lw v0, 0(v1)
            NOP,
            0x0040_0008, // jr v0
            NOP,
        ];
        w.extend([JR_RA, NOP, JR_RA, NOP, JR_RA, NOP, JR_RA, NOP, NOP]);
        w.extend([x(0x2C), x(0x34), x(0x3C), x(0x2C)]);
        let c = code(&w);
        assert_eq!(dispatch(&c, x(0x24)), Some((x(0x50), 3)));
        let mut a = Analysis::new(&c, Some((x(0), x(0x60))), Box::new(|_| true));
        let table: Vec<(u32, u32)> = (0..4).map(|k| (x(0x50 + 4 * k), w[0x14 + k as usize])).collect();
        a.guessing = true;
        while a.pointers(&table) > 0 {}
        a.prologues();
        let got: Vec<(u32, u32, &str)> = a.functions.values().map(|f| (f.start, f.end, f.ends)).collect();
        assert_eq!(got, [(x(0), x(0x4C), "return")]);
        let (rows, _) = a.report();
        assert_eq!(rows[0]["switches"], json!([{"jr": "80010024", "table": "80010050", "cases": 3}]));
        // Without its bound, the table's length is not known: no switch.
        let mut w = w.clone();
        w[1] = NOP;
        assert_eq!(dispatch(&code(&w), x(0x24)), None);
    }

    #[test]
    fn a_switch_whose_table_is_in_a_saved_register_is_read_from_its_function() {
        let x = |o: u32| 0x8001_0000 + o;
        let w = |s3_again: bool| {
            let mut w = vec![
                PROLOGUE,
                0x3C02_8001, // lui v0, 0x8001
                0x2453_0100, // addiu s3, v0, 0x100 (the table)
                jal(x(0x200)),
                NOP,
                0x0800_4008, // j 0x20, as a switch's case goes on
                NOP,
                if s3_again { 0x2413_0005 } else { NOP }, // li s3, 5
                0x2CA2_0002,                              // 0x20: sltiu v0, a1, 2
                0x1040_0008,                              // beq v0, zero, (the default)
                0x0005_1080,                              // sll v0, a1, 2
                0x0053_1021,                              // addu v0, v0, s3
                0x8C42_0000,                              // lw v0, 0(v0)
                NOP,
                0x0040_0008, // 0x38: jr v0
                NOP,
                JR_RA,
                NOP,
                JR_RA,
                NOP,
            ];
            w.resize(0x40, NOP);
            w.extend([x(0x40), x(0x48)]);
            code(&w)
        };
        assert_eq!(dispatch(&w(false), x(0x38)), Some((x(0x100), 2)));
        assert_eq!(dispatch(&w(true), x(0x38)), None);
    }

    #[test]
    fn a_switch_is_known_in_a_moment_without_its_table_by_its_bytes() {
        // Its default is past the next function: only the table says the
        // cases after its `jr` are its own.
        let x = |o: u32| 0x8010_0000 + o;
        let w = [
            PROLOGUE,
            0x2CA2_0003, // sltiu v0, a1, 3
            0x1040_0010, // beq v0, zero, 0x4C (the default)
            0x3C02_8010, // lui v0, 0x8010
            0x2442_1000, // addiu v0, v0, 0x1000 (the table, in the next page)
            0x0005_1880, // sll v1, a1, 2
            0x0062_1821, // addu v1, v1, v0
            0x8C62_0000, // lw v0, 0(v1)
            NOP,
            0x0040_0008, // jr v0
            NOP,
            JR_RA,
            NOP,
            JR_RA,
            NOP,
            JR_RA,
            NOP,
            JR_RA, // 0x44: another function
            NOP,
            JR_RA, // 0x4C: the default
            NOP,
        ];
        let moment = |table: bool| {
            let mut m = Moment {
                ram: vec![0xFF; 0x20_0000],
                known: vec![false; 0x200],
                owner: vec![None; 0x200],
                continues: BTreeSet::new(),
                fresh: None,
                live: None,
            };
            for (k, v) in w.iter().enumerate() {
                m.ram[0x10_0000 + 4 * k..][..4].copy_from_slice(&v.to_le_bytes());
            }
            m.known[0x100] = true;
            if table {
                for (k, v) in [x(0x2C), x(0x34), x(0x3C)].iter().enumerate() {
                    m.ram[0x10_1000 + 4 * k..][..4].copy_from_slice(&v.to_le_bytes());
                }
                m.known[0x101] = true;
            }
            m
        };
        let end = |m: &Moment, switches: &mut Switches| {
            let seeds = [(x(0), "call"), (x(0x44), "call")];
            let rows = moment_functions(m, &[], &[], &seeds, &BTreeSet::new(), None, switches);
            rows.iter().flat_map(|c| &c.2).find(|r| r["entry"] == "80100000").map(|r| parse_hex(&r["end"]))
        };
        assert_eq!(end(&moment(false), &mut Switches::new()), Some(x(0x2C)));
        let mut switches = Switches::new();
        assert_eq!(end(&moment(true), &mut switches), Some(x(0x44)));
        assert_eq!(end(&moment(false), &mut switches), Some(x(0x44)));
    }

    #[test]
    fn stubs_jumping_through_a_register_are_each_a_function() {
        // Three stubs into the BIOS (`addiu t2, zero, 0xA0; jr t2; addiu
        // t1, zero, n; nop`); only the first ran.
        let stub = |n: u32| [0x240A_00A0, 0x0140_0008, 0x2409_0000 | n, NOP];
        let c = code(&[stub(1), stub(2), stub(3)].concat());
        let x = |o: u32| 0x8001_0000 + o;
        let mut a = Analysis::new(&c, None, Box::new(|_| true));
        a.place(&[Fact {
            entry: x(0),
            how: "trace_call",
            words: [0, 4, 8].iter().map(|&w| x(w)).collect(),
            landed: 0,
            jumps: BTreeMap::new(),
            shared_by: BTreeSet::new(),
            live: true,
        }]);
        a.guesses();
        a.region = Some((x(0), x(0x30)));
        a.prologues();
        let got: Vec<(u32, u32, &str)> = a.functions.values().map(|f| (f.start, f.end, f.ends)).collect();
        assert_eq!(
            got,
            [(x(0), x(0xC), "ran"), (x(0x10), x(0x1C), "register_jump"), (x(0x20), x(0x2C), "register_jump")]
        );
    }

    #[test]
    fn a_landing_is_its_resume_points_call_across_copies_that_changed_only_beside_it() {
        // A call runs 0..0x1C (its branch at 0xC not taken) and returns.
        // Later a variable beside it changes (copy 1) and a landing resumes
        // at 0xC, where the call's own call returned through the stack:
        // it takes the branch, to 0x1C. Then the code at 0x20 changes (copy
        // 2) and nothing runs: the call is not there whole any more.
        const ADDIU: u32 = 0x2402_0001;
        let p = 0x8010_0000u32;
        let ws = [PROLOGUE, 0x0C04_4000, NOP, 0x1480_0003, NOP, JR_RA, NOP, ADDIU, JR_RA, NOP];
        let page = |var: u8, w8: u32| {
            let mut b = vec![0u8; 0x1000];
            for (k, w) in ws.iter().enumerate() {
                b[4 * k..4 * k + 4].copy_from_slice(&w.to_le_bytes());
            }
            b[0x20..0x24].copy_from_slice(&w8.to_le_bytes());
            b[0x800] = var;
            b
        };
        let run = |c: u32, ks: &[u32], m: u32| -> Vec<(u32, u32, u32, bool)> {
            ks.iter().map(|&k| (p + 4 * k, ws[k as usize], m, k != 0 && c == 0)).collect()
        };
        let words = |c: u32, ks: &[u32]| ks.iter().map(|&k| (c, p + 4 * k)).collect::<Vec<_>>();
        let owners = [
            OwnerRow {
                kind: "call".into(),
                entry: Some((0, p)),
                body: words(0, &[0, 1, 2, 3, 4, 5, 6]),
                ..Default::default()
            },
            OwnerRow {
                kind: "landed".into(),
                entry: Some((1, p + 0xC)),
                body: words(1, &[3, 4, 7, 8, 9]),
                returned_inside: vec![0],
                ..Default::default()
            },
        ];
        let t = Timeline::new(
            vec![(10, p, page(0, JR_RA)), (20, p, page(1, JR_RA)), (30, p, page(1, ADDIU))],
            vec![run(0, &[0, 1, 2, 3, 4, 5, 6], 0), run(1, &[3, 4, 7, 8, 9], 1), vec![]],
            vec![vec![(p, vec![(0, 0)])], vec![], vec![]],
            &owners,
        );
        assert_eq!(
            (t.version(1, p + 0x1C), t.version(1, p + 0x800), t.version(2, p + 0x20)),
            (Some(0), Some(1), Some(2))
        );
        assert_eq!(t.groups.len(), 1);
        assert_eq!(t.groups[0].landed, BTreeSet::from([(0, p + 0x1C), (0, p + 0x20), (0, p + 0x24)]));
        let mut f = Finder {
            exes: &[],
            files: &[],
            exe_functions: Vec::new(),
            exe_keys: BTreeSet::new(),
            found: BTreeMap::new(),
            named: BTreeMap::new(),
            known: Vec::new(),
            switches: BTreeMap::new(),
        };
        take_timeline(&mut f, &t);
        let got: Vec<(u32, u32, Value)> = f
            .found
            .values()
            .map(|x| (parse_hex(&x.row["entry"]), parse_hex(&x.row["end"]), x.row["landed_words"].clone()))
            .collect();
        assert_eq!(got, [(p, p + 0x28, json!(3))]);
    }

    #[test]
    fn code_loaded_again_at_the_same_place_is_one_function_of_what_ran_in_each_load() {
        // A routine that jumps by `jr t0`: the first load's call runs the
        // way to 0x8, the second's (the same bytes, loaded again after other
        // code was there) the way to 0x14. A third load runs nothing. Each
        // load is the routine with what ran in both.
        const ADDIU: u32 = 0x2402_0001;
        const JR_T0: u32 = 0x0100_0008;
        let p = 0x8010_0000u32;
        let ws = [JR_T0, NOP, ADDIU, JR_RA, NOP, ADDIU, JR_RA, NOP];
        let mut code = vec![0u8; 0x1000];
        for (k, w) in ws.iter().enumerate() {
            code[4 * k..4 * k + 4].copy_from_slice(&w.to_le_bytes());
        }
        let run = |ks: &[u32], m: u32| -> Vec<(u32, u32, u32, bool)> {
            ks.iter().map(|&k| (p + 4 * k, ws[k as usize], m, false)).collect()
        };
        let words = |c: u32, ks: &[u32]| ks.iter().map(|&k| (c, p + 4 * k)).collect::<Vec<_>>();
        let owners = [
            OwnerRow {
                kind: "call".into(),
                entry: Some((0, p)),
                body: words(0, &[0, 1, 2, 3, 4]),
                ..Default::default()
            },
            OwnerRow {
                kind: "call".into(),
                entry: Some((2, p)),
                body: words(2, &[0, 1, 5, 6, 7]),
                ..Default::default()
            },
        ];
        let t = Timeline::new(
            vec![
                (10, p, code.clone()),
                (20, p, vec![0u8; 0x1000]),
                (30, p, code.clone()),
                (40, p, vec![0u8; 0x1000]),
                (50, p, code),
            ],
            vec![run(&[0, 1, 2, 3, 4], 0), vec![], run(&[0, 1, 5, 6, 7], 2), vec![], vec![]],
            vec![vec![(p, vec![(0, 0)])], vec![], vec![(p, vec![(2, 2)])], vec![], vec![]],
            &owners,
        );
        assert_eq!(t.groups.len(), 2, "two loads, two lines of copies");
        let mut f = Finder {
            exes: &[],
            files: &[],
            exe_functions: Vec::new(),
            exe_keys: BTreeSet::new(),
            found: BTreeMap::new(),
            named: BTreeMap::new(),
            known: Vec::new(),
            switches: BTreeMap::new(),
        };
        take_timeline(&mut f, &t);
        let got: Vec<(u32, u32, Value)> = f
            .found
            .values()
            .map(|x| (parse_hex(&x.row["entry"]), parse_hex(&x.row["end"]), x.row["chunks"].clone()))
            .collect();
        assert_eq!(got, [(p, p + 0x20, Value::Null)]);
    }
}
