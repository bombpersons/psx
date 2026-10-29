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
use crate::front_matter::{Fm, front_matter};

pub mod check;
mod confirm;
mod init;
pub mod next;
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
    #[serde(default)]
    pub confirmed: Option<Confirmed>,
}

/// The person's confirmation: when, and the list they confirmed.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Confirmed {
    pub at: String,
    pub features: Features,
}

/// One catalogue entry: `catalogue/F-NNNN.md`.
#[derive(Debug, Clone)]
pub struct Entry {
    pub id: String,
    pub title: String,
    pub area: String,
    pub depends: Vec<String>,
    pub mechanics: Vec<String>,
    pub scenarios: Vec<String>,
    /// The paragraph after the front matter, in game terms.
    pub description: String,
    /// The file, relative to the workspace.
    pub file: String,
}

/// A feature's stage (design section 9): each holds when its check, a fact
/// read from the workspace's files, holds and so do the ones before it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Stage {
    Catalogued,
    Captured,
    Implemented,
    Verified,
    Reviewed,
    Extracted,
}

impl Stage {
    pub const ALL: [Stage; 6] =
        [Stage::Catalogued, Stage::Captured, Stage::Implemented, Stage::Verified, Stage::Reviewed, Stage::Extracted];

    pub fn name(self) -> &'static str {
        match self {
            Stage::Catalogued => "catalogued",
            Stage::Captured => "captured",
            Stage::Implemented => "implemented",
            Stage::Verified => "verified",
            Stage::Reviewed => "reviewed",
            Stage::Extracted => "extracted",
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
    pub problems: Vec<Problem>,
}

/// `F-` and four or more digits.
pub fn is_feature_id(id: &str) -> bool {
    id.strip_prefix("F-").is_some_and(|d| d.len() >= 4 && d.bytes().all(|b| b.is_ascii_digit()))
}

fn problem(file: &str, problem: impl Into<String>) -> Problem {
    Problem { file: file.to_string(), problem: problem.into() }
}

/// One catalogue file: its entry, or why it is not one.
fn entry(file: &str, text: &str) -> Result<Entry, String> {
    let fm = front_matter(text).ok_or("no front matter: the file starts with --- and its fields, closed by ---")?;
    let get = |k: &str| fm.get(k).map(Fm::str).unwrap_or_default().trim().to_string();
    let list = |k: &str| fm.get(k).map(Fm::list).unwrap_or_default();
    let stem = Path::new(file).file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let id = get("id");
    if id != stem {
        return Err(format!("its id is {id:?}; a file's id is its name ({stem})"));
    }
    Ok(Entry {
        id,
        title: get("title"),
        area: get("area"),
        depends: list("depends"),
        mechanics: list("mechanics"),
        scenarios: list("scenarios"),
        description: crate::front_matter::body(text).trim().to_string(),
        file: file.to_string(),
    })
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
        let mut port = Port { goal, entries, problems };
        port.check();
        Ok(port)
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
                    "mechanics": e.map(|e| &e.mechanics),
                    "scenarios": e.map(|e| &e.scenarios),
                    "required_by": required_by,
                })
            })
            .collect();
        json!(rows)
    }

    pub(crate) fn confirmation_json(&self) -> Value {
        match self.confirmation() {
            Confirmation::Missing => json!({"state": "missing"}),
            Confirmation::Covers { at } => json!({"state": "covers", "at": at}),
            Confirmation::Shrunk { at, missing } => json!({"state": "shrunk", "at": at, "missing": missing}),
        }
    }
}

#[cfg(test)]
mod tests {
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
        format!("---\nid: {id}\ntitle: Feature {id}\narea: driving\ndepends: [{depends}]\n---\nWhat it does.\n")
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
                ("F-0003.md", "---\nid: F-0004\n---\n"),
                ("F-0005.md", "---\nid: F-0005\ndepends: [F-0006]\n---\n"),
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
