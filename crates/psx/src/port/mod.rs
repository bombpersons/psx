//! A port: one workspace where one agent investigates the original and
//! builds the port (docs/port-design.md). `port init` writes it.
//! The goal (`goal.toml`) says what the person wants in their words, the
//! recordings under `scenarios/` are what the port is checked against,
//! the catalogue holds what the agent has set aside (`not_ported.toml`),
//! found unreachable (`wont_reach.toml`) or not understood yet
//! (`questions.md`), and `port confirm` records the person's approval.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use serde::Deserialize;
use serde_json::{Value, json};

use crate::Error;
use crate::script_cover::Stretch;

pub mod assets;
pub mod check;
mod confirm;
mod init;
pub mod manifest;
pub(crate) mod migrate;
pub mod next;
mod pointer;
pub mod process;
pub mod review;
pub mod vectors;
pub mod verdict;

pub use check::PortCheckArgs;
pub use confirm::PortConfirmArgs;
pub use init::PortInitArgs;
pub use next::{PortNextArgs, PortStatusArgs};

pub const GOAL: &str = "goal.toml";
pub const CATALOGUE: &str = "catalogue";
/// The original's code the port has no counterpart of, by function or
/// address range, with why.
pub const NOT_PORTED: &str = "catalogue/not_ported.toml";
/// Branches no recording can reach, by address, range or script
/// position, with why; the person confirms them.
pub const WONT_REACH: &str = "catalogue/wont_reach.toml";
/// What is not known yet: one question per `- ` line.
pub const QUESTIONS: &str = "catalogue/questions.md";
pub const SCENARIOS: &str = "scenarios";
/// Where the port's code is, which cites the original by address.
pub const SOURCES: [&str; 2] = ["game", "tools"];

pub(crate) const GOAL_TOML: &str = "\
# What the person wants from this port, in their words.
description = \"\"
assets = false  # what to extract: true (every disc file), false, or names: [\"format:level\", \"/MAPS/*.WLD\"] (formats/README.md)
# review_lines = 800  # psx says a review is due once the code changed by this many lines since the last
# held_out = []  # the person's plays, scenario/name: judged, never worked on
";

/// The disc's assets a goal asks for: every file, or the files some names
/// cover (none for `false`). A name is a disc path or archive entry, `*`
/// and `?` within a name (`/MAPS/*.WLD`, `/CROCFILE.1:CROC.*`), or
/// `format:<id>`, every file of that format.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(try_from = "AssetsToml")]
pub enum Assets {
    All,
    Names(Vec<String>),
}

#[derive(Deserialize)]
#[serde(untagged)]
enum AssetsToml {
    Flag(bool),
    Names(Vec<String>),
}

impl TryFrom<AssetsToml> for Assets {
    type Error = String;
    fn try_from(a: AssetsToml) -> Result<Self, String> {
        Ok(match a {
            AssetsToml::Flag(true) => Assets::All,
            AssetsToml::Flag(false) => Assets::Names(Vec::new()),
            AssetsToml::Names(n) => Assets::Names(n),
        })
    }
}

impl Default for Assets {
    fn default() -> Self {
        Assets::Names(Vec::new())
    }
}

impl Assets {
    pub fn is_all(&self) -> bool {
        *self == Assets::All
    }
    /// The names it lists; none for every file.
    pub fn names(&self) -> &[String] {
        match self {
            Assets::All => &[],
            Assets::Names(n) => n,
        }
    }
    /// Asks for nothing.
    pub fn is_none(&self) -> bool {
        self.names().is_empty() && !self.is_all()
    }
    pub(crate) fn to_json(&self) -> Value {
        match self {
            Assets::All => json!(true),
            Assets::Names(n) if n.is_empty() => json!(false),
            Assets::Names(n) => json!(n),
        }
    }
    pub(crate) fn to_toml(&self) -> String {
        match self {
            Assets::All => "true".into(),
            Assets::Names(n) if n.is_empty() => "false".into(),
            Assets::Names(n) => toml::Value::from(n.clone()).to_string(),
        }
    }
}

/// What is wrong with an asset name, if anything.
pub fn asset_name_problem(name: &str) -> Option<String> {
    match name.strip_prefix("format:") {
        Some(id) if id.is_empty() || id.contains(['/', '*', '?']) => {
            Some(format!("{name:?} is not format:<id>, a format doc's id (formats/<id>.md) or a standard format"))
        }
        Some(_) => None,
        None if name.starts_with('/') => None,
        None => Some(format!("{name:?} is not a disc path from / (/MAPS/*.WLD, /ARCHIVE:ENTRY) or format:<id>")),
    }
}

