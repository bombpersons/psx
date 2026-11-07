//! The pointer (docs/understanding-plan.md, step 3): for a failing
//! recording, where in the original to look. Worked out on the recording's
//! replay: the bytes the binding reads for each differing quantity, the
//! code that stored into them near the first difference, the code the
//! original ran there for the first time in the recording, the features
//! that code belongs to and its checked C. It names code and features, not
//! the moment: the agent finds that with the debugger.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::time::{Duration, Instant};

use psx_core::Machine;
use psx_core::cpu::StepInfo;
use serde_json::{Value, json};

use super::{Entry, Port};
use crate::binding::Plugin;
use crate::commands::Ctx;
use crate::scenario::Described;
use crate::session::{Flow, Observer, Out};

/// Ticks before the first difference whose stores and new code count.
const WINDOW: u64 = 4;
/// Bytes the binding read that are tried per quantity's attribution.
const BYTES: usize = 16 * 1024;
/// Functions shown per list.
const SHOWN: usize = 8;

const RAM: u32 = 0x20_0000;
const SCRATCH: u32 = 0x1F80_0000;

/// A byte's index in the machine's memory: main RAM, then the scratchpad.
fn slot(folded: u32) -> Option<usize> {
    if folded < RAM {
        Some(folded as usize)
    } else if (SCRATCH..SCRATCH + 0x400).contains(&folded) {
        Some((RAM + folded - SCRATCH) as usize)
    } else {
        None
    }
}

/// An address as the game uses it: KSEG0 for main RAM.
fn shown(folded: u32) -> u32 {
    if folded < RAM { folded | 0x8000_0000 } else { folded }
}

/// What the replay saw, up to the end of the tick that first differs.
#[derive(Default)]
pub(crate) struct Seen {
    /// Whether the replay reached the end of that tick.
    pub reached: bool,
    pub refused: Option<String>,
    /// Each quantity's bytes (folded), at the end of that tick.
    pub reads: BTreeMap<String, Vec<u32>>,
    /// Stores in the window, by (pc, byte): how many.
    pub stores: HashMap<(u32, u32), u32>,
    /// Each byte's last writer, from the replay's start.
    pub last: Vec<u32>,
    /// Words first run in the window, and branch ways first taken there
    /// (pc, taken).
    pub first_words: Vec<u32>,
    pub first_ways: Vec<(u32, bool)>,
}

/// The observer: every instruction up to the end of tick `at`.
struct Look<'a> {
    binding: &'a Plugin<Described>,
    at: u64,
    /// Ticks ended so far: the one running is this one.
    ended: u64,
    ran: Vec<u64>,
    ways: Vec<u8>,
    /// A conditional branch and the steps since it.
    branch: Option<(u32, u8)>,
    seen: Seen,
}

impl Look<'_> {
    fn in_window(&self) -> bool {
        self.ended + WINDOW > self.at
    }
}

impl Observer for Look<'_> {
    fn per_step(&self) -> bool {
        true
    }

    fn step(&mut self, info: &StepInfo, _m: &mut Machine) -> Flow {
        let window = self.in_window();
        let pc = crate::run::fold(info.pc);
        if info.exception.is_some() {
            self.branch = None;
            return Flow::Go;
        }
        if let Some((b, n)) = self.branch.take() {
            if n == 0 {
                self.branch = Some((b, 1));
            } else if b < RAM {
                let taken = pc != b + 8;
                let bit = if taken { 2 } else { 1 };
                let w = &mut self.ways[(b / 4) as usize];
                if *w & bit == 0 {
                    *w |= bit;
                    if window && self.at > 0 {
                        self.seen.first_ways.push((b, taken));
                    }
                }
            }
        }
        if pc < RAM {
            let (i, bit) = ((pc / 4) as usize / 64, 1u64 << ((pc / 4) % 64));
            if self.ran[i] & bit == 0 {
                self.ran[i] |= bit;
                if window && self.at > 0 && self.seen.first_words.len() < 1 << 16 {
                    self.seen.first_words.push(pc);
                }
            }
        }
        if crate::coverage::conditional(info.instruction) {
            self.branch = Some((pc, 0));
        }
        if let Some(acc) = info.mem.filter(|a| a.store) {
            let a = crate::run::fold(acc.addr);
            for b in a..a + acc.width as u32 {
                let Some(s) = slot(b) else { continue };
                self.seen.last[s] = pc | 1;
                if window && self.seen.stores.len() < 1 << 20 {
                    *self.seen.stores.entry((pc, b)).or_default() += 1;
                }
            }
        }
        Flow::Go
    }

    fn tick(&mut self, _rel: u64, m: &mut Machine) -> Flow {
        if self.ended < self.at {
            self.ended += 1;
            return Flow::Go;
        }
        match self.binding.reads_by_quantity(m, BYTES) {
            Ok(r) => self.seen.reads = r,
            Err(e) => self.seen.refused = Some(e),
        }
        self.seen.reached = true;
        Flow::Stop("the pointer looked")
    }

    fn finish(self: Box<Self>, _m: &Machine, _out: &mut Out) -> crate::Result<()> {
        Ok(())
    }
}

