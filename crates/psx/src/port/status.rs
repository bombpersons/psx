//! `port status`: where a port stands, read once per call. Information
//! only: what to do next is the agent's to choose and the person's to
//! redirect (docs/simple-port-plan.md).

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use super::Port;
use super::assets::{self, State as AssetState};
use super::check;
use super::review::{self, Review};
use super::verdict::{self, State};
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
}

fn git(root: &Path, args: &[&str]) -> Option<String> {
    let out = std::process::Command::new("git").args(args).current_dir(root).output().ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

impl Git {
    pub fn read(root: &Path) -> Git {
        let head = git(root, &["rev-parse", "--verify", "-q", "HEAD"]).map(|s| s.trim().to_string());
        let status = git(root, &["status", "--porcelain"]).unwrap_or_default();
        // `XY path`, or `XY from -> to` for a rename.
        let clean = status.lines().all(|l| l.get(3..).is_some_and(|p| p.split(" -> ").all(super::verdict::inert)));
        Git { head, clean }
    }

    /// Commits `paths` (under `root`) alone, whatever else is changed or
    /// staged: the person's files psx writes for them (a play, goal.toml's
    /// held_out), so the agent finds them committed rather than in its
    /// way. The new commit, none outside a repository or with nothing to
    /// commit, or why git refused.
    pub fn commit_alone(root: &Path, paths: &[&str], message: &str) -> Result<Option<String>, String> {
        let paths: Vec<&str> = paths.iter().copied().filter(|p| root.join(p).exists()).collect();
        if paths.is_empty() || git(root, &["rev-parse", "--is-inside-work-tree"]).is_none() {
            return Ok(None);
        }
        let run = |args: &[&str]| -> Result<std::process::Output, String> {
            // Another git (the agent's) may hold the index a moment.
            for _ in 0..10 {
                let out = std::process::Command::new("git")
                    .args(args)
                    .current_dir(root)
                    .output()
                    .map_err(|e| format!("git: {e}"))?;
                if out.status.success() || !String::from_utf8_lossy(&out.stderr).contains("index.lock") {
                    return Ok(out);
                }
                std::thread::sleep(std::time::Duration::from_millis(300));
            }
            Err("git: the index stayed locked".into())
        };
        let refused = |out: &std::process::Output| String::from_utf8_lossy(&out.stderr).trim().to_string();
        let out = run(&[&["add", "--"][..], &paths].concat())?;
        if !out.status.success() {
            return Err(refused(&out));
        }
        let same = run(&[&["diff", "--cached", "--quiet", "--"][..], &paths].concat())?;
        if same.status.success() {
            return Ok(None);
        }
        let out = run(&[&["commit", "-q", "-m", message, "--only", "--"][..], &paths].concat())?;
        if !out.status.success() {
            return Err(refused(&out));
        }
        Ok(git(root, &["rev-parse", "HEAD"]).map(|s| s.trim().to_string()))
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
    pub fn code(pc: u32, function: u32, ways: u8) -> Branch {
        let never = match ways {
            0 => "ran",
            1 => "fell through",
            2 => "taken",
            _ => "",
        };
        Branch { at: Place::Code { pc, function }, never: never.into(), test: None, behind: None, runs: 0 }
    }

    /// The recordings did all of it.
    pub fn resolved(&self) -> bool {
        self.never.is_empty()
    }

    /// Not all done, and behind no other branch's missed way: one the list
    /// shows.
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

/// Recordings, each with its failures (empty: passed) by the verdict at
/// HEAD; None without one.
pub type Results = Vec<(String, Option<Vec<String>>)>;

/// The last tick of a recording (`<scenario>/<name>`), 0 if it can't be read.
pub(crate) fn last_tick(root: &Path, recording: &str) -> u64 {
    let text = std::fs::read_to_string(root.join(crate::scenario::SCENARIOS).join(format!("{recording}.jsonl")));
    text.ok()
        .and_then(|t| {
            t.lines().rev().find(|l| !l.trim().is_empty()).and_then(|l| serde_json::from_str::<Value>(l).ok())
        })
        .and_then(|v| v["tick"].as_u64())
        .unwrap_or(0)
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

/// Where the port stands.
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
        let (mut branches, mut ran, mut cited) = (Vec::new(), BTreeSet::new(), BTreeSet::new());
        if let Some(inv) = &inv {
            let not_ported = |f: u32| port.not_ported(f).is_none();
            let cb = crate::coverage::code_branches(&ctx, inv, &paths, &not_ported, &port.cited.addresses);
            unrecorded.extend(cb.problems);
            (ran, cited) = (cb.ran, cb.cited);
            branches.extend(cb.branches.into_iter().map(|c| Branch {
                test: c.test,
                behind: c.behind,
                runs: c.runs,
                ..Branch::code(c.pc, c.function, c.ways)
            }));
        }
        let script = &port.cited.stretches;
        if !script.is_empty() && !paths.is_empty() {
            let (items, p) = crate::script_cover::items(&crate::script_cover::merged(&paths), script);
            unrecorded.extend(p);
            branches.extend(items.into_iter().map(|i| Branch {
                at: Place::Script { at: i.at, routine: i.routine },
                never: i.never,
                test: None,
                behind: None,
                runs: 0,
            }));
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

    /// The work recordings that fail, the one differing earliest first.
    pub fn failing(&self) -> Vec<String> {
        match &self.verdict {
            State::Current(v) => v.failing(&self.work.iter().map(|(n, _)| n.clone()).collect::<Vec<_>>()),
            _ => Vec::new(),
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
                    json!({"tests": g[0].never_test(), "branches": g.len(),
                        "runs": g.iter().map(|b| b.runs).sum::<u64>(),
                        "behind": g.iter().map(|b| behind(b)).sum::<usize>(),
                        "first": g.iter().take(3).map(|b| one(b)).collect::<Vec<_>>()})
                }
            })
            .collect();
        let mut v = json!({"total": self.branches.len(), "all_ways": self.branches.len() - unresolved,
               "open_groups": groups.len(), "open": uncovered});
        if uncovered.len() < groups.len() {
            v["more"] = json!(format!("{} more groups: port_status branches", groups.len() - uncovered.len()));
        }
        v
    }

    /// The recordings by state, as status shows them: the person's plays
    /// counted, their results for the person alone (`--plain`).
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
        let mut held_out = json!({"total": self.held_out.len(), "shown": "port status --plain, for the person"});
        // A port judged only on what it was built from cannot show it holds.
        if self.held_out.is_empty() && !self.work.is_empty() {
            held_out["ask"] = json!(
                "none yet: ask for a play of the goal with scenario_play held_out=true, a route your work recordings don't take"
            );
        }
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

    /// The brief's front matter and the parts both briefs share.
    fn front(&self) -> String {
        format!("commit = \"{}\"", self.git.head.as_deref().unwrap_or("HEAD"))
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
    /// How much of the game the work recordings can show: how many, from
    /// how many starting points, and the longest. None without any.
    pub fn reach(&self, root: &Path) -> Option<Value> {
        let names: Vec<String> = self.work.iter().map(|(n, _)| n.clone()).collect();
        let longest = names.iter().max_by_key(|n| last_tick(root, n))?;
        Some(json!({"recordings": names.len(), "starting_points": super::scenarios_of(&names).len(),
                    "longest": longest, "longest_ticks": last_tick(root, longest)}))
    }

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
            "problems": self.port.problems,
            "recordings": self.recordings_json(),
            "verdict": self.verdict.name(),
            "code": code,
            "branches": self.branches_json(false),
            "review": self.review_json(),
            "git": {"head": self.git.head, "clean": self.git.clean},
            "decomp": self.decomp,
        });
        if !self.regressed.is_empty() {
            v["decomp_regressed"] = json!(
                self.regressed
                    .iter()
                    .map(|(f, commit, why)| json!({"function": f, "passed_at": commit, "now": why}))
                    .collect::<Vec<_>>()
            );
        }
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
        let failing = self.failing();
        let n = self.work.len();
        let work = match &self.verdict {
            State::Current(_) => format!("{n} work, {} passing", n - failing.len()),
            State::Missing => format!("{n} work, never checked (port_check)"),
            State::Stale { .. } if self.git.clean => {
                format!("{n} work, not checked since the last change (port_check)")
            }
            State::Stale { .. } => {
                format!("{n} work, not checked since the last change (uncommitted changes; port_check judges a commit)")
            }
        };
        let plays = match &self.verdict {
            State::Current(_) => format!("{} held-out plays, {} passing", self.held_out.len(), passed(&self.held_out)),
            _ => format!("{} held-out plays", self.held_out.len()),
        };
        let plays = if self.held_out.is_empty() {
            format!("{plays} (a play you start in psx web's Play now is held out)")
        } else {
            plays
        };
        s.push_str(&format!("recordings  {work} · {plays}\n"));
        let accounted = self.ran.len() - self.uncited.len();
        let all_ways = self.branches.iter().filter(|b| b.resolved()).count();
        s.push_str(&format!(
            "code  {accounted}/{} functions the recordings ran cited or not ported · branches run both ways {all_ways}/{}\n",
            self.ran.len(),
            self.branches.len()
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
        for r in self.held_out.iter().filter(|(_, f)| f.as_ref().is_some_and(|f| !f.is_empty())).take(3) {
            s.push_str(&format!("  held-out play failing: {}\n", r.0));
        }
        if !self.port.problems.is_empty() {
            s.push_str(&format!("\n{} problems in the goal or catalogue.\n", self.port.problems.len()));
        }
        s
    }
}

/// Where the port stands, a line each time it changes, for `psx web`'s
/// progress over time.
pub const PROGRESS: &str = ".psx/progress.jsonl";

/// Append the port's standing (from its status) to
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

/// The counts the progress line keeps: the verdict, the work recordings,
/// the code accounted for and the branches, problems, the review and the
/// decomp.
fn standing(status: &Value) -> Value {
    let work = &status["recordings"]["work"];
    json!({
        "head": status["git"]["head"],
        "verdict": status["verdict"],
        "branches": {"all_ways": status["branches"]["all_ways"], "total": status["branches"]["total"]},
        "recordings": {"pass": work["pass"], "total": work["total"]},
        "code": {"accounted": status["code"]["ran"].as_u64().unwrap_or(0) - status["code"]["uncited"].as_u64().unwrap_or(0),
                 "ran": status["code"]["ran"]},
        "problems": status["problems"].as_array().map_or(0, Vec::len),
        "review": status["review"]["state"],
        "decomp": status["decomp"],
    })
}

/// The port's status as `port status` gives it, for `psx web`: nothing is
/// written.
pub fn look(root: &Path) -> crate::Result<Value> {
    let f = Facts::gather(root)?;
    Ok(f.status())
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
    /// Draw the work recordings on both sides and score how far the port's
    /// picture is from the original's. Takes minutes; without it the last
    /// scores are repeated.
    #[arg(long)]
    #[serde(default)]
    pub screens: bool,
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
                if let Some(r) = f.reach(&ctx.root) {
                    text.push_str(&format!(
                        "reach  {} recordings from {} starting points, longest {} ticks ({})\n",
                        r["recordings"],
                        r["starting_points"],
                        r["longest_ticks"],
                        r["longest"].as_str().unwrap_or("")
                    ));
                    v["reach"] = r;
                }
                let scores = if self.screens {
                    let work: Vec<String> = f.work.iter().map(|(n, _)| n.clone()).collect();
                    Some(super::screens::score(ctx, &work, f.git.head.as_deref(), f.git.clean)?)
                } else {
                    super::screens::last(&ctx.root)
                };
                if let Some(s) = scores {
                    text.push_str(&super::screens::line(&s));
                    v["screens"] = s;
                }
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
                Ok(if self.plain { json!({"text": text}) } else { v })
            }),
            plain,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::super::tests::workspace;
    use super::*;

    const GOAL: &str = "description = \"x\"\n";

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
            pointers: BTreeMap::new(),
        }
    }

    fn results(rs: &[(&str, &[&str])]) -> Results {
        rs.iter().map(|(n, f)| (n.to_string(), Some(f.iter().map(|l| l.to_string()).collect()))).collect()
    }

    #[test]
    fn reach_counts_starting_points_and_finds_the_longest_recording() {
        let header = "{\"format\":\"scenario-recording\"}\n";
        let (d, mut f) = facts(
            GOAL,
            &[
                ("scenarios/a/short.jsonl", &format!("{header}{{\"tick\":5}}\n")),
                ("scenarios/a/long.jsonl", &format!("{header}{{\"tick\":900,\"values\":{{}}}}\n")),
                ("scenarios/b/other.jsonl", &format!("{header}{{\"tick\":40}}\n")),
            ],
        );
        f.work.clear();
        assert_eq!(f.reach(&d.0), None);
        f.work = ["a/short", "a/long", "b/other"].iter().map(|n| (n.to_string(), None)).collect();
        let r = f.reach(&d.0).unwrap();
        assert_eq!((r["recordings"].as_u64(), r["starting_points"].as_u64()), (Some(3), Some(2)));
        assert_eq!((r["longest"].as_str(), r["longest_ticks"].as_u64()), (Some("a/long"), Some(900)));
    }

    #[test]
    fn recordings_without_a_result_read_as_not_checked_rather_than_failing_none() {
        let (_d, mut f) = facts(GOAL, &[]);
        f.work = vec![("a/b".to_string(), None)];
        let text = f.plain();
        assert!(text.contains("1 work, never checked"), "{text}");
        assert!(!text.contains("failing"), "{text}");
        assert!(f.status()["recordings"]["work"]["fail"].is_null());
    }

    #[test]
    fn a_review_is_due_after_enough_changed_code_and_never_before() {
        let (d, mut f) = facts(GOAL, &[]);
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
    fn a_function_whose_c_passed_and_now_fails_is_shown() {
        let (d, _) = facts(GOAL, &[]);
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
        let v = Facts::gather(&d.0).unwrap().status();
        assert_eq!(v["decomp_regressed"][0]["function"], "step", "{v}");
        last("passed");
        assert!(Facts::gather(&d.0).unwrap().regressed.is_empty());
    }

    #[test]
    fn uncommitted_changes_leave_a_verdict_at_head_stale() {
        let (dir, _) = workspace(GOAL, &[("scenarios/walk/a.jsonl", "")]);
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
        let (d, _) = facts(GOAL, &[]);
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
    fn held_out_plays_are_counted_for_the_agent_and_judged_for_the_person() {
        let (_d, mut f) = facts(GOAL, &[]);
        f.verdict = State::Current(verdict("abc", true, &[("walk/a", &[]), ("walk/play", &["x differs"])]));
        f.work = results(&[("walk/a", &[])]);
        f.held_out = results(&[("walk/play", &["x differs"])]);
        let v = f.status();
        assert_eq!(v["recordings"]["held_out"]["total"], 1);
        assert!(v["recordings"]["held_out"].get("pass").is_none() && !v.to_string().contains("walk/play"), "{v}");
        let text = f.plain();
        assert!(
            text.contains("1 held-out plays, 0 passing") && text.contains("held-out play failing: walk/play"),
            "{text}"
        );
        assert!(v["recordings"]["held_out"].get("ask").is_none());
        // With none, the agent is asked to get one and the person told how.
        f.held_out = Vec::new();
        let v = f.status();
        assert!(v["recordings"]["held_out"]["ask"].as_str().unwrap().contains("scenario_play held_out=true"), "{v}");
        assert!(f.plain().contains("0 held-out plays, 0 passing (a play you start in psx web's Play now is held out)"));
    }
}
