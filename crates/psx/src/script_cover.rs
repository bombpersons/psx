//! Coverage of scripted rules (docs/script-coverage-design.md): when a
//! game's rules live in a program its own interpreter runs, the glossary's
//! binding says where the programs are, and psx keeps which of their bytes
//! the game read while recording; it may also name points of the
//! interpreter where its own code says which script position ran and which
//! way a decision went. psx decodes no script: positions are what the
//! binding names them, offsets into its programs.
//!
//! A recording keeps this beside its machine code's coverage, under
//! `programs` in `<name>.coverage.json`: per program, its length and the
//! sha256 of its bytes when the binding declared where it is, the stretches
//! read, the sites run and the ways each decision went. No program byte is
//! kept, so it is committed with the recording.
//!
//! A binding that decodes the script can also give each program's map
//! (section 10): its statements, decisions and routines. Coverage then
//! counts statements run rather than bytes read, and decisions no hook
//! named; the recording keeps the map, offsets and names only.

use std::collections::{BTreeMap, BTreeSet};

use psx_core::Machine;
use psx_core::cpu::StepInfo;
use serde_json::{Value, json};

use crate::binding::Plugin;
use crate::scenario::Described;

/// Problems kept per recording; past it they are only counted.
const MAX_PROBLEMS: usize = 20;

/// The binding's hooks (`hooks` in its answer to DESCRIBE).
#[derive(Clone, Debug, Default, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HookPoints {
    #[serde(default)]
    pub at: Vec<u32>,
    #[serde(default)]
    pub branch: Vec<u32>,
}

/// A program as recorded: what was read of it, the sites run and the ways
/// its decisions went.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Program {
    /// Its length and the sha256 of its bytes, when the binding declared
    /// where it is (a program only hooks name has neither).
    pub bytes: Option<u32>,
    pub sha256: Option<String>,
    /// Each byte read, by offset.
    pub read: BTreeSet<u32>,
    pub sites: BTreeMap<u32, u64>,
    /// Each decision by position: how many ways it has, and the ways seen.
    pub ways: BTreeMap<u32, (u32, BTreeSet<u32>)>,
    /// Its structure, when the binding gives it.
    pub map: Option<ProgramMap>,
}

/// A program's structure as the binding decoded it (`Glossary::map`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ProgramMap {
    /// Each statement's start, to its end (excluded).
    pub statements: BTreeMap<u32, u32>,
    /// Each decision's position, to how many ways it has.
    pub decisions: BTreeMap<u32, u32>,
    /// Named stretches: name, start, end (excluded).
    pub routines: Vec<(String, u32, u32)>,
}

impl ProgramMap {
    /// The binding's answer to MAP for a program of `bytes` bytes; None for
    /// an empty map.
    fn parse(text: &str, bytes: u32) -> Result<Option<ProgramMap>, String> {
        #[derive(serde::Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Raw {
            statements: Vec<(u32, u32)>,
            decisions: Vec<(u32, u32)>,
            routines: Vec<(String, u32, u32)>,
        }
        let raw: Raw = serde_json::from_str(text).map_err(|e| format!("the binding's answer: {e}"))?;
        let mut map = ProgramMap::default();
        for (start, end) in raw.statements {
            if start >= end || end > bytes {
                return Err(format!("statement {start:#x}..{end:#x}: not inside the program's {bytes:#x} bytes"));
            }
            if map.statements.insert(start, end).is_some() {
                return Err(format!("statement {start:#x}: given twice"));
            }
        }
        let mut last = 0;
        for (&start, &end) in &map.statements {
            if start < last {
                return Err(format!("statement {start:#x}..{end:#x}: overlaps the one before"));
            }
            last = end;
        }
        for (at, ways) in raw.decisions {
            if !map.statements.contains_key(&at) {
                return Err(format!("decision {at:#x}: no statement starts there"));
            }
            if ways < 2 {
                return Err(format!("decision {at:#x}: {ways} ways; a decision has at least 2"));
            }
            map.decisions.insert(at, ways);
        }
        for (name, start, end) in raw.routines {
            if !valid_name(&name) {
                return Err(format!("routine {name:?}: a name is letters, digits, _ and -"));
            }
            if start >= end || end > bytes {
                return Err(format!("routine {name} {start:#x}..{end:#x}: not inside the program's {bytes:#x} bytes"));
            }
            map.routines.push((name, start, end));
        }
        map.routines.sort_by_key(|r| (r.1, std::cmp::Reverse(r.2)));
        let empty = map.statements.is_empty() && map.routines.is_empty();
        Ok((!empty).then_some(map))
    }

    /// The innermost routine holding `at`.
    pub fn routine(&self, at: u32) -> Option<&str> {
        self.routines.iter().filter(|r| (r.1..r.2).contains(&at)).min_by_key(|r| r.2 - r.1).map(|r| r.0.as_str())
    }

    fn kept(&self) -> Value {
        // Statements as runs of back-to-back lengths: [start, [length]].
        let mut runs: Vec<(u32, u32, Vec<u32>)> = Vec::new();
        for (&start, &end) in &self.statements {
            match runs.last_mut() {
                Some(r) if r.1 == start => {
                    r.1 = end;
                    r.2.push(end - start);
                }
                _ => runs.push((start, end, vec![end - start])),
            }
        }
        json!({
            "statements": runs.iter().map(|(s, _, l)| json!([hex(*s), l])).collect::<Vec<_>>(),
            "decisions": self.decisions.iter().map(|(&a, w)| json!([hex(a), w])).collect::<Vec<_>>(),
            "routines": self.routines.iter().map(|(n, s, e)| json!([n, hex(*s), hex(*e)])).collect::<Vec<_>>(),
        })
    }

    fn from_kept(v: &Value) -> ProgramMap {
        let mut map = ProgramMap::default();
        for r in v["statements"].as_array().into_iter().flatten() {
            let Some(mut at) = unhex(&r[0]) else { continue };
            for l in r[1].as_array().into_iter().flatten().filter_map(Value::as_u64) {
                map.statements.insert(at, at + l as u32);
                at += l as u32;
            }
        }
        for d in v["decisions"].as_array().into_iter().flatten() {
            if let (Some(a), Some(w)) = (unhex(&d[0]), d[1].as_u64()) {
                map.decisions.insert(a, w as u32);
            }
        }
        for r in v["routines"].as_array().into_iter().flatten() {
            if let (Some(n), Some(s), Some(e)) = (r[0].as_str(), unhex(&r[1]), unhex(&r[2])) {
                map.routines.push((n.to_string(), s, e));
            }
        }
        map
    }
}

