//! Static code reading for `psx code disasm` and `psx code xrefs`: code from a PS-X EXE
//! or from a run's RAM (overlays, code loaded at run time), function bounds
//! by heuristics, address resolution of `lui`+`addiu/ori/load/store` pairs
//! and `gp`-relative accesses, and cross-references.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use crate::Error;
use crate::commands::{Ctx, Op, Outcome};
use psx_core::disasm::disasm;
use psx_media::PsxExe;
use psx_media::memmap::hex;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{Value, json};
use std::path::PathBuf;

#[derive(clap::Args, Deserialize, JsonSchema)]
pub struct DisasmArgs {
    /// PS-X EXE file, or `image.cue::/PATH`. Optional with --ram.
    pub source: Option<String>,
    /// Start address (hex); defaults to the entry point.
    #[arg(long)]
    pub addr: Option<String>,
    /// Maximum instructions to list; defaults to 32, or the discovered function with --function.
    #[arg(long)]
    pub count: Option<u32>,
    /// Instruction budget for finding function boundaries, independent of --count (1..=262144).
    #[arg(long, default_value_t = d_function_budget())]
    #[serde(default = "d_function_budget")]
    pub function_budget: u32,
    /// `.sym` or `.symbols.json` files for labels; repeatable.
    #[arg(long = "symbols")]
    #[serde(default)]
    pub symbols: Vec<PathBuf>,
    /// Plain-text listing (`ADDR WORD asm ; notes`, with labels for symbols and
    /// branch targets) instead of JSON lines.
    #[arg(long)]
    #[serde(default)]
    pub text: bool,
    /// Disassemble the whole function containing this hex address: back to the
    /// nearest jal target or `addiu sp,sp,-N` prologue, forward to its final `jr ra`.
    #[arg(long)]
    pub function: Option<String>,
    /// Read code from a run's RAM (run directory or ram.bin) instead of the EXE:
    /// overlays and code loaded at run time. With an EXE source too, warns where they differ.
    #[arg(long)]
    pub ram: Option<PathBuf>,
    /// gp value (hex) for resolving gp-relative loads/stores (default: the EXE header's gp).
    #[arg(long)]
    pub gp: Option<String>,
}

fn d_function_budget() -> u32 {
    4096
}

impl Op for DisasmArgs {
    fn exec(self, _: &Ctx) -> Outcome {
        let text = self.text;
        Outcome::result_text(disasm_cmd(self), text)
    }
}

#[derive(clap::Args, Deserialize, JsonSchema)]
pub struct XrefsArgs {
    /// Target: a hex address, or a hex range LO-HI (HI exclusive) such as a whole struct.
    pub target: String,
    /// PS-X EXE file, or `image.cue::/PATH` (scans its load range). Optional with --ram.
    #[arg(long)]
    pub source: Option<String>,
    /// Scan a run's RAM (run directory or ram.bin) instead of the EXE: all 2 MiB.
    #[arg(long)]
    pub ram: Option<PathBuf>,
    /// gp value (hex) for gp-relative accesses (default: the EXE header's gp, when an EXE is given).
    #[arg(long)]
    pub gp: Option<String>,
    /// Skip data words equal to the target (pointer and jump tables).
    #[arg(long)]
    #[serde(default)]
    pub no_data: bool,
    /// Keep only this kind (load, store, call, jump, branch, addr, data); repeatable.
    #[arg(long = "kind")]
    #[serde(default)]
    pub kinds: Vec<String>,
    /// Hex mask: only loads testing these bits of the target word.
    #[arg(long)]
    pub bits: Option<String>,
    /// `.sym` or `.symbols.json` files to name the referencing functions; repeatable.
    #[arg(long = "symbols")]
    #[serde(default)]
    pub symbols: Vec<PathBuf>,
    /// References listed individually (counts are always complete).
    #[arg(long, default_value_t = d_xref_limit())]
    #[serde(default = "d_xref_limit")]
    pub limit: usize,
    #[arg(long, default_value_t = 0)]
    #[serde(default)]
    pub offset: usize,
    /// Plain-text listing instead of JSON.
    #[arg(long)]
    #[serde(default)]
    pub text: bool,
}

fn d_xref_limit() -> usize {
    200
}

