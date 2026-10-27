//! The clean-mode requests: record, compare, heldout, screens and next.
//!
//! What may come out is decided here, not by psx: a request names only
//! published things (start states in the implementer's `specs/states/`,
//! spec ids that crossed, names the glossary or a crossed case uses), and a
//! recording keeps only published columns whose values do not look like
//! memory addresses. psx's own output, messages and files stay on the
//! investigator's side; an error says what was wrong with the request, or
//! gives psx's error code alone.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

type Result<T> = std::result::Result<T, String>;

/// Fields a recording or a picture run may last unless protocol.toml's
/// [clean] max_frames says otherwise (five minutes at 60 fields a second).
const MAX_FRAMES: u32 = 18_000;
/// The case runner psx-bevy's template gives a port.
const RUNNER: &str = "cargo test --test cases cases_from_folder -- --exact";
/// Seconds a heldout run may take unless [clean] heldout_timeout says otherwise.
const RUNNER_TIMEOUT: u64 = 1800;
/// The keys of a case clean mode passes on (case record's).
const CASE_KEYS: [&str; 15] = [
    "id",
    "spec",
    "role",
    "level",
    "subject",
    "start",
    "input",
    "activation",
    "writes",
    "observations",
    "compare",
    "fields_per_tick",
    "first_tick",
    "timing",
    "coverage",
];
/// Columns every recording has, which are not samples.
const FIXED: [&str; 3] = ["run_frame", "frame", "tick"];

fn json_arg(s: &str) -> Result<Value> {
    serde_json::from_str(s).map_err(|e| format!("not JSON: {e}"))
}

#[derive(clap::Args, Clone, Debug, Serialize, Deserialize, schemars::JsonSchema)]
pub struct RecordArgs {
    /// The exploratory case's name (letters, digits, _ and -): notes/oracle/NAME.json and NAME.csv.
    pub name: String,
    /// A published start state, by its name in specs/states/: row 0.
    #[arg(long)]
    pub start: String,
    /// Fields to record: rows 0..=frames.
    #[arg(long)]
    pub frames: u32,
    /// Pad input as a case gives it, [{"frame":0,"buttons":["CROSS"],"until":5}], fields
    /// counted from the start; none when left out.
    #[arg(long, value_parser = json_arg)]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input: Option<Value>,
    /// The subject, by its published name (as a case's subject; NAME[N] for one of several):
    /// its published fields are the columns.
    #[arg(long)]
    pub subject: String,
    /// More columns by published name (SUBJECT.FIELD for another subject's); repeatable.
    #[arg(long = "sample")]
    #[serde(default, rename = "sample", skip_serializing_if = "Vec::is_empty")]
    pub samples: Vec<String>,
    /// A column that is input rather than compared; repeatable.
    #[arg(long)]
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub activation: Vec<String>,
    /// COLUMN or COLUMN=TOLERANCE to compare; default every other column, exactly. Repeatable.
    #[arg(long)]
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub compare: Vec<String>,
    /// The crossed spec the case explores.
    #[arg(long)]
    pub spec: String,
    /// SPEC:COLUMNS[:ROWS], what a crossed spec claims, as in a case's coverage; repeatable.
    #[arg(long)]
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub coverage: Vec<String>,
    /// Record over a case of this name.
    #[arg(long)]
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub replace: bool,
}

#[derive(clap::Args, Clone, Debug, Serialize, Deserialize, schemars::JsonSchema)]
pub struct CompareArgs {
    /// The case: an exploratory case's name (notes/oracle) or a published case's (specs/cases).
    pub case: String,
    /// The port's run of it, a CSV with the case's columns, in this repository (default: the
    /// newest NAME.csv write_run left in the results folder).
    #[arg(long)]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub port: Option<PathBuf>,
}

#[derive(clap::Args, Clone, Debug, Serialize, Deserialize, schemars::JsonSchema)]
pub struct HeldoutArgs {
    /// Only the cases kept back for this spec.
    #[arg(long)]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spec: Option<String>,
    /// The case runner, run in this repository with PSX_CASES (a folder of cases) and
    /// PSX_RESULTS (where write_run writes) set (default: the template's cases_from_folder test).
    #[arg(long)]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
}

#[derive(clap::Args, Clone, Debug, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ScreensArgs {
    /// A name for the pictures: notes/oracle/NAME/fNNNNNN.png, NNNNNN the row (field from the start).
    pub name: String,
    /// A published start state, by its name in specs/states/.
    #[arg(long)]
    pub start: String,
    /// Fields to run.
    #[arg(long)]
    pub frames: u32,
    /// Pad input as a case gives it; none when left out.
    #[arg(long, value_parser = json_arg)]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input: Option<Value>,
    /// A picture every N fields (default: the first and the last).
    #[arg(long)]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub every: Option<u32>,
    /// A folder in this repository of the port's pictures from the same start and input,
    /// fNNNNNN.png by row: each gets a side by side, fNNNNNN.side.png (original left).
    #[arg(long)]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub port: Option<PathBuf>,
}

/// The repositories and the psx clean mode drives.
pub struct Clean {
    investigator: PathBuf,
    implementer: PathBuf,
    reviewer: Option<PathBuf>,
    psx: PathBuf,
    settings: toml::Value,
}

/// What crossed to the implementer: start states, spec ids and names.
#[derive(Default)]
struct Published {
    states: BTreeSet<String>,
    specs: BTreeSet<String>,
    names: BTreeSet<String>,
}

impl Published {
    /// A sample or subject name: each part of SUBJECT.FIELD published, an
    /// index [N] left off.
    fn has(&self, name: &str) -> bool {
        let name = name.split('[').next().unwrap_or_default();
        !name.is_empty()
            && (self.names.contains(name) || name.split('.').all(|p| !p.is_empty() && self.names.contains(p)))
    }
}

