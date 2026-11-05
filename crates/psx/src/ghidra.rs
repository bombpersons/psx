//! Optional, out-of-process Ghidra analysis. An ordinary query imports and
//! analyses its source automatically; manifests and query results are reusable.
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant, UNIX_EPOCH};

use clap::Args;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::commands::{Ctx, Op, Outcome};
use crate::{Error, Result};
use psx_media::{PsxExe, disc::sha256_bytes};

const SCRIPT: &str = include_str!("../../../tools/ghidra/PsxGhidra.java");
/// MIPS with the GTE's registers and commands by name and GCC's division
/// checks folded; tools/ghidra/language holds its source.
const LANGUAGE: &str = "MIPS:LE:32:psx";
const LANGUAGE_FILES: [(&str, &str); 3] = [
    ("psx.slaspec", include_str!("../../../tools/ghidra/language/psx.slaspec")),
    ("psx.ldefs", include_str!("../../../tools/ghidra/language/psx.ldefs")),
    ("psx_gte.sinc", include_str!("../../../tools/ghidra/language/psx_gte.sinc")),
];
const FORMAT: u32 = 2;
const FILE_LIMIT: u64 = 64 * 1024 * 1024;
fn outcome(result: Result<Value>) -> Outcome {
    Outcome::report(result.map(|value| (value["status"] != "partial", value)))
}
fn timeout() -> u64 {
    300
}
fn function_timeout() -> u32 {
    30
}
fn limit() -> usize {
    100
}

#[derive(Args, Deserialize, JsonSchema)]
pub struct Input {
    /// PS-X EXE file or disc.cue::/PATH. Imports and analyses automatically.
    #[arg(long, conflicts_with_all = ["analysis", "ram"])]
    pub source: Option<String>,
    /// Analysis manifest returned by an earlier query, instead of source.
    #[arg(long, conflicts_with_all = ["source", "ram"])]
    pub analysis: Option<PathBuf>,
    /// Runtime main RAM image or run directory; requires candidate ranges and entry seeds.
    #[arg(long, conflicts_with_all = ["source", "analysis"])]
    pub ram: Option<PathBuf>,
    /// Candidate code ranges LO-HI in hex (end exclusive); repeatable, with ram only.
    #[arg(long = "range")]
    #[serde(default)]
    pub ranges: Vec<String>,
    /// Hex function entry seeds within candidate ranges; repeatable, with ram only.
    #[arg(long = "entry")]
    #[serde(default)]
    pub entries: Vec<String>,
    /// Explicit hex GP context for a RAM snapshot, overriding run registers.
    #[arg(long)]
    pub gp: Option<String>,
    /// Optional .symbols.json or .sym seeds; repeatable, with source or ram.
    #[arg(long)]
    #[serde(default)]
    pub symbols: Vec<PathBuf>,
    /// Optional Ghidra installation override; normally discovered automatically.
    #[arg(long)]
    pub ghidra_home: Option<PathBuf>,
    /// Total deadline in seconds, including import, analysis and decompilation (1..=3600).
    #[arg(long, default_value_t = timeout())]
    #[serde(default = "timeout")]
    pub timeout: u64,
    /// Build a fresh project from source/ram, preserving previous projects and artifacts.
    #[arg(long)]
    #[serde(default)]
    pub reanalyze: bool,
}

#[derive(Args, Deserialize, JsonSchema)]
pub struct DecompileArgs {
    #[command(flatten)]
    #[serde(flatten)]
    pub input: Input,
    /// Hex address anywhere inside the desired function, e.g. a watched writer PC.
    #[arg(long)]
    pub addr: String,
    /// Per-function decompiler budget in seconds (1..=300).
    #[arg(long, default_value_t = function_timeout())]
    #[serde(default = "function_timeout")]
    pub decompile_timeout: u32,
}
#[derive(Args, Deserialize, JsonSchema)]
pub struct FunctionsArgs {
    #[command(flatten)]
    #[serde(flatten)]
    pub input: Input,
    /// Case-insensitive substring of function name. Unnamed functions are
    /// called `FUN_<address>`, so a hex fragment also matches their addresses.
    #[arg(long)]
    pub filter: Option<String>,
    #[arg(long, default_value_t = 0)]
    #[serde(default)]
    pub offset: usize,
    /// Maximum rows (1..=1000).
    #[arg(long, default_value_t = limit())]
    #[serde(default = "limit")]
    pub limit: usize,
}
#[derive(Clone, Serialize, Deserialize)]
struct Installation {
    home: PathBuf,
    launcher: PathBuf,
    version: String,
    build_sha256: String,
}
fn installation(explicit: Option<&Path>) -> Result<Installation> {
    fn inspect(p: &Path) -> Result<Installation> {
        let home = p.canonicalize().map_err(|e| Error::new("ghidra_unavailable", format!("{}: {e}", p.display())))?;
        let launcher = home.join("support/analyzeHeadless");
        let props = fs::read_to_string(home.join("Ghidra/application.properties")).map_err(|e| {
            Error::new("ghidra_unavailable", format!("{} is not a Ghidra installation: {e}", home.display()))
        })?;
        let version =
            props.lines().find_map(|s| s.strip_prefix("application.version=")).unwrap_or("unknown").to_string();
        if !launcher.is_file() {
            return Err(Error::new("ghidra_unavailable", format!("missing {}", launcher.display())));
        }
        Ok(Installation { home, launcher, version, build_sha256: sha256_bytes(props.as_bytes()) })
    }
    if let Some(p) = explicit {
        return inspect(p);
    }
    if let Some(p) = std::env::var_os("PSX_GHIDRA_HOME") {
        return inspect(Path::new(&p));
    }
    if let Some(path) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&path) {
            if let Ok(p) = dir.join("analyzeHeadless").canonicalize() {
                if let Some(home) = p.parent().and_then(Path::parent) {
                    if let Ok(i) = inspect(home) {
                        return Ok(i);
                    }
                }
            }
        }
    }
    let mut roots = vec![PathBuf::from("/opt"), PathBuf::from("external")];
    if let Some(home) = std::env::var_os("HOME") {
        roots.push(PathBuf::from(home).join("tools"));
    }
    let mut candidates = Vec::new();
    for root in roots {
        if let Ok(entries) = fs::read_dir(root) {
            for entry in entries.flatten() {
                if entry.file_name().to_string_lossy().to_lowercase().starts_with("ghidra") {
                    if let Ok(i) = inspect(&entry.path()) {
                        candidates.push(i);
                    }
                }
            }
        }
    }
    candidates.sort_by(|a, b| a.home.cmp(&b.home));
    candidates.pop().ok_or_else(|| {
        Error::new(
            "ghidra_unavailable",
            "Install Ghidra and set PSX_GHIDRA_HOME to its directory (or pass --ghidra-home). Other psx commands do not require Ghidra.",
        )
    })
}

fn read_json(path: &Path) -> Result<Value> {
    if fs::metadata(path).map_err(|e| Error::io(path, e))?.len() > FILE_LIMIT {
        return Err(Error::new("limit_exceeded", format!("{} exceeds 64 MiB", path.display())));
    }
    serde_json::from_slice(&fs::read(path).map_err(|e| Error::io(path, e))?)
        .map_err(|e| Error::new("invalid_artifact", format!("{}: {e}", path.display())))
}
fn save_json(path: &Path, value: &impl Serialize) -> Result<()> {
    crate::write_atomic(path, &serde_json::to_vec_pretty(value).unwrap()).map_err(Error::from)
}
fn hex(s: &str) -> Result<u32> {
    psx_media::memmap::parse_hex(s).ok_or_else(|| Error::invalid(format!("invalid hex address {s:?}")))
}
fn ram_address(addr: u32) -> Result<u32> {
    let region = psx_media::memmap::resolve(addr);
    if region.region != psx_media::memmap::Region::MainRam {
        return Err(Error::invalid(format!("0x{addr:08X} is not a main-RAM address")));
    }
    Ok(0x8000_0000 | region.offset)
}
fn budget(input: &Input) -> Result<Instant> {
    if !(1..=3600).contains(&input.timeout) {
        return Err(Error::invalid("timeout must be 1..=3600 seconds"));
    }
    if [input.source.is_some(), input.analysis.is_some(), input.ram.is_some()].into_iter().filter(|v| *v).count() != 1 {
        return Err(Error::invalid("give exactly one of source, analysis or ram"));
    }
    if input.ram.is_none() && (!input.ranges.is_empty() || !input.entries.is_empty() || input.gp.is_some()) {
        return Err(Error::invalid("range, entry and gp require ram"));
    }
    if input.analysis.is_some() && (!input.symbols.is_empty() || input.reanalyze) {
        return Err(Error::invalid("symbols/reanalyze require source or ram, not an analysis handle"));
    }
    Ok(crate::deadline_in(input.timeout))
}