impl Op for XrefsArgs {
    fn exec(self, _: &Ctx) -> Outcome {
        let XrefsArgs { target, source, ram, gp, no_data, kinds, bits, symbols, limit, offset, text } = self;
        Outcome::result_text(
            (|| {
                crate::page::validate(limit).map_err(Error::invalid)?;
                let mut code = Code::open(source.as_deref(), ram.as_deref())?;
                if let Some(g) = crate::commands::opt_hex(&gp, "gp")? {
                    code.gp = Some(g);
                }
                let addr = |s: &str| {
                    psx_media::memmap::parse_hex(s).ok_or_else(|| Error::invalid(format!("bad hex address {s:?}")))
                };
                let (lo, hi) = match target.split_once('-') {
                    Some((l, h)) => (addr(l)?, addr(h)?),
                    None => {
                        let a = addr(&target)?;
                        (a, a.wrapping_add(1))
                    }
                };
                if hi <= lo {
                    return Err(Error::invalid(format!("empty range {target:?}")));
                }
                const KINDS: [&str; 7] = ["load", "store", "call", "jump", "branch", "addr", "data"];
                if let Some(k) = kinds.iter().find(|k| !KINDS.contains(&k.as_str())) {
                    return Err(Error::invalid(format!("kind {k:?} is not one of {}", KINDS.join(", "))));
                }
                let bits = crate::commands::opt_hex(&bits, "bits")?;
                if bits == Some(0) {
                    return Err(Error::invalid("bits needs at least one bit"));
                }
                let filter = Filter { kinds, bits };
                let scopes = crate::load_symbols(&symbols)?;
                let (mut v, t) = xref_report(&code, lo, hi, !no_data, &filter, &scopes, offset, limit, text);
                if text {
                    v["text"] = json!(t);
                    v.as_object_mut().unwrap().remove("refs");
                    v.as_object_mut().unwrap().remove("functions");
                }
                Ok(v)
            })(),
            text,
        )
    }
}

/// Bytes of code at a KSEG0 base address.
pub struct Code {
    pub base: u32,
    pub bytes: Vec<u8>,
    /// Where the bytes came from (EXE source or ram.bin path).
    pub origin: String,
    /// The EXE header's gp, when the code is an EXE.
    pub gp: Option<u32>,
    /// The EXE, when RAM is read but an EXE was also given (for a residency check).
    pub exe: Option<(u32, Vec<u8>)>,
}

const JR_RA: u32 = 0x03E0_0008;

impl Code {
    /// `source` (EXE file or `image.cue::/PATH`) and/or `ram` (run directory or ram.bin).
    pub fn open(source: Option<&str>, ram: Option<&Path>) -> crate::Result<Code> {
        let exe = match source {
            Some(s) => {
                let bytes = psx_media::source::read(s)?;
                let exe = PsxExe::parse(&bytes)?;
                Some((s.to_string(), exe.clone(), exe.payload(&bytes).to_vec()))
            }
            None => None,
        };
        match (ram, exe) {
            (Some(r), exe) => {
                let snap = crate::ram::load(r)?;
                // gp: the EXE header's, else the CPU's at the end of the run (report.json of a native run).
                let run_gp = || {
                    let text = std::fs::read_to_string(r.join("report.json")).ok()?;
                    let v: Value = serde_json::from_str(&text).ok()?;
                    crate::commands::addr_value(&v["registers"]["gp"]).filter(|g| g >> 28 == 8)
                };
                Ok(Code {
                    base: 0x8000_0000,
                    bytes: snap.bytes,
                    origin: snap.path.display().to_string(),
                    gp: exe.as_ref().map(|e| e.1.gp).filter(|&g| g != 0).or_else(run_gp),
                    exe: exe.map(|(_, h, p)| (0x8000_0000 | (h.text_addr & 0x1F_FFFF), p)),
                })
            }
            (None, Some((s, h, p))) => Ok(Code {
                base: 0x8000_0000 | (h.text_addr & 0x1F_FFFF),
                bytes: p,
                origin: s,
                gp: (h.gp != 0).then_some(h.gp),
                exe: None,
            }),
            (None, None) => Err(Error::invalid("give an EXE source and/or ram (a run directory or ram.bin)")),
        }
    }

    fn name(&self, scopes: &[crate::ScopedSymbols], addr: u32, distance: u32) -> Option<String> {
        scopes.iter().find_map(|s| s.describe_image(addr, self.base, &self.bytes, distance))
    }

    pub fn end(&self) -> u32 {
        self.base.wrapping_add(self.bytes.len() as u32)
    }

    fn offset(&self, addr: u32) -> Option<usize> {
        let p = addr & 0x1FFF_FFFF;
        if p >= 0x0080_0000 {
            return None;
        }
        let p = p & 0x1F_FFFF;
        let b = self.base & 0x1F_FFFF;
        let o = p.checked_sub(b)? as usize;
        (o + 4 <= self.bytes.len()).then_some(o)
    }

    pub fn word(&self, addr: u32) -> Option<u32> {
        let o = self.offset(addr & !3)?;
        Some(u32::from_le_bytes(self.bytes[o..o + 4].try_into().unwrap()))
    }

    /// Words in `[lo, hi)` where RAM differs from the EXE (only when both were given).
    pub fn exe_mismatch(&self, lo: u32, hi: u32) -> Option<(u32, u32)> {
        let (base, payload) = self.exe.as_ref()?;
        let (mut checked, mut differ) = (0, 0);
        let mut a = lo & !3;
        while a < hi {
            let p = (a & 0x1F_FFFF).wrapping_sub(base & 0x1F_FFFF) as usize;
            if let (Some(e), Some(r)) = (payload.get(p..p + 4), self.word(a)) {
                checked += 1;
                differ += (u32::from_le_bytes(e.try_into().unwrap()) != r) as u32;
            }
            a += 4;
        }
        Some((checked, differ))
    }