/// `goal.toml`.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Goal {
    #[serde(default)]
    pub description: String,
    /// The disc's assets the person wants extracted: each file they cover
    /// imported and checked, or unused. The scenarios bring in their own
    /// besides.
    #[serde(default)]
    pub assets: Assets,
    #[serde(default)]
    pub confirmed: Option<Confirmed>,
    /// A follow-up review comes due once the code has changed by this many
    /// lines (added and removed) in the commits since the last review;
    /// [`REVIEW_LINES`] when unset.
    #[serde(default)]
    pub review_lines: Option<usize>,
    /// The person's plays (scenario/name), held out from the work: judged
    /// by a whole psx port check, never worked on.
    #[serde(default)]
    pub held_out: Vec<String>,
    /// No longer read: the goal named features before recordings were the
    /// unit. port update takes it out.
    #[serde(default)]
    pub features: Option<toml::Value>,
}

/// The id every verdict is kept under: the goal's.
pub const GOAL_ID: &str = "goal";

/// The lines of change that ask for a follow-up review when goal.toml does
/// not say: on Croc, +931/-534 lines went unreviewed while nothing
/// matched or finished.
pub const REVIEW_LINES: usize = 800;

impl Goal {
    pub fn review_lines(&self) -> usize {
        self.review_lines.unwrap_or(REVIEW_LINES)
    }
}

/// The person's confirmation: when, the description and assets they
/// confirmed, their held-out plays, and the branches they agreed no
/// recording can reach (address = reason).
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Confirmed {
    pub at: String,
    /// The description confirmed, as [`description_stamp`]: absent in
    /// confirmations made before it was kept.
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub assets: Assets,
    #[serde(default, deserialize_with = "flat_wont_reach")]
    pub wont_reach: WontReach,
    /// The person's held-out plays as confirmed: absent in confirmations
    /// made before they were kept.
    #[serde(default)]
    pub held_out: Option<Vec<String>>,
    /// No longer read: the list of features once confirmed.
    #[serde(default)]
    pub features: Option<toml::Value>,
}

/// The description as the confirmation keeps it: its sha256's first 16
/// hex digits, whitespace at the ends aside.
pub fn description_stamp(d: &str) -> String {
    crate::stamp::sha256(d.trim().as_bytes())[..16].to_string()
}

/// Branches no recording can reach, by address, with why.
pub type WontReach = BTreeMap<String, String>;

/// A confirmation's wont_reach: flat, or by feature as it was kept while
/// there were features.
fn flat_wont_reach<'de, D: serde::Deserializer<'de>>(d: D) -> Result<WontReach, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Item {
        Why(String),
        Feature(BTreeMap<String, String>),
    }
    let mut out = WontReach::new();
    for (k, v) in BTreeMap::<String, Item>::deserialize(d)? {
        match v {
            Item::Why(w) => {
                out.insert(k, w);
            }
            Item::Feature(m) => out.extend(m),
        }
    }
    Ok(out)
}

/// The recordings under `scenarios/`, as `<scenario>/<name>`.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize)]
pub struct Recordings {
    /// Every recording goal.toml does not hold out: what the port is
    /// built against.
    pub work: Vec<String>,
    /// The person's plays goal.toml holds out, on disk.
    pub held_out: Vec<String>,
}

impl Recordings {
    pub fn all(&self) -> impl Iterator<Item = &String> {
        self.work.iter().chain(&self.held_out)
    }
}

/// The scenario a recording (`<scenario>/<name>`) is of.
pub fn scenario_of(r: &str) -> &str {
    r.split_once('/').map_or(r, |(s, _)| s)
}

/// The scenarios of some recordings, in the order first named.
pub fn scenarios_of<'a>(recordings: impl IntoIterator<Item = &'a String>) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for r in recordings {
        let s = scenario_of(r);
        if !out.iter().any(|o| o == s) {
            out.push(s.to_string());
        }
    }
    out
}

/// An address in the catalogue: `0x` and up to eight hex digits.
pub fn entry_addr(s: &str) -> Option<u32> {
    let h = s.strip_prefix("0x")?;
    (!h.is_empty() && h.len() <= 8).then(|| u32::from_str_radix(h, 16).ok()).flatten()
}

/// An address range in the catalogue: `0xSTART..0xEND`, the end excluded.
pub fn entry_range(s: &str) -> Option<(u32, u32)> {
    let (a, b) = s.split_once("..")?;
    let (a, b) = (entry_addr(a)?, entry_addr(b)?);
    (a < b).then_some((a, b))
}

/// What the port's sources (game/, tools/) cite of the original: code
/// addresses (`0x8001_52A4`) and script stretches (`croc+0x1803..0x18A0`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Cited {
    pub addresses: BTreeSet<u32>,
    pub stretches: Vec<Stretch>,
}