fn canon(p: &Path) -> PathBuf {
    p.canonicalize().unwrap_or_else(|_| std::path::absolute(p).unwrap_or_else(|_| p.to_path_buf()))
}

fn plain_name(s: &str) -> bool {
    !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

pub(crate) fn sha256(bytes: &[u8]) -> String {
    use sha2::Digest;
    format!("{:x}", sha2::Sha256::digest(bytes))
}

/// The time now, UTC, as 2026-10-26T12:34:56Z.
fn utc_now() -> String {
    let secs = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    let (days, rest) = ((secs / 86400) as i64, secs % 86400);
    // Howard Hinnant's civil_from_days.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z", rest / 3600, rest / 60 % 60, rest % 60)
}

/// Every file under `dir`, recursively.
fn files_under(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut todo = vec![dir.to_path_buf()];
    while let Some(d) = todo.pop() {
        for e in std::fs::read_dir(&d).into_iter().flatten().flatten() {
            let p = e.path();
            if p.is_dir() {
                todo.push(p);
            } else {
                out.push(p);
            }
        }
    }
    out.sort();
    out
}

fn read_json(p: &Path) -> Option<Value> {
    serde_json::from_slice(&std::fs::read(p).ok()?).ok()
}

fn write(p: &Path, bytes: &[u8]) -> Result<()> {
    if let Some(d) = p.parent() {
        std::fs::create_dir_all(d).map_err(|e| format!("could not write {}: {e}", p.display()))?;
    }
    std::fs::write(p, bytes).map_err(|e| format!("could not write {}: {e}", p.display()))
}

/// A CSV of plain cells, as psx writes them.
struct Table {
    header: Vec<String>,
    rows: Vec<Vec<String>>,
}

impl Table {
    fn parse(text: &str) -> Table {
        let mut lines = text.lines().filter(|l| !l.is_empty()).map(|l| l.split(',').map(str::to_string).collect());
        Table { header: lines.next().unwrap_or_default(), rows: lines.collect() }
    }

    fn column(&self, name: &str) -> Option<usize> {
        self.header.iter().position(|h| h == name)
    }

    fn keep(&self, columns: &[usize]) -> String {
        let mut s = String::new();
        for line in std::iter::once(&self.header).chain(&self.rows) {
            let cells: Vec<&str> = columns.iter().map(|&i| line.get(i).map(String::as_str).unwrap_or("")).collect();
            s += &cells.join(",");
            s.push('\n');
        }
        s
    }
}

/// A cell as a number: decimal, fixed point or 0x hex.
fn number(s: &str) -> Option<f64> {
    let s = s.trim();
    match s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        Some(h) => u64::from_str_radix(h, 16).ok().map(|n| n as f64),
        None => s.parse().ok(),
    }
}

/// A value that reads as an address in the console's RAM (any of its
/// mirrors, or as a signed word) or its scratchpad.
fn address_like(v: f64) -> bool {
    if v.fract() != 0.0 {
        return false;
    }
    let ranges: [(f64, f64); 4] = [
        (0x8000_0000u32 as f64, 0x8020_0000u32 as f64),
        (0xA000_0000u32 as f64, 0xA020_0000u32 as f64),
        (0x1F80_0000u32 as f64, 0x1F80_0400u32 as f64),
        (i32::MIN as f64, (0x8020_0000u32 as i32) as f64),
    ];
    ranges.iter().any(|&(a, b)| v >= a && v < b)
}

/// A working folder on the investigator's side, removed when dropped.
struct Work(PathBuf);

impl Work {
    fn new(investigator: &Path, what: &str) -> Result<Work> {
        let t = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
        let p = investigator.join(".psx/clean").join(format!("{what}-{}-{t}", std::process::id()));
        std::fs::create_dir_all(&p).map_err(|e| format!("could not make a working folder: {e}"))?;
        Ok(Work(p))
    }
}

impl Drop for Work {
    fn drop(&mut self) {
        remove_tree(&self.0);
    }
}

/// Removes a folder, first making every folder in it writable (an overlay's
/// work folder is left unreadable).
fn remove_tree(p: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut todo = vec![p.to_path_buf()];
        while let Some(d) = todo.pop() {
            let _ = std::fs::set_permissions(&d, std::fs::Permissions::from_mode(0o700));
            for e in std::fs::read_dir(&d).into_iter().flatten().flatten() {
                if e.file_type().is_ok_and(|t| t.is_dir()) {
                    todo.push(e.path());
                }
            }
        }
    }
    let _ = std::fs::remove_dir_all(p);
}

impl Clean {
    pub fn open(investigator: Option<PathBuf>, psx: Option<PathBuf>) -> Result<Clean> {
        let investigator =
            canon(&investigator.ok_or("no investigator: give --investigator (psx port init writes it)")?);
        let text = std::fs::read_to_string(investigator.join("protocol.toml"))
            .map_err(|_| "the investigator's repository has no protocol.toml (psx port init)".to_string())?;
        let settings: toml::Value = toml::from_str(&text).map_err(|_| "protocol.toml does not parse".to_string())?;
        let repo = |k: &str| {
            let p = PathBuf::from(settings.get("repositories")?.get(k)?.as_str()?);
            Some(canon(&if p.is_absolute() { p } else { investigator.join(p) }))
        };
        let implementer = repo("implementer").ok_or("protocol.toml names no implementer")?;
        let reviewer = repo("reviewer");
        let psx = match psx {
            Some(p) => p,
            None => {
                let me = std::env::current_exe().map_err(|_| "cannot find psx beside psx-clean".to_string())?;
                me.with_file_name(format!("psx{}", std::env::consts::EXE_SUFFIX))
            }
        };
        Ok(Clean { investigator, implementer, reviewer, psx, settings })
    }

