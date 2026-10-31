//! A port: one workspace where one agent investigates the original and
//! builds the port in Bevy (docs/port-design.md). `port init` writes it.
//! The goal (`goal.toml`) says which features the person wants, the
//! catalogue (`catalogue/F-NNNN.md`) describes the game's features in game
//! terms with their dependencies, and `port confirm` records the person's
//! approval of the goal's list. The goal's features and everything they
//! depend on are its closure; each has a stage.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use serde::Deserialize;
use serde_json::{Value, json};

use crate::Error;

pub mod assets;
pub mod check;
mod confirm;
mod init;
pub mod next;
pub mod review;
pub mod verdict;

pub use check::PortCheckArgs;
pub use confirm::PortConfirmArgs;
pub use init::PortInitArgs;
pub use next::{PortNextArgs, PortStatusArgs};

pub const GOAL: &str = "goal.toml";
pub const CATALOGUE: &str = "catalogue";

pub(crate) const GOAL_TOML: &str = "\
# What the person wants from this port. The agent proposes the features
# (catalogue/F-NNNN.md) and lists them here; the person confirms the list
# with `psx port confirm --yes`, which writes `confirmed`. The list may grow
# after that; taking a feature out needs the person to confirm again.
description = \"\"
features = []   # feature ids, or \"all\"
assets = false  # true: every disc file imported and checked, or unused (formats/README.md)
";

/// The features a goal names: a list, or every catalogued feature.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(try_from = "FeaturesToml")]
pub enum Features {
    All,
    List(Vec<String>),
}

#[derive(Deserialize)]
#[serde(untagged)]
enum FeaturesToml {
    Word(String),
    List(Vec<String>),
}

impl TryFrom<FeaturesToml> for Features {
    type Error = String;
    fn try_from(f: FeaturesToml) -> Result<Self, String> {
        match f {
            FeaturesToml::Word(w) if w == "all" => Ok(Features::All),
            FeaturesToml::Word(w) => Err(format!("features is a list of feature ids or \"all\", not {w:?}")),
            FeaturesToml::List(l) => Ok(Features::List(l)),
        }
    }
}

impl Default for Features {
    fn default() -> Self {
        Features::List(Vec::new())
    }
}

impl Features {
    pub(crate) fn to_json(&self) -> Value {
        match self {
            Features::All => json!("all"),
            Features::List(l) => json!(l),
        }
    }
    pub(crate) fn to_toml(&self) -> String {
        match self {
            Features::All => "\"all\"".into(),
            Features::List(l) => toml::Value::from(l.clone()).to_string(),
        }
    }
}

/// `goal.toml`.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Goal {
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub features: Features,
    /// Whether the goal includes the disc's assets: every file imported and
    /// checked, or unused.
    #[serde(default)]
    pub assets: bool,
    #[serde(default)]
    pub confirmed: Option<Confirmed>,
}

/// The person's confirmation: when, the list they confirmed, and the
/// branches they agreed no recording can reach (feature, address, reason).
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Confirmed {
    pub at: String,
    pub features: Features,
    #[serde(default)]
    pub assets: bool,
    #[serde(default)]
    pub wont_reach: WontReach,
}

/// Branches no recording can reach, by feature, then address, with why.
pub type WontReach = BTreeMap<String, BTreeMap<String, String>>;

/// One catalogue entry: `catalogue/F-NNNN.md`, its TOML front matter
/// between `+++` lines, then a paragraph in game terms.
#[derive(Debug, Clone, Default, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Entry {
    pub id: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub area: String,
    #[serde(default)]
    pub depends: Vec<String>,
    /// The original's functions that are the feature, by entry address:
    /// found with psx (coverage diff, watches), each one the inventory has.
    #[serde(default)]
    pub code: Vec<String>,
    #[serde(default)]
    pub recordings: Recordings,
    /// Branches of its code no recording can reach, by address, and why;
    /// the person confirms them (`port confirm`).
    #[serde(default)]
    pub wont_reach: BTreeMap<String, String>,
    /// What is not known yet about the feature.
    #[serde(default)]
    pub questions: Vec<String>,
    /// The paragraph after the front matter, in game terms.
    #[serde(skip)]
    pub description: String,
    /// The file, relative to the workspace.
    #[serde(skip)]
    pub file: String,
}

