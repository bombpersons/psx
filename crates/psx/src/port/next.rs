//! What a port's goal needs next (docs/port-design.md, section 5): the facts, read once
//! per call; the stages they give each feature in the goal's closure; and
//! the items, each due on a fact and done on a fact the work leaves behind.
//! `port status` shows the facts, `port next` gives the one item due.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use super::assets::{self, State as AssetState};
use super::check;
use super::review::{self, Review};
use super::verdict::{self, State};
use super::{Confirmation, Port, Stage};
use crate::Error;
use crate::commands::{Ctx, Op, Outcome};
use crate::script_cover::Stretch;

/// The workspace's commit.
#[derive(Debug, Clone, Default)]
pub struct Git {
    /// None before the first commit, or outside a repository.
    pub head: Option<String>,
    /// No uncommitted changes but to notes and reviews, which a check does
    /// not read.
    pub clean: bool,
    /// A hash of the uncommitted changes, so the loop guard sees work in
    /// progress.
    pub changes: String,
}

fn git(root: &Path, args: &[&str]) -> Option<String> {
    let out = std::process::Command::new("git").args(args).current_dir(root).output().ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

impl Git {
    pub fn read(root: &Path) -> Git {
        let head = git(root, &["rev-parse", "--verify", "-q", "HEAD"]).map(|s| s.trim().to_string());
        let status = git(root, &["status", "--porcelain"]).unwrap_or_default();
        let diff = git(root, &["diff", "HEAD"]).unwrap_or_default();
        let changes = format!("{:x}", Sha256::digest(format!("{status}\n{diff}")));
        // `XY path`, or `XY from -> to` for a rename.
        let clean = status.lines().all(|l| l.get(3..).is_some_and(|p| p.split(" -> ").all(super::verdict::inert)));
        Git { head, clean, changes }
    }
}

/// Where a branch of a feature's code is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Place {
    /// A conditional branch of its machine code, in the function at `function`.
    Code { pc: u32, function: u32 },
    /// A stretch of a script program it names: one read or never read (run
    /// or never run, with the program's map), or a decision; the routine
    /// the map puts it in (docs/script-coverage-design.md).
    Script { at: Stretch, routine: Option<String> },
}

/// A branch of a feature's code, as its recordings ran it: a conditional
/// branch of its machine code, or a stretch or decision of its script.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Branch {
    pub at: Place,
    /// What no recording did there ("fell through", "read", "went way 1
    /// of 2"); empty when they did it all.
    pub never: String,
    /// Listed under wont_reach: whether the person confirmed it as it stands.
    pub wont_reach: Option<bool>,
    /// What a code branch tests, when the code before it says.
    pub test: Option<crate::flow::Test>,
    /// A code branch no recording reached behind another's way none took:
    /// that branch, which answers for it.
    pub behind: Option<u32>,
    /// How often its recordings ran it.
    pub runs: u64,
}

impl Branch {
    /// A conditional branch of machine code whose recordings took `ways`
    /// (1 taken, 2 fell through).
    pub fn code(pc: u32, function: u32, ways: u8, wont_reach: Option<bool>) -> Branch {
        let never = match ways {
            0 => "ran",
            1 => "fell through",
            2 => "taken",
            _ => "",
        };
        Branch { at: Place::Code { pc, function }, never: never.into(), wont_reach, test: None, behind: None, runs: 0 }
    }

    pub fn resolved(&self) -> bool {
        self.never.is_empty() || self.wont_reach == Some(true)
    }

    /// Unresolved and answered for by no other branch: one the gaps list.
    pub fn open(&self) -> bool {
        !self.resolved() && self.behind.is_none()
    }

    fn pc(&self) -> Option<u32> {
        match self.at {
            Place::Code { pc, .. } => Some(pc),
            Place::Script { .. } => None,
        }
    }

    /// What it tests and the outcome no recording saw: `[0x8007529C] == 2`
    /// never true.
    fn never_test(&self) -> Option<String> {
        let t = self.test.as_ref()?;
        let held = match self.never.as_str() {
            "taken" => !t.taken,
            "fell through" => t.taken,
            _ => return Some(format!("{} (never ran)", t.key)),
        };
        Some(format!("{} never {}", t.key, if held { "false" } else { "true" }))
    }

    /// What no recording did, and where.
    pub fn describe(&self) -> String {
        match &self.at {
            Place::Code { pc, function } => {
                let mut s = format!("the branch at {} (in {}) never {}", hex(*pc), hex(*function), self.never);
                if let Some(t) = self.never_test() {
                    s += &format!(": {t}");
                }
                s
            }
            Place::Script { at, routine } => {
                let at = match routine {
                    Some(r) => format!("{at} (in {r})"),
                    None => at.to_string(),
                };
                match self.never.as_str() {
                    "read" => format!("the script's bytes {at} were never read"),
                    "ran" => format!("the script's statements {at} never ran"),
                    never => format!("the script's decision at {at} never {never}"),
                }
            }
        }
    }

    fn json(&self) -> Value {
        match &self.at {
            Place::Code { pc, function } => {
                let mut v = json!({"pc": hex(*pc), "function": hex(*function), "never": self.never, "runs": self.runs});
                if let Some(t) = self.never_test() {
                    v["tests"] = json!(t);
                }
                v
            }
            Place::Script { at, routine: None } => json!({"at": at.to_string(), "never": self.never}),
            Place::Script { at, routine: Some(r) } => {
                json!({"at": at.to_string(), "routine": r, "never": self.never})
            }
        }
    }
}

fn hex(a: u32) -> String {
    format!("0x{a:08X}")
}

/// One feature of the closure, as the facts give it.
#[derive(Debug, Clone)]
pub struct Feature {
    pub id: String,
    pub in_goal: bool,
    pub catalogued: bool,
    /// Its entry names its code.
    pub located: bool,
    /// What keeps it from being recorded: no work recordings, its
    /// scenarios' problems or determinism checks, coverage psx cannot
    /// count.
    pub unrecorded: Vec<String>,
    /// Its recordings in which the original's driver kept the tick's input
    /// from the game, each with what it asks for (fix, record again, or
    /// change a fix that does not work).
    pub late: Vec<(crate::scenario::Late, String)>,
    pub verdict: State,
    /// Launchers and observers the port lacks for its scenarios, by the
    /// last check at HEAD.
    pub unbuilt: Vec<String>,
    /// Each recording with its failures (empty: passed), by the verdict at
    /// HEAD; None without one.
    pub work: Vec<(String, Option<Vec<String>>)>,
    pub held_out: Vec<(String, Option<Vec<String>>)>,
    pub branches: Vec<Branch>,
    /// The other features' recordings its branches also count (their work
    /// recordings passing at HEAD), and how many branches only they
    /// resolve.
    pub borrowed: Vec<String>,
    pub borrowed_resolved: usize,
    pub questions: Vec<String>,
    /// The code's review holds (one review covers every feature).
    pub code_reviewed: bool,
    /// The disc's assets its entry names, as the inventory has them.
    pub assets: assets::Needs,
    /// Its code a scenario turns off (`[off]`), as `scenario: function`:
    /// it is not done while the original's recordings run without it.
    pub off: Vec<String>,
}

/// Every recording passed.
fn all_pass(rs: &[(String, Option<Vec<String>>)]) -> bool {
    !rs.is_empty() && rs.iter().all(|(_, f)| f.as_ref().is_some_and(Vec::is_empty))
}

/// The failing recordings, each with its failures.
fn failing(rs: &[(String, Option<Vec<String>>)]) -> Vec<(&String, &Vec<String>)> {
    rs.iter().filter_map(|(n, f)| f.as_ref().filter(|f| !f.is_empty()).map(|f| (n, f))).collect()
}

/// The tick a failure line names (`... tick 32 croc_y ...`), for the
/// earliest divergence.
fn tick_of(line: &str) -> u64 {
    line.split_once("tick ")
        .and_then(|(_, t)| t.split(|c: char| !c.is_ascii_digit()).next()?.parse().ok())
        .unwrap_or(u64::MAX)
}

/// The failing recording that diverges first, with its first failure.
fn earliest(rs: &[(String, Option<Vec<String>>)]) -> Option<(String, String)> {
    failing(rs).into_iter().filter_map(|(n, f)| Some((n.clone(), f.first()?.clone()))).min_by_key(|(_, l)| tick_of(l))
}

impl Feature {
    /// Its own work is finished: matching, every branch resolved, no
    /// question open.
    pub fn finished(&self) -> bool {
        self.matching() && self.branches_resolved() && self.questions.is_empty()
    }

    pub fn matching(&self) -> bool {
        matches!(self.verdict, State::Current(_)) && all_pass(&self.work)
    }

    pub fn branches_resolved(&self) -> bool {
        self.branches.iter().all(Branch::resolved)
    }

    /// The last stage whose fact holds, with every one before it; None
    /// when the feature has no catalogue entry.
    pub fn stage(&self) -> Option<Stage> {
        if !self.catalogued {
            return None;
        }
        let checks = [
            self.located,
            self.unrecorded.is_empty() && self.late.is_empty(),
            self.matching(),
            self.branches_resolved() && self.questions.is_empty() && self.code_reviewed && self.assets.settled(),
            all_pass(&self.held_out) && self.off.is_empty(),
        ];
        Some(Stage::ALL[checks.iter().take_while(|c| **c).count()])
    }

    fn reviewed(&self) -> bool {
        self.stage() >= Some(Stage::Reviewed)
    }

    /// Its work recordings, as status shows them.
    fn work_json(&self) -> Value {
        let pass = self.work.iter().filter(|(_, f)| f.as_ref().is_some_and(Vec::is_empty)).count();
        let checked = self.work.iter().all(|(_, f)| f.is_some());
        let mut v = json!({"total": self.work.len(), "pass": checked.then_some(pass)});
        if let Some((r, first)) = earliest(&self.work) {
            v["first_failure"] = json!(format!("{r}: {first}"));
        }
        v
    }

    /// Its held-out recordings: how many, and once it is reviewed how they did.
    fn held_out_json(&self) -> Value {
        if !self.reviewed() {
            return json!({"total": self.held_out.len(), "shown": "once the code is reviewed"});
        }
        let pass = self.held_out.iter().filter(|(_, f)| f.as_ref().is_some_and(Vec::is_empty)).count();
        let mut v = json!({"total": self.held_out.len(), "pass": pass});
        if let Some((r, first)) = earliest(&self.held_out) {
            v["first_failure"] = json!(format!("{r}: {first}"));
        }
        v
    }

    /// Its open branches by what they test and the outcome no recording
    /// saw, the largest group first; a branch whose test psx cannot read
    /// is a group of its own.
    pub fn groups(&self) -> Vec<Vec<&Branch>> {
        let mut out: Vec<(Option<String>, Vec<&Branch>)> = Vec::new();
        for b in self.branches.iter().filter(|b| b.open()) {
            let key = b.never_test();
            match key.as_ref().and_then(|k| out.iter_mut().find(|g| g.0.as_ref() == Some(k))) {
                Some(g) => g.1.push(b),
                None => out.push((key, vec![b])),
            }
        }
        out.sort_by_key(|g| std::cmp::Reverse(g.1.len()));
        out.into_iter().map(|g| g.1).collect()
    }

    /// The branches, with the first five groups of open ones, or all of them.
    fn branches_json(&self, all: bool) -> Value {
        let unresolved = self.branches.iter().filter(|b| !b.resolved()).count();
        let one = |b: &Branch| {
            let mut v = b.json();
            if b.wont_reach == Some(false) {
                v["wont_reach"] = json!("not confirmed by the person");
            }
            let behind = self.branches.iter().filter(|x| x.behind.is_some() && x.behind == b.pc()).count();
            if behind > 0 {
                v["behind"] = json!(behind);
            }
            v
        };
        let groups = self.groups();
        let uncovered: Vec<Value> = groups
            .iter()
            .take(if all { usize::MAX } else { 5 })
            .map(|g| match g.as_slice() {
                [b] => one(b),
                g => {
                    let t = g[0].test.as_ref().map(|t| t.key.clone());
                    json!({"tests": g[0].never_test(), "branches": g.len(),
                        "runs": g.iter().map(|b| b.runs).sum::<u64>(),
                        "behind": g.iter().map(|b| self.branches.iter().filter(|x| x.behind.is_some() && x.behind == b.pc()).count()).sum::<usize>(),
                        "wont_reach_key": t,
                        "first": g.iter().take(3).map(|b| one(b)).collect::<Vec<_>>()})
                }
            })
            .collect();
        let mut v = json!({"total": self.branches.len(), "resolved": self.branches.len() - unresolved,
               "open_groups": groups.len(), "unresolved": uncovered});
        if self.borrowed_resolved > 0 {
            v["resolved_by_others"] = json!({"branches": self.borrowed_resolved, "recordings": self.borrowed.len(),
                "first": first_lines(&self.borrowed)});
        }
        if uncovered.len() < groups.len() {
            v["more"] =
                json!(format!("{} more groups: port_status feature {}", groups.len() - uncovered.len(), self.id));
        }
        v
    }

    /// One line for the person: recordings, held out, branches, review.
    fn summary(&self) -> String {
        if !self.located {
            return "code not found yet".into();
        }
        if self.work.is_empty() {
            return "no recordings".into();
        }
        if let Some(why) = self.unrecorded.first() {
            return format!("{} recordings, not ready: {why}", self.work.len());
        }
        let mut parts = Vec::new();
        let pass = self.work.iter().filter(|(_, f)| f.as_ref().is_some_and(Vec::is_empty)).count();
        if self.work.iter().all(|(_, f)| f.is_some()) {
            let mut p = format!("{pass}/{} pass", self.work.len());
            if let Some((r, first)) = earliest(&self.work) {
                p += &format!(" · first: {r} {first}");
            }
            parts.push(p);
        } else {
            parts.push(format!("{} recordings, not checked at HEAD", self.work.len()));
        }
        parts.push(match (self.held_out.len(), self.reviewed()) {
            (0, _) => "held out: none yet".into(),
            (n, false) => format!("{n} held out"),
            (n, true) => {
                let p = self.held_out.iter().filter(|(_, f)| f.as_ref().is_some_and(Vec::is_empty)).count();
                format!("held out {p}/{n} pass")
            }
        });
        if !self.branches.is_empty() {
            let r = self.branches.iter().filter(|b| b.resolved()).count();
            let mut p = if r == self.branches.len() {
                format!("branches {r}/{r} resolved")
            } else {
                format!("{r}/{} branches", self.branches.len())
            };
            if self.borrowed_resolved > 0 {
                p += &format!(" ({} by other features' recordings)", self.borrowed_resolved);
            }
            parts.push(p);
        }
        parts.join(" · ")
    }
}