/// One owner per analysis, including cached readers: an flock on psx.lock,
/// which the kernel releases however its owner ends, so a killed query never
/// leaves the analysis locked. The file names the owner and, while one runs, its
/// Ghidra worker on the published project; a later owner that finds a worker
/// there knows that query was interrupted (see [`recover`]).
struct Lock {
    file: File,
    /// A query an earlier owner left unfinished on a published project.
    interrupted: Option<Interrupted>,
}
struct Interrupted {
    worker: u32,
    /// The PID namespace the worker's number belongs to.
    ns: String,
    project: PathBuf,
}
impl Lock {
    fn acquire(dir: &Path) -> Result<Self> {
        use std::io::Read;
        let path = dir.join("psx.lock");
        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)
            .map_err(|e| Error::io(&path, e))?;
        match file.try_lock() {
            Ok(()) => {}
            Err(std::fs::TryLockError::WouldBlock) => {
                return Err(Error::new("analysis_busy", format!("{} is held by another query", path.display())));
            }
            Err(std::fs::TryLockError::Error(e)) => return Err(Error::io(&path, e)),
        }
        let mut before = String::new();
        file.read_to_string(&mut before)?;
        let field = |k: &str| before.lines().find_map(|l| l.strip_prefix(k)).map(str::to_string);
        let interrupted = match (field("worker=").and_then(|w| w.parse().ok()), field("project=")) {
            (Some(worker), Some(project)) => {
                Some(Interrupted { worker, ns: field("ns=").unwrap_or_default(), project: project.into() })
            }
            _ => None,
        };
        let mut lock = Self { file, interrupted };
        // The record stays until recover() has dealt with it.
        if lock.interrupted.is_none() {
            lock.worker(None)?;
        }
        Ok(lock)
    }
    /// Records the Ghidra worker now running on the published `project`, or none.
    fn worker(&mut self, worker: Option<(u32, &Path)>) -> Result<()> {
        use std::io::{Seek, SeekFrom};
        self.file.set_len(0)?;
        self.file.seek(SeekFrom::Start(0))?;
        write!(self.file, "psx-query-lock-v2\npid={}\nns={}\n", std::process::id(), pid_namespace())?;
        if let Some((pid, project)) = worker {
            write!(self.file, "worker={pid}\nproject={}\n", project.display())?;
        }
        Ok(())
    }
    /// Deals with a query an earlier owner left unfinished (see [`recover`]):
    /// the project it ran on and what was done, or None.
    fn recover(&mut self) -> Result<Option<(PathBuf, Value)>> {
        let Some(i) = self.interrupted.take() else { return Ok(None) };
        let done = recover(&i)?;
        self.worker(None)?;
        Ok(Some((i.project, done)))
    }
    fn queued(dir: &Path, deadline: Instant, ctx: &Ctx) -> Result<Self> {
        loop {
            if ctx.cancel.as_ref().is_some_and(|c| c.is_cancelled()) {
                return Err(Error::new("cancelled", "Cancelled while queued for this analysis"));
            }
            if Instant::now() >= deadline {
                return Err(Error::new("timeout", "Deadline reached while queued for this analysis"));
            }
            match Self::acquire(dir) {
                Err(e) if e.code == "analysis_busy" => std::thread::sleep(Duration::from_millis(25)),
                other => return other,
            }
        }
    }
}
fn pid_namespace() -> String {
    fs::read_link("/proc/self/ns/pid").map(|p| p.to_string_lossy().into_owned()).unwrap_or_default()
}
/// Processes of the worker started as process group `group` still running on
/// `project` (Linux; elsewhere none can be seen).
fn worker_processes(group: u32, project: &Path) -> Vec<u32> {
    let needle = project.to_string_lossy();
    let Ok(procs) = fs::read_dir("/proc") else { return Vec::new() };
    procs
        .flatten()
        .filter_map(|e| e.file_name().to_str()?.parse::<u32>().ok())
        .filter(|pid| {
            // The process group is the third field after the command name's ")".
            let stat = fs::read_to_string(format!("/proc/{pid}/stat")).unwrap_or_default();
            let pgrp = stat.rsplit_once(')').and_then(|(_, r)| r.split_whitespace().nth(2)?.parse::<u32>().ok());
            pgrp == Some(group)
                && fs::read(format!("/proc/{pid}/cmdline"))
                    .is_ok_and(|c| String::from_utf8_lossy(&c).contains(&*needle))
        })
        .collect()
}
/// After a query interrupted on the published project (its psx killed, or
/// its worker stopped at a deadline): stops a worker still running from it,
/// removes the Ghidra lock it left, and takes the project's metadata as it now
/// is, since a read-only query changes no analysis. Returns what was done.
fn recover(Interrupted { worker, ns, project }: &Interrupted) -> Result<Value> {
    let mut stopped = false;
    if *ns == pid_namespace() && !ns.is_empty() {
        for _ in 0..200 {
            if worker_processes(*worker, project).is_empty() {
                break;
            }
            stopped = true;
            let _ = Command::new("kill").args(["-KILL", "--", &format!("-{worker}")]).status();
            std::thread::sleep(Duration::from_millis(50));
        }
        if !worker_processes(*worker, project).is_empty() {
            return Err(Error::new(
                "analysis_busy",
                format!("an interrupted query's Ghidra worker (process group {worker}) would not stop"),
            ));
        }
        // Ghidra's lock is the stopped worker's; one found with no worker
        // may be a GUI's opened since, and stays.
        if stopped {
            remove_ghidra_lock(project);
        }
    } else if gui_lock(project) {
        return Err(Error::new(
            "analysis_busy",
            "an interrupted query's Ghidra worker ran where psx cannot see it (another PID namespace) and its project lock remains; retry once it ends",
        ));
    }
    Ok(json!({"interrupted_query": true, "worker_stopped": stopped,
        "note": "an earlier query on this analysis was interrupted; psx stopped what remained of it and kept the analysis"}))
}

