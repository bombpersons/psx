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
use serde_json::{json, Value};

use crate::commands::{Ctx, Op, Outcome};
use psx_media::{disc::sha256_bytes, PsxExe};

const SCRIPT: &str = include_str!("../../../tools/ghidra/PsxGhidra.java");
const LANGUAGE: &str = "MIPS:LE:32:default";
const FORMAT: u32 = 1;
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
        Err(e) => Outcome { value: json!({"error":{"code":e.code,"message":e.message}}), exit: 1, pretty: true, text: false },
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
}
impl Action {
    pub fn exec(self, ctx: &Ctx) -> Outcome {
        match self {
            Self::Doctor(a) => a.exec(ctx),
            Self::Analyze(a) => a.exec(ctx),
            Self::Functions(a) => a.exec(ctx),
            Self::Decompile(a) => a.exec(ctx),
            Self::Xrefs(a) => a.exec(ctx),
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
    #[arg(long, conflicts_with = "analysis")]
    pub source: Option<String>,
    /// Analysis manifest returned by an earlier query, instead of source.
    #[arg(long, conflicts_with = "source")]
    pub analysis: Option<PathBuf>,
    /// Optional .symbols.json or .sym inputs to seed function names; repeatable, with source only.
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
    /// Build a fresh project from source, preserving the previous project and artifacts.
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
    /// Case-insensitive substring of function name.
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
        let version = props.lines().find_map(|s| s.strip_prefix("application.version=")).unwrap_or("unknown").to_string();
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
    serde_json::from_slice(&fs::read(path).map_err(io)?).map_err(|e| fail("invalid_artifact", format!("{}: {e}", path.display())))
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
    u32::from_str_radix(s.trim().trim_start_matches("0x").trim_start_matches("0X"), 16)
        .map_err(|_| invalid(format!("invalid hex address {s:?}")))
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
    if input.source.is_some() == input.analysis.is_some() {
        return Err(invalid("give exactly one of source or analysis"));
    }
    if input.analysis.is_some() && !input.symbols.is_empty() {
        return Err(invalid("symbols can only be supplied with source"));
    }
    if input.analysis.is_some() && input.reanalyze {
        return Err(invalid("reanalyze requires source, not an analysis handle"));
    }
    Ok(Instant::now() + Duration::from_secs(input.timeout))
}

/// One owner per project, including cached readers. Never remove a lock left by
/// another process; interrupted locks require explicit inspection by the user.
struct Lock(PathBuf);
impl Lock {
    fn acquire(dir: &Path) -> Result<Self> {
        let path = dir.join("psx.lock");
        let mut f = OpenOptions::new().write(true).create_new(true).open(&path).map_err(|e| {
            fail(
                "analysis_busy",
                format!(
                    "{}: {e}; another query may be using this analysis. Inspect an interrupted owner before removing the lock.",
                    path.display()
                ),
            )
        })?;
        let lock = Self(path);
        writeln!(f, "pid={}", std::process::id()).map_err(io)?;
        Ok(lock)
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
            Some(status) => return Err(fail("ghidra_failed", format!("Ghidra exited {status}; inspect {}", log.display()))),
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
    exe: PsxExe,
    installation: Installation,
    language: String,
    compiler: String,
    script_sha256: String,
    project_dir: PathBuf,
    index: PathBuf,
    project_fingerprint: String,
    warnings: Vec<String>,
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
    let manifest: Manifest = serde_json::from_value(read_json(&path)?).map_err(io)?;
    if manifest.format != FORMAT || manifest.script_sha256 != sha256_bytes(SCRIPT.as_bytes()) {
        return Err(fail("stale_analysis", "Analysis format/scripts changed; query the source again to create a compatible analysis."));
    }
    if gui_lock(&manifest.project_dir) {
        return Err(fail("analysis_busy", "Close this analysis in the Ghidra GUI before querying it."));
    }
    if fingerprint(&manifest.project_dir)? != manifest.project_fingerprint {
        return Err(fail("stale_analysis", "Ghidra project changed outside psx; cached results are no longer valid. Query source with reanalyze=true for a fresh import; the edited project is preserved."));
    }
    let bytes = fs::read(&manifest.input).map_err(io)?;
    if sha256_bytes(&bytes) != manifest.source_sha256 {
        return Err(fail("invalid_artifact", "Analysis input copy has changed."));
    }
    let index = read_json(&manifest.index)?;
    Ok(Analysis { path, manifest, index, fresh_query: None, _lock: lock })
}

fn prepare(input: &Input, addr: Option<u32>, decompile_timeout: u32, deadline: Instant, ctx: &Ctx) -> Result<Analysis> {
    if let Some(path) = &input.analysis {
        let path = path.canonicalize().map_err(io)?;
        let lock = Lock::acquire(path.parent().ok_or_else(|| invalid("invalid manifest path"))?)?;
        return cached(path, lock);
    }
    let source = input.source.as_ref().ok_or_else(|| invalid("missing source"))?;
    let bytes = crate::read_source(source).map_err(|e| fail(e.code(), e))?;
    let exe = PsxExe::parse(&bytes).map_err(|e| fail(e.code(), e))?;
    let base = ram_address(exe.text_addr)?;
    let end = base.checked_add(exe.text_size).filter(|&end| end <= 0x8020_0000).ok_or_else(|| invalid("EXE payload exceeds main RAM"))?;
    let entry = ram_address(exe.pc)?;
    if exe.text_size == 0
        || exe.payload(&bytes).len() != exe.text_size as usize
        || !(base..end).contains(&entry)
        || entry & 3 != 0
        || base & 3 != 0
    {
        return Err(invalid("EXE has a truncated/empty payload or an invalid entry address"));
    }
    if let Some(a) = addr {
        if !(base..end).contains(&a) {
            return Err(invalid("requested function address is outside the EXE payload"));
        }
    }
    let bss = if exe.bss_size > 0 {
        let start = ram_address(exe.bss_addr)?;
        let stop = start.checked_add(exe.bss_size).filter(|&n| n <= 0x8020_0000).ok_or_else(|| invalid("BSS exceeds main RAM"))?;
        if start < end && stop > base {
            return Err(invalid("BSS overlaps the loaded payload"));
        }
        start
    } else {
        0
    };
    let install = installation(input.ghidra_home.as_deref())?;
    let mut symbols = Vec::new();
    let mut warnings = exe.warnings.clone();
    for path in &input.symbols {
        let text = fs::read_to_string(path).map_err(io)?;
        if path.to_string_lossy().ends_with(".symbols.json") {
            let value: Value = serde_json::from_str(&text).map_err(io)?;
            let original = crate::read_source(value["source"].as_str().ok_or_else(|| invalid("symbols report lacks source"))?)
                .map_err(|e| fail(e.code(), e))?;
            if sha256_bytes(&original) != sha256_bytes(&bytes) {
                return Err(invalid(format!("{} describes a different executable", path.display())));
            }
            if value["exe_sha256"].as_str().is_some_and(|s| s != sha256_bytes(&original)) {
                return Err(invalid(format!("{} is stale: its source bytes have changed", path.display())));
            }
            let list = value["report"]["symbols"].as_array().ok_or_else(|| invalid("symbols report lacks report.symbols"))?;
            for s in list {
                let a = s["address"].as_u64().and_then(|a| u32::try_from(a).ok()).ok_or_else(|| invalid("invalid symbol address"))?;
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
        if !(base..end).contains(&a) || a & 3 != 0 {
            return Err(invalid("symbol address outside payload or unaligned"));
        }
        if s["name"].as_str().is_none_or(str::is_empty) || !matches!(s["kind"].as_str(), Some("function" | "label" | "local")) {
            return Err(invalid("symbols need a nonempty name and kind function, label or local"));
        }
    }
    let source_hash = sha256_bytes(&bytes);
    let identity = json!({"format":FORMAT,"source":source_hash,"ghidra":install.build_sha256,"language":LANGUAGE,"compiler":"default","script":sha256_bytes(SCRIPT.as_bytes()),"symbols":symbols});
    let key = sha256_bytes(&serde_json::to_vec(&identity).map_err(io)?);
    let dir = ctx.root.join(".psx/ghidra").join(&key);
    fs::create_dir_all(&dir).map_err(io)?;
    let dir = dir.canonicalize().map_err(io)?;
    let lock = Lock::acquire(&dir)?;
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
    let scripts = script_dir(&attempt)?;
    let copy = attempt.join("source.ps-exe");
    fs::write(&copy, &bytes).map_err(io)?;
    let payload = attempt.join("payload.bin");
    fs::write(&payload, exe.payload(&bytes)).map_err(io)?;
    let index_path = attempt.join("index.json");
    let setup_result = attempt.join("setup-result.json");
    let mut request = json!({"mode":"setup","base":base,"entry":entry,"gp":exe.gp,"sp":exe.initial_sp(),"bss_addr":bss,"bss_size":exe.bss_size,"symbols":symbols,"setup_result":setup_result});
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
    let setup_out = read_json(&setup_result)
        .map_err(|e| fail("ghidra_failed", format!("{}; inspect {}", e.message, attempt.join("worker.log").display())))?;
    if let Some(w) = setup_out["warnings"].as_array() {
        warnings.extend(w.iter().filter_map(Value::as_str).map(str::to_owned));
    }
    let index = read_json(&index_path)
        .map_err(|e| fail("ghidra_failed", format!("{}; inspect {}", e.message, attempt.join("worker.log").display())))?;
    if index["analysis_timed_out"] == true {
        return Err(fail(
            "analysis_timeout",
            format!("Partial project retained at {}; retry with a larger timeout. It is not cached as complete.", project_dir.display()),
        ));
    }
    warnings.push("Generic MIPS analysis: PS1 load-delay and COP2/GTE semantics are not guaranteed; verify affected pseudocode against instructions and runtime evidence.".into());
    let manifest = Manifest {
        format: FORMAT,
        key,
        source: source.clone(),
        source_sha256: source_hash,
        input: copy,
        payload_sha256: sha256_bytes(exe.payload(&bytes)),
        exe,
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
        "references_truncated":a.index["references_truncated"],"cop2_instructions":a.index["cop2_instructions"],"rfe_instructions":a.index["rfe_instructions"]})
}
fn references(a: &Analysis) -> &[Value] {
    a.index["references"].as_array().map_or(&[], Vec::as_slice)
}
fn same_address(value: &Value, target: u32) -> bool {
    value.as_str().and_then(|s| hex(s).ok()).is_some_and(|addr| ram_address(addr).unwrap_or(addr) == target)
}
fn page(rows: &[Value], offset: usize, count: usize) -> Value {
    json!({"total":rows.len(),"offset":offset,"items":rows.iter().skip(offset).take(count).collect::<Vec<_>>(),"next_offset":(offset.saturating_add(count)<rows.len()).then_some(offset.saturating_add(count))})
}
fn valid_limit(n: usize) -> Result<()> {
    if (1..=1000).contains(&n) {
        Ok(())
    } else {
        Err(invalid("limit must be 1..=1000"))
    }
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
            let to: Vec<Value> =
                references(&a).iter().filter(|r| same_address(&r["to"], canonical) && r["to_space"] == "ram").cloned().collect();
            let from: Vec<Value> = references(&a).iter().filter(|r| same_address(&r["from"], canonical)).cloned().collect();
            let mut out = summary(&a);
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
                save_json(&request, &json!({"mode":"query","addr":addr,"decompile_timeout":self.decompile_timeout,"query_result":output}))?;
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
                let result = read_json(&output)
                    .map_err(|e| fail("ghidra_failed", format!("{}; inspect {}", e.message, request.with_extension("log").display())))?;
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
                .filter(|r| r["call"] == true && r["to_space"] == "ram" && entry_addr.is_some_and(|entry| same_address(&r["to"], entry)))
                .cloned()
                .collect();
            let callees: Vec<Value> = references(&a).iter().filter(|r| r["call"] == true && r["function"] == entry).cloned().collect();
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
    fn aliases_and_paging_are_bounded() {
        assert_eq!(ram_address(0xA061_2340).unwrap(), 0x8001_2340);
        assert!(ram_address(0x1F80_1810).is_err());
        assert!(ram_address(0xBFC0_0000).is_err());
        assert!(same_address(&json!("0xA0012340"), 0x8001_2340));
        assert!(!same_address(&json!("0x1F801810"), 0x8000_1810));
        let rows = vec![json!(1), json!(2), json!(3)];
        assert_eq!(page(&rows, 1, 1), json!({"total":3,"offset":1,"items":[2],"next_offset":2}));
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
                run_worker(&mut cmd, &ctx.root.join("worker.log"), Instant::now() + Duration::from_millis(500), &ctx).unwrap_err();
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
            assert!(state.as_ref().map_or(true, |s| s.split_whitespace().nth(2) == Some("Z")), "descendant still running: {state:?}");
            fs::remove_dir_all(ctx.root).unwrap();
        }
    }
}
