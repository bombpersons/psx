//! A port: one workspace where one agent investigates the original and
//! builds the port in Bevy (docs/port-design.md). `port init` writes it.
//! The goal (`goal.toml`) says which features the person wants, the
//! catalogue (`catalogue/F-NNNN.md`) describes the game's features in game
//! terms with their dependencies, and `port confirm` records the person's
//! approval of the goal's list. The goal's features and everything they
//! depend on are its closure; each has a stage.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use serde::Deserialize;
use serde_json::{Value, json};

use crate::Error;
use crate::commands::{Ctx, Op, Outcome};
use crate::front_matter::{Fm, front_matter};
use crate::roles::{absolute, game_name, link_or_copy, psx_exe};
use crate::template::{TemplateOptions, TemplateRegion};

pub const GOAL: &str = "goal.toml";
pub const CATALOGUE: &str = "catalogue";

const GOAL_TOML: &str = "\
# What the person wants from this port. The agent proposes the features
# (catalogue/F-NNNN.md) and lists them here; the person confirms the list
# with `psx port confirm --yes`, which writes `confirmed`. The list may grow
# after that; taking a feature out needs the person to confirm again.
description = \"\"
features = []   # feature ids, or \"all\"
";

const CATALOGUE_README: &str = include_str!("../../../templates/port/catalogue/README.md");

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
    fn to_json(&self) -> Value {
        match self {
            Features::All => json!("all"),
            Features::List(l) => json!(l),
        }
    }
    fn to_toml(&self) -> String {
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
    /// Features the person confirmed are no longer in the goal.
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

    /// A feature's stage: the last whose check holds, None when it has no
    /// catalogue entry. Only the first stage's check exists so far; the
    /// others come with the files they read (mechanics, plugins, verdicts,
    /// reviews, sandboxes).
    pub fn stage(&self, id: &str) -> Option<Stage> {
        self.entries.contains_key(id).then_some(Stage::Catalogued)
    }

    pub fn confirmation(&self) -> Confirmation {
        let Some(c) = &self.goal.confirmed else { return Confirmation::Missing };
        let at = c.at.clone();
        let missing: Vec<String> = match (&c.features, &self.goal.features) {
            (Features::All, Features::All) => Vec::new(),
            (Features::All, Features::List(_)) => vec!["all".into()],
            (Features::List(was), _) => {
                let now = self.goal_ids();
                was.iter().filter(|id| !now.contains(*id)).cloned().collect()
            }
        };
        if missing.is_empty() { Confirmation::Covers { at } } else { Confirmation::Shrunk { at, missing } }
    }

    /// The closure as the person reads it before confirming.
    pub fn closure_json(&self) -> Value {
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
                    "stage": self.stage(id).map(Stage::name),
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

    fn confirmation_json(&self) -> Value {
        match self.confirmation() {
            Confirmation::Missing => json!({"state": "missing"}),
            Confirmation::Covers { at } => json!({"state": "covers", "at": at}),
            Confirmation::Shrunk { at, missing } => json!({"state": "shrunk", "at": at, "missing": missing}),
        }
    }
}

/// `goal.toml`'s text with `confirmed` set to `c`: an earlier confirmation
/// (a line, or a `[confirmed]` table) replaced, everything else kept.
fn with_confirmation(text: &str, c: &Confirmed) -> String {
    let mut lines: Vec<&str> = Vec::new();
    let mut in_table = false;
    for line in text.lines() {
        let t = line.trim_start();
        if t.starts_with('[') {
            in_table = t.starts_with("[confirmed]");
            if in_table {
                continue;
            }
        }
        let key = t.split(['=', ' ', '\t']).next().unwrap_or("");
        if in_table || key == "confirmed" {
            continue;
        }
        lines.push(line);
    }
    let line = format!("confirmed = {{ at = \"{}\", features = {} }}", c.at, c.features.to_toml());
    // A top-level key goes before the first table.
    let at = lines.iter().position(|l| l.trim_start().starts_with('[')).unwrap_or(lines.len());
    lines.insert(at, &line);
    let mut out = lines.join("\n");
    out.push('\n');
    out
}

#[derive(clap::Args, Clone, Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct PortConfirmArgs {
    /// Record the confirmation. Without it, show the goal's features, the
    /// closure and any problems: what would be confirmed.
    #[arg(long)]
    #[serde(default)]
    pub yes: bool,
}