/// The recordings a feature is checked against, as `<scenario>/<name>`.
#[derive(Debug, Clone, Default, Deserialize, serde::Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Recordings {
    /// Checked as the feature is built.
    #[serde(default)]
    pub work: Vec<String>,
    /// Checked only once the agent says the feature matches.
    #[serde(default)]
    pub held_out: Vec<String>,
}

impl Recordings {
    pub fn all(&self) -> impl Iterator<Item = &String> {
        self.work.iter().chain(&self.held_out)
    }
}

impl Entry {
    /// The scenarios its recordings come from, in the order first named.
    pub fn scenarios(&self) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for r in self.recordings.all() {
            let s = r.split_once('/').map_or(r.as_str(), |(s, _)| s);
            if !out.iter().any(|o| o == s) {
                out.push(s.to_string());
            }
        }
        out
    }
}

/// An address in an entry: `0x` and up to eight hex digits.
pub fn entry_addr(s: &str) -> Option<u32> {
    let h = s.strip_prefix("0x")?;
    (!h.is_empty() && h.len() <= 8).then(|| u32::from_str_radix(h, 16).ok()).flatten()
}

/// A feature's stage (docs/port-design.md, section 5): each holds when its
/// fact, read from the workspace's files, holds and so do the ones before it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Stage {
    /// It has a catalogue entry.
    Catalogued,
    /// Its entry names its code.
    Located,
    /// It names work recordings, and their scenarios pass the determinism check.
    Recorded,
    /// Every work recording passes at HEAD.
    Matching,
    /// Every branch of its code covered or confirmed won't-reach, no question
    /// open, and a review that holds.
    Reviewed,
    /// Its held-out recordings pass.
    Done,
}

impl Stage {
    pub const ALL: [Stage; 6] =
        [Stage::Catalogued, Stage::Located, Stage::Recorded, Stage::Matching, Stage::Reviewed, Stage::Done];

    pub fn name(self) -> &'static str {
        match self {
            Stage::Catalogued => "catalogued",
            Stage::Located => "located",
            Stage::Recorded => "recorded",
            Stage::Matching => "matching",
            Stage::Reviewed => "reviewed",
            Stage::Done => "done",
        }
    }
}

/// Whether the person's confirmation covers the goal's list as it is now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Confirmation {
    /// Never confirmed.
    Missing,
    /// Every confirmed feature is still in the goal.
    Covers { at: String },
    /// Features the person confirmed are no longer in the goal's closure.
    Shrunk { at: String, missing: Vec<String> },
}

/// A problem in the goal or the catalogue, for the agent to fix.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Problem {
    pub file: String,
    pub problem: String,
}

/// A port workspace's goal and catalogue, read once.
#[derive(Debug)]
pub struct Port {
    pub goal: Goal,
    pub entries: BTreeMap<String, Entry>,
    /// `formats/<id>.md`: the disc's formats found so far.
    pub formats: BTreeMap<String, assets::Format>,
    /// `formats/unused.toml`: disc files the game never loads, with why.
    pub unused: BTreeMap<String, String>,
    pub problems: Vec<Problem>,
}

/// `F-` and four or more digits.
pub fn is_feature_id(id: &str) -> bool {
    id.strip_prefix("F-").is_some_and(|d| d.len() >= 4 && d.bytes().all(|b| b.is_ascii_digit()))
}

fn problem(file: &str, problem: impl Into<String>) -> Problem {
    Problem { file: file.to_string(), problem: problem.into() }
}

/// The front matter's text and what follows it, or why there is none.
pub(crate) fn split_front(text: &str) -> Result<(String, String), String> {
    let mut lines = text.split_inclusive('\n');
    let first = lines.next().unwrap_or_default().trim();
    if first != "+++" {
        return Err(if first == "---" {
            "its front matter is TOML between +++ lines, not ---".into()
        } else {
            "no front matter: the file starts with +++, its TOML fields, and +++".into()
        });
    }
    let mut front = String::new();
    for line in lines.by_ref() {
        if line.trim() == "+++" {
            return Ok((front, lines.collect()));
        }
        front.push_str(line);
    }
    Err("its front matter is never closed by a +++ line".into())
}