/// Replays recording `id/name` on the original up to the end of tick `at`
/// (0: the initial values), looking.
pub(crate) fn look(ctx: &Ctx, id: &str, name: &str, at: u64, timeout: Duration) -> crate::Result<Seen> {
    let (_, binding) = crate::scenario::glossary_binding(&ctx.root)?;
    let mut l = Look {
        binding: &binding,
        at,
        ended: 0,
        ran: vec![0; (RAM / 4 / 64) as usize],
        ways: vec![0; (RAM / 4) as usize],
        branch: None,
        seen: Seen { last: vec![0; (RAM + 0x400) as usize], ..Seen::default() },
    };
    crate::scenario::replay(ctx, id, name, timeout, &mut |_, _| {}, None, Some(&mut l))?;
    Ok(l.seen)
}

/// The tick a difference's `at` names: `initial` is 0.
pub(crate) fn tick_of(at: &str) -> Option<u64> {
    if at == "initial" { Some(0) } else { at.strip_prefix("tick ")?.parse().ok() }
}

/// The original's functions, from the inventory, with their names and
/// the features whose code lists them.
pub(crate) struct Code {
    functions: Vec<(u32, Vec<(u32, u32)>)>,
    names: BTreeMap<u32, String>,
    features: BTreeMap<u32, Vec<String>>,
    /// Functions with C: whether its last check passed on this C and names.
    checked: BTreeMap<String, bool>,
}

impl Code {
    pub(crate) fn load(ctx: &Ctx, port: &Port) -> Code {
        let mut functions = Vec::new();
        if let Ok((_, inv)) = crate::coverage::inventory(ctx, None) {
            let lists = inv["executables"].as_array().into_iter().chain(inv["overlays"].as_array()).flatten();
            for f in lists.flat_map(|x| x["functions"].as_array().into_iter().flatten()) {
                let hex = |v: &Value| crate::commands::addr_value(v).map(crate::run::fold);
                let (Some(entry), Some(end)) = (hex(&f["entry"]), hex(&f["end"])) else { continue };
                let chunks = f["chunks"]
                    .as_array()
                    .map(|cs| cs.iter().filter_map(|c| hex(&c[0]).zip(hex(&c[1]))).collect())
                    .unwrap_or_else(|| vec![(entry, end)]);
                functions.push((entry, chunks));
            }
        }
        let names = psx_symbols::names::load(&ctx.root);
        let mut features: BTreeMap<u32, Vec<String>> = BTreeMap::new();
        for e in port.entries.values() {
            for a in e.code.iter().filter_map(|c| super::entry_addr(c)) {
                features.entry(crate::run::fold(a)).or_default().push(e.id.clone());
            }
        }
        let checked = crate::decomp::c_functions(&ctx.root)
            .into_iter()
            .map(|(f, _, _)| {
                let k = crate::decomp::last_check_in(&ctx.root, &names, &f);
                let ok = k.is_some_and(|k| k["status"] == "passed" && k["stale"] != true);
                (f, ok)
            })
            .collect();
        let names = names.functions.iter().map(|f| (crate::run::fold(f.address), f.name.text.clone())).collect();
        Code { functions, names, features, checked }
    }

    /// The function holding `pc` (folded): its entry. Where overlays share
    /// the address, one a feature lists comes first.
    fn holding(&self, pc: u32) -> Option<u32> {
        let mut found =
            self.functions.iter().filter(|(_, cs)| cs.iter().any(|&(s, e)| pc >= s && pc < e)).map(|(entry, _)| *entry);
        let first = found.next()?;
        Some(std::iter::once(first).chain(found).find(|e| self.features.contains_key(e)).unwrap_or(first))
    }