/// Files larger than this are not the port's sources.
const SOURCE_MAX: u64 = 1 << 20;

impl Cited {
    /// What the text files under `root`'s [`SOURCES`] cite, `target/`
    /// and `.git` aside.
    pub fn read(root: &Path) -> Cited {
        let mut out = Cited::default();
        let mut todo: Vec<std::path::PathBuf> = SOURCES.iter().map(|s| root.join(s)).collect();
        while let Some(dir) = todo.pop() {
            let Ok(d) = std::fs::read_dir(&dir) else { continue };
            for e in d.flatten() {
                let (path, name) = (e.path(), e.file_name());
                let Ok(t) = e.file_type() else { continue };
                if t.is_dir() {
                    if name != "target" && name != ".git" {
                        todo.push(path);
                    }
                } else if t.is_file() && e.metadata().is_ok_and(|m| m.len() <= SOURCE_MAX) {
                    if let Ok(text) = std::fs::read_to_string(&path) {
                        out.scan(&text);
                    }
                }
            }
        }
        out.stretches.sort();
        out.stretches.dedup();
        out
    }

    /// Adds what `text` cites: `0x8` and seven hex digits (underscores
    /// between them allowed), and `name+0xA..0xB` or `name+0xA`.
    pub fn scan(&mut self, text: &str) {
        let b = text.as_bytes();
        let word = |c: u8| c.is_ascii_alphanumeric() || c == b'_';
        let mut i = 0;
        while let Some(at) = text[i..].find("0x").map(|o| o + i) {
            let mut end = at + 2;
            while end < b.len() && (b[end].is_ascii_hexdigit() || b[end] == b'_') {
                end += 1;
            }
            i = end;
            // Not part of a word, and not a script position (`croc+0x..`).
            if (at > 0 && (word(b[at - 1]) || b[at - 1] == b'+')) || (end < b.len() && word(b[end])) {
                continue;
            }
            let digits: String = text[at + 2..end].chars().filter(|c| *c != '_').collect();
            if digits.len() == 8 && digits.starts_with('8') {
                if let Ok(a) = u32::from_str_radix(&digits, 16) {
                    self.addresses.insert(a);
                }
            }
        }
        // Stretches: a name, `+`, a position or a range.
        let mut i = 0;
        while let Some(plus) = text[i..].find("+0x").map(|o| o + i) {
            i = plus + 3;
            let mut start = plus;
            while start > 0 && (b[start - 1].is_ascii_alphanumeric() || b[start - 1] == b'_' || b[start - 1] == b'-') {
                start -= 1;
            }
            let mut end = plus + 1;
            while end < b.len() && (b[end].is_ascii_hexdigit() || b[end] == b'x' || b[end] == b'.') {
                end += 1;
            }
            let token = text[start..end].trim_end_matches('.');
            if let Some(s) = Stretch::parse(token) {
                self.stretches.push(s);
            }
        }
    }
}

/// Whether the person's confirmation covers the goal as it is now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Confirmation {
    /// Never confirmed.
    Missing,
    /// The description, the assets and the person's held-out plays are
    /// as confirmed.
    Covers { at: String },
    /// What changed since: the description, assets no longer asked for,
    /// held-out plays deleted or moved to work.
    Shrunk { at: String, missing: Vec<String> },
}

/// A problem in the goal or the catalogue, for the agent to fix.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Problem {
    pub file: String,
    pub problem: String,
}