    fn words(&self) -> impl Iterator<Item = (u32, u32)> + '_ {
        self.bytes
            .as_chunks::<4>()
            .0
            .iter()
            .enumerate()
            .map(|(i, w)| (self.base.wrapping_add(4 * i as u32), u32::from_le_bytes(*w)))
    }

    /// Every `jal` target in the code.
    pub fn jal_targets(&self) -> BTreeSet<u32> {
        self.words()
            .filter(|&(_, i)| i >> 26 == 3)
            .map(|(pc, i)| jump_target(pc, i))
            .filter(|&t| self.word(t).is_some())
            .collect()
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

/// KSEG0/KSEG1 main RAM, scratchpad or I/O: worth reporting as an address.
fn plausible_address(a: u32) -> bool {
    let seg = a >> 29;
    let p = a & 0x1FFF_FFFF;
    (seg == 4 || seg == 5) && p < 0x0080_0000
        || (0x1F80_0000..0x1F80_3000).contains(&p) && (seg == 0 || seg == 4 || seg == 5)
}

/// `addiu sp, sp, -N`.
fn is_prologue(i: u32) -> bool {
    i >> 16 == 0x27BD && (i & 0x8000) != 0
}

/// Register written by an instruction (approximate, for invalidating `lui` tracking).
fn dest_reg(i: u32) -> Option<u32> {
    let op = i >> 26;
    match op {
        0x00 => {
            let f = i & 0x3F;
            match f {
                0x08 | 0x0C | 0x0D | 0x11 | 0x13 | 0x18..=0x1B => None,
                _ => Some((i >> 11) & 31),
            }
        }
        0x03 => Some(31),
        0x08..=0x0F | 0x20..=0x26 => Some((i >> 16) & 31),
        0x10 | 0x12 => match (i >> 21) & 31 {
            0x00 | 0x02 => Some((i >> 16) & 31),
            _ => None,
        },
        _ => None,
    }
}

#[derive(Clone, Debug)]
pub struct Ref {
    /// Resolved address.
    pub target: u32,
    /// `call` (jal), `jump` (j), `branch`, `load`, `store`, `addr` (address formed with addiu/ori), `data` (a word equal to the address).
    pub kind: &'static str,
    /// How the address was formed: `lui+addiu`, `gp+imm`, ...
    pub via: &'static str,
}

/// Address resolution over a straight run of instructions: tracks `lui` values
/// per register (and `gp`), resetting at function returns.
pub struct Resolver {
    hi: [Option<u32>; 32],
    gp: Option<u32>,
    /// Also an address built in KUSEG (`lui 0x8` for 0x0008xxxx), which
    /// main RAM mirrors: kept where a range is asked for, which filters the
    /// constants it lets through.
    kuseg: bool,
}

impl Resolver {
    pub fn new(gp: Option<u32>) -> Self {
        Resolver { hi: [None; 32], gp, kuseg: false }
    }

    pub fn reset(&mut self) {
        self.hi = [None; 32];
    }

    /// References made by instruction `i` at `pc` (and update the tracking).
    pub fn step(&mut self, pc: u32, i: u32) -> Vec<Ref> {
        let op = i >> 26;
        let rs = ((i >> 21) & 31) as usize;
        let rt = ((i >> 16) & 31) as usize;
        let simm = (i & 0xFFFF) as i16 as i32 as u32;
        let mut out = Vec::new();
        match op {
            0x02 => out.push(Ref { target: jump_target(pc, i), kind: "jump", via: "j" }),
            0x03 => out.push(Ref { target: jump_target(pc, i), kind: "call", via: "jal" }),
            _ => {
                if let Some(t) = branch_target(pc, i) {
                    out.push(Ref { target: t, kind: "branch", via: "branch" });
                }
            }
        }
        let base = |r: usize| -> Option<(u32, &'static str)> {
            if r == 28 {
                if let Some(g) = self.gp {
                    return Some((g, "gp+imm"));
                }
            }
            self.hi[r].map(|h| (h, "lui+imm"))
        };
        match op {
            0x09 | 0x0D if rs != 0 => {
                if let Some((b, via)) = base(rs) {
                    let t = if op == 0x0D { b | (i & 0xFFFF) } else { b.wrapping_add(simm) };
                    // Constants built with lui+ori are only addresses when they point into memory.
                    if plausible_address(t) || self.kuseg && t < 0x0020_0000 {
                        out.push(Ref {
                            target: t,
                            kind: "addr",
                            via: if via == "gp+imm" {
                                "gp+imm"
                            } else if op == 0x0D {
                                "lui+ori"
                            } else {
                                "lui+addiu"
                            },
                        });
                    }
                }
            }
            0x20..=0x26 | 0x32 => {
                if let Some((b, via)) = base(rs) {
                    out.push(Ref { target: b.wrapping_add(simm), kind: "load", via });
                }
            }
            0x28..=0x2B | 0x2E | 0x3A => {
                if let Some((b, via)) = base(rs) {
                    out.push(Ref { target: b.wrapping_add(simm), kind: "store", via });
                }
            }
            _ => {}
        }
        // Update tracking after using the old values.
        if op == 0x0F {
            self.hi[rt] = Some((i & 0xFFFF) << 16);
        } else if let Some(d) = dest_reg(i) {
            if d != 0 {
                self.hi[d as usize] = None;
            }
        }
        out
    }
}

/// Function bounds: walk back from `addr` to the nearest `jal` target or
/// `addiu sp, sp, -N` prologue, then forward through whole functions (each
/// ending after the `jr ra` + delay slot that no earlier branch jumps past, a
/// tail `j` out of the function, or the next prologue/jal target) until one
/// contains `addr`. Returns `[start, end)`.
#[derive(Clone, Debug)]
pub struct FunctionExtent {
    pub start: u32,
    pub end: u32,
    pub start_evidence: &'static str,
    pub stop_reason: &'static str,
}
impl FunctionExtent {
    pub fn truncated(&self) -> bool {
        matches!(self.stop_reason, "search_limit" | "image_end") || self.start_evidence == "backward_search_limit"
    }
}

pub fn function_bounds(code: &Code, addr: u32, jal: &BTreeSet<u32>, max: u32) -> Option<(u32, u32)> {
    function_extent(code, addr, jal, max).map(|f| (f.start, f.end))
}

pub fn function_extent(code: &Code, addr: u32, jal: &BTreeSet<u32>, max: u32) -> Option<FunctionExtent> {
    let addr = addr & !3;
    code.word(addr)?;
    let mut s = addr;
    let mut start_evidence = "backward_search_limit";
    for _ in 0..max {
        if jal.contains(&s) {
            start_evidence = "jal_target";
            break;
        }
        if code.word(s).is_some_and(is_prologue) {
            start_evidence = "stack_prologue";
            break;
        }
        match s.checked_sub(4).filter(|&p| code.word(p).is_some()) {
            Some(p) => s = p,
            None => {
                start_evidence = "image_start";
                break;
            }
        }
    }
    loop {
        let (e, stop_reason) = function_end(code, s, jal, max);
        if addr < e {
            return Some(FunctionExtent { start: s, end: e, start_evidence, stop_reason });
        }
        s = e;
        while s < addr && code.word(s) == Some(0) {
            s += 4;
        }
        start_evidence = "sequential_heuristic";
        if e == s && s > addr {
            let (end, stop_reason) = function_end(code, addr, jal, max);
            return Some(FunctionExtent { start: addr, end, start_evidence, stop_reason });
        }
    }
}

fn function_end(code: &Code, start: u32, jal: &BTreeSet<u32>, max: u32) -> (u32, &'static str) {
    let mut pc = start;
    let mut reach = start;
    for _ in 0..max {
        let Some(i) = code.word(pc) else { return (pc.max(start + 4), "image_end") };
        if pc != start && pc > reach && (jal.contains(&pc) || is_prologue(i)) {
            return (pc, "next_entry");
        }
        if let Some(t) = branch_target(pc, i) {
            if t > reach && t.wrapping_sub(start) < 0x10_0000 {
                reach = t;
            }
        }
        let tail_jump = i >> 26 == 0x02 && {
            let t = jump_target(pc, i);
            t < start || t > pc + 8
        };
        if (i == JR_RA || tail_jump) && reach <= pc + 4 {
            if code.word(pc + 4).is_none() {
                return (pc + 4, "image_end");
            }
            return (pc + 8, if i == JR_RA { "return" } else { "tail_jump" });
        }
        pc += 4;
    }
    (pc.max(start + 4), "search_limit")
}

pub struct ListOpts<'a> {
    pub scopes: &'a [crate::ScopedSymbols],
    pub text: bool,
}