    /// A function as the pointer names it: name, entry, the features
    /// whose code lists it, and its C (checked, unchecked, or none).
    fn describe(&self, entry: u32) -> Value {
        let name = self.names.get(&entry);
        let c = match name.and_then(|n| self.checked.get(n)) {
            Some(true) => "checked",
            Some(false) => "unchecked",
            None => "none",
        };
        json!({"function": name.cloned().unwrap_or_else(|| format!("{:#010X}", shown(entry))),
               "entry": format!("{:#010X}", shown(entry)),
               "features": self.features.get(&entry).cloned().unwrap_or_default(), "c": c})
    }
}

/// The features other than `own` whose code the pointer `p` names, in
/// the order it names them.
pub fn features_named(p: &Value, own: &str) -> Vec<String> {
    let qs = p["quantities"].as_array().into_iter().flatten();
    let lists = qs.flat_map(|q| [&q["writers"], &q["written_before"]]).chain([&p["first_run"]]);
    let mut out: Vec<String> = Vec::new();
    for f in lists.flat_map(|l| l.as_array().into_iter().flatten()) {
        for id in f["features"].as_array().into_iter().flatten().filter_map(Value::as_str) {
            if id != own && !out.iter().any(|o| o == id) {
                out.push(id.to_string());
            }
        }
    }
    out
}

/// Byte addresses as spans: `0x800A9D58..0x800A9D5C`, the end excluded.
fn spans(bytes: &[u32]) -> Vec<String> {
    let mut out: Vec<(u32, u32)> = Vec::new();
    for &b in bytes {
        match out.last_mut() {
            Some((_, e)) if *e == b => *e += 1,
            _ => out.push((b, b + 1)),
        }
    }
    out.iter().map(|(s, e)| format!("{:#010X}..{:#010X}", shown(*s), shown(*e))).collect()
}

/// Functions by how often they come up, most first, `SHOWN` of them.
fn ranked(code: &Code, counts: BTreeMap<u32, u64>, what: &str) -> Vec<Value> {
    let mut v: Vec<(u32, u64)> = counts.into_iter().collect();
    v.sort_by_key(|&(e, n)| (std::cmp::Reverse(n), e));
    v.into_iter()
        .take(SHOWN)
        .map(|(e, n)| {
            let mut d = code.describe(e);
            d[what] = json!(n);
            d
        })
        .collect()
}