/// A port workspace's goal, recordings and catalogue, read once.
#[derive(Debug)]
pub struct Port {
    pub goal: Goal,
    pub recordings: Recordings,
    /// Each scenario's `assets`: the disc's assets its recordings need to
    /// replay, by name (as the goal's `assets` names them).
    pub scenario_assets: BTreeMap<String, Vec<String>>,
    /// `formats/<id>.md`: the disc's formats found so far.
    pub formats: BTreeMap<String, assets::Format>,
    /// `formats/unused.toml`: disc files the game never loads, with why.
    pub unused: BTreeMap<String, String>,
    /// [`NOT_PORTED`], by function entry or address range.
    pub not_ported: BTreeMap<String, String>,
    /// [`WONT_REACH`], by branch address, range or script position.
    pub wont_reach: WontReach,
    /// [`QUESTIONS`], open.
    pub questions: Vec<String>,
    /// What game/ and tools/ cite.
    pub cited: Cited,
    pub problems: Vec<Problem>,
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

/// What is wrong with a list of asset names.
fn asset_names_problems(file: &str, names: &[String], out: &mut Vec<Problem>) {
    let mut seen = BTreeSet::new();
    for n in names {
        if let Some(p) = asset_name_problem(n) {
            out.push(problem(file, format!("assets: {p}")));
        } else if !seen.insert(n) {
            out.push(problem(file, format!("assets: {n} is listed twice")));
        }
    }
}

/// A catalogue TOML file of key = reason, or the problem reading it.
fn reasons(root: &Path, file: &str, problems: &mut Vec<Problem>) -> BTreeMap<String, String> {
    match std::fs::read_to_string(root.join(file)) {
        Ok(text) => toml::from_str::<BTreeMap<String, String>>(&text).unwrap_or_else(|e| {
            problems.push(problem(file, format!("{} (each line: \"key\" = \"why\")", e.message())));
            BTreeMap::new()
        }),
        Err(_) => BTreeMap::new(),
    }
}

/// The open questions in questions.md's text: each `- ` line.
pub fn questions_in(text: &str) -> Vec<String> {
    text.lines()
        .filter_map(|l| {
            let l = l.trim();
            if l == "-" { Some(String::new()) } else { l.strip_prefix("- ").map(|q| q.trim().to_string()) }
        })
        .collect()
}

/// The recordings under `scenarios/`, by scenario, sorted.
fn recordings_on_disk(root: &Path) -> Vec<String> {
    let mut out = Vec::new();
    let Ok(d) = std::fs::read_dir(root.join(SCENARIOS)) else { return out };
    for e in d.flatten().filter(|e| e.file_type().is_ok_and(|t| t.is_dir())) {
        let s = e.file_name().to_string_lossy().into_owned();
        let Ok(files) = std::fs::read_dir(e.path()) else { continue };
        for f in files.flatten() {
            let n = f.file_name().to_string_lossy().into_owned();
            if let Some(name) = n.strip_suffix(".jsonl") {
                out.push(format!("{s}/{name}"));
            }
        }
    }
    out.sort();
    out
}

impl Port {
    /// The goal, recordings and catalogue under `root`. An error when
    /// `root` is not a port (no `goal.toml`) or the goal cannot be read;
    /// anything wrong in the rest is a problem.
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
        let dir = root.join(CATALOGUE);
        let mut files: Vec<String> = match std::fs::read_dir(&dir) {
            Ok(d) => d.flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect(),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
            Err(e) => return Err(Error::io(&dir, e)),
        };
        files.sort();
        for name in files.iter().filter(|n| n.starts_with("F-") && n.ends_with(".md")) {
            problems.push(problem(
                &format!("{CATALOGUE}/{name}"),
                "a feature's entry: there are no features now (the recordings are the unit); psx port update moves its questions, wont_reach, assets and held-out plays out",
            ));
        }
        let (formats, unused) = assets::read_formats(root, &mut problems)?;
        let not_ported = reasons(root, NOT_PORTED, &mut problems);
        let wont_reach = reasons(root, WONT_REACH, &mut problems);
        let questions = std::fs::read_to_string(root.join(QUESTIONS)).map(|t| questions_in(&t)).unwrap_or_default();
        let on_disk = recordings_on_disk(root);
        let recordings = Recordings {
            work: on_disk.iter().filter(|r| !goal.held_out.contains(r)).cloned().collect(),
            held_out: Vec::new(),
        };
        let mut recordings = recordings;
        for r in goal.held_out.iter().filter(|r| on_disk.contains(r)) {
            if !recordings.held_out.contains(r) {
                recordings.held_out.push(r.clone());
            }
        }
        let mut scenario_assets = BTreeMap::new();
        for s in scenarios_of(&on_disk) {
            let file = format!("{SCENARIOS}/{s}.toml");
            let Ok(text) = std::fs::read_to_string(root.join(&file)) else { continue };
            let Ok(v) = toml::from_str::<toml::Table>(&text) else { continue };
            let names: Vec<String> = match v.get("assets") {
                None => continue,
                Some(toml::Value::Array(a)) => a.iter().filter_map(|n| n.as_str().map(String::from)).collect(),
                Some(_) => {
                    problems.push(problem(&file, "assets: a list of asset names"));
                    continue;
                }
            };
            asset_names_problems(&file, &names, &mut problems);
            scenario_assets.insert(s, names);
        }
        let cited = Cited::read(root);
        let mut port = Port {
            goal,
            recordings,
            scenario_assets,
            formats,
            unused,
            not_ported,
            wont_reach,
            questions,
            cited,
            problems,
        };
        port.check(&on_disk);
        port.check_disk(root);
        Ok(port)
    }