    fn setting(&self, table: &str, key: &str) -> Option<&toml::Value> {
        self.settings.get(table)?.get(key)
    }

    fn max_frames(&self) -> u32 {
        self.setting("clean", "max_frames").and_then(|v| v.as_integer()).map_or(MAX_FRAMES, |n| n.max(1) as u32)
    }

    /// The disc, as port init wrote it in protocol.toml: psx's arguments for it.
    fn media(&self) -> Vec<String> {
        match self.setting("disc", "media").and_then(|v| v.as_str()) {
            Some(m) => vec!["--media".into(), m.into()],
            None => Vec::new(),
        }
    }

    /// Where the implementer's tests leave their runs (as protocol next reads it).
    fn results(&self) -> PathBuf {
        match self.setting("protocol", "results").and_then(|v| v.as_str()) {
            Some(r) if Path::new(r).is_absolute() => PathBuf::from(r),
            Some(r) => self.implementer.join(r),
            None => self.implementer.join("target/tmp"),
        }
    }

    /// A path the implementer named, which must be inside its repository.
    fn inside(&self, p: &Path) -> Result<PathBuf> {
        let p = canon(&if p.is_absolute() { p.to_path_buf() } else { self.implementer.join(p) });
        if p.starts_with(&self.implementer) { Ok(p) } else { Err(format!("{} is not in this repository", p.display())) }
    }

    fn published(&self) -> Published {
        let specs = self.implementer.join("specs");
        let mut out = Published::default();
        for p in files_under(&specs) {
            let name = p.file_name().and_then(|n| n.to_str()).unwrap_or_default().to_string();
            let rel = p.strip_prefix(&specs).unwrap_or(&p);
            if rel.starts_with("states") && name.ends_with(".json") {
                if let Some(n) = read_json(&p).and_then(|v| v["name"].as_str().map(str::to_string)) {
                    out.states.insert(n);
                }
            } else if name.ends_with(".md") {
                let text = std::fs::read_to_string(&p).unwrap_or_default();
                // The id in the front matter.
                if text.starts_with("---") {
                    for line in text.lines().skip(1).take_while(|l| l.trim() != "---") {
                        if let Some(id) = line.trim().strip_prefix("id:") {
                            out.specs.insert(id.trim().trim_matches('"').to_string());
                        }
                    }
                }
                if name == "glossary.md" {
                    for term in text.split("**").skip(1).step_by(2) {
                        out.names.insert(term.trim().to_string());
                    }
                }
            } else if name.ends_with(".csv") && rel.starts_with("cases") {
                let text = std::fs::read_to_string(&p).unwrap_or_default();
                let header = text.lines().next().unwrap_or_default();
                out.names.extend(header.split(',').filter(|h| !FIXED.contains(h)).map(|h| h.trim().to_string()));
            } else if name.ends_with(".json") && rel.starts_with("cases") {
                if let Some(v) = read_json(&p) {
                    let s = &v["subject"];
                    if let Some(n) = s.as_str().or_else(|| s["name"].as_str()) {
                        out.names.insert(n.to_string());
                    }
                }
            }
        }
        out.names.remove("");
        out
    }

    /// Runs psx on the investigator's repository: its result, or its error code alone.
    fn psx(&self, args: &[String]) -> Result<Value> {
        let out = Command::new(&self.psx)
            .arg("--root")
            .arg(&self.investigator)
            .args(args)
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .output()
            .map_err(|_| "psx could not be started".to_string())?;
        let v: Value = serde_json::from_slice(&out.stdout).map_err(|_| "psx gave no result".to_string())?;
        match v["error"]["code"].as_str() {
            Some(code) => Err(format!("the original's run failed ({code})")),
            None => Ok(v),
        }
    }

    /// The state file behind a published start.
    fn start(&self, published: &Published, name: &str) -> Result<PathBuf> {
        if !published.states.contains(name) {
            let list: Vec<&str> = published.states.iter().map(String::as_str).collect();
            return Err(format!("{name:?} is not a published start ({})", list.join(", ")));
        }
        let p = self.investigator.join(".psx/states").join(format!("{name}.bin"));
        if !p.is_file() {
            return Err(format!("start {name} is not built on the investigator's side yet: ask for it"));
        }
        Ok(p)
    }

    /// Removes a run folder psx left on the investigator's side.
    fn drop_run(&self, report: &Value) {
        if let Some(dir) = report["run_dir"].as_str().map(PathBuf::from) {
            if canon(&dir).starts_with(self.investigator.join(".psx/runs")) {
                remove_tree(&dir);
            }
        }
    }

    /// One request, logged with what came of it.
    pub fn call(&self, tool: &str, args: Value) -> Result<Value> {
        let result = match tool {
            "record" => parse(&args).and_then(|a| self.record(a)),
            "compare" => parse(&args).and_then(|a| self.compare(a)),
            "heldout" => parse(&args).and_then(|a| self.heldout(a)),
            "screens" => parse(&args).and_then(|a| self.screens(a)),
            "next" => self.next(),
            _ => Err(format!("no tool {tool}")),
        };
        let logged = match &result {
            Ok(v) => v.get("logged").cloned().unwrap_or(json!({"status": "ok"})),
            Err(e) => json!({"status": "error", "error": e}),
        };
        self.log(tool, &args, &logged)?;
        result.map(|mut v| {
            if let Some(m) = v.as_object_mut() {
                m.remove("logged");
            }
            v
        })
    }