/// A branch hook whose way is not known yet: its pc, where it goes when
/// taken, and whether its delay slot has run.
#[derive(Clone, Copy, Debug)]
struct Pending {
    pc: u32,
    target: u32,
    slot_ran: bool,
}

/// What a recording collects of the scripts the original ran.
pub struct Scripts {
    /// The hooks' pcs, folded, to the pc as the binding gave it (which its
    /// hook is told).
    at: BTreeMap<u32, u32>,
    branch: BTreeMap<u32, u32>,
    /// The programs declared at the last tick: name, folded start, length.
    regions: Vec<(String, u32, u32)>,
    pub programs: BTreeMap<String, Program>,
    pending: Option<Pending>,
    pub problems: Vec<String>,
    pub problem_count: usize,
}

impl Scripts {
    pub fn new(hooks: &HookPoints) -> Scripts {
        Scripts {
            at: hooks.at.iter().map(|&p| (crate::run::fold(p), p)).collect(),
            branch: hooks.branch.iter().map(|&p| (crate::run::fold(p), p)).collect(),
            regions: Vec::new(),
            programs: BTreeMap::new(),
            pending: None,
            problems: Vec::new(),
            problem_count: 0,
        }
    }

    fn problem(&mut self, p: String) {
        self.problem_count += 1;
        if self.problems.len() < MAX_PROBLEMS && !self.problems.contains(&p) {
            self.problems.push(p);
        }
    }

    /// Asks the binding where its programs are now (where the recording
    /// starts, and when each tick ends).
    pub fn declare(&mut self, b: &Plugin<Described>, m: &Machine) {
        let text = match b.call(psx_capture::abi::PROGRAMS, m) {
            Ok(t) => t,
            Err(e) => return self.problem(format!("programs: {e}")),
        };
        let list: Vec<(String, u32, u32)> = match serde_json::from_str(&text) {
            Ok(l) => l,
            Err(e) => return self.problem(format!("programs: the binding's answer: {e}")),
        };
        let mut regions = Vec::new();
        for (index, (name, at, bytes)) in list.into_iter().enumerate() {
            let start = crate::run::fold(at);
            if !valid_name(&name) {
                self.problem(format!("program {name:?}: a name is letters, digits, _ and -"));
                continue;
            }
            if bytes == 0 || start >= 0x20_0000 || start + bytes > 0x20_0000 {
                self.problem(format!("program {name}: {bytes} bytes at {at:#010x} are not in main RAM"));
                continue;
            }
            let moved = !self.regions.iter().any(|r| r.0 == name && r.1 == start && r.2 == bytes);
            if moved {
                let sha = crate::stamp::sha256(&m.bus.ram[start as usize..(start + bytes) as usize]);
                let p = self.programs.entry(name.clone()).or_default();
                match (&p.bytes, &p.sha256) {
                    (None, _) => {
                        (p.bytes, p.sha256) = (Some(bytes), Some(sha));
                        let answer = b.call_with(psx_capture::abi::MAP, m, [index as u32, 0, 0, 0]);
                        match answer.and_then(|t| ProgramMap::parse(&t, bytes)) {
                            Ok(map) => self.programs.get_mut(&name).expect("just added").map = map,
                            Err(e) => self.problem(format!("program {name}'s map: {e}")),
                        }
                    }
                    (Some(b), Some(s)) if *b == bytes && *s == sha => {}
                    _ => self.problem(format!(
                        "program {name}: other bytes than it held before in the recording, so its positions mean something else: name each program the game runs by its own name"
                    )),
                }
            }
            regions.push((name, start, bytes));
        }
        self.regions = regions;
    }