/// The pointer for feature `e`, from what the replay of `recording` saw
/// for the `quantities` that differ. `done` says whether a feature passes
/// at this check; `with_c` is the recording's replay with the checked C,
/// when there was one.
pub(crate) fn pointer(
    code: &Code,
    e: &Entry,
    recording: &str,
    quantities: &[String],
    seen: &Seen,
    with_c: Option<&Value>,
    done: &dyn Fn(&str) -> bool,
) -> Value {
    let mut out = json!({"recording": recording});
    let mut evidence: Vec<String> = Vec::new();
    if !seen.reached {
        out["evidence"] = json!(["the replay on the original did not reach the difference in time"]);
        return out;
    }
    if let Some(why) = &seen.refused {
        out["evidence"] = json!([format!("the binding's reads could not be told apart: {why}")]);
        return out;
    }
    let unknown = |pc: u32| json!({"function": Value::Null, "pc": format!("{:#010X}", shown(pc))});
    let mut unheld: BTreeSet<u32> = BTreeSet::new();
    let mut qs = Vec::new();
    for q in quantities {
        let bytes = seen.reads.get(q).cloned().unwrap_or_default();
        let mut q_out = json!({"quantity": q, "reads": spans(&bytes)});
        if bytes.is_empty() {
            evidence.push(format!("{q}: no byte the binding reads changes it (a constant, or the pad)"));
            qs.push(q_out);
            continue;
        }
        let set: BTreeSet<u32> = bytes.iter().copied().collect();
        let mut writers: BTreeMap<u32, u64> = BTreeMap::new();
        let mut written: BTreeSet<u32> = BTreeSet::new();
        for (&(pc, b), &n) in &seen.stores {
            if set.contains(&b) {
                written.insert(b);
                match code.holding(pc) {
                    Some(f) => *writers.entry(f).or_default() += n as u64,
                    None => {
                        unheld.insert(pc);
                    }
                }
            }
        }
        let mut before: BTreeMap<u32, u64> = BTreeMap::new();
        for b in set.iter().filter(|b| !written.contains(b)) {
            let Some(last) = slot(*b).map(|s| seen.last[s]).filter(|l| *l != 0) else { continue };
            let pc = last & !1;
            match code.holding(pc) {
                Some(f) => *before.entry(f).or_default() += 1,
                None => {
                    unheld.insert(pc);
                }
            }
        }
        q_out["writers"] = json!(ranked(code, writers, "stores"));
        if !before.is_empty() {
            q_out["written_before"] = json!(ranked(code, before, "bytes"));
        }
        qs.push(q_out);
    }
    // Code first run there: by function, a word or a branch way.
    let mut first: BTreeMap<u32, u64> = BTreeMap::new();
    let mut called: BTreeSet<u32> = BTreeSet::new();
    for &pc in &seen.first_words {
        match code.holding(pc) {
            Some(f) => {
                *first.entry(f).or_default() += 1;
                if pc == f {
                    called.insert(f);
                }
            }
            None => {
                unheld.insert(pc);
            }
        }
    }
    for &(pc, _) in &seen.first_ways {
        if let Some(f) = code.holding(pc) {
            *first.entry(f).or_default() += 1;
        }
    }
    let mut first_run = ranked(code, first, "new");
    for f in &mut first_run {
        let entry = crate::commands::addr_value(&f["entry"]).map(crate::run::fold);
        f["called_first"] = json!(entry.is_some_and(|e| called.contains(&e)));
    }
    if !first_run.is_empty() {
        out["first_run"] = json!(first_run);
    }
    out["quantities"] = json!(qs);
    if !unheld.is_empty() {
        out["outside_the_inventory"] = json!(unheld.iter().take(SHOWN).map(|&pc| unknown(pc)).collect::<Vec<_>>());
    }
    // The evidence, by the functions shown: whose code each is.
    let mut functions: BTreeSet<u32> = BTreeSet::new();
    let lists = qs.iter().flat_map(|q| [&q["writers"], &q["written_before"]]).chain([&out["first_run"]]);
    for f in lists.flat_map(|l| l.as_array().into_iter().flatten()) {
        functions.extend(crate::commands::addr_value(&f["entry"]).map(crate::run::fold));
    }
    let name = |f: u32| code.describe(f)["function"].as_str().unwrap_or_default().to_string();
    let (mut own_unchecked, mut own_checked, mut nobody) = (vec![], vec![], vec![]);
    let mut others: BTreeMap<(bool, Vec<String>), Vec<String>> = BTreeMap::new();
    for &f in &functions {
        let d = code.describe(f);
        let fs: Vec<String> =
            d["features"].as_array().into_iter().flatten().filter_map(|x| x.as_str().map(String::from)).collect();
        if fs.contains(&e.id) {
            if d["c"] == "checked" { own_checked.push(name(f)) } else { own_unchecked.push(name(f)) }
        } else if fs.is_empty() {
            nobody.push(name(f));
        } else {
            let all_done = fs.iter().all(|x| done(x));
            others.entry((all_done, fs)).or_default().push(name(f));
        }
    }
    if !own_unchecked.is_empty() {
        evidence.push(format!(
            "{}: this feature's code, with no checked C: the original's rule there is not understood yet",
            own_unchecked.join(", ")
        ));
    }
    for ((all_done, fs), fns) in &others {
        let fs = fs.join(", ");
        evidence.push(if *all_done {
            format!("{}: code of {fs}, which pass: shared code, which may be given something else here", fns.join(", "))
        } else {
            format!("{}: code of {fs}, not done: the rule may be there", fns.join(", "))
        });
    }
    if !nobody.is_empty() {
        evidence.push(format!("{}: in no feature's code", nobody.join(", ")));
    }
    if let Some(c) = with_c {
        let mut kept = json!({"status": c["status"], "share_in_c": c["share_in_c"]});
        if !c["differs_in"].is_null() {
            kept["differs_in"] = c["differs_in"].clone();
        }
        out["with_c"] = kept;
        match c["status"].as_str() {
            Some("same") if !own_checked.is_empty() && own_unchecked.is_empty() => evidence.push(format!(
                "checked C covers {} and the recording replays with it: the original's rule is known there; the port may not follow the C, or what is around it differs (code on the path with no C, the data the C reads, the launch from the scenario's parameters, the order the tick calls things in)",
                own_checked.join(", ")
            )),
            Some("diverges_with_c") => evidence.push(
                "the recording does not replay with the checked C: a function it replaces does not do what the original's does here".into(),
            ),
            _ => {}
        }
    }
    out["evidence"] = json!(evidence);
    out
}