    /// The problems in what was read.
    fn check(&mut self, on_disk: &[String]) {
        let mut out = Vec::new();
        if self.goal.description.trim().is_empty() {
            out.push(problem(GOAL, "no description: the goal in the person's words"));
        }
        if self.goal.features.is_some() {
            out.push(problem(
                GOAL,
                "features: no longer read (the recordings are the unit); psx port update takes it out",
            ));
        }
        asset_names_problems(GOAL, self.goal.assets.names(), &mut out);
        let mut seen = BTreeSet::new();
        for r in &self.goal.held_out {
            if !seen.insert(r) {
                out.push(problem(GOAL, format!("held_out: {r} is listed twice")));
            } else if !on_disk.contains(r) {
                out.push(problem(GOAL, format!("held_out: {r}: no such recording (scenario/name, under scenarios/)")));
            }
        }
        for (k, why) in &self.not_ported {
            if entry_addr(k).is_none() && entry_range(k).is_none() {
                out.push(problem(NOT_PORTED, format!("{k:?} is not a function's entry (0x…) or a range (0x…..0x…)")));
            }
            if why.trim().is_empty() {
                out.push(problem(NOT_PORTED, format!("{k}: no reason: what the code is, and why the port has none")));
            }
        }
        for (a, why) in &self.wont_reach {
            match (entry_addr(a).or(entry_range(a).map(|r| r.0)), Stretch::parse(a)) {
                (None, None) if !crate::flow::is_test(a) => out.push(problem(
                    WONT_REACH,
                    format!(
                        "{a:?} is not an address (0x8002D820), an address range (0x8002D800..0x8002D860, the end excluded), a test as port next gives it ([0x8007529C] == 2) or a script position or stretch (croc+0x18F0)"
                    ),
                )),
                (None, Some(w)) if !self.cited.stretches.iter().any(|s| s.contains(&w)) => out.push(problem(
                    WONT_REACH,
                    format!("{a} is in no script stretch the port cites (game/, tools/)"),
                )),
                _ => {}
            }
            if why.trim().is_empty() {
                out.push(problem(WONT_REACH, format!("{a}: no reason: why no recording can reach it")));
            }
        }
        if self.questions.iter().any(|q| q.is_empty()) {
            out.push(problem(QUESTIONS, "an empty question"));
        }
        self.problems.extend(out);
    }

    /// What the catalogue names that the inventory does not have.
    fn check_disk(&mut self, root: &Path) {
        let mut out = Vec::new();
        // Script positions are the binding's programs': only addresses are the inventory's.
        let branches = self.wont_reach.keys().any(|c| entry_addr(c).or(entry_range(c).map(|r| r.0)).is_some());
        let loaders: Vec<&assets::Format> = self.formats.values().filter(|f| !f.loader.is_empty()).collect();
        if !branches && loaders.is_empty() && self.not_ported.is_empty() {
            return;
        }
        let ctx = crate::commands::Ctx { root: root.to_path_buf(), cancel: None };
        let functions =
            crate::coverage::inventory(&ctx, None).and_then(|(_, inv)| crate::coverage::Functions::read(root, &inv));
        let fns = match &functions {
            Ok(fns) => fns,
            Err(err) => {
                if branches {
                    out.push(problem(WONT_REACH, format!("checked against the inventory: {}", err.message)));
                }
                for f in loaders {
                    out.push(problem(&f.file, format!("loader is checked against the inventory: {}", err.message)));
                }
                self.problems.extend(out);
                return;
            }
        };
        for k in self.not_ported.keys().filter(|k| entry_addr(k).is_some_and(|a| !fns.starts(a))) {
            out.push(problem(NOT_PORTED, format!("{k}: no function of the inventory starts there")));
        }
        for f in loaders {
            for l in f.loader.iter().filter(|l| entry_addr(l).is_some_and(|a| !fns.starts(a))) {
                out.push(problem(&f.file, format!("loader: no function of the inventory starts at {l}")));
            }
        }
        for a in self.wont_reach.keys() {
            if let Some((start, end)) = entry_range(a) {
                if fns.branches_in(start, end).is_empty() {
                    out.push(problem(WONT_REACH, format!("{a} holds no conditional branch of the inventory's code")));
                }
            } else if entry_addr(a).is_some_and(|addr| fns.branch(addr).is_none()) {
                out.push(problem(WONT_REACH, format!("{a} is not a conditional branch of the inventory's code")));
            }
        }
        self.problems.extend(out);
    }

    /// Why the function at `entry` is not ported, when not_ported.toml
    /// lists it or a range holding it.
    pub fn not_ported(&self, entry: u32) -> Option<&String> {
        not_ported_in(&self.not_ported, entry)
    }

    /// The scenarios of the work recordings, in order.
    pub fn work_scenarios(&self) -> Vec<String> {
        scenarios_of(&self.recordings.work)
    }