    /// What the instruction `info` read of a program, and the hooks it
    /// brings.
    pub fn step(&mut self, info: &StepInfo, b: &Plugin<Described>, m: &Machine) {
        if let Some(a) = info.mem.filter(|a| !a.store && info.exception.is_none()) {
            let addr = crate::run::fold(a.addr);
            for (name, start, bytes) in &self.regions {
                for x in (addr..addr + a.width as u32).filter(|x| (*start..start + bytes).contains(x)) {
                    if let Some(p) = self.programs.get_mut(name) {
                        p.read.insert(x - start);
                    }
                }
            }
        }
        if self.at.is_empty() && self.branch.is_empty() {
            return;
        }
        let pc = crate::run::fold(info.pc);
        // A branch hook goes once its delay slot has run: the next pc is where.
        if let Some(p) = self.pending.take() {
            if !p.slot_ran {
                if pc == p.pc + 4 && info.exception.is_none() {
                    self.pending = Some(Pending { slot_ran: true, ..p });
                }
            } else {
                let taken = pc == p.target;
                if taken || pc == p.pc + 8 {
                    self.hook(b, m, [self.branch[&p.pc], 1, taken as u32, 0]);
                }
            }
        }
        if info.exception.is_some() {
            return;
        }
        if self.branch.contains_key(&pc) {
            let target = pc.wrapping_add(4).wrapping_add(((info.instruction & 0xFFFF) as i16 as i32 as u32) << 2);
            self.pending = Some(Pending { pc, target, slot_ran: false });
        }
        if let Some(&given) = self.at.get(&pc) {
            self.hook(b, m, [given, 0, 0, 0]);
        }
    }

    fn hook(&mut self, b: &Plugin<Described>, m: &Machine, args: [u32; 4]) {
        let what = if args[1] == 0 { "at" } else { "branch" };
        let text = match b.call_with(psx_capture::abi::HOOK, m, args) {
            Ok(t) => t,
            Err(e) => return self.problem(format!("hook {what} {:#010x}: {e}", args[0])),
        };
        let marks: Vec<Vec<Value>> = match serde_json::from_str(&text) {
            Ok(l) => l,
            Err(e) => return self.problem(format!("hook {what} {:#010x}: the binding's answer: {e}", args[0])),
        };
        for mark in marks {
            let (Some(name), Some(pos)) = (mark.first().and_then(Value::as_str), mark.get(1).and_then(Value::as_u64))
            else {
                continue;
            };
            if !valid_name(name) {
                self.problem(format!(
                    "hook {what} {:#010x}: program {name:?}: a name is letters, digits, _ and -",
                    args[0]
                ));
                continue;
            }
            let pos = pos as u32;
            let p = self.programs.entry(name.to_string()).or_default();
            let decision = mark.get(3).and_then(Value::as_u64);
            let unmapped = p.map.as_ref().and_then(|map| match decision {
                Some(of) if map.decisions.get(&pos) != Some(&(of as u32)) => Some(format!(
                    "{name}+{pos:#x}: a hook names a decision of {of} ways there, which its map does not have"
                )),
                None if !map.statements.contains_key(&pos) => {
                    Some(format!("{name}+{pos:#x}: a hook names a site there, where its map has no statement start"))
                }
                _ => None,
            });
            if let Some(problem) = unmapped {
                self.problem(problem);
                continue;
            }
            let p = self.programs.get_mut(name).expect("just added");
            match (mark.get(2).and_then(Value::as_u64), decision) {
                (Some(way), Some(of)) => {
                    let e = p.ways.entry(pos).or_insert_with(|| (of as u32, BTreeSet::new()));
                    if e.0 != of as u32 {
                        let was = e.0;
                        self.problem(format!(
                            "{name}+{pos:#x}: a decision of {of} ways, and of {was} before: a position's ways are always as many"
                        ));
                        continue;
                    }
                    e.1.insert(way as u32);
                }
                _ => *p.sites.entry(pos).or_default() += 1,
            }
        }
    }

    /// What the recording keeps: `programs`, or None when the binding
    /// names none.
    pub fn kept(&self) -> Option<Value> {
        (!self.programs.is_empty()).then(|| Value::Array(self.programs.iter().map(|(n, p)| kept(n, p)).collect()))
    }
}