impl PortConfirmArgs {
    fn run(&self, ctx: &Ctx) -> crate::Result<Value> {
        let port = Port::load(&ctx.root)?;
        let mut out = json!({
            "description": port.goal.description,
            "features": port.goal.features.to_json(),
            "closure": port.closure_json(),
            "problems": port.problems,
            "stages": Stage::ALL.map(Stage::name),
            "confirmation": port.confirmation_json(),
        });
        if !self.yes {
            out["next"] = json!(if port.problems.is_empty() {
                "if this is the list the person wants: psx port confirm --yes (the person runs it)"
            } else {
                "the agent fixes the problems first"
            });
            return Ok(out);
        }
        if !port.problems.is_empty() {
            return Err(Error::invalid(format!(
                "the goal or catalogue has {} problems; run psx port confirm without --yes to see them",
                port.problems.len()
            )));
        }
        if port.goal_ids().is_empty() {
            return Err(Error::invalid("the goal names no features, and no feature is catalogued"));
        }
        let c = Confirmed { at: crate::publish::utc_now(), features: port.goal.features.clone() };
        let path = ctx.root.join(GOAL);
        let text = std::fs::read_to_string(&path).map_err(|e| Error::io(&path, e))?;
        let new = with_confirmation(&text, &c);
        // What was written must read back as this confirmation.
        let check: Goal = toml::from_str(&new)
            .map_err(|e| Error::invalid(format!("{GOAL} rewritten unreadably: {}", e.message())))?;
        if check.confirmed.as_ref().map(|k| (&k.at, &k.features)) != Some((&c.at, &c.features)) {
            return Err(Error::invalid(format!("{GOAL} rewritten without the confirmation; nothing written")));
        }
        crate::write_atomic(&path, new.as_bytes()).map_err(|e| Error::io(&path, e))?;
        out["confirmation"] = json!({"state": "covers", "at": c.at});
        out["confirmed"] = json!({"at": c.at, "features": c.features.to_json()});
        Ok(out)
    }
}

impl Op for PortConfirmArgs {
    fn exec(self, ctx: &Ctx) -> Outcome {
        Outcome::result(self.run(ctx))
    }
}

#[derive(clap::Args, Clone, Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct PortInitArgs {
    /// The port's workspace: a new folder, or one holding nothing the
    /// template writes.
    pub dest: PathBuf,
    /// The disc image (.cue, .chd, .iso or .bin), linked into the
    /// workspace's media/.
    #[arg(long)]
    pub media: PathBuf,
    /// The port's package name (default from the disc image's name).
    #[arg(long)]
    #[serde(default)]
    pub name: Option<String>,
    /// The original's video region. Default: the boot executable's region, else ntsc.
    #[arg(long, value_enum)]
    #[serde(default)]
    pub region: Option<TemplateRegion>,
    /// Video fields per logic tick. Default 1.
    #[arg(long)]
    #[serde(default)]
    pub fields_per_tick: Option<u32>,
}

/// A package name from a game's name: lower case, words joined by `-`.
fn package_from(game: &str) -> String {
    let n: String =
        game.to_ascii_lowercase().chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '-' }).collect();
    let mut n = n.trim_matches('-').to_string();
    while n.contains("--") {
        n = n.replace("--", "-");
    }
    if n.starts_with(|c: char| c.is_ascii_lowercase()) { n } else { format!("game-{n}") }
}

