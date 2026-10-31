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
        Branch { at: Place::Code { pc, function }, never: never.into(), wont_reach }
    }

    pub fn resolved(&self) -> bool {
        self.never.is_empty() || self.wont_reach == Some(true)
    }

    /// What no recording did, and where.
    pub fn describe(&self) -> String {
        match &self.at {
            Place::Code { pc, function } => {
                format!("the branch at {} (in {}) never {}", hex(*pc), hex(*function), self.never)
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
            Place::Code { pc, function } => json!({"pc": hex(*pc), "function": hex(*function), "never": self.never}),
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
    /// The code's review holds (one review covers every feature).
    pub code_reviewed: bool,
    /// The disc's assets its entry names, as the inventory has them.
    pub assets: assets::Needs,
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
            self.unrecorded.is_empty(),
            self.matching(),
            self.branches_resolved() && self.questions.is_empty() && self.code_reviewed && self.assets.settled(),
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
            return json!({"total": self.held_out.len(), "shown": "once the code is reviewed"});
        }
        let pass = self.held_out.iter().filter(|(_, f)| f.as_ref().is_some_and(Vec::is_empty)).count();
        let mut v = json!({"total": self.held_out.len(), "pass": pass});
        if let Some((r, first)) = earliest(&self.held_out) {
            v["first_failure"] = json!(format!("{r}: {first}"));
        }
        v
    }

    /// The branches, with the first five unresolved, or all of them.
    fn branches_json(&self, all: bool) -> Value {
        let unresolved = self.branches.iter().filter(|b| !b.resolved()).count();
        let uncovered: Vec<Value> = self
            .branches
            .iter()
            .filter(|b| !b.resolved())
            .take(if all { usize::MAX } else { 5 })
            .map(|b| {
                let mut v = b.json();
                if b.wont_reach == Some(false) {
                    v["wont_reach"] = json!("not confirmed by the person");
                }
                v
            })
            .collect();
        let mut v = json!({"total": self.branches.len(), "resolved": self.branches.len() - unresolved,
               "unresolved": uncovered});
        if uncovered.len() < unresolved {
            v["more"] = json!(format!("{} more: port_status feature {}", unresolved - uncovered.len(), self.id));
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
            parts.push(if r == self.branches.len() {
                format!("branches {r}/{r} resolved")
            } else {
                format!("{r}/{} branches", self.branches.len())
            });
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
    /// The features it lists as finished when it was written.
    pub review_features: Vec<String>,
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
        let features: Vec<Feature> = port
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
                        code_reviewed: false,
                        assets: Default::default(),
                        id,
                    };
                };
                let mut unrecorded = if e.recordings.work.is_empty() {
                    vec![format!("{}: names no work recordings (recordings.work: <scenario>/<name>)", e.file)]
                } else {
                    crate::scenario::feature_unready(root, e)
                };
                let mut branches = Vec::new();
                let paths: Vec<PathBuf> = e
                    .recordings
                    .all()
                    .filter_map(|r| r.split_once('/'))
                    .map(|(s, n)| root.join("scenarios").join(s).join(format!("{n}.jsonl")))
                    .collect();
                let mine = confirmed.get(&id);
                // The person confirmed the key as it stands.
                let agreed = |k: &String| mine.and_then(|m| m.get(k)) == e.wont_reach.get(k);
                let code: Vec<u32> = e.code.iter().filter_map(|c| super::entry_addr(c)).collect();
                if let (Some(inv), false, false) = (&inv, code.is_empty(), e.recordings.work.is_empty()) {
                    let fb = crate::coverage::feature_branches(&ctx, inv, &paths, &code);
                    unrecorded.extend(fb.problems);
                    branches.extend(fb.branches.into_iter().map(|(pc, function, ways)| {
                        let key = e.wont_reach.keys().find(|k| super::entry_addr(k) == Some(pc));
                        Branch::code(pc, function, ways, key.map(agreed))
                    }));
                }
                let script: Vec<Stretch> = e.code.iter().filter_map(|c| Stretch::parse(c)).collect();
                if !script.is_empty() && !e.recordings.work.is_empty() {
                    let (items, problems) = crate::script_cover::items(&crate::script_cover::merged(&paths), &script);
                    unrecorded.extend(problems);
                    branches.extend(items.into_iter().map(|i| {
                        let key = e.wont_reach.keys().find(|k| Stretch::parse(k).is_some_and(|w| w.contains(&i.at)));
                        Branch {
                            at: Place::Script { at: i.at, routine: i.routine },
                            never: i.never,
                            wont_reach: key.map(agreed),
                        }
                    }));
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
                    code_reviewed: review.holds(),
                    assets: needs(&e.assets, false, &e.file, &mut problems),
                    id,
                }
            })
            .collect();
        port.problems.extend(problems);
        if let Ok(a) = &assets {
            port.problems.extend(a.problems.iter().cloned());
        }
        let review_features = review::features(root);
        Ok(Facts { port, git, features, unpack, assets, needs: goal_needs, review, review_features })
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
        // A feature finished since the last review: a review of the
        // change, so each reads about one feature's work; findings are
        // answered as they come.
        let unreviewed: Vec<&str> = self
            .features
            .iter()
            .filter(|f| f.finished() && !self.review_features.contains(&f.id))
            .map(|f| f.id.as_str())
            .collect();
        if matches!(self.review, Review::Open { .. } | Review::Invalid(_))
            || (!unreviewed.is_empty() && !self.review.holds())
        {
            return self.review_item(format!("{} finished its own work since the last review", unreviewed.join(", ")));
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
                "declare the quantities in glossary/ and bind them and the tick (the logic step) in glossary/binding/, write scenarios/<id>.toml from a psx states chain whose last run ends at a game moment (until), run psx scenario record, name the recording under recordings.work in the catalogue entry, then psx scenario check (psx guide, Recordings)",
            );
        }
        if let Some(f) = find(&|f| at(f, Stage::Recorded) && !matches!(f.verdict, State::Current(_))) {
            let (why, how) = match &f.verdict {
                State::Stale { commit } => (
                    format!(
                        "its verdict is from {commit}, not HEAD with no uncommitted changes (notes and the review aside)"
                    ),
                    "commit the changes and run psx port check",
                ),
                _ => (
                    "no psx port check has judged it".into(),
                    "write the feature from the original's code (citing each rule), register its scenario launchers and what it observes (psx_bevy::scenario), commit, and run psx port check",
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
                format!("{} in its recordings; {n} branches of its code are unresolved", b.describe()),
                "every branch of its code is taken both ways by its recordings, or listed under wont_reach",
                "read the branch's condition (code disasm; for a script's stretch or decision, your own reading of the program) and work out what reaches the other way in game terms; record that (psx scenario record) and name it under recordings, or when it cannot be scripted ask the person to play it (psx scenario play); when no recording can reach it, list it under wont_reach with why; a script decision no hook named needs a hook in the glossary binding that names its way, then the recordings made again",
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
        if let Some(f) = find(&|f| f.reviewed() && f.held_out.is_empty()) {
            return Item::person(
                "Play",
                Some(&f.id),
                "the code is reviewed and it has no held-out recordings".into(),
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

    /// The features whose own work is finished now.
    fn finished_ids(&self) -> Vec<String> {
        self.features.iter().filter(|f| f.finished()).map(|f| f.id.clone()).collect()
    }

    /// The review item due, `why` it is due when the review is missing or
    /// stale.
    fn review_item(&self, why: String) -> Item {
        let done = "the review holds: the code (game/, tools/) unchanged since its commit, every finding answered, its features the ones finished";
        match &self.review {
            Review::Open { unanswered } => Item::agent(
                "Review",
                None,
                format!("findings without an answer: {}", first_lines(unanswered).join("; ")),
                done,
                &format!(
                    "answer each finding in {} with an **Answer:** line: done (restructure as it asks, keeping what the code does: psx port check must pass again), or kept and why; changed code has a follow-up review when the next feature finishes",
                    review::REVIEW
                ),
            ),
            Review::Invalid(why) => Item::agent(
                "Review",
                None,
                format!("{} is not a review: {why}", review::REVIEW),
                done,
                "write the review's front matter as the brief says: +++, commit, features, +++",
            ),
            Review::Stale { commit, changed } => Item::agent(
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
            "Give this brief to a reviewer who has not seen the work (a sub-agent or a new session): review the port's code as a whole, {code} (git ls-files {code}). Its recordings already check that it does what the original does, so this review does not: leave the catalogue, the notes and the original's code unread. Ask instead: would you write a new game like this? Judge it as code written for a new game today, not as a port: how it is divided (modules, plugins, systems, components, data), names, how readily it takes more (another character, level or rule), duplication, and what is shaped by the original rather than chosen. AGENTS.md's \"The code\" says what the tools rely on. Write {review}: front matter between +++ lines with commit = \"{head}\" and features = {features}, then one ## heading per finding: what you would change, the shape you would choose instead and why; what the code does stays as it is. The writer then answers each finding with an **Answer:** line.",
            code = review::CODE.join(" "),
            review = review::REVIEW,
            head = self.git.head.as_deref().unwrap_or("HEAD"),
            features = toml::Value::from(self.finished_ids()),
        )
    }

    /// The brief for a review of what changed since the review at `commit`:
    /// what that review read is not read again.
    fn follow_up(&self, commit: &str) -> String {
        format!(
            "Give this brief to a reviewer who did not write the changes (a sub-agent or a new session): a follow-up review of the port's code. The review at {commit} read the code then; read what changed since: git diff {commit} -- {code}, with the findings and answers in {review} (are the changes what the answers say?), and enough of the code around it to judge how the change fits the whole (how the code is divided, what a new feature reuses or duplicates). Ask what the review asked: would you write a new game like this? If the changes rewrite most of the code, review it whole. Leave the catalogue, the notes and the original's code unread: the recordings check what the code does. Keep the findings and answers already in {review}; set its commit to \"{head}\" and its features to {features}, and add one ## heading per new finding: what you would change, the shape you would choose instead and why; with none, the commit is all that changes. The writer then answers each new finding with an **Answer:** line.",
            code = review::CODE.join(" "),
            review = review::REVIEW,
            head = self.git.head.as_deref().unwrap_or("HEAD"),
            features = toml::Value::from(self.finished_ids()),
        )
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
            if !f.unbuilt.is_empty() {
                row["unbuilt"] = json!(f.unbuilt);
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
            Review::Stale { commit, changed } => {
                format!("code reviewed at {commit}, changed since in {} files\n", changed.len())
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
            if let Some(b) = f.branches.iter().find(|b| !b.resolved()) {
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
        Outcome::result_text(Facts::gather(&ctx.root).and_then(|f| {
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
            Ok(if self.plain { json!({"text": f.plain()}) } else { v })
        }), plain)
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
        // F-0001's own work is finished: a review of it comes before more work.
        let item = f.next();
        assert_eq!(item.item, "Review");
        assert_eq!(item.why, "F-0001 finished its own work since the last review, and the code has no review");
        assert!(item.how.contains("features = [\"F-0001\"]"), "{}", item.how);
        // Once a review lists them, the review waits for the next feature to finish, or the goal.
        f.review_features = vec!["F-0001".into(), "F-0002".into()];
        let item = f.next();
        assert_eq!((item.item, item.feature.as_deref()), ("Fix", Some("F-0002")));
        assert_eq!(item.why, "walk/c fails: tick 4 y: expected 3, got 2");
        assert_eq!(f.features[1].held_out_json()["shown"], "once the code is reviewed");
        set(&mut f, "F-0002", |x| x.work = results(&[("walk/b", &[])]));
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
            (5, &json!("2 more: port_status feature F-0001"))
        );
        assert_eq!(f.features[0].branches_json(true)["unresolved"].as_array().unwrap().len(), 7);
        set(&mut f, "F-0001", |x| x.branches = vec![branch(None)]);
        assert_eq!(next(&f), ("Cover", "agent", "F-0001".into()));
        assert!(f.next().why.starts_with("the branch at 0x8002D820 (in 0x8002D810) never fell through"));
        set(&mut f, "F-0001", |x| x.branches = vec![branch(Some(false))]);
        assert_eq!(next(&f), ("Confirm", "person", "F-0001".into()));
        let script = Branch {
            at: Place::Script { at: Stretch::parse("croc+0x3..0x8").unwrap(), routine: None },
            never: "read".into(),
            wont_reach: None,
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
        review(&mut f, Review::Stale { commit: "abc".into(), changed: vec!["game/src/jump.rs".into()] });
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
    fn a_review_comes_each_time_a_feature_finishes() {
        let (_d, mut f) = facts(CONFIRMED, &["F-0001", "F-0002"]);
        finished(&mut f, vec![]);
        // F-0002 still has work: a branch its recordings never took.
        set(&mut f, "F-0002", |x| x.branches = vec![Branch::code(0x8002_D820, 0x8002_D810, 1, None)]);
        let next = |f: &Facts| (f.next().item, f.next().why, f.next().how);
        // A review that holds at HEAD saw the finished code: nothing to review.
        review(&mut f, Review::Holds { commit: "abc".into(), findings: 0 });
        assert_eq!(next(&f).0, "Cover");
        // Changed since, and F-0001 not listed: a follow-up of the change.
        review(&mut f, Review::Stale { commit: "abc".into(), changed: vec!["game/src/run.rs".into()] });
        let (item, why, how) = next(&f);
        assert_eq!(item, "Review");
        assert!(why.starts_with("F-0001 finished its own work since the last review, and the code changed"), "{why}");
        assert!(how.contains("git diff abc") && how.contains("its features to [\"F-0001\"]"), "{how}");
        // Listed: building F-0002 changes the code without a review each time.
        f.review_features = vec!["F-0001".into()];
        assert_eq!(next(&f).0, "Cover");
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