/// A program's name: letters, digits, `_` and `-`.
pub fn valid_name(name: &str) -> bool {
    !name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

fn hex(n: u32) -> String {
    format!("{n:#x}")
}

fn unhex(v: &Value) -> Option<u32> {
    u32::from_str_radix(v.as_str()?.strip_prefix("0x")?, 16).ok()
}

/// Consecutive offsets as `[start, end)` stretches.
fn stretches(set: &BTreeSet<u32>) -> Vec<(u32, u32)> {
    let mut out: Vec<(u32, u32)> = Vec::new();
    for &x in set {
        match out.last_mut() {
            Some(r) if r.1 == x => r.1 = x + 1,
            _ => out.push((x, x + 1)),
        }
    }
    out
}

fn kept(name: &str, p: &Program) -> Value {
    let mut v = json!({"name": name});
    if let (Some(b), Some(s)) = (p.bytes, &p.sha256) {
        v["bytes"] = json!(b);
        v["sha256"] = json!(s);
    }
    v["read"] = json!(stretches(&p.read).iter().map(|&(s, e)| json!([hex(s), hex(e)])).collect::<Vec<_>>());
    v["sites"] = json!(p.sites.iter().map(|(&a, n)| json!([hex(a), n])).collect::<Vec<_>>());
    v["ways"] = json!(p.ways.iter().map(|(&a, (of, w))| json!([hex(a), of, w])).collect::<Vec<_>>());
    if let Some(map) = &p.map {
        v["map"] = map.kept();
    }
    v
}

/// One recording's kept `programs`, read back.
fn parse(doc: &Value) -> Vec<(String, Program)> {
    let mut out = Vec::new();
    for g in doc["programs"].as_array().into_iter().flatten() {
        let Some(name) = g["name"].as_str() else { continue };
        let mut p = Program {
            bytes: g["bytes"].as_u64().map(|b| b as u32),
            sha256: g["sha256"].as_str().map(String::from),
            map: g.get("map").map(ProgramMap::from_kept),
            ..Program::default()
        };
        for r in g["read"].as_array().into_iter().flatten() {
            if let (Some(s), Some(e)) = (unhex(&r[0]), unhex(&r[1])) {
                p.read.extend(s..e);
            }
        }
        for s in g["sites"].as_array().into_iter().flatten() {
            if let (Some(a), Some(n)) = (unhex(&s[0]), s[1].as_u64()) {
                p.sites.insert(a, n);
            }
        }
        for w in g["ways"].as_array().into_iter().flatten() {
            if let (Some(a), Some(of)) = (unhex(&w[0]), w[1].as_u64()) {
                let seen = w[2].as_array().into_iter().flatten().filter_map(|x| x.as_u64().map(|x| x as u32));
                p.ways.insert(a, (of as u32, seen.collect()));
            }
        }
        out.push((name.to_string(), p));
    }
    out
}

/// Programs merged across recordings or runs, and what does not merge.
#[derive(Debug, Default)]
pub struct Merged {
    pub programs: BTreeMap<String, Program>,
    pub problems: Vec<String>,
}

impl Merged {
    /// Adds the `programs` of the kept coverage `doc` (of `what`, for
    /// problems).
    pub fn add(&mut self, doc: &Value, what: &str) {
        for (name, p) in parse(doc) {
            let m = self.programs.entry(name.clone()).or_default();
            match (&m.sha256, &p.sha256) {
                (Some(a), Some(b)) if a != b => {
                    self.problems.push(format!(
                        "{what}: its program {name} holds other bytes than the other recordings', so its positions are not theirs: record them again from one release of the script, or name each program by its own name"
                    ));
                    continue;
                }
                (None, Some(_)) => (m.bytes, m.sha256) = (p.bytes, p.sha256.clone()),
                _ => {}
            }
            match (&m.map, &p.map) {
                (Some(a), Some(b)) if a != b => self.problems.push(format!(
                    "{what}: its map of {name} is not the other recordings': record them again with the binding as it is"
                )),
                (None, Some(b)) => m.map = Some(b.clone()),
                _ => {}
            }
            m.read.extend(&p.read);
            for (a, n) in &p.sites {
                *m.sites.entry(*a).or_default() += n;
            }
            for (a, (of, seen)) in &p.ways {
                let e = m.ways.entry(*a).or_insert_with(|| (*of, BTreeSet::new()));
                if e.0 != *of {
                    self.problems
                        .push(format!("{what}: {name}+{a:#x} is a decision of {of} ways there, of {} elsewhere", e.0));
                    continue;
                }
                e.1.extend(seen);
            }
        }
    }

    /// A short summary per program, for `code coverage`.
    pub fn summary(&self) -> Value {
        json!(
            self.programs
                .iter()
                .map(|(n, p)| {
                    let incomplete = p.ways.values().filter(|(of, seen)| seen.len() < *of as usize).count();
                    let mut v = json!({"name": n, "bytes": p.bytes, "bytes_read": p.read.len(), "sites": p.sites.len(),
                       "decisions": p.ways.len(), "decisions_with_ways_never_taken": incomplete,
                       "read": stretches(&p.read).iter().map(|&(s, e)| format!("{n}+{s:#x}..{e:#x}")).collect::<Vec<_>>(),
                       "never_read": p.bytes.map(|b| unread(p, 0, b).iter().map(|&(s, e)| format!("{n}+{s:#x}..{e:#x}")).collect::<Vec<_>>())});
                    if let (Some(map), Some(b)) = (&p.map, p.bytes) {
                        let groups = statement_groups(map, 0, b, |s, e| ran(p, s, e));
                        let at = |ran: bool| -> Vec<String> {
                            groups.iter().filter(|g| g.2 == ran).map(|g| located(n, g.0, g.1, &g.3)).collect()
                        };
                        let entered = |r: &(String, u32, u32)| map.statements.range(r.1..r.2).any(|(&s, &e)| ran(p, s, e));
                        v["map"] = json!({
                            "statements": map.statements.len(),
                            "statements_ran": map.statements.iter().filter(|(s, e)| ran(p, **s, **e)).count(),
                            "decisions": map.decisions.len(),
                            "decisions_unnamed_by_hooks": map.decisions.keys().filter(|a| !p.ways.contains_key(a)
                                && map.statements.get(a).is_some_and(|&e| ran(p, **a, e))).count(),
                            "routines_never_entered": map.routines.iter().filter(|r| !entered(r)).map(|r| r.0.clone()).collect::<Vec<_>>(),
                            "ran": at(true),
                            "never_ran": at(false),
                        });
                        // Bytes read are the map's statements, less noisy.
                        for k in ["read", "never_read"] {
                            v.as_object_mut().expect("an object").remove(k);
                        }
                    }
                    v
                })
                .collect::<Vec<_>>()
        )
    }

    /// What these programs ran that `against`'s did not: stretches read,
    /// sites and decision ways, by program.
    pub fn diff(&self, against: &Merged) -> Value {
        let mut out = Vec::new();
        for (n, p) in &self.programs {
            let other = against.programs.get(n);
            let read: BTreeSet<u32> =
                p.read.iter().filter(|x| !other.is_some_and(|o| o.read.contains(x))).copied().collect();
            let sites: Vec<String> = p
                .sites
                .keys()
                .filter(|a| !other.is_some_and(|o| o.sites.contains_key(a)))
                .map(|a| format!("{n}+{a:#x}"))
                .collect();
            let mut ways = Vec::new();
            for (a, (of, seen)) in &p.ways {
                let theirs = other.and_then(|o| o.ways.get(a)).map(|w| &w.1);
                for w in seen.iter().filter(|w| !theirs.is_some_and(|t| t.contains(w))) {
                    ways.push(json!({"at": format!("{n}+{a:#x}"), "way": w, "of": of}));
                }
            }
            if let (Some(map), Some(b)) = (&p.map, p.bytes) {
                let theirs = |s: u32, e: u32| other.is_some_and(|o| ran(o, s, e));
                let only: Vec<String> = statement_groups(map, 0, b, |s, e| ran(p, s, e) && !theirs(s, e))
                    .iter()
                    .filter(|g| g.2)
                    .map(|g| located(n, g.0, g.1, &g.3))
                    .collect();
                let entered = |r: &(String, u32, u32), f: &dyn Fn(u32, u32) -> bool| {
                    map.statements.range(r.1..r.2).any(|(&s, &e)| f(s, e))
                };
                let routines: Vec<&str> = map
                    .routines
                    .iter()
                    .filter(|r| entered(r, &|s, e| ran(p, s, e)) && !entered(r, &theirs))
                    .map(|r| r.0.as_str())
                    .collect();
                if only.is_empty() && ways.is_empty() {
                    continue;
                }
                out.push(json!({"name": n, "statements_only": only, "routines_only": routines, "ways_only": ways}));
                continue;
            }
            if read.is_empty() && sites.is_empty() && ways.is_empty() {
                continue;
            }
            out.push(json!({"name": n,
                "read_only": stretches(&read).iter().map(|&(s, e)| format!("{n}+{s:#x}..{e:#x}")).collect::<Vec<_>>(),
                "sites_only": sites, "ways_only": ways}));
        }
        json!(out)
    }
}

/// The stretches of `[start, end)` no recording read.
fn unread(p: &Program, start: u32, end: u32) -> Vec<(u32, u32)> {
    let gaps: BTreeSet<u32> = (start..end).filter(|x| !p.read.contains(x)).collect();
    stretches(&gaps)
}

/// Whether the statement `[start, end)` ran: a byte of it read, or a hook
/// named its start.
fn ran(p: &Program, start: u32, end: u32) -> bool {
    p.sites.contains_key(&start) || p.read.range(start..end).next().is_some()
}

/// The map's statements starting in `[start, end)`, grouped into stretches
/// of back-to-back statements that `keep` says the same of and that lie in
/// the same routine: `(start, end, said, routine)`.
fn statement_groups<T: PartialEq + Copy>(
    map: &ProgramMap,
    start: u32,
    end: u32,
    keep: impl Fn(u32, u32) -> T,
) -> Vec<(u32, u32, T, Option<String>)> {
    let mut out: Vec<(u32, u32, T, Option<String>)> = Vec::new();
    for (&s, &e) in map.statements.range(start..end) {
        let (said, routine) = (keep(s, e), map.routine(s).map(String::from));
        match out.last_mut() {
            Some(g) if g.1 == s && g.2 == said && g.3 == routine => g.1 = e,
            _ => out.push((s, e, said, routine)),
        }
    }
    out
}

/// `name+0xS..0xE`, with the routine it is in.
fn located(name: &str, start: u32, end: u32, routine: &Option<String>) -> String {
    let at = Stretch { program: name.to_string(), start, end };
    match routine {
        Some(r) => format!("{at} ({r})"),
        None => at.to_string(),
    }
}

/// A stretch of a program: `name+0xSTART..0xEND` (end excluded), or one
/// position `name+0xAT`.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Stretch {
    pub program: String,
    pub start: u32,
    pub end: u32,
}