    /// Appends a request to the implementer's notes/oracle.jsonl.
    fn log(&self, tool: &str, request: &Value, result: &Value) -> Result<()> {
        let path = self.implementer.join("notes/oracle.jsonl");
        let text = std::fs::read_to_string(&path).unwrap_or_default();
        let prev = text.lines().rfind(|l| !l.trim().is_empty()).map(|l| sha256(l.as_bytes()));
        let entry = json!({"time": utc_now(), "tool": tool, "request": request, "result": result, "prev": prev});
        let mut out = text;
        if !out.is_empty() && !out.ends_with('\n') {
            out.push('\n');
        }
        out += &serde_json::to_string(&entry).unwrap_or_default();
        out.push('\n');
        write(&path, out.as_bytes())
    }

    fn record(&self, a: RecordArgs) -> Result<Value> {
        if !plain_name(&a.name) {
            return Err(format!("case name {:?}: letters, digits, _ and - only", a.name));
        }
        let max = self.max_frames();
        if a.frames == 0 || a.frames > max {
            return Err(format!("frames: 1 to {max}"));
        }
        let published = self.published();
        let unknown = |n: &str| (!published.has(n)).then(|| format!("{n:?} is not a published name"));
        let column = |c: &str| c.split('=').next().unwrap_or_default().trim().to_string();
        let names = std::iter::once(a.subject.clone())
            .chain(a.samples.iter().cloned())
            .chain(a.activation.iter().cloned())
            .chain(a.compare.iter().map(|c| column(c)));
        if let Some(e) = names.into_iter().find_map(|n| unknown(&n)) {
            return Err(e);
        }
        for s in std::iter::once(&a.spec).chain(a.coverage.iter()) {
            let id = s.split(':').next().unwrap_or_default().trim();
            if !published.specs.contains(id) {
                return Err(format!("{id:?} is not a crossed spec"));
            }
        }
        for c in &a.coverage {
            let cols = c.split(':').nth(1).unwrap_or_default();
            if let Some(e) = cols.split(',').map(str::trim).filter(|c| *c != "*").find_map(unknown) {
                return Err(e);
            }
        }
        let state = self.start(&published, &a.start)?;
        let out = self.implementer.join("notes/oracle");
        let (case_path, csv_path) = (out.join(format!("{}.json", a.name)), out.join(format!("{}.csv", a.name)));
        if case_path.exists() && !a.replace {
            return Err(format!("notes/oracle/{}.json exists: replace=true records over it", a.name));
        }

        let work = Work::new(&self.investigator, "record")?;
        let mut args: Vec<String> =
            ["case", "record", &a.name, "--role", "exploratory", "--spec", &a.spec, "--subject", &a.subject]
                .map(String::from)
                .to_vec();
        args.extend(["--from-state".into(), state.display().to_string(), "--frames".into(), a.frames.to_string()]);
        args.extend(["--input".into(), a.input.clone().unwrap_or(json!([])).to_string()]);
        args.extend(self.media());
        for (flag, list) in [("--sample", &a.samples), ("--activation", &a.activation), ("--compare", &a.compare)] {
            for x in list {
                args.extend([flag.to_string(), x.clone()]);
            }
        }
        for c in &a.coverage {
            args.extend(["--coverage".into(), c.clone()]);
        }
        // The recipe names the investigator's files: it stays apart and goes with the folder.
        let (cases, recipes) = (work.0.join("cases"), work.0.join("recipes"));
        args.extend(["--cases".into(), cases.display().to_string(), "--recipes".into(), recipes.display().to_string()]);
        args.push("--replace".into());
        let report = self.psx(&args)?;
        self.drop_run(&report);
        let recorded = read_json(&cases.join(format!("{}.json", a.name))).ok_or("the recording left no case")?;
        // A case's own keys only.
        let mut case = json!({});
        for k in CASE_KEYS {
            if let Some(v) = recorded.get(k).filter(|v| !v.is_null()) {
                case[k] = v.clone();
            }
        }
        let table = Table::parse(&std::fs::read_to_string(cases.join(format!("{}.csv", a.name))).unwrap_or_default());

        // Only published columns, and none holding addresses.
        let (mut keep, mut unpublished, mut addresses) = (Vec::new(), 0, Vec::new());
        for (i, h) in table.header.iter().enumerate() {
            if FIXED.contains(&h.as_str()) {
                keep.push(i);
            } else if !published.has(h) {
                unpublished += 1;
            } else if table.rows.iter().any(|r| r.get(i).and_then(|c| number(c)).is_some_and(address_like)) {
                addresses.push(h.clone());
            } else {
                keep.push(i);
            }
        }
        let kept: BTreeSet<&str> = keep.iter().map(|&i| table.header[i].as_str()).collect();
        if let Some(m) = case.get_mut("compare").and_then(Value::as_object_mut) {
            m.retain(|k, _| kept.contains(k.as_str()));
        }
        if let Some(l) = case.get_mut("activation").and_then(Value::as_array_mut) {
            l.retain(|c| c.as_str().is_some_and(|c| kept.contains(c)));
        }
        if let Some(l) = case.get_mut("writes").and_then(Value::as_array_mut) {
            l.retain(|w| w["column"].as_str().is_some_and(|c| kept.contains(c)));
        }
        let mut unclaimed: BTreeSet<String> =
            case["compare"].as_object().into_iter().flatten().map(|(k, _)| k.clone()).collect();
        if let Some(m) = case.get_mut("coverage").and_then(Value::as_object_mut) {
            for claim in m.values_mut() {
                if let Some(l) = claim["columns"].as_array_mut() {
                    l.retain(|c| c.as_str().is_some_and(|c| kept.contains(c)));
                    for c in l.iter().filter_map(|c| c.as_str()) {
                        unclaimed.remove(c);
                    }
                }
            }
        } else {
            unclaimed.clear();
        }
        // Row 0 is the subject as recorded (from first_row); the start it was recorded from, by name.
        case["start_state"] = json!(a.start);
        case["recorded_by"] = json!("psx clean");
        let csv = table.keep(&keep);
        let text = serde_json::to_string_pretty(&case).unwrap_or_default() + "\n";
        write(&csv_path, csv.as_bytes())?;
        write(&case_path, text.as_bytes())?;

        let mut warnings = Vec::new();
        match table.column("tick").filter(|_| kept.contains("tick")) {
            None => warnings.push("no tick column: the investigator has not given the game's tick, so the case cannot say which fields ticks ran in".to_string()),
            Some(t) if table.rows.first().map(|r| r.get(t)) == table.rows.last().map(|r| r.get(t)) => {
                warnings.push("no tick ran: is the game paused at this start?".to_string())
            }
            Some(_) => {}
        }
        if !unclaimed.is_empty() {
            let list: Vec<&str> = unclaimed.iter().map(String::as_str).collect();
            warnings.push(format!("no spec's coverage claims {}", list.join(", ")));
        }
        if unpublished > 0 {
            warnings.push(format!("{unpublished} of the subject's fields are not published and were left out"));
        }
        if !addresses.is_empty() {
            warnings.push(format!("left out, as their values look like memory addresses: {}", addresses.join(", ")));
        }
        let rel = |p: &Path| p.strip_prefix(&self.implementer).unwrap_or(p).display().to_string();
        let bytes = csv.len() + text.len();
        let columns: Vec<&str> = keep.iter().map(|&i| table.header[i].as_str()).collect();
        Ok(json!({
            "case": rel(&case_path), "observations": rel(&csv_path), "rows": table.rows.len(), "columns": columns,
            "compare": case["compare"], "fields_per_tick": case["fields_per_tick"], "first_tick": case["first_tick"],
            "warnings": warnings,
            "logged": {"status": "ok", "case": rel(&case_path), "rows": table.rows.len(), "columns": columns.len(), "bytes": bytes},
        }))
    }