/// Disassemble `[lo, hi)`: JSON lines (addr, word, asm, label, refs) and, for `--text`,
/// an assembler-style listing with labels for branch targets and resolved addresses.
pub fn listing(code: &Code, lo: u32, hi: u32, o: &ListOpts) -> (Vec<Value>, String) {
    let label = |a: u32| code.name(o.scopes, a, 0);
    let name = |a: u32| code.name(o.scopes, a, 0x4000);
    let mut targets = BTreeSet::new();
    let mut a = lo;
    while a < hi {
        if let Some(i) = code.word(a) {
            if let Some(t) = branch_target(a, i) {
                targets.insert(t);
            }
            if i >> 26 == 0x02 {
                targets.insert(jump_target(a, i));
            }
        }
        a += 4;
    }
    let mut res = Resolver::new(code.gp);
    let mut lines = Vec::new();
    let mut text = String::new();
    let mut a = lo;
    let mut after_return = false;
    while a < hi {
        let Some(i) = code.word(a) else { break };
        let refs = res.step(a, i);
        let lbl = label(a);
        let asm = disasm(i, a);
        let mut notes: Vec<String> = Vec::new();
        for r in &refs {
            match r.kind {
                "call" | "jump" => {
                    if let Some(n) = name(r.target) {
                        notes.push(n);
                    }
                }
                "branch" => {}
                _ => notes.push(format!(
                    "{} 0x{:08X}{}",
                    r.kind,
                    r.target,
                    name(r.target).map(|n| format!(" {n}")).unwrap_or_default()
                )),
            }
        }
        let mut v = json!({"addr": format!("0x{a:08X}"), "word": format!("{i:08X}"), "asm": asm, "label": lbl});
        let data_refs: Vec<Value> = refs
            .iter()
            .filter(|r| !matches!(r.kind, "branch" | "call" | "jump"))
            .map(|r| json!({"kind": r.kind, "via": r.via, "target": format!("0x{:08X}", r.target)}))
            .collect();
        if !data_refs.is_empty() {
            v["refs"] = json!(data_refs);
        }
        if o.text {
            if let Some(l) = &lbl {
                text += &format!("{l}:\n");
            } else if targets.contains(&a) {
                text += &format!("loc_{a:08X}:\n");
            } else if a > lo && code.word(a.wrapping_sub(8)) == Some(JR_RA) {
                text.push('\n'); // after a return's delay slot
            }
            let asm_l = match refs.iter().find(|r| r.kind == "branch") {
                Some(r) if targets.contains(&r.target) && label(r.target).is_none() => {
                    asm.replace(&format!("0x{:08X}", r.target), &format!("loc_{:08X}", r.target))
                }
                _ => asm,
            };
            let note = if notes.is_empty() { String::new() } else { format!("  ; {}", notes.join(", ")) };
            text += &format!("  {a:08X}  {i:08X}  {asm_l:<32}{note}\n");
        }
        lines.push(v);
        // A return ends the straight-line tracking after its delay slot.
        if after_return {
            res.reset();
        }
        after_return = i == JR_RA;
        a += 4;
    }
    (lines, text.lines().map(|l| l.trim_end().to_string() + "\n").collect())
}