/// Uses a separate Unix process group so timeout/cancellation kills Java and
/// its native decompiler, not just the shell launcher. No shell interpolation.
/// A worker on a published project is recorded in its analysis's lock.
fn run_worker(
    command: &mut Command,
    log: &Path,
    deadline: Instant,
    ctx: &Ctx,
    published: Option<(&mut Lock, &Path)>,
) -> Result<()> {
    if cfg!(not(unix)) {
        return Err(Error::new("unsupported_platform", "Ghidra worker supervision currently requires Unix."));
    }
    if ctx.cancel.as_ref().is_some_and(|c| c.is_cancelled()) {
        return Err(Error::new("cancelled", "Ghidra query cancelled before worker launch"));
    }
    if Instant::now() >= deadline {
        return Err(Error::new("timeout", "Ghidra query deadline expired before worker launch"));
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    let output = File::create(log).map_err(|e| Error::io(log, e))?;
    command.stdout(output.try_clone()?).stderr(output).stdin(Stdio::null());
    let mut child =
        command.spawn().map_err(|e| Error::new("worker_unavailable", format!("{e}; log: {}", log.display())))?;
    let project = match published {
        Some((lock, project)) => {
            lock.worker(Some((child.id(), project)))?;
            Some(project)
        }
        None => None,
    };
    loop {
        let cancelled = ctx.cancel.as_ref().is_some_and(|c| c.is_cancelled());
        if cancelled || Instant::now() >= deadline {
            #[cfg(unix)]
            {
                let _ = Command::new("kill").args(["-KILL", "--", &format!("-{}", child.id())]).status();
            }
            let _ = child.kill();
            let _ = child.wait();
            project.map(remove_ghidra_lock);
            return Err(Error::new(
                if cancelled { "cancelled" } else { "timeout" },
                format!("Ghidra worker stopped; interrupted analysis is not reused. Log: {}", log.display()),
            ));
        }
        match child.try_wait()? {
            Some(status) if status.success() => return Ok(()),
            Some(status) => {
                return Err(Error::new("ghidra_failed", format!("Ghidra exited {status}; inspect {}", log.display())));
            }
            None => std::thread::sleep(Duration::from_millis(50)),
        }
    }
}

#[derive(Serialize, Deserialize)]
struct Manifest {
    format: u32,
    key: String,
    source: String,
    source_sha256: String,
    input: PathBuf,
    payload_sha256: String,
    exe: Option<PsxExe>,
    mapping: Mapping,
    provenance: Value,
    revision: u64,
    installation: Installation,
    language: String,
    compiler: String,
    script_sha256: String,
    project_dir: PathBuf,
    index: PathBuf,
    project_fingerprint: String,
    warnings: Vec<String>,
}
#[derive(Clone, Serialize, Deserialize)]
struct Mapping {
    base: u32,
    size: u32,
    file_offset: usize,
    entry: u32,
    gp: u32,
    sp: u32,
    bss_addr: u32,
    bss_size: u32,
    code_ranges: Vec<(u32, u32)>,
    entries: Vec<u32>,
}
struct Import {
    source: String,
    bytes: Vec<u8>,
    exe: Option<PsxExe>,
    mapping: Mapping,
    provenance: Value,
    warnings: Vec<String>,
}
fn import(input: &Input) -> Result<Import> {
    if let Some(path) = &input.ram {
        let snap = crate::ram::load(path)?;
        let mut ranges = Vec::new();
        for spec in &input.ranges {
            let (lo, hi) = spec.split_once('-').ok_or_else(|| Error::invalid("range must be LO-HI (end exclusive)"))?;
            let lo = ram_address(crate::commands::addr_arg(lo, "range start")?)?;
            let raw_hi = crate::commands::addr_arg(hi, "range end")?;
            let hi = ram_address(raw_hi.checked_sub(1).ok_or_else(|| Error::invalid("invalid range end"))?)? + 1;
            if lo >= hi || lo & 3 != 0 || hi & 3 != 0 {
                return Err(Error::invalid("empty, wrapping or unaligned code range"));
            }
            ranges.push((lo, hi));
        }
        ranges.sort_unstable();
        if ranges.is_empty() || ranges.windows(2).any(|r| r[0].1 > r[1].0) {
            return Err(Error::invalid("RAM requires non-overlapping candidate code ranges"));
        }
        let mut entries = input
            .entries
            .iter()
            .map(|s| ram_address(crate::commands::addr_arg(s, "entry")?))
            .collect::<Result<Vec<_>>>()?;
        entries.sort_unstable();
        entries.dedup();
        if entries.is_empty()
            || entries.iter().any(|e| e & 3 != 0 || !ranges.iter().any(|&(lo, hi)| lo <= *e && *e < hi))
        {
            return Err(Error::invalid("RAM requires aligned entry seeds within candidate ranges"));
        }
        let report_path = path.join("report.json");
        let mut provenance = json!({"kind":"ram","ram_path":snap.path.canonicalize().map_err(|e| Error::io(path, e))?});
        let mut gp = 0;
        let mut warnings =
            vec!["Snapshot bytes prove residency only at capture time; final RAM cannot establish earlier overlay residency.".into()];
        if path.is_dir() && report_path.is_file() {
            let report = read_json(&report_path)?;
            // Bind metadata only when the report itself identifies these captured bytes.
            let hash = sha256_bytes(&snap.bytes);
            if report["files"]["ram.bin"]["sha256"] == hash {
                provenance["report"] = json!(report_path.canonicalize().map_err(|e| Error::io(&report_path, e))?);
                provenance["report_sha256"] =
                    json!(sha256_bytes(&fs::read(&report_path).map_err(|e| Error::io(&report_path, e))?));
                provenance["frames"] = report["frames"].clone();
                provenance["identity"] = report["identity"].clone();
                provenance["registers"] = report["registers"].clone();
                for file in ["cd_native.jsonl", "cd.jsonl", "bios.jsonl", "loads.json"] {
                    let p = path.join(file);
                    if p.is_file() {
                        provenance[file] = json!({"path":p.canonicalize().map_err(|e| Error::io(&p, e))?,"sha256":sha256_bytes(&fs::read(&p).map_err(|e| Error::io(&p, e))?)});
                    }
                }
                gp = crate::commands::addr_value(&report["registers"]["gp"]).unwrap_or(0);
            } else {
                warnings.push(
                    "Run report does not bind these RAM bytes; frame/register/media metadata was not associated."
                        .into(),
                );
            }
        }
        if let Some(value) = &input.gp {
            gp = hex(value)?;
        }
        return Ok(Import {
            source: snap.path.display().to_string(),
            bytes: snap.bytes,
            exe: None,
            mapping: Mapping {
                base: 0x8000_0000,
                size: 0x20_0000,
                file_offset: 0,
                entry: entries[0],
                gp,
                sp: 0,
                bss_addr: 0,
                bss_size: 0,
                code_ranges: ranges,
                entries,
            },
            provenance,
            warnings,
        });
    }
    let source = input.source.as_ref().ok_or_else(|| Error::invalid("missing source"))?;
    let bytes = psx_media::source::read(source)?;
    let exe = PsxExe::parse(&bytes)?;
    let base = ram_address(exe.text_addr)?;
    let end = base
        .checked_add(exe.text_size)
        .filter(|&end| end <= 0x8020_0000)
        .ok_or_else(|| Error::invalid("EXE payload exceeds main RAM"))?;
    let entry = ram_address(exe.pc)?;
    if exe.text_size == 0
        || exe.payload(&bytes).len() != exe.text_size as usize
        || !(base..end).contains(&entry)
        || entry & 3 != 0
        || base & 3 != 0
    {
        return Err(Error::invalid("EXE has a truncated/empty payload or an invalid entry address"));
    }
    let bss = if exe.bss_size > 0 {
        let start = ram_address(exe.bss_addr)?;
        let stop = start
            .checked_add(exe.bss_size)
            .filter(|&n| n <= 0x8020_0000)
            .ok_or_else(|| Error::invalid("BSS exceeds main RAM"))?;
        if start < end && stop > base {
            return Err(Error::invalid("BSS overlaps the loaded payload"));
        }
        start
    } else {
        0
    };
    let mapping = Mapping {
        base,
        size: exe.text_size,
        file_offset: 0x800,
        entry,
        // Most headers leave gp 0; the startup code sets it.
        gp: if exe.gp != 0 {
            exe.gp
        } else {
            psx_symbols::build::startup_gp(exe.payload(&bytes), exe.text_addr, exe.pc)
                .and_then(|gp| ram_address(gp).ok())
                .unwrap_or(0)
        },
        sp: exe.initial_sp().unwrap_or(0),
        bss_addr: bss,
        bss_size: exe.bss_size,
        code_ranges: vec![(base, end)],
        entries: vec![entry],
    };
    Ok(Import {
        source: source.clone(),
        warnings: exe.warnings.clone(),
        bytes,
        exe: Some(exe),
        mapping,
        provenance: json!({"kind":"exe"}),
    })
}
struct Analysis {
    path: PathBuf,
    manifest: Manifest,
    index: Value,
    fresh_query: Option<Value>,
    lock: Lock,
    /// What was done about a query an earlier owner left unfinished.
    recovered: Option<Value>,
}

fn fingerprint(dir: &Path) -> Result<String> {
    fn walk(dir: &Path, out: &mut Vec<String>) -> Result<()> {
        let mut entries =
            fs::read_dir(dir).map_err(|e| Error::io(dir, e))?.collect::<std::result::Result<Vec<_>, _>>()?;
        entries.sort_by_key(|e| e.file_name());
        for e in entries {
            let name = e.file_name().to_string_lossy().into_owned();
            let m = e.metadata()?;
            if m.is_dir() {
                out.push(name);
                walk(&e.path(), out)?;
            } else if !name.ends_with(".lock") && !name.ends_with(".lock~") {
                out.push(format!(
                    "{name}:{}:{}",
                    m.len(),
                    m.modified()?.duration_since(UNIX_EPOCH).unwrap_or_default().as_nanos()
                ));
            }
        }
        Ok(())
    }
    let mut rows = Vec::new();
    walk(dir, &mut rows)?;
    Ok(sha256_bytes(rows.join("\n").as_bytes()))
}
/// Removes the lock a worker psx itself stopped left in its project.
fn remove_ghidra_lock(project: &Path) {
    for name in ["project.lock", "project.lock~"] {
        let _ = fs::remove_file(project.join(name));
    }
}
fn gui_lock(dir: &Path) -> bool {
    ["project.lock", "project.lock~"].iter().any(|name| dir.join(name).exists())
}
/// Removes a project psx created and has superseded. Projects open in the
/// GUI or edited outside psx (fingerprint mismatch) are left alone.
fn discard_project(dir: &Path, expected_fingerprint: Option<&str>) {
    if gui_lock(dir) || expected_fingerprint.is_some_and(|f| fingerprint(dir).ok().as_deref() != Some(f)) {
        return;
    }
    let _ = fs::remove_dir_all(dir);
}
/// Deletes a fresh project directory unless the import publishes it (or a
/// timeout deliberately retains it), so failed attempts do not accumulate.
struct PendingProject(Option<PathBuf>);
impl PendingProject {
    fn keep(&mut self) {
        self.0 = None;
    }
}
impl Drop for PendingProject {
    fn drop(&mut self) {
        if let Some(dir) = self.0.take() {
            discard_project(&dir, None);
        }
    }
}
/// The psx language built for this installation from its own MIPS sources,
/// once, under .psx/ghidra/language; returns the extension's directory.
fn language(install: &Installation, cache: &Path) -> Result<PathBuf> {
    let mut sources = install.build_sha256.clone();
    for (name, text) in LANGUAGE_FILES {
        sources += name;
        sources += &sha256_bytes(text.as_bytes());
    }
    let dir = cache.join("language").join(&sha256_bytes(sources.as_bytes())[..16]);
    let ext = dir.join("PsxGte");
    if ext.join("data/languages/psx.sla").is_file() {
        return Ok(ext);
    }
    let tmp = dir.with_extension(crate::stamp());
    let languages = tmp.join("PsxGte/data/languages");
    fs::create_dir_all(&languages).map_err(|e| Error::io(&languages, e))?;
    let mips = install.home.join("Ghidra/Processors/MIPS/data/languages");
    for entry in fs::read_dir(&mips).map_err(|e| Error::io(&mips, e))? {
        let path = entry?.path();
        let name = path.file_name().unwrap_or_default().to_string_lossy().into_owned();
        if name.ends_with(".sinc") || name == "mips32.pspec" || name == "mips32le.cspec" {
            fs::copy(&path, languages.join(&name)).map_err(|e| Error::io(&path, e))?;
        }
    }
    for (name, text) in LANGUAGE_FILES {
        fs::write(languages.join(name), text)?;
    }
    let ext_tmp = tmp.join("PsxGte");
    fs::write(
        ext_tmp.join("extension.properties"),
        format!(
            "name=PsxGte\ndescription=PlayStation MIPS for psx\nauthor=psx\ncreatedOn=\nversion={}\n",
            install.version
        ),
    )?;
    fs::write(ext_tmp.join("Module.manifest"), "")?;
    let log = tmp.join("sleigh.log");
    let out = File::create(&log).map_err(|e| Error::io(&log, e))?;
    let status = Command::new(install.home.join("support/sleigh"))
        .arg(languages.join("psx.slaspec"))
        .arg(languages.join("psx.sla"))
        .env("XDG_CONFIG_HOME", &tmp)
        .env("XDG_CACHE_HOME", &tmp)
        .stdout(out.try_clone()?)
        .stderr(out)
        .stdin(Stdio::null())
        .status()
        .map_err(|e| Error::new("ghidra_unavailable", format!("sleigh: {e}")))?;
    if !status.success() || !languages.join("psx.sla").is_file() {
        return Err(Error::new(
            "ghidra_failed",
            format!("building the psx language failed; inspect {}", log.display()),
        ));
    }
    // Another process may have built it meanwhile; either copy will do.
    if fs::rename(&ext_tmp, &ext).is_err() && !ext.join("data/languages/psx.sla").is_file() {
        fs::create_dir_all(&dir).map_err(|e| Error::io(&dir, e))?;
        fs::rename(&ext_tmp, &ext).map_err(|e| Error::io(&ext, e))?;
    }
    let _ = fs::remove_dir_all(&tmp);
    Ok(ext)
}
/// The user name Java reports (from the password database), which Ghidra puts
/// in its settings directory's name when XDG_CONFIG_HOME is set outside the
/// user's home (Ghidra's ApplicationUtilities.getUserSpecificDirName).
fn java_user_name() -> String {
    let uid = fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|s| s.lines().find_map(|l| l.strip_prefix("Uid:")?.split_whitespace().next().map(str::to_owned)));
    let from_passwd = uid.and_then(|uid| {
        fs::read_to_string("/etc/passwd").ok()?.lines().find_map(|l| {
            let f: Vec<&str> = l.split(':').collect();
            (f.len() > 2 && f[2] == uid).then(|| f[0].to_owned())
        })
    });
    let name = from_passwd.or_else(|| std::env::var("USER").ok()).unwrap_or_else(|| "?".into());
    name.split_whitespace().collect()
}
/// Java's `user.home` (`ApplicationUtilities.getJavaUserHomeDir`): the HOME
/// environment variable, which the JVM on Linux hands to `user.home`.
fn java_user_home() -> Option<PathBuf> {
    std::env::var_os("HOME").filter(|home| !home.is_empty()).map(PathBuf::from)
}
/// The settings directory Ghidra computes for a run whose XDG_CONFIG_HOME is
/// `attempt/config`: `ghidra_<version>_<release>` under `<config>/ghidra`
/// when `<config>` lies within the user's home, else under
/// `<config>/<user>-ghidra` (Ghidra 12.1.4's ApplicationUtilities
/// .getUserSpecificDirName, which prefixes the name unless
/// FileUtilities.isPathContainedWithin holds between the home and the
/// parent). Projects under the user's home take the plain name; without a
/// HOME Ghidra itself fails, and the prefixed name is as good as any.
fn ghidra_settings_dir(attempt: &Path, home: Option<&Path>, user: &str, versioned: &str) -> PathBuf {
    let config = attempt.join("config");
    let name = if home.is_some_and(|home| path_contains(home, &config)) {
        "ghidra".to_owned()
    } else {
        format!("{user}-ghidra")
    };
    config.join(name).join(versioned)
}
/// FileUtilities.isPathContainedWithin: whether `child` is `parent` or
/// beneath it, by canonical path.
fn path_contains(parent: &Path, child: &Path) -> bool {
    match (parent.canonicalize(), child.canonicalize()) {
        (Ok(parent), Ok(child)) => child.starts_with(parent),
        _ => false,
    }
}
fn command(install: &Installation, attempt: &Path, language_id: &str, ctx: &Ctx) -> Result<Command> {
    let mut cmd = Command::new(&install.launcher);
    // Keep Ghidra's generated configuration and compiled scripts in the project,
    // including when psx is used in a filesystem sandbox.
    for (key, dir) in [("XDG_CONFIG_HOME", "config"), ("XDG_CACHE_HOME", "cache")] {
        let p = attempt.join(dir);
        fs::create_dir_all(&p).map_err(|e| Error::io(&p, e))?;
        cmd.env(key, p);
    }
    cmd.env("GHIDRA_HEADLESS_MAXMEM", "2G");
    // PID namespaces can share /tmp/hsperfdata_* while reusing JVM PIDs. A
    // perf-data warning on stdout corrupts Ghidra LaunchSupport's Java-home probe.
    cmd.env(
        "JAVA_TOOL_OPTIONS",
        format!("{} -XX:-UsePerfData", std::env::var("JAVA_TOOL_OPTIONS").unwrap_or_default()),
    );
    // Ghidra loads extensions from its settings directory: put the psx language
    // there for the analyses that use it.
    if language_id != LANGUAGE {
        return Ok(cmd);
    }
    let ext = language(install, &ctx.root.join(".psx/ghidra"))?;
    let props = fs::read_to_string(install.home.join("Ghidra/application.properties"))?;
    let prop = |key: &str| {
        props.lines().find_map(|l| l.strip_prefix(key)?.strip_prefix('=')).unwrap_or_default().trim().to_owned()
    };
    let versioned = format!(
        "{}_{}_{}",
        prop("application.name").to_lowercase(),
        prop("application.version"),
        prop("application.release.name")
    );
    let settings = ghidra_settings_dir(attempt, java_user_home().as_deref(), &java_user_name(), &versioned);
    let installed = settings.join("Extensions/PsxGte");
    if !installed.join("data/languages/psx.sla").is_file() {
        copy_dir(&ext, &installed)?;
    }
    Ok(cmd)
}
fn script_dir(dir: &Path) -> Result<PathBuf> {
    let p = dir.join("scripts");
    fs::create_dir_all(&p).map_err(|e| Error::io(&p, e))?;
    let file = p.join("PsxGhidra.java");
    if fs::read_to_string(&file).ok().as_deref() != Some(SCRIPT) {
        fs::write(file, SCRIPT)?;
    }
    Ok(p)
}
fn cached(path: PathBuf, mut lock: Lock) -> Result<Analysis> {
    let value = read_json(&path)?;
    if value["format"] != FORMAT {
        return Err(Error::new(
            "stale_analysis",
            "Analysis format changed; query source or ram again to create a compatible analysis.",
        ));
    }
    let mut manifest: Manifest = serde_json::from_value(value).map_err(|e| Error::new("invalid_artifact", e))?;
    if manifest.format != FORMAT || manifest.script_sha256 != sha256_bytes(SCRIPT.as_bytes()) {
        return Err(Error::new(
            "stale_analysis",
            "Analysis format/scripts changed; query the source again to create a compatible analysis.",
        ));
    }
    let recovered = lock.recover()?;
    // An interrupted read-only query may have touched the project's metadata.
    let ours = recovered.as_ref().is_some_and(|(p, _)| *p == manifest.project_dir);
    let recovered = recovered.map(|(_, done)| done);
    if gui_lock(&manifest.project_dir) {
        return Err(Error::new(
            "analysis_busy",
            "Close this analysis in the Ghidra GUI before querying it. With no Ghidra open on it, project.lock was left by a worker killed together with its psx: remove it.",
        ));
    }
    let now = fingerprint(&manifest.project_dir)?;
    if now != manifest.project_fingerprint && ours {
        manifest.project_fingerprint = now;
        save_json(&path, &manifest)?;
    } else if now != manifest.project_fingerprint {
        return Err(Error::new(
            "stale_analysis",
            "Ghidra project changed outside psx; cached results are no longer valid. Query source with reanalyze=true for a fresh import; the edited project is preserved.",
        ));
    }
    let bytes = fs::read(&manifest.input).map_err(|e| Error::io(&manifest.input, e))?;
    if sha256_bytes(&bytes) != manifest.source_sha256 {
        return Err(Error::new("invalid_artifact", "Analysis input copy has changed."));
    }
    let index = read_json(&manifest.index)?;
    Ok(Analysis { path, manifest, index, fresh_query: None, lock, recovered })
}

