//! Optional, out-of-process Ghidra analysis. An ordinary query imports and
//! analyses its source automatically; manifests and query results are reusable.
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use clap::{Args, Subcommand};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::commands::{Ctx, Op, Outcome};
use psx_media::{PsxExe, disc::sha256_bytes};

const SCRIPT: &str = include_str!("../../../tools/ghidra/PsxGhidra.java");
const LANGUAGE: &str = "MIPS:LE:32:default";
const FORMAT: u32 = 2;
const FILE_LIMIT: u64 = 64 * 1024 * 1024;
type Result<T> = std::result::Result<T, Failure>;

#[derive(Debug)]
struct Failure {
    code: &'static str,
    message: String,
}
fn fail(code: &'static str, message: impl ToString) -> Failure {
    Failure { code, message: message.to_string() }
}
fn io(e: impl ToString) -> Failure {
    fail("io", e)
}
fn invalid(e: impl ToString) -> Failure {
    fail("invalid_arguments", e)
}
fn outcome(result: Result<Value>) -> Outcome {
    match result {
        Ok(value) => Outcome { exit: u8::from(value["status"] == "partial"), value, pretty: true, text: false },
        Err(e) => {
            Outcome { value: json!({"error":{"code":e.code,"message":e.message}}), exit: 1, pretty: true, text: false }
        }
    }
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

#[derive(Subcommand)]
pub enum Action {
    /// Check the optional Ghidra installation and Java runtime.
    Doctor(DoctorArgs),
    /// Create/reuse an analysis (optional: query commands do this automatically).
    Analyze(Input),
    /// List analysed functions, with optional name filtering and pagination.
    Functions(FunctionsArgs),
    /// Return a function's pseudocode and callers, automatically importing its source.
    Decompile(DecompileArgs),
    /// Query Ghidra's references to/from an address (code or data).
    Xrefs(XrefsArgs),
    /// Apply explicit function renames and comments from a JSON patch.
    Annotate(AnnotateArgs),
    /// Export byte-bound function names for runtime and static tools.
    ExportSymbols(ExportArgs),
}
impl Action {
    pub fn exec(self, ctx: &Ctx) -> Outcome {
        match self {
            Self::Doctor(a) => a.exec(ctx),
            Self::Analyze(a) => a.exec(ctx),
            Self::Functions(a) => a.exec(ctx),
            Self::Decompile(a) => a.exec(ctx),
            Self::Xrefs(a) => a.exec(ctx),
            Self::Annotate(a) => a.exec(ctx),
            Self::ExportSymbols(a) => a.exec(ctx),
        }
    }
}

#[derive(Args, Deserialize, JsonSchema)]
pub struct DoctorArgs {
    /// Ghidra installation directory. Else PSX_GHIDRA_HOME, PATH, or common install locations.
    #[arg(long)]
    pub ghidra_home: Option<PathBuf>,
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
#[derive(Args, Deserialize, JsonSchema)]
pub struct XrefsArgs {
    #[command(flatten)]
    #[serde(flatten)]
    pub input: Input,
    /// Hex address, including a RAM data address.
    #[arg(long)]
    pub addr: String,
    #[arg(long, default_value_t = 0)]
    #[serde(default)]
    pub offset: usize,
    /// Maximum rows in each reference direction (1..=1000).
    #[arg(long, default_value_t = limit())]
    #[serde(default = "limit")]
    pub limit: usize,
}

#[derive(Args, Deserialize, JsonSchema)]
pub struct AnnotateArgs {
    /// Existing analysis manifest.
    pub analysis: PathBuf,
    /// JSON {"functions":[{"addr":"80010000","name":"writer","comment":"Evidence..."}]}.
    #[arg(long)]
    pub file: PathBuf,
    #[arg(long)]
    pub ghidra_home: Option<PathBuf>,
    #[arg(long, default_value_t = timeout())]
    #[serde(default = "timeout")]
    pub timeout: u64,
}
#[derive(Args, Deserialize, JsonSchema)]
pub struct ExportArgs {
    /// Existing analysis manifest.
    pub analysis: PathBuf,
    /// Output .symbols.json; defaults to an immutable revision-specific analysis artifact.
    #[arg(long)]
    pub out: Option<PathBuf>,
    /// Total deadline including time queued for this analysis (1..=3600 seconds).
    #[arg(long, default_value_t=timeout())]
    #[serde(default = "timeout")]
    pub timeout: u64,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Patch {
    functions: Vec<FunctionPatch>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FunctionPatch {
    addr: String,
    name: Option<String>,
    comment: Option<String>,
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
        let home = p.canonicalize().map_err(|e| fail("ghidra_unavailable", format!("{}: {e}", p.display())))?;
        let launcher = home.join("support/analyzeHeadless");
        let props = fs::read_to_string(home.join("Ghidra/application.properties"))
            .map_err(|e| fail("ghidra_unavailable", format!("{} is not a Ghidra installation: {e}", home.display())))?;
        let version =
            props.lines().find_map(|s| s.strip_prefix("application.version=")).unwrap_or("unknown").to_string();
        if !launcher.is_file() {
            return Err(fail("ghidra_unavailable", format!("missing {}", launcher.display())));
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
        fail(
            "ghidra_unavailable",
            "Install Ghidra and set PSX_GHIDRA_HOME to its directory (or pass --ghidra-home). Other psx commands do not require Ghidra.",
        )
    })
}

fn read_json(path: &Path) -> Result<Value> {
    if fs::metadata(path).map_err(io)?.len() > FILE_LIMIT {
        return Err(fail("limit_exceeded", format!("{} exceeds 64 MiB", path.display())));
    }
    serde_json::from_slice(&fs::read(path).map_err(io)?)
        .map_err(|e| fail("invalid_artifact", format!("{}: {e}", path.display())))
}
fn save_json(path: &Path, value: &impl Serialize) -> Result<()> {
    let tmp = path.with_extension(format!("tmp-{}", crate::instance_suffix()));
    fs::write(&tmp, serde_json::to_vec_pretty(value).map_err(io)?).map_err(io)?;
    fs::rename(tmp, path).map_err(io)
}
fn stamp() -> String {
    format!(
        "{}-{}{}",
        SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis(),
        std::process::id(),
        crate::instance_suffix()
    )
}
fn hex(s: &str) -> Result<u32> {
    psx_media::memmap::parse_hex(s).ok_or_else(|| invalid(format!("invalid hex address {s:?}")))
}
fn ram_address(addr: u32) -> Result<u32> {
    let region = psx_media::memmap::resolve(addr);
    if region.region != psx_media::memmap::Region::MainRam {
        return Err(invalid(format!("0x{addr:08X} is not a main-RAM address")));
    }
    Ok(0x8000_0000 | region.offset)
}
fn budget(input: &Input) -> Result<Instant> {
    if !(1..=3600).contains(&input.timeout) {
        return Err(invalid("timeout must be 1..=3600 seconds"));
    }
    if [input.source.is_some(), input.analysis.is_some(), input.ram.is_some()].into_iter().filter(|v| *v).count() != 1 {
        return Err(invalid("give exactly one of source, analysis or ram"));
    }
    if input.ram.is_none() && (!input.ranges.is_empty() || !input.entries.is_empty() || input.gp.is_some()) {
        return Err(invalid("range, entry and gp require ram"));
    }
    if input.analysis.is_some() && (!input.symbols.is_empty() || input.reanalyze) {
        return Err(invalid("symbols/reanalyze require source or ram, not an analysis handle"));
    }
    Ok(Instant::now() + Duration::from_secs(input.timeout))
}

/// One owner per project, including cached readers. Never remove a lock left by
/// another process; interrupted locks require explicit inspection by the user.
struct Lock(PathBuf);
impl Lock {
    fn acquire(dir: &Path) -> Result<Self> {
        let path = dir.join("psx.lock");
        // Publish a fully written ownership marker atomically: a concurrent reader
        // must never mistake an empty, newly created lock for a foreign lock.
        let temp = dir.join(format!("lock-{}.tmp", stamp()));
        let mut f = OpenOptions::new().write(true).create_new(true).open(&temp).map_err(io)?;
        let staged = Self(temp);
        writeln!(f, "psx-query-lock-v1\npid={}", std::process::id()).map_err(io)?;
        match fs::hard_link(&staged.0, &path) {
            Ok(()) => Ok(Self(path)),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Err(fail(
                "analysis_busy",
                format!(
                    "{} is owned by another query; inspect interrupted owners before removing locks",
                    path.display()
                ),
            )),
            Err(e) => Err(io(e)),
        }
    }
    fn queued(dir: &Path, deadline: Instant, ctx: &Ctx) -> Result<Self> {
        loop {
            if ctx.cancel.as_ref().is_some_and(|c| c.is_cancelled()) {
                return Err(fail("cancelled", "Cancelled while queued for this analysis"));
            }
            if Instant::now() >= deadline {
                return Err(fail(
                    "timeout",
                    "Deadline reached while queued for this analysis; an interrupted owner may need inspection",
                ));
            }
            match Self::acquire(dir) {
                Ok(lock) => return Ok(lock),
                Err(e) if e.code == "analysis_busy" => {
                    // Only psx-owned locks participate in the queue. Foreign locks
                    // (and Ghidra GUI locks checked by cached()) remain errors.
                    let mut b = String::new();
                    match File::open(dir.join("psx.lock")) {
                        Ok(f) => {
                            use std::io::Read;
                            f.take(128).read_to_string(&mut b).map_err(io)?;
                        }
                        Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
                        Err(e) => return Err(io(e)),
                    }
                    if !b.starts_with("psx-query-lock-v1\n") {
                        return Err(e);
                    }
                    std::thread::sleep(Duration::from_millis(25));
                }
                Err(e) => return Err(e),
            }
        }
    }
}
impl Drop for Lock {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

/// Uses a separate Unix process group so timeout/cancellation kills Java and
/// its native decompiler, not just the shell launcher. No shell interpolation.
fn run_worker(command: &mut Command, log: &Path, deadline: Instant, ctx: &Ctx) -> Result<()> {
    if cfg!(not(unix)) {
        return Err(fail("unsupported_platform", "Ghidra worker supervision currently requires Unix."));
    }
    if ctx.cancel.as_ref().is_some_and(|c| c.is_cancelled()) {
        return Err(fail("cancelled", "Ghidra query cancelled before worker launch"));
    }
    if Instant::now() >= deadline {
        return Err(fail("timeout", "Ghidra query deadline expired before worker launch"));
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    let output = File::create(log).map_err(io)?;
    command.stdout(output.try_clone().map_err(io)?).stderr(output).stdin(Stdio::null());
    let mut child = command.spawn().map_err(|e| fail("worker_unavailable", format!("{e}; log: {}", log.display())))?;
    loop {
        let cancelled = ctx.cancel.as_ref().is_some_and(|c| c.is_cancelled());
        if cancelled || Instant::now() >= deadline {
            #[cfg(unix)]
            {
                let _ = Command::new("kill").args(["-KILL", "--", &format!("-{}", child.id())]).status();
            }
            let _ = child.kill();
            let _ = child.wait();
            return Err(fail(
                if cancelled { "cancelled" } else { "timeout" },
                format!("Ghidra worker stopped; interrupted analysis is not reused. Log: {}", log.display()),
            ));
        }
        match child.try_wait().map_err(io)? {
            Some(status) if status.success() => return Ok(()),
            Some(status) => {
                return Err(fail("ghidra_failed", format!("Ghidra exited {status}; inspect {}", log.display())));
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
    annotations: Vec<Value>,
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
        let snap = crate::ram::load(path).map_err(|e| fail(e.code(), e))?;
        let mut ranges = Vec::new();
        for spec in &input.ranges {
            let (lo, hi) = spec.split_once('-').ok_or_else(|| invalid("range must be LO-HI (end exclusive)"))?;
            let lo = ram_address(hex(lo)?)?;
            let raw_hi = hex(hi)?;
            let hi = ram_address(raw_hi.checked_sub(1).ok_or_else(|| invalid("invalid range end"))?)? + 1;
            if lo >= hi || lo & 3 != 0 || hi & 3 != 0 {
                return Err(invalid("empty, wrapping or unaligned code range"));
            }
            ranges.push((lo, hi));
        }
        ranges.sort_unstable();
        if ranges.is_empty() || ranges.windows(2).any(|r| r[0].1 > r[1].0) {
            return Err(invalid("RAM requires non-overlapping candidate code ranges"));
        }
        let mut entries = input.entries.iter().map(|s| ram_address(hex(s)?)).collect::<Result<Vec<_>>>()?;
        entries.sort_unstable();
        entries.dedup();
        if entries.is_empty()
            || entries.iter().any(|e| e & 3 != 0 || !ranges.iter().any(|&(lo, hi)| lo <= *e && *e < hi))
        {
            return Err(invalid("RAM requires aligned entry seeds within candidate ranges"));
        }
        let report_path = path.join("report.json");
        let mut provenance = json!({"kind":"ram","ram_path":snap.path.canonicalize().map_err(io)?});
        let mut gp = 0;
        let mut warnings =
            vec!["Snapshot bytes prove residency only at capture time; final RAM cannot establish earlier overlay residency.".into()];
        if path.is_dir() && report_path.is_file() {
            let report = read_json(&report_path)?;
            // Bind metadata only when the report itself identifies these captured bytes.
            let hash = sha256_bytes(&snap.bytes);
            if report["files"]["ram.bin"]["sha256"] == hash {
                provenance["report"] = json!(report_path.canonicalize().map_err(io)?);
                provenance["report_sha256"] = json!(sha256_bytes(&fs::read(&report_path).map_err(io)?));
                provenance["frames"] = report["frames"].clone();
                provenance["identity"] = report["identity"].clone();
                provenance["registers"] = report["registers"].clone();
                for file in ["cd_native.jsonl", "cd.jsonl", "bios.jsonl", "loads.json"] {
                    let p = path.join(file);
                    if p.is_file() {
                        provenance[file] = json!({"path":p.canonicalize().map_err(io)?,"sha256":sha256_bytes(&fs::read(p).map_err(io)?)});
                    }
                }
                gp = report["registers"]["gp"].as_u64().and_then(|v| u32::try_from(v).ok()).unwrap_or(0);
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
    let source = input.source.as_ref().ok_or_else(|| invalid("missing source"))?;
    let bytes = psx_media::source::read(source).map_err(|e| fail(e.code(), e))?;
    let exe = PsxExe::parse(&bytes).map_err(|e| fail(e.code(), e))?;
    let base = ram_address(exe.text_addr)?;
    let end = base
        .checked_add(exe.text_size)
        .filter(|&end| end <= 0x8020_0000)
        .ok_or_else(|| invalid("EXE payload exceeds main RAM"))?;
    let entry = ram_address(exe.pc)?;
    if exe.text_size == 0
        || exe.payload(&bytes).len() != exe.text_size as usize
        || !(base..end).contains(&entry)
        || entry & 3 != 0
        || base & 3 != 0
    {
        return Err(invalid("EXE has a truncated/empty payload or an invalid entry address"));
    }
    let bss = if exe.bss_size > 0 {
        let start = ram_address(exe.bss_addr)?;
        let stop = start
            .checked_add(exe.bss_size)
            .filter(|&n| n <= 0x8020_0000)
            .ok_or_else(|| invalid("BSS exceeds main RAM"))?;
        if start < end && stop > base {
            return Err(invalid("BSS overlaps the loaded payload"));
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
        gp: exe.gp,
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
    _lock: Lock,
}

fn fingerprint(dir: &Path) -> Result<String> {
    fn walk(dir: &Path, out: &mut Vec<String>) -> Result<()> {
        let mut entries = fs::read_dir(dir).map_err(io)?.collect::<std::result::Result<Vec<_>, _>>().map_err(io)?;
        entries.sort_by_key(|e| e.file_name());
        for e in entries {
            let name = e.file_name().to_string_lossy().into_owned();
            let m = e.metadata().map_err(io)?;
            if m.is_dir() {
                out.push(name);
                walk(&e.path(), out)?;
            } else if !name.ends_with(".lock") && !name.ends_with(".lock~") {
                out.push(format!(
                    "{name}:{}:{}",
                    m.len(),
                    m.modified().map_err(io)?.duration_since(UNIX_EPOCH).unwrap_or_default().as_nanos()
                ));
            }
        }
        Ok(())
    }
    let mut rows = Vec::new();
    walk(dir, &mut rows)?;
    Ok(sha256_bytes(rows.join("\n").as_bytes()))
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
fn command(install: &Installation, attempt: &Path) -> Result<Command> {
    let mut cmd = Command::new(&install.launcher);
    // Keep Ghidra's generated configuration and compiled scripts in the project,
    // including when psx is used in a filesystem sandbox.
    for (key, dir) in [("XDG_CONFIG_HOME", "config"), ("XDG_CACHE_HOME", "cache")] {
        let p = attempt.join(dir);
        fs::create_dir_all(&p).map_err(io)?;
        cmd.env(key, p);
    }
    cmd.env("GHIDRA_HEADLESS_MAXMEM", "2G");
    // PID namespaces can share /tmp/hsperfdata_* while reusing JVM PIDs. A
    // perf-data warning on stdout corrupts Ghidra LaunchSupport's Java-home probe.
    cmd.env(
        "JAVA_TOOL_OPTIONS",
        format!("{} -XX:-UsePerfData", std::env::var("JAVA_TOOL_OPTIONS").unwrap_or_default()),
    );
    Ok(cmd)
}
fn script_dir(dir: &Path) -> Result<PathBuf> {
    let p = dir.join("scripts");
    fs::create_dir_all(&p).map_err(io)?;
    let file = p.join("PsxGhidra.java");
    if fs::read_to_string(&file).ok().as_deref() != Some(SCRIPT) {
        fs::write(file, SCRIPT).map_err(io)?;
    }
    Ok(p)
}
fn cached(path: PathBuf, lock: Lock) -> Result<Analysis> {
    let value = read_json(&path)?;
    if value["format"] != FORMAT {
        return Err(fail(
            "stale_analysis",
            "Analysis format changed; query source or ram again to create a compatible analysis.",
        ));
    }
    let manifest: Manifest = serde_json::from_value(value).map_err(io)?;
    if manifest.format != FORMAT || manifest.script_sha256 != sha256_bytes(SCRIPT.as_bytes()) {
        return Err(fail(
            "stale_analysis",
            "Analysis format/scripts changed; query the source again to create a compatible analysis.",
        ));
    }
    if gui_lock(&manifest.project_dir) {
        return Err(fail("analysis_busy", "Close this analysis in the Ghidra GUI before querying it."));
    }
    if fingerprint(&manifest.project_dir)? != manifest.project_fingerprint {
        return Err(fail(
            "stale_analysis",
            "Ghidra project changed outside psx; cached results are no longer valid. Query source with reanalyze=true for a fresh import; the edited project is preserved.",
        ));
    }
    let bytes = fs::read(&manifest.input).map_err(io)?;
    if sha256_bytes(&bytes) != manifest.source_sha256 {
        return Err(fail("invalid_artifact", "Analysis input copy has changed."));
    }
    let index = read_json(&manifest.index)?;
    Ok(Analysis { path, manifest, index, fresh_query: None, _lock: lock })
}

/// Names to apply before analysis, from `.symbols.json` reports (checked
/// against the input's bytes) or plain `.sym` files (unverified, with a warning).
fn analysis_symbols(input: &Input, bytes: &[u8], mapping: &Mapping, warnings: &mut Vec<String>) -> Result<Vec<Value>> {
    let (base, end) = (mapping.base, mapping.base + mapping.size);
    let mut symbols = Vec::new();
    for path in &input.symbols {
        let text = fs::read_to_string(path).map_err(io)?;
        if path.to_string_lossy().ends_with(".symbols.json") {
            let value: Value = serde_json::from_str(&text).map_err(io)?;
            if value["format"] == 2 {
                let doc = serde_json::from_value(value).map_err(io)?;
                let bound = crate::bound_symbols::Loaded::load(doc, path.parent().unwrap_or(Path::new(".")))
                    .map_err(invalid)?;
                let image = &bytes[mapping.file_offset..mapping.file_offset + mapping.size as usize];
                for f in &bound.document.functions {
                    if bound.describe(f.address, base, image).is_none() {
                        return Err(invalid("bound symbol function differs from analysis input"));
                    }
                    symbols.push(json!({"address":f.address,"name":f.name,"kind":"function"}));
                }
                continue;
            }
            let original = psx_media::source::read(
                value["source"].as_str().ok_or_else(|| invalid("symbols report lacks source"))?,
            )
            .map_err(|e| fail(e.code(), e))?;
            let original_exe = PsxExe::parse(&original).map_err(|e| fail(e.code(), e))?;
            let original_base = ram_address(original_exe.text_addr)?;
            let payload = original_exe.payload(&original);
            let offset =
                original_base.checked_sub(base).ok_or_else(|| invalid("symbol EXE is outside mapping"))? as usize;
            let mapped = &bytes[mapping.file_offset..mapping.file_offset + mapping.size as usize];
            if payload.len() != original_exe.text_size as usize
                || mapped.get(offset..offset + payload.len()) != Some(payload)
            {
                return Err(invalid(format!("{} describes code that differs from analysis input", path.display())));
            }
            if value["exe_sha256"].as_str().is_some_and(|s| s != sha256_bytes(&original)) {
                return Err(invalid(format!("{} is stale: its source bytes have changed", path.display())));
            }
            let list =
                value["report"]["symbols"].as_array().ok_or_else(|| invalid("symbols report lacks report.symbols"))?;
            for s in list {
                let a = s["address"]
                    .as_u64()
                    .and_then(|a| u32::try_from(a).ok())
                    .ok_or_else(|| invalid("invalid symbol address"))?;
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
            return Err(invalid("symbol address outside payload or unaligned"));
        }
        if s["name"].as_str().is_none_or(str::is_empty)
            || !matches!(s["kind"].as_str(), Some("function" | "label" | "local"))
        {
            return Err(invalid("symbols need a nonempty name and kind function, label or local"));
        }
    }
    Ok(symbols)
}

fn prepare(input: &Input, addr: Option<u32>, decompile_timeout: u32, deadline: Instant, ctx: &Ctx) -> Result<Analysis> {
    if let Some(path) = &input.analysis {
        let path = path.canonicalize().map_err(io)?;
        let lock = Lock::queued(path.parent().ok_or_else(|| invalid("invalid manifest path"))?, deadline, ctx)?;
        return cached(path, lock);
    }
    let Import { source, bytes, exe, mapping, provenance, mut warnings } = import(input)?;
    let base = mapping.base;
    let end = base + mapping.size;
    if addr.is_some_and(|a| !(base..end).contains(&a)) {
        return Err(invalid("requested address is outside the mapped input"));
    }
    let install = installation(input.ghidra_home.as_deref())?;
    let symbols = analysis_symbols(input, &bytes, &mapping, &mut warnings)?;
    let source_hash = sha256_bytes(&bytes);
    let identity = json!({"format":FORMAT,"source":source_hash,"ghidra":install.build_sha256,"language":LANGUAGE,"compiler":"default","script":sha256_bytes(SCRIPT.as_bytes()),"symbols":symbols,"mapping":mapping,"capture":{"frames":provenance["frames"],"identity":provenance["identity"],"registers":provenance["registers"]}});
    let key = sha256_bytes(&serde_json::to_vec(&identity).map_err(io)?);
    let dir = ctx.root.join(".psx/ghidra").join(&key);
    fs::create_dir_all(&dir).map_err(io)?;
    let dir = dir.canonicalize().map_err(io)?;
    let lock = Lock::queued(&dir, deadline, ctx)?;
    let path = dir.join("analysis.json");
    if path.is_file() && !input.reanalyze {
        return cached(path, lock);
    }
    let attempt = dir.join(stamp());
    fs::create_dir(&attempt).map_err(io)?;
    // Ghidra 12 rejects project paths with any hidden path component, so its
    // database cannot live beneath .psx. Inputs/indexes remain there.
    let project_dir = ctx.root.join("ghidra-projects").join(format!("{}-{}", &key[..16], stamp()));
    fs::create_dir_all(&project_dir).map_err(io)?;
    let mut pending = PendingProject(Some(project_dir.clone()));
    let scripts = script_dir(&attempt)?;
    let copy = attempt.join(if exe.is_some() { "source.ps-exe" } else { "ram.bin" });
    fs::write(&copy, &bytes).map_err(io)?;
    let payload = attempt.join("payload.bin");
    fs::write(&payload, &bytes[mapping.file_offset..mapping.file_offset + mapping.size as usize]).map_err(io)?;
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
    let mut cmd = command(&install, &attempt)?;
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
    run_worker(&mut cmd, &attempt.join("worker.log"), deadline, ctx)?;
    let setup_out = read_json(&setup_result).map_err(|e| {
        fail("ghidra_failed", format!("{}; inspect {}", e.message, attempt.join("worker.log").display()))
    })?;
    if let Some(w) = setup_out["warnings"].as_array() {
        warnings.extend(w.iter().filter_map(Value::as_str).map(str::to_owned));
    }
    let index = read_json(&index_path).map_err(|e| {
        fail("ghidra_failed", format!("{}; inspect {}", e.message, attempt.join("worker.log").display()))
    })?;
    if index["analysis_timed_out"] == true {
        pending.keep();
        return Err(fail(
            "analysis_timeout",
            format!(
                "Partial project retained at {}; retry with a larger timeout. It is not cached as complete.",
                project_dir.display()
            ),
        ));
    }
    warnings.push("Only canonical KSEG0 main RAM is mapped; query aliases are folded, but static references using other RAM mirrors may remain unresolved.".into());
    warnings.push("Generic MIPS analysis: PS1 load-delay and COP2/GTE semantics are not guaranteed; verify affected pseudocode against instructions and runtime evidence.".into());
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
        annotations: vec![],
        installation: install,
        language: LANGUAGE.into(),
        compiler: "default".into(),
        script_sha256: sha256_bytes(SCRIPT.as_bytes()),
        project_fingerprint: fingerprint(&project_dir)?,
        project_dir,
        index: index_path,
        warnings,
    };
    save_json(&path, &manifest)?;
    pending.keep();
    let fresh_query = if let Some(a) = addr {
        let value = read_json(&attempt.join("query.json"))?;
        if value["status"] == "complete" {
            save_json(&attempt.join(format!("decompile-{a:08X}.json")), &value)?;
        }
        Some(value)
    } else {
        None
    };
    Ok(Analysis { path, manifest, index, fresh_query, _lock: lock })
}

fn summary(a: &Analysis) -> Value {
    json!({"analysis":a.path,"source":a.manifest.source,"source_sha256":a.manifest.source_sha256,"ghidra_version":a.manifest.installation.version,
        "project":a.manifest.project_dir.join("project.gpr"),"function_count":a.index["functions"].as_array().map_or(0,Vec::len),"warnings":a.manifest.warnings,
        "mapping":a.manifest.mapping,"provenance":a.manifest.provenance,"revision":a.manifest.revision,"references_truncated":a.index["references_truncated"],"cop2_instructions":a.index["cop2_instructions"],"rfe_instructions":a.index["rfe_instructions"]})
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
    if (1..=1000).contains(&n) { Ok(()) } else { Err(invalid("limit must be 1..=1000")) }
}

impl Op for DoctorArgs {
    fn exec(self, ctx: &Ctx) -> Outcome {
        outcome((|| {
            let install = installation(self.ghidra_home.as_deref())?;
            let dir = ctx.root.join(".psx/ghidra/doctor");
            fs::create_dir_all(&dir).map_err(io)?;
            let log = dir.join(format!("{}.log", stamp()));
            run_worker(Command::new("java").arg("-version"), &log, Instant::now() + Duration::from_secs(15), ctx)?;
            Ok(
                json!({"status":"available","ghidra":install,"java":fs::read_to_string(log).map_err(io)?,"validated_version":"12.1.4","language":LANGUAGE}),
            )
        })())
    }
}
impl Op for Input {
    fn exec(self, ctx: &Ctx) -> Outcome {
        outcome((|| {
            let deadline = budget(&self)?;
            let a = prepare(&self, None, function_timeout(), deadline, ctx)?;
            Ok(summary(&a))
        })())
    }
}
impl Op for FunctionsArgs {
    fn exec(self, ctx: &Ctx) -> Outcome {
        outcome((|| {
            valid_limit(self.limit)?;
            let deadline = budget(&self.input)?;
            let a = prepare(&self.input, None, function_timeout(), deadline, ctx)?;
            let filter = self.filter.unwrap_or_default().to_lowercase();
            let rows: Vec<Value> = a.index["functions"]
                .as_array()
                .ok_or_else(|| fail("invalid_artifact", "missing functions index"))?
                .iter()
                .filter(|f| f["name"].as_str().unwrap_or("").to_lowercase().contains(&filter))
                .cloned()
                .collect();
            let mut out = summary(&a);
            out["functions"] = page(&rows, self.offset, self.limit);
            Ok(out)
        })())
    }
}
impl Op for XrefsArgs {
    fn exec(self, ctx: &Ctx) -> Outcome {
        outcome((|| {
            valid_limit(self.limit)?;
            let deadline = budget(&self.input)?;
            let addr = hex(&self.addr)?;
            let canonical = ram_address(addr).unwrap_or(addr);
            let a = prepare(&self.input, None, function_timeout(), deadline, ctx)?;
            let target = format!("0x{canonical:08X}");
            let to: Vec<Value> = references(&a)
                .iter()
                .filter(|r| same_address(&r["to"], canonical) && r["to_space"] == "ram")
                .cloned()
                .collect();
            let from: Vec<Value> =
                references(&a).iter().filter(|r| same_address(&r["from"], canonical)).cloned().collect();
            let mut out = summary(&a);
            out["requested_addr"] = json!(self.addr);
            out["addr"] = json!(target);
            out["references_to"] = page(&to, self.offset, self.limit);
            out["references_from"] = page(&from, self.offset, self.limit);
            out["references_truncated"] = a.index["references_truncated"].clone();
            Ok(out)
        })())
    }
}
impl Op for DecompileArgs {
    fn exec(self, ctx: &Ctx) -> Outcome {
        outcome((|| {
            if !(1..=300).contains(&self.decompile_timeout) {
                return Err(invalid("decompile_timeout must be 1..=300"));
            }
            let deadline = budget(&self.input)?;
            let addr = ram_address(hex(&self.addr)?)?;
            let mut a = prepare(&self.input, Some(addr), self.decompile_timeout, deadline, ctx)?;
            let dir = a.manifest.index.parent().unwrap();
            let result_path = dir.join(format!("decompile-{addr:08X}.json"));
            let mut result = if let Some(result) = a.fresh_query.take() {
                result
            } else if result_path.is_file() {
                read_json(&result_path)?
            } else {
                let install = installation(self.input.ghidra_home.as_deref().or(Some(&a.manifest.installation.home)))?;
                if install.build_sha256 != a.manifest.installation.build_sha256 {
                    return Err(fail("stale_analysis", "Ghidra version changed; query source to reanalyse"));
                }
                let attempt = a.manifest.index.parent().unwrap();
                let request = attempt.join(format!("query-{}.json", stamp()));
                let output = request.with_extension("result.json");
                save_json(
                    &request,
                    &json!({"mode":"query","addr":addr,"decompile_timeout":self.decompile_timeout,"query_result":output}),
                )?;
                let mut cmd = command(&install, attempt)?;
                cmd.arg(&a.manifest.project_dir)
                    .arg("project")
                    .args(["-process", "payload.bin", "-noanalysis", "-readOnly", "-max-cpu", "2"])
                    .arg("-scriptPath")
                    .arg(script_dir(attempt)?)
                    .arg("-postScript")
                    .arg("PsxGhidra.java")
                    .arg(&request);
                run_worker(&mut cmd, &request.with_extension("log"), deadline, ctx)?;
                let result = read_json(&output).map_err(|e| {
                    fail("ghidra_failed", format!("{}; inspect {}", e.message, request.with_extension("log").display()))
                })?;
                // Ghidra may touch project metadata even in read-only mode.
                a.manifest.project_fingerprint = fingerprint(&a.manifest.project_dir)?;
                save_json(&a.path, &a.manifest)?;
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
            out["callers"] = page(&callers, 0, 100);
            out["callees"] = page(&callees, 0, 100);
            out["artifact"] = if result_path.exists() { json!(result_path) } else { Value::Null };
            Ok(out)
        })())
    }
}

fn open_analysis(path: &Path, deadline: Instant, ctx: &Ctx) -> Result<Analysis> {
    let path = path.canonicalize().map_err(io)?;
    let lock = Lock::queued(path.parent().ok_or_else(|| invalid("invalid analysis path"))?, deadline, ctx)?;
    cached(path, lock)
}

fn patches(index: &Value, patch: Patch) -> Result<(Vec<Value>, Vec<Value>)> {
    if patch.functions.is_empty() || patch.functions.len() > 1000 {
        return Err(invalid("patch requires 1..=1000 functions"));
    }
    let functions = index["functions"].as_array().ok_or_else(|| fail("invalid_artifact", "missing functions"))?;
    let mut rows = std::collections::BTreeMap::new();
    for p in patch.functions {
        let addr = ram_address(hex(&p.addr)?)?;
        if !functions.iter().any(|f| same_address(&f["entry"], addr)) {
            return Err(invalid(format!("0x{addr:08X} is not a function entry")));
        }
        if p.name.is_none() && p.comment.is_none() {
            return Err(invalid("empty function patch"));
        }
        let mut row = json!({"address":addr});
        if let Some(name) = p.name {
            if name.is_empty()
                || name.len() > 128
                || !name
                    .bytes()
                    .enumerate()
                    .all(|(i, b)| b.is_ascii_alphabetic() || b == b'_' || (i > 0 && b.is_ascii_digit()))
            {
                return Err(invalid("names must be ASCII identifiers, at most 128 characters"));
            }
            row["name"] = json!(name);
        }
        if let Some(comment) = p.comment {
            if comment.len() > 16384 || comment.contains('\0') {
                return Err(invalid("comment exceeds 16 KiB or contains NUL"));
            }
            row["comment"] = json!(comment);
        }
        if rows.insert(addr, row).is_some() {
            return Err(invalid("duplicate function in patch"));
        }
    }
    let mut names = std::collections::BTreeMap::new();
    for f in functions {
        let addr = hex(f["entry"].as_str().ok_or_else(|| invalid("invalid indexed entry"))?)?;
        let name = rows.get(&addr).and_then(|r| r["name"].as_str()).unwrap_or_else(|| f["name"].as_str().unwrap_or(""));
        names.entry(name.to_string()).or_insert_with(Vec::new).push(addr);
    }
    let mut conflicts = Vec::new();
    for (name, addrs) in &names {
        if addrs.len() < 2 {
            continue;
        }
        let claimed: Vec<_> = addrs.iter().filter(|a| rows.get(a).is_some_and(|p| p["name"].is_string())).collect();
        if claimed.is_empty() {
            continue;
        }
        if claimed.len() != 1 {
            return Err(fail("symbol_conflict", format!("name {name:?} is ambiguous")));
        }
        for addr in addrs.iter().filter(|a| *a != claimed[0]) {
            let f = functions.iter().find(|f| same_address(&f["entry"], *addr)).unwrap();
            if matches!(f["name_source"].as_str(), Some("USER_DEFINED" | "IMPORTED")) {
                return Err(fail(
                    "symbol_conflict",
                    format!("name {name:?} belongs to user/imported function 0x{addr:08X}"),
                ));
            }
            let mut replacement = format!("psx_generated_{addr:08X}");
            while names.contains_key(&replacement) {
                replacement.push('_');
            }
            let row = rows.entry(*addr).or_insert_with(|| json!({"address":addr}));
            row["name"] = json!(replacement);
            row["generated"] = json!(true);
            conflicts.push(json!({"name":name,"displaced_entry":format!("0x{addr:08X}"),"replacement":replacement}));
        }
    }
    Ok((rows.into_values().collect(), conflicts))
}

fn copy_project(source: &Path, target: &Path) -> Result<()> {
    fs::create_dir(target).map_err(io)?;
    for entry in fs::read_dir(source).map_err(io)? {
        let entry = entry.map_err(io)?;
        let ty = entry.file_type().map_err(io)?;
        let name = entry.file_name();
        if ty.is_symlink() {
            return Err(fail("invalid_artifact", "project contains a symlink"));
        }
        if name.to_string_lossy().ends_with(".lock") || name.to_string_lossy().ends_with(".lock~") {
            continue;
        }
        let dest = target.join(name);
        if ty.is_dir() {
            copy_project(&entry.path(), &dest)?;
        } else {
            fs::copy(entry.path(), dest).map_err(io)?;
        }
    }
    Ok(())
}
impl Op for AnnotateArgs {
    fn exec(self, ctx: &Ctx) -> Outcome {
        outcome((|| {
            if !(1..=3600).contains(&self.timeout) {
                return Err(invalid("timeout must be 1..=3600"));
            }
            let deadline = Instant::now() + Duration::from_secs(self.timeout);
            let mut a = open_analysis(&self.analysis, deadline, ctx)?;
            let patch: Patch = serde_json::from_value(read_json(&self.file)?).map_err(invalid)?;
            let (patches, conflicts) = patches(&a.index, patch)?;
            let install = installation(self.ghidra_home.as_deref().or(Some(&a.manifest.installation.home)))?;
            if install.build_sha256 != a.manifest.installation.build_sha256 {
                return Err(fail("stale_analysis", "Ghidra version changed"));
            }
            // Work on a copy: timeout, cancellation or script failure cannot leave the
            // published manifest pointing at a partially annotated database.
            let attempt = a.path.parent().unwrap().join(stamp());
            fs::create_dir(&attempt).map_err(io)?;
            let project = a.manifest.project_dir.with_file_name(format!("{}-{}", &a.manifest.key[..16], stamp()));
            let mut pending = PendingProject(Some(project.clone()));
            copy_project(&a.manifest.project_dir, &project)?;
            let index = attempt.join("index.json");
            let request = attempt.join("annotations.json");
            save_json(&request, &json!({"mode":"annotate","patches":patches,"index":index}))?;
            let mut cmd = command(&install, &attempt)?;
            cmd.arg(&project)
                .arg("project")
                .args(["-process", "payload.bin", "-noanalysis", "-max-cpu", "2"])
                .arg("-scriptPath")
                .arg(script_dir(&attempt)?)
                .arg("-postScript")
                .arg("PsxGhidra.java")
                .arg(&request);
            run_worker(&mut cmd, &attempt.join("worker.log"), deadline, ctx)?;
            a.index = read_json(&index)?;
            if !a.index["functions"].is_array() {
                return Err(fail("ghidra_failed", "annotation index missing"));
            }
            a.manifest.revision += 1;
            a.manifest.annotations.push(json!({"revision":a.manifest.revision,"patch":request,"conflicts":conflicts}));
            a.manifest.index = index;
            let old = std::mem::replace(&mut a.manifest.project_dir, project);
            let old_fingerprint =
                std::mem::replace(&mut a.manifest.project_fingerprint, fingerprint(&a.manifest.project_dir)?);
            save_json(&a.path, &a.manifest)?;
            pending.keep();
            discard_project(&old, Some(&old_fingerprint));
            let mut out = summary(&a);
            out["status"] = json!("complete");
            out["conflicts"] = json!(conflicts);
            out["patches"] = json!(patches);
            Ok(out)
        })())
    }
}
impl Op for ExportArgs {
    fn exec(self, ctx: &Ctx) -> Outcome {
        outcome((|| {
            use crate::bound_symbols::{Document, Function, Range};
            if !(1..=3600).contains(&self.timeout) {
                return Err(invalid("timeout must be 1..=3600"));
            }
            let a = open_analysis(&self.analysis, Instant::now() + Duration::from_secs(self.timeout), ctx)?;
            let m = &a.manifest;
            let bytes = fs::read(&m.input).map_err(io)?;
            let mapping = &m.mapping;
            let image = bytes
                .get(mapping.file_offset..mapping.file_offset + mapping.size as usize)
                .ok_or_else(|| fail("invalid_artifact", "invalid input mapping"))?;
            let mut functions = Vec::new();
            let mut names = std::collections::HashSet::new();
            for f in a.index["functions"].as_array().ok_or_else(|| fail("invalid_artifact", "missing functions"))? {
                let address = hex(f["entry"].as_str().ok_or_else(|| invalid("invalid indexed entry"))?)?;
                let mut body = Vec::new();
                for r in f["body"].as_array().ok_or_else(|| invalid("missing function body"))? {
                    let start = hex(r["start"].as_str().ok_or_else(|| invalid("invalid body start"))?)?;
                    let end = hex(r["end_inclusive"].as_str().ok_or_else(|| invalid("invalid body end"))?)?
                        .checked_add(1)
                        .ok_or_else(|| invalid("body end overflow"))?;
                    let lo =
                        start.checked_sub(mapping.base).ok_or_else(|| invalid("body outside mapped input"))? as usize;
                    let hi =
                        end.checked_sub(mapping.base).ok_or_else(|| invalid("body outside mapped input"))? as usize;
                    let data = image
                        .get(lo..hi)
                        .filter(|_| start < end)
                        .ok_or_else(|| invalid("body outside mapped input"))?;
                    body.push(Range { start, end, sha256: sha256_bytes(data) });
                }
                let name = f["name"].as_str().ok_or_else(|| invalid("missing function name"))?;
                if name.chars().any(char::is_whitespace) || !names.insert(name.to_owned()) {
                    return Err(fail(
                        "symbol_conflict",
                        "Export requires unique names without whitespace; rename conflicting functions first.",
                    ));
                }
                functions.push(Function { address, name: name.into(), body });
            }
            let document = Document {
                format: 2,
                analysis: a.path.clone(),
                revision: m.revision,
                input: m.input.clone(),
                input_sha256: m.source_sha256.clone(),
                base: mapping.base,
                file_offset: mapping.file_offset,
                size: mapping.size as usize,
                functions,
            };
            crate::bound_symbols::Loaded::load(document.clone(), Path::new(".")).map_err(invalid)?;
            let path = self.out.unwrap_or_else(|| m.index.parent().unwrap().join("export.symbols.json"));
            if !path.to_string_lossy().ends_with(".symbols.json") {
                return Err(invalid("output must end in .symbols.json"));
            }
            // Preserve the copied input, manifest, project and indexes even with an explicit output path.
            if path.exists() {
                if read_json(&path)? != serde_json::to_value(&document).map_err(io)? {
                    return Err(fail(
                        "output_exists",
                        "symbol output exists with different content; choose a new path",
                    ));
                }
            } else {
                save_json(&path, &document)?;
            }
            Ok(
                json!({"analysis":a.path,"revision":m.revision,"symbols":path,"function_count":document.functions.len(),"verification":"all function body bytes; names are withheld without matching code context"}),
            )
        })())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn context() -> Ctx {
        let root = std::env::temp_dir().join(format!("psx-ghidra-unit-{}", stamp()));
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
    fn annotations_prioritize_user_names_and_report_collisions() {
        let index = json!({"functions":[{"entry":"80010000","name":"FUN_80010000","name_source":"DEFAULT"},
            {"entry":"80010040","name":"named_writer","name_source":"USER_DEFINED"},
            {"entry":"80010080","name":"FUN_80010080","name_source":"DEFAULT"}]});
        let patch = |v| serde_json::from_value(v).unwrap();
        let (rows, conflicts) = patches(
            &index,
            patch(json!({"functions":[{"addr":"a0010040","name":"FUN_80010000","comment":"observed"}]})),
        )
        .unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(conflicts.len(), 1);
        assert_eq!(rows[0]["generated"], true);
        let err =
            patches(&index, patch(json!({"functions":[{"addr":"80010000","name":"named_writer"}]}))).err().unwrap();
        assert_eq!(err.code, "symbol_conflict");
        assert!(patches(&index, patch(json!({"functions":[{"addr":"80010004","name":"interior"}]}))).is_err());
        assert!(
            patches(
                &index,
                patch(json!({"functions":[{"addr":"80010000","name":"same"},{"addr":"80010080","name":"same"}]}))
            )
            .is_err()
        );
        let (rows, _) = patches(
            &index,
            patch(json!({"functions":[{"addr":"80010000","name":"named_writer"},{"addr":"80010040","name":"FUN_80010000"}]})),
        )
        .unwrap();
        assert_eq!(rows.len(), 2); // name swaps are valid
    }

    #[cfg(unix)]
    #[test]
    fn annotation_timeout_preserves_the_published_project_and_revision() {
        use std::os::unix::fs::PermissionsExt;
        let ctx = context();
        let home = ctx.root.join("fake-ghidra");
        fs::create_dir_all(home.join("support")).unwrap();
        fs::create_dir_all(home.join("Ghidra")).unwrap();
        let props = "application.version=fake\n";
        fs::write(home.join("Ghidra/application.properties"), props).unwrap();
        let launcher = home.join("support/analyzeHeadless");
        fs::write(&launcher, "#!/bin/sh\nsleep 30\n").unwrap();
        fs::set_permissions(&launcher, fs::Permissions::from_mode(0o755)).unwrap();
        let project = ctx.root.join("original-project");
        fs::create_dir(&project).unwrap();
        fs::write(project.join("project.gpr"), "original database").unwrap();
        let copy = ctx.root.join("input.bin");
        fs::write(&copy, [0; 8]).unwrap();
        let index = ctx.root.join("index.json");
        save_json(&index, &json!({"functions":[{"entry":"80010000","name":"original","name_source":"USER_DEFINED"}]}))
            .unwrap();
        let manifest = Manifest {
            format: FORMAT,
            key: "0123456789abcdef".repeat(4),
            source: "authored".into(),
            source_sha256: sha256_bytes(&[0; 8]),
            input: copy,
            payload_sha256: sha256_bytes(&[0; 8]),
            exe: None,
            mapping: Mapping {
                base: 0x80010000,
                size: 8,
                file_offset: 0,
                entry: 0x80010000,
                gp: 0,
                sp: 0,
                bss_addr: 0,
                bss_size: 0,
                code_ranges: vec![(0x80010000, 0x80010008)],
                entries: vec![0x80010000],
            },
            provenance: Value::Null,
            revision: 0,
            annotations: vec![],
            installation: installation(Some(&home)).unwrap(),
            language: LANGUAGE.into(),
            compiler: "default".into(),
            script_sha256: sha256_bytes(SCRIPT.as_bytes()),
            project_fingerprint: fingerprint(&project).unwrap(),
            project_dir: project.clone(),
            index,
            warnings: vec![],
        };
        let path = ctx.root.join("analysis.json");
        save_json(&path, &manifest).unwrap();
        let before = fs::read(&path).unwrap();
        let patch = ctx.root.join("patch.json");
        save_json(&patch, &json!({"functions":[{"addr":"80010000","name":"renamed"}]})).unwrap();
        let out = AnnotateArgs { analysis: path.clone(), file: patch, ghidra_home: Some(home), timeout: 1 }.exec(&ctx);
        assert_eq!(out.value["error"]["code"], "timeout", "{}", out.value);
        assert_eq!(fs::read(&path).unwrap(), before);
        assert_eq!(fs::read_to_string(project.join("project.gpr")).unwrap(), "original database");
        let original = open_analysis(&path, Instant::now() + Duration::from_secs(5), &ctx).unwrap();
        assert_eq!(original.index["functions"][0]["name"], "original");
        drop(original);
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
        let _next = Lock::acquire(&ctx.root).unwrap();
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
        assert!(held.0.exists(), "a timeout must preserve the owner");
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
        assert!(held.0.exists());
        drop(held);
        fs::write(ctx.root.join("psx.lock"), "foreign owner").unwrap();
        assert_eq!(
            Lock::queued(&ctx.root, Instant::now() + Duration::from_secs(2), &ctx).err().unwrap().code,
            "analysis_busy"
        );
        fs::remove_dir_all(ctx.root).unwrap();
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
            let failure =
                run_worker(&mut cmd, &ctx.root.join("worker.log"), Instant::now() + Duration::from_millis(500), &ctx)
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
