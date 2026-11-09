//! What a port's goal needs next (docs/understanding-plan.md, "Without
//! features"): the facts, read once per call, and the items, each due on a
//! fact and done on a fact the work leaves behind. `port status` shows the
//! facts, `port next` gives the one item due.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use super::assets::{self, State as AssetState};
use super::check;
use super::review::{self, Review};
use super::verdict::{self, State};
use super::{Confirmation, Port};
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

/// Where a branch of the code to port is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Place {
    /// A conditional branch of its machine code, in the function at `function`.
    Code { pc: u32, function: u32 },
    /// A stretch of a script program it names: one read or never read (run
    /// or never run, with the program's map), or a decision; the routine
    /// the map puts it in (docs/script-coverage-design.md).
    Script { at: Stretch, routine: Option<String> },
}

/// A branch of the code to port, as the work recordings ran it: a conditional
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

/// The goal's standing as one program: how many of each, of how many.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Shares {
    pub recordings: (usize, usize),
    pub code: (usize, usize),
    pub branches: (usize, usize),
    /// The work recordings have no result for this commit.
    pub unchecked: bool,
}

impl Shares {
    fn json(&self) -> Value {
        let pair = |(n, of): (usize, usize)| json!({"n": n, "of": of});
        json!({"recordings_passing": pair(self.recordings), "code_accounted": pair(self.code),
               "branches_resolved": pair(self.branches)})
    }

    fn plain(&self) -> String {
        let pct = |(n, of): (usize, usize)| (n * 100).checked_div(of).map_or("-".to_string(), |p| format!("{p}%"));
        let recordings = if self.unchecked {
            format!("{} recordings not checked", self.recordings.1)
        } else {
            format!("recordings passing {}/{} ({})", self.recordings.0, self.recordings.1, pct(self.recordings))
        };
        format!(
            "goal    {recordings} · code cited or not ported {}/{} ({}) · branches resolved {}/{} ({})\n",
            self.code.0,
            self.code.1,
            pct(self.code),
            self.branches.0,
            self.branches.1,
            pct(self.branches)
        )
    }
}

/// Recordings, each with its failures (empty: passed) by the verdict at
/// HEAD; None without one.
pub type Results = Vec<(String, Option<Vec<String>>)>;

/// Every recording passed.
fn all_pass(rs: &Results) -> bool {
    !rs.is_empty() && rs.iter().all(|(_, f)| f.as_ref().is_some_and(Vec::is_empty))
}

/// How many passed.
fn passed(rs: &Results) -> usize {
    rs.iter().filter(|(_, f)| f.as_ref().is_some_and(Vec::is_empty)).count()
}

/// A function a scenario turns off (`[off]`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Off {
    pub scenario: String,
    pub name: String,
    /// The port's code cites it: the port has it, so the original's
    /// recordings can run with it again.
    pub cited: bool,
}

/// Everything the items are decided on.
pub struct Facts {
    pub port: Port,
    pub git: Git,
    pub verdict: State,
    /// The work recordings and the person's held-out plays, by the
    /// verdict at HEAD.
    pub work: Results,
    pub held_out: Results,
    /// What keeps the work recordings from counting: none yet, their
    /// scenarios' problems or determinism checks, coverage psx cannot
    /// count.
    pub unrecorded: Vec<String>,
    /// Recordings in which the original's driver kept the tick's input
    /// from the game, each with what it asks for (fix, record again, or
    /// change a fix that does not work).
    pub late: Vec<(crate::scenario::Late, String)>,
    /// Launchers and observers the port lacks, by the last check at HEAD.
    pub unbuilt: Vec<String>,
    /// Every function the work recordings run, by entry.
    pub ran: BTreeSet<u32>,
    /// The code to port cited nowhere: functions the work recordings run
    /// that not_ported.toml does not list and game/ and tools/ do not cite.
    pub uncited: Vec<u32>,
    /// The branches of the code to port, and of the script it cites.
    pub branches: Vec<Branch>,
    /// The functions the scenarios turn off.
    pub off: Vec<Off>,
    /// What each scenario's assets need, by scenario.
    pub scenario_needs: Vec<(String, assets::Needs)>,
    /// Overlays the inventory knows only from the code that ran in them.
    pub unpack: Vec<String>,
    /// The disc's files by state, or why psx cannot list them.
    pub assets: Result<assets::Inventory, String>,
    /// What the goal's own asset names need.
    pub needs: assets::Needs,
    /// The code's review.
    pub review: Review,
    /// Whether the review says the goal matched its work recordings, and
    /// was finished, when it was written.
    pub review_matching: bool,
    pub review_finished: bool,
    /// The play requests in the queue (`scenario play`), oldest first.
    pub plays: Vec<crate::handoff::queue::Request>,
    /// Debug files `port init`'s scan found on the disc, until `code
    /// symbols` reports them to the agent.
    pub debug_files: Vec<String>,
    /// Functions named, in C and passing ([`crate::decomp::counts`]).
    pub decomp: Value,
    /// Functions whose C passed and now fails ([`crate::decomp::regressed`]).
    pub regressed: Vec<(String, Option<String>, String)>,
    /// The original's function names (names/), by entry with the
    /// segment's bits cleared.
    pub names: BTreeMap<u32, String>,
}