/// Every reference to `[lo, hi)` in the code: jal/j/branch targets, `lui`
/// pairs and `gp`-relative accesses (tracked within straight-line code), and
/// aligned data words equal to an address in the range (pointer tables).
pub fn xrefs(code: &Code, lo: u32, hi: u32, data: bool) -> Vec<(u32, Ref)> {
    let norm = |a: u32| a & 0x1FFF_FFFF;
    let (l, h) = (norm(lo), norm(hi));
    let inside = |a: u32| (l..h).contains(&norm(a));
    let mut out = Vec::new();
    let mut res = Resolver { kuseg: true, ..Resolver::new(code.gp) };
    let mut after_return = false;
    for (pc, i) in code.words() {
        for r in res.step(pc, i) {
            if inside(r.target) {
                out.push((pc, r));
            }
        }
        if data && (0x8000_0000..0x8020_0000).contains(&i) && inside(i) {
            out.push((pc, Ref { target: i, kind: "data", via: "word" }));
        }
        if after_return {
            res.reset();
        }
        after_return = i == JR_RA;
    }
    out
}

/// Which references xrefs keeps: by kind, and loads by the bits tested.
#[derive(Default)]
pub struct Filter {
    pub kinds: Vec<String>,
    /// A mask of the word at the range's start.
    pub bits: Option<u32>,
}

/// The bits of `mask` (of the word at `lo`) that the value loaded at `pc`
/// is tested with: by andi, or and with a constant, on the loaded register or
/// a copy of it, in the few instructions after the load and before the
/// register is written again.
fn tested_bits(code: &Code, pc: u32, target: u32, lo: u32, mask: u32) -> u32 {
    let Some(i) = code.word(pc) else { return 0 };
    let k = (target & 0x1FFF_FFFF).wrapping_sub(lo & 0x1FFF_FFFF);
    if !(0x20..=0x26).contains(&(i >> 26)) || k >= 4 {
        return 0;
    }
    let mask = mask >> (8 * k);
    let mut held = 1u32 << ((i >> 16) & 31);
    let mut konst: [Option<u32>; 32] = [None; 32];
    let mut tested = 0;
    for n in 1..=8 {
        let Some(w) = code.word(pc.wrapping_add(4 * n)) else { break };
        let (op, rs, rt, rd) = (w >> 26, (w >> 21) & 31, (w >> 16) & 31, (w >> 11) & 31);
        let now = held;
        let has = move |r: u32| r != 0 && now & (1 << r) != 0;
        let (imm, funct) = (w & 0xFFFF, w & 0x3F);
        if op == 0x0C && has(rs) {
            tested |= imm & mask;
        }
        if op == 0 && funct == 0x24 {
            for (a, b) in [(rs, rt), (rt, rs)] {
                if has(a) {
                    tested |= konst[b as usize].unwrap_or(0) & mask;
                }
            }
        }
        // A copy (addu/or with zero) holds the value too.
        let copy = op == 0 && matches!(funct, 0x21 | 0x25) && ((has(rs) && rt == 0) || (has(rt) && rs == 0));
        // The register this instruction writes, if any.
        let dest = match op {
            0 if !matches!(funct, 0x08 | 0x0C | 0x0D | 0x11 | 0x13 | 0x18..=0x1B) => Some(rd),
            0x03 => Some(31),
            0x08..=0x0F | 0x20..=0x26 => Some(rt),
            _ => None,
        };
        if let Some(d) = dest.filter(|&d| d != 0) {
            konst[d as usize] = match op {
                0x0F => Some(imm << 16),
                0x0D => konst[rs as usize].map(|c| c | imm),
                0x09 if rs == 0 => Some(imm as i16 as i32 as u32),
                _ => None,
            };
            if copy {
                held |= 1 << d;
            } else {
                held &= !(1 << d);
            }
        }
        if held == 0 {
            break;
        }
        // jr / jalr ends the straight run after its delay slot.
        if op == 0 && matches!(funct, 0x08 | 0x09) {
            if let Some(slot) = code.word(pc.wrapping_add(4 * (n + 1))) {
                if slot >> 26 == 0x0C && has((slot >> 21) & 31) {
                    tested |= (slot & 0xFFFF) & mask;
                }
            }
            break;
        }
    }
    tested << (8 * k)
}