impl Stretch {
    pub fn parse(s: &str) -> Option<Stretch> {
        let (name, rest) = s.split_once('+')?;
        let num = |h: &str| {
            let h = h.strip_prefix("0x")?;
            (!h.is_empty() && h.len() <= 8).then(|| u32::from_str_radix(h, 16).ok()).flatten()
        };
        let (start, end) = match rest.split_once("..") {
            Some((a, b)) => (num(a)?, num(b)?),
            None => {
                let a = num(rest)?;
                (a, a.checked_add(1)?)
            }
        };
        (valid_name(name) && start < end).then(|| Stretch { program: name.to_string(), start, end })
    }

    pub fn contains(&self, other: &Stretch) -> bool {
        self.program == other.program && self.start <= other.start && other.end <= self.end
    }
}

impl std::fmt::Display for Stretch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.end == self.start + 1 {
            write!(f, "{}+{:#x}", self.program, self.start)
        } else {
            write!(f, "{}+{:#x}..{:#x}", self.program, self.start, self.end)
        }
    }
}

/// One thing a feature's script stretch has to have covered: a stretch
/// read or never read, or a decision; `never` is what no recording did
/// there (empty when covered).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Item {
    pub at: Stretch,
    pub never: String,
    /// The routine of the program's map it is in.
    pub routine: Option<String>,
}