/// Pointers for the failing features: one recording each (its first work
/// recording that differs), each recording replayed once, all within
/// `budget`. `failing` gives each feature's recording, quantities and
/// first tick; `done`, whether a feature passes at this check.
pub(crate) fn pointers(
    ctx: &Ctx,
    port: &Port,
    failing: &[(&Entry, String, Vec<String>, u64)],
    done: &dyn Fn(&str) -> bool,
    budget: Duration,
) -> BTreeMap<String, Value> {
    let started = Instant::now();
    let code = Code::load(ctx, port);
    let replacement = crate::decomp::Replacement::new(&ctx.root, &[]).ok().filter(|r| !r.functions.is_empty());
    let mut seen: BTreeMap<String, Result<(Seen, Option<Value>), String>> = BTreeMap::new();
    let mut out = BTreeMap::new();
    for (e, recording, quantities, at) in failing {
        let left = budget.saturating_sub(started.elapsed());
        let Some((id, name)) = recording.split_once('/') else { continue };
        let s = seen.entry(recording.clone()).or_insert_with(|| {
            if left.is_zero() {
                return Err("no time left in this check for it".into());
            }
            let s = look(ctx, id, name, *at, left).map_err(|e| e.to_string())?;
            // The checked C, where the pointer's code has some.
            let c = replacement.as_ref().filter(|_| s.reached).and_then(|d| {
                let left = budget.saturating_sub(started.elapsed());
                let (_, c) = crate::scenario::with_c(ctx, id, Some(&[name.to_string()]), d, left).ok()?;
                c["recordings"].get(0).cloned()
            });
            Ok((s, c))
        });
        let v = match s {
            Ok((s, c)) => pointer(&code, e, recording, quantities, s, c.as_ref(), done),
            Err(why) => json!({"recording": recording, "evidence": [format!("no pointer: {why}")]}),
        };
        out.insert(e.id.clone(), v);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spans_join_neighbouring_bytes() {
        assert_eq!(spans(&[0x10, 0x11, 0x12, 0x20]), ["0x80000010..0x80000013", "0x80000020..0x80000021"]);
        assert_eq!(tick_of("initial"), Some(0));
        assert_eq!(tick_of("tick 12"), Some(12));
    }

    #[test]
    fn the_evidence_names_whose_code_it_is() {
        let mut code = Code {
            functions: vec![(0x1000, vec![(0x1000, 0x1100)]), (0x2000, vec![(0x2000, 0x2100)])],
            names: [(0x1000, "grip".to_string()), (0x2000, "collide".to_string())].into(),
            features: [(0x1000, vec!["F-0007".to_string()]), (0x2000, vec!["F-0010".to_string()])].into(),
            checked: BTreeMap::new(),
        };
        let mut seen = Seen { reached: true, last: vec![0; (RAM + 0x400) as usize], ..Seen::default() };
        seen.reads.insert("speed".into(), vec![0x5000, 0x5001]);
        seen.stores.insert((0x1010, 0x5000), 3);
        seen.first_words = vec![0x2000, 0x2004];
        let e = Entry { id: "F-0007".into(), ..Entry::default() };
        let p = pointer(&code, &e, "b1/offroad", &["speed".into()], &seen, None, &|_| false);
        assert_eq!(p["quantities"][0]["reads"], json!(["0x80005000..0x80005002"]));
        assert_eq!(p["quantities"][0]["writers"][0]["function"], "grip");
        assert_eq!(p["first_run"][0]["function"], "collide");
        assert_eq!(p["first_run"][0]["called_first"], true);
        let ev = p["evidence"].to_string();
        assert!(ev.contains("collide: code of F-0010, not done"), "{ev}");
        assert!(ev.contains("grip: this feature's code, with no checked C"), "{ev}");
        // port next's Fix names the other features it finds.
        assert_eq!(features_named(&p, "F-0007"), ["F-0010"]);
        // Checked C that replays the recording: the rule is known there.
        code.checked.insert("grip".into(), true);
        let c = json!({"status": "same", "share_in_c": 0.2});
        let p = pointer(&code, &e, "b1/offroad", &["speed".into()], &seen, Some(&c), &|_| false);
        assert!(p["evidence"].to_string().contains("checked C covers grip and the recording replays with it"));
        assert!(!p.to_string().contains("\"at\""), "{p}");
    }
}
