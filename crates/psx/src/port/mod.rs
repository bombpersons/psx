//! A port: one workspace where one agent investigates the original and
//! builds the port (docs/port-design.md). `port init` writes it.
//! The goal (`goal.toml`) says which features the person wants, the
//! catalogue (`catalogue/F-NNNN.md`) describes the game's features in game
//! terms, and `port confirm` records the person's approval of the goal's
//! list. The goal's features are its closure; each has a stage.

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
/// The original's code no feature ports, by function or address range,
/// with why.
pub const NOT_PORTED: &str = "catalogue/not_ported.toml";

pub(crate) const GOAL_TOML: &str = "\
# What the person wants from this port. The agent proposes the features
# (catalogue/F-NNNN.md) and lists them here; the person confirms the list
# with `psx port confirm --yes`, which writes `confirmed`. Features merge,
# split and come and go after that as the agent learns; the description,
# the assets and the person's held-out plays changing needs confirming again.
description = \"\"
features = []   # feature ids, or \"all\"
assets = false  # what to extract: true (every disc file), false, or names: [\"format:level\", \"/MAPS/*.WLD\"] (formats/README.md)
# review_lines = 800  # a follow-up review once the code changed by this many lines since the last
# held_out = []  # the goal's own plays, scenario/name, once every feature is reviewed
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
    #[serde(default)]
    pub features: Features,
    /// The disc's assets the person wants extracted: each file they cover
    /// imported and checked, or unused. The goal's features bring in their
    /// own besides.
    #[serde(default)]
    pub assets: Assets,
    #[serde(default)]
    pub confirmed: Option<Confirmed>,
    /// A follow-up review comes due once the code has changed by this many
    /// lines (added and removed) in the commits since the last review,
    /// whatever the features do; [`REVIEW_LINES`] when unset.
    #[serde(default)]
    pub review_lines: Option<usize>,
    /// The goal's own held-out plays (scenario/name), requested from its
    /// description once every feature is reviewed: judged by a whole
    /// psx port check as [`GOAL_ID`], and shown once they are.
    #[serde(default)]
    pub held_out: Vec<String>,
}

/// The verdict the goal's held-out plays are judged under.
pub const GOAL_ID: &str = "goal";

/// The lines of change that ask for a follow-up review when goal.toml does
/// not say: on Croc, +931/-534 lines went unreviewed while no feature
/// matched or finished.
pub const REVIEW_LINES: usize = 800;

impl Goal {
    pub fn review_lines(&self) -> usize {
        self.review_lines.unwrap_or(REVIEW_LINES)
    }
}

/// The person's confirmation: when, the list they confirmed, and the
/// branches they agreed no recording can reach (feature, address, reason).
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Confirmed {
    pub at: String,
    /// The list first confirmed; ids leaving it later are the agent's cut.
    pub features: Features,
    /// The description confirmed, as [`description_stamp`]: absent in
    /// confirmations made before it was kept.
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub assets: Assets,
    #[serde(default)]
    pub wont_reach: WontReach,
}

/// The description as the confirmation keeps it: its sha256's first 16
/// hex digits, whitespace at the ends aside.
pub fn description_stamp(d: &str) -> String {
    crate::stamp::sha256(d.trim().as_bytes())[..16].to_string()
}

/// The recordings the goal's features have named (`<scenario>/<name>` =
/// work or held_out), as port next last saw them.
pub const GOAL_RECORDINGS: &str = ".psx/goal_recordings.json";

/// Branches no recording can reach, by feature, then address, with why.
pub type WontReach = BTreeMap<String, BTreeMap<String, String>>;

/// One catalogue entry: `catalogue/F-NNNN.md`, its TOML front matter
/// between `+++` lines, then a paragraph in game terms.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Entry {
    pub id: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub area: String,
    /// No longer read: features have no dependencies (a failure's
    /// pointer says whose code it is in). Kept so an entry that has it
    /// loads; port update keeps the closure it made in the goal.
    #[serde(default)]
    pub depends: Vec<String>,
    /// The original's functions that are the feature, by entry address:
    /// found with psx (coverage diff, watches), each one the inventory has;
    /// and stretches of script programs the glossary's binding names
    /// (`croc+0x1803..0x18A0`).
    #[serde(default)]
    pub code: Vec<String>,
    #[serde(default)]
    pub recordings: Recordings,
    /// The disc's assets its recordings need to replay, by name (as the
    /// goal's `assets` names them): brought in whatever the goal asks.
    #[serde(default)]
    pub assets: Vec<String>,
    /// Branches of its code no recording can reach, by address (or a
    /// script position or stretch in one of its code's), and why; the
    /// person confirms them (`port confirm`).
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
#[derive(Debug, Clone, Default, Deserialize, serde::Serialize)]
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