/// What the feature's script stretches `code` have covered, by the merged
/// recordings `m`: each stretch read and not read (when the binding
/// declared the program) and each decision in it; and what keeps them from
/// being counted.
pub fn items(m: &Merged, code: &[Stretch]) -> (Vec<Item>, Vec<String>) {
    let (mut items, mut problems) = (Vec::new(), Vec::new());
    for s in code {
        let Some(p) = m.programs.get(&s.program) else {
            problems.push(format!(
                "code {s}: no recording of the feature has the program {}: the glossary binding's programs (or hooks) name it, and recordings made before it did are recorded again",
                s.program
            ));
            continue;
        };
        if let (Some(map), Some(b)) = (&p.map, p.bytes) {
            if s.end > b {
                problems.push(format!("code {s}: past the end of {} ({b:#x} bytes)", s.program));
                continue;
            }
            let stretch = |start, end| Stretch { program: s.program.clone(), start, end };
            for (gs, ge, ran, routine) in statement_groups(map, s.start, s.end, |a, b| ran(p, a, b)) {
                items.push(Item { at: stretch(gs, ge), never: if ran { "" } else { "ran" }.into(), routine });
            }
            let mut decisions: BTreeMap<u32, u32> = p.ways.range(s.start..s.end).map(|(&a, w)| (a, w.0)).collect();
            decisions.extend(map.decisions.range(s.start..s.end));
            for (a, of) in decisions {
                // One never run is in a stretch of statements never run.
                if map.statements.get(&a).is_some_and(|&e| !ran(p, a, e)) {
                    continue;
                }
                let never = match p.ways.get(&a) {
                    None => "had its way named by a hook".into(),
                    Some((_, seen)) => never_ways(of, seen),
                };
                items.push(Item { at: stretch(a, a + 1), never, routine: map.routine(a).map(String::from) });
            }
            continue;
        }
        if let Some(b) = p.bytes {
            if s.end > b {
                problems.push(format!("code {s}: past the end of {} ({b:#x} bytes)", s.program));
                continue;
            }
            let gaps = unread(p, s.start, s.end);
            let mut x = s.start;
            for (gs, ge) in gaps {
                if x < gs {
                    items.push(Item {
                        at: Stretch { program: s.program.clone(), start: x, end: gs },
                        never: String::new(),
                        routine: None,
                    });
                }
                items.push(Item {
                    at: Stretch { program: s.program.clone(), start: gs, end: ge },
                    never: "read".into(),
                    routine: None,
                });
                x = ge;
            }
            if x < s.end {
                items.push(Item {
                    at: Stretch { program: s.program.clone(), start: x, end: s.end },
                    never: String::new(),
                    routine: None,
                });
            }
        }
        for (&a, (of, seen)) in p.ways.range(s.start..s.end) {
            items.push(Item {
                at: Stretch { program: s.program.clone(), start: a, end: a + 1 },
                never: never_ways(*of, seen),
                routine: None,
            });
        }
    }
    problems.extend(m.problems.iter().cloned());
    (items, problems)
}