    /// A case by name: notes/oracle first, then specs/cases.
    fn case(&self, name: &str) -> Result<(PathBuf, Value)> {
        let name = name.strip_suffix(".json").unwrap_or(name);
        if !plain_name(name) {
            return Err(format!("case {name:?}: a case's name, letters, digits, _ and - only"));
        }
        for dir in ["notes/oracle", "specs/cases"] {
            let p = self.implementer.join(dir).join(format!("{name}.json"));
            if let Some(v) = read_json(&p) {
                return Ok((p, v));
            }
        }
        Err(format!("no case {name} in notes/oracle or specs/cases"))
    }

    fn compare(&self, a: CompareArgs) -> Result<Value> {
        let (path, case) = self.case(&a.case)?;
        let name = path.file_stem().and_then(|s| s.to_str()).unwrap_or_default().to_string();
        let observations = case["observations"].as_str().ok_or("the case names no observations")?;
        let original = Table::parse(
            &std::fs::read_to_string(path.with_file_name(observations))
                .map_err(|_| "the case's observations are missing")?,
        );
        let port_path = match &a.port {
            Some(p) => self.inside(p)?,
            None => {
                // write_run's NAME.csv beside a NAME.json recorded from the rebuild.
                let mut found: Vec<(std::time::SystemTime, PathBuf)> = files_under(&self.results())
                    .into_iter()
                    .filter(|p| p.file_name().and_then(|n| n.to_str()) == Some(&format!("{name}.csv")))
                    .filter(|p| read_json(&p.with_extension("json")).is_some_and(|v| v["recorded_from"] == "rebuild"))
                    .filter_map(|p| Some((p.metadata().ok()?.modified().ok()?, p)))
                    .collect();
                found.sort();
                found.pop().map(|(_, p)| p).ok_or(format!("no run of {name} in the results folder: give port"))?
            }
        };
        let port = Table::parse(&std::fs::read_to_string(&port_path).map_err(|_| "the port's run is missing")?);
        let tolerances: Vec<(String, f64)> = case["compare"]
            .as_object()
            .into_iter()
            .flatten()
            .map(|(k, v)| (k.clone(), v.as_f64().unwrap_or(0.0)))
            .collect();
        let rows = original.rows.len().min(port.rows.len());
        let (mut first, mut columns, mut missing) = (None::<Value>, serde_json::Map::new(), Vec::new());
        let mut pairs = Vec::new();
        for (c, tol) in &tolerances {
            match (original.column(c), port.column(c)) {
                (Some(i), Some(j)) => pairs.push((c, *tol, i, j)),
                _ => missing.push(c.clone()),
            }
        }
        let cell = |t: &Table, r: usize, i: usize| t.rows[r].get(i).cloned().unwrap_or_default();
        for r in 0..rows {
            for &(c, tol, i, j) in &pairs {
                let (o, p) = (cell(&original, r, i), cell(&port, r, j));
                let differs = match (number(&o), number(&p)) {
                    (Some(x), Some(y)) => ((x - y).abs() > tol + 1e-9).then_some(json!(y - x)),
                    _ => (o.trim() != p.trim()).then_some(Value::Null),
                };
                let Some(by) = differs else { continue };
                let e = columns.entry(c.clone()).or_insert(json!({"rows_differing": 0, "first_row": r}));
                e["rows_differing"] = json!(e["rows_differing"].as_u64().unwrap_or(0) + 1);
                if let Some(d) = by.as_f64() {
                    if e["largest"].as_f64().is_none_or(|l| d.abs() > l) {
                        e["largest"] = json!(d.abs());
                    }
                }
                if first.is_none() {
                    first = Some(json!({"row": r, "column": c, "original": o, "port": p, "by": by, "tolerance": tol}));
                }
            }
        }
        let tick = match (original.column("tick"), port.column("tick")) {
            (Some(i), Some(j)) => (0..rows).find(|&r| cell(&original, r, i) != cell(&port, r, j)),
            _ => None,
        };
        let same = first.is_none() && missing.is_empty() && tick.is_none() && original.rows.len() == port.rows.len();
        let rel = |p: &Path| p.strip_prefix(&self.implementer).unwrap_or(p).display().to_string();
        let compared: Vec<&str> = pairs.iter().map(|p| p.0.as_str()).collect();
        Ok(json!({
            "case": rel(&path), "port": rel(&port_path), "same": same,
            "rows": {"original": original.rows.len(), "port": port.rows.len()},
            "compared": compared, "missing_from_port": missing,
            "first_difference": first, "first_tick_differing": tick, "columns": columns,
            "logged": {"status": "ok", "same": same, "first_difference": first, "first_tick_differing": tick},
        }))
    }