/// Each scenario's functions turned off (`[off]` in `scenarios/<id>.toml`),
/// as (scenario, name, address); a name that does not resolve is left out
/// (recording that scenario reports it).
fn turned_off(root: &Path) -> Vec<(String, String, u32)> {
    let mut out = Vec::new();
    for id in crate::scenario::ids(root) {
        let p = root.join(crate::scenario::SCENARIOS).join(format!("{id}.toml"));
        let Some(s) = std::fs::read_to_string(&p).ok().and_then(|t| psx_scenario::scenario::Scenario::parse(&t).ok())
        else {
            continue;
        };
        for name in s.off.keys() {
            if let Ok(a) = crate::names_bind::address(name) {
                out.push((id.clone(), name.clone(), a));
            }
        }
    }
    out
}

/// Each feature's work recordings that pass at HEAD, by id (`of` gives
/// them, None without a verdict at HEAD).
fn passing(ids: &[String], of: impl Fn(&str) -> Option<Vec<String>>) -> Vec<(String, Vec<String>)> {
    ids.iter().filter_map(|id| Some((id.clone(), of(id)?))).filter(|(_, r)| !r.is_empty()).collect()
}

/// The wont_reach key that answers for the code branch at `pc` its
/// recordings took `ways` (0 never ran): its own address, else a range
/// holding it when it never ran.
fn code_key<'a>(wont_reach: &'a BTreeMap<String, String>, pc: u32, ways: u8, test: Option<&str>) -> Option<&'a String> {
    let within = |k: &String| super::entry_range(k).is_some_and(|(start, end)| start <= pc && pc < end);
    wont_reach
        .keys()
        .find(|k| super::entry_addr(k) == Some(pc))
        .or_else(|| (ways == 0).then(|| wont_reach.keys().find(|k| within(k))).flatten())
        .or_else(|| test.and_then(|t| wont_reach.keys().find(|k| k.as_str() == t)))
}

/// The wont_reach tests no branch of the code tests.
fn tests_unmatched(wont_reach: &BTreeMap<String, String>, branches: &[Branch]) -> Vec<String> {
    wont_reach
        .keys()
        .filter(|k| crate::flow::is_test(k))
        .filter(|k| !branches.iter().any(|b| b.test.as_ref().is_some_and(|t| &t.key == *k)))
        .map(|k| format!("wont_reach: {k} is the test of no branch of its code (port next gives each group's test as psx reads it)"))
        .collect()
}

/// The wont_reach ranges holding code branches a recording ran: a range
/// holds only code none reaches.
fn ranges_run(wont_reach: &BTreeMap<String, String>, branches: &[Branch]) -> Vec<String> {
    let ran: Vec<u32> = branches
        .iter()
        .filter_map(|b| match b.at {
            Place::Code { pc, .. } if b.never != "ran" => Some(pc),
            _ => None,
        })
        .collect();
    wont_reach
        .keys()
        .filter_map(|k| {
            let (start, end) = super::entry_range(k)?;
            let pc = ran.iter().find(|&&pc| start <= pc && pc < end)?;
            Some(format!(
                "wont_reach: {k} holds the branch at {}, which a recording ran: a range holds only code no recording reaches",
                hex(*pc)
            ))
        })
        .collect()
}

/// The recordings of the other features `passing` gives that `id`'s
/// branches also count: not its own, each once.
fn borrowed(id: &str, own: &[String], passing: &[(String, Vec<String>)]) -> Vec<String> {
    let mut out: Vec<String> = passing
        .iter()
        .filter(|(other, _)| other != id)
        .flat_map(|(_, r)| r)
        .filter(|r| !own.contains(r))
        .cloned()
        .collect();
    out.sort();
    out.dedup();
    out
}

/// Up to three lines, and how many more there are.
fn first_lines(lines: &[String]) -> Vec<String> {
    let mut out: Vec<String> = lines.iter().take(3).cloned().collect();
    if lines.len() > 3 {
        out.push(format!("and {} more", lines.len() - 3));
    }
    out
}

/// A request's text on one line, shortened.
fn one_line(s: &str) -> String {
    let s = s.split_whitespace().collect::<Vec<_>>().join(" ");
    match s.char_indices().nth(120) {
        Some((i, _)) => format!("{}…", &s[..i]),
        None => s,
    }
}

/// Everything the items are decided on.
pub struct Facts {
    pub port: Port,
    pub git: Git,
    /// The closure in dependency order.
    pub features: Vec<Feature>,
    /// Overlays the inventory knows only from the code that ran in them.
    pub unpack: Vec<String>,
    /// The disc's files by state, or why psx cannot list them.
    pub assets: Result<assets::Inventory, String>,
    /// What the goal's own asset names need.
    pub needs: assets::Needs,
    /// The code's review.
    pub review: Review,
    /// The features it lists as matching when it was written, and as
    /// finished.
    pub review_features: Vec<String>,
    pub review_finished: Vec<String>,
    /// The play requests in the queue (`scenario play`), oldest first.
    pub plays: Vec<crate::handoff::queue::Request>,
    /// Debug files `port init`'s scan found on the disc, until `code
    /// symbols` reports them to the agent.
    pub debug_files: Vec<String>,
    /// Functions named, in C and passing ([`crate::decomp::counts`]).
    pub decomp: Value,
    /// Functions whose C passed and now fails ([`crate::decomp::regressed`]).
    pub regressed: Vec<(String, Option<String>, String)>,
}

impl Facts {
    pub fn gather(root: &Path) -> crate::Result<Facts> {
        let mut port = Port::load(root)?;
        let git = Git::read(root);
        let goal = port.goal_ids();
        let ctx = Ctx { root: root.to_path_buf(), cancel: None };
        let inv = crate::coverage::inventory(&ctx, None).ok().map(|(_, v)| v);
        let unpack = inv
            .iter()
            .flat_map(|inv| inv["overlays"].as_array().cloned().unwrap_or_default())
            .filter(|o| o["bounded_by"] == "found_code")
            .filter_map(|o| o["name"].as_str().map(String::from))
            .collect();
        let confirmed = port.goal.confirmed.as_ref().map(|c| c.wont_reach.clone()).unwrap_or_default();
        let head = git.head.as_deref();
        // A check judges HEAD as committed: uncommitted changes to what it
        // reads (a recording made again, the port's code) leave it stale.
        let judged = head.filter(|_| git.clean);
        let review = review::read(root);
        let assets = assets::inventory(root, &port, inv.as_ref(), head);
        let needs = |names: &[String], all: bool, file: &str, problems: &mut Vec<super::Problem>| {
            let n = assets::Needs::of(names, all, &port, &assets);
            for (name, why) in n.unresolved.iter().filter(|(_, w)| *w == assets::Unresolved::NoMatch) {
                problems.push(super::problem(file, format!("assets: {name} {}", why.describe())));
            }
            n
        };
        let mut problems = Vec::new();
        let goal_needs = needs(port.goal.assets.names(), port.goal.assets.is_all(), super::GOAL, &mut problems);
        let closure = port.closure();
        let passing = passing(&closure, |id| {
            let e = port.entries.get(id)?;
            match verdict::state(root, id, judged) {
                State::Current(v) => Some(e.recordings.work.iter().filter(|n| v.of(n).is_empty()).cloned().collect()),
                _ => None,
            }
        });
        let path =
            |r: &String| r.split_once('/').map(|(s, n)| root.join("scenarios").join(s).join(format!("{n}.jsonl")));
        let turned_off = turned_off(root);
        let features: Vec<Feature> = closure
            .into_iter()
            .map(|id| {
                let Some(e) = port.entries.get(&id) else {
                    return Feature {
                        in_goal: goal.contains(&id),
                        catalogued: false,
                        located: false,
                        unrecorded: Vec::new(),
                        late: Vec::new(),
                        verdict: State::Missing,
                        unbuilt: Vec::new(),
                        work: Vec::new(),
                        held_out: Vec::new(),
                        branches: Vec::new(),
                        borrowed: Vec::new(),
                        borrowed_resolved: 0,
                        questions: Vec::new(),
                        code_reviewed: false,
                        assets: Default::default(),
                        off: Vec::new(),
                        id,
                    };
                };
                let mut unrecorded = if e.recordings.work.is_empty() {
                    vec![format!("{}: names no work recordings (recordings.work: <scenario>/<name>)", e.file)]
                } else {
                    crate::scenario::feature_unready(root, e)
                };
                let paths: Vec<PathBuf> = e.recordings.all().filter_map(path).collect();
                let mine = confirmed.get(&id);
                // The person confirmed the key as it stands.
                let agreed = |k: &String| mine.and_then(|m| m.get(k)) == e.wont_reach.get(k);
                let code: Vec<u32> = e.code.iter().filter_map(|c| super::entry_addr(c)).collect();
                let script: Vec<Stretch> = e.code.iter().filter_map(|c| Stretch::parse(c)).collect();
                // The ways `paths` took the branches of its code, and what
                // stops psx counting them.
                let branches_of = |paths: &[PathBuf]| -> (Vec<Branch>, Vec<String>) {
                    let (mut branches, mut problems) = (Vec::new(), Vec::new());
                    if e.recordings.work.is_empty() {
                        return (branches, problems);
                    }
                    if let (Some(inv), false) = (&inv, code.is_empty()) {
                        let fb = crate::coverage::feature_branches(&ctx, inv, paths, &code);
                        problems.extend(fb.problems);
                        let mut found: Vec<Branch> = fb
                            .branches
                            .into_iter()
                            .map(|c| {
                                let key =
                                    code_key(&e.wont_reach, c.pc, c.ways, c.test.as_ref().map(|t| t.key.as_str()));
                                Branch {
                                    test: c.test,
                                    behind: c.behind,
                                    runs: c.runs,
                                    ..Branch::code(c.pc, c.function, c.ways, key.map(agreed))
                                }
                            })
                            .collect();
                        // A branch behind another's missed way is answered
                        // as that way is, unless listed itself.
                        let answered: BTreeMap<u32, Option<bool>> = found
                            .iter()
                            .filter_map(|b| match b.at {
                                Place::Code { pc, .. } => Some((pc, b.wont_reach)),
                                _ => None,
                            })
                            .collect();
                        for b in found.iter_mut().filter(|b| b.wont_reach.is_none()) {
                            if let Some(p) = b.behind {
                                b.wont_reach = answered.get(&p).copied().flatten();
                            }
                        }
                        branches.extend(found);
                    }
                    if !script.is_empty() {
                        let (items, p) = crate::script_cover::items(&crate::script_cover::merged(paths), &script);
                        problems.extend(p);
                        branches.extend(items.into_iter().map(|i| {
                            let key =
                                e.wont_reach.keys().find(|k| Stretch::parse(k).is_some_and(|w| w.contains(&i.at)));
                            Branch {
                                at: Place::Script { at: i.at, routine: i.routine },
                                never: i.never,
                                wont_reach: key.map(agreed),
                                test: None,
                                behind: None,
                                runs: 0,
                            }
                        }));
                    }
                    (branches, problems)
                };
                let (mut branches, uncounted) = branches_of(&paths);
                unrecorded.extend(uncounted);
                // Code the other features' recordings run counts their
                // ways too, once the port matches them (their problems
                // are theirs).
                let borrowed = borrowed(&id, &e.recordings.all().cloned().collect::<Vec<_>>(), &passing);
                let mut borrowed_resolved = 0;
                if !borrowed.is_empty() && !branches.is_empty() {
                    let all: Vec<PathBuf> = paths.iter().cloned().chain(borrowed.iter().filter_map(path)).collect();
                    let own = branches.iter().filter(|b| b.resolved()).count();
                    branches = branches_of(&all).0;
                    borrowed_resolved = branches.iter().filter(|b| b.resolved()).count().saturating_sub(own);
                }
                problems.extend(ranges_run(&e.wont_reach, &branches).into_iter().map(|p| super::problem(&e.file, p)));
                if !code.is_empty() && inv.is_some() && !e.recordings.work.is_empty() {
                    problems.extend(
                        tests_unmatched(&e.wont_reach, &branches).into_iter().map(|p| super::problem(&e.file, p)),
                    );
                }
                let verdict = verdict::state(root, &id, judged);
                let results = |names: &[String]| -> Vec<(String, Option<Vec<String>>)> {
                    names
                        .iter()
                        .map(|n| (n.clone(), if let State::Current(v) = &verdict { Some(v.of(n)) } else { None }))
                        .collect()
                };
                let (work, held_out) = (results(&e.recordings.work), results(&e.recordings.held_out));
                let unbuilt = if matches!(verdict, State::Current(_)) && !e.recordings.work.is_empty() {
                    check::unimplemented(root, e, head)
                } else {
                    Vec::new()
                };
                Feature {
                    in_goal: goal.contains(&id),
                    catalogued: true,
                    located: !e.code.is_empty(),
                    unrecorded,
                    late: crate::scenario::feature_late_input(root, e),
                    verdict,
                    unbuilt,
                    work,
                    held_out,
                    branches,
                    borrowed,
                    borrowed_resolved,
                    questions: e.questions.clone(),
                    code_reviewed: review.holds(),
                    assets: needs(&e.assets, false, &e.file, &mut problems),
                    off: turned_off
                        .iter()
                        .filter(|(_, _, a)| code.iter().any(|c| c & 0x1FFF_FFFF == a & 0x1FFF_FFFF))
                        .map(|(s, n, _)| format!("{s}: {n}"))
                        .collect(),
                    id,
                }
            })
            .collect();
        port.problems.extend(problems);
        if let Ok(a) = &assets {
            port.problems.extend(a.problems.iter().cloned());
        }
        let (review_features, review_finished) = review::features(root);
        let plays = crate::handoff::queue::list(root);
        Ok(Facts {
            port,
            git,
            features,
            unpack,
            assets,
            needs: goal_needs,
            review,
            review_features,
            review_finished,
            plays,
            debug_files: crate::disc_names::unread(root),
            decomp: crate::decomp::counts(root),
            regressed: crate::decomp::regressed(root),
        })
    }