/// Group xrefs by the function containing each referencing PC.
#[allow(clippy::too_many_arguments)]
pub fn xref_report(
    code: &Code,
    lo: u32,
    hi: u32,
    data: bool,
    filter: &Filter,
    scopes: &[crate::ScopedSymbols],
    offset: usize,
    limit: usize,
    text: bool,
) -> (Value, String) {
    let all = xrefs(code, lo, hi, data);
    let refs: Vec<(u32, Ref, u32)> = all
        .iter()
        .filter(|(_, r)| filter.kinds.is_empty() || filter.kinds.iter().any(|k| k == r.kind))
        .filter_map(|(pc, r)| match filter.bits {
            None => Some((*pc, r.clone(), 0)),
            Some(m) if r.kind == "load" => {
                let t = tested_bits(code, *pc, r.target, lo, m);
                (t != 0).then(|| (*pc, r.clone(), t))
            }
            Some(_) => None,
        })
        .collect();
    let jal = code.jal_targets();
    let mut by_kind: BTreeMap<&str, u64> = BTreeMap::new();
    let mut funcs: BTreeMap<u32, u64> = BTreeMap::new();
    let mut lines = Vec::new();
    let mut out = String::new();
    for (index, (pc, r, tested)) in refs.iter().enumerate() {
        *by_kind.entry(r.kind).or_default() += 1;
        let f = if r.kind == "data" { None } else { function_bounds(code, *pc, &jal, 16384).map(|b| b.0) };
        if let Some(f) = f {
            *funcs.entry(f).or_default() += 1;
        }
        if index >= offset && lines.len() < limit {
            let word = code.word(*pc).unwrap_or(0);
            let fname = f.and_then(|f| code.name(scopes, f, 0));
            lines.push(json!({
                "pc": hex(*pc), "kind": r.kind, "via": r.via,
                "target": hex(r.target), "asm": if r.kind == "data" { format!(".word   0x{word:08X}") } else { disasm(word, *pc) },
                "function_start": f.map(hex), "function": fname,
            }));
            if filter.bits.is_some() {
                lines.last_mut().unwrap()["tests_hex"] = json!(format!("0x{tested:X}"));
            }
            if text {
                let mut fdesc = match (f, &fname) {
                    (Some(_), Some(n)) => format!("  in {n}"),
                    (Some(f), None) => format!("  in fn_{f:08X}"),
                    _ => String::new(),
                };
                if filter.bits.is_some() {
                    fdesc += &format!("  tests {tested:X}");
                }
                out += &format!(
                    "{pc:08X}  {:<6} {:<10} {:<36} -> {:08X}{fdesc}\n",
                    r.kind,
                    r.via,
                    lines.last().unwrap()["asm"].as_str().unwrap_or(""),
                    r.target
                );
            }
        }
    }
    let v = json!({
        "source": code.origin, "range": [hex(lo), hex(hi)], "region": psx_media::memmap::region(lo),
        "scanned": [format!("0x{:08X}", code.base), format!("0x{:08X}", code.end())],
        "gp": code.gp.map(|g| format!("0x{g:08X}")),
        "total": refs.len(), "by_kind": by_kind, "shown": lines.len(),
        "before_filter": (filter.kinds.len() + filter.bits.is_some() as usize > 0).then_some(all.len()),
        "page":crate::page::metadata(refs.len(),offset,lines.len(),limit),
        "coverage":{"complete":true,"scope":"static direct control-flow, resolved address pairs and optional aligned data pointers","indirect_references_complete":false},
        "functions": funcs.iter().map(|(f, n)| json!({"start": hex(*f), "refs": n, "name": code.name(scopes, *f, 0)})).collect::<Vec<_>>(),
        "refs": lines,
    });
    let header = format!(
        "; xrefs to 0x{lo:08X}..0x{hi:08X} in {} (gp {}): {} refs in {} functions\n",
        code.origin,
        code.gp.map(|g| format!("0x{g:08X}")).unwrap_or_else(|| "unknown".into()),
        refs.len(),
        funcs.len()
    );
    (v, header + &out)
}