/// Names to apply before analysis, from `.symbols.json` reports (checked
/// against the input's bytes) or plain `.sym` files (unverified, with a warning).
fn analysis_symbols(input: &Input, bytes: &[u8], mapping: &Mapping, warnings: &mut Vec<String>) -> Result<Vec<Value>> {
    let (base, end) = (mapping.base, mapping.base + mapping.size);
    let mut symbols = Vec::new();
    for path in &input.symbols {
        let text = fs::read_to_string(path).map_err(|e| Error::io(path, e))?;
        if path.to_string_lossy().ends_with(".symbols.json") {
            let value: Value =
                serde_json::from_str(&text).map_err(|e| Error::malformed(format!("{}: {e}", path.display())))?;
            let original = psx_media::source::read(
                value["source"].as_str().ok_or_else(|| Error::invalid("symbols report lacks source"))?,
            )?;
            let original_exe = PsxExe::parse(&original)?;
            let original_base = ram_address(original_exe.text_addr)?;
            let payload = original_exe.payload(&original);
            let offset = original_base
                .checked_sub(base)
                .ok_or_else(|| Error::invalid("symbol EXE is outside mapping"))? as usize;
            let mapped = &bytes[mapping.file_offset..mapping.file_offset + mapping.size as usize];
            if payload.len() != original_exe.text_size as usize
                || mapped.get(offset..offset + payload.len()) != Some(payload)
            {
                return Err(Error::invalid(format!(
                    "{} describes code that differs from analysis input",
                    path.display()
                )));
            }
            if value["exe_sha256"].as_str().is_some_and(|s| s != sha256_bytes(&original)) {
                return Err(Error::invalid(format!("{} is stale: its source bytes have changed", path.display())));
            }
            let list = value["report"]["symbols"]
                .as_array()
                .ok_or_else(|| Error::invalid("symbols report lacks report.symbols"))?;
            for s in list {
                let a = crate::commands::addr_value(&s["address"])
                    .ok_or_else(|| Error::invalid("invalid symbol address"))?;
                symbols.push(json!({"address":ram_address(a)?,"name":s["name"],"kind":s["kind"]}));
            }
        } else {
            warnings.push(format!("{}: plain .sym names are unverified", path.display()));
            for line in text.lines() {
                if line.trim_start().starts_with(['#', ';']) {
                    continue;
                }
                let mut words = line.split_whitespace();
                if let (Some(a), Some(name)) = (words.next(), words.next()) {
                    symbols.push(json!({"address":ram_address(hex(a)?)?,"name":name,"kind":"function"}));
                }
            }
        }
    }
    for s in &symbols {
        let a = s["address"].as_u64().unwrap_or(0) as u32;
        if !(base..end).contains(&a)
            || a & 3 != 0
            || (s["kind"] == "function" && !mapping.code_ranges.iter().any(|&(lo, hi)| lo <= a && a < hi))
        {
            return Err(Error::invalid("symbol address outside payload or unaligned"));
        }
        if s["name"].as_str().is_none_or(str::is_empty)
            || !matches!(s["kind"].as_str(), Some("function" | "label" | "local"))
        {
            return Err(Error::invalid("symbols need a nonempty name and kind function, label or local"));
        }
    }
    Ok(symbols)
}

