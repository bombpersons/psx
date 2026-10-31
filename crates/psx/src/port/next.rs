//! What a port's goal needs next (docs/port-design.md, section 5): the facts, read once
//! per call; the stages they give each feature in the goal's closure; and
//! the items, each due on a fact and done on a fact the work leaves behind.
//! `port status` shows the facts, `port next` gives the one item due.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use super::check;
use super::review::{self, Review};
use super::verdict::{self, State};
use super::{Confirmation, Port, Stage};
use crate::Error;
use crate::commands::{Ctx, Op, Outcome};

/// The workspace's commit.
#[derive(Debug, Clone, Default)]
pub struct Git {
    /// None before the first commit, or outside a repository.
    pub head: Option<String>,
    /// No uncommitted changes.
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
        Git { head, clean: status.trim().is_empty(), changes }
    }
}

/// A conditional branch of a feature's code, as its recordings ran it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Branch {
    pub pc: u32,
    /// The entry of the function holding it.
    pub function: u32,
    /// The ways its recordings took: 1 taken, 2 fell through.
    pub ways: u8,
    /// Listed under wont_reach: whether the person confirmed it as it stands.
    pub wont_reach: Option<bool>,
}

impl Branch {
    pub fn resolved(&self) -> bool {
        self.ways == 3 || self.wont_reach == Some(true)
    }

    /// What no recording did there.
    pub fn never(&self) -> &'static str {
        match self.ways {
            0 => "ran",
            1 => "fell through",
            2 => "taken",
            _ => "",
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
    pub verdict: State,
    /// Launchers and observers the port lacks for its scenarios, by the
    /// last check at HEAD.
    pub unbuilt: Vec<String>,
    /// Each recording with its failures (empty: passed), by the verdict at
    /// HEAD; None without one.
    pub work: Vec<(String, Option<Vec<String>>)>,
    pub held_out: Vec<(String, Option<Vec<String>>)>,
    pub branches: Vec<Branch>,
    pub questions: Vec<String>,
    pub review: Review,
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
            self.unrecorded.is_empty(),
            self.matching(),
            self.branches_resolved() && self.questions.is_empty() && self.review.holds(),
            all_pass(&self.held_out),
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
            return json!({"total": self.held_out.len(), "shown": "once the feature is reviewed"});
        }
        let pass = self.held_out.iter().filter(|(_, f)| f.as_ref().is_some_and(Vec::is_empty)).count();
        let mut v = json!({"total": self.held_out.len(), "pass": pass});
        if let Some((r, first)) = earliest(&self.held_out) {
            v["first_failure"] = json!(format!("{r}: {first}"));
        }
        v
    }

    fn branches_json(&self) -> Value {
        let uncovered: Vec<Value> = self
            .branches
            .iter()
            .filter(|b| !b.resolved())
            .take(5)
            .map(|b| {
                let mut v = json!({"pc": hex(b.pc), "function": hex(b.function), "never": b.never()});
                if b.wont_reach == Some(false) {
                    v["wont_reach"] = json!("not confirmed by the person");
                }
                v
            })
            .collect();
        json!({"total": self.branches.len(), "resolved": self.branches.iter().filter(|b| b.resolved()).count(),
               "unresolved": uncovered})
    }

    fn review_json(&self) -> Value {
        match &self.review {
            Review::Missing => json!({"state": "missing"}),
            Review::Invalid(why) => json!({"state": "invalid", "why": why}),
            Review::Stale { commit, changed } => json!({"state": "stale", "commit": commit, "changed": changed}),
            Review::Open { unanswered } => json!({"state": "open", "unanswered": unanswered}),
            Review::Holds { commit, findings } => json!({"state": "holds", "commit": commit, "findings": findings}),
        }
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
            parts.push(if r == self.branches.len() {
                format!("branches {r}/{r} resolved")
            } else {
                format!("{r}/{} branches", self.branches.len())
            });
        }
        if self.matching() && !self.review.holds() {
            parts.push(format!("review {}", self.review.name()));
        }
        parts.join(" · ")
    }
}

/// Up to three lines, and how many more there are.
fn first_lines(lines: &[String]) -> Vec<String> {
    let mut out: Vec<String> = lines.iter().take(3).cloned().collect();
    if lines.len() > 3 {
        out.push(format!("and {} more", lines.len() - 3));
    }
    out
}

/// Everything the items are decided on.
pub struct Facts {
    pub root: PathBuf,
    pub port: Port,
    pub git: Git,
    /// The closure in dependency order.
    pub features: Vec<Feature>,
    /// Overlays the inventory knows only from the code that ran in them.
    pub unpack: Vec<String>,
}