impl Facts {
    pub fn gather(root: &Path) -> crate::Result<Facts> {
        let mut port = Port::load(root)?;
        let git = Git::read(root);
        let ctx = Ctx { root: root.to_path_buf(), cancel: None };
        let inv = crate::coverage::inventory(&ctx, None).ok().map(|(_, v)| v);
        let unpack = inv
            .iter()
            .flat_map(|inv| inv["overlays"].as_array().cloned().unwrap_or_default())
            .filter(|o| o["bounded_by"] == "found_code")
            .filter_map(|o| o["name"].as_str().map(String::from))
            .collect();
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
        let scenario_needs: Vec<(String, assets::Needs)> = port
            .scenario_assets
            .iter()
            .map(|(s, names)| {
                (s.clone(), needs(names, false, &format!("{}/{s}.toml", super::SCENARIOS), &mut problems))
            })
            .collect();
        let work_scenarios = port.work_scenarios();
        let mut unrecorded = if port.recordings.work.is_empty() {
            vec!["no work recordings under scenarios/ (psx scenario record)".into()]
        } else {
            crate::scenario::scenarios_unready(root, &work_scenarios)
        };
        let path =
            |r: &String| r.split_once('/').map(|(s, n)| root.join(super::SCENARIOS).join(s).join(format!("{n}.jsonl")));
        let paths: Vec<PathBuf> = port.recordings.work.iter().filter_map(path).collect();
        // The person confirmed the key as it stands.
        let agreed = |k: &String| port.wont_reach.get(k).is_some_and(|why| port.wont_reach_agreed(k, why));
        let (mut branches, mut ran, mut cited) = (Vec::new(), BTreeSet::new(), BTreeSet::new());
        if let Some(inv) = &inv {
            let not_ported = |f: u32| port.not_ported(f).is_none();
            let cb = crate::coverage::code_branches(&ctx, inv, &paths, &not_ported, &port.cited.addresses);
            unrecorded.extend(cb.problems);
            (ran, cited) = (cb.ran, cb.cited);
            let mut found: Vec<Branch> = cb
                .branches
                .into_iter()
                .map(|c| {
                    let key = code_key(&port.wont_reach, c.pc, c.ways, c.test.as_ref().map(|t| t.key.as_str()));
                    Branch {
                        test: c.test,
                        behind: c.behind,
                        runs: c.runs,
                        ..Branch::code(c.pc, c.function, c.ways, key.map(agreed))
                    }
                })
                .collect();
            // A branch behind another's missed way is answered as that
            // way is, unless listed itself.
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
        let script = &port.cited.stretches;
        if !script.is_empty() && !paths.is_empty() {
            let (items, p) = crate::script_cover::items(&crate::script_cover::merged(&paths), script);
            unrecorded.extend(p);
            branches.extend(items.into_iter().map(|i| {
                let key = port.wont_reach.keys().find(|k| Stretch::parse(k).is_some_and(|w| w.contains(&i.at)));
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
        problems
            .extend(ranges_run(&port.wont_reach, &branches).into_iter().map(|p| super::problem(super::WONT_REACH, p)));
        if inv.is_some() && !paths.is_empty() {
            problems.extend(
                tests_unmatched(&port.wont_reach, &branches).into_iter().map(|p| super::problem(super::WONT_REACH, p)),
            );
        }
        let uncited: Vec<u32> =
            ran.iter().copied().filter(|&f| port.not_ported(f).is_none() && !cited.contains(&f)).collect();
        let fold = |a: u32| a & 0x1FFF_FFFF;
        let off = turned_off(root)
            .into_iter()
            .map(|(scenario, name, a)| Off { cited: cited.iter().any(|&c| fold(c) == fold(a)), scenario, name })
            .collect();
        let verdict = verdict::state(root, judged);
        let results = |names: &[String]| -> Results {
            names
                .iter()
                .map(|n| (n.clone(), if let State::Current(v) = &verdict { Some(v.of(n)) } else { None }))
                .collect()
        };
        let (work, held_out) = (results(&port.recordings.work), results(&port.recordings.held_out));
        let unbuilt = if matches!(verdict, State::Current(_)) && !work.is_empty() {
            check::unimplemented(root, &work_scenarios, head)
        } else {
            Vec::new()
        };
        port.problems.extend(problems);
        if let Ok(a) = &assets {
            port.problems.extend(a.problems.iter().cloned());
        }
        let (review_matching, review_finished) = review::says(root);
        Ok(Facts {
            late: crate::scenario::scenarios_late_input(root, &work_scenarios),
            port,
            git,
            verdict,
            work,
            held_out,
            unrecorded,
            unbuilt,
            ran,
            uncited,
            branches,
            off,
            scenario_needs,
            unpack,
            assets,
            needs: goal_needs,
            review,
            review_matching,
            review_finished,
            plays: crate::handoff::queue::list(root),
            debug_files: crate::disc_names::unread(root),
            decomp: crate::decomp::counts(root),
            regressed: crate::decomp::regressed(root),
            names: psx_symbols::names::load(root)
                .functions
                .iter()
                .map(|f| (f.address & 0x1FFF_FFFF, f.name.text.clone()))
                .collect(),
        })
    }

    /// How many never-ran code branches each wont_reach range answers for.
    pub fn wont_reach_ranges(&self) -> BTreeMap<String, usize> {
        let mut out = BTreeMap::new();
        for k in self.port.wont_reach.keys().filter(|k| super::entry_range(k).is_some()) {
            let answers = |b: &&Branch| match b.at {
                Place::Code { pc, .. } => {
                    code_key(&self.port.wont_reach, pc, (b.never != "ran") as u8, None) == Some(k)
                }
                _ => false,
            };
            out.insert(k.clone(), self.branches.iter().filter(answers).count());
        }
        out
    }

    /// Every work recording passes at HEAD.
    pub fn matching(&self) -> bool {
        matches!(self.verdict, State::Current(_)) && all_pass(&self.work)
    }

    pub fn branches_resolved(&self) -> bool {
        self.branches.iter().all(Branch::resolved)
    }

    /// The work is finished: matching, every branch resolved, no question
    /// open, all the code to port cited.
    pub fn finished(&self) -> bool {
        self.matching() && self.branches_resolved() && self.port.questions.is_empty() && self.uncited.is_empty()
    }

    /// Every asset the goal and the scenarios name is checked or unused.
    fn assets_settled(&self) -> bool {
        self.needs.settled() && self.scenario_needs.iter().all(|(_, n)| n.settled())
    }

    /// The goal is reviewed: finished, the review holds and the assets are
    /// settled. The person's plays are shown from here.
    pub fn reviewed(&self) -> bool {
        self.finished() && self.review.holds() && self.assets_settled()
    }

    /// Reviewed, the person's plays pass, and no scenario turns off code.
    pub fn complete(&self) -> bool {
        self.port.confirmation() != Confirmation::Missing
            && self.reviewed()
            && all_pass(&self.held_out)
            && self.off.is_empty()
    }

    /// The work recordings that fail, in the order Fix gives them.
    pub fn failing(&self) -> Vec<String> {
        match &self.verdict {
            State::Current(v) => v.failing(&self.work.iter().map(|(n, _)| n.clone()).collect::<Vec<_>>()),
            _ => Vec::new(),
        }
    }

    /// The goal's three shares: its recordings passing (the person's plays
    /// once the goal is reviewed), the functions they run cited or not
    /// ported, and the branches resolved.
    pub fn shares(&self) -> Shares {
        let shown = self.reviewed();
        let plays = if shown { passed(&self.held_out) } else { 0 };
        let accounted = self.ran.len() - self.uncited.len();
        let resolved = self.branches.iter().filter(|b| b.resolved()).count();
        Shares {
            recordings: (passed(&self.work) + plays, self.work.len() + self.held_out.len()),
            code: (accounted, self.ran.len()),
            branches: (resolved, self.branches.len()),
            unchecked: !self.work.is_empty() && !matches!(self.verdict, State::Current(_)),
        }
    }

    /// The open branches by what they test and the outcome no recording
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
        let behind = |b: &Branch| self.branches.iter().filter(|x| x.behind.is_some() && x.behind == b.pc()).count();
        let one = |b: &Branch| {
            let mut v = b.json();
            if b.wont_reach == Some(false) {
                v["wont_reach"] = json!("not confirmed by the person");
            }
            if behind(b) > 0 {
                v["behind"] = json!(behind(b));
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
                        "behind": g.iter().map(|b| behind(b)).sum::<usize>(),
                        "wont_reach_key": t,
                        "first": g.iter().take(3).map(|b| one(b)).collect::<Vec<_>>()})
                }
            })
            .collect();
        let mut v = json!({"total": self.branches.len(), "resolved": self.branches.len() - unresolved,
               "open_groups": groups.len(), "unresolved": uncovered});
        if uncovered.len() < groups.len() {
            v["more"] = json!(format!("{} more groups: port_status branches", groups.len() - uncovered.len()));
        }
        v
    }

    /// The recordings by state, as status shows them: the person's plays
    /// counted until the goal is reviewed.
    fn recordings_json(&self) -> Value {
        let checked = self.work.iter().all(|(_, f)| f.is_some());
        let failing = self.failing();
        let mut work = json!({"total": self.work.len(), "pass": checked.then(|| passed(&self.work)),
                              "fail": checked.then_some(failing.len())});
        if !failing.is_empty() {
            let of = |r: &String| {
                let f = self.work.iter().find(|(n, _)| n == r).and_then(|(_, f)| f.clone()).unwrap_or_default();
                format!("{r}: {}", f.join("; "))
            };
            work["failing"] = json!(failing.iter().take(10).map(of).collect::<Vec<_>>());
        }
        let held_out = if self.reviewed() {
            json!({"total": self.held_out.len(), "pass": passed(&self.held_out)})
        } else {
            json!({"total": self.held_out.len(), "shown": "port status --plain, for the person"})
        };
        json!({"work": work, "held_out": held_out})
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

    /// The files the goal and the scenarios need, by index, and how many
    /// of them are checked or unused.
    fn needed(&self) -> (BTreeSet<usize>, usize) {
        let files: BTreeSet<usize> =
            self.scenario_needs.iter().flat_map(|(_, n)| &n.files).chain(&self.needs.files).copied().collect();
        let settled = match &self.assets {
            Ok(a) => files.iter().filter(|&&i| a.files[i].state.settled()).count(),
            Err(_) => 0,
        };
        (files, settled)
    }

    /// The play requests still waiting for the person.
    fn waiting_plays(&self) -> impl Iterator<Item = &crate::handoff::queue::Request> {
        self.plays.iter().filter(|r| !matches!(r.state, crate::handoff::queue::State::Answered(_)))
    }

    /// The item due now, in the order of docs/understanding-plan.md,
    /// "Without features".
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
            let mut item = Item::agent(
                "Read",
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
                    "read the answer: psx scenario play --status {}. A recording is a work recording once it is there; one asked for from the goal's description goes under goal.toml's held_out. When declined, the note says why: ask again in other words or from another start, list the branch under wont_reach with the note as why, or go on without it",
                    r.dir.display()
                ),
            );
            item.recording = r.recording();
            return item;
        }
        // Debug files the build left on the disc name what the agent would.
        if !self.debug_files.is_empty() {
            return Item::agent(
                "Read",
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
        // Until the person confirms the goal, the agent only investigates;
        // the person is asked once the goal and catalogue have no problems.
        if !self.port.problems.is_empty() {
            let n = self.port.problems.len();
            return Item::agent(
                "Catalogue",
                format!("the goal or catalogue has {n} problems: {}", problems_line(&self.port)),
                "the goal and catalogue have no problems",
                "fix each problem (psx port status lists them; catalogue/README.md has the catalogue's files)",
            );
        }
        if !matches!(self.port.confirmation(), Confirmation::Covers { .. }) {
            let why = match self.port.confirmation() {
                Confirmation::Shrunk { missing, .. } => {
                    format!(
                        "what the person confirmed has changed since (the description, assets no longer asked for, their held-out plays deleted or moved to work): {}",
                        missing.join(", ")
                    )
                }
                _ => "the person has not confirmed the goal".into(),
            };
            return Item::person(
                "Confirm",
                why,
                "goal.toml's confirmation covers the goal",
                "the person reads psx port confirm and runs psx port confirm --yes; the agent waits, investigating only",
            );
        }
        // Findings are answered as they come; and a review of the code
        // once much of it changed, so no review reads too much at once.
        let limit = self.port.goal.review_lines();
        let grown = match self.review {
            Review::Stale { lines, .. } if lines >= limit => Some(lines),
            _ => None,
        };
        if matches!(self.review, Review::Open { .. } | Review::Invalid(_)) {
            return self.review_item(String::new());
        }
        if let Some(n) = grown {
            return self.review_item(format!("{n} lines of the code changed (goal.toml's review_lines: {limit})"));
        }
        // The assets a scenario's recordings need to replay.
        if let Some(item) = self.scenario_needs.iter().find_map(|(s, n)| self.asset_item(Some(s), n)) {
            return item;
        }
        // The port cannot replay a recording.
        if self.unrecorded.is_empty() && self.late.is_empty() && !self.unbuilt.is_empty() {
            return Item::agent(
                "Build",
                format!("the port cannot replay the recordings: {}", first_lines(&self.unbuilt).join("; ")),
                "the last psx port check at HEAD finds a launcher for each scenario's kind and each quantity they compare observed",
                "launch the scenario kind in the port, building the world from the scenario's parameters with the port's own loading, and observe the compared quantities (the port's psx_scenario::Port); commit and run psx port check",
            );
        }
        if !self.unpack.is_empty() {
            return Item::agent(
                "Unpack",
                format!(
                    "the inventory knows these overlays only from the code that ran in them, so their functions are not all counted: {}",
                    first_lines(&self.unpack).join(", ")
                ),
                "the inventory has no overlay bounded by found_code",
                "find what decompresses them, unpack them and take the inventory again with --unpacked (guide topic code, Decompressed code); record again any recording whose coverage the new inventory does not credit",
            );
        }
        // Input the original delays: fixed in fixes/, then recorded again.
        // A recording made with fixes/ as it is and still late says the fix
        // does not work; only a change to fixes/ asks for it again, so a fix
        // that does not work is changed, not recorded over and over.
        use crate::scenario::Late;
        let late =
            |k: Late| -> Vec<String> { self.late.iter().filter(|(x, _)| *x == k).map(|(_, l)| l.clone()).collect() };
        let done = "the recordings report no late_input";
        if let ls @ [_, ..] = late(Late::Fails).as_slice() {
            return Item::agent(
                "Fix input",
                format!("fixes/ does not stop the delay: {}", first_lines(ls).join("; ")),
                done,
                "the recording ran with fixes/ as it is and its input still came late on those ticks. A replacement that never ran is not what the game calls there (another function, or another address than names/ gives); one that ran leaves another path that holds the input back. Read those ticks again (run with pad_reads, watch and trace there), change fixes/, and record the scenarios again (guide topic code, the fixes/ paragraphs)",
            );
        }
        if let ls @ [_, ..] = late(Late::Unfixed).as_slice() {
            return Item::agent(
                "Fix input",
                format!("the original's own driver delays its input: {}", first_lines(ls).join("; ")),
                done,
                "the port takes its input every tick, so its recordings must too: find what keeps the poll or the game's read of the buttons back on those ticks (run with pad_reads, watch and trace around them), replace that function in fixes/ so the game takes the tick's input every field (guide topic code, the fixes/ paragraphs), then record the scenarios again; late_input left out with fix on is the fix working",
            );
        }
        if let ls @ [_, ..] = late(Late::Before).as_slice() {
            return Item::agent(
                "Record",
                format!("recorded with input late and before fixes/ as it is: {}", first_lines(ls).join("; ")),
                done,
                "record those scenarios again (psx scenario record): a recording takes fixes/ as it is. late_input left out then is the fix working; still there, port next says the fix does not work",
            );
        }
        if !self.unrecorded.is_empty() {
            return Item::agent(
                "Record",
                format!("the recordings are not ready: {}", first_lines(&self.unrecorded).join("; ")),
                "there are work recordings, each of their scenarios passes psx scenario check, and their coverage counts",
                "declare the quantities in glossary/ and bind them and the tick (the logic step) in glossary/binding/, write scenarios/<id>.toml from a psx states chain whose last run ends at a game moment (until), run psx scenario record, then psx scenario check (psx guide, Recordings)",
            );
        }
        if !matches!(self.verdict, State::Current(_)) {
            let (why, how) = match &self.verdict {
                State::Stale { commit } if self.git.head.as_deref() == Some(commit.as_str()) => (
                    "the verdict is from HEAD, and the workspace has changed since (uncommitted changes besides notes and the review)".into(),
                    "commit the changes and run psx port check",
                ),
                State::Stale { commit } => (
                    format!(
                        "the verdict is from {commit}, not HEAD with no uncommitted changes (notes and the review aside)"
                    ),
                    "commit the changes and run psx port check",
                ),
                _ => (
                    "no psx port check has judged the port".into(),
                    "write the port from the original's code (citing each rule by address), launch the scenario kinds and observe their quantities (the port's psx_scenario::Port), commit, and run psx port check",
                ),
            };
            return Item::agent("Implement", why, "psx port check has judged HEAD", how);
        }
        // A failing work recording: one at a time, the one being fixed while
        // it fails, else the one differing earliest.
        let failing = self.failing();
        if let Some(r) = failing.first() {
            let first = self.work.iter().find(|(n, _)| n == r).and_then(|(_, f)| f.clone()).unwrap_or_default();
            let mut item = Item::agent(
                "Fix",
                format!(
                    "{r} fails: {}{}",
                    first.join("; "),
                    match failing.len() {
                        1 => String::new(),
                        n => format!(" ({n} of {} work recordings fail; this one first)", self.work.len()),
                    }
                ),
                &format!("{r} passes at HEAD"),
                "a recording that differs means the original's rule for those quantities is not yet understood, or the port lacks code the recording runs: the pointer names the code that writes them there, whether the port cites it and its C; understand that code (the debugger on the recording, checked C), port the rule and cite it, commit, and run psx port check; once the port draws, both games' screens at the recording's ticks (psx scenario show --screens) show what differs as well as the quantities do",
            );
            if let State::Current(v) = &self.verdict {
                item.pointer = v.pointers.get(r).cloned();
            }
            item.recording = Some(r.clone());
            return item;
        }
        // A system turned off that the port now has: the original's
        // recordings run with it again.
        let restored: Vec<String> =
            self.off.iter().filter(|o| o.cited).map(|o| format!("{}: {}", o.scenario, o.name)).collect();
        if !restored.is_empty() {
            return Item::agent(
                "Restore",
                format!("the port's code cites what a scenario turns off: {}", restored.join(", ")),
                "no scenario turns off code the port cites",
                "take the function out of the scenario's [off] (scenarios/<id>.toml), record that scenario's recordings again (psx scenario record ... --replace) and check the port against them: they now run with the system the port has",
            );
        }
        if let Some(q) = self.port.questions.first() {
            return Item::agent(
                "Answer",
                format!("open question: {q}"),
                "catalogue/questions.md has no questions left",
                "answer it from the original's code or a recording, cite the answer where the port's code uses it, and take the question out of catalogue/questions.md",
            );
        }
        // Code the work recordings run that the port has nothing of: ported
        // and cited, or set aside.
        if !self.uncited.is_empty() {
            let shown = self.uncited_shown();
            let more = self.uncited.len().saturating_sub(shown.len());
            return Item::agent(
                "Locate",
                format!(
                    "the work recordings run {} functions the port's code (game/, tools/) cites nowhere and catalogue/not_ported.toml does not list: {}{}",
                    self.uncited.len(),
                    shown.join(", "),
                    if more > 0 { format!(" and {more} more (port status lists them)") } else { String::new() }
                ),
                "every function the work recordings run is cited in the port's code or listed in catalogue/not_ported.toml",
                "read each (code disasm, decompile, its callers and what it writes): one holding a rule the game follows is ported, its rules cited by address where the port's code follows them; code the port has its own of or none (library code, the CD and sound drivers, the GPU's command building, debug code) goes in catalogue/not_ported.toml, by entry or address range, with why",
            );
        }
        // An uncovered branch: a recording that reaches it. As many groups as
        // plays wait are left to them: an answer may cover one.
        let waiting = self.waiting_plays().count();
        let groups = self.groups();
        let open: Vec<&Vec<&Branch>> =
            groups.iter().filter(|g| g.iter().any(|b| b.wont_reach.is_none())).skip(waiting).collect();
        if let Some(g) = open.first() {
            let b = g.iter().find(|b| b.wont_reach.is_none()).expect("filtered");
            let n = self.branches.iter().filter(|b| !b.resolved()).count();
            return Item::agent(
                "Cover",
                format!(
                    "{} in the work recordings{}; {n} branches of the code to port are unresolved, in {} groups by what they test",
                    b.describe(),
                    match g.len() {
                        1 => String::new(),
                        k => format!(", as {} other open branches", k - 1),
                    },
                    groups.len()
                ),
                "every branch of the code to port is taken both ways by the work recordings, or listed under wont_reach",
                "read the branch's condition (code disasm; for a script's stretch or decision, your own reading of the program) and work out what reaches the other way in game terms; record that (psx scenario record), or when it cannot be scripted ask the person to play it (psx scenario play queues the request; go on with other work meanwhile); when no recording can reach it, list it in catalogue/wont_reach.toml with why (by its address, or by its group's test, which answers every branch testing it); a script decision no hook named needs a hook in the glossary binding that names its way, then the recordings made again",
            );
        }
        if self.branches.iter().any(|b| !b.resolved() && b.wont_reach.is_some()) {
            return Item::person(
                "Confirm",
                format!(
                    "branches listed under wont_reach the person has not confirmed: {}",
                    first_lines(&self.port.unconfirmed_wont_reach()).join(", ")
                ),
                "the person confirmed each wont_reach branch as it stands",
                "the person reads psx port confirm (each branch and why no recording can reach it) and runs psx port confirm --yes; the agent may go on with other work",
            );
        }
        if let Some(item) = self.asset_item(None, &self.needs) {
            return item;
        }
        // The goal come to match its recordings since the last review, or
        // finished since, without a review that holds.
        if !self.review.holds() {
            let mut why = Vec::new();
            if self.matching() && !self.review_matching {
                why.push("the goal came to match its work recordings since the last review");
            }
            if self.finished() && !self.review_finished {
                why.push(
                    "its work is finished since the last review (branches resolved, no question open, its code cited)",
                );
            }
            if why.is_empty() && self.finished() {
                why.push("the goal's work is finished");
            }
            if !why.is_empty() {
                return self.review_item(why.join(", and "));
            }
        }
        // The person's plays, once the goal is reviewed.
        if self.reviewed() && self.waiting_plays().next().is_none() {
            if self.held_out.is_empty() {
                return Item::person(
                    "Play",
                    "the goal is reviewed and has no held-out plays".into(),
                    "goal.toml names held-out plays (held_out)",
                    &format!(
                        "the agent starts psx scenario play with a request written from the goal's description, in game terms, as broad as it is (\"{}\"): not the work recordings again; the person plays it when they can, and the agent names the recording under goal.toml's held_out when port next gives its Read item",
                        one_line(&self.port.goal.description)
                    ),
                );
            }
            if let Some(r) = self.held_out.iter().find(|(_, f)| f.is_none()).map(|(r, _)| r) {
                return Item::agent(
                    "Implement",
                    format!("no psx port check has judged the play {r} at HEAD"),
                    "psx port check has judged the goal's plays at HEAD",
                    "commit, and run psx port check without a scenario",
                );
            }
            if let Some((r, f)) = self.held_out.iter().find(|(_, f)| f.as_ref().is_some_and(|f| !f.is_empty())) {
                let mut item = Item::agent(
                    "Fix",
                    format!("the goal's play {r} fails: {}", f.as_deref().unwrap_or_default().join("; ")),
                    "the goal's held-out plays pass at HEAD",
                    "the play shows what the work recordings do not: find what in it the port does differently (scenario show on the play, code coverage of it against the work recordings), record what reaches it as a work recording, fix it there, commit, and run psx port check",
                );
                item.recording = Some(r.clone());
                return item;
            }
        }
        // Code a scenario turns off that the port has nothing of.
        let left: Vec<String> = self.off.iter().map(|o| format!("{}: {}", o.scenario, o.name)).collect();
        if self.reviewed() && all_pass(&self.held_out) && !left.is_empty() {
            return Item::agent(
                "Restore",
                format!("scenarios still turn off code the port's code cites nowhere: {}", left.join(", ")),
                "no scenario turns off code",
                "port the function the scenario turns off and cite it (port next then gives Restore for it), or record the scenario's recordings again without the [off] once the port follows it without",
            );
        }
        if self.complete() {
            return Item::person(
                "Done",
                "the goal is reviewed, and the person's plays pass".into(),
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
                format!(
                    "{n} play {} for the person: {}",
                    if n == 1 { "request waits" } else { "requests wait" },
                    first_lines(&asks).join("; ")
                ),
                "the person answers them",
                "the person answers them from psx web or psx play; the agent waits (port next gives a Read item for each answer)",
            );
        }
        let s = self.shares();
        Item::person(
            "Stuck",
            format!(
                "no item fits: recordings passing {}/{}, code accounted {}/{}, branches resolved {}/{}, review {}",
                s.recordings.0,
                s.recordings.1,
                s.code.0,
                s.code.1,
                s.branches.0,
                s.branches.1,
                self.review.name()
            ),
            "",
            "psx's port next has no item for this state: tell the person",
        )
    }

    /// An item for assets `needs` brings in: a scenario's (`scenario`), or
    /// the goal's.
    fn asset_item(&self, scenario: Option<&str>, needs: &assets::Needs) -> Option<Item> {
        if needs.settled() {
            return None;
        }
        let a = match &self.assets {
            Ok(a) => a,
            Err(why) => {
                return Some(Item::agent(
                    "Assets",
                    format!("psx cannot list the disc's files: {why}"),
                    "psx port status lists the disc's files",
                    "save the workspace's disc image as its media: psx context --image media/<game>.cue --save",
                ));
            }
        };
        let asks = |name: &str| match scenario {
            Some(s) => format!("{}/{s}.toml's assets name {name}", super::SCENARIOS),
            None => format!("the goal's assets name {name}"),
        };
        for (name, why) in &needs.unresolved {
            match why {
                assets::Unresolved::NoFormat(id) => {
                    return Some(Item::agent(
                        "Format",
                        format!("{}, and {}", asks(name), why.describe()),
                        &format!("formats/{id}.md names the disc files in that format"),
                        "find which disc files these are: run the game where it loads one with trace_cd (loads.json gives each file's place in RAM), watch that buffer for the code that parses it, and read that code (code disasm, decompile); write the format doc (formats/README.md) naming the files and the loader",
                    ));
                }
                assets::Unresolved::Archive { path, documented: false } => {
                    return Some(Item::agent(
                        "Format",
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
                format!("{} has no format yet{read}; {n} files are unknown", f.path),
                "every disc file is covered by a format doc, found whole as a standard format, or listed in formats/unused.toml",
                "find what loads it: run with trace_cd (loads.json gives where each file goes in RAM), watch that buffer for the code that parses it, and read that code (code disasm, decompile); write formats/<id>.md (formats/README.md) naming the files and the loader; when no run ever loads it, list it in formats/unused.toml with the runs that show so",
            ));
        }
        if let Some(f) = unlisted.first() {
            return Some(Item::agent(
                "Index",
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
                format!("{} ({}) is not imported{stale}", f.path, f.format.as_deref().unwrap_or("")),
                "the importers' outputs name it in their provenance, at HEAD",
                "write its importer in tools/import, converting exactly into the port's formats with Provenance naming the disc file; commit and run psx port check, which runs the importers on the disc",
            ));
        }
        first(AssetState::Imported).map(|f| {
            Item::agent(
                "Check",
                format!("{} is imported, not checked against what the game built", f.path),
                "an importer test that compares it with what the game built marks it with import::checking and passes, at HEAD",
                "write a test in tools/import comparing the importer's output with what the game built from that file (RAM after loading it, its VRAM uploads, a GPU capture of the same view: psx run with trace_cd, psx debug go --gpu), starting with let _checked = import::checking(\"/PATH\"); commit and run psx port check",
            )
        })
    }

    /// The uncited functions as Locate shows them: the first 20 by address.
    fn uncited_shown(&self) -> Vec<String> {
        self.uncited.iter().take(20).map(|&a| self.function_name(a)).collect()
    }

    /// A function as port next names it: its name and entry, or the entry.
    fn function_name(&self, a: u32) -> String {
        match self.names.get(&(a & 0x1FFF_FFFF)) {
            Some(n) => format!("{n} ({})", hex(a)),
            None => hex(a),
        }
    }

    /// The review item due, `why` it is due when the review is missing or
    /// stale.
    fn review_item(&self, why: String) -> Item {
        let done = "the review holds: the code (game/, tools/) unchanged since its commit, every finding answered, its matching and finished what the goal is";
        match &self.review {
            Review::Open { unanswered } => Item::agent(
                "Review",
                format!("findings without an answer: {}", first_lines(unanswered).join("; ")),
                done,
                &format!(
                    "answer each finding in {} with an **Answer:** line: done (restructure as it asks, keeping what the code does: psx port check must pass again), or kept and why; changed code has a follow-up review when the goal next comes to match or finish, or once {} lines of it change",
                    review::REVIEW,
                    self.port.goal.review_lines()
                ),
            ),
            Review::Invalid(why) => Item::agent(
                "Review",
                format!("{} is not a review: {why}", review::REVIEW),
                done,
                "write the review's front matter as the brief says: +++, commit, matching, finished, +++",
            ),
            Review::Stale { commit, changed, .. } => Item::agent(
                "Review",
                format!("{why}, and the code changed since the review: {}", first_lines(changed).join(", ")),
                done,
                &self.follow_up(commit),
            ),
            _ => Item::agent("Review", format!("{why}, and the code has no review"), done, &self.brief()),
        }
    }

    /// The brief's front matter and the parts both briefs share.
    fn front(&self) -> String {
        format!(
            "commit = \"{}\", matching = {} and finished = {}",
            self.git.head.as_deref().unwrap_or("HEAD"),
            self.matching(),
            self.finished()
        )
    }

    /// The brief's part on what the work recordings leave out, against the
    /// goal's description.
    fn breadth(&self) -> String {
        format!(
            " Then the goal: read {goal}'s description and the scenarios (scenarios/*.toml, what each records) and ask what the description asks for that no work recording shows, its breadth included; each is a finding, answered by a recording that shows it or kept with why.",
            goal = super::GOAL,
        )
    }

    /// What both briefs end with: the screens and the person's report.
    const LOOK: &'static str = " Then, before writing how the port looks, look at it: both games' screens over a few recordings (psx scenario show ID --name NAME --screens), or that it does not draw yet. Then file a report for the person alone with report (psx report --text ...): how the port looks to you as a game in the making, what worries you about it and anything you would not put to the writer; the writer does not see it.";

    /// The Review item's brief, for a reviewer who has not seen the work.
    fn brief(&self) -> String {
        format!(
            "Give this brief to a reviewer who has not seen the work (a sub-agent or a new session): review the port's code as a whole, {code} (git ls-files {code}). Its recordings already check that it does what the original does, so this review does not: for the code, leave the notes and the original's code unread. Ask instead: would you write a new game like this? Judge it as code written for a new game today, not as a port: how it is divided (modules, plugins, systems, components, data), names, how readily it takes more (another character, level or rule), duplication, and what is shaped by the original rather than chosen. AGENTS.md's \"The code\" says what the tools rely on. Write {review}: front matter between +++ lines with {front}, then one ## heading per finding: what you would change, the shape you would choose instead and why; what the code does stays as it is. The writer then answers each finding with an **Answer:** line.{breadth}{look}",
            breadth = self.breadth(),
            look = Self::LOOK,
            code = review::CODE.join(" "),
            review = review::REVIEW,
            front = self.front(),
        )
    }

    /// The brief for a review of what changed since the review at `commit`:
    /// what that review read is not read again.
    fn follow_up(&self, commit: &str) -> String {
        format!(
            "Give this brief to a reviewer who did not write the changes (a sub-agent or a new session): a follow-up review of the port's code. The review at {commit} read the code then; read what changed since: git diff {commit} -- {code}, with the findings and answers in {review} (are the changes what the answers say?), and enough of the code around it to judge how the change fits the whole (how the code is divided, what new code reuses or duplicates). Ask what the review asked: would you write a new game like this? If the changes rewrite most of the code, review it whole. Leave the notes and the original's code unread: the recordings check what the code does. Keep the findings and answers already in {review}; set its front matter to {front}, and add one ## heading per new finding: what you would change, the shape you would choose instead and why; with none, the front matter is all that changes. The writer then answers each new finding with an **Answer:** line.{breadth}{look}",
            breadth = self.breadth(),
            look = Self::LOOK,
            code = review::CODE.join(" "),
            review = review::REVIEW,
            front = self.front(),
        )
    }

    /// A review is due: `review_lines` of the code changed since the last,
    /// or that many written with none yet. Why, and the reviewer's brief.
    /// Nothing waits on it.
    /// Every quantity the recordings' scenarios compare, by name.
    pub fn compared(&self, root: &Path) -> Vec<String> {
        let names: BTreeSet<String> = super::scenarios_of(self.port.recordings.all())
            .iter()
            .filter_map(|id| crate::scenario::Loaded::load(root, id).scenario)
            .flat_map(|s| s.compare.into_keys())
            .collect();
        names.into_iter().collect()
    }

    pub fn review_due(&self, root: &Path) -> Option<(String, String)> {
        let limit = self.port.goal.review_lines();
        match &self.review {
            Review::Stale { commit, lines, .. } if *lines >= limit => Some((
                format!("{lines} lines of the code changed since the review at {commit}"),
                self.follow_up(commit),
            )),
            Review::Missing => {
                let lines = review::lines_written(root);
                (lines >= limit).then(|| (format!("{lines} lines of the code written and no review yet"), self.brief()))
            }
            _ => None,
        }
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

    /// Where the goal is: matching, finished, reviewed, complete, or none.
    fn state(&self) -> &'static str {
        if self.complete() {
            "complete"
        } else if self.reviewed() {
            "reviewed"
        } else if self.finished() {
            "finished"
        } else if self.matching() {
            "matching"
        } else {
            "working"
        }
    }

    /// `port status`.
    pub fn status(&self) -> Value {
        let accounted = self.ran.len() - self.uncited.len();
        let not_ported = self.ran.iter().filter(|&&f| self.port.not_ported(f).is_some()).count();
        let mut code = json!({"ran": self.ran.len(), "cited": accounted - not_ported, "not_ported": not_ported,
                              "uncited": self.uncited.len()});
        if !self.uncited.is_empty() {
            code["first_uncited"] = json!(self.uncited_shown());
        }
        let mut v = json!({
            "description": self.port.goal.description,
            "state": self.state(),
            "confirmation": self.port.confirmation_json(),
            "problems": self.port.problems,
            "recordings": self.recordings_json(),
            "verdict": self.verdict.name(),
            "code": code,
            "branches": self.branches_json(false),
            "questions": self.port.questions,
            "review": self.review_json(),
            "complete": self.complete(),
            "git": {"head": self.git.head, "clean": self.git.clean},
            "decomp": self.decomp,
            "shares": self.shares().json(),
        });
        if !self.unrecorded.is_empty() {
            v["unrecorded"] = json!(self.unrecorded);
        }
        if !self.late.is_empty() {
            v["late_input"] = json!(self.late.iter().map(|(_, l)| l).collect::<Vec<_>>());
        }
        if !self.unbuilt.is_empty() {
            v["unbuilt"] = json!(self.unbuilt);
        }
        if !self.off.is_empty() {
            v["off"] = json!(
                self.off
                    .iter()
                    .map(|o| json!({"scenario": o.scenario, "function": o.name, "cited": o.cited}))
                    .collect::<Vec<_>>()
            );
        }
        v["assets"] = match &self.assets {
            Ok(a) => {
                let mut j = a.json(self.port.formats.len());
                j["in_goal"] = self.port.goal.assets.to_json();
                let (needed, settled) = self.needed();
                j["needed"] = json!({"files": needed.len(), "settled": settled});
                if !self.needs.unresolved.is_empty() {
                    j["goal"] = self.needs.json();
                }
                let scenarios: BTreeMap<&String, Value> =
                    self.scenario_needs.iter().filter(|(_, n)| !n.settled()).map(|(s, n)| (s, n.json())).collect();
                if !scenarios.is_empty() {
                    j["scenarios"] = json!(scenarios);
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
            .map(|r| r.json())
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
        s.push_str(&format!("goal: {}\n", if goal.is_empty() { "(no description)" } else { goal }));
        s.push_str(&self.shares().plain());
        let failing = self.failing();
        let n = self.work.len();
        let work = match &self.verdict {
            State::Current(_) => format!("{n} work, {} failing", failing.len()),
            State::Missing => format!("{n} work, never checked (port_check)"),
            State::Stale { .. } if self.git.clean => {
                format!("{n} work, not checked since the last change (port_check)")
            }
            State::Stale { .. } => {
                format!("{n} work, not checked since the last change (uncommitted changes; port_check judges a commit)")
            }
        };
        s.push_str(&format!(
            "recordings  {work} · {} held-out plays, {} passing\n",
            self.held_out.len(),
            passed(&self.held_out),
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
        s.push_str(&match &self.review {
            Review::Holds { commit, findings } => {
                format!("code reviewed at {commit} ({findings} findings, answered)\n")
            }
            Review::Stale { commit, changed, lines } => {
                format!(
                    "code reviewed at {commit}, changed since in {} files ({lines} of {} lines before a follow-up)\n",
                    changed.len(),
                    self.port.goal.review_lines()
                )
            }
            r => format!("code review: {}\n", r.name()),
        });
        for r in failing.iter().take(3) {
            s.push_str(&format!("  failing: {r}\n"));
        }
        if let Some(b) = self.groups().first().map(|g| g[0]) {
            s.push_str(&format!("  uncovered: {}\n", b.describe()));
        }
        for q in self.port.questions.iter().take(2) {
            s.push_str(&format!("  open: {q}\n"));
        }
        if !self.port.problems.is_empty() {
            s.push_str(&format!("\n{} problems in the goal or catalogue.\n", self.port.problems.len()));
        }
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
    /// The recording it is about (Fix, Read).
    pub recording: Option<String>,
    /// The fact that makes it due.
    pub why: String,
    /// The fact that will show it done.
    pub done_when: String,
    pub how: String,
    /// Where in the original to look (Fix: port check's pointer).
    pub pointer: Option<Value>,
}

impl Item {
    fn agent(item: &'static str, why: String, done_when: &str, how: &str) -> Item {
        Item { item, who: "agent", recording: None, why, done_when: done_when.into(), how: how.into(), pointer: None }
    }

    fn person(item: &'static str, why: String, done_when: &str, how: &str) -> Item {
        Item { who: "person", ..Item::agent(item, why, done_when, how) }
    }

    fn to_json(&self) -> Value {
        let mut v = json!({"item": self.item, "who": self.who, "why": self.why,
               "done_when": self.done_when, "how": self.how});
        if let Some(r) = &self.recording {
            v["recording"] = json!(r);
        }
        if let Some(p) = &self.pointer {
            v["pointer"] = p.clone();
        }
        v
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

/// The counts the progress line keeps: the goal's state and verdict, its
/// branches and work recordings, problems, questions, the review and the
/// next item.
fn standing(status: &Value) -> Value {
    let next = &status["next"];
    let work = &status["recordings"]["work"];
    json!({
        "head": status["git"]["head"],
        "state": status["state"],
        "verdict": status["verdict"],
        "branches": {"resolved": status["branches"]["resolved"], "total": status["branches"]["total"]},
        "recordings": {"pass": work["pass"], "total": work["total"]},
        "shares": status["shares"],
        "problems": status["problems"].as_array().map_or(0, Vec::len),
        "questions": status["questions"].as_array().map_or(0, Vec::len),
        "review": status["review"]["state"],
        "decomp": status["decomp"],
        "next": {"item": next["item"], "recording": next["recording"], "who": next["who"], "times": next["times"]},
    })
}

/// The port's status with its next item as `port status` gives them, and
/// the loop guard as the last `port next` left it, for `psx web`: nothing is
/// written.
pub fn look(root: &Path) -> crate::Result<Value> {
    let f = Facts::gather(root)?;
    Ok(f.status())
}

fn guarded(root: &Path, facts: &Facts, item: Item) -> crate::Result<Value> {
    let path = root.join(".psx/port/next.json");
    let key = format!("{} {}", item.item, item.recording.as_deref().unwrap_or(""));
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
    /// Every unresolved branch, not only the first groups.
    #[arg(long)]
    #[serde(default)]
    pub branches: bool,
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
        let plain = self.plain && !self.branches && self.assets.is_none();
        Outcome::result_text(
            Facts::gather(&ctx.root).and_then(|f| {
                if self.branches {
                    return Ok(json!({"branches": f.branches_json(true)}));
                }
                if let Some(name) = &self.assets {
                    return f.assets_named(name);
                }
                let mut v = f.status();
                progress(&ctx.root, &v);
                let mut text = f.plain();
                let compared = f.compared(&ctx.root);
                text.push_str(&format!(
                    "compared  {}\n",
                    if compared.is_empty() { "nothing yet".into() } else { compared.join(", ") }
                ));
                v["compared"] = json!(compared);
                if let Some((why, brief)) = f.review_due(&ctx.root) {
                    text.push_str(&format!("\nreview due: {why}\n"));
                    v["review_due"] = json!({"why": why, "brief": brief});
                }
                if !f.debug_files.is_empty() {
                    v["debug_files"] = json!(f.debug_files);
                }
                // Information only: what the states and shares decided is
                // the person's now.
                if let Some(o) = v.as_object_mut() {
                    for k in ["state", "confirmation", "complete", "shares", "questions"] {
                        o.remove(k);
                    }
                }
                Ok(if self.plain { json!({"text": text}) } else { v })
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
    use super::super::tests::workspace;
    use super::*;

    const CONFIRMED: &str = "description = \"x\"\nconfirmed = { at = \"t\" }\n";

    fn facts(goal: &str, files: &[(&str, &str)]) -> (super::super::tests::tempdir::Dir, Facts) {
        let (dir, _) = workspace(goal, files);
        let f = Facts::gather(&dir.0).unwrap();
        (dir, f)
    }

    fn verdict(commit: &str, clean: bool, recordings: &[(&str, &[&str])]) -> verdict::Verdict {
        verdict::Verdict {
            goal: super::super::GOAL_ID.into(),
            commit: commit.into(),
            clean,
            at: "t".into(),
            failures: vec![],
            recordings: recordings
                .iter()
                .map(|(n, f)| (n.to_string(), f.iter().map(|l| l.to_string()).collect()))
                .collect(),
            differs: BTreeMap::new(),
            fixing: None,
            pointers: BTreeMap::new(),
        }
    }

    fn results(rs: &[(&str, &[&str])]) -> Results {
        rs.iter().map(|(n, f)| (n.to_string(), Some(f.iter().map(|l| l.to_string()).collect()))).collect()
    }

    /// Facts for a goal whose work recordings all pass at HEAD, its code
    /// cited and its branches resolved, the review missing.
    fn matching() -> (super::super::tests::tempdir::Dir, Facts) {
        let (d, mut f) = facts(CONFIRMED, &[]);
        f.unrecorded.clear();
        f.port.problems.clear();
        f.verdict = State::Current(verdict("abc", true, &[("walk/a", &[]), ("walk/b", &[])]));
        f.work = results(&[("walk/a", &[]), ("walk/b", &[])]);
        f.ran = [0x8001_0000].into();
        f.needs = assets::Needs::default();
        (d, f)
    }

    #[test]
    fn recordings_without_a_result_read_as_not_checked_rather_than_failing_none() {
        let (_d, mut f) = facts(CONFIRMED, &[]);
        f.work = vec![("a/b".to_string(), None)];
        let text = f.plain();
        assert!(text.contains("1 recordings not checked"), "{text}");
        assert!(text.contains("1 work, never checked"), "{text}");
        assert!(!text.contains("failing"), "{text}");
        assert!(f.status()["recordings"]["work"]["fail"].is_null());
    }

    #[test]
    fn a_review_is_due_after_enough_changed_code_and_never_before() {
        let (d, mut f) = facts(CONFIRMED, &[]);
        let stale = |lines| Review::Stale { commit: "abc".into(), changed: vec!["game/src/a.rs".into()], lines };
        f.review = stale(super::super::REVIEW_LINES - 1);
        assert_eq!(f.review_due(&d.0), None);
        f.review = stale(super::super::REVIEW_LINES);
        let (why, brief) = f.review_due(&d.0).unwrap();
        assert!(why.contains("since the review at abc"), "{why}");
        assert!(brief.starts_with("Give this brief"), "{brief}");
        f.review = Review::Open { unanswered: vec!["a finding".into()] };
        assert_eq!(f.review_due(&d.0), None);
    }

    #[test]
    fn debug_files_on_the_disc_are_read_first_until_code_symbols_reports_them() {
        let (d, _) = facts(CONFIRMED, &[]);
        let scan = json!({"debug_files": [{"at": "/GAME.SYM", "kind": "sym"}], "files_by_name": [
            {"path": "/GAME.SYM", "kind": "sym"}, {"path": "/README.C", "kind": "source"}]});
        crate::disc_names::record(&d.0, &scan, false).unwrap();
        let item = Facts::gather(&d.0).unwrap().next();
        assert_eq!((item.item, item.who), ("Read", "agent"));
        assert!(item.why.ends_with("/GAME.SYM (sym), /README.C (source)"), "{}", item.why);
        crate::disc_names::record(&d.0, &scan, true).unwrap();
        assert_ne!(Facts::gather(&d.0).unwrap().next().item, "Read");
    }

    #[test]
    fn a_function_whose_c_passed_and_now_fails_is_fixed() {
        let (d, _) = facts(CONFIRMED, &[]);
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
        last("failed");
        std::fs::write(checks.join("checks.jsonl"), "{\"function\":\"step\",\"status\":\"failed\"}\n").unwrap();
        assert!(Facts::gather(&d.0).unwrap().regressed.is_empty());
        std::fs::write(checks.join("checks.jsonl"),
            "{\"function\":\"step\",\"status\":\"passed\",\"commit\":\"abcdef0123456\"}\n{\"function\":\"step\",\"status\":\"failed\"}\n").unwrap();
        let item = Facts::gather(&d.0).unwrap().next();
        assert_eq!((item.item, item.who, item.recording.as_deref()), ("Fix", "agent", None));
        assert_eq!(item.why, "step's C passed its check at abcdef0123 and now fails: v0: original 0x1, yours 0x2");
        last("passed");
        assert!(Facts::gather(&d.0).unwrap().regressed.is_empty());
    }

    #[test]
    fn the_catalogue_comes_before_the_person_confirms() {
        let (_d, f) = facts("description = \"\"\n", &[]);
        assert_eq!(f.next().item, "Catalogue");
        let (_d, f) = facts("description = \"x\"\n", &[]);
        assert_eq!((f.next().item, f.next().who), ("Confirm", "person"));
        // The description changed since: confirmed again.
        let (_d, f) = facts("description = \"x\"\nconfirmed = { at = \"t\", description = \"0\" }\n", &[]);
        assert_eq!(f.next().item, "Confirm");
        let (_d, f) = facts(CONFIRMED, &[]);
        assert_ne!(f.next().item, "Confirm");
    }

    #[test]
    fn items_follow_the_goal() {
        let (_d, mut f) = facts(CONFIRMED, &[]);
        assert_eq!(f.next().item, "Record");
        assert!(f.next().why.contains("no work recordings"), "{}", f.next().why);
        f.unrecorded.clear();
        f.work = vec![("walk/a".into(), None), ("walk/b".into(), None)];
        assert_eq!(f.next().item, "Implement");
        // Fix: the recording differing earliest first.
        let mut v = verdict("abc", true, &[("walk/a", &["x differs"]), ("walk/b", &["y differs"])]);
        v.differs.insert("walk/a".into(), verdict::Differs { tick: 90, quantities: 1 });
        v.differs.insert("walk/b".into(), verdict::Differs { tick: 10, quantities: 3 });
        f.verdict = State::Current(v.clone());
        f.work = results(&[("walk/a", &["x differs"]), ("walk/b", &["y differs"])]);
        let item = f.next();
        assert_eq!((item.item, item.recording.as_deref()), ("Fix", Some("walk/b")));
        assert!(item.why.starts_with("walk/b fails: y differs (2 of 2 work recordings fail"), "{}", item.why);
        // While it still fails, the one being fixed stays first.
        v.fixing = Some("walk/a".into());
        f.verdict = State::Current(v);
        assert_eq!(f.next().recording.as_deref(), Some("walk/a"));
        // Passing: what the recordings run that the port does not cite.
        let (_d, mut f) = matching();
        f.ran = [0x8001_0000, 0x8001_0100].into();
        f.uncited = vec![0x8001_0100];
        let item = f.next();
        assert_eq!(item.item, "Locate");
        assert!(item.why.contains("run 1 functions") && item.why.ends_with("0x80010100"), "{}", item.why);
        assert_eq!(f.shares().code, (1, 2));
        // A question first.
        f.port.questions = vec!["what is the slope limit?".into()];
        assert_eq!(f.next().item, "Answer");
        f.port.questions.clear();
        f.uncited.clear();
        // An open branch: Cover, unless as many groups wait on plays.
        f.branches = vec![Branch::code(0x8001_0010, 0x8001_0000, 1, None)];
        assert_eq!(f.next().item, "Cover");
        f.branches[0].wont_reach = Some(false);
        let item = f.next();
        assert_eq!((item.item, item.who), ("Confirm", "person"));
        f.branches[0].wont_reach = Some(true);
        // Finished: the review.
        let item = f.next();
        assert_eq!(item.item, "Review");
        assert!(
            item.why.contains("came to match") && item.how.contains("matching = true and finished = true"),
            "{item:?}"
        );
        f.review = Review::Holds { commit: "abc".into(), findings: 0 };
        f.review_matching = true;
        f.review_finished = true;
        // Reviewed: the person's plays.
        let item = f.next();
        assert_eq!((item.item, item.who), ("Play", "person"));
        f.held_out = results(&[("walk/play", &["x differs"])]);
        let item = f.next();
        assert_eq!((item.item, item.recording.as_deref()), ("Fix", Some("walk/play")));
        f.held_out = results(&[("walk/play", &[])]);
        assert!(f.complete());
        assert_eq!(f.next().item, "Done");
        assert_eq!(f.shares().recordings, (3, 3));
    }

    #[test]
    fn a_system_turned_off_comes_back_once_the_port_cites_it() {
        let (_d, mut f) = matching();
        f.review = Review::Holds { commit: "abc".into(), findings: 0 };
        f.held_out = results(&[("walk/play", &[])]);
        f.off = vec![Off { scenario: "walk".into(), name: "enemy_update".into(), cited: false }];
        assert!(!f.complete());
        let item = f.next();
        assert_eq!(item.item, "Restore");
        assert!(item.why.starts_with("scenarios still turn off code"), "{}", item.why);
        f.off[0].cited = true;
        let item = f.next();
        assert!(item.item == "Restore" && item.why.contains("walk: enemy_update"), "{item:?}");
        // A failing recording comes first.
        f.verdict = State::Current(verdict("abc", true, &[("walk/a", &["x"]), ("walk/b", &[])]));
        f.work = results(&[("walk/a", &["x"]), ("walk/b", &[])]);
        assert_eq!(f.next().item, "Fix");
    }

    #[test]
    fn input_the_original_delays_is_fixed_before_the_port_is_written() {
        use crate::scenario::Late;
        let (_d, mut f) = facts(CONFIRMED, &[]);
        f.unrecorded.clear();
        let line = "scenarios/walk/a.jsonl: the game ran 3 ticks on input a tick old (ticks 4, 9, 12)";
        f.late = vec![(Late::Unfixed, line.into())];
        let item = f.next();
        assert_eq!(item.item, "Fix input");
        assert!(item.why.contains("ticks 4, 9, 12") && item.how.contains("replace that function in fixes/"));
        f.late = vec![(Late::Before, line.into())];
        assert_eq!(f.next().item, "Record");
        f.late = vec![(Late::Fails, line.into()), (Late::Before, line.into())];
        assert!(f.next().why.starts_with("fixes/ does not stop the delay"));
    }

    #[test]
    fn uncommitted_changes_leave_a_verdict_at_head_stale() {
        let (dir, _) = workspace(CONFIRMED, &[("scenarios/walk/a.jsonl", "")]);
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
        verdict::write(&dir.0, &verdict(&head, true, &[("walk/a", &[])])).unwrap();
        let state = || Facts::gather(&dir.0).unwrap().verdict;
        assert!(matches!(state(), State::Current(_)), "{:?}", state());
        assert!(Facts::gather(&dir.0).unwrap().work == results(&[("walk/a", &[])]));
        std::fs::create_dir_all(dir.0.join("notes")).unwrap();
        std::fs::write(dir.0.join("notes/a.md"), "x").unwrap();
        assert!(matches!(state(), State::Current(_)));
        std::fs::write(dir.0.join("scenarios/walk/a.jsonl"), "{}").unwrap();
        assert_eq!(state(), State::Stale { commit: head });
    }

    #[test]
    fn verdicts_count_only_at_their_clean_commit() {
        let (d, _) = facts(CONFIRMED, &[]);
        let state = || verdict::state(&d.0, Some("abc"));
        assert_eq!(state(), State::Missing);
        verdict::write(&d.0, &verdict("abc", true, &[("walk/a", &["x"])])).unwrap();
        let State::Current(now) = state() else { panic!("{:?}", state()) };
        assert_eq!(
            (now.of("walk/a"), now.of("walk/b")),
            (vec!["x".to_string()], vec!["not run by the last psx port check".to_string()])
        );
        verdict::write(&d.0, &verdict("abc", false, &[])).unwrap();
        assert_eq!(state(), State::Stale { commit: "abc".into() });
        verdict::write(&d.0, &verdict("old", true, &[])).unwrap();
        assert_eq!(verdict::state(&d.0, None), State::Stale { commit: "old".into() });
    }

    #[test]
    fn a_wont_reach_range_answers_only_for_the_branches_in_it_that_never_ran() {
        let wont: BTreeMap<String, String> =
            [("0x8002D800..0x8002D840", "the swim exit"), ("0x8002D830", "under water")]
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .into();
        assert_eq!(code_key(&wont, 0x8002_D820, 0, None).map(String::as_str), Some("0x8002D800..0x8002D840"));
        assert_eq!(code_key(&wont, 0x8002_D820, 1, None), None);
        assert_eq!(code_key(&wont, 0x8002_D830, 0, None).map(String::as_str), Some("0x8002D830"));
        let mut tested = wont.clone();
        tested.insert("[0x8007529C] == 2".into(), "no analog pad".into());
        let b = Branch {
            test: Some(crate::flow::Test { key: "[0x8007529C] == 2".into(), taken: false }),
            ..Branch::code(0x8002_D900, 0x8002_D810, 1, None)
        };
        assert!(tests_unmatched(&tested, std::slice::from_ref(&b)).is_empty());
        assert_eq!(tests_unmatched(&tested, &[]).len(), 1);
        let never_ran = Branch::code(0x8002_D820, 0x8002_D810, 0, Some(true));
        let ran = Branch::code(0x8002_D824, 0x8002_D810, 1, None);
        assert_eq!(ranges_run(&wont, std::slice::from_ref(&never_ran)), Vec::<String>::new());
        assert_eq!(ranges_run(&wont, &[never_ran, ran]).len(), 1);
    }

    #[test]
    fn the_loop_guard_stops_an_unchanged_agent_item() {
        let (d, f) = facts("description = \"\"\n", &[]);
        let call = || guarded(&d.0, &f, f.next()).unwrap();
        assert_eq!(call()["item"], "Catalogue");
        assert_eq!(call()["item"], "Catalogue");
        let third = call();
        assert_eq!((third["item"].as_str(), third["stuck"]["item"].as_str()), (Some("Stuck"), Some("Catalogue")));
        std::fs::write(d.0.join(super::super::GOAL), "description = \"x\"\n").unwrap();
        let f2 = Facts::gather(&d.0).unwrap();
        for _ in 0..4 {
            assert_eq!(guarded(&d.0, &f2, f2.next()).unwrap()["item"], "Confirm");
        }
    }
}