/// Whether two texts of catalogue file `file` are the same entry but for
/// the held-out recordings it names, which its review does not read.
pub(crate) fn same_but_held_out(file: &str, a: &str, b: &str) -> bool {
    let without = |text| {
        entry(file, text).ok().map(|mut e| {
            e.recordings.held_out.clear();
            e
        })
    };
    matches!((without(a), without(b)), (Some(a), Some(b)) if a == b)
}

/// One catalogue file: its entry, or why it is not one.
fn entry(file: &str, text: &str) -> Result<Entry, String> {
    let (front, body) = split_front(text)?;
    let mut e: Entry = toml::from_str(&front).map_err(|e| format!("front matter: {}", e.message()))?;
    let stem = Path::new(file).file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    if e.id != stem {
        return Err(format!("its id is {:?}; a file's id is its name ({stem})", e.id));
    }
    e.description = body.trim().to_string();
    e.file = file.to_string();
    Ok(e)
}

/// What is wrong with an entry's own fields.
fn entry_problems(e: &Entry, out: &mut Vec<Problem>) {
    let f = &e.file;
    let mut seen = BTreeSet::new();
    for r in e.recordings.all() {
        let ok = r.split_once('/').is_some_and(|(s, n)| {
            [s, n].iter().all(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-'))
        });
        if !ok {
            out.push(problem(f, format!("recordings: {r:?} is not <scenario>/<name>")));
        } else if !seen.insert(r) {
            out.push(problem(f, format!("recordings: {r} is listed twice (work and held_out are apart)")));
        }
    }
    let mut code = BTreeSet::new();
    for c in &e.code {
        match entry_addr(c) {
            None => out.push(problem(f, format!("code: {c:?} is not an address (0x8002D810)"))),
            Some(a) if !code.insert(a) => out.push(problem(f, format!("code: {c} is listed twice"))),
            Some(_) => {}
        }
    }
    for (a, why) in &e.wont_reach {
        if entry_addr(a).is_none() {
            out.push(problem(f, format!("wont_reach: {a:?} is not an address (0x8002D820)")));
        }
        if why.trim().is_empty() {
            out.push(problem(f, format!("wont_reach.{a}: no reason: why no recording can reach it")));
        }
    }
    if e.questions.iter().any(|q| q.trim().is_empty()) {
        out.push(problem(f, "questions: an empty question"));
    }
}

impl Port {
    /// The goal and catalogue under `root`. An error when `root` is not a
    /// port (no `goal.toml`) or the goal cannot be read; anything wrong in
    /// the catalogue is a problem.
    pub fn load(root: &Path) -> crate::Result<Port> {
        let path = root.join(GOAL);
        let text = match std::fs::read_to_string(&path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Err(Error::invalid(format!(
                    "{} has no {GOAL}: run in a port's workspace (psx port init)",
                    root.display()
                )));
            }
            Err(e) => return Err(Error::io(&path, e)),
        };
        let goal: Goal = toml::from_str(&text).map_err(|e| Error::invalid(format!("{GOAL}: {}", e.message())))?;
        let mut problems = Vec::new();
        let mut entries = BTreeMap::new();
        let dir = root.join(CATALOGUE);
        let mut files: Vec<String> = match std::fs::read_dir(&dir) {
            Ok(d) => d.flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect(),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
            Err(e) => return Err(Error::io(&dir, e)),
        };
        files.sort();
        for name in files.iter().filter(|n| n.starts_with("F-") && n.ends_with(".md")) {
            let file = format!("{CATALOGUE}/{name}");
            let text = std::fs::read_to_string(dir.join(name)).map_err(|e| Error::io(dir.join(name), e))?;
            match entry(&file, &text) {
                Ok(e) => {
                    entries.insert(e.id.clone(), e);
                }
                Err(p) => problems.push(problem(&file, p)),
            }
        }
        let (formats, unused) = assets::read_formats(root, &mut problems)?;
        let mut port = Port { goal, entries, formats, unused, problems };
        port.check();
        port.check_disk(root);
        Ok(port)
    }

    /// What the entries name that the workspace does not have: recordings
    /// not on disk, code that is not the inventory's.
    fn check_disk(&mut self, root: &Path) {
        let mut out = Vec::new();
        for e in self.entries.values() {
            for r in e.recordings.all().collect::<BTreeSet<_>>() {
                let Some((s, n)) = r.split_once('/') else { continue };
                if !root.join("scenarios").join(s).join(format!("{n}.jsonl")).is_file() {
                    out.push(problem(
                        &e.file,
                        format!("recordings: no scenarios/{s}/{n}.jsonl (psx scenario record, or play)"),
                    ));
                }
            }
        }
        let naming: Vec<&Entry> =
            self.entries.values().filter(|e| !e.code.is_empty() || !e.wont_reach.is_empty()).collect();
        let loaders: Vec<&assets::Format> = self.formats.values().filter(|f| !f.loader.is_empty()).collect();
        if !naming.is_empty() || !loaders.is_empty() {
            let ctx = crate::commands::Ctx { root: root.to_path_buf(), cancel: None };
            let functions = crate::coverage::inventory(&ctx, None)
                .and_then(|(_, inv)| crate::coverage::Functions::read(root, &inv));
            for f in loaders {
                match &functions {
                    Err(err) => {
                        out.push(problem(&f.file, format!("loader is checked against the inventory: {}", err.message)))
                    }
                    Ok(fns) => {
                        for l in f.loader.iter().filter(|l| entry_addr(l).is_some_and(|a| !fns.starts(a))) {
                            out.push(problem(&f.file, format!("loader: no function of the inventory starts at {l}")));
                        }
                    }
                }
            }
            for e in naming {
                let f = &e.file;
                let fns = match &functions {
                    Ok(fns) => fns,
                    Err(err) => {
                        out.push(problem(
                            f,
                            format!("code and wont_reach are checked against the inventory: {}", err.message),
                        ));
                        continue;
                    }
                };
                let code: Vec<u32> = e.code.iter().filter_map(|c| entry_addr(c)).collect();
                for c in &e.code {
                    if entry_addr(c).is_some_and(|a| !fns.starts(a)) {
                        out.push(problem(f, format!(
                            "code: no function of the inventory starts at {c} (code coverage diff finds a feature's functions; take the inventory again with the run that has it)"
                        )));
                    }
                }
                for a in e.wont_reach.keys() {
                    let Some(addr) = entry_addr(a) else { continue };
                    match fns.branch(addr) {
                        None => out.push(problem(
                            f,
                            format!("wont_reach: {a} is not a conditional branch of the inventory's code"),
                        )),
                        Some(holders) if !holders.iter().any(|h| code.contains(h)) => {
                            let holders: Vec<String> = holders.iter().map(|h| format!("0x{h:08X}")).collect();
                            out.push(problem(
                                f,
                                format!(
                                    "wont_reach: {a} is in {}, which code does not list",
                                    if holders.is_empty() { "no function".to_string() } else { holders.join(", ") }
                                ),
                            ));
                        }
                        Some(_) => {}
                    }
                }
            }
        }
        self.problems.extend(out);
    }

    /// The problems in what was read.
    fn check(&mut self) {
        let mut out = Vec::new();
        for e in self.entries.values() {
            let f = &e.file;
            if !is_feature_id(&e.id) {
                out.push(problem(f, format!("{} is not a feature id (F- and four digits)", e.id)));
            }
            for (k, v) in [("title", &e.title), ("area", &e.area)] {
                if v.is_empty() {
                    out.push(problem(f, format!("no {k}")));
                }
            }
            if e.description.is_empty() {
                out.push(problem(f, "no description: a paragraph in game terms after the front matter"));
            }
            entry_problems(e, &mut out);
            for d in &e.depends {
                if d == &e.id {
                    out.push(problem(f, "depends on itself"));
                } else if !self.entries.contains_key(d) {
                    out.push(problem(f, format!("depends on {d}, which has no catalogue entry")));
                }
            }
        }
        let cyclic = self.cyclic();
        if !cyclic.is_empty() {
            let ids: Vec<&str> = cyclic.iter().map(String::as_str).collect();
            out.push(problem(CATALOGUE, format!("a dependency cycle runs through or before {}", ids.join(", "))));
        }
        if self.goal.description.trim().is_empty() {
            out.push(problem(GOAL, "no description: the goal in the person's words"));
        }
        if self.goal.features == Features::List(Vec::new()) {
            out.push(problem(GOAL, "names no features: list the catalogue entries the goal needs, or \"all\""));
        }
        if let Features::List(l) = &self.goal.features {
            let mut seen = BTreeSet::new();
            for id in l {
                if !seen.insert(id) {
                    out.push(problem(GOAL, format!("{id} is listed twice")));
                } else if !self.entries.contains_key(id) {
                    out.push(problem(GOAL, format!("{id} has no catalogue entry")));
                }
            }
        }
        self.problems.extend(out);
    }

    /// Entries that no dependency order can place: in a cycle, or depending
    /// on one.
    fn cyclic(&self) -> BTreeSet<String> {
        let all: BTreeSet<String> = self.entries.keys().cloned().collect();
        let placed: BTreeSet<String> = self.order(&all).into_iter().collect();
        all.difference(&placed).cloned().collect()
    }

    /// `ids` in dependency order (dependencies first, then by id), leaving
    /// out any that a cycle keeps from being placed.
    fn order(&self, ids: &BTreeSet<String>) -> Vec<String> {
        let deps = |id: &String| -> BTreeSet<String> {
            self.entries
                .get(id)
                .map(|e| e.depends.iter().filter(|d| ids.contains(*d) && *d != id).cloned().collect())
                .unwrap_or_default()
        };
        let mut waiting: BTreeMap<String, BTreeSet<String>> = ids.iter().map(|id| (id.clone(), deps(id))).collect();
        let mut out = Vec::new();
        while let Some(next) = waiting.iter().find(|(_, d)| d.is_empty()).map(|(id, _)| id.clone()) {
            waiting.remove(&next);
            for d in waiting.values_mut() {
                d.remove(&next);
            }
            out.push(next);
        }
        out
    }

    /// The goal's features as ids: its list, or every catalogued feature.
    pub fn goal_ids(&self) -> BTreeSet<String> {
        match &self.goal.features {
            Features::All => self.entries.keys().cloned().collect(),
            Features::List(l) => l.iter().cloned().collect(),
        }
    }

    /// The goal's features and every feature they depend on, in dependency
    /// order; features a cycle keeps from being placed come last, by id.
    pub fn closure(&self) -> Vec<String> {
        let mut ids = BTreeSet::new();
        let mut todo: Vec<String> = self.goal_ids().into_iter().collect();
        while let Some(id) = todo.pop() {
            if ids.insert(id.clone()) {
                if let Some(e) = self.entries.get(&id) {
                    todo.extend(e.depends.iter().filter(|d| !ids.contains(*d)).cloned());
                }
            }
        }
        let mut out = self.order(&ids);
        let placed: BTreeSet<String> = out.iter().cloned().collect();
        out.extend(ids.difference(&placed).cloned());
        out
    }

    pub fn confirmation(&self) -> Confirmation {
        let Some(c) = &self.goal.confirmed else { return Confirmation::Missing };
        let at = c.at.clone();
        let missing: Vec<String> = match (&c.features, &self.goal.features) {
            (Features::All, Features::All) => Vec::new(),
            (Features::All, Features::List(_)) => vec!["all".into()],
            (Features::List(was), _) => {
                // Still needed by another goal feature counts as still wanted.
                let now: BTreeSet<String> = self.closure().into_iter().collect();
                was.iter().filter(|id| !now.contains(*id)).cloned().collect()
            }
        };
        let missing: Vec<String> =
            missing.into_iter().chain((c.assets && !self.goal.assets).then(|| "assets".to_string())).collect();
        if missing.is_empty() { Confirmation::Covers { at } } else { Confirmation::Shrunk { at, missing } }
    }

    /// The closure as the person reads it before confirming, with each
    /// feature's stage.
    pub fn closure_json(&self, stage: &dyn Fn(&str) -> Option<Stage>) -> Value {
        let goal = self.goal_ids();
        let closure = self.closure();
        let wanted: BTreeSet<&String> = closure.iter().collect();
        let rows: Vec<Value> = closure
            .iter()
            .map(|id| {
                let e = self.entries.get(id);
                let required_by: Vec<&String> = self
                    .entries
                    .values()
                    .filter(|o| wanted.contains(&o.id) && o.depends.contains(id))
                    .map(|o| &o.id)
                    .collect();
                json!({
                    "id": id,
                    "title": e.map(|e| e.title.as_str()),
                    "area": e.map(|e| e.area.as_str()),
                    "stage": stage(id).map(Stage::name),
                    "in_goal": goal.contains(id),
                    "depends": e.map(|e| &e.depends),
                    "code": e.map(|e| &e.code),
                    "recordings": e.map(|e| &e.recordings),
                    "wont_reach": e.map(|e| &e.wont_reach),
                    "questions": e.map(|e| &e.questions),
                    "required_by": required_by,
                })
            })
            .collect();
        json!(rows)
    }

    pub(crate) fn confirmation_json(&self) -> Value {
        let mut v = match self.confirmation() {
            Confirmation::Missing => json!({"state": "missing"}),
            Confirmation::Covers { at } => json!({"state": "covers", "at": at}),
            Confirmation::Shrunk { at, missing } => json!({"state": "shrunk", "at": at, "missing": missing}),
        };
        let unconfirmed = self.unconfirmed_wont_reach();
        if !unconfirmed.is_empty() {
            v["wont_reach_unconfirmed"] = json!(unconfirmed);
        }
        v
    }

    /// The closure's won't-reach branches, as the entries give them now.
    pub(crate) fn wont_reach(&self) -> WontReach {
        self.closure()
            .iter()
            .filter_map(|id| self.entries.get(id))
            .filter(|e| !e.wont_reach.is_empty())
            .map(|e| (e.id.clone(), e.wont_reach.clone()))
            .collect()
    }

    /// The closure's won't-reach branches the person has not confirmed as
    /// they stand (`F-0003 0x8002D820`): new, or with another reason.
    pub fn unconfirmed_wont_reach(&self) -> Vec<String> {
        let confirmed = self.goal.confirmed.as_ref().map(|c| &c.wont_reach);
        let mut out = Vec::new();
        for (id, branches) in self.wont_reach() {
            for (a, why) in branches {
                if confirmed.and_then(|c| c.get(&id)).and_then(|b| b.get(&a)) != Some(&why) {
                    out.push(format!("{id} {a}"));
                }
            }
        }
        out
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub(crate) fn workspace(goal: &str, entries: &[(&str, &str)]) -> (tempdir::Dir, Port) {
        let dir = tempdir::Dir::new();
        std::fs::write(dir.0.join(GOAL), goal).unwrap();
        std::fs::create_dir_all(dir.0.join(CATALOGUE)).unwrap();
        for (name, text) in entries {
            std::fs::write(dir.0.join(CATALOGUE).join(name), text).unwrap();
        }
        let port = Port::load(&dir.0).unwrap();
        (dir, port)
    }

    pub(crate) mod tempdir {
        pub struct Dir(pub std::path::PathBuf);
        impl Dir {
            pub fn new() -> Self {
                let p = std::env::temp_dir().join(format!("psx-port-test-{}", crate::stamp()));
                std::fs::create_dir_all(&p).unwrap();
                Dir(p)
            }
        }
        impl Drop for Dir {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
    }

    pub(crate) fn feature(id: &str, depends: &str) -> String {
        let depends: Vec<String> = depends.split(", ").filter(|d| !d.is_empty()).map(|d| format!("\"{d}\"")).collect();
        format!(
            "+++\nid = \"{id}\"\ntitle = \"Feature {id}\"\narea = \"driving\"\ndepends = [{}]\n+++\nWhat it does.\n",
            depends.join(", ")
        )
    }

    #[test]
    fn the_closure_adds_dependencies_first() {
        let (a, b, c, d) = (
            feature("F-0001", ""),
            feature("F-0002", "F-0001"),
            feature("F-0003", "F-0002, F-0001"),
            feature("F-0004", ""),
        );
        let (_dir, port) = workspace(
            "description = \"x\"\nfeatures = [\"F-0003\"]\n",
            &[
                ("F-0001.md", &a),
                ("F-0002.md", &b),
                ("F-0003.md", &c),
                ("F-0004.md", &d),
                ("README.md", "# not an entry"),
            ],
        );
        assert_eq!(port.problems, []);
        assert_eq!(port.closure(), ["F-0001", "F-0002", "F-0003"]);
        let rows = port.closure_json(&|id| port.entries.contains_key(id).then_some(Stage::Catalogued));
        assert_eq!(rows[0]["required_by"], json!(["F-0002", "F-0003"]));
        assert_eq!(rows[2]["in_goal"], json!(true));
        assert_eq!(rows[1]["in_goal"], json!(false));
    }

    #[test]
    fn problems_name_the_file() {
        let (a, b) = (feature("F-0001", "F-0002"), feature("F-0002", "F-0001"));
        let (_dir, port) = workspace(
            "description = \"x\"\nfeatures = [\"F-0001\", \"F-0007\", \"F-0001\"]\n",
            &[
                ("F-0001.md", &a),
                ("F-0002.md", &b),
                ("F-0003.md", "+++\nid = \"F-0004\"\n+++\n"),
                ("F-0005.md", "+++\nid = \"F-0005\"\ndepends = [\"F-0006\"]\n+++\n"),
            ],
        );
        let p: Vec<String> = port.problems.iter().map(|p| format!("{}: {}", p.file, p.problem)).collect();
        assert_eq!(
            p,
            [
                "catalogue/F-0003.md: its id is \"F-0004\"; a file's id is its name (F-0003)",
                "catalogue/F-0005.md: no title",
                "catalogue/F-0005.md: no area",
                "catalogue/F-0005.md: no description: a paragraph in game terms after the front matter",
                "catalogue/F-0005.md: depends on F-0006, which has no catalogue entry",
                "catalogue: a dependency cycle runs through or before F-0001, F-0002",
                "goal.toml: F-0007 has no catalogue entry",
                "goal.toml: F-0001 is listed twice",
            ]
        );
        // A cycle still gives every feature in the closure.
        assert_eq!(port.closure(), ["F-0007", "F-0001", "F-0002"]);
    }

    #[test]
    fn an_entry_names_recordings_code_and_branches_the_workspace_has() {
        let entry = |extra: &str| {
            format!("+++\nid = \"F-0001\"\ntitle = \"Jump\"\narea = \"player\"\n{extra}\n+++\nWhat it does.\n")
        };
        let goal = "description = \"x\"\nfeatures = [\"F-0001\"]\n";
        let fields = "code = [\"0x8002D810\", \"jump\", \"0x8002D810\"]\nquestions = [\"\"]\n\
            [recordings]\nwork = [\"jump/stomp\", \"jump\"]\nheld_out = [\"jump/stomp\"]\n\
            [wont_reach]\n\"0x8002D820\" = \"\"";
        let (d, port) = workspace(goal, &[("F-0001.md", &entry(fields))]);
        let p: Vec<&str> = port.problems.iter().map(|p| p.problem.as_str()).collect();
        assert_eq!(
            p,
            [
                "recordings: \"jump\" is not <scenario>/<name>",
                "recordings: jump/stomp is listed twice (work and held_out are apart)",
                "code: \"jump\" is not an address (0x8002D810)",
                "code: 0x8002D810 is listed twice",
                "wont_reach.0x8002D820: no reason: why no recording can reach it",
                "questions: an empty question",
                "recordings: no scenarios/jump/stomp.jsonl (psx scenario record, or play)",
                "code and wont_reach are checked against the inventory: .psx/inventory.json: no inventory; take one with inventory first",
            ]
            .map(|l| l.replace(".psx/inventory.json", &d.0.join(".psx/inventory.json").display().to_string()))
        );
        let e = &port.entries["F-0001"];
        assert_eq!(e.scenarios(), ["jump"]);
        // The old YAML front matter, and an unknown field, say what is wrong.
        let (_d, port) = workspace(goal, &[("F-0001.md", "---\nid: F-0001\n---\n")]);
        assert!(port.problems[0].problem.contains("TOML between +++ lines"), "{:?}", port.problems);
        let (_d, port) = workspace(goal, &[("F-0001.md", &entry("scenarios = []"))]);
        assert!(port.problems[0].problem.contains("unknown field `scenarios`"), "{:?}", port.problems);
    }

    #[test]
    fn wont_reach_branches_need_the_persons_confirmation_as_they_stand() {
        let entry = |why: &str| {
            format!(
                "+++\nid = \"F-0001\"\ntitle = \"Jump\"\narea = \"player\"\n[wont_reach]\n\"0x8002D820\" = \"{why}\"\n+++\nWhat it does.\n"
            )
        };
        let goal = "features = [\"F-0001\"]\nconfirmed = { at = \"t\", features = [\"F-0001\"], wont_reach = { \"F-0001\" = { \"0x8002D820\" = \"under water\" } } }\n";
        let (_d, port) = workspace(goal, &[("F-0001.md", &entry("under water"))]);
        assert_eq!(port.unconfirmed_wont_reach(), Vec::<String>::new());
        let (_d, port) = workspace(goal, &[("F-0001.md", &entry("in a cutscene"))]);
        assert_eq!(port.unconfirmed_wont_reach(), ["F-0001 0x8002D820"]);
        assert_eq!(port.confirmation_json()["wont_reach_unconfirmed"], json!(["F-0001 0x8002D820"]));
    }

    #[test]
    fn a_new_goal_asks_for_a_description_and_features() {
        let (_dir, port) = workspace(GOAL_TOML, &[]);
        let p: Vec<&str> = port.problems.iter().map(|p| p.problem.as_str()).collect();
        assert_eq!(
            p,
            [
                "no description: the goal in the person's words",
                "names no features: list the catalogue entries the goal needs, or \"all\""
            ]
        );
    }

    #[test]
    fn a_confirmed_feature_still_needed_by_the_goal_has_not_left_it() {
        let (a, b) = (feature("F-0001", ""), feature("F-0002", "F-0001"));
        let goal = "features = [\"F-0002\"]\nconfirmed = { at = \"t\", features = [\"F-0001\", \"F-0002\"] }\n";
        let (_d, port) = workspace(goal, &[("F-0001.md", &a), ("F-0002.md", &b)]);
        assert_eq!(port.confirmation(), Confirmation::Covers { at: "t".into() });
    }

    #[test]
    fn a_confirmation_covers_a_list_that_grew_but_not_one_that_shrank() {
        let entries: Vec<(String, String)> =
            (1..=3).map(|i| (format!("F-000{i}.md"), feature(&format!("F-000{i}"), ""))).collect();
        let entries: Vec<(&str, &str)> = entries.iter().map(|(a, b)| (a.as_str(), b.as_str())).collect();
        let confirmed = "confirmed = { at = \"2026-10-29T10:00:00Z\", features = [\"F-0001\", \"F-0002\"] }\n";
        let (_d, port) = workspace("features = [\"F-0001\", \"F-0002\"]\n", &entries);
        assert_eq!(port.confirmation(), Confirmation::Missing);
        let (_d, port) = workspace(&format!("features = [\"F-0001\", \"F-0002\", \"F-0003\"]\n{confirmed}"), &entries);
        assert_eq!(port.confirmation(), Confirmation::Covers { at: "2026-10-29T10:00:00Z".into() });
        let (_d, port) = workspace(&format!("features = \"all\"\n{confirmed}"), &entries);
        assert!(matches!(port.confirmation(), Confirmation::Covers { .. }));
        let (_d, port) = workspace(&format!("features = [\"F-0002\"]\n{confirmed}"), &entries);
        assert_eq!(
            port.confirmation(),
            Confirmation::Shrunk { at: "2026-10-29T10:00:00Z".into(), missing: vec!["F-0001".into()] }
        );
        let all = "confirmed = { at = \"t\", features = \"all\" }\n";
        let (_d, port) = workspace(&format!("features = [\"F-0001\"]\n{all}"), &entries);
        assert!(matches!(port.confirmation(), Confirmation::Shrunk { .. }));
    }

    #[test]
    fn a_goal_reads_all_or_a_list_and_nothing_else() {
        assert_eq!(toml::from_str::<Goal>("features = \"all\"").unwrap().features, Features::All);
        assert!(toml::from_str::<Goal>("features = \"some\"").is_err());
        assert!(toml::from_str::<Goal>("feature = []").is_err());
        assert_eq!(toml::from_str::<Goal>(GOAL_TOML).unwrap().features, Features::List(vec![]));
    }
}