    /// The cases the investigator kept back whose spec crossed: (label, its
    /// spec, path). A label is the case file's sha256, cut short, so the
    /// investigator can find it and the implementer learns nothing from it.
    fn kept_back(&self, published: &Published) -> (Vec<(String, String, PathBuf)>, usize) {
        let (mut out, mut waiting) = (Vec::new(), 0);
        for p in files_under(&self.investigator.join("specs/cases")) {
            if p.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }
            let Ok(bytes) = std::fs::read(&p) else { continue };
            let Ok(v) = serde_json::from_slice::<Value>(&bytes) else { continue };
            if v["kept_back"] != json!(true) {
                continue;
            }
            let spec = v["spec"].as_str().unwrap_or_default().to_string();
            if published.specs.contains(&spec) {
                out.push((format!("k{}", &sha256(&bytes)[..10]), spec, p));
            } else {
                waiting += 1;
            }
        }
        (out, waiting)
    }

    fn heldout(&self, a: HeldoutArgs) -> Result<Value> {
        let published = self.published();
        if let Some(s) = &a.spec {
            if !published.specs.contains(s) {
                return Err(format!("{s:?} is not a crossed spec"));
            }
        }
        let (mut cases, not_crossed) = self.kept_back(&published);
        cases.retain(|(_, spec, _)| a.spec.as_ref().is_none_or(|s| s == spec));
        if cases.is_empty() {
            return Ok(json!({"cases": [], "note": "no case is kept back for a crossed spec",
                "logged": {"status": "ok", "cases": 0}}));
        }
        let command = a
            .command
            .clone()
            .or_else(|| self.setting("clean", "heldout").and_then(|v| v.as_str()).map(str::to_string))
            .unwrap_or_else(|| RUNNER.to_string());
        if command.trim().is_empty() {
            return Err("command is empty".into());
        }
        // A shell line, as typed.
        let shell: [&str; 2] = if cfg!(windows) { ["cmd", "/C"] } else { ["sh", "-c"] };
        let work = Work::new(&self.investigator, "heldout")?;
        // The cases under their labels, in the folder the runner reads.
        let sandbox = cfg!(target_os = "linux") && Command::new("bwrap").arg("--version").output().is_ok();
        let (upper, inside) = (work.0.join("upper"), self.implementer.join(".psx-heldout"));
        let staged = if sandbox { upper.join(".psx-heldout") } else { work.0.clone() };
        for (label, _, p) in &cases {
            let mut v = read_json(p).ok_or("a kept-back case does not parse")?;
            let csv = v["observations"].as_str().map(|o| p.with_file_name(o));
            let csv = csv.and_then(|c| std::fs::read(c).ok()).ok_or("a kept-back case's observations are missing")?;
            v["observations"] = json!(format!("{label}.csv"));
            write(
                &staged.join("cases").join(format!("{label}.json")),
                serde_json::to_string_pretty(&v).unwrap_or_default().as_bytes(),
            )?;
            write(&staged.join("cases").join(format!("{label}.csv")), &csv)?;
        }
        std::fs::create_dir_all(staged.join("results")).map_err(|e| format!("could not make a working folder: {e}"))?;
        let (cases_dir, results_dir) = if sandbox {
            (inside.join("cases"), inside.join("results"))
        } else {
            (staged.join("cases"), staged.join("results"))
        };
        let mut cmd = if sandbox {
            // The port's repository and cargo's home as overlays whose writes
            // land in the working folder and go with it; the investigator's
            // and reviewer's repositories hidden; no network.
            let mut b = Command::new("bwrap");
            b.args(["--ro-bind", "/", "/", "--dev", "/dev", "--proc", "/proc"]);
            let mut overlay = |src: &Path, tag: &str| -> Result<()> {
                let (u, w) = (work.0.join(format!("{tag}-upper")), work.0.join(format!("{tag}-work")));
                let u = if tag == "port" { upper.clone() } else { u };
                for d in [&u, &w] {
                    std::fs::create_dir_all(d).map_err(|e| format!("could not make a working folder: {e}"))?;
                }
                b.arg("--overlay-src").arg(src).arg("--overlay").arg(&u).arg(&w).arg(src);
                Ok(())
            };
            overlay(&self.implementer, "port")?;
            let cargo = std::env::var_os("CARGO_HOME")
                .map(PathBuf::from)
                .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cargo")));
            if let Some(c) = cargo.filter(|c| c.is_dir()) {
                overlay(&canon(&c), "cargo")?;
            }
            for hide in std::iter::once(&self.investigator).chain(self.reviewer.as_ref()) {
                if hide.is_dir() && !self.implementer.starts_with(hide) {
                    b.arg("--tmpfs").arg(hide);
                }
            }
            b.args(["--unshare-net", "--unshare-pid", "--die-with-parent", "--new-session"]);
            b.arg("--chdir").arg(&self.implementer);
            b.arg("--setenv").arg("TMPDIR").arg(inside.join("tmp"));
            b.arg("--").args(shell).arg(&command);
            std::fs::create_dir_all(staged.join("tmp")).map_err(|e| format!("could not make a working folder: {e}"))?;
            b
        } else {
            let mut c = Command::new(shell[0]);
            c.arg(shell[1]).arg(&command).current_dir(&self.implementer);
            c
        };
        cmd.env("PSX_CASES", &cases_dir)
            .env("PSX_RESULTS", &results_dir)
            .env("CARGO_NET_OFFLINE", "true")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        let timeout = self
            .setting("clean", "heldout_timeout")
            .and_then(|v| v.as_integer())
            .map_or(RUNNER_TIMEOUT, |n| n.max(1) as u64);
        let mut child = cmd.spawn().map_err(|_| "the runner could not be started".to_string())?;
        let started = std::time::Instant::now();
        let timed_out = loop {
            match child.try_wait() {
                Ok(Some(_)) => break false,
                Ok(None) if started.elapsed().as_secs() >= timeout => {
                    let _ = child.kill();
                    let _ = child.wait();
                    break true;
                }
                Ok(None) => std::thread::sleep(std::time::Duration::from_millis(200)),
                Err(_) => break false,
            }
        };
        let results = staged.join("results");
        let mut list = Vec::new();
        let mut by_spec: BTreeMap<String, BTreeMap<&str, u64>> = BTreeMap::new();
        for (label, spec, _) in &cases {
            let v = read_json(&results.join(format!("{label}.result.json")));
            let verdict = match v.as_ref().and_then(|v| v["specs"].as_object()) {
                None => "no_result",
                Some(m) if m.is_empty() => "waiting",
                Some(m) if m.values().all(|x| x == &json!(true)) => "pass",
                Some(_) => "fail",
            };
            *by_spec.entry(spec.clone()).or_default().entry(verdict).or_default() += 1;
            list.push(json!({"case": label, "spec": spec, "verdict": verdict}));
        }
        let mut out = json!({
            "cases": list, "by_spec": by_spec, "sandboxed": sandbox,
            "logged": {"status": "ok", "cases": list, "sandboxed": sandbox, "command": command, "timed_out": timed_out},
        });
        if timed_out {
            out["note"] = json!(format!("the runner was stopped after {timeout} s"));
        }
        if not_crossed > 0 {
            out["not_crossed"] = json!(not_crossed);
        }
        if list.iter().all(|c| c["verdict"] == "no_result") {
            out["hint"] = json!(
                "no case left a verdict: the runner must run every case in PSX_CASES and write_run each to PSX_RESULTS (the template's cases_from_folder), and build offline"
            );
        }
        Ok(out)
    }

    fn screens(&self, a: ScreensArgs) -> Result<Value> {
        if !plain_name(&a.name) {
            return Err(format!("name {:?}: letters, digits, _ and - only", a.name));
        }
        let max = self.max_frames();
        if a.frames == 0 || a.frames > max {
            return Err(format!("frames: 1 to {max}"));
        }
        let published = self.published();
        let state = self.start(&published, &a.start)?;
        let port = a.port.as_ref().map(|p| self.inside(p)).transpose()?;
        let every = a.every.unwrap_or(a.frames).max(1);
        let work = Work::new(&self.investigator, "screens")?;
        let mut args: Vec<String> = vec![
            "run".into(),
            "--from-state".into(),
            state.display().to_string(),
            "--frames".into(),
            a.frames.to_string(),
            "--input".into(),
            a.input.clone().unwrap_or(json!([])).to_string(),
            "--snapshot-every".into(),
            every.to_string(),
            "--out".into(),
            work.0.join("run").display().to_string(),
            "--summary".into(),
        ];
        // run takes the disc as its first argument.
        if let [_, m] = self.media().as_slice() {
            args.insert(1, m.clone());
        }
        self.psx(&args)?;
        let out = self.implementer.join("notes/oracle").join(&a.name);
        let mut pictures = Vec::new();
        let mut sides = Vec::new();
        let mut bytes = 0;
        let mut shots: Vec<(u32, PathBuf)> = files_under(&work.0.join("run/screens"))
            .into_iter()
            .filter_map(|p| {
                let n = p.file_name()?.to_str()?.strip_prefix('f')?.strip_suffix(".png")?.parse().ok()?;
                Some((n, p))
            })
            .collect();
        let last = work.0.join("run/screen.png");
        if !shots.iter().any(|(n, _)| *n == a.frames) && last.is_file() {
            shots.push((a.frames, last));
        }
        for (n, p) in shots {
            let file = format!("f{n:06}.png");
            let png = std::fs::read(&p).map_err(|_| "a picture is missing")?;
            write(&out.join(&file), &png)?;
            bytes += png.len();
            pictures.push(format!("notes/oracle/{}/{file}", a.name));
            if let Some(port) = &port {
                if let Ok(theirs) = std::fs::read(port.join(&file)) {
                    let side = side_by_side(&png, &theirs)?;
                    let name = format!("f{n:06}.side.png");
                    write(&out.join(&name), &side)?;
                    sides.push(format!("notes/oracle/{}/{name}", a.name));
                }
            }
        }
        Ok(json!({
            "pictures": pictures, "side_by_side": sides,
            "logged": {"status": "ok", "pictures": pictures.len(), "side_by_side": sides.len(), "bytes": bytes},
        }))
    }

    fn next(&self) -> Result<Value> {
        self.psx(&["protocol".into(), "next".into(), "--role".into(), "implementer".into()])?;
        let text = std::fs::read_to_string(self.implementer.join("notes/NEXT.md")).map_err(|_| "no notes/NEXT.md")?;
        Ok(json!({"next": text, "logged": {"status": "ok", "bytes": text.len()}}))
    }
}