/// What no recording did of a decision of `of` ways that went `seen`.
fn never_ways(of: u32, seen: &BTreeSet<u32>) -> String {
    let missing: Vec<String> = (0..of).filter(|w| !seen.contains(w)).map(|w| w.to_string()).collect();
    match missing.len() {
        0 => String::new(),
        1 => format!("went way {} of {of}", missing[0]),
        _ => format!("went ways {} of {of}", missing.join(", ")),
    }
}

/// The programs kept beside the recordings `paths` (`.jsonl`), merged.
pub fn merged(paths: &[std::path::PathBuf]) -> Merged {
    let mut m = Merged::default();
    for p in paths {
        let kept = p.with_extension("coverage.json");
        let Ok(text) = std::fs::read_to_string(&kept) else { continue };
        let Ok(doc) = serde_json::from_str::<Value>(&text) else { continue };
        m.add(&doc, &p.display().to_string());
    }
    m
}

#[cfg(test)]
mod tests {
    use super::*;

    fn st(s: &str) -> Stretch {
        Stretch::parse(s).unwrap()
    }

    #[test]
    fn stretches_parse_and_contain() {
        assert_eq!(st("croc+0x1803..0x18A0"), Stretch { program: "croc".into(), start: 0x1803, end: 0x18A0 });
        assert_eq!(st("croc+0x1803").end, 0x1804);
        assert_eq!(st("croc+0x1803").to_string(), "croc+0x1803");
        assert_eq!(st("croc+0x1803..0x18a0").to_string(), "croc+0x1803..0x18a0");
        for bad in ["croc", "0x8002D810", "croc+1803", "croc+0x10..0x10", "cr oc+0x1", "+0x1"] {
            assert_eq!(Stretch::parse(bad), None, "{bad}");
        }
        assert!(st("croc+0x1800..0x1900").contains(&st("croc+0x1803")));
        assert!(!st("croc+0x1800..0x1900").contains(&st("croc+0x18F0..0x1901")));
        assert!(!st("croc+0x1800..0x1900").contains(&st("gex+0x1803")));
    }

    #[test]
    fn kept_programs_merge_and_give_a_features_items() {
        let mut a = Program { bytes: Some(0x20), sha256: Some("s".into()), ..Program::default() };
        a.read.extend([0x0, 0x1, 0x2, 0x8, 0x9]);
        a.sites.insert(0x0, 3);
        a.ways.insert(0x2, (2, [0].into()));
        let mut b = a.clone();
        b.read = [0xA, 0xB].into();
        b.ways.insert(0x2, (2, [1].into()));
        b.ways.insert(0x9, (3, [2].into()));
        let doc = |p: &Program| json!({"programs": [kept("croc", p)]});
        let mut m = Merged::default();
        m.add(&doc(&a), "a");
        let (items, problems) = items(&m, &[st("croc+0x0..0x10")]);
        assert!(problems.is_empty(), "{problems:?}");
        let never: Vec<(String, &str)> = items.iter().map(|i| (i.at.to_string(), i.never.as_str())).collect();
        assert_eq!(
            never,
            [
                ("croc+0x0..0x3".into(), ""),
                ("croc+0x3..0x8".into(), "read"),
                ("croc+0x8..0xa".into(), ""),
                ("croc+0xa..0x10".into(), "read"),
                ("croc+0x2".into(), "went way 1 of 2"),
            ]
        );
        m.add(&doc(&b), "b");
        let (items, _) = super::items(&m, &[st("croc+0x0..0x10")]);
        let never: Vec<(String, &str)> = items.iter().map(|i| (i.at.to_string(), i.never.as_str())).collect();
        assert_eq!(never[3], ("croc+0xc..0x10".into(), "read"));
        assert_eq!(never[4], ("croc+0x2".into(), ""));
        assert_eq!(never[5], ("croc+0x9".into(), "went ways 0, 1 of 3"));
        // What one set ran that the other did not.
        let (mut ma, mut mb) = (Merged::default(), Merged::default());
        ma.add(&doc(&b), "b");
        mb.add(&doc(&a), "a");
        let d = ma.diff(&mb);
        assert_eq!(d[0]["read_only"], json!(["croc+0xa..0xc"]));
        assert_eq!(
            d[0]["ways_only"],
            json!([{"at": "croc+0x2", "way": 1, "of": 2}, {"at": "croc+0x9", "way": 2, "of": 3}])
        );
        // Other bytes under the same name do not merge.
        let other = Program { sha256: Some("t".into()), ..a.clone() };
        m.add(&doc(&other), "c");
        assert!(m.problems[0].starts_with("c: its program croc holds other bytes"), "{:?}", m.problems);
        let (_, problems) = super::items(&m, &[st("gex+0x0..0x4"), st("croc+0x10..0x30")]);
        assert!(problems[0].starts_with("code gex+0x0..0x4: no recording of the feature has the program gex"));
        assert_eq!(problems[1], "code croc+0x10..0x30: past the end of croc (0x20 bytes)");
    }