    /// How many never-ran code branches each wont_reach range answers
    /// for, by feature.
    pub fn wont_reach_ranges(&self) -> BTreeMap<String, BTreeMap<String, usize>> {
        let mut out: BTreeMap<String, BTreeMap<String, usize>> = BTreeMap::new();
        for f in &self.features {
            let Some(e) = self.port.entries.get(&f.id) else { continue };
            for k in e.wont_reach.keys().filter(|k| super::entry_range(k).is_some()) {
                let answers = |b: &&Branch| match b.at {
                    Place::Code { pc, .. } => code_key(&e.wont_reach, pc, (b.never != "ran") as u8, None) == Some(k),
                    _ => false,
                };
                out.entry(f.id.clone()).or_default().insert(k.clone(), f.branches.iter().filter(answers).count());
            }
        }
        out
    }

    pub fn stage(&self, id: &str) -> Option<Stage> {
        self.features.iter().find(|f| f.id == id).and_then(Feature::stage)
    }

    /// Every feature of the closure done.
    pub fn complete(&self) -> bool {
        self.port.confirmation() != Confirmation::Missing
            && !self.features.is_empty()
            && self.features.iter().all(|f| f.stage() == Some(Stage::Done))
            && self.needs.settled()
    }

    /// The files the goal and its features need, by index, and how many
    /// of them are checked or unused.
    fn needed(&self) -> (std::collections::BTreeSet<usize>, usize) {
        let files: std::collections::BTreeSet<usize> =
            self.features.iter().flat_map(|f| &f.assets.files).chain(&self.needs.files).copied().collect();
        let settled = match &self.assets {
            Ok(a) => files.iter().filter(|&&i| a.files[i].state.settled()).count(),
            Err(_) => 0,
        };
        (files, settled)
    }

    /// `port status --assets NAME`: the files a name covers, or why none.
    pub fn assets_named(&self, name: &str) -> crate::Result<Value> {
        if let Some(p) = super::asset_name_problem(name) {
            return Err(Error::invalid(p));
        }
        let a =
            self.assets.as_ref().map_err(|why| Error::invalid(format!("psx cannot list the disc's files: {why}")))?;
        let needed = self.needed().0;
        Ok(match assets::resolve(name, &self.port, a) {
            Err(why) => json!({"name": name, "files": [], "unresolved": why.describe()}),
            Ok(files) => {
                let rows: Vec<Value> = files
                    .iter()
                    .map(|&i| {
                        let f = &a.files[i];
                        let mut v = json!({"path": f.path, "format": f.format, "state": f.state.name(), "needed": needed.contains(&i)});
                        if f.unlisted {
                            v["archive"] = json!("entries not listed yet");
                        }
                        v
                    })
                    .collect();
                json!({"name": name, "count": rows.len(), "files": rows})
            }
        })
    }

    fn title(&self, id: &str) -> String {
        self.port.entries.get(id).map(|e| e.title.clone()).unwrap_or_default()
    }

    /// The item due now, in the order of docs/port-design.md, section 5.
    /// The play requests still waiting for the person.
    fn waiting_plays(&self) -> impl Iterator<Item = &crate::handoff::queue::Request> {
        self.plays.iter().filter(|r| !matches!(r.state, crate::handoff::queue::State::Answered(_)))
    }

    /// The feature a play request is for: the first whose recordings name
    /// it, or else use its scenario.
    fn play_feature(&self, r: &crate::handoff::queue::Request) -> Option<&Feature> {
        let rec = r.recording()?;
        let scenario = r.args["id"].as_str()?;
        let names = |f: &Feature| f.work.iter().chain(&f.held_out).map(|(n, _)| n.clone()).collect::<Vec<_>>();
        self.features
            .iter()
            .find(|f| names(f).contains(&rec))
            .or_else(|| self.features.iter().find(|f| names(f).iter().any(|n| n.split('/').next() == Some(scenario))))
    }

    /// The feature waits on a play request: what the agent would ask the
    /// person for next is already asked.
    fn waits(&self, f: &Feature) -> bool {
        self.waiting_plays().any(|r| self.play_feature(r).is_some_and(|w| w.id == f.id))
    }

    pub fn next(&self) -> Item {
        // An answer to a play request, read before anything else: the
        // recording is there to use, or the note says why not.
        if let Some(r) =
            self.plays.iter().find(|r| !r.read && matches!(r.state, crate::handoff::queue::State::Answered(_)))
        {
            let crate::handoff::queue::State::Answered(report) = &r.state else { unreachable!() };
            let answer = match report["status"].as_str() {
                Some("declined") => format!("declined: {}", report["note"].as_str().unwrap_or("").trim()),
                Some(s) if report.get("error").is_none() => {
                    let rec =
                        report["recording"]["status"].as_str().map(|r| format!(", recording {r}")).unwrap_or_default();
                    format!("{s}{rec}")
                }
                _ => format!("failed: {}", report["error"]["message"].as_str().unwrap_or("")),
            };
            return Item::agent(
                "Read",
                self.play_feature(r).map(|f| f.id.as_str()),
                if r.args["person"] == true {
                    format!(
                        "the person played on their own (\"{}\"): {answer}",
                        one_line(r.args["request"].as_str().unwrap_or(""))
                    )
                } else {
                    format!(
                        "the person answered the play request \"{}\": {answer}",
                        one_line(r.args["request"].as_str().unwrap_or(""))
                    )
                },
                "the answer is read (scenario play --status returns it and marks it read)",
                &format!(
                    "read the answer: psx scenario play --status {}. When it is a recording, name it under the feature's recordings and go on; when declined, the note says why: ask again in other words or from another start, list the branch under wont_reach with the note as why, or go on without it",
                    r.dir.display()
                ),
            );
        }
        // Debug files the build left on the disc name what the agent would.
        if !self.debug_files.is_empty() {
            return Item::agent(
                "Read",
                None,
                format!("the disc holds debug files its build left: {}", self.debug_files.join(", ")),
                "code symbols on the disc has reported them",
                "run psx code symbols on the disc: it imports those that describe an executable to .psx/names/ (every decompile and view then uses their names) and says why the others were not",
            );
        }
        // The agent's C for a function, passing once, failing since a change
        // to it or the names: the change just made broke it.
        if let Some((f, commit, first)) = self.regressed.first() {
            return Item::agent(
                "Fix",
                None,
                format!(
                    "{f}'s C passed its check{} and now fails: {first}",
                    commit.as_deref().map(|c| format!(" at {}", &c[..c.len().min(10)])).unwrap_or_default()
                ),
                &format!("psx code check {f} passes"),
                &format!(
                    "a change to its C or the names since it passed broke it: compare them with then (git diff), fix the C or names, and run psx code check {f}"
                ),
            );
        }
        // Until the person confirms the goal, the agent only investigates
        // and catalogues; the person is asked once the list has no problems.
        if !self.port.problems.is_empty() {
            let n = self.port.problems.len();
            return Item::agent(
                "Catalogue",
                None,
                format!("the goal or catalogue has {n} problems: {}", problems_line(&self.port)),
                "the goal and catalogue have no problems",
                "fix each problem (psx port status lists them); catalogue features the goal needs as catalogue/F-NNNN.md (catalogue/README.md)",
            );
        }
        if !matches!(self.port.confirmation(), Confirmation::Covers { .. }) {
            let why = match self.port.confirmation() {
                Confirmation::Shrunk { missing, .. } => {
                    format!(
                        "what the person confirmed is no longer in the goal or needed by it: {}",
                        missing.join(", ")
                    )
                }
                _ => "the person has not confirmed the goal's list".into(),
            };
            return Item::person(
                "Confirm",
                None,
                why,
                "goal.toml's confirmation covers its list",
                "the person reads psx port confirm and runs psx port confirm --yes; the agent waits, investigating and cataloguing only",
            );
        }
        let find = |pred: &dyn Fn(&Feature) -> bool| self.features.iter().find(|f| pred(f));
        let at = |f: &Feature, s: Stage| f.stage().is_some_and(|x| x >= s);
        // A feature come to match its recordings since the last review, or
        // finished since: a review of the change while it is fresh, so each
        // reads about one feature's work (most of its code is written once
        // it matches; what Cover adds, the review at its finish reads);
        // findings are answered as they come.
        let since = |listed: &[String], pred: fn(&Feature) -> bool| -> Vec<&str> {
            self.features.iter().filter(|f| pred(f) && !listed.contains(&f.id)).map(|f| f.id.as_str()).collect()
        };
        let finished = since(&self.review_finished, Feature::finished);
        // A feature that matches and finishes at once is said to finish.
        let mut matched = since(&self.review_features, Feature::matching);
        matched.retain(|id| !finished.contains(id));
        // And when much of the code changed with no feature moving: a
        // controller several features share can grow long before any of
        // them matches.
        let limit = self.port.goal.review_lines();
        let grown = match self.review {
            Review::Stale { lines, .. } if lines >= limit => Some(lines),
            _ => None,
        };
        if matches!(self.review, Review::Open { .. } | Review::Invalid(_))
            || ((!matched.is_empty() || !finished.is_empty()) && !self.review.holds())
            || grown.is_some()
        {
            let mut why = Vec::new();
            if !matched.is_empty() {
                let their = if matched.len() == 1 { "its" } else { "their" };
                why.push(format!("{} came to match {their} recordings", matched.join(", ")));
            }
            if !finished.is_empty() {
                why.push(format!("{} finished (branches resolved, no question open)", finished.join(", ")));
            }
            if let Some(n) = grown.filter(|_| why.is_empty()) {
                why.push(format!("{n} lines of the code changed (goal.toml's review_lines: {limit})"));
            }
            return self.review_item(format!("{} since the last review", why.join(", and ")));
        }
        // 0. The assets a feature's recordings need to replay.
        if let Some(item) = self.features.iter().find_map(|f| self.asset_item(Some(&f.id), &f.assets)) {
            return item;
        }
        // 1. The port cannot replay a recording.
        if let Some(f) = find(&|f| at(f, Stage::Recorded) && !f.unbuilt.is_empty()) {
            return Item::agent(
                "Build",
                Some(&f.id),
                format!("the port cannot replay its recordings: {}", first_lines(&f.unbuilt).join("; ")),
                "the last psx port check at HEAD finds a launcher for each of its scenarios' kinds and each quantity they compare observed",
                "launch the scenario kind in the port, building the world from the scenario's parameters with the port's own loading, and observe the compared quantities (the port's psx_scenario::Port); commit and run psx port check",
            );
        }
        // 2. The code: unpacked whole, then each feature's found.
        if !self.unpack.is_empty() {
            return Item::agent(
                "Unpack",
                None,
                format!(
                    "the inventory knows these overlays only from the code that ran in them, so their functions are not all counted: {}",
                    first_lines(&self.unpack).join(", ")
                ),
                "the inventory has no overlay bounded by found_code",
                "find what decompresses them, unpack them and take the inventory again with --unpacked (guide topic code, Decompressed code); record again any recording whose coverage the new inventory does not credit",
            );
        }
        if let Some(f) = find(&|f| f.catalogued && !f.located) {
            return Item::agent(
                "Locate",
                Some(&f.id),
                "its entry names no code".into(),
                "its catalogue entry lists the original's functions that are the feature under code",
                "trace or record runs doing the thing and runs from the same state not doing it, run psx code coverage diff on them, read what only the first ran (code disasm, decompile), and list the feature's functions by entry address under code",
            );
        }
        // Input the original delays: fixed in fixes/, then recorded again.
        // A recording made with fixes/ as it is and still late says the fix
        // does not work; only a change to fixes/ asks for it again, so a fix
        // that does not work is changed, not recorded over and over.
        use crate::scenario::Late;
        let late = |k: Late| move |f: &Feature| f.located && f.late.iter().any(|(x, _)| *x == k);
        let lines = |f: &Feature, k: Late| -> Vec<String> {
            f.late.iter().filter(|(x, _)| *x == k).map(|(_, l)| l.clone()).collect()
        };
        if let Some(f) = find(&late(Late::Fails)) {
            return Item::agent(
                "Fix input",
                Some(&f.id),
                format!("fixes/ does not stop the delay: {}", first_lines(&lines(f, Late::Fails)).join("; ")),
                "its recordings report no late_input",
                "the recording ran with fixes/ as it is and its input still came late on those ticks. A replacement that never ran is not what the game calls there (another function, or another address than names/ gives); one that ran leaves another path that holds the input back. Read those ticks again (run with pad_reads, watch and trace there), change fixes/, and record the scenarios again (guide topic code, the fixes/ paragraphs)",
            );
        }
        if let Some(f) = find(&late(Late::Unfixed)) {
            return Item::agent(
                "Fix input",
                Some(&f.id),
                format!(
                    "the original's own driver delays its input: {}",
                    first_lines(&lines(f, Late::Unfixed)).join("; ")
                ),
                "its recordings report no late_input",
                "the port takes its input every tick, so its recordings must too: find what keeps the poll or the game's read of the buttons back on those ticks (run with pad_reads, watch and trace around them), replace that function in fixes/ so the game takes the tick's input every field (guide topic code, the fixes/ paragraphs), then record the scenarios again; late_input left out with fix on is the fix working",
            );
        }
        if let Some(f) = find(&late(Late::Before)) {
            return Item::agent(
                "Record",
                Some(&f.id),
                format!(
                    "recorded with input late and before fixes/ as it is: {}",
                    first_lines(&lines(f, Late::Before)).join("; ")
                ),
                "its recordings report no late_input",
                "record those scenarios again (psx scenario record): a recording takes fixes/ as it is. late_input left out then is the fix working; still there, port next says the fix does not work",
            );
        }
        if let Some(f) = find(&|f| f.located && !f.unrecorded.is_empty()) {
            return Item::agent(
                "Record",
                Some(&f.id),
                format!("its recordings are not ready: {}", first_lines(&f.unrecorded).join("; ")),
                "it names work recordings, each scenario of its recordings passes psx scenario check, and their coverage counts",
                "declare the quantities in glossary/ and bind them and the tick (the logic step) in glossary/binding/, write scenarios/<id>.toml from a psx states chain whose last run ends at a game moment (until), run psx scenario record, name the recording under recordings.work in the catalogue entry, then psx scenario check (psx guide, Recordings)",
            );
        }
        if let Some(f) = find(&|f| at(f, Stage::Recorded) && !matches!(f.verdict, State::Current(_))) {
            let (why, how) = match &f.verdict {
                State::Stale { commit } if self.git.head.as_deref() == Some(commit.as_str()) => (
                    "its verdict is from HEAD, and the workspace has changed since (uncommitted changes besides notes and the review)".into(),
                    "commit the changes and run psx port check (for a quick look first, cargo test -p <game> --test scenarios)",
                ),
                State::Stale { commit } => (
                    format!(
                        "its verdict is from {commit}, not HEAD with no uncommitted changes (notes and the review aside)"
                    ),
                    "commit the changes and run psx port check",
                ),
                _ => (
                    "no psx port check has judged it".into(),
                    "write the feature from the original's code (citing each rule), launch its scenario kinds and observe their quantities (the port's psx_scenario::Port), commit, and run psx port check",
                ),
            };
            return Item::agent("Implement", Some(&f.id), why, "psx port check has judged it at HEAD", how);
        }
        // 3. A failing recording, the earliest divergence first.
        let failed = |f: &Feature| earliest(&f.work).or_else(|| earliest(&f.held_out).filter(|_| f.reviewed()));
        if let Some((f, (r, first))) = self.features.iter().find_map(|f| Some((f, failed(f)?))) {
            return Item::agent(
                "Fix",
                Some(&f.id),
                format!("{r} fails: {first}"),
                "each of its recordings it is checked against passes at HEAD",
                "find the rule the port gets wrong at that tick, reading the original's code there (psx scenario show gives the recording per tick); fix the port's code, commit, and run psx port check",
            );
        }
        // 3b. A system turned off that the port now has: the original's
        // recordings run with it again.
        if let Some(f) = find(&|f| f.matching() && !f.off.is_empty()) {
            return Item::agent(
                "Restore",
                Some(&f.id),
                format!(
                    "{} matches its recordings, and a scenario still turns off its code: {}",
                    f.id,
                    f.off.join(", ")
                ),
                "no scenario turns off the feature's code",
                "take the function out of the scenario's [off] (scenarios/<id>.toml), record that scenario's recordings again (psx scenario record ... --replace) and check the port against them: they now run with the system the feature ports",
            );
        }
        // 4. An open question.
        if let Some(f) = find(&|f| !f.questions.is_empty()) {
            return Item::agent(
                "Answer",
                Some(&f.id),
                format!("open question: {}", f.questions[0]),
                "its entry has no questions left",
                "answer it from the original's code or a recording, cite the answer where the port's code uses it, and remove the question from the entry",
            );
        }
        // 5. An uncovered branch: a recording that reaches it.
        // Not one waiting on a play request: its answer may cover it.
        let uncovered = |f: &Feature| {
            let g = f.groups().into_iter().find(|g| g.iter().any(|b| b.wont_reach.is_none()))?;
            let b = g.iter().find(|b| b.wont_reach.is_none())?;
            (!self.waits(f)).then(|| ((*b).clone(), g.len()))
        };
        if let Some((f, (b, like))) = self.features.iter().find_map(|f| Some((f, uncovered(f)?))) {
            let n = f.branches.iter().filter(|b| !b.resolved()).count();
            let groups = f.groups().len();
            return Item::agent(
                "Cover",
                Some(&f.id),
                format!(
                    "{} in its recordings{}{}; {n} branches of its code are unresolved, in {groups} groups by what they test",
                    b.describe(),
                    match f.borrowed.len() {
                        0 => String::new(),
                        k => format!(" or the {k} of other features that pass"),
                    },
                    match like {
                        1 => String::new(),
                        k => format!(", as {} other open branches", k - 1),
                    }
                ),
                "every branch of its code is taken both ways by its recordings (or the other features' that pass at HEAD), or listed under wont_reach",
                "read the branch's condition (code disasm; for a script's stretch or decision, your own reading of the program) and work out what reaches the other way in game terms; record that (psx scenario record) and name it under recordings, or when it cannot be scripted ask the person to play it (psx scenario play queues the request; go on with other work meanwhile); when no recording can reach it, list it under wont_reach with why (by its address, or by its group's test, which answers every branch testing it); a script decision no hook named needs a hook in the glossary binding that names its way, then the recordings made again",
            );
        }
        if let Some(f) = find(&|f| f.branches.iter().any(|b| !b.resolved() && b.wont_reach.is_some())) {
            return Item::person(
                "Confirm",
                Some(&f.id),
                format!(
                    "branches listed under wont_reach the person has not confirmed: {}",
                    self.port.unconfirmed_wont_reach().join(", ")
                ),
                "the person confirmed each wont_reach branch as it stands",
                "the person reads psx port confirm (each branch and why no recording can reach it) and runs psx port confirm --yes; the agent may go on with other work",
            );
        }
        // 6. The assets the goal asks for.
        if let Some(item) = self.asset_item(None, &self.needs) {
            return item;
        }
        // 7. The review of the code as a whole, once the rest of the
        // goal's work is finished.
        if self.features.iter().all(Feature::finished) && !self.review.holds() {
            return self.review_item("the goal's features are finished".into());
        }
        // 8. Held-out recordings, played by the person.
        if let Some(f) = find(&|f| f.reviewed() && f.held_out.is_empty() && !self.waits(f)) {
            return Item::person(
                "Play",
                Some(&f.id),
                "the code is reviewed and it has no held-out recordings".into(),
                "its entry names held-out recordings (recordings.held_out)",
                "the agent starts psx scenario play on one of its scenarios with a request to play the feature freely and differently from its work recordings; the person plays it when they can, and the agent names the recording under recordings.held_out when port next gives its Read item",
            );
        }
        if self.complete() {
            return Item::person(
                "Done",
                None,
                "every feature of the goal is done".into(),
                "",
                "the goal is complete; the person may set a new one",
            );
        }
        // Nothing for the agent but what the person was asked to play.
        let plays: Vec<_> = self.waiting_plays().collect();
        if !plays.is_empty() {
            let n = plays.len();
            let asks: Vec<String> = plays.iter().map(|r| one_line(r.args["request"].as_str().unwrap_or(""))).collect();
            return Item::person(
                "Wait",
                plays.iter().find_map(|r| self.play_feature(r)).map(|f| f.id.as_str()),
                format!(
                    "{n} play {} for the person: {}",
                    if n == 1 { "request waits" } else { "requests wait" },
                    first_lines(&asks).join("; ")
                ),
                "the person answers them",
                "the person answers them from psx web or psx play; the agent waits (port next gives a Read item for each answer)",
            );
        }
        let left: Vec<String> = self
            .features
            .iter()
            .filter(|f| f.stage() != Some(Stage::Done))
            .map(|f| format!("{} is {}", f.id, f.stage().map(Stage::name).unwrap_or("not catalogued")))
            .collect();
        Item::person(
            "Stuck",
            None,
            format!("no item fits: {}", first_lines(&left).join("; ")),
            "",
            "psx's port next has no item for this state: tell the person",
        )
    }