/// `psx code disasm`: a listing from an address, an EXE entry point or a discovered function.
pub(crate) fn disasm_cmd(a: DisasmArgs) -> crate::Result<Value> {
    let DisasmArgs { source, addr, count, function_budget, symbols, text, function, ram, gp } = a;
    if !(1..=262144).contains(&function_budget) || count.is_some_and(|n| !(1..=262144).contains(&n)) {
        return Err(Error::invalid("count/function_budget must be 1..=262144"));
    }
    let mut code = Code::open(source.as_deref(), ram.as_deref())?;
    if let Some(g) = crate::commands::opt_hex(&gp, "gp")? {
        code.gp = Some(g);
    }
    let scopes = crate::load_symbols(&symbols)?;
    let entry = match &source {
        Some(s) => Some(PsxExe::parse(&psx_media::source::read(s)?)?.pc),
        None => None,
    };
    let (start, end, func) = if let Some(f) = crate::commands::opt_hex(&function, "function address")? {
        let jal = code.jal_targets();
        let extent = function_extent(&code, f, &jal, function_budget)
            .ok_or_else(|| Error::invalid(format!("0x{f:08X} is outside the code ({})", code.origin)))?;
        let (s, e) = (extent.start, extent.end);
        let listed_end = count.map_or(e, |n| s.saturating_add(4 * n).min(e));
        let callers = xrefs(&code, s, s + 4, false).iter().filter(|(_, r)| r.kind == "call").count();
        (
            s,
            listed_end,
            Some(json!({"start": format!("0x{s:08X}"), "end": format!("0x{e:08X}"), "instructions": (e-s)/4,
                "jal_callers":callers,"boundary_source":"heuristic","start_evidence":extent.start_evidence,
                "stop_reason":extent.stop_reason,"truncated":extent.truncated(),"search_budget":function_budget,
                "listing_truncated":listed_end<e})),
        )
    } else {
        let start = match crate::commands::opt_hex(&addr, "address")? {
            Some(a) => a,
            None => entry.ok_or_else(|| {
                Error::invalid("give --addr or --function (there is no EXE entry point with --ram alone)")
            })?,
        } & !3;
        (start, start.saturating_add(4 * count.unwrap_or(32)), None)
    };
    let (lines, listing) = listing(&code, start, end, &ListOpts { scopes: &scopes, text });
    let mut warnings = Vec::new();
    if func.as_ref().is_some_and(|f| f["truncated"] == true) {
        warnings.push(
            "Function boundary search is incomplete; increase --function-budget. Reported bounds are partial.".into(),
        );
    }
    if func.as_ref().is_some_and(|f| f["listing_truncated"] == true) {
        warnings.push("Listing limited by --count; omit it to list the discovered function.".into());
    }
    if let Some((checked, differ)) = code.exe_mismatch(start, end) {
        if differ > 0 {
            warnings.push(format!("RAM differs from the EXE in {differ} of {checked} words here: code patched or overlaid at run time (the listing shows RAM)"));
        } else if checked == 0 {
            warnings.push("this range is outside the EXE's load range: RAM only".to_string());
        }
    }
    if lines.is_empty() {
        warnings.push(format!(
            "0x{start:08X} is outside the code ({}: 0x{:08X}..0x{:08X})",
            code.origin,
            code.base,
            code.end()
        ));
    }
    let mut v = json!({"source": source.clone().unwrap_or_else(|| code.origin.clone()), "lines": lines,
        "listing":{"start":format!("0x{start:08X}"),"end":format!("0x{end:08X}"),
            "next_addr":func.as_ref().filter(|f| f["truncated"]==true || f["listing_truncated"]==true).map(|_|format!("0x{end:08X}"))}});
    if ram.is_some() {
        v["ram"] = json!(code.origin);
    }
    if let Some(f) = func {
        v["function"] = f;
    }
    if !warnings.is_empty() {
        v["warnings"] = json!(warnings);
    }
    if text {
        let mut t = String::new();
        if let Some(f) = v.get("function") {
            t += &format!(
                "; function {}..{} ({} instructions, {} jal callers) in {}\n",
                f["start"].as_str().unwrap_or(""),
                f["end"].as_str().unwrap_or(""),
                f["instructions"],
                f["jal_callers"],
                code.origin
            );
        }
        for w in &warnings {
            t += &format!("; warning: {w}\n");
        }
        v["text"] = json!(t + &listing);
        v.as_object_mut().unwrap().remove("lines");
    }
    Ok(v)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn code(words: &[u32]) -> Code {
        let bytes = words.iter().flat_map(|w| w.to_le_bytes()).collect();
        Code { base: 0x8001_0000, bytes, origin: "test".into(), gp: Some(0x8007_0000), exe: None }
    }

    // Two functions:
    // 80010000 f: addiu sp,sp,-24 / beq a0,zero,+3 (-> 80010014) / nop / jr ra / nop /
    //   80010014 lui at,0x8015 / lw v0,-0x1764(at) / sw v0,0x10(gp) / jal 80010030 / nop / jr ra / nop
    // 80010030 g: (leaf, no prologue) lui t0,0x8015 / ori t0,t0,0xE89C / jr ra / nop
    fn sample() -> Code {
        code(&[
            0x27BD_FFE8,
            0x1080_0003,
            0x0000_0000,
            JR_RA,
            0x0000_0000, //
            0x3C01_8015,
            0x8C22_E89C,
            0xAF82_0010,
            0x0C00_400C,
            0x0000_0000,
            JR_RA,
            0x0000_0000, //
            0x3C08_8015,
            0x3508_E89C,
            JR_RA,
            0x0000_0000, //
            0x8001_0030, // a pointer to g
        ])
    }

    #[test]
    fn function_bounds_follow_branches_past_early_returns() {
        let c = sample();
        let jal = c.jal_targets();
        assert_eq!(jal.iter().copied().collect::<Vec<_>>(), vec![0x8001_0030]);
        assert_eq!(function_bounds(&c, 0x8001_0000, &jal, 1000), Some((0x8001_0000, 0x8001_0030)));
        assert_eq!(
            function_bounds(&c, 0x8001_0018, &jal, 1000),
            Some((0x8001_0000, 0x8001_0030)),
            "enclosing function"
        );
        assert_eq!(function_bounds(&c, 0x8001_0034, &jal, 1000), Some((0x8001_0030, 0x8001_0040)));
    }

    #[test]
    fn xrefs_keep_the_loads_that_test_given_bits() {
        let c = code(&[
            0x3C01_8007, // lui at, 0x8007
            0x8C22_0010, // lw v0, 0x10(at)
            0x0000_0000,
            0x3043_0400, // andi v1, v0, 0x400
            0x3C01_8007,
            0x9424_0012, // lhu a0, 0x12(at): the word's upper half
            0x2408_0001, // addiu t0, zero, 1
            0x0088_2824, // and a1, a0, t0: bit 16 of the word
            0x3C01_8007,
            0x8C26_0010, // lw a2, 0x10(at)
            0x30C7_0002, // andi a3, a2, 2: not asked for
            JR_RA,
            0x0000_0000,
        ]);
        let pcs = |f: &Filter| {
            let (v, _) = xref_report(&c, 0x8007_0010, 0x8007_0014, true, f, &[], 0, 100, false);
            v["refs"]
                .as_array()
                .unwrap()
                .iter()
                .map(|r| (crate::commands::addr_value(&r["pc"]).unwrap() as u64, r["tests_hex"].clone()))
                .collect::<Vec<_>>()
        };
        assert_eq!(pcs(&Filter { kinds: vec!["load".into()], bits: None }).len(), 3);
        assert_eq!(
            pcs(&Filter { kinds: vec![], bits: Some(0x0001_0400) }),
            vec![(0x8001_0004, json!("0x400")), (0x8001_0014, json!("0x10000"))]
        );
        assert!(pcs(&Filter { kinds: vec!["store".into()], bits: None }).is_empty());
    }

    #[test]
    fn xrefs_find_a_table_built_in_the_kuseg_mirror() {
        let c = code(&[
            0x3C08_0008, // lui t0, 0x8
            0x2508_1234, // addiu t0, t0, 0x1234: the table at 0x00081234
            0x3C09_0001, // lui t1, 0x1
            0x2529_0010, // addiu t1, t1, 0x10: 0x00010010, outside the range asked
            JR_RA,
            0x0000_0000,
        ]);
        let r = xrefs(&c, 0x8008_1234, 0x8008_1238, false);
        assert_eq!(
            r.iter().map(|(pc, r)| (*pc, r.kind, r.via, r.target)).collect::<Vec<_>>(),
            [(0x8001_0004, "addr", "lui+addiu", 0x0008_1234)]
        );
    }

    #[test]
    fn xrefs_resolve_lui_pairs_gp_calls_and_pointers() {
        let c = sample();
        let r = xrefs(&c, 0x8015_0000, 0x8016_0000, true);
        let got: Vec<(u32, &str, &str, u32)> = r.iter().map(|(pc, r)| (*pc, r.kind, r.via, r.target)).collect();
        assert_eq!(got, vec![(0x8001_0034, "addr", "lui+ori", 0x8015_E89C)]);
        // lw v0, -0x1764(at) with at = 0x80150000 is 0x8014E89C: outside the range.
        assert!(got.iter().all(|g| g.0 != 0x8001_0018));
        let r = xrefs(&c, 0x8014_E89C, 0x8014_E8A0, true);
        assert_eq!(r.len(), 1);
        assert_eq!((r[0].0, r[0].1.kind, r[0].1.via), (0x8001_0018, "load", "lui+imm"));
        let r = xrefs(&c, 0x8007_0010, 0x8007_0014, true);
        assert_eq!((r[0].0, r[0].1.kind, r[0].1.via), (0x8001_001C, "store", "gp+imm"));
        let r = xrefs(&c, 0x8001_0030, 0x8001_0034, true);
        let kinds: Vec<&str> = r.iter().map(|x| x.1.kind).collect();
        assert_eq!(kinds, vec!["call", "data"]);
    }

    #[test]
    fn text_listing_labels_branch_targets() {
        let c = sample();
        let (lines, text) = listing(&c, 0x8001_0000, 0x8001_0030, &ListOpts { scopes: &[], text: true });
        assert_eq!(lines.len(), 12);
        assert!(text.contains("loc_80010014:\n"), "{text}");
        assert!(text.contains("beq     a0, zero, loc_80010014"), "{text}");
        assert!(text.contains("; store 0x80070010"), "{text}");
        assert_eq!(lines[6]["refs"][0]["target"], "0x8014E89C");
    }
}