    /// The person's held-out plays confirmed that goal.toml no longer
    /// holds out: deleted, or moved to work.
    pub fn held_out_dropped(&self) -> Vec<String> {
        let Some(c) = &self.goal.confirmed else { return Vec::new() };
        c.held_out.iter().flatten().filter(|r| !self.goal.held_out.contains(r)).cloned().collect()
    }

    pub fn confirmation(&self) -> Confirmation {
        let Some(c) = &self.goal.confirmed else { return Confirmation::Missing };
        let at = c.at.clone();
        let mut missing: Vec<String> = Vec::new();
        if c.description.as_ref().is_some_and(|d| *d != description_stamp(&self.goal.description)) {
            missing.push("description".into());
        }
        missing.extend(self.held_out_dropped().into_iter().map(|r| format!("held_out {r}")));
        // Every asset name the person confirmed is still asked for.
        match (&c.assets, &self.goal.assets) {
            (_, Assets::All) => {}
            (Assets::All, _) => missing.push("assets = true".into()),
            (Assets::Names(was), Assets::Names(now)) => {
                missing.extend(was.iter().filter(|n| !now.contains(n)).map(|n| format!("assets {n}")))
            }
        }
        if missing.is_empty() { Confirmation::Covers { at } } else { Confirmation::Shrunk { at, missing } }
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

    /// The won't-reach branches the person has not confirmed as they
    /// stand: new, or with another reason.
    pub fn unconfirmed_wont_reach(&self) -> Vec<String> {
        self.wont_reach.iter().filter(|(a, why)| !self.wont_reach_agreed(a, why)).map(|(a, _)| a.clone()).collect()
    }

    /// The person confirmed branch `key` with reason `why`.
    pub fn wont_reach_agreed(&self, key: &str, why: &str) -> bool {
        self.goal.confirmed.as_ref().is_some_and(|c| c.wont_reach.get(key).is_some_and(|w| w == why))
    }
}

/// Why the function at `entry` is not ported, by not_ported.toml's
/// `map`: its entry or a range holding it.
pub fn not_ported_in(map: &BTreeMap<String, String>, entry: u32) -> Option<&String> {
    let entry = entry & 0x1FFF_FFFF;
    map.iter().find_map(|(k, why)| {
        let hit = match (entry_addr(k), entry_range(k)) {
            (Some(a), _) => a & 0x1FFF_FFFF == entry,
            (_, Some((s, e))) => (s & 0x1FFF_FFFF..e & 0x1FFF_FFFF).contains(&entry),
            _ => false,
        };
        hit.then_some(why)
    })
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A workspace with goal.toml and `files` (relative path, text).
    pub(crate) fn workspace(goal: &str, files: &[(&str, &str)]) -> (tempdir::Dir, Port) {
        let dir = tempdir::Dir::new();
        std::fs::write(dir.0.join(GOAL), goal).unwrap();
        std::fs::create_dir_all(dir.0.join(CATALOGUE)).unwrap();
        for (name, text) in files {
            let p = dir.0.join(name);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, text).unwrap();
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

    fn problems(port: &Port) -> Vec<String> {
        port.problems.iter().map(|p| format!("{}: {}", p.file, p.problem)).collect()
    }

    #[test]
    fn the_recordings_are_the_work_but_the_persons_plays() {
        let goal = "description = \"x\"\nheld_out = [\"s/play\", \"s/gone\", \"s/play\"]\n";
        let (_d, port) = workspace(
            goal,
            &[("scenarios/s/a.jsonl", ""), ("scenarios/s/play.jsonl", ""), ("scenarios/t/b.jsonl", "")],
        );
        assert_eq!(port.recordings.work, ["s/a", "t/b"]);
        assert_eq!(port.recordings.held_out, ["s/play"]);
        assert_eq!(port.work_scenarios(), ["s", "t"]);
        assert_eq!(
            problems(&port),
            [
                "goal.toml: held_out: s/gone: no such recording (scenario/name, under scenarios/)",
                "goal.toml: held_out: s/play is listed twice",
            ]
        );
    }

    #[test]
    fn a_feature_left_over_asks_for_port_update() {
        let (_d, port) = workspace(
            "description = \"x\"\nfeatures = [\"F-0001\"]\n",
            &[("catalogue/F-0001.md", "+++\nid = \"F-0001\"\n+++\n"), ("catalogue/README.md", "# not one")],
        );
        let p = problems(&port);
        assert_eq!(p.len(), 2, "{p:?}");
        assert!(
            p[0].starts_with("catalogue/F-0001.md: a feature's entry")
                && p[0].ends_with("psx port update moves its questions, wont_reach, assets and held-out plays out")
        );
        assert!(p[1].starts_with("goal.toml: features: no longer read"));
    }

    #[test]
    fn the_catalogue_is_three_files() {
        let np = "\"0x80010088\" = \"memcpy: the port has its own\"\n\"0x80050000..0x80060000\" = \"libgte\"\n\"main\" = \"x\"\n\"0x80070000\" = \" \"\n";
        let wr = "\"0x8002D820\" = \"\"\n\"0x8002D860..0x8002D800\" = \"backwards\"\n\"croc+0x1890\" = \"wall contact\"\n\"croc+0x2000\" = \"elsewhere\"\n";
        let qs = "# Questions\n\n- what resets the timer?\n-\n  - nested counts too\nnot a question\n";
        let (_d, port) = workspace(
            "description = \"x\"\n",
            &[
                (NOT_PORTED, np),
                (WONT_REACH, wr),
                (QUESTIONS, qs),
                ("game/src/jump.rs", "// The wall check: croc+0x1803..0x18A0.\n"),
            ],
        );
        assert_eq!(port.not_ported(0x0001_0088).map(String::as_str), Some("memcpy: the port has its own"));
        assert_eq!(port.not_ported(0x8005_8000).map(String::as_str), Some("libgte"));
        assert_eq!(port.not_ported(0x8006_0000), None);
        assert_eq!(port.questions, ["what resets the timer?", "", "nested counts too"]);
        let p = problems(&port);
        assert_eq!(
            p[..7],
            [
                "catalogue/not_ported.toml: 0x80070000: no reason: what the code is, and why the port has none",
                "catalogue/not_ported.toml: \"main\" is not a function's entry (0x…) or a range (0x…..0x…)",
                "catalogue/wont_reach.toml: 0x8002D820: no reason: why no recording can reach it",
                "catalogue/wont_reach.toml: \"0x8002D860..0x8002D800\" is not an address (0x8002D820), an address range (0x8002D800..0x8002D860, the end excluded), a test as port next gives it ([0x8007529C] == 2) or a script position or stretch (croc+0x18F0)",
                "catalogue/wont_reach.toml: croc+0x2000 is in no script stretch the port cites (game/, tools/)",
                "catalogue/questions.md: an empty question",
                "catalogue/wont_reach.toml: checked against the inventory: .psx/inventory.json: no inventory; take one with inventory first"
                    .replace(".psx/inventory.json", &_d.0.join(".psx/inventory.json").display().to_string())
                    .as_str(),
            ]
        );
    }

    #[test]
    fn the_port_cites_code_and_script_by_address() {
        let mut c = Cited::default();
        c.scan(
            "t.store(0x800A_1C30, 4).by(0x8001_52A4); // 0x8001_52a8 and 0x80012345.\n\
             // not: 0x1F800000, 0x800123, 0x8001234567, x0x80012345, y = 0x80099999z\n\
             // croc+0x1803..0x18A0 and croc_2+0x10. and croc+zz",
        );
        assert_eq!(c.addresses, BTreeSet::from([0x8001_52A4, 0x8001_52A8, 0x800A_1C30, 0x8001_2345]));
        assert_eq!(
            c.stretches,
            [Stretch::parse("croc+0x1803..0x18A0").unwrap(), Stretch::parse("croc_2+0x10").unwrap()]
        );
        // Scanned under game/ and tools/, target/ aside.
        let (_d, port) = workspace(
            "description = \"x\"\n",
            &[
                ("game/src/a.rs", "// 0x80011000"),
                ("tools/import/b.rs", "// 0x80022000"),
                ("game/target/c.rs", "// 0x80033000"),
                ("glossary/src/lib.rs", "// 0x80044000"),
            ],
        );
        assert_eq!(port.cited.addresses, BTreeSet::from([0x8001_1000, 0x8002_2000]));
    }

    #[test]
    fn a_scenario_names_the_assets_its_recordings_need() {
        let (_d, port) = workspace(
            "description = \"x\"\n",
            &[
                ("scenarios/s.toml", "moment = \"m\"\nassets = [\"format:level\", \"MAPS\"]\n"),
                ("scenarios/s/a.jsonl", ""),
                ("scenarios/t.toml", "assets = \"x\"\n"),
                ("scenarios/t/a.jsonl", ""),
            ],
        );
        assert_eq!(port.scenario_assets["s"], ["format:level", "MAPS"]);
        assert_eq!(
            problems(&port),
            [
                "scenarios/s.toml: assets: \"MAPS\" is not a disc path from / (/MAPS/*.WLD, /ARCHIVE:ENTRY) or format:<id>",
                "scenarios/t.toml: assets: a list of asset names",
            ]
        );
    }

    #[test]
    fn wont_reach_branches_need_the_persons_confirmation_as_they_stand() {
        let goal =
            "description = \"x\"\nconfirmed = { at = \"t\", wont_reach = { \"0x8002D820\" = \"under water\" } }\n";
        let wr = |why: &str| format!("\"0x8002D820\" = \"{why}\"\n");
        let (_d, port) = workspace(goal, &[(WONT_REACH, &wr("under water"))]);
        assert_eq!(port.unconfirmed_wont_reach(), Vec::<String>::new());
        let (_d, port) = workspace(goal, &[(WONT_REACH, &wr("in a cutscene"))]);
        assert_eq!(port.unconfirmed_wont_reach(), ["0x8002D820"]);
        assert_eq!(port.confirmation_json()["wont_reach_unconfirmed"], json!(["0x8002D820"]));
        // Kept by feature before features went, it reads flat.
        let old = "description = \"x\"\nconfirmed = { at = \"t\", features = [\"F-0001\"], wont_reach = { \"F-0001\" = { \"0x8002D820\" = \"under water\" } } }\n";
        let (_d, port) = workspace(old, &[(WONT_REACH, &wr("under water"))]);
        assert_eq!(port.unconfirmed_wont_reach(), Vec::<String>::new());
    }

    #[test]
    fn a_new_goal_asks_for_a_description() {
        let (_dir, port) = workspace(GOAL_TOML, &[]);
        assert_eq!(problems(&port), ["goal.toml: no description: the goal in the person's words"]);
    }

    #[test]
    fn a_confirmation_covers_the_description_assets_and_the_persons_plays() {
        let files = [("scenarios/s/a.jsonl", ""), ("scenarios/s/play.jsonl", "")];
        let (_d, port) = workspace("description = \"drive\"\n", &files);
        assert_eq!(port.confirmation(), Confirmation::Missing);
        let stamped = format!(
            "description = \"drive\"\nheld_out = [\"s/play\"]\nconfirmed = {{ at = \"t\", description = \"{}\", held_out = [\"s/play\"] }}\n",
            description_stamp("drive")
        );
        let (_d, port) = workspace(&stamped, &files);
        assert_eq!(port.confirmation(), Confirmation::Covers { at: "t".into() });
        let (_d, port) = workspace(&stamped.replacen("\"drive\"", "\"drive fast\"", 1), &files);
        assert_eq!(port.confirmation(), Confirmation::Shrunk { at: "t".into(), missing: vec!["description".into()] });
        // The person's play moved to work asks the person.
        let (_d, port) = workspace(&stamped.replacen("held_out = [\"s/play\"]\n", "", 1), &files);
        assert_eq!(
            port.confirmation(),
            Confirmation::Shrunk { at: "t".into(), missing: vec!["held_out s/play".into()] }
        );
        assert_eq!(port.recordings.work, ["s/a", "s/play"]);
        // A confirmation from when there were features reads, its list unread.
        let (_d, port) = workspace("description = \"x\"\nconfirmed = { at = \"t\", features = \"all\" }\n", &files);
        assert_eq!(port.confirmation(), Confirmation::Covers { at: "t".into() });
    }

    #[test]
    fn a_goal_asks_for_every_asset_none_or_names_the_person_confirms() {
        let goal = |a: &str| toml::from_str::<Goal>(&format!("assets = {a}")).map(|g| g.assets);
        assert_eq!(goal("true").unwrap(), Assets::All);
        assert_eq!(goal("false").unwrap(), Assets::Names(vec![]));
        assert_eq!(goal("[\"format:level\"]").unwrap(), Assets::Names(vec!["format:level".into()]));
        assert!(goal("\"some\"").is_err());
        assert!(toml::from_str::<Goal>("feature = []").is_err());
        assert_eq!(toml::from_str::<Goal>(GOAL_TOML).unwrap().assets, Assets::Names(vec![]));
        let confirmed = "confirmed = { at = \"t\", assets = [\"format:level\", \"format:car\"] }\n";
        let (_d, port) =
            workspace(&format!("description = \"x\"\nassets = [\"format:level\", \"lvl\"]\n{confirmed}"), &[]);
        assert_eq!(
            problems(&port),
            ["goal.toml: assets: \"lvl\" is not a disc path from / (/MAPS/*.WLD, /ARCHIVE:ENTRY) or format:<id>"]
        );
        // A name the person confirmed taken out needs them again; every file covers it.
        assert_eq!(
            port.confirmation(),
            Confirmation::Shrunk { at: "t".into(), missing: vec!["assets format:car".into()] }
        );
        let (_d, port) = workspace(&format!("assets = true\n{confirmed}"), &[]);
        assert!(matches!(port.confirmation(), Confirmation::Covers { .. }));
    }
}