    /// The next asset item of what `needs` covers, for `feature` or the
    /// goal: a name that covers no files yet, then an unknown file, an
    /// archive not listed, one not imported, one not checked.
    fn asset_item(&self, feature: Option<&str>, needs: &assets::Needs) -> Option<Item> {
        if needs.settled() {
            return None;
        }
        let a = match &self.assets {
            Ok(a) => a,
            Err(why) => {
                return Some(Item::agent(
                    "Assets",
                    feature,
                    format!("psx cannot list the disc's files: {why}"),
                    "psx port status lists the disc's files",
                    "save the workspace's disc image as its media: psx context --image media/<game>.cue --save",
                ));
            }
        };
        let asks = |name: &str| match feature {
            Some(_) => format!("its entry's assets name {name}"),
            None => format!("the goal's assets name {name}"),
        };
        for (name, why) in &needs.unresolved {
            match why {
                assets::Unresolved::NoFormat(id) => {
                    return Some(Item::agent(
                        "Format",
                        feature,
                        format!("{}, and {}", asks(name), why.describe()),
                        &format!("formats/{id}.md names the disc files in that format"),
                        "find which disc files these are: run the game where it loads one with trace_cd (loads.json gives each file's place in RAM), watch that buffer for the code that parses it, and read that code (code disasm, decompile); write the format doc (formats/README.md) naming the files and the loader",
                    ));
                }
                assets::Unresolved::Archive { path, documented: false } => {
                    return Some(Item::agent(
                        "Format",
                        feature,
                        format!("{}, an entry of {path}, which no format doc says is an archive", asks(name)),
                        &format!("a format doc with archive = true covers {path}"),
                        "find the archive's index from the code that reads it (trace_cd, a watch on the buffer, code disasm), and write its format doc with archive = true (formats/README.md)",
                    ));
                }
                // The Index item below lists it; a name matching nothing is a problem.
                _ => {}
            }
        }
        let stale = match (&a.report, &a.stale) {
            (Some(r), _) if !r.failures.is_empty() => {
                format!(" (the importers failed: {})", first_lines(&r.failures).join("; "))
            }
            (None, Some(c)) => format!(" (the last psx port check judged {c}, not HEAD)"),
            (None, None) => " (no psx port check has run the importers)".into(),
            _ => String::new(),
        };
        let mut unlisted: Vec<&assets::Asset> =
            needs.files.iter().map(|&i| &a.files[i]).filter(|f| f.unlisted).collect();
        for (_, why) in &needs.unresolved {
            if let assets::Unresolved::Archive { path, documented: true } = why {
                unlisted.extend(a.files.iter().find(|f| &f.path == path));
            }
        }
        let files: Vec<&assets::Asset> = needs.files.iter().map(|&i| &a.files[i]).collect();
        let first = |s: AssetState| files.iter().find(|f| f.state == s && !f.unlisted).copied();
        if let Some(f) = first(AssetState::Unknown) {
            let n = files.iter().filter(|f| f.state == AssetState::Unknown).count();
            let read = if f.loaded == Some(false) { "; no traced run loaded it" } else { "" };
            return Some(Item::agent(
                "Format",
                feature,
                format!("{} has no format yet{read}; {n} files are unknown", f.path),
                "every disc file is covered by a format doc, found whole as a standard format, or listed in formats/unused.toml",
                "find what loads it: run with trace_cd (loads.json gives where each file goes in RAM), watch that buffer for the code that parses it, and read that code (code disasm, decompile); write formats/<id>.md (formats/README.md) naming the files and the loader; when no run ever loads it, list it in formats/unused.toml with the runs that show so",
            ));
        }
        if let Some(f) = unlisted.first() {
            return Some(Item::agent(
                "Index",
                feature,
                format!(
                    "{} is an archive ({}) whose entries the importer does not list{stale}",
                    f.path,
                    f.format.as_deref().unwrap_or("")
                ),
                "the importer lists its entries (import::index), at the last psx port check",
                "write import::index for it in tools/import, reading the archive's own index as the importer will (psx_assets::archive reads DOT-T archives and Croc-style DIR indexes); commit and run psx port check: each entry is then an asset of its own (/ARCHIVE:NAME, or @0xOFFSET without names)",
            ));
        }
        if let (Some(c), Some(f)) = (&a.stale, files.iter().find(|f| f.imported_then)) {
            return Some(Item::agent(
                "Import",
                feature,
                format!(
                    "{} was imported at {c}, the last psx port check, not at HEAD with no uncommitted changes",
                    f.path
                ),
                "the importers' outputs name it in their provenance, at HEAD",
                "commit the changes and run psx port check",
            ));
        }
        if let Some(f) = first(AssetState::Found) {
            return Some(Item::agent(
                "Import",
                feature,
                format!("{} ({}) is not imported{stale}", f.path, f.format.as_deref().unwrap_or("")),
                "the importers' outputs name it in their provenance, at HEAD",
                "write its importer in tools/import, converting exactly into the port's formats with Provenance naming the disc file; commit and run psx port check, which runs the importers on the disc",
            ));
        }
        first(AssetState::Imported).map(|f| {
            Item::agent(
                "Check",
                feature,
                format!("{} is imported, not checked against what the game built", f.path),
                "an importer test that compares it with what the game built marks it with import::checking and passes, at HEAD",
                "write a test in tools/import comparing the importer's output with what the game built from that file (RAM after loading it, its VRAM uploads, a GPU capture of the same view: psx run with trace_cd, gpu_frame), starting with let _checked = import::checking(\"/PATH\"); commit and run psx port check",
            )
        })
    }

    /// The features matching their recordings now: what a review lists.
    fn matching_ids(&self) -> Vec<String> {
        self.features.iter().filter(|f| f.matching()).map(|f| f.id.clone()).collect()
    }