/// An address range in an entry: `0xSTART..0xEND`, the end excluded.
pub fn entry_range(s: &str) -> Option<(u32, u32)> {
    let (a, b) = s.split_once("..")?;
    let (a, b) = (entry_addr(a)?, entry_addr(b)?);
    (a < b).then_some((a, b))
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
    /// open, and the code's review holds.
    Reviewed,
    /// Its held-out recordings pass, and no scenario turns off its code.
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

/// A port workspace's goal and catalogue, read once.
#[derive(Debug)]
pub struct Port {
    pub goal: Goal,
    pub entries: BTreeMap<String, Entry>,
    /// `formats/<id>.md`: the disc's formats found so far.
    pub formats: BTreeMap<String, assets::Format>,
    /// `formats/unused.toml`: disc files the game never loads, with why.
    pub unused: BTreeMap<String, String>,
    /// `catalogue/not_ported.toml`: the original's code no feature ports,
    /// by function entry or address range, with why.
    pub not_ported: BTreeMap<String, String>,
    /// [`GOAL_RECORDINGS`]: every recording a goal feature has named, as
    /// work or held_out (held_out once it was).
    pub ledger: BTreeMap<String, String>,
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
        match (entry_addr(c), Stretch::parse(c)) {
            (None, None) => out.push(problem(
                f,
                format!("code: {c:?} is not an address (0x8002D810) or a script stretch (croc+0x1803..0x18A0)"),
            )),
            (Some(a), _) if !code.insert(a) => out.push(problem(f, format!("code: {c} is listed twice"))),
            _ => {}
        }
    }
    let script: Vec<Stretch> = e.code.iter().filter_map(|c| Stretch::parse(c)).collect();
    for (a, why) in &e.wont_reach {
        match (entry_addr(a).or(entry_range(a).map(|r| r.0)), Stretch::parse(a)) {
            (None, None) if !crate::flow::is_test(a) => out.push(problem(
                f,
                format!(
                    "wont_reach: {a:?} is not an address (0x8002D820), an address range (0x8002D800..0x8002D860, the end excluded), a test as port next gives it ([0x8007529C] == 2) or a script position or stretch (croc+0x18F0)"
                ),
            )),
            (None, Some(w)) if !script.iter().any(|s| s.contains(&w)) => {
                out.push(problem(f, format!("wont_reach: {a} is in no script stretch code lists")))
            }
            _ => {}
        }
        if why.trim().is_empty() {
            out.push(problem(f, format!("wont_reach.{a}: no reason: why no recording can reach it")));
        }
    }
    if e.questions.iter().any(|q| q.trim().is_empty()) {
        out.push(problem(f, "questions: an empty question"));
    }
    asset_names_problems(f, &e.assets, out);
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
        let not_ported = match std::fs::read_to_string(root.join(NOT_PORTED)) {
            Ok(text) => toml::from_str::<BTreeMap<String, String>>(&text).unwrap_or_else(|e| {
                problems.push(problem(NOT_PORTED, e.message()));
                BTreeMap::new()
            }),
            Err(_) => BTreeMap::new(),
        };
        for (k, why) in &not_ported {
            if entry_addr(k).is_none() && entry_range(k).is_none() {
                problems
                    .push(problem(NOT_PORTED, format!("{k:?} is not a function's entry (0x…) or a range (0x…..0x…)")));
            }
            if why.trim().is_empty() {
                problems
                    .push(problem(NOT_PORTED, format!("{k}: no reason: what the code is, and why the port has none")));
            }
        }
        let ledger = std::fs::read(root.join(GOAL_RECORDINGS))
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default();
        let mut port = Port { goal, entries, formats, unused, not_ported, ledger, problems };
        port.check();
        port.check_ledger(root);
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
        // Script stretches are the binding's programs': only addresses are the inventory's.
        let naming: Vec<&Entry> = self
            .entries
            .values()
            .filter(|e| {
                e.code
                    .iter()
                    .chain(e.wont_reach.keys())
                    .any(|c| entry_addr(c).or(entry_range(c).map(|r| r.0)).is_some())
            })
            .collect();
        let loaders: Vec<&assets::Format> = self.formats.values().filter(|f| !f.loader.is_empty()).collect();
        if !naming.is_empty() || !loaders.is_empty() || !self.not_ported.is_empty() {
            let ctx = crate::commands::Ctx { root: root.to_path_buf(), cancel: None };
            let functions = crate::coverage::inventory(&ctx, None)
                .and_then(|(_, inv)| crate::coverage::Functions::read(root, &inv));
            if let Ok(fns) = &functions {
                for k in self.not_ported.keys().filter(|k| entry_addr(k).is_some_and(|a| !fns.starts(a))) {
                    out.push(problem(NOT_PORTED, format!("{k}: no function of the inventory starts there")));
                }
            }
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
                let named = |holders: &[u32]| {
                    let holders: Vec<String> = holders.iter().map(|h| format!("0x{h:08X}")).collect();
                    if holders.is_empty() { "no function".to_string() } else { holders.join(", ") }
                };
                for a in e.wont_reach.keys() {
                    if let Some((start, end)) = entry_range(a) {
                        let branches = fns.branches_in(start, end);
                        let outside: Vec<u32> = branches
                            .iter()
                            .filter(|(_, h)| !h.iter().any(|h| code.contains(h)))
                            .flat_map(|(_, h)| h.iter().copied())
                            .collect::<BTreeSet<u32>>()
                            .into_iter()
                            .collect();
                        if branches.is_empty() {
                            out.push(problem(
                                f,
                                format!("wont_reach: {a} holds no conditional branch of the inventory's code"),
                            ));
                        } else if branches.iter().any(|(_, h)| !h.iter().any(|h| code.contains(h))) {
                            out.push(problem(
                                f,
                                format!(
                                    "wont_reach: {a} holds branches of {}, which code does not list",
                                    named(&outside)
                                ),
                            ));
                        }
                        continue;
                    }
                    let Some(addr) = entry_addr(a) else { continue };
                    match fns.branch(addr) {
                        None => out.push(problem(
                            f,
                            format!("wont_reach: {a} is not a conditional branch of the inventory's code"),
                        )),
                        Some(holders) if !holders.iter().any(|h| code.contains(h)) => out.push(problem(
                            f,
                            format!("wont_reach: {a} is in {}, which code does not list (code taken out takes its entries; moved, they go with it)", named(&holders)),
                        )),
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
        }
        if self.goal.description.trim().is_empty() {
            out.push(problem(GOAL, "no description: the goal in the person's words"));
        }
        if self.goal.features == Features::List(Vec::new()) {
            out.push(problem(GOAL, "names no features: list the catalogue entries the goal needs, or \"all\""));
        }
        asset_names_problems(GOAL, self.goal.assets.names(), &mut out);
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

    /// The goal's recordings now: each named by a goal feature, as work or
    /// held_out (held_out when any names it so).
    pub fn goal_recordings(&self) -> BTreeMap<String, String> {
        let mut out = BTreeMap::new();
        for e in self.goal_ids().iter().filter_map(|id| self.entries.get(id)) {
            for r in &e.recordings.work {
                out.entry(r.clone()).or_insert_with(|| "work".to_string());
            }
            for r in &e.recordings.held_out {
                out.insert(r.clone(), "held_out".to_string());
            }
        }
        for r in &self.goal.held_out {
            out.insert(r.clone(), "held_out".to_string());
        }
        out
    }

    /// The ledger's recordings no goal feature names while their files are
    /// there: a merge or a split moves recordings, never drops them.
    fn check_ledger(&mut self, root: &Path) {
        for r in &self.goal.held_out {
            let file = r.split_once('/').map(|(s, n)| root.join("scenarios").join(s).join(format!("{n}.jsonl")));
            let why = if !file.is_some_and(|f| f.is_file()) {
                "held_out: no such recording (scenario/name, under scenarios/)"
            } else if self.entries.values().any(|e| e.recordings.work.contains(r)) {
                "held_out: also a feature's work recording; the goal's plays are held out from the work"
            } else {
                continue;
            };
            self.problems.push(problem(GOAL, format!("{r}: {why}")));
        }
        let now = self.goal_recordings();
        for r in self.ledger.keys().filter(|r| !now.contains_key(*r)) {
            let Some((s, n)) = r.split_once('/') else { continue };
            if root.join("scenarios").join(s).join(format!("{n}.jsonl")).is_file() {
                self.problems.push(problem(
                    CATALOGUE,
                    format!("{r}: a goal feature named it and none does now: name it under the recordings of the feature it shows (a merge or a split moves recordings)"),
                ));
            }
        }
    }

    /// The person's held-out recordings that left the goal or held_out
    /// since they confirmed: deleted, or moved to work.
    pub fn held_out_dropped(&self) -> Vec<String> {
        let now = self.goal_recordings();
        self.ledger
            .iter()
            .filter(|(r, k)| *k == "held_out" && now.get(*r).map(String::as_str) != Some("held_out"))
            .map(|(r, _)| r.clone())
            .collect()
    }

    /// The ledger with the goal's recordings now added; a work recording
    /// no feature names and whose file is gone leaves it. `confirmed`:
    /// the person accepted what left, so it is the goal's recordings now.
    pub(crate) fn note_recordings(&self, root: &Path, confirmed: bool) {
        let now = self.goal_recordings();
        let mut ledger = if confirmed { BTreeMap::new() } else { self.ledger.clone() };
        ledger.retain(|r, k| {
            let file = r.split_once('/').map(|(s, n)| root.join("scenarios").join(s).join(format!("{n}.jsonl")));
            now.contains_key(r) || k == "held_out" || file.is_some_and(|f| f.is_file())
        });
        for (r, k) in now {
            let e = ledger.entry(r).or_insert_with(|| k.clone());
            if k == "held_out" {
                *e = k;
            }
        }
        if ledger != self.ledger {
            let path = root.join(GOAL_RECORDINGS);
            let _ = std::fs::create_dir_all(root.join(".psx"));
            let _ = crate::write_atomic(&path, serde_json::to_string_pretty(&ledger).unwrap_or_default().as_bytes());
        }
    }

    /// Why the function at `entry` is not ported, when not_ported.toml
    /// lists it or a range holding it.
    pub fn not_ported(&self, entry: u32) -> Option<&String> {
        not_ported_in(&self.not_ported, entry)
    }

    /// The features whose code lists the function at `entry`.
    pub fn claiming(&self, entry: u32) -> Vec<&str> {
        let entry = entry & 0x1FFF_FFFF;
        self.entries
            .values()
            .filter(|e| e.code.iter().filter_map(|c| entry_addr(c)).any(|a| a & 0x1FFF_FFFF == entry))
            .map(|e| e.id.as_str())
            .collect()
    }

    /// The goal's features as ids: its list, or every catalogued feature.
    pub fn goal_ids(&self) -> BTreeSet<String> {
        match &self.goal.features {
            Features::All => self.entries.keys().cloned().collect(),
            Features::List(l) => l.iter().cloned().collect(),
        }
    }

    /// The goal's features, by id.
    pub fn closure(&self) -> Vec<String> {
        self.goal_ids().into_iter().collect()
    }

    /// The features the goal's list and the entries' `depends` brought in
    /// before `depends` went, which the goal does not list: what port
    /// update adds to it. None when the goal is every feature.
    pub(crate) fn brought_by_depends(&self) -> Vec<String> {
        let Features::List(listed) = &self.goal.features else { return Vec::new() };
        let mut ids: BTreeSet<String> = BTreeSet::new();
        let mut todo: Vec<String> = listed.clone();
        while let Some(id) = todo.pop() {
            if ids.insert(id.clone()) {
                if let Some(e) = self.entries.get(&id) {
                    todo.extend(
                        e.depends.iter().filter(|d| !ids.contains(*d) && self.entries.contains_key(*d)).cloned(),
                    );
                }
            }
        }
        ids.into_iter().filter(|id| !listed.contains(id)).collect()
    }

    /// Lists the features `depends` brought into the goal in goal.toml,
    /// now that it is no longer read, so the goal and its confirmation keep
    /// them; returns them. Nothing without a goal.toml.
    pub(crate) fn keep_depends(root: &Path, dry_run: bool) -> crate::Result<Vec<String>> {
        if !root.join(GOAL).is_file() {
            return Ok(Vec::new());
        }
        let port = Port::load(root)?;
        let brought = port.brought_by_depends();
        let Features::List(listed) = &port.goal.features else { return Ok(brought) };
        if brought.is_empty() || dry_run {
            return Ok(brought);
        }
        let path = root.join(GOAL);
        let text = std::fs::read_to_string(&path).map_err(|e| crate::Error::io(&path, e))?;
        let all: Vec<String> = listed.iter().chain(&brought).cloned().collect();
        let new = with_features(&text, &all);
        let read: Goal = toml::from_str(&new)
            .map_err(|e| crate::Error::invalid(format!("{GOAL} rewritten unreadably: {}", e.message())))?;
        if read.features != Features::List(all) || read.confirmed != port.goal.confirmed {
            return Err(crate::Error::invalid(format!("{GOAL}: its features could not be rewritten; nothing written")));
        }
        std::fs::write(&path, new).map_err(|e| crate::Error::io(&path, e))?;
        Ok(brought)
    }

    pub fn confirmation(&self) -> Confirmation {
        let Some(c) = &self.goal.confirmed else { return Confirmation::Missing };
        let at = c.at.clone();
        // The cut is the agent's: ids leaving are not the goal shrinking;
        // its description changing, or the person's plays leaving, is.
        let mut missing: Vec<String> = Vec::new();
        if c.description.as_ref().is_some_and(|d| *d != description_stamp(&self.goal.description)) {
            missing.push("description".into());
        }
        missing.extend(self.held_out_dropped().into_iter().map(|r| format!("held_out {r}")));
        // Every asset name the person confirmed is still asked for.
        let assets: Vec<String> = match (&c.assets, &self.goal.assets) {
            (_, Assets::All) => Vec::new(),
            (Assets::All, _) => vec!["assets = true".into()],
            (Assets::Names(was), Assets::Names(now)) => {
                was.iter().filter(|n| !now.contains(n)).map(|n| format!("assets {n}")).collect()
            }
        };
        let missing: Vec<String> = missing.into_iter().chain(assets).collect();
        if missing.is_empty() { Confirmation::Covers { at } } else { Confirmation::Shrunk { at, missing } }
    }

    /// The closure as the person reads it before confirming, with each
    /// feature's stage.
    pub fn closure_json(&self, stage: &dyn Fn(&str) -> Option<Stage>) -> Value {
        let rows: Vec<Value> = self
            .closure()
            .iter()
            .map(|id| {
                let e = self.entries.get(id);
                json!({
                    "id": id,
                    "title": e.map(|e| e.title.as_str()),
                    "area": e.map(|e| e.area.as_str()),
                    "stage": stage(id).map(Stage::name),
                    "code": e.map(|e| &e.code),
                    "recordings": e.map(|e| &e.recordings),
                    "assets": e.map(|e| &e.assets),
                    "wont_reach": e.map(|e| &e.wont_reach),
                    "questions": e.map(|e| &e.questions),
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
        let mut out = Vec::new();
        for (id, branches) in self.wont_reach() {
            for (a, why) in branches {
                if !self.wont_reach_agreed(&a, &why) {
                    out.push(format!("{id} {a}"));
                }
            }
        }
        out
    }

    /// The person confirmed branch `key` with reason `why`, under any
    /// feature: an entry follows its code from one feature to another.
    pub fn wont_reach_agreed(&self, key: &str, why: &str) -> bool {
        let confirmed = self.goal.confirmed.as_ref().map(|c| &c.wont_reach);
        confirmed.is_some_and(|c| c.values().any(|b| b.get(key).is_some_and(|w| w == why)))
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

/// `goal.toml`'s text with its top-level `features` set to `ids`, on one
/// line where the key was (or before the first table), the rest kept.
fn with_features(text: &str, ids: &[String]) -> String {
    let line = format!("features = {}", toml::Value::from(ids.to_vec()));
    let mut out: Vec<String> = Vec::new();
    let (mut at, mut depth, mut in_table) = (None, 0i32, false);
    for l in text.lines() {
        let t = l.trim_start();
        if depth > 0 {
            depth += t.matches('[').count() as i32 - t.matches(']').count() as i32;
            continue;
        }
        if t.starts_with('[') {
            in_table = true;
        }
        if !in_table && t.split(['=', ' ', '\t']).next() == Some("features") {
            at = Some(out.len());
            let value = t.split_once('=').map(|(_, v)| v).unwrap_or("");
            depth = value.matches('[').count() as i32 - value.matches(']').count() as i32;
            continue;
        }
        out.push(l.to_string());
    }
    let at = at.unwrap_or_else(|| out.iter().position(|l| l.trim_start().starts_with('[')).unwrap_or(out.len()));
    out.insert(at, line);
    let mut s = out.join("\n");
    s.push('\n');
    s
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
    fn the_closure_is_the_goal_and_depends_is_not_read() {
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
        assert_eq!(port.closure(), ["F-0003"]);
        let rows = port.closure_json(&|id| port.entries.contains_key(id).then_some(Stage::Catalogued));
        assert_eq!(rows.as_array().unwrap().len(), 1);
        assert_eq!(port.brought_by_depends(), ["F-0001", "F-0002"]);
    }

    #[test]
    fn port_update_lists_what_depends_brought_in_and_keeps_the_confirmation() {
        let (a, b, c) = (feature("F-0001", ""), feature("F-0002", "F-0001"), feature("F-0003", "F-0002"));
        let goal = "description = \"x\"\nfeatures = [\n  \"F-0003\",\n]\nconfirmed = { at = \"t\", features = [\"F-0001\", \"F-0002\", \"F-0003\"] }\n# features: the goal's list\n";
        let (dir, port) = workspace(goal, &[("F-0001.md", &a), ("F-0002.md", &b), ("F-0003.md", &c)]);
        assert_eq!(port.closure(), ["F-0003"]);
        assert_eq!(Port::keep_depends(&dir.0, true).unwrap(), ["F-0001", "F-0002"]);
        assert_eq!(std::fs::read_to_string(dir.0.join(GOAL)).unwrap(), goal);
        assert_eq!(Port::keep_depends(&dir.0, false).unwrap(), ["F-0001", "F-0002"]);
        let text = std::fs::read_to_string(dir.0.join(GOAL)).unwrap();
        assert!(text.contains("features = [\"F-0003\", \"F-0001\", \"F-0002\"]\nconfirmed"), "{text}");
        assert!(text.ends_with("# features: the goal's list\n"), "{text}");
        let port = Port::load(&dir.0).unwrap();
        assert_eq!(port.confirmation(), Confirmation::Covers { at: "t".into() });
        assert_eq!(Port::keep_depends(&dir.0, false).unwrap(), Vec::<String>::new());
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
                "goal.toml: F-0007 has no catalogue entry",
                "goal.toml: F-0001 is listed twice",
            ]
        );
        // A cycle in depends is no problem: it is not read.
        assert_eq!(port.closure(), ["F-0001", "F-0007"]);
    }

    #[test]
    fn an_entry_names_recordings_code_and_branches_the_workspace_has() {
        let entry = |extra: &str| {
            format!("+++\nid = \"F-0001\"\ntitle = \"Jump\"\narea = \"player\"\n{extra}\n+++\nWhat it does.\n")
        };
        let goal = "description = \"x\"\nfeatures = [\"F-0001\"]\n";
        let fields = "code = [\"0x8002D810\", \"jump\", \"0x8002D810\"]\nquestions = [\"\"]\n\
            [recordings]\nwork = [\"jump/stomp\", \"jump\"]\nheld_out = [\"jump/stomp\"]\n\
            [wont_reach]\n\"0x8002D820\" = \"\"\n\"0x8002D860..0x8002D800\" = \"backwards\"";
        let (d, port) = workspace(goal, &[("F-0001.md", &entry(fields))]);
        let p: Vec<&str> = port.problems.iter().map(|p| p.problem.as_str()).collect();
        assert_eq!(
            p,
            [
                "recordings: \"jump\" is not <scenario>/<name>",
                "recordings: jump/stomp is listed twice (work and held_out are apart)",
                "code: \"jump\" is not an address (0x8002D810) or a script stretch (croc+0x1803..0x18A0)",
                "code: 0x8002D810 is listed twice",
                "wont_reach.0x8002D820: no reason: why no recording can reach it",
                "wont_reach: \"0x8002D860..0x8002D800\" is not an address (0x8002D820), an address range (0x8002D800..0x8002D860, the end excluded), a test as port next gives it ([0x8007529C] == 2) or a script position or stretch (croc+0x18F0)",
                "questions: an empty question",
                "recordings: no scenarios/jump/stomp.jsonl (psx scenario record, or play)",
                "code and wont_reach are checked against the inventory: .psx/inventory.json: no inventory; take one with inventory first",
            ]
            .map(|l| l.replace(".psx/inventory.json", &d.0.join(".psx/inventory.json").display().to_string()))
        );
        let e = &port.entries["F-0001"];
        assert_eq!(e.scenarios(), ["jump"]);
        // Script stretches need no inventory; a wont_reach in one lies in a stretch code lists.
        let fields = "code = [\"croc+0x1803..0x18A0\"]\n[wont_reach]\n\"croc+0x1890\" = \"wall contact\"\n\
            \"croc+0x2000\" = \"elsewhere\"\n\"croc+\" = \"bad\"";
        let (_d, port) = workspace(goal, &[("F-0001.md", &entry(fields))]);
        let p: Vec<&str> = port.problems.iter().map(|p| p.problem.as_str()).collect();
        assert_eq!(
            p,
            [
                "wont_reach: \"croc+\" is not an address (0x8002D820), an address range (0x8002D800..0x8002D860, the end excluded), a test as port next gives it ([0x8007529C] == 2) or a script position or stretch (croc+0x18F0)",
                "wont_reach: croc+0x2000 is in no script stretch code lists",
            ]
        );
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
        // Moved with its code to another feature, the confirmation follows it.
        let moved = entry("under water").replace("F-0001", "F-0002");
        let goal2 = goal.replace("features = [\"F-0001\"]\n", "features = [\"F-0001\", \"F-0002\"]\n");
        let bare = "+++\nid = \"F-0001\"\ntitle = \"Jump\"\narea = \"player\"\n+++\nWhat it does.\n";
        let (_d, port) = workspace(&goal2, &[("F-0001.md", bare), ("F-0002.md", &moved)]);
        assert_eq!(port.unconfirmed_wont_reach(), Vec::<String>::new());
    }

    #[test]
    fn not_ported_lists_functions_and_ranges_with_why() {
        let np = "\"0x80010088\" = \"memcpy: the port has its own\"\n\"0x80050000..0x80060000\" = \"libgte\"\n\"main\" = \"x\"\n\"0x80070000\" = \" \"\n";
        let (_d, port) = workspace("description = \"x\"\nfeatures = \"all\"\n", &[("not_ported.toml", np)]);
        assert_eq!(port.not_ported(0x0001_0088).map(String::as_str), Some("memcpy: the port has its own"));
        assert_eq!(port.not_ported(0x8005_8000).map(String::as_str), Some("libgte"));
        assert_eq!(port.not_ported(0x8006_0000), None);
        let p: Vec<&str> = port.problems.iter().filter(|p| p.file == NOT_PORTED).map(|p| p.problem.as_str()).collect();
        assert_eq!(
            p[..2],
            [
                "0x80070000: no reason: what the code is, and why the port has none",
                "\"main\" is not a function's entry (0x…) or a range (0x…..0x…)",
            ]
        );
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
    fn a_confirmed_feature_only_depends_named_has_left_the_goal() {
        let (a, b) = (feature("F-0001", ""), feature("F-0002", "F-0001"));
        let goal = "features = [\"F-0002\"]\nconfirmed = { at = \"t\", features = [\"F-0001\", \"F-0002\"] }\n";
        let (_d, port) = workspace(goal, &[("F-0001.md", &a), ("F-0002.md", &b)]);
        assert_eq!(port.closure(), ["F-0002"]);
        assert_eq!(port.brought_by_depends(), ["F-0001"]);
    }

    #[test]
    fn a_confirmation_covers_the_agents_cut_but_not_the_description_or_the_persons_plays() {
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
        // An id leaving is the agent's cut, not the goal shrinking.
        let (_d, port) = workspace(&format!("features = [\"F-0002\"]\n{confirmed}"), &entries);
        assert!(matches!(port.confirmation(), Confirmation::Covers { .. }));
        let all = "confirmed = { at = \"t\", features = \"all\" }\n";
        let (_d, port) = workspace(&format!("features = [\"F-0001\"]\n{all}"), &entries);
        assert!(matches!(port.confirmation(), Confirmation::Covers { .. }));
        // The description the person confirmed, changed.
        let stamped = format!(
            "description = \"drive\"\nfeatures = [\"F-0001\"]\nconfirmed = {{ at = \"t\", features = [\"F-0001\"], description = \"{}\" }}\n",
            description_stamp("drive")
        );
        let (_d, port) = workspace(&stamped, &entries);
        assert!(matches!(port.confirmation(), Confirmation::Covers { .. }));
        let (_d, port) = workspace(&stamped.replacen("\"drive\"", "\"drive fast\"", 1), &entries);
        assert_eq!(port.confirmation(), Confirmation::Shrunk { at: "t".into(), missing: vec!["description".into()] });
    }

    #[test]
    fn the_goal_keeps_its_recordings_through_a_merge() {
        let held = |id: &str, work: &str, held: &str| {
            format!(
                "+++\nid = \"{id}\"\ntitle = \"t\"\narea = \"a\"\n[recordings]\nwork = [{work}]\nheld_out = [{held}]\n+++\nWhat it does.\n"
            )
        };
        let goal = "description = \"x\"\nfeatures = [\"F-0001\", \"F-0002\"]\nconfirmed = { at = \"t\", features = [\"F-0001\", \"F-0002\"] }\n";
        let (a, b) = (held("F-0001", "\"s/a\"", "\"s/play\""), held("F-0002", "\"s/b\"", ""));
        let (d, port) = workspace(goal, &[("F-0001.md", &a), ("F-0002.md", &b)]);
        std::fs::create_dir_all(d.0.join("scenarios/s")).unwrap();
        for n in ["a", "b", "play"] {
            std::fs::write(d.0.join(format!("scenarios/s/{n}.jsonl")), "").unwrap();
        }
        port.note_recordings(&d.0, false);
        // Merged into F-0001 with every recording: the goal is as confirmed.
        let merged = held("F-0001", "\"s/a\", \"s/b\"", "\"s/play\"");
        let one = goal.replace("[\"F-0001\", \"F-0002\"]\nc", "[\"F-0001\"]\nc");
        std::fs::write(d.0.join(GOAL), &one).unwrap();
        std::fs::write(d.0.join("catalogue/F-0001.md"), &merged).unwrap();
        std::fs::remove_file(d.0.join("catalogue/F-0002.md")).unwrap();
        let port = Port::load(&d.0).unwrap();
        assert_eq!((port.problems.clone(), port.confirmation()), (vec![], Confirmation::Covers { at: "t".into() }));
        // A recording the merge left unnamed is a problem.
        std::fs::write(d.0.join("catalogue/F-0001.md"), held("F-0001", "\"s/a\"", "\"s/play\"")).unwrap();
        let port = Port::load(&d.0).unwrap();
        assert_eq!(port.problems.len(), 1, "{:?}", port.problems);
        assert!(port.problems[0].problem.starts_with("s/b: a goal feature named it and none does now"));
        // The person's play moved to work asks the person.
        std::fs::write(d.0.join("catalogue/F-0001.md"), held("F-0001", "\"s/a\", \"s/b\", \"s/play\"", "")).unwrap();
        let port = Port::load(&d.0).unwrap();
        assert_eq!(
            port.confirmation(),
            Confirmation::Shrunk { at: "t".into(), missing: vec!["held_out s/play".into()] }
        );
        // Once confirmed, the goal's recordings are what it names now.
        port.note_recordings(&d.0, true);
        assert!(matches!(Port::load(&d.0).unwrap().confirmation(), Confirmation::Covers { .. }));
        // A work recording deleted is gone.
        std::fs::write(d.0.join("catalogue/F-0001.md"), held("F-0001", "\"s/a\", \"s/play\"", "")).unwrap();
        std::fs::remove_file(d.0.join("scenarios/s/b.jsonl")).unwrap();
        let port = Port::load(&d.0).unwrap();
        assert!(port.problems.is_empty(), "{:?}", port.problems);
        port.note_recordings(&d.0, false);
        assert!(!Port::load(&d.0).unwrap().ledger.contains_key("s/b"));
        // The goal's own play: a feature's work recording is not one, and
        // one taken out of goal.toml asks the person.
        std::fs::write(d.0.join(GOAL), one.replacen("features =", "held_out = [\"s/a\", \"s/gone\"]\nfeatures =", 1))
            .unwrap();
        let port = Port::load(&d.0).unwrap();
        let p: Vec<&str> = port.problems.iter().map(|p| p.problem.as_str()).collect();
        assert!(
            p[0].starts_with("s/a: held_out: also a feature's work") && p[1].starts_with("s/gone: held_out: no such"),
            "{p:?}"
        );
        std::fs::write(d.0.join("scenarios/s/g.jsonl"), "").unwrap();
        std::fs::write(d.0.join(GOAL), one.replacen("features =", "held_out = [\"s/g\"]\nfeatures =", 1)).unwrap();
        let port = Port::load(&d.0).unwrap();
        assert!(port.problems.is_empty(), "{:?}", port.problems);
        port.note_recordings(&d.0, true);
        std::fs::write(d.0.join(GOAL), &one).unwrap();
        let port = Port::load(&d.0).unwrap();
        assert_eq!(port.confirmation(), Confirmation::Shrunk { at: "t".into(), missing: vec!["held_out s/g".into()] });
    }

    #[test]
    fn a_goal_asks_for_every_asset_none_or_names_the_person_confirms() {
        let goal = |a: &str| toml::from_str::<Goal>(&format!("assets = {a}")).map(|g| g.assets);
        assert_eq!(goal("true").unwrap(), Assets::All);
        assert_eq!(goal("false").unwrap(), Assets::Names(vec![]));
        assert_eq!(goal("[\"format:level\"]").unwrap(), Assets::Names(vec!["format:level".into()]));
        assert!(goal("\"some\"").is_err());
        assert_eq!(toml::from_str::<Goal>(GOAL_TOML).unwrap().assets, Assets::Names(vec![]));
        let a = feature("F-0001", "").replace("depends = []", "depends = []\nassets = [\"MAPS/*.WLD\", \"format:\"]");
        let confirmed =
            "confirmed = { at = \"t\", features = [\"F-0001\"], assets = [\"format:level\", \"format:car\"] }\n";
        let (_d, port) = workspace(
            &format!("description = \"x\"\nfeatures = [\"F-0001\"]\nassets = [\"format:level\", \"lvl\"]\n{confirmed}"),
            &[("F-0001.md", &a)],
        );
        let p: Vec<String> = port.problems.iter().map(|p| format!("{}: {}", p.file, p.problem)).collect();
        assert_eq!(
            p,
            [
                "catalogue/F-0001.md: assets: \"MAPS/*.WLD\" is not a disc path from / (/MAPS/*.WLD, /ARCHIVE:ENTRY) or format:<id>",
                "catalogue/F-0001.md: assets: \"format:\" is not format:<id>, a format doc's id (formats/<id>.md) or a standard format",
                "goal.toml: assets: \"lvl\" is not a disc path from / (/MAPS/*.WLD, /ARCHIVE:ENTRY) or format:<id>",
            ]
        );
        // A name the person confirmed taken out needs them again; every file covers it.
        assert_eq!(
            port.confirmation(),
            Confirmation::Shrunk { at: "t".into(), missing: vec!["assets format:car".into()] }
        );
        let (_d, port) = workspace(&format!("features = [\"F-0001\"]\nassets = true\n{confirmed}"), &[]);
        assert!(matches!(port.confirmation(), Confirmation::Covers { .. }));
    }

    #[test]
    fn a_goal_reads_all_or_a_list_and_nothing_else() {
        assert_eq!(toml::from_str::<Goal>("features = \"all\"").unwrap().features, Features::All);
        assert!(toml::from_str::<Goal>("features = \"some\"").is_err());
        assert!(toml::from_str::<Goal>("feature = []").is_err());
        assert_eq!(toml::from_str::<Goal>(GOAL_TOML).unwrap().features, Features::List(vec![]));
    }
}