impl Facts {
    pub fn gather(root: &Path) -> crate::Result<Facts> {
        let port = Port::load(root)?;
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
        let features = port
            .closure()
            .into_iter()
            .map(|id| {
                let Some(e) = port.entries.get(&id) else {
                    return Feature {
                        in_goal: goal.contains(&id),
                        catalogued: false,
                        located: false,
                        unrecorded: Vec::new(),
                        verdict: State::Missing,
                        unbuilt: Vec::new(),
                        work: Vec::new(),
                        held_out: Vec::new(),
                        branches: Vec::new(),
                        questions: Vec::new(),
                        review: Review::Missing,
                        id,
                    };
                };
                let mut unrecorded = if e.recordings.work.is_empty() {
                    vec![format!("{}: names no work recordings (recordings.work: <scenario>/<name>)", e.file)]
                } else {
                    crate::scenario::feature_unready(root, e)
                };
                let mut branches = Vec::new();
                if let (Some(inv), false, false) = (&inv, e.code.is_empty(), e.recordings.work.is_empty()) {
                    let paths: Vec<PathBuf> = e
                        .recordings
                        .all()
                        .filter_map(|r| r.split_once('/'))
                        .map(|(s, n)| root.join("scenarios").join(s).join(format!("{n}.jsonl")))
                        .collect();
                    let code: Vec<u32> = e.code.iter().filter_map(|c| super::entry_addr(c)).collect();
                    let fb = crate::coverage::feature_branches(&ctx, inv, &paths, &code);
                    unrecorded.extend(fb.problems);
                    let mine = confirmed.get(&id);
                    branches = fb
                        .branches
                        .into_iter()
                        .map(|(pc, function, ways)| {
                            let key = e.wont_reach.keys().find(|k| super::entry_addr(k) == Some(pc));
                            let wont_reach = key.map(|k| mine.and_then(|m| m.get(k)) == e.wont_reach.get(k));
                            Branch { pc, function, ways, wont_reach }
                        })
                        .collect();
                }
                let verdict = verdict::state(root, &id, head);
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
                    verdict,
                    unbuilt,
                    work,
                    held_out,
                    branches,
                    questions: e.questions.clone(),
                    review: review::read(root, &id),
                    id,
                }
            })
            .collect();
        Ok(Facts { root: root.to_path_buf(), port, git, features, unpack })
    }

    pub fn stage(&self, id: &str) -> Option<Stage> {
        self.features.iter().find(|f| f.id == id).and_then(Feature::stage)
    }

    /// Every feature of the closure done.
    pub fn complete(&self) -> bool {
        self.port.confirmation() != Confirmation::Missing
            && !self.features.is_empty()
            && self.features.iter().all(|f| f.stage() == Some(Stage::Done))
    }

    fn title(&self, id: &str) -> String {
        self.port.entries.get(id).map(|e| e.title.clone()).unwrap_or_default()
    }

    /// The item due now, in the order of docs/port-design.md, section 5.
    pub fn next(&self) -> Item {
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
                        "features the person confirmed are no longer in the goal or needed by it: {}",
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
        // 1. The port cannot replay a recording.
        if let Some(f) = find(&|f| at(f, Stage::Recorded) && !f.unbuilt.is_empty()) {
            return Item::agent(
                "Build",
                Some(&f.id),
                format!("the port cannot replay its recordings: {}", first_lines(&f.unbuilt).join("; ")),
                "the last psx port check at HEAD finds a launcher for each of its scenarios' kinds and each quantity they compare observed",
                "register the scenario kind's launcher, building the world from the scenario's parameters with the port's own loading, and observe the compared quantities (psx_bevy::scenario); commit and run psx port check",
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
        if let Some(f) = find(&|f| f.located && !f.unrecorded.is_empty()) {
            return Item::agent(
                "Record",
                Some(&f.id),
                format!("its recordings are not ready: {}", first_lines(&f.unrecorded).join("; ")),
                "it names work recordings, each scenario of its recordings passes psx scenario check, and their coverage counts",
                "declare the quantities in glossary/ and bind them and the tick (the logic step) in glossary/binding/, write scenarios/<id>.toml from a psx states chain whose last run ends at a game moment (until), run psx scenario record, name the recording under recordings.work in the catalogue entry, then psx scenario check (docs/scenarios.md)",
            );
        }
        if let Some(f) = find(&|f| at(f, Stage::Recorded) && !matches!(f.verdict, State::Current(_))) {
            let why = match &f.verdict {
                State::Stale { commit } => {
                    format!("its verdict is from {commit}, not HEAD with no uncommitted changes")
                }
                _ => "no psx port check has judged it".into(),
            };
            return Item::agent(
                "Implement",
                Some(&f.id),
                why,
                "psx port check has judged it at HEAD",
                "write the feature from the original's code (citing each rule), register its scenario launchers and what it observes (psx_bevy::scenario), commit, and run psx port check",
            );
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
        let uncovered = |f: &Feature| f.branches.iter().find(|b| !b.resolved() && b.wont_reach.is_none()).cloned();
        if let Some((f, b)) = self.features.iter().find_map(|f| Some((f, uncovered(f)?))) {
            let n = f.branches.iter().filter(|b| !b.resolved()).count();
            return Item::agent(
                "Cover",
                Some(&f.id),
                format!(
                    "the branch at {} (in {}) never {} in its recordings; {n} branches of its code are unresolved",
                    hex(b.pc),
                    hex(b.function),
                    b.never()
                ),
                "every branch of its code is taken both ways by its recordings, or listed under wont_reach",
                "read the branch's condition (code disasm) and work out what reaches the other way in game terms; record that (psx scenario record) and name it under recordings, or when it cannot be scripted ask the person to play it (psx scenario play); when no recording can reach it, list it under wont_reach with why",
            );
        }
        if let Some(f) = find(&|f| f.branches.iter().any(|b| !b.resolved())) {
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
        // 6. The review.
        if let Some(f) = find(&|f| f.matching() && f.branches_resolved() && f.questions.is_empty() && !f.review.holds())
        {
            return match &f.review {
                Review::Open { unanswered } => Item::agent(
                    "Review",
                    Some(&f.id),
                    format!("findings without an answer: {}", first_lines(unanswered).join("; ")),
                    "every finding in its review has an answer and its files are unchanged since the review's commit",
                    &format!(
                        "answer each finding in reviews/{}.md with an **Answer:** line: kept and why, or fixed; a fix changes the files, so the review is then due again",
                        f.id
                    ),
                ),
                Review::Invalid(why) => Item::agent(
                    "Review",
                    Some(&f.id),
                    format!("reviews/{}.md is not a review: {why}", f.id),
                    "its review holds",
                    "write the review's front matter as the brief says: +++, commit and files, +++",
                ),
                r => Item::agent(
                    "Review",
                    Some(&f.id),
                    match r {
                        Review::Stale { changed, .. } => {
                            format!("its files changed since the review: {}", first_lines(changed).join(", "))
                        }
                        _ => "its own work is finished and its code has no review".into(),
                    },
                    "its review holds: its files unchanged since the review's commit, every finding answered",
                    &self.brief(f),
                ),
            };
        }
        // 7. Held-out recordings, played by the person.
        if let Some(f) = find(&|f| f.reviewed() && f.held_out.is_empty()) {
            return Item::person(
                "Play",
                Some(&f.id),
                "it is reviewed and has no held-out recordings".into(),
                "its entry names held-out recordings (recordings.held_out)",
                "the agent starts psx scenario play on one of its scenarios with a request to play the feature freely and differently from its work recordings; the person plays, and the agent names the recording under recordings.held_out",
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

    /// The Review item's brief, for a reviewer who has not seen the work.
    fn brief(&self, f: &Feature) -> String {
        let files = review::touched(&self.root, &f.id);
        let files = if files.is_empty() {
            format!(
                "no commit names {} yet: name the feature's id in its commits' messages, then ask port next again",
                f.id
            )
        } else {
            format!("the files its commits touched: {}", files.join(", "))
        };
        format!(
            "Give this brief to a reviewer who has not seen the work (a sub-agent or a new session): Review {id} ({title}). Read the code as a stranger who must maintain or reuse it: the catalogue entry catalogue/{id}.md, {files}, and what that code uses; not the disassembly. Judge it against AGENTS.md's \"The code\", not against the original (the recordings check that). Write reviews/{id}.md: front matter between +++ lines with commit = \"{head}\" and files = [the files read], then one ## heading per finding, each saying what is wrong and why, not the fix. The writer then answers each finding with an **Answer:** line.",
            id = f.id,
            title = self.title(&f.id),
            head = self.git.head.as_deref().unwrap_or("HEAD"),
        )
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
                "work": f.work_json(), "held_out": f.held_out_json(), "branches": f.branches_json(),
                "questions": f.questions, "review": f.review_json(),
            });
            if !f.unrecorded.is_empty() && f.located {
                row["unrecorded"] = json!(f.unrecorded);
            }
            if !f.unbuilt.is_empty() {
                row["unbuilt"] = json!(f.unbuilt);
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
            "complete": self.complete(),
            "git": {"head": self.git.head, "clean": self.git.clean},
            "not_measured_yet": ["assets"],
        });
        if !self.unpack.is_empty() {
            v["unpack"] = json!(self.unpack);
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
        s.push_str(&match self.port.confirmation() {
            Confirmation::Missing => "not confirmed yet\n".to_string(),
            Confirmation::Covers { at } => format!("confirmed {at}\n"),
            Confirmation::Shrunk { at, missing } => {
                format!("confirmed {at}, but {} is no longer in the goal or needed by it\n", missing.join(", "))
            }
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
            if let Some(b) = f.branches.iter().find(|b| !b.resolved()) {
                s.push_str(&format!("{indent}uncovered: {} never {} (in {})\n", hex(b.pc), b.never(), hex(b.function)));
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
    /// Text for the person instead of JSON.
    #[arg(long)]
    #[serde(default)]
    pub plain: bool,
}

impl Op for PortStatusArgs {
    fn exec(self, ctx: &Ctx) -> Outcome {
        Outcome::result(Facts::gather(&ctx.root).map(|f| {
            let mut v = f.status();
            v["next"] = f.next().to_json();
            if self.plain { json!({"text": f.plain()}) } else { v }
        }))
    }
}

#[derive(clap::Args, Clone, Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct PortNextArgs {}

impl Op for PortNextArgs {
    fn exec(self, ctx: &Ctx) -> Outcome {
        Outcome::result(Facts::gather(&ctx.root).and_then(|f| guarded(&ctx.root, &f, f.next())))
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
        let item = f.next();
        assert_eq!((item.item, item.feature.as_deref()), ("Fix", Some("F-0002")));
        assert_eq!(item.why, "walk/c fails: tick 4 y: expected 3, got 2");
        assert_eq!(f.features[1].held_out_json()["shown"], "once the feature is reviewed");
        set(&mut f, "F-0002", |x| x.work = results(&[("walk/b", &[])]));
        set(&mut f, "F-0001", |x| x.unbuilt = vec!["walk: no launcher for scenario kind level".into()]);
        assert_eq!(next(&f), ("Build", "agent", "F-0001".into()));
        set(&mut f, "F-0001", |x| x.unbuilt.clear());
        set(&mut f, "F-0002", |x| x.questions = vec!["does it snap while rising?".into()]);
        assert_eq!(next(&f), ("Answer", "agent", "F-0002".into()));
        set(&mut f, "F-0002", |x| x.questions.clear());
        let branch = |wont_reach| Branch { pc: 0x8002_D820, function: 0x8002_D810, ways: 1, wont_reach };
        set(&mut f, "F-0001", |x| x.branches = vec![branch(None)]);
        assert_eq!(next(&f), ("Cover", "agent", "F-0001".into()));
        assert!(f.next().why.starts_with("the branch at 0x8002D820 (in 0x8002D810) never fell through"));
        set(&mut f, "F-0001", |x| x.branches = vec![branch(Some(false))]);
        assert_eq!(next(&f), ("Confirm", "person", "F-0001".into()));
        set(&mut f, "F-0001", |x| x.branches = vec![branch(Some(true))]);
        assert_eq!(next(&f), ("Review", "agent", "F-0001".into()));
        assert!(f.next().how.contains("Review F-0001 (Feature F-0001)"), "{}", f.next().how);
        set(&mut f, "F-0001", |x| x.review = Review::Open { unanswered: vec!["Stomp is a bool".into()] });
        assert!(f.next().why.contains("Stomp is a bool"));
        for id in ["F-0001", "F-0002"] {
            set(&mut f, id, |x| x.review = Review::Holds { commit: "abc".into(), findings: 1 });
        }
        assert_eq!(f.stage("F-0001"), Some(Stage::Reviewed));
        assert_eq!(next(&f), ("Fix", "agent", "F-0002".into()), "reviewed: its held-out failure shows");
        set(&mut f, "F-0002", |x| x.held_out = results(&[("walk/h", &[])]));
        assert_eq!(next(&f), ("Play", "person", "F-0001".into()));
        set(&mut f, "F-0001", |x| x.held_out = results(&[("walk/i", &[])]));
        assert!(f.complete());
        assert_eq!(next(&f), ("Done", "person", String::new()));
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