    /// The features finished now: what a review lists as finished.
    fn finished_ids(&self) -> Vec<String> {
        self.features.iter().filter(|f| f.finished()).map(|f| f.id.clone()).collect()
    }

    /// The review item due, `why` it is due when the review is missing or
    /// stale.
    fn review_item(&self, why: String) -> Item {
        let done = "the review holds: the code (game/, tools/) unchanged since its commit, every finding answered, its features and finished the ones matching and finished";
        match &self.review {
            Review::Open { unanswered } => Item::agent(
                "Review",
                None,
                format!("findings without an answer: {}", first_lines(unanswered).join("; ")),
                done,
                &format!(
                    "answer each finding in {} with an **Answer:** line: done (restructure as it asks, keeping what the code does: psx port check must pass again), or kept and why; changed code has a follow-up review when the next feature matches or finishes, or once {} lines of it change",
                    review::REVIEW,
                    self.port.goal.review_lines()
                ),
            ),
            Review::Invalid(why) => Item::agent(
                "Review",
                None,
                format!("{} is not a review: {why}", review::REVIEW),
                done,
                "write the review's front matter as the brief says: +++, commit, features, finished, +++",
            ),
            Review::Stale { commit, changed, .. } => Item::agent(
                "Review",
                None,
                format!("{why}, and the code changed since the review: {}", first_lines(changed).join(", ")),
                done,
                &self.follow_up(commit),
            ),
            _ => Item::agent("Review", None, format!("{why}, and the code has no review"), done, &self.brief()),
        }
    }

    /// The Review item's brief, for a reviewer who has not seen the work.
    fn brief(&self) -> String {
        format!(
            "Give this brief to a reviewer who has not seen the work (a sub-agent or a new session): review the port's code as a whole, {code} (git ls-files {code}). Its recordings already check that it does what the original does, so this review does not: leave the catalogue, the notes and the original's code unread. Ask instead: would you write a new game like this? Judge it as code written for a new game today, not as a port: how it is divided (modules, plugins, systems, components, data), names, how readily it takes more (another character, level or rule), duplication, and what is shaped by the original rather than chosen. AGENTS.md's \"The code\" says what the tools rely on. Write {review}: front matter between +++ lines with commit = \"{head}\", features = {features} and finished = {finished}, then one ## heading per finding: what you would change, the shape you would choose instead and why; what the code does stays as it is. The writer then answers each finding with an **Answer:** line. Then file a report for the person alone with report (psx report --text ...): how the port looks to you as a game in the making, what worries you about it and anything you would not put to the writer; the writer does not see it.",
            code = review::CODE.join(" "),
            review = review::REVIEW,
            head = self.git.head.as_deref().unwrap_or("HEAD"),
            features = toml::Value::from(self.matching_ids()),
            finished = toml::Value::from(self.finished_ids()),
        )
    }

    /// The brief for a review of what changed since the review at `commit`:
    /// what that review read is not read again.
    fn follow_up(&self, commit: &str) -> String {
        format!(
            "Give this brief to a reviewer who did not write the changes (a sub-agent or a new session): a follow-up review of the port's code. The review at {commit} read the code then; read what changed since: git diff {commit} -- {code}, with the findings and answers in {review} (are the changes what the answers say?), and enough of the code around it to judge how the change fits the whole (how the code is divided, what a new feature reuses or duplicates). Ask what the review asked: would you write a new game like this? If the changes rewrite most of the code, review it whole. Leave the catalogue, the notes and the original's code unread: the recordings check what the code does. Keep the findings and answers already in {review}; set its commit to \"{head}\", its features to {features} and its finished to {finished}, and add one ## heading per new finding: what you would change, the shape you would choose instead and why; with none, the commit is all that changes. The writer then answers each new finding with an **Answer:** line. Then file a report for the person alone with report (psx report --text ...): how the port looks to you as a game in the making, what worries you about it and anything you would not put to the writer; the writer does not see it.",
            code = review::CODE.join(" "),
            review = review::REVIEW,
            head = self.git.head.as_deref().unwrap_or("HEAD"),
            features = toml::Value::from(self.matching_ids()),
            finished = toml::Value::from(self.finished_ids()),
        )
    }

    fn review_json(&self) -> Value {
        match &self.review {
            Review::Missing => json!({"state": "missing"}),
            Review::Invalid(why) => json!({"state": "invalid", "why": why}),
            Review::Stale { commit, changed, lines } => {
                json!({"state": "stale", "commit": commit, "changed": changed, "lines": lines, "review_lines": self.port.goal.review_lines()})
            }
            Review::Open { unanswered } => json!({"state": "open", "unanswered": unanswered}),
            Review::Holds { commit, findings } => json!({"state": "holds", "commit": commit, "findings": findings}),
        }
    }

    /// `port status`.
    pub fn status(&self) -> Value {
        let mut areas: BTreeMap<String, Vec<Value>> = BTreeMap::new();
        let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
        for f in &self.features {
            let e = self.port.entries.get(&f.id);
            let stage = f.stage().map(Stage::name).unwrap_or("not catalogued");
            *counts.entry(stage).or_default() += 1;
            let mut row = json!({
                "id": f.id, "title": e.map(|e| e.title.as_str()), "stage": stage, "in_goal": f.in_goal,
                "depends": e.map(|e| &e.depends), "code": e.map(|e| &e.code), "verdict": f.verdict.name(),
                "work": f.work_json(), "held_out": f.held_out_json(), "branches": f.branches_json(false),
                "questions": f.questions,
            });
            if !f.unrecorded.is_empty() && f.located {
                row["unrecorded"] = json!(f.unrecorded);
            }
            if !f.late.is_empty() {
                row["late_input"] = json!(f.late.iter().map(|(_, l)| l).collect::<Vec<_>>());
            }
            if !f.unbuilt.is_empty() {
                row["unbuilt"] = json!(f.unbuilt);
            }
            if !f.off.is_empty() {
                row["off"] = json!(f.off);
            }
            if !f.assets.is_empty() {
                row["assets"] = f.assets.json();
            }
            areas.entry(e.map(|e| e.area.clone()).unwrap_or_default()).or_default().push(row);
        }
        let mut v = json!({
            "description": self.port.goal.description,
            "features": self.port.goal.features.to_json(),
            "confirmation": self.port.confirmation_json(),
            "problems": self.port.problems,
            "stages": counts,
            "areas": areas,
            "review": self.review_json(),
            "complete": self.complete(),
            "git": {"head": self.git.head, "clean": self.git.clean},
            "decomp": self.decomp,
        });
        v["assets"] = match &self.assets {
            Ok(a) => {
                let mut j = a.json(self.port.formats.len());
                j["in_goal"] = self.port.goal.assets.to_json();
                let (needed, settled) = self.needed();
                j["needed"] = json!({"files": needed.len(), "settled": settled});
                if !self.needs.unresolved.is_empty() {
                    j["goal"] = self.needs.json();
                }
                j
            }
            Err(why) => json!({"in_goal": self.port.goal.assets.to_json(), "error": why}),
        };
        if !self.unpack.is_empty() {
            v["unpack"] = json!(self.unpack);
        }
        // The play requests still open, and answers the agent has not read.
        let open: Vec<Value> = self
            .plays
            .iter()
            .filter(|r| !r.read || !matches!(r.state, crate::handoff::queue::State::Answered(_)))
            .map(|r| {
                let mut j = r.json();
                if let Some(f) = self.play_feature(r) {
                    j["feature"] = json!(f.id);
                }
                j
            })
            .collect();
        if !open.is_empty() {
            v["plays"] = json!(open);
        }
        v
    }

    /// `port status --plain`: for the person.
    pub fn plain(&self) -> String {
        let mut s = String::new();
        let goal = &self.port.goal.description;
        let done = self.features.iter().filter(|f| f.stage() == Some(Stage::Done)).count();
        s.push_str(&format!(
            "goal: {}    {done}/{} features done\n",
            if goal.is_empty() { "(no description)" } else { goal },
            self.features.len()
        ));
        if let Ok(a) = &self.assets {
            let (needed, settled) = self.needed();
            let all = format!(
                "{}/{} files checked · {} unused · {} formats · {} unknown",
                a.count(AssetState::Checked),
                a.files.len(),
                a.count(AssetState::Unused),
                self.port.formats.len(),
                a.count(AssetState::Unknown),
            );
            s.push_str(&if self.port.goal.assets.is_all() {
                format!("assets  {all}\n")
            } else if needed.is_empty() {
                format!("assets  {all}  (not in the goal)\n")
            } else {
                format!("assets  {settled}/{} needed files checked or unused · disc: {all}\n", needed.len())
            });
        }
        s.push_str(&match self.port.confirmation() {
            Confirmation::Missing => "not confirmed yet\n".to_string(),
            Confirmation::Covers { at } => format!("confirmed {at}\n"),
            Confirmation::Shrunk { at, missing } => {
                format!("confirmed {at}, but {} is no longer in the goal or needed by it\n", missing.join(", "))
            }
        });
        s.push_str(&match &self.review {
            Review::Holds { commit, findings } => {
                format!("code reviewed at {commit} ({findings} findings, answered)\n")
            }
            Review::Stale { commit, changed, lines } => {
                format!(
                    "code reviewed at {commit}, changed since in {} files ({lines} of {} lines committed before a follow-up)\n",
                    changed.len(),
                    self.port.goal.review_lines()
                )
            }
            r => format!("code review: {}\n", r.name()),
        });
        let width = self.features.iter().map(|f| self.title(&f.id).chars().count()).max().unwrap_or(0).min(32);
        // By area, in the order areas first come in the closure; dependency
        // order within each.
        let area_of = |f: &Feature| self.port.entries.get(&f.id).map(|e| e.area.as_str()).unwrap_or("");
        let mut areas: Vec<&str> = Vec::new();
        for f in &self.features {
            if !areas.contains(&area_of(f)) {
                areas.push(area_of(f));
            }
        }
        let mut features: Vec<&Feature> = self.features.iter().collect();
        features.sort_by_key(|f| areas.iter().position(|a| *a == area_of(f)));
        let mut area = None;
        for f in features {
            let e = self.port.entries.get(&f.id);
            let a = e.map(|e| e.area.as_str()).unwrap_or("");
            if area != Some(a) {
                s.push_str(&format!("\n{}\n", if a.is_empty() { "(no area)" } else { a }));
                area = Some(a);
            }
            let title: String = self.title(&f.id).chars().take(32).collect();
            let title = if title.is_empty() { format!("{} (not catalogued)", f.id) } else { title };
            let stage = f.stage().map(Stage::name).unwrap_or("-");
            let mark = if f.in_goal { "" } else { "  (needed by the goal)" };
            s.push_str(&format!("  {title:<width$}  {stage:<10} {}{mark}\n", f.summary()));
            let indent = " ".repeat(width + 4);
            if let Some(b) = f.groups().first().map(|g| g[0]) {
                s.push_str(&format!("{indent}uncovered: {}\n", b.describe()));
            }
            for q in f.questions.iter().take(2) {
                s.push_str(&format!("{indent}open: {q}\n"));
            }
        }
        if !self.port.problems.is_empty() {
            s.push_str(&format!("\n{} problems in the goal or catalogue.\n", self.port.problems.len()));
        }
        let item = self.next();
        s.push_str(&format!("\nNext: {}", item.item));
        if let Some(f) = &item.feature {
            s.push_str(&format!(" {f}"));
        }
        s.push_str(&format!(" ({}): {}\n", item.who, item.why));
        s
    }

    /// What the loop guard compares: the item's facts and the work in the
    /// workspace.
    fn fingerprint(&self, item: &Item) -> String {
        let text = format!(
            "{}\n{}\n{:?}\n{}\n{}",
            serde_json::to_string(&item.to_json()).unwrap_or_default(),
            serde_json::to_string(&self.status()).unwrap_or_default(),
            self.git.head,
            self.git.changes,
            self.port.problems.len(),
        );
        format!("{:x}", Sha256::digest(text))
    }
}

fn problems_line(port: &Port) -> String {
    let mut l: Vec<String> = port.problems.iter().take(3).map(|p| format!("{}: {}", p.file, p.problem)).collect();
    if port.problems.len() > 3 {
        l.push(format!("and {} more", port.problems.len() - 3));
    }
    l.join("; ")
}

/// One thing to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Item {
    pub item: &'static str,
    /// agent or person.
    pub who: &'static str,
    pub feature: Option<String>,
    /// The fact that makes it due.
    pub why: String,
    /// The fact that will show it done.
    pub done_when: String,
    pub how: String,
}

impl Item {
    fn agent(item: &'static str, feature: Option<&str>, why: String, done_when: &str, how: &str) -> Item {
        Item {
            item,
            who: "agent",
            feature: feature.map(str::to_string),
            why,
            done_when: done_when.into(),
            how: how.into(),
        }
    }

    fn person(item: &'static str, feature: Option<&str>, why: String, done_when: &str, how: &str) -> Item {
        Item { who: "person", ..Item::agent(item, feature, why, done_when, how) }
    }

    fn to_json(&self) -> Value {
        json!({"item": self.item, "who": self.who, "feature": self.feature, "why": self.why,
               "done_when": self.done_when, "how": self.how})
    }
}

/// How many calls in a row may hand out the same agent item with nothing
/// changed before the loop guard reports it instead.
const GUARD: u32 = 3;

/// `.psx/port/next.json`: the last item handed out, its fingerprint and
/// how many calls in a row gave it unchanged.
#[derive(Debug, Default, serde::Serialize, serde::Deserialize)]
struct Guard {
    item: String,
    fingerprint: String,
    count: u32,
}

/// Where the port stands, a line each time it changes, for `psx web`'s
/// progress over time.
pub const PROGRESS: &str = ".psx/progress.jsonl";