/// The psx language's sleigh/pspec sources: an analysis made with an older
/// spec disassembled differently, so a change to them is a new analysis.
fn language_files_sha256() -> String {
    let mut all = Vec::new();
    for (name, text) in LANGUAGE_FILES {
        all.extend_from_slice(name.as_bytes());
        all.push(0);
        all.extend_from_slice(text.as_bytes());
        all.push(0);
    }
    sha256_bytes(&all)
}

fn prepare(input: &Input, addr: Option<u32>, decompile_timeout: u32, deadline: Instant, ctx: &Ctx) -> Result<Analysis> {
    if let Some(path) = &input.analysis {
        let path = path.canonicalize().map_err(|e| Error::io(path, e))?;
        let lock = Lock::queued(path.parent().ok_or_else(|| Error::invalid("invalid manifest path"))?, deadline, ctx)?;
        return cached(path, lock);
    }
    let Import { source, bytes, exe, mapping, provenance, mut warnings } = import(input)?;
    let base = mapping.base;
    let end = base + mapping.size;
    if addr.is_some_and(|a| !(base..end).contains(&a)) {
        return Err(Error::invalid("requested address is outside the mapped input"));
    }
    let install = installation(input.ghidra_home.as_deref())?;
    let symbols = analysis_symbols(input, &bytes, &mapping, &mut warnings)?;
    let source_hash = sha256_bytes(&bytes);
    let identity = json!({"format":FORMAT,"source":source_hash,"ghidra":install.build_sha256,"language":LANGUAGE,"language_files":language_files_sha256(),"compiler":"default","script":sha256_bytes(SCRIPT.as_bytes()),"symbols":symbols,"mapping":mapping,"capture":{"frames":provenance["frames"],"identity":provenance["identity"],"registers":provenance["registers"]}});
    let key = sha256_bytes(&serde_json::to_vec(&identity).unwrap());
    let dir = ctx.root.join(".psx/ghidra").join(&key);
    fs::create_dir_all(&dir).map_err(|e| Error::io(&dir, e))?;
    let dir = dir.canonicalize().map_err(|e| Error::io(&dir, e))?;
    let mut lock = Lock::queued(&dir, deadline, ctx)?;
    let path = dir.join("analysis.json");
    if path.is_file() && !input.reanalyze {
        return cached(path, lock);
    }
    lock.recover()?;
    let attempt = dir.join(crate::stamp());
    fs::create_dir(&attempt).map_err(|e| Error::io(&attempt, e))?;
    // Ghidra 12 rejects project paths with any hidden path component, so its
    // database cannot live beneath .psx. Inputs/indexes remain there.
    let project_dir = ctx.root.join("ghidra-projects").join(format!("{}-{}", &key[..16], crate::stamp()));
    fs::create_dir_all(&project_dir).map_err(|e| Error::io(&project_dir, e))?;
    let mut pending = PendingProject(Some(project_dir.clone()));
    let scripts = script_dir(&attempt)?;
    let copy = attempt.join(if exe.is_some() { "source.ps-exe" } else { "ram.bin" });
    fs::write(&copy, &bytes)?;
    let payload = attempt.join("payload.bin");
    fs::write(&payload, &bytes[mapping.file_offset..mapping.file_offset + mapping.size as usize])?;
    let index_path = attempt.join("index.json");
    let setup_result = attempt.join("setup-result.json");
    let mut request = json!({"mode":"setup","base":base,"size":mapping.size,"entry":mapping.entry,"gp":mapping.gp,"sp":mapping.sp,"bss_addr":mapping.bss_addr,"bss_size":mapping.bss_size,"code_ranges":mapping.code_ranges,"entries":mapping.entries,"symbols":symbols,"setup_result":setup_result});
    let setup = attempt.join("setup.json");
    save_json(&setup, &request)?;
    request = json!({"mode":"export","index":index_path,"decompile_timeout":decompile_timeout,"query_result":attempt.join("query.json")});
    if let Some(a) = addr {
        request["addr"] = json!(a);
    }
    let export = attempt.join("export.json");
    save_json(&export, &request)?;
    let mut cmd = command(&install, &attempt, LANGUAGE, ctx)?;
    cmd.arg(&project_dir)
        .arg("project")
        .arg("-import")
        .arg(&payload)
        .args([
            "-loader",
            "BinaryLoader",
            "-loader-baseAddr",
            &format!("0x{base:08X}"),
            "-processor",
            LANGUAGE,
            "-cspec",
            "default",
            "-max-cpu",
            "2",
        ])
        .arg("-analysisTimeoutPerFile")
        .arg(deadline.saturating_duration_since(Instant::now()).as_secs().max(1).to_string())
        .arg("-scriptPath")
        .arg(scripts)
        .arg("-preScript")
        .arg("PsxGhidra.java")
        .arg(setup)
        .arg("-postScript")
        .arg("PsxGhidra.java")
        .arg(export);
    run_worker(&mut cmd, &attempt.join("worker.log"), deadline, ctx, None)?;
    let setup_out = read_json(&setup_result).map_err(|e| {
        Error::new("ghidra_failed", format!("{}; inspect {}", e.message, attempt.join("worker.log").display()))
    })?;
    if let Some(w) = setup_out["warnings"].as_array() {
        warnings.extend(w.iter().filter_map(Value::as_str).map(str::to_owned));
    }
    let index = read_json(&index_path).map_err(|e| {
        Error::new("ghidra_failed", format!("{}; inspect {}", e.message, attempt.join("worker.log").display()))
    })?;
    if index["analysis_timed_out"] == true {
        pending.keep();
        return Err(Error::new(
            "analysis_timeout",
            format!(
                "Partial project retained at {}; retry with a larger timeout. It is not cached as complete.",
                project_dir.display()
            ),
        ));
    }
    warnings.push("Only canonical KSEG0 main RAM is mapped; query aliases are folded, but static references using other RAM mirrors may remain unresolved.".into());
    warnings.push("GTE (COP2) operations are shown by register and command name without their arithmetic, and PS1 load delays are not modelled; verify affected pseudocode against instructions and runtime evidence.".into());
    let manifest = Manifest {
        format: FORMAT,
        key,
        source: source.clone(),
        source_sha256: source_hash,
        input: copy,
        payload_sha256: sha256_bytes(&bytes[mapping.file_offset..mapping.file_offset + mapping.size as usize]),
        exe,
        mapping,
        provenance,
        revision: 0,
        installation: install,
        language: LANGUAGE.into(),
        compiler: "default".into(),
        script_sha256: sha256_bytes(SCRIPT.as_bytes()),
        project_fingerprint: fingerprint(&project_dir)?,
        project_dir,
        index: index_path,
        warnings,
    };
    // A reanalysis replaces the cached one: its project is superseded.
    let previous = read_json(&path).ok().and_then(|old| {
        let dir = PathBuf::from(old["project_dir"].as_str()?);
        Some((dir, old["project_fingerprint"].as_str()?.to_string()))
    });
    save_json(&path, &manifest)?;
    pending.keep();
    if let Some((dir, fp)) = previous {
        // Only a project psx made, in this project's ghidra-projects/.
        if dir != manifest.project_dir && dir.parent() == manifest.project_dir.parent() {
            discard_project(&dir, Some(&fp));
        }
    }
    let fresh_query = if let Some(a) = addr {
        let value = read_json(&attempt.join("query.json"))?;
        if value["status"] == "complete" {
            save_json(&attempt.join(format!("decompile-{a:08X}.json")), &value)?;
        }
        Some(value)
    } else {
        None
    };
    Ok(Analysis { path, manifest, index, fresh_query, lock, recovered: None })
}