fn write_new(dest: &Path, rel: &str, text: &str, written: &mut Vec<String>) -> crate::Result<()> {
    let p = dest.join(rel);
    if let Some(d) = p.parent() {
        std::fs::create_dir_all(d).map_err(|e| Error::io(d, e))?;
    }
    std::fs::write(&p, text).map_err(|e| Error::io(&p, e))?;
    written.push(rel.to_string());
    Ok(())
}

impl PortInitArgs {
    fn run(&self, ctx: &Ctx) -> crate::Result<Value> {
        let media = absolute(&ctx.root.join(&self.media));
        let dest = absolute(&ctx.root.join(&self.dest));
        let ours = [GOAL, "catalogue/README.md", ".mcp.json", ".psx/project.json"];
        let clashes: Vec<&str> = ours.into_iter().filter(|r| dest.join(r).exists()).collect();
        if !clashes.is_empty() {
            return Err(Error::invalid(format!(
                "{} already has {}; port init writes a new workspace",
                dest.display(),
                clashes.join(", ")
            )));
        }
        // The disc first, so one that cannot be read writes nothing.
        let disc = psx_media::disc::Disc::open(&media)?;
        let mut sources = vec![disc.source.clone()];
        sources.extend(disc.files.iter().map(|f| f.path.clone()));
        sources.sort();
        sources.dedup();
        let game = game_name(&media);
        let name = self.name.clone().unwrap_or_else(|| package_from(&game));
        let (region, region_from) = match self.region {
            Some(r) => (r, "argument"),
            None => match crate::context::region(Some(&media))? {
                Some(crate::bios::Region::Pal) => (TemplateRegion::Pal, "disc"),
                Some(_) => (TemplateRegion::Ntsc, "disc"),
                None => (TemplateRegion::Ntsc, "default"),
            },
        };
        let options = TemplateOptions { region, fields_per_tick: self.fields_per_tick.unwrap_or(1) };
        std::fs::create_dir_all(&dest).map_err(|e| Error::io(&dest, e))?;
        let template = crate::template::create(&dest, Some(&name), options)?;
        let mut written = Vec::new();
        write_new(&dest, GOAL, GOAL_TOML, &mut written)?;
        write_new(&dest, "catalogue/README.md", CATALOGUE_README, &mut written)?;
        let mcp = json!({"mcpServers": {"psx": {"command": psx_exe(), "args": ["--root", dest, "mcp"]}}});
        write_new(
            &dest,
            ".mcp.json",
            &format!("{}\n", serde_json::to_string_pretty(&mcp).unwrap_or_default()),
            &mut written,
        )?;
        // The disc and the BIOS, linked from where they are and never committed.
        let ignore = "# Linked by psx port init from where they are: never commit them.\n*\n";
        write_new(&dest, "media/.gitignore", ignore, &mut written)?;
        write_new(&dest, "bios/.gitignore", ignore, &mut written)?;
        let mut linked = Vec::new();
        for f in &sources {
            let to = dest.join("media").join(f.file_name().unwrap_or_default());
            if !to.exists() {
                link_or_copy(&absolute(f), &to)?;
                linked.push(format!("media/{}", f.file_name().unwrap_or_default().to_string_lossy()));
            }
        }
        let mut bios = Vec::new();
        for e in std::fs::read_dir(ctx.root.join("bios")).into_iter().flatten().flatten() {
            let to = dest.join("bios").join(e.file_name());
            if e.path().is_file() && e.file_name() != ".gitignore" && !to.exists() {
                link_or_copy(&absolute(&e.path()), &to)?;
                bios.push(format!("bios/{}", e.file_name().to_string_lossy()));
            }
        }
        // The disc as the workspace's default media, for every psx command run in it.
        let disc_rel = format!("media/{}", media.file_name().unwrap_or_default().to_string_lossy());
        let project = json!({"format": 1, "media": disc_rel});
        write_new(
            &dest,
            ".psx/project.json",
            &serde_json::to_string_pretty(&project).unwrap_or_default(),
            &mut written,
        )?;
        let git = std::process::Command::new("git")
            .args(["init", "-q"])
            .current_dir(&dest)
            .status()
            .is_ok_and(|s| s.success());
        let mut next = vec![
            format!("the person starts a session (Claude Code or Codex) in {}", dest.display()),
            "the person states the goal; the agent catalogues the features it needs and lists them in goal.toml".into(),
            "the person reads the list (psx port confirm) and confirms it (psx port confirm --yes)".into(),
        ];
        if bios.is_empty() {
            next.push("no BIOS was linked: psx bios install in the workspace".into());
        }
        Ok(json!({
            "game": game, "package": name, "workspace": dest,
            "template": {"owned_files": template["owned_files"], "seeded_files": template["seeded_files"],
                         "region": template["region"], "region_from": region_from, "tick_hz": template["tick_hz"]},
            "written": written, "media": linked, "bios": bios, "git_init": git,
            "next": next,
        }))
    }
}