/// Append the port's standing (from its status, `next` set) to
/// [`PROGRESS`] when it differs from the last line. Never fails the command.
fn progress(root: &Path, status: &Value) {
    let line = standing(status);
    let path = root.join(PROGRESS);
    let last = std::fs::read_to_string(&path)
        .ok()
        .and_then(|t| t.lines().last().and_then(|l| serde_json::from_str::<Value>(l).ok()));
    if last.is_some_and(|mut l| {
        l.as_object_mut().map(|o| o.remove("t"));
        l == line
    }) {
        return;
    }
    let mut line = line;
    line["t"] = json!(crate::now_ms() as u64);
    let _ = std::fs::create_dir_all(root.join(".psx"));
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&path) {
        use std::io::Write;
        let _ = f.write_all(format!("{line}\n").as_bytes());
    }
}

/// The counts the progress line keeps: the goal's features by stage and
/// verdict, their branches and work recordings, problems, questions, the
/// review and the next item.
fn standing(status: &Value) -> Value {
    let mut verdicts: BTreeMap<String, u64> = BTreeMap::new();
    let (mut resolved, mut branches, mut passing, mut recordings, mut questions) = (0, 0, 0, 0, 0);
    let mut features = 0;
    for f in status["areas"]
        .as_object()
        .into_iter()
        .flat_map(|a| a.values())
        .flat_map(|v| v.as_array().into_iter().flatten())
    {
        if f["in_goal"] != json!(true) {
            continue;
        }
        features += 1;
        *verdicts.entry(f["verdict"].as_str().unwrap_or("none").to_string()).or_default() += 1;
        resolved += f["branches"]["resolved"].as_u64().unwrap_or(0);
        branches += f["branches"]["total"].as_u64().unwrap_or(0);
        passing += f["work"]["pass"].as_u64().unwrap_or(0);
        recordings += f["work"]["total"].as_u64().unwrap_or(0);
        questions += f["questions"].as_array().map_or(0, Vec::len);
    }
    let next = &status["next"];
    json!({
        "head": status["git"]["head"],
        "features": features,
        "stages": status["stages"],
        "verdicts": verdicts,
        "branches": {"resolved": resolved, "total": branches},
        "recordings": {"pass": passing, "total": recordings},
        "problems": status["problems"].as_array().map_or(0, Vec::len),
        "questions": questions,
        "review": status["review"]["state"],
        "decomp": status["decomp"],
        "next": {"item": next["item"], "feature": next["feature"], "who": next["who"], "times": next["times"]},
    })
}

/// The port's status with its next item as `port status` gives them, and
/// the loop guard as the last `port next` left it, for `psx web`: nothing is
/// written.
pub fn look(root: &Path) -> crate::Result<Value> {
    let f = Facts::gather(root)?;
    let mut v = f.status();
    v["next"] = f.next().to_json();
    let g: Option<Guard> =
        std::fs::read(root.join(".psx/port/next.json")).ok().and_then(|b| serde_json::from_slice(&b).ok());
    if let Some(g) = g {
        v["guard"] = json!({"item": g.item, "count": g.count, "limit": GUARD});
    }
    Ok(v)
}

fn guarded(root: &Path, facts: &Facts, item: Item) -> crate::Result<Value> {
    let path = root.join(".psx/port/next.json");
    let key = format!("{} {}", item.item, item.feature.as_deref().unwrap_or(""));
    let fingerprint = facts.fingerprint(&item);
    let old: Guard = std::fs::read(&path).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
    let count = if old.item == key && old.fingerprint == fingerprint { old.count + 1 } else { 1 };
    if let Some(d) = path.parent() {
        std::fs::create_dir_all(d).map_err(|e| Error::io(d, e))?;
    }
    let guard = Guard { item: key, fingerprint, count };
    crate::write_atomic(&path, &serde_json::to_vec_pretty(&guard).unwrap_or_default())
        .map_err(|e| Error::io(&path, e))?;
    let mut out = item.to_json();
    if item.who == "agent" && count >= GUARD {
        out = json!({
            "item": "Stuck", "who": "person", "stuck": out, "times": count,
            "why": format!("port next gave this item {count} times with nothing changed: stop and ask the person what is wrong"),
        });
    }
    Ok(out)
}

#[derive(clap::Args, Clone, Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct PortStatusArgs {
    /// One feature's unresolved branches, all of them.
    #[arg(long)]
    #[serde(default)]
    pub feature: Option<String>,
    /// The disc files an asset name covers (/MAPS/*.WLD, /ARCHIVE:ENTRY,
    /// format:<id>), each with its format and state.
    #[arg(long)]
    #[serde(default)]
    pub assets: Option<String>,
    /// Text for the person instead of JSON.
    #[arg(long)]
    #[serde(default)]
    pub plain: bool,
}

impl Op for PortStatusArgs {
    fn exec(self, ctx: &Ctx) -> Outcome {
        let plain = self.plain && self.feature.is_none() && self.assets.is_none();
        Outcome::result_text(
            Facts::gather(&ctx.root).and_then(|f| {
                if let Some(id) = &self.feature {
                    let feature = f.features.iter().find(|x| &x.id == id);
                    let feature =
                        feature.ok_or_else(|| crate::Error::invalid(format!("feature {id}: not in the catalogue")))?;
                    return Ok(json!({"feature": id, "branches": feature.branches_json(true)}));
                }
                if let Some(name) = &self.assets {
                    return f.assets_named(name);
                }
                let mut v = f.status();
                v["next"] = f.next().to_json();
                progress(&ctx.root, &v);
                Ok(if self.plain { json!({"text": f.plain()}) } else { v })
            }),
            plain,
        )
    }
}

#[derive(clap::Args, Clone, Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct PortNextArgs {}