fn summary(a: &Analysis) -> Value {
    let mut out = json!({"analysis":a.path,"source":a.manifest.source,"source_sha256":a.manifest.source_sha256,"ghidra_version":a.manifest.installation.version,
        "project":a.manifest.project_dir.join("project.gpr"),"function_count":a.index["functions"].as_array().map_or(0,Vec::len),"warnings":a.manifest.warnings,
        "mapping":a.manifest.mapping,"provenance":a.manifest.provenance,"revision":a.manifest.revision,"references_truncated":a.index["references_truncated"],"cop2_instructions":a.index["cop2_instructions"],"rfe_instructions":a.index["rfe_instructions"]});
    if let Some(r) = &a.recovered {
        out["recovered"] = r.clone();
    }
    out
}
fn references(a: &Analysis) -> &[Value] {
    a.index["references"].as_array().map_or(&[], Vec::as_slice)
}
fn same_address(value: &Value, target: u32) -> bool {
    value.as_str().and_then(|s| hex(s).ok()).is_some_and(|addr| ram_address(addr).unwrap_or(addr) == target)
}
fn page(rows: &[Value], offset: usize, count: usize) -> Value {
    let items = rows.iter().skip(offset).take(count).collect::<Vec<_>>();
    let mut result = crate::page::metadata(rows.len(), offset, items.len(), count);
    result["items"] = json!(items);
    result
}
fn valid_limit(n: usize) -> Result<()> {
    if (1..=1000).contains(&n) { Ok(()) } else { Err(Error::invalid("limit must be 1..=1000")) }
}

/// For `context`: whether Ghidra and a Java runtime are found, without starting either.
pub fn status() -> Value {
    let java = std::env::var_os("JAVA_HOME")
        .map(|h| PathBuf::from(h).join("bin/java"))
        .filter(|p| p.is_file())
        .or_else(|| {
            std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
                .map(|d| d.join("java"))
                .find(|p| p.is_file())
        });
    match installation(None) {
        Ok(i) => json!({"available": java.is_some(), "home": i.home, "version": i.version, "java": java,
            "validated_version": "12.1.4", "language": LANGUAGE}),
        Err(e) => json!({"available": false, "reason": e.to_value()["error"]["message"], "java": java,
            "setup": "set PSX_GHIDRA_HOME to a Ghidra installation (or put analyzeHeadless on PATH); code decompile and functions need it"}),
    }
}
impl Op for FunctionsArgs {
    fn exec(self, ctx: &Ctx) -> Outcome {
        outcome((|| {
            valid_limit(self.limit)?;
            let deadline = budget(&self.input)?;
            let a = prepare(&self.input, None, function_timeout(), deadline, ctx)?;
            let filter = self.filter.unwrap_or_default().to_lowercase();
            // The project's names that bind to this analysis name its rows.
            let (names, _) = names_for(&ctx.root, &a.manifest);
            let named: std::collections::HashMap<u32, String> = names["functions"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|f| {
                    let text = f["name"]["text"].as_str()?;
                    let guess = if f["name"]["guess"] == true { "?" } else { "" };
                    Some((psx_media::memmap::parse_hex(f["address"].as_str()?)?, format!("{text}{guess}")))
                })
                .collect();
            let rows: Vec<Value> = a.index["functions"]
                .as_array()
                .ok_or_else(|| Error::new("invalid_artifact", "missing functions index"))?
                .iter()
                .map(|f| {
                    let mut f = f.clone();
                    if let Some(n) =
                        f["entry"].as_str().and_then(psx_media::memmap::parse_hex).and_then(|e| named.get(&e))
                    {
                        f["name"] = json!(n);
                        f["name_source"] = json!("names");
                    }
                    f
                })
                .filter(|f| f["name"].as_str().unwrap_or("").to_lowercase().contains(&filter))
                .collect();
            let mut out = summary(&a);
            out["functions"] = page(&rows, self.offset, self.limit);
            Ok(out)
        })())
    }
}
impl Op for DecompileArgs {
    fn exec(self, ctx: &Ctx) -> Outcome {
        outcome((|| {
            if !(1..=300).contains(&self.decompile_timeout) {
                return Err(Error::invalid("decompile_timeout must be 1..=300"));
            }
            let deadline = budget(&self.input)?;
            let addr = ram_address(crate::commands::addr_arg(&self.addr, "address")?)?;
            let mut a = prepare(&self.input, Some(addr), self.decompile_timeout, deadline, ctx)?;
            let (names, problems) = names_for(&ctx.root, &a.manifest);
            let dir = a.manifest.index.parent().unwrap();
            let result_path = match &names {
                Value::Null => dir.join(format!("decompile-{addr:08X}.json")),
                n => dir.join(format!(
                    "decompile-{addr:08X}-{}.json",
                    &sha256_bytes(&serde_json::to_vec(n).unwrap())[..16]
                )),
            };
            // The analysis's own first decompile is without names.
            let fresh = a.fresh_query.take().filter(|_| names.is_null());
            let mut result = if let Some(result) = fresh {
                result
            } else if result_path.is_file() {
                read_json(&result_path)?
            } else {
                let install = installation(self.input.ghidra_home.as_deref().or(Some(&a.manifest.installation.home)))?;
                if install.build_sha256 != a.manifest.installation.build_sha256 {
                    return Err(Error::new("stale_analysis", "Ghidra version changed; query source to reanalyse"));
                }
                let attempt = a.manifest.index.parent().unwrap();
                let request = attempt.join(format!("query-{}.json", crate::stamp()));
                let output = request.with_extension("result.json");
                save_json(
                    &request,
                    &json!({"mode":"query","addr":addr,"decompile_timeout":self.decompile_timeout,"query_result":output,"names":names}),
                )?;
                let mut cmd = command(&install, attempt, &a.manifest.language, ctx)?;
                cmd.arg(&a.manifest.project_dir)
                    .arg("project")
                    .args(["-process", "payload.bin", "-noanalysis", "-readOnly", "-max-cpu", "2"])
                    .arg("-scriptPath")
                    .arg(script_dir(attempt)?)
                    .arg("-postScript")
                    .arg("PsxGhidra.java")
                    .arg(&request);
                let project = a.manifest.project_dir.clone();
                run_worker(&mut cmd, &request.with_extension("log"), deadline, ctx, Some((&mut a.lock, &project)))?;
                let result = read_json(&output).map_err(|e| {
                    Error::new(
                        "ghidra_failed",
                        format!("{}; inspect {}", e.message, request.with_extension("log").display()),
                    )
                })?;
                // Ghidra may touch project metadata even in read-only mode.
                a.manifest.project_fingerprint = fingerprint(&a.manifest.project_dir)?;
                save_json(&a.path, &a.manifest)?;
                a.lock.worker(None)?;
                if result["status"] == "complete" {
                    save_json(&result_path, &result)?;
                }
                result
            };
            let entry = result["function"]["entry"].as_str().unwrap_or("");
            let entry_addr = hex(entry).ok();
            let callers: Vec<Value> = references(&a)
                .iter()
                .filter(|r| {
                    r["call"] == true
                        && r["to_space"] == "ram"
                        && entry_addr.is_some_and(|entry| same_address(&r["to"], entry))
                })
                .cloned()
                .collect();
            let callees: Vec<Value> =
                references(&a).iter().filter(|r| r["call"] == true && r["function"] == entry).cloned().collect();
            let mut out = summary(&a);
            out["requested_addr"] = json!(self.addr);
            out["addr"] = json!(format!("0x{addr:08X}"));
            out["status"] = json!(if result["status"] == "complete" { "complete" } else { "partial" });
            out["decompile_status"] = result["status"].take();
            out["function"] = result["function"].take();
            out["pseudocode"] = result["pseudocode"].take();
            out["diagnostic"] = result["diagnostic"].take();
            if let Some(note) = unset_registers(&a, out["pseudocode"].as_str().unwrap_or(""), entry_addr) {
                out["note"] = json!(note);
            }
            if !names.is_null() {
                let mut guesses = Vec::new();
                guessed(&names, &mut guesses);
                out["pseudocode"] = json!(mark_guesses(out["pseudocode"].as_str().unwrap_or(""), &guesses));
            }
            if !names.is_null() || !problems.is_empty() {
                let mut n = result["names"].take();
                if n.is_null() {
                    n = json!({});
                }
                // A line the files' check already reported is not reported again.
                let line = |e: &str| e.splitn(3, ':').take(2).collect::<Vec<_>>().join(":");
                let reported: Vec<String> = problems.iter().map(|p| line(p)).collect();
                let errors = n["errors"]
                    .as_array()
                    .cloned()
                    .unwrap_or_default()
                    .into_iter()
                    .filter(|e| !e.as_str().is_some_and(|e| reported.contains(&line(e))));
                n["errors"] = json!(problems.into_iter().map(Value::from).chain(errors).collect::<Vec<_>>());
                out["names"] = n;
            }
            // One line a call: where it is, and the function it is in or calls.
            let call = |r: &Value, other: &str, sep: &str| {
                let kind = r["type"].as_str().filter(|t| *t != "UNCONDITIONAL_CALL");
                let mut s = format!("{} {sep} {}", r["from"].as_str().unwrap_or("?"), r[other].as_str().unwrap_or("?"));
                if let Some(k) = kind {
                    s += &format!(" ({})", k.to_ascii_lowercase());
                }
                json!(s)
            };
            let callers: Vec<Value> = callers.iter().map(|r| call(r, "function", "in")).collect();
            let callees: Vec<Value> = callees.iter().map(|r| call(r, "to", "->")).collect();
            out["callers"] = page(&callers, 0, 100);
            out["callees"] = page(&callees, 0, 100);
            out["artifact"] = if result_path.exists() { json!(result_path) } else { Value::Null };
            Ok(out)
        })())
    }
}