fn parse<T: serde::de::DeserializeOwned>(args: &Value) -> Result<T> {
    serde_json::from_value(args.clone()).map_err(|e| format!("arguments: {e}"))
}

/// Decodes a PNG to RGBA.
fn decode(bytes: &[u8]) -> Result<(u32, u32, Vec<u8>)> {
    let mut d = png::Decoder::new(std::io::Cursor::new(bytes));
    d.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
    let mut r = d.read_info().map_err(|_| "a picture does not decode as PNG")?;
    let mut buf = vec![0; r.output_buffer_size().ok_or("a picture is too large")?];
    let info = r.next_frame(&mut buf).map_err(|_| "a picture does not decode as PNG")?;
    let (w, h) = (info.width, info.height);
    let px = (w * h) as usize;
    let rgba = match info.color_type {
        png::ColorType::Rgba => buf[..px * 4].to_vec(),
        png::ColorType::Rgb => buf[..px * 3].chunks(3).flat_map(|c| [c[0], c[1], c[2], 255]).collect(),
        png::ColorType::GrayscaleAlpha => buf[..px * 2].chunks(2).flat_map(|c| [c[0], c[0], c[0], c[1]]).collect(),
        png::ColorType::Grayscale => buf[..px].iter().flat_map(|&g| [g, g, g, 255]).collect(),
        png::ColorType::Indexed => return Err("an indexed picture was not expanded".into()),
    };
    Ok((w, h, rgba))
}