    #[test]
    fn a_map_counts_statements_and_decisions_no_hook_named() {
        // Statements 0..3, 3..5, 5..6, 8..10 (6..8 is data); decisions at 0
        // and 5; the routine `go` is 5..10.
        let answer = r#"{"statements":[[0,3],[3,5],[5,6],[8,10]],"decisions":[[0,2],[5,3]],"routines":[["go",5,10]]}"#;
        let map = ProgramMap::parse(answer, 0x10).unwrap().unwrap();
        for (bad, why) in [
            (r#"{"statements":[[0,3],[2,4]],"decisions":[],"routines":[]}"#, "statement 0x2..0x4: overlaps"),
            (r#"{"statements":[[0,17]],"decisions":[],"routines":[]}"#, "statement 0x0..0x11: not inside"),
            (r#"{"statements":[[0,3]],"decisions":[[1,2]],"routines":[]}"#, "decision 0x1: no statement starts there"),
            (r#"{"statements":[[0,3]],"decisions":[[0,1]],"routines":[]}"#, "decision 0x0: 1 ways"),
        ] {
            assert!(ProgramMap::parse(bad, 0x10).unwrap_err().starts_with(why), "{bad}");
        }
        assert_eq!(ProgramMap::parse(r#"{"statements":[],"decisions":[],"routines":[]}"#, 4), Ok(None));
        let mut p =
            Program { bytes: Some(0x10), sha256: Some("s".into()), map: Some(map.clone()), ..Program::default() };
        // The first statement's opcode read and its decision named; the
        // last only by an operand byte; 6..8 read as data.
        p.read.extend([0, 6, 7, 9]);
        p.ways.insert(0, (2, [1].into()));
        let doc = json!({"programs": [kept("croc", &p)]});
        let mut m = Merged::default();
        m.add(&doc, "a");
        assert_eq!(m.programs["croc"].map.as_ref(), Some(&map), "the map is kept and read back");
        let (items, problems) = items(&m, &[st("croc+0x0..0x10")]);
        assert!(problems.is_empty(), "{problems:?}");
        let got: Vec<(String, &str, Option<&str>)> =
            items.iter().map(|i| (i.at.to_string(), i.never.as_str(), i.routine.as_deref())).collect();
        assert_eq!(
            got,
            [
                ("croc+0x0..0x3".into(), "", None),
                ("croc+0x3..0x5".into(), "ran", None),
                ("croc+0x5".into(), "ran", Some("go")),
                ("croc+0x8..0xa".into(), "", Some("go")),
                ("croc+0x0".into(), "went way 0 of 2", None),
            ]
        );
        // A site at 5 runs it: its decision then ran with no way named.
        let mut q = p.clone();
        q.sites.insert(5, 1);
        let mut m2 = Merged::default();
        m2.add(&json!({"programs": [kept("croc", &q)]}), "b");
        let (items, _) = super::items(&m2, &[st("croc+0x5..0x6")]);
        assert_eq!(items[1].never, "had its way named by a hook");
        // The diff gives statements and routines only one side ran.
        let d = m2.diff(&m);
        assert_eq!(d[0]["statements_only"], json!(["croc+0x5 (go)"]));
        assert_eq!(d[0]["routines_only"], json!([]), "a ran 8..10, in go too");
        let mut empty = Merged::default();
        empty.add(&json!({"programs": [kept("croc", &Program { read: BTreeSet::new(), ..p.clone() })]}), "c");
        assert_eq!(m2.diff(&empty)[0]["routines_only"], json!(["go"]));
        // Another map of the same bytes does not merge.
        let other = Program { map: Some(ProgramMap { routines: vec![], ..map }), ..p };
        m.add(&json!({"programs": [kept("croc", &other)]}), "d");
        assert!(m.problems[0].starts_with("d: its map of croc is not the other recordings'"), "{:?}", m.problems);
    }
}