/// The project's names that apply to an analysis, as the query script
/// takes them (Null when there are none), and the names files' problems as
/// `file:line: message`. Names of an overlay (`in RACE.OVL`) apply to an
/// analysis of that file; the others to any analysis whose memory holds
/// them, which the script checks.
fn names_for(root: &Path, manifest: &Manifest) -> (Value, Vec<String>) {
    use crate::names_bind::{Bind, Code, Inventory};
    use psx_symbols::names::{Type, TypeBody};
    let names = psx_symbols::names::load(root);
    let mut problems: Vec<String> = names.problems.iter().map(|p| format!("{}: {}", p.at, p.message)).collect();
    let input = std::fs::read(&manifest.input).unwrap_or_default();
    let code =
        Code { base: manifest.mapping.base, bytes: input.get(manifest.mapping.file_offset..).unwrap_or_default() };
    let mut functions = Vec::new();
    let mut globals = Vec::new();
    match Inventory::load(root) {
        // Bound by content: a name applies where the code is the code it names.
        Some(mut inv) => {
            for f in &names.functions {
                match inv.function(&code, f.address, f.module.as_deref()) {
                    Bind::Apply => functions.push(f),
                    Bind::Elsewhere => {}
                    Bind::Skip(why) => problems.push(format!("{}: {}: {why}", f.at, f.name.text)),
                }
            }
            for g in &names.globals {
                if g.module.as_deref().is_none_or(|m| inv.holds(&code, m)) {
                    globals.push(g);
                }
            }
        }
        // No inventory: a module is the analysed executable's file name.
        None => {
            let module = manifest.exe.as_ref().and_then(|_| manifest.source.rsplit(['/', '\\']).next());
            let applies = |m: &Option<String>| match m {
                None => true,
                Some(m) => module.is_some_and(|x| x.eq_ignore_ascii_case(m)),
            };
            globals = names.globals.iter().filter(|g| applies(&g.module)).collect();
            functions = names.functions.iter().filter(|f| applies(&f.module)).collect();
        }
    }
    if names.types.is_empty() && globals.is_empty() && functions.is_empty() {
        return (Value::Null, problems);
    }
    let types: Vec<Value> = names
        .types
        .iter()
        .map(|t| {
            let mut v = json!(t);
            if matches!(t.body, TypeBody::Struct { .. } | TypeBody::Union { .. }) {
                v["size"] = json!(names.size_of(&Type::Named { name: t.name.text.clone() }).unwrap_or(0));
            }
            v
        })
        .collect();
    (json!({"types": types, "globals": globals, "functions": functions}), problems)
}

/// The names marked as guesses (`name?`) anywhere in the names JSON.
fn guessed(v: &Value, out: &mut Vec<String>) {
    match v {
        Value::Object(o) => {
            if let (Some(t), Some(true)) =
                (o.get("text").and_then(Value::as_str), o.get("guess").and_then(Value::as_bool))
            {
                out.push(t.to_owned());
            }
            o.values().for_each(|x| guessed(x, out));
        }
        Value::Array(a) => a.iter().for_each(|x| guessed(x, out)),
        _ => {}
    }
}

/// Ghidra prints a guess's `?` as `_`; puts the mark back on each whole
/// word `name_` for a guessed name.
fn mark_guesses(code: &str, guesses: &[String]) -> String {
    let ident = |c: char| c.is_ascii_alphanumeric() || c == '_';
    let mut out = String::with_capacity(code.len());
    let mut rest = code;
    while let Some(start) = rest.find(|c: char| ident(c)) {
        out.push_str(&rest[..start]);
        rest = &rest[start..];
        let end = rest.find(|c: char| !ident(c)).unwrap_or(rest.len());
        let word = &rest[..end];
        match word.strip_suffix('_').filter(|w| guesses.iter().any(|g| g == w)) {
            Some(w) => {
                out.push_str(w);
                out.push('?');
            }
            None => out.push_str(word),
        }
        rest = &rest[end..];
    }
    out.push_str(rest);
    out
}