impl Op for PortNextArgs {
    fn exec(self, ctx: &Ctx) -> Outcome {
        Outcome::result(Facts::gather(&ctx.root).and_then(|f| {
            let next = guarded(&ctx.root, &f, f.next())?;
            let mut v = f.status();
            v["next"] = next.clone();
            progress(&ctx.root, &v);
            Ok(next)
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::super::tests::{feature, workspace};
    use super::*;

    fn facts(goal: &str, ids: &[&str]) -> (super::super::tests::tempdir::Dir, Facts) {
        let entries: Vec<(String, String)> = ids.iter().map(|id| (format!("{id}.md"), feature(id, ""))).collect();
        let entries: Vec<(&str, &str)> = entries.iter().map(|(a, b)| (a.as_str(), b.as_str())).collect();
        let (dir, _) = workspace(goal, &entries);
        let f = Facts::gather(&dir.0).unwrap();
        (dir, f)
    }

    const CONFIRMED: &str = "description = \"x\"\nfeatures = [\"F-0001\", \"F-0002\"]\nconfirmed = { at = \"t\", features = [\"F-0001\", \"F-0002\"] }\n";

    #[test]
    fn debug_files_on_the_disc_are_read_first_until_code_symbols_reports_them() {
        let (d, _) = facts(CONFIRMED, &["F-0001", "F-0002"]);
        let scan = json!({"debug_files": [{"at": "/GAME.SYM", "kind": "sym"}], "files_by_name": [
            {"path": "/GAME.SYM", "kind": "sym"}, {"path": "/README.C", "kind": "source"}]});
        crate::disc_names::record(&d.0, &scan, false).unwrap();
        let f = Facts::gather(&d.0).unwrap();
        let item = f.next();
        assert_eq!((item.item, item.who), ("Read", "agent"));
        assert!(item.why.ends_with("/GAME.SYM (sym), /README.C (source)"), "{}", item.why);
        crate::disc_names::record(&d.0, &scan, true).unwrap();
        assert_ne!(Facts::gather(&d.0).unwrap().next().item, "Read");
    }

    #[test]
    fn a_function_whose_c_passed_and_now_fails_is_fixed() {
        let (d, _) = facts(CONFIRMED, &["F-0001", "F-0002"]);
        let checks = d.0.join(crate::decomp::CHECKS);
        std::fs::create_dir_all(&checks).unwrap();
        std::fs::create_dir_all(d.0.join("decomp")).unwrap();
        std::fs::write(d.0.join("decomp/step.c"), "void step(void)\n{\n}\n").unwrap();
        let last = |status: &str| {
            std::fs::write(
                checks.join("step.json"),
                json!({"function": "step", "status": status,
                "first_failure": {"differences": ["v0: original 0x1, yours 0x2"]}})
                .to_string(),
            )
            .unwrap();
        };
        // Failing, never passed: not a regression.
        last("failed");
        std::fs::write(checks.join("checks.jsonl"), "{\"function\":\"step\",\"status\":\"failed\"}\n").unwrap();
        assert!(Facts::gather(&d.0).unwrap().regressed.is_empty());
        std::fs::write(checks.join("checks.jsonl"),
            "{\"function\":\"step\",\"status\":\"passed\",\"commit\":\"abcdef0123456\"}\n{\"function\":\"step\",\"status\":\"failed\"}\n").unwrap();
        let f = Facts::gather(&d.0).unwrap();
        let item = f.next();
        assert_eq!((item.item, item.who, item.feature.as_deref()), ("Fix", "agent", None));
        assert_eq!(item.why, "step's C passed its check at abcdef0123 and now fails: v0: original 0x1, yours 0x2");
        last("passed");
        assert!(Facts::gather(&d.0).unwrap().regressed.is_empty());
    }

    #[test]
    fn the_catalogue_comes_before_the_person_confirms() {
        let (_d, f) = facts("description = \"x\"\nfeatures = [\"F-0001\", \"F-0002\"]\n", &["F-0001"]);
        assert_eq!(f.next().item, "Catalogue");
        let (_d, f) = facts("description = \"x\"\nfeatures = [\"F-0001\", \"F-0002\"]\n", &["F-0001", "F-0002"]);
        let item = f.next();
        assert_eq!((item.item, item.who), ("Confirm", "person"));
        let (_d, f) = facts(
            &CONFIRMED.replace("features = [\"F-0001\", \"F-0002\"]\nc", "features = [\"F-0002\"]\nc"),
            &["F-0001", "F-0002"],
        );
        assert_eq!(f.next().item, "Confirm");
        assert!(f.next().why.contains("F-0001"));
    }

    fn set(f: &mut Facts, id: &str, change: impl FnOnce(&mut Feature)) {
        change(f.features.iter_mut().find(|x| x.id == id).unwrap());
    }

    fn review(f: &mut Facts, r: Review) {
        for x in f.features.iter_mut() {
            x.code_reviewed = r.holds();
        }
        f.review = r;
    }

    fn current() -> State {
        State::Current(verdict::Verdict {
            feature: String::new(),
            commit: "abc".into(),
            clean: true,
            at: "t".into(),
            failures: vec![],
            recordings: BTreeMap::new(),
        })
    }

    fn results(rs: &[(&str, &[&str])]) -> Vec<(String, Option<Vec<String>>)> {
        rs.iter().map(|(n, f)| (n.to_string(), Some(f.iter().map(|l| l.to_string()).collect()))).collect()
    }

    #[test]
    fn a_system_turned_off_comes_back_once_its_feature_matches() {
        let (_d, mut f) = facts(CONFIRMED, &["F-0001", "F-0002"]);
        for id in ["F-0001", "F-0002"] {
            set(&mut f, id, |x| {
                x.located = true;
                x.unrecorded.clear();
                x.verdict = current();
                x.work = results(&[("walk/a", &[])]);
                x.held_out = results(&[("walk/h", &[])]);
                x.code_reviewed = true;
            });
        }
        f.review_features = vec!["F-0001".into(), "F-0002".into()];
        f.review_finished = vec!["F-0001".into(), "F-0002".into()];
        assert_eq!(f.stage("F-0002"), Some(Stage::Done));
        set(&mut f, "F-0002", |x| x.off = vec!["walk: enemy_update".into()]);
        // Not done while the original's recordings run without its code.
        assert_eq!(f.stage("F-0002"), Some(Stage::Reviewed));
        let item = f.next();
        assert_eq!((item.item, item.feature.as_deref()), ("Restore", Some("F-0002")));
        assert!(item.why.contains("walk: enemy_update"), "{}", item.why);
        // Before it matches, the feature's own work comes first.
        set(&mut f, "F-0002", |x| x.work = results(&[("walk/a", &["tick 2 x: expected 1, got 2"])]));
        assert_eq!(f.next().item, "Fix");
    }

    #[test]
    fn input_the_original_delays_is_fixed_before_the_port_is_written() {
        use crate::scenario::Late;
        let (_d, mut f) = facts(CONFIRMED, &["F-0001", "F-0002"]);
        for id in ["F-0001", "F-0002"] {
            set(&mut f, id, |x| {
                x.located = true;
                x.unrecorded.clear();
            });
        }
        let line = "scenarios/walk/a.jsonl: the game ran 3 ticks on input a tick old (ticks 4, 9, 12)";
        let at = |f: &mut Facts, k: Late| set(f, "F-0001", |x| x.late = vec![(k, line.to_string())]);
        at(&mut f, Late::Unfixed);
        assert_eq!(f.stage("F-0001"), Some(Stage::Located));
        let item = f.next();
        assert_eq!((item.item, item.feature.as_deref()), ("Fix input", Some("F-0001")));
        assert!(
            item.why.contains("ticks 4, 9, 12") && item.how.contains("replace that function in fixes/"),
            "{item:?}"
        );
        // A fix written: recorded again, the recording takes it.
        at(&mut f, Late::Before);
        let item = f.next();
        assert_eq!((item.item, item.feature.as_deref()), ("Record", Some("F-0001")));
        assert!(item.how.contains("record those scenarios again"), "{item:?}");
        // Recorded with it and still late: the fix is changed, not recorded again.
        at(&mut f, Late::Fails);
        let item = f.next();
        assert_eq!(item.item, "Fix input");
        assert!(
            item.why.starts_with("fixes/ does not stop the delay") && item.how.contains("change fixes/"),
            "{item:?}"
        );
        // One recording still to take the fix waits on the fix that fails.
        set(&mut f, "F-0001", |x| x.late.push((Late::Before, line.to_string())));
        assert!(f.next().why.starts_with("fixes/ does not stop the delay"));
        set(&mut f, "F-0001", |x| x.late.clear());
        assert!(!["Fix input", "Record"].contains(&f.next().item));
    }

    #[test]
    fn uncommitted_changes_leave_a_verdict_at_head_stale() {
        let (dir, _) = workspace(CONFIRMED, &[("F-0001.md", &feature("F-0001", ""))]);
        let git = |args: &[&str]| {
            let ok = std::process::Command::new("git")
                .args(["-c", "user.name=t", "-c", "user.email=t@t", "-c", "commit.gpgsign=false"])
                .args(args)
                .current_dir(&dir.0)
                .output()
                .is_ok_and(|o| o.status.success());
            assert!(ok, "git {args:?}");
        };
        std::fs::write(dir.0.join(".gitignore"), ".psx/\n").unwrap();
        git(&["init", "-q"]);
        git(&["add", "-A"]);
        git(&["commit", "-qm", "c"]);
        let head = Git::read(&dir.0).head.unwrap();
        let v = verdict::Verdict {
            feature: "F-0001".into(),
            commit: head.clone(),
            clean: true,
            at: "t".into(),
            failures: vec![],
            recordings: Default::default(),
        };
        verdict::write(&dir.0, &v).unwrap();
        let state = || Facts::gather(&dir.0).unwrap().features[0].verdict.clone();
        assert!(matches!(state(), State::Current(_)), "{:?}", state());
        // Notes change nothing a check reads.
        std::fs::create_dir_all(dir.0.join("notes")).unwrap();
        std::fs::write(dir.0.join("notes/a.md"), "x").unwrap();
        assert!(matches!(state(), State::Current(_)));
        // A recording made again does.
        std::fs::create_dir_all(dir.0.join("scenarios/walk")).unwrap();
        std::fs::write(dir.0.join("scenarios/walk/a.jsonl"), "{}").unwrap();
        assert_eq!(state(), State::Stale { commit: head });
    }

    #[test]
    fn the_feature_whose_code_a_scenario_turns_off_owns_it() {
        let entry =
            feature("F-0001", "").replace("area = \"driving\"\n", "area = \"driving\"\ncode = [\"0x80031F64\"]\n");
        let (dir, _) = workspace(CONFIRMED, &[("F-0001.md", &entry), ("F-0002.md", &feature("F-0002", ""))]);
        std::fs::create_dir_all(dir.0.join("scenarios")).unwrap();
        std::fs::write(
            dir.0.join("scenarios/walk.toml"),
            "moment = \"m\"\nchain = \"c\"\nkind = \"level\"\n[compare]\nx = 0\n[off]\n\"0x80031F64\" = 0\n\"0x80040000\" = 1\n",
        )
        .unwrap();
        let f = Facts::gather(&dir.0).unwrap();
        let off = |id: &str| f.features.iter().find(|x| x.id == id).unwrap().off.clone();
        assert_eq!(off("F-0001"), ["walk: 0x80031F64"]);
        assert!(off("F-0002").is_empty());
        let status = f.status().to_string();
        assert!(status.contains(r#""off":["walk: 0x80031F64"]"#), "{status}");
    }

    #[test]
    fn items_follow_the_stages() {
        let (_d, mut f) = facts(CONFIRMED, &["F-0001", "F-0002"]);
        let next = |f: &Facts| {
            let i = f.next();
            (i.item, i.who, i.feature.unwrap_or_default())
        };
        assert_eq!(next(&f), ("Locate", "agent", "F-0001".into()));
        for id in ["F-0001", "F-0002"] {
            set(&mut f, id, |x| x.located = true);
        }
        assert_eq!(next(&f), ("Record", "agent", "F-0001".into()));
        assert!(f.next().why.contains("names no work recordings"));
        for id in ["F-0001", "F-0002"] {
            set(&mut f, id, |x| {
                x.unrecorded.clear();
                x.work = vec![("walk/a".into(), None)];
            });
        }
        assert_eq!(f.stage("F-0001"), Some(Stage::Recorded));
        assert_eq!(next(&f), ("Implement", "agent", "F-0001".into()));
        set(&mut f, "F-0001", |x| {
            x.verdict = current();
            x.work = results(&[("walk/a", &[])]);
        });
        set(&mut f, "F-0002", |x| {
            x.verdict = current();
            x.work =
                results(&[("walk/b", &["tick 9 x: expected 1, got 2"]), ("walk/c", &["tick 4 y: expected 3, got 2"])]);
            // Not reviewed: its held-out results neither fail it nor show.
            x.held_out = results(&[("walk/h", &["tick 1 x: expected 0, got 1"])]);
        });
        assert_eq!(f.stage("F-0001"), Some(Stage::Matching));
        // F-0001 matches: a review of it comes before more work.
        let item = f.next();
        assert_eq!(item.item, "Review");
        assert_eq!(
            item.why,
            "F-0001 finished (branches resolved, no question open) since the last review, and the code has no review"
        );
        assert!(item.how.contains("features = [\"F-0001\"]"), "{}", item.how);
        // Once a review lists them, the review waits for the next feature to match or finish.
        f.review_features = vec!["F-0001".into(), "F-0002".into()];
        f.review_finished = vec!["F-0001".into()];
        let item = f.next();
        assert_eq!((item.item, item.feature.as_deref()), ("Fix", Some("F-0002")));
        assert_eq!(item.why, "walk/c fails: tick 4 y: expected 3, got 2");
        assert_eq!(f.features[1].held_out_json()["shown"], "once the code is reviewed");
        set(&mut f, "F-0002", |x| x.work = results(&[("walk/b", &[])]));
        f.review_finished = vec!["F-0001".into(), "F-0002".into()];
        set(&mut f, "F-0001", |x| x.unbuilt = vec!["walk: no launcher for scenario kind level".into()]);
        assert_eq!(next(&f), ("Build", "agent", "F-0001".into()));
        set(&mut f, "F-0001", |x| x.unbuilt.clear());
        set(&mut f, "F-0002", |x| x.questions = vec!["does it snap while rising?".into()]);
        assert_eq!(next(&f), ("Answer", "agent", "F-0002".into()));
        set(&mut f, "F-0002", |x| x.questions.clear());
        let branch = |wont_reach| Branch::code(0x8002_D820, 0x8002_D810, 1, wont_reach);
        set(&mut f, "F-0001", |x| x.branches = vec![branch(None); 7]);
        let short = f.features[0].branches_json(false);
        assert_eq!(
            (short["unresolved"].as_array().unwrap().len(), &short["more"]),
            (5, &json!("2 more groups: port_status feature F-0001"))
        );
        assert_eq!(f.features[0].branches_json(true)["unresolved"].as_array().unwrap().len(), 7);
        set(&mut f, "F-0001", |x| x.branches = vec![branch(None)]);
        assert_eq!(next(&f), ("Cover", "agent", "F-0001".into()));
        assert!(f.next().why.starts_with("the branch at 0x8002D820 (in 0x8002D810) never fell through"));
        // Asked of the person: the feature waits on the answer, then reads it.
        let play = |state, read| crate::handoff::queue::Request {
            dir: PathBuf::from("/q/1"),
            args: json!({"request": "walk off the ledge", "id": "walk", "name": "ledge"}),
            queued_unix_ms: 1,
            after: None,
            state,
            read,
        };
        f.plays = vec![play(crate::handoff::queue::State::Queued, false)];
        assert_eq!(next(&f), ("Wait", "person", "F-0001".into()));
        assert!(
            f.next().why.starts_with("1 play request waits for the person: walk off the ledge"),
            "{}",
            f.next().why
        );
        let declined = json!({"status": "declined", "note": "there is no ledge there"});
        f.plays = vec![play(crate::handoff::queue::State::Answered(declined.clone()), false)];
        assert_eq!(next(&f), ("Read", "agent", "F-0001".into()));
        assert!(f.next().why.ends_with("declined: there is no ledge there"), "{}", f.next().why);
        f.plays = vec![play(crate::handoff::queue::State::Answered(declined), true)];
        assert_eq!(next(&f), ("Cover", "agent", "F-0001".into()));
        f.plays.clear();
        set(&mut f, "F-0001", |x| x.branches = vec![branch(Some(false))]);
        assert_eq!(next(&f), ("Confirm", "person", "F-0001".into()));
        let script = Branch {
            at: Place::Script { at: Stretch::parse("croc+0x3..0x8").unwrap(), routine: None },
            never: "read".into(),
            wont_reach: None,
            test: None,
            behind: None,
            runs: 0,
        };
        set(&mut f, "F-0001", |x| x.branches = vec![script]);
        assert_eq!(next(&f), ("Cover", "agent", "F-0001".into()));
        assert!(
            f.next().why.starts_with("the script's bytes croc+0x3..0x8 were never read in its recordings"),
            "{}",
            f.next().why
        );
        assert_eq!(
            f.features[0].branches_json(false)["unresolved"][0],
            json!({"at": "croc+0x3..0x8", "never": "read"})
        );
        set(&mut f, "F-0001", |x| x.branches = vec![branch(Some(true))]);
        // Every feature finished: one review of the code as a whole.
        assert_eq!(next(&f), ("Review", "agent", String::new()));
        let how = f.next().how;
        assert!(how.contains("would you write a new game like this?") && how.contains("game tools"), "{how}");
        review(&mut f, Review::Stale { commit: "abc".into(), changed: vec!["game/src/jump.rs".into()], lines: 3 });
        let how = f.next().how;
        assert!(how.contains("a follow-up review") && how.contains("git diff abc -- game tools"), "{how}");
        review(&mut f, Review::Open { unanswered: vec!["Stomp is a bool".into()] });
        assert!(f.next().why.contains("Stomp is a bool"));
        review(&mut f, Review::Holds { commit: "abc".into(), findings: 1 });
        assert_eq!(f.stage("F-0001"), Some(Stage::Reviewed));
        assert_eq!(next(&f), ("Fix", "agent", "F-0002".into()), "reviewed: its held-out failure shows");
        set(&mut f, "F-0002", |x| x.held_out = results(&[("walk/h", &[])]));
        assert_eq!(next(&f), ("Play", "person", "F-0001".into()));
        set(&mut f, "F-0001", |x| x.held_out = results(&[("walk/i", &[])]));
        assert!(f.complete());
        assert_eq!(next(&f), ("Done", "person", String::new()));
    }

    /// Every feature of `f` done, with the given files on the disc.
    fn finished(f: &mut Facts, files: Vec<assets::Asset>) {
        for x in f.features.iter_mut() {
            x.located = true;
            x.unrecorded.clear();
            x.verdict = current();
            x.work = results(&[("walk/a", &[])]);
            x.held_out = results(&[("walk/h", &[])]);
        }
        review(f, Review::Holds { commit: "abc".into(), findings: 0 });
        f.assets = Ok(assets::Inventory { files, ..Default::default() });
        needs_again(f);
    }

    /// What the goal and the features need, read again from the inventory.
    fn needs_again(f: &mut Facts) {
        f.needs = assets::Needs::of(f.port.goal.assets.names(), f.port.goal.assets.is_all(), &f.port, &f.assets);
        for x in f.features.iter_mut() {
            let names = f.port.entries.get(&x.id).map(|e| e.assets.clone()).unwrap_or_default();
            x.assets = assets::Needs::of(&names, false, &f.port, &f.assets);
        }
    }

    fn file(path: &str, state: AssetState) -> assets::Asset {
        assets::Asset { path: path.into(), state, ..Default::default() }
    }

    #[test]
    fn assets_follow_the_features_when_the_goal_has_them() {
        let (_d, mut f) = facts(CONFIRMED, &["F-0001", "F-0002"]);
        let archive =
            assets::Asset { unlisted: true, format: Some("pak".into()), ..file("/DATA.PAK", AssetState::Found) };
        finished(
            &mut f,
            vec![
                file("/MAPS/L1.WLD", AssetState::Found),
                assets::Asset { loaded: Some(false), ..file("/CUTS/BALL.AN2", AssetState::Unknown) },
                archive,
            ],
        );
        // Not in the goal: shown, not asked for.
        assert!(f.complete());
        assert!(f.plain().contains("(not in the goal)"), "{}", f.plain());
        f.port.goal.assets = super::super::Assets::All;
        needs_again(&mut f);
        assert!(!f.complete());
        let next = |f: &Facts| (f.next().item, f.next().why);
        assert_eq!(next(&f).0, "Format");
        assert!(next(&f).1.starts_with("/CUTS/BALL.AN2 has no format yet; no traced run loaded it"));
        let set = |f: &mut Facts, i: usize, s| {
            f.assets.as_mut().unwrap().files[i].state = s;
            needs_again(f);
        };
        set(&mut f, 1, AssetState::Unused);
        assert_eq!(next(&f).0, "Index");
        assert!(next(&f).1.starts_with("/DATA.PAK is an archive (pak) whose entries the importer does not list"));
        // Listed: its entries take its place.
        f.assets.as_mut().unwrap().files[2] = file("/DATA.PAK:LEVEL1.DAT", AssetState::Unused);
        needs_again(&mut f);
        assert_eq!(next(&f).0, "Import");
        assert!(next(&f).1.contains("no psx port check has run the importers"), "{}", next(&f).1);
        // Imported by a check of an older commit: rerun it, no importer to write.
        f.assets.as_mut().unwrap().stale = Some("6c91908".into());
        f.assets.as_mut().unwrap().files[0].imported_then = true;
        needs_again(&mut f);
        assert_eq!(next(&f).0, "Import");
        assert!(
            next(&f).1.starts_with("/MAPS/L1.WLD was imported at 6c91908, the last psx port check"),
            "{}",
            next(&f).1
        );
        assert_eq!(f.next().how, "commit the changes and run psx port check");
        f.assets.as_mut().unwrap().stale = None;
        f.assets.as_mut().unwrap().files[0].imported_then = false;
        set(&mut f, 0, AssetState::Imported);
        assert_eq!(next(&f).0, "Check");
        set(&mut f, 0, AssetState::Checked);
        assert!(f.complete());
        assert_eq!(next(&f).0, "Done");
        assert!(f.plain().contains("assets  1/3 files checked · 2 unused · 0 formats · 0 unknown\n"), "{}", f.plain());
    }

    #[test]
    fn a_feature_brings_in_the_assets_it_names_before_its_own_work() {
        let level = "+++\nid = \"F-0002\"\ntitle = \"Slopes\"\narea = \"player\"\nassets = [\"/CROCFILE.1:MAP1*\"]\n+++\nOn slopes.\n";
        let (d, _) = workspace(CONFIRMED, &[("F-0001.md", &feature("F-0001", "")), ("F-0002.md", level)]);
        let mut f = Facts::gather(&d.0).unwrap();
        let disc = vec![file("/CROCFILE.1", AssetState::Unknown), file("/MAPS/L2.WLD", AssetState::Unknown)];
        finished(&mut f, disc);
        let next = |f: &Facts| {
            let i = f.next();
            (i.item, i.feature.unwrap_or_default(), i.why)
        };
        // No format doc says CROCFILE.1 is an archive: the feature's item comes first.
        let (item, feature, why) = next(&f);
        assert_eq!((item, feature.as_str()), ("Format", "F-0002"));
        assert!(why.contains("an entry of /CROCFILE.1, which no format doc says is an archive"), "{why}");
        assert_eq!(f.stage("F-0002"), Some(Stage::Matching), "its assets keep it from Reviewed");
        // An archive doc, not listed yet: Index.
        f.port.formats.insert(
            "crocfile".into(),
            assets::Format {
                id: "crocfile".into(),
                files: vec!["/CROCFILE.1".into()],
                archive: true,
                ..Default::default()
            },
        );
        f.assets.as_mut().unwrap().files[0] =
            assets::Asset { unlisted: true, format: Some("crocfile".into()), ..file("/CROCFILE.1", AssetState::Found) };
        needs_again(&mut f);
        assert_eq!(next(&f).0, "Index");
        // Listed: only the entries it names.
        let files = &mut f.assets.as_mut().unwrap().files;
        files[0] = assets::Asset { format: Some("level".into()), ..file("/CROCFILE.1:MAP1.GEO", AssetState::Found) };
        files.push(file("/CROCFILE.1:CROC.MOD", AssetState::Found));
        needs_again(&mut f);
        assert_eq!(f.features[1].assets.files, [0]);
        assert_eq!(next(&f).0, "Import");
        f.assets.as_mut().unwrap().files[0].state = AssetState::Checked;
        needs_again(&mut f);
        // The goal asks for no assets: the files no feature names wait.
        assert!(f.complete());
        assert!(f.plain().contains("assets  1/1 needed files checked or unused · disc:"), "{}", f.plain());
        // The goal asks for every level: a format with no doc yet is the agent's to find.
        f.port.goal.assets = super::super::Assets::Names(vec!["format:level".into(), "format:car".into()]);
        needs_again(&mut f);
        let (item, feature, why) = next(&f);
        assert_eq!((item, feature.as_str()), ("Format", ""));
        assert!(why.contains("format:car, and no format doc (formats/car.md)"), "{why}");
        f.port.goal.assets = super::super::Assets::Names(vec!["format:level".into()]);
        needs_again(&mut f);
        assert!(f.complete());
        let named = f.assets_named("/CROCFILE.1:*").unwrap();
        assert_eq!(named["count"], 2);
        assert_eq!(
            named["files"][1],
            json!({"path": "/CROCFILE.1:CROC.MOD", "format": null, "state": "format found", "needed": false})
        );
        assert_eq!(
            f.assets_named("/NOPE.*").unwrap()["unresolved"],
            "matches no disc file or archive entry (code files are not assets)"
        );
    }

    #[test]
    fn branches_count_the_other_features_passing_recordings() {
        let ids: Vec<String> = ["F-0001", "F-0002", "F-0003"].map(String::from).to_vec();
        let of = |id: &str| match id {
            "F-0001" => Some(vec!["walk/a".to_string()]),
            "F-0002" => Some(vec!["run/a".to_string(), "walk/a".to_string()]),
            _ => None,
        };
        // F-0003 has no verdict at HEAD: none of its recordings count.
        let p = passing(&ids, of);
        assert_eq!(p.len(), 2);
        assert_eq!(borrowed("F-0001", &["walk/a".into()], &p), ["run/a"]);
        assert_eq!(borrowed("F-0003", &["climb/a".into()], &p), ["run/a", "walk/a"]);
        let (_d, mut f) = facts(CONFIRMED, &["F-0001", "F-0002"]);
        for id in ["F-0001", "F-0002"] {
            set(&mut f, id, |x| {
                x.located = true;
                x.unrecorded.clear();
                x.verdict = current();
                x.work = results(&[("walk/a", &[])]);
            });
        }
        set(&mut f, "F-0001", |x| {
            x.branches =
                vec![Branch::code(0x8002_D820, 0x8002_D810, 1, None), Branch::code(0x8002_D830, 0x8002_D810, 3, None)];
            x.borrowed = vec!["run/a".into()];
            x.borrowed_resolved = 1;
        });
        // Both matching and reviewed: F-0001's branch is what is left.
        review(&mut f, Review::Holds { commit: "abc".into(), findings: 0 });
        f.review_features = vec!["F-0001".into(), "F-0002".into()];
        let x = &f.features[0];
        assert!(x.summary().ends_with("1/2 branches (1 by other features' recordings)"), "{}", x.summary());
        assert_eq!(x.branches_json(false)["resolved_by_others"]["branches"], 1);
        assert_eq!(f.next().item, "Cover");
        assert!(f.next().why.contains("in its recordings or the 1 of other features that pass;"), "{}", f.next().why);
    }

    #[test]
    fn a_wont_reach_range_answers_only_for_the_branches_in_it_that_never_ran() {
        let wont: BTreeMap<String, String> =
            [("0x8002D800..0x8002D840", "the swim exit"), ("0x8002D830", "under water")]
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .into();
        // Never ran, inside: the range. Ran one way: no key. Its own address wins.
        assert_eq!(code_key(&wont, 0x8002_D820, 0, None).map(String::as_str), Some("0x8002D800..0x8002D840"));
        assert_eq!(code_key(&wont, 0x8002_D820, 1, None), None);
        assert_eq!(code_key(&wont, 0x8002_D830, 0, None).map(String::as_str), Some("0x8002D830"));
        assert_eq!(code_key(&wont, 0x8002_D840, 0, None), None);
        // A test answers for every branch testing it, whichever way it missed.
        let mut tested = wont.clone();
        tested.insert("[0x8007529C] == 2".into(), "no analog pad".into());
        assert_eq!(
            code_key(&tested, 0x8002_D900, 1, Some("[0x8007529C] == 2")).map(String::as_str),
            Some("[0x8007529C] == 2")
        );
        assert_eq!(code_key(&tested, 0x8002_D900, 1, Some("[0x8007529C] == 3")), None);
        let b = Branch {
            test: Some(crate::flow::Test { key: "[0x8007529C] == 2".into(), taken: false }),
            ..Branch::code(0x8002_D900, 0x8002_D810, 1, None)
        };
        assert!(tests_unmatched(&tested, std::slice::from_ref(&b)).is_empty());
        assert_eq!(tests_unmatched(&tested, &[]).len(), 1);
        assert_eq!(b.never_test().as_deref(), Some("[0x8007529C] == 2 never true"));
        // A recording that ran a branch in it says the range is wrong.
        let never_ran = Branch::code(0x8002_D820, 0x8002_D810, 0, Some(true));
        assert_eq!(ranges_run(&wont, std::slice::from_ref(&never_ran)), Vec::<String>::new());
        let ran = Branch::code(0x8002_D824, 0x8002_D810, 1, None);
        assert_eq!(
            ranges_run(&wont, &[never_ran, ran]),
            [
                "wont_reach: 0x8002D800..0x8002D840 holds the branch at 0x8002D824, which a recording ran: a range holds only code no recording reaches"
            ]
        );
    }

    #[test]
    fn a_review_comes_each_time_a_feature_matches_or_finishes() {
        let (_d, mut f) = facts(CONFIRMED, &["F-0001", "F-0002"]);
        finished(&mut f, vec![]);
        // F-0002 is still being built: a recording fails.
        set(&mut f, "F-0002", |x| x.work = results(&[("walk/b", &["tick 9 x: expected 1, got 2"])]));
        let next = |f: &Facts| (f.next().item, f.next().why, f.next().how);
        // A review that holds at HEAD saw the matching code: nothing to review.
        review(&mut f, Review::Holds { commit: "abc".into(), findings: 0 });
        assert_eq!(next(&f).0, "Fix");
        // Changed since, and F-0001 not listed: a follow-up of the change.
        review(&mut f, Review::Stale { commit: "abc".into(), changed: vec!["game/src/run.rs".into()], lines: 40 });
        let (item, why, how) = next(&f);
        assert_eq!(item, "Review");
        assert!(
            why.starts_with(
                "F-0001 finished (branches resolved, no question open) since the last review, and the code changed"
            ),
            "{why}"
        );
        assert!(how.contains("git diff abc") && how.contains("its features to [\"F-0001\"]"), "{how}");
        assert!(how.contains("its finished to [\"F-0001\"]"), "{how}");
        // Listed as matching, not as finished (a review from before the
        // finished list): its finish asks for one.
        f.review_features = vec!["F-0001".into()];
        let (item, why, _) = next(&f);
        assert_eq!(item, "Review");
        assert!(
            why.starts_with("F-0001 finished (branches resolved, no question open) since the last review"),
            "{why}"
        );
        // Listed: building F-0002 changes the code without a review each time.
        f.review_finished = vec!["F-0001".into()];
        assert_eq!(next(&f).0, "Fix");
        // Unless much of it changes before F-0002 matches: a follow-up then.
        let grown = |lines| Review::Stale { commit: "abc".into(), changed: vec!["game/src/run.rs".into()], lines };
        review(&mut f, grown(800));
        let (item, why, how) = next(&f);
        assert_eq!(item, "Review");
        assert!(
            why.starts_with("800 lines of the code changed (goal.toml's review_lines: 800) since the last review"),
            "{why}"
        );
        assert!(how.contains("git diff abc"), "{how}");
        f.port.goal.review_lines = Some(1000);
        assert_eq!(next(&f).0, "Fix");
        f.port.goal.review_lines = None;
        review(&mut f, grown(40));
        // F-0002 matches, with branches still to cover: its review comes
        // before the Cover work, not once that is done.
        set(&mut f, "F-0002", |x| {
            x.work = results(&[("walk/b", &[])]);
            x.branches = vec![Branch::code(0x8002_D820, 0x8002_D810, 1, None)];
        });
        let (item, why, _) = next(&f);
        assert_eq!(item, "Review");
        assert!(why.starts_with("F-0002 came to match"), "{why}");
        f.review_features = vec!["F-0001".into(), "F-0002".into()];
        assert_eq!(next(&f).0, "Cover");
        // Its branches covered: the review of what Cover added.
        set(&mut f, "F-0002", |x| x.branches.clear());
        let (item, why, _) = next(&f);
        assert_eq!(item, "Review");
        assert!(why.starts_with("F-0002 finished"), "{why}");
        f.review_finished = vec!["F-0001".into(), "F-0002".into()];
        review(&mut f, Review::Holds { commit: "abc".into(), findings: 0 });
        assert_ne!(next(&f).0, "Review");
        // Findings are answered as they come.
        review(&mut f, Review::Open { unanswered: vec!["Run is a bool".into()] });
        assert_eq!(next(&f).0, "Review");
    }

    #[test]
    fn verdicts_count_only_at_their_clean_commit() {
        let (d, _) = facts(CONFIRMED, &["F-0001"]);
        let v = |commit: &str, clean| verdict::Verdict {
            feature: "F-0001".into(),
            commit: commit.into(),
            clean,
            at: "t".into(),
            failures: vec![],
            recordings: [("walk/a".to_string(), vec!["tick 3 x".to_string()])].into(),
        };
        let state = || verdict::state(&d.0, "F-0001", Some("abc"));
        assert_eq!(state(), State::Missing);
        verdict::write(&d.0, &v("abc", true)).unwrap();
        let State::Current(now) = state() else { panic!("{:?}", state()) };
        assert_eq!(
            (now.of("walk/a"), now.of("walk/b")),
            (vec!["tick 3 x".to_string()], vec!["not run by the last psx port check".to_string()])
        );
        verdict::write(&d.0, &v("abc", false)).unwrap();
        assert_eq!(state(), State::Stale { commit: "abc".into() });
        verdict::write(&d.0, &v("old", true)).unwrap();
        assert_eq!(state(), State::Stale { commit: "old".into() });
        assert_eq!(verdict::state(&d.0, "F-0001", None), State::Stale { commit: "old".into() });
    }

    #[test]
    fn the_loop_guard_stops_an_unchanged_agent_item() {
        let (d, f) = facts("description = \"x\"\nfeatures = [\"F-0001\", \"F-0002\"]\n", &["F-0001"]);
        let call = || guarded(&d.0, &f, f.next()).unwrap();
        assert_eq!(call()["item"], "Catalogue");
        assert_eq!(call()["item"], "Catalogue");
        let third = call();
        assert_eq!((third["item"].as_str(), third["stuck"]["item"].as_str()), (Some("Stuck"), Some("Catalogue")));
        // Something changing hands the item out again.
        std::fs::write(d.0.join("catalogue/F-0002.md"), feature("F-0002", "")).unwrap();
        let f2 = Facts::gather(&d.0).unwrap();
        assert_eq!(guarded(&d.0, &f2, f2.next()).unwrap()["item"], "Confirm");
        // The person's items never trip it.
        for _ in 0..4 {
            assert_eq!(guarded(&d.0, &f2, f2.next()).unwrap()["item"], "Confirm");
        }
    }
}