impl Op for PortInitArgs {
    fn exec(self, ctx: &Ctx) -> Outcome {
        Outcome::result(self.run(ctx))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn workspace(goal: &str, entries: &[(&str, &str)]) -> (tempdir::Dir, Port) {
        let dir = tempdir::Dir::new();
        std::fs::write(dir.0.join(GOAL), goal).unwrap();
        std::fs::create_dir_all(dir.0.join(CATALOGUE)).unwrap();
        for (name, text) in entries {
            std::fs::write(dir.0.join(CATALOGUE).join(name), text).unwrap();
        }
        let port = Port::load(&dir.0).unwrap();
        (dir, port)
    }

    mod tempdir {
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

    fn feature(id: &str, depends: &str) -> String {
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
        assert_eq!(port.stage("F-0002"), Some(Stage::Catalogued));
        assert_eq!(port.stage("F-0009"), None);
        let rows = port.closure_json();
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
    fn confirming_rewrites_only_the_confirmation() {
        let c = Confirmed { at: "2026-10-29T10:00:00Z".into(), features: Features::List(vec!["F-0001".into()]) };
        let text = "# The goal.\ndescription = \"x\"\nfeatures = [\"F-0001\"]   # one\nconfirmed = { at = \"old\", features = [] }\n";
        assert_eq!(
            with_confirmation(text, &c),
            "# The goal.\ndescription = \"x\"\nfeatures = [\"F-0001\"]   # one\nconfirmed = { at = \"2026-10-29T10:00:00Z\", features = [\"F-0001\"] }\n"
        );
        let table = "features = \"all\"\n\n[confirmed]\nat = \"old\"\nfeatures = []\n";
        let all = Confirmed { at: "t".into(), features: Features::All };
        assert_eq!(
            with_confirmation(table, &all),
            "features = \"all\"\n\nconfirmed = { at = \"t\", features = \"all\" }\n"
        );
        let goal: Goal = toml::from_str(&with_confirmation(GOAL_TOML, &c)).unwrap();
        assert_eq!(goal.confirmed.unwrap().features, c.features);
    }

    #[test]
    fn a_goal_reads_all_or_a_list_and_nothing_else() {
        assert_eq!(toml::from_str::<Goal>("features = \"all\"").unwrap().features, Features::All);
        assert!(toml::from_str::<Goal>("features = \"some\"").is_err());
        assert!(toml::from_str::<Goal>("feature = []").is_err());
        assert_eq!(toml::from_str::<Goal>(GOAL_TOML).unwrap().features, Features::List(vec![]));
    }

    #[test]
    fn package_names_come_from_the_game() {
        assert_eq!(package_from("Croc - Legend of the Gobbos"), "croc-legend-of-the-gobbos");
        assert_eq!(package_from("1943"), "game-1943");
    }
}