/// Explains pseudocode that reads registers no path in the function sets
/// (Ghidra's unaff_ and in_ names): a start inside a function, or code
/// that reads such values on purpose.
fn unset_registers(a: &Analysis, pseudocode: &str, entry: Option<u32>) -> Option<String> {
    const REGISTERS: &[&str] = &[
        "zero", "at", "v0", "v1", "a0", "a1", "a2", "a3", "t0", "t1", "t2", "t3", "t4", "t5", "t6", "t7", "t8", "t9",
        "s0", "s1", "s2", "s3", "s4", "s5", "s6", "s7", "s8", "fp", "k0", "k1", "gp", "retaddr",
    ];
    let mut names: Vec<&str> = pseudocode
        .split(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
        .filter(|w| w.strip_prefix("unaff_").or_else(|| w.strip_prefix("in_")).is_some_and(|r| REGISTERS.contains(&r)))
        .collect();
    names.sort_unstable();
    names.dedup();
    if names.is_empty() {
        return None;
    }
    let list = names.join(", ");
    let payload = fs::read(a.manifest.index.parent()?.join("payload.bin")).ok()?;
    let entry = entry?;
    Some(if psx_symbols::build::follows_function_end(&payload, a.manifest.mapping.base, entry) {
        format!(
            "Reads registers it never sets ({list}): the original reads values no path sets, or was written by hand to its own register use; code disasm shows the instructions."
        )
    } else {
        format!(
            "Reads registers it never sets ({list}), and 0x{entry:08X} does not follow the end of a function: it probably starts earlier, inside the code before it (with ram, seed an earlier entry)."
        )
    })
}

fn copy_dir(source: &Path, target: &Path) -> Result<()> {
    fs::create_dir_all(target).map_err(|e| Error::io(target, e))?;
    for entry in fs::read_dir(source).map_err(|e| Error::io(source, e))? {
        let entry = entry?;
        let dest = target.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_dir(&entry.path(), &dest)?;
        } else {
            fs::copy(entry.path(), dest)?;
        }
    }
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn guesses_get_their_mark_back() {
        let names = json!({"functions": [{"name": {"text": "sqr12", "guess": true}}, {"name": {"text": "rsin"}}]});
        let mut g = Vec::new();
        guessed(&names, &mut g);
        assert_eq!(g, ["sqr12"]);
        assert_eq!(
            mark_guesses("x = sqr12_(a) + my_sqr12_(b) + sqr12__;", &g),
            "x = sqr12?(a) + my_sqr12_(b) + sqr12__;"
        );
    }

    fn context() -> Ctx {
        let root = std::env::temp_dir().join(format!("psx-ghidra-unit-{}", crate::stamp()));
        fs::create_dir_all(&root).unwrap();
        Ctx { root, cancel: None }
    }
    fn input(source: Option<String>) -> Input {
        Input {
            source,
            analysis: None,
            ram: None,
            ranges: vec![],
            entries: vec![],
            gp: None,
            symbols: vec![],
            ghidra_home: Some(PathBuf::from("missing-ghidra")),
            timeout: 300,
            reanalyze: false,
        }
    }

    #[test]
    fn input_validation_precedes_worker_launch() {
        let ctx = context();
        let input = |source| FunctionsArgs { input: input(source), filter: None, offset: 0, limit: limit() };
        let missing = input(None).exec(&ctx);
        assert_eq!(missing.value["error"]["code"], "invalid_arguments");
        fs::write(ctx.root.join("short.exe"), b"PS-X EXE").unwrap();
        let malformed = input(Some(ctx.root.join("short.exe").display().to_string())).exec(&ctx);
        assert!(malformed.value.get("error").is_some());
        let mut exe = vec![0u8; 0x800];
        exe[..8].copy_from_slice(b"PS-X EXE");
        for (off, value) in [(0x10, 0x8001_0000u32), (0x18, 0x8001_0000), (0x1C, 0x800)] {
            exe[off..off + 4].copy_from_slice(&value.to_le_bytes());
        }
        fs::write(ctx.root.join("truncated.exe"), &exe).unwrap();
        let truncated = input(Some(ctx.root.join("truncated.exe").display().to_string())).exec(&ctx);
        assert_eq!(truncated.value["error"]["code"], "invalid_arguments");
        assert!(truncated.value["error"]["message"].as_str().unwrap().contains("truncated"));
        assert!(!ctx.root.join(".psx").exists(), "invalid EXEs must not create projects");
        fs::remove_dir_all(ctx.root).unwrap();
    }

    #[test]
    fn runtime_ranges_entries_and_metadata_require_evidence() {
        let ctx = context();
        let path = ctx.root.join("ram.bin");
        fs::write(&path, vec![0; 0x200000]).unwrap();
        let mut a = input(None);
        a.ram = Some(path.clone());
        assert!(import(&a).is_err());
        a.ranges = vec!["A0010000-A0010100".into()];
        a.entries = vec!["80010004".into()];
        a.gp = Some("80018000".into());
        let imported = import(&a).unwrap();
        assert_eq!(imported.mapping.code_ranges, vec![(0x80010000, 0x80010100)]);
        assert_eq!(imported.mapping.gp, 0x80018000);
        a.entries = vec!["1f801000".into()];
        assert!(import(&a).is_err());
        a.entries = vec!["80010004".into()];
        a.ranges = vec!["801ffff0-80200000".into()];
        a.entries = vec!["801ffff0".into()];
        assert!(import(&a).is_ok());
        a.ranges = vec!["801ffff0-80200004".into()];
        assert!(import(&a).is_err());
        a.ranges = vec!["80010000-80010100".into(), "80010080-80010200".into()];
        assert!(import(&a).is_err());
        a.ranges = vec!["80010000-80010100".into()];
        a.entries = vec!["80010004".into()];
        a.ram = Some(ctx.root.clone());
        fs::write(ctx.root.join("report.json"), br#"{"registers":{"gp":2147549184},"frames":{"end":123}}"#).unwrap();
        a.gp = None;
        let imported = import(&a).unwrap();
        assert_eq!(imported.mapping.gp, 0);
        assert!(imported.provenance.get("frames").is_none());
        save_json(
            &ctx.root.join("report.json"),
            &json!({"files":{"ram.bin":{"sha256":sha256_bytes(&vec![0;0x200000])}},"registers":{"gp":0x80018000u32},"frames":{"end":123}}),
        )
        .unwrap();
        let imported = import(&a).unwrap();
        assert_eq!(imported.mapping.gp, 0x80018000);
        assert_eq!(imported.provenance["frames"]["end"], 123);
        fs::remove_dir_all(ctx.root).unwrap();
    }

    #[test]
    fn aliases_and_paging_are_bounded() {
        assert_eq!(ram_address(0xA061_2340).unwrap(), 0x8001_2340);
        assert!(ram_address(0x1F80_1810).is_err());
        assert!(ram_address(0xBFC0_0000).is_err());
        assert!(same_address(&json!("0xA0012340"), 0x8001_2340));
        assert!(!same_address(&json!("0x1F801810"), 0x8000_1810));
        let rows = vec![json!(1), json!(2), json!(3)];
        assert_eq!(
            page(&rows, 1, 1),
            json!({"total":3,"offset":1,"returned":1,"limit":1,"truncated":true,"complete":false,"items":[2],"next_offset":2})
        );
        assert_eq!(page(&rows, usize::MAX, 1)["items"], json!([]));
    }

    #[test]
    fn project_locks_are_exclusive_and_edits_are_detected() {
        let ctx = context();
        let first = Lock::acquire(&ctx.root).unwrap();
        assert_eq!(Lock::acquire(&ctx.root).err().unwrap().code, "analysis_busy");
        drop(first);
        // Queued: a fork in another test thread holds the lock's descriptor until it execs.
        let _next = Lock::queued(&ctx.root, Instant::now() + Duration::from_secs(2), &ctx).unwrap();
        let before = fingerprint(&ctx.root).unwrap();
        fs::write(ctx.root.join("database"), "modified").unwrap();
        assert_ne!(before, fingerprint(&ctx.root).unwrap());
        fs::write(ctx.root.join("project.lock"), "GUI owner").unwrap();
        assert!(gui_lock(&ctx.root));
        fs::remove_dir_all(&ctx.root).unwrap();
    }

    #[test]
    fn queued_project_owners_wait_cancel_and_respect_deadlines() {
        let ctx = context();
        let first = Lock::acquire(&ctx.root).unwrap();
        let root = ctx.root.clone();
        let (tx, rx) = std::sync::mpsc::channel();
        let waiter = std::thread::spawn(move || {
            let ctx = Ctx { root, cancel: None };
            let next = Lock::queued(&ctx.root, Instant::now() + Duration::from_secs(3), &ctx).unwrap();
            tx.send(()).unwrap();
            drop(next);
        });
        assert!(rx.recv_timeout(Duration::from_millis(100)).is_err(), "waiter must not enter while owned");
        // Another analysis remains independently usable.
        let other = ctx.root.join("other");
        fs::create_dir(&other).unwrap();
        drop(Lock::queued(&other, Instant::now() + Duration::from_secs(1), &ctx).unwrap());
        drop(first);
        rx.recv_timeout(Duration::from_secs(2)).unwrap();
        waiter.join().unwrap();
        let held = Lock::acquire(&ctx.root).unwrap();
        let error = Lock::queued(&ctx.root, Instant::now() + Duration::from_millis(75), &ctx).err().unwrap();
        assert_eq!(error.code, "timeout");
        assert_eq!(Lock::acquire(&ctx.root).err().unwrap().code, "analysis_busy", "a timeout must preserve the owner");
        let token = tokio_util::sync::CancellationToken::new();
        let cancel = token.clone();
        let thread = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(50));
            cancel.cancel();
        });
        let waiting = Ctx { root: ctx.root.clone(), cancel: Some(token) };
        assert_eq!(
            Lock::queued(&ctx.root, Instant::now() + Duration::from_secs(2), &waiting).err().unwrap().code,
            "cancelled"
        );
        thread.join().unwrap();
        assert_eq!(Lock::acquire(&ctx.root).err().unwrap().code, "analysis_busy");
        drop(held);
        // A lock file whose owner is gone (an older psx's, or one killed) holds nothing.
        fs::write(ctx.root.join("psx.lock"), "psx-query-lock-v1\npid=1\n").unwrap();
        drop(Lock::queued(&ctx.root, Instant::now() + Duration::from_secs(2), &ctx).unwrap());
        fs::remove_dir_all(ctx.root).unwrap();
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn an_interrupted_query_is_recovered_by_the_next_owner() {
        use std::os::unix::process::CommandExt;
        let ctx = context();
        let project = ctx.root.join("project-dir");
        fs::create_dir(&project).unwrap();
        // A query's psx was killed: its worker still runs and holds Ghidra's lock.
        let mut worker = Command::new("sh");
        worker.args(["-c", "sleep 60", "psx-worker-test"]).arg(&project).process_group(0);
        let mut worker = worker.spawn().unwrap();
        fs::write(project.join("project.lock"), "worker").unwrap();
        {
            let mut killed = Lock::acquire(&ctx.root).unwrap();
            killed.worker(Some((worker.id(), &project))).unwrap();
        }
        let mut next = Lock::queued(&ctx.root, Instant::now() + Duration::from_secs(2), &ctx).unwrap();
        let (on, done) = next.recover().unwrap().unwrap();
        assert_eq!(on, project);
        assert_eq!(done["worker_stopped"], true);
        assert!(worker.try_wait().unwrap().is_some() || worker.wait().is_ok());
        assert!(!gui_lock(&project), "the dead worker's Ghidra lock is removed");
        drop(next);
        let again = Lock::queued(&ctx.root, Instant::now() + Duration::from_secs(2), &ctx).unwrap();
        assert!(again.interrupted.is_none(), "recovered once");
        fs::remove_dir_all(ctx.root).unwrap();
    }

    #[test]
    fn settings_dir_follows_ghidras_user_home_rule() {
        // Ghidra keeps the plain settings name when the XDG parent is inside
        // the user's home (GT2's project is), and prefixes it with the user
        // when it is not (ApplicationUtilities.getUserSpecificDirName).
        let root = std::env::temp_dir().join(format!("psx-ghidra-settings-{}", crate::stamp()));
        let attempt = root.join("home/project/.psx/ghidra/key/stamp");
        fs::create_dir_all(attempt.join("config")).unwrap();
        let home = root.join("home");
        let versioned = "ghidra_12.1.4_PUBLIC";
        assert_eq!(
            ghidra_settings_dir(&attempt, Some(&home), "maisy", versioned),
            attempt.join("config/ghidra").join(versioned),
            "inside the home the name is plain"
        );
        assert_eq!(
            ghidra_settings_dir(&attempt, Some(&root.join("elsewhere")), "maisy", versioned),
            attempt.join("config/maisy-ghidra").join(versioned),
            "outside the home the name carries the user"
        );
        assert_eq!(
            ghidra_settings_dir(&attempt, None, "maisy", versioned),
            attempt.join("config/maisy-ghidra").join(versioned),
            "no HOME, no plain name"
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn cancellation_and_timeout_stop_worker_descendants() {
        for cancelled in [false, true] {
            let mut ctx = context();
            let token = tokio_util::sync::CancellationToken::new();
            ctx.cancel = Some(token.clone());
            let pid_file = ctx.root.join("child.pid");
            let cancel_file = pid_file.clone();
            let cancel = std::thread::spawn(move || {
                for _ in 0..100 {
                    if cancel_file.exists() {
                        if cancelled {
                            token.cancel();
                        }
                        return;
                    }
                    std::thread::sleep(Duration::from_millis(10));
                }
            });
            let mut cmd = Command::new("sh");
            cmd.args(["-c", "sleep 60 & echo $! > \"$1\"; wait", "psx-worker-test"]).arg(&pid_file);
            let failure = run_worker(
                &mut cmd,
                &ctx.root.join("worker.log"),
                Instant::now() + Duration::from_millis(500),
                &ctx,
                None,
            )
            .unwrap_err();
            cancel.join().unwrap();
            assert_eq!(failure.code, if cancelled { "cancelled" } else { "timeout" });
            let pid = fs::read_to_string(pid_file).unwrap();
            let mut state = fs::read_to_string(format!("/proc/{}/stat", pid.trim()));
            // SIGKILL delivery to a grandchild can lag reaping the launcher.
            for _ in 0..100 {
                if state.as_ref().map_or(true, |s| s.split_whitespace().nth(2) == Some("Z")) {
                    break;
                }
                std::thread::sleep(Duration::from_millis(10));
                state = fs::read_to_string(format!("/proc/{}/stat", pid.trim()));
            }
            assert!(
                state.as_ref().map_or(true, |s| s.split_whitespace().nth(2) == Some("Z")),
                "descendant still running: {state:?}"
            );
            fs::remove_dir_all(ctx.root).unwrap();
        }
    }
}