/// The original's picture on the left and the port's on the right, the
/// port's scaled (nearest) to the original's height, four black pixels between.
fn side_by_side(original: &[u8], port: &[u8]) -> Result<Vec<u8>> {
    let (ow, oh, o) = decode(original)?;
    let (pw, ph, p) = decode(port)?;
    let sw = ((pw as u64 * oh as u64) / ph.max(1) as u64).max(1) as u32;
    let gap = 4;
    let (w, h) = (ow + gap + sw, oh);
    let mut out = vec![0u8; (w * h * 4) as usize];
    for y in 0..h {
        for x in 0..w {
            let px = if x < ow {
                let i = ((y * ow + x) * 4) as usize;
                [o[i], o[i + 1], o[i + 2], 255]
            } else if x < ow + gap {
                [0, 0, 0, 255]
            } else {
                let (sx, sy) = ((x - ow - gap) as u64 * pw as u64 / sw as u64, y as u64 * ph as u64 / oh as u64);
                let i = ((sy * pw as u64 + sx) * 4) as usize;
                [p[i], p[i + 1], p[i + 2], 255]
            };
            let j = ((y * w + x) * 4) as usize;
            out[j..j + 4].copy_from_slice(&px);
        }
    }
    let mut bytes = Vec::new();
    {
        let mut e = png::Encoder::new(&mut bytes, w, h);
        e.set_color(png::ColorType::Rgba);
        e.set_depth(png::BitDepth::Eight);
        let mut wr = e.write_header().map_err(|_| "could not write a picture")?;
        wr.write_image_data(&out).map_err(|_| "could not write a picture")?;
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn addresses_are_told_from_values() {
        assert!(address_like(0x8001_2345u32 as f64));
        assert!(address_like((0x8001_2345u32 as i32) as f64));
        assert!(address_like(0x1F80_0010u32 as f64));
        assert!(!address_like(4096.0));
        assert!(!address_like(-2048.0));
        assert!(!address_like(0x8001_2345u32 as f64 + 0.5));
        assert_eq!(number("0x80010000"), Some(0x8001_0000u32 as f64));
    }

    #[test]
    fn published_names_need_every_part() {
        let p = Published { names: ["lara", "x", "speed"].map(String::from).into(), ..Default::default() };
        assert!(p.has("lara") && p.has("lara[2]") && p.has("lara.x") && p.has("speed"));
        assert!(!p.has("ptr") && !p.has("lara.ptr") && !p.has("80012345:u32:x") && !p.has(""));
    }

    #[test]
    fn side_by_side_scales_the_port_to_the_original() {
        let png = |w: u32, h: u32, rgb: [u8; 3]| {
            let mut bytes = Vec::new();
            let mut e = png::Encoder::new(&mut bytes, w, h);
            e.set_color(png::ColorType::Rgb);
            e.set_depth(png::BitDepth::Eight);
            let data: Vec<u8> = (0..w * h).flat_map(|_| rgb).collect();
            e.write_header().unwrap().write_image_data(&data).unwrap();
            bytes
        };
        let out = side_by_side(&png(4, 2, [255, 0, 0]), &png(8, 4, [0, 0, 255])).unwrap();
        let (w, h, rgba) = decode(&out).unwrap();
        assert_eq!((w, h), (4 + 4 + 4, 2));
        assert_eq!(&rgba[..4], &[255, 0, 0, 255]);
        assert_eq!(&rgba[(4 * 4)..(4 * 4 + 4)], &[0, 0, 0, 255]);
        assert_eq!(&rgba[(11 * 4)..(11 * 4 + 4)], &[0, 0, 255, 255]);
    }
}
