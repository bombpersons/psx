//! Runs PCSX-Redux as a separate process for one bounded job.
//!
//! Each job gets its own directory holding the parameters, a private Redux
//! configuration (`-portable`) and every output. Nothing is shared between
//! jobs, so concurrent agents cannot disturb each other. Redux is GPL-2.0; it
//! is executed, never linked.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub const RAM_SIZE: usize = 2 * 1024 * 1024;
pub const VRAM_SIZE: usize = 1024 * 1024;

#[derive(Debug)]
pub enum ReduxError {
    Setup(String),
    Io(PathBuf, std::io::Error),
    Timeout { seconds: u64, log_tail: String },
    Failed { status: String, log_tail: String },
}

impl std::fmt::Display for ReduxError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ReduxError::Setup(m) => write!(f, "setup: {m}"),
            ReduxError::Io(p, e) => write!(f, "{}: {e}", p.display()),
            ReduxError::Timeout { seconds, log_tail } => write!(f, "timed out after {seconds}s\n{log_tail}"),
            ReduxError::Failed { status, log_tail } => write!(f, "redux exited ({status}) without a result\n{log_tail}"),
        }
    }
}

impl std::error::Error for ReduxError {}

impl ReduxError {
    pub fn code(&self) -> &'static str {
        match self {
            ReduxError::Setup(_) => "backend_unavailable",
            ReduxError::Io(..) => "io_error",
            ReduxError::Timeout { .. } => "timeout",
            ReduxError::Failed { .. } => "backend_failed",
        }
    }
}

pub type Result<T> = std::result::Result<T, ReduxError>;

fn io<T>(path: &Path, r: std::io::Result<T>) -> Result<T> {
    r.map_err(|e| ReduxError::Io(path.to_path_buf(), e))
}

/// Where the backend lives. Paths are resolved by the caller.
#[derive(Clone, Debug, Serialize)]
pub struct Backend {
    pub binary: PathBuf,
    pub job_script: PathBuf,
    /// Interpreter + debugger are required for watchpoints.
    pub interpreter: bool,
}

impl Backend {
    /// Locate the pinned checkout relative to the psx project root.
    pub fn from_project(root: &Path) -> Result<Self> {
        let binary = std::env::var_os("PSX_REDUX_BIN")
            .map(PathBuf::from)
            .unwrap_or_else(|| root.join("external/pcsx-redux/pcsx-redux"));
        let job_script = root.join("tools/redux/psx_job.lua");
        for p in [&binary, &job_script] {
            if !p.exists() {
                return Err(ReduxError::Setup(format!("{} not found (build PCSX-Redux or set PSX_REDUX_BIN)", p.display())));
            }
        }
        Ok(Backend { binary, job_script, interpreter: true })
    }

    pub fn identity(&self) -> Result<String> {
        sha256_file(&self.binary)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum Media {
    /// A disc image (`.cue`, `.bin`, `.iso`).
    Disc(PathBuf),
    /// A PS-X EXE side-loaded after the BIOS shell.
    Exe(PathBuf),
    /// BIOS only (or continuing from a state).
    None,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ReplayEvent {
    /// Frame (vsync) relative to the job start at which `buttons` become held.
    pub frame: u32,
    /// Redux button names: CROSS, CIRCLE, SQUARE, TRIANGLE, L1, R1, L2, R2,
    /// START, SELECT, UP, DOWN, LEFT, RIGHT.
    pub buttons: Vec<String>,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub struct Watch {
    pub addr: u32,
    pub width: u32,
}

#[derive(Clone, Debug)]
pub struct Job {
    pub bios: PathBuf,
    pub media: Media,
    pub frames: u32,
    pub replay: Vec<ReplayEvent>,
    pub watches: Vec<Watch>,
    pub max_watch_hits: u32,
    pub load_state: Option<PathBuf>,
    pub save_state: bool,
    pub timeout: Duration,
    /// Pace emulation at real time instead of as fast as possible.
    pub realtime: bool,
}

pub const BUTTONS: [&str; 14] =
    ["CROSS", "CIRCLE", "SQUARE", "TRIANGLE", "L1", "R1", "L2", "R2", "START", "SELECT", "UP", "DOWN", "LEFT", "RIGHT"];

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Registers {
    pub pc: u32,
    #[serde(flatten)]
    pub other: serde_json::Map<String, serde_json::Value>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ScreenInfo {
    pub width: u32,
    pub height: u32,
    pub bpp: u32,
}

/// `result.json` written by the job script.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct JobReport {
    pub termination: String,
    pub requested_frames: u32,
    pub reached_frames: u32,
    pub cpu_cycles: u64,
    pub registers: Registers,
    pub screen: ScreenInfo,
    pub watch_hits: u64,
    pub watch_hits_recorded: u64,
    pub saved_state: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WatchHit {
    pub frame: u32,
    pub address: u32,
    pub width: u32,
    pub cycle: u64,
    pub pc: u32,
    pub ra: u32,
    pub sp: u32,
    /// Aligned word at the address *before* the write executes.
    pub old_word: u32,
}

#[derive(Debug)]
pub struct JobOutput {
    pub dir: PathBuf,
    pub report: JobReport,
    pub hits: Vec<WatchHit>,
    pub wall_ms: u128,
}

impl JobOutput {
    pub fn file(&self, name: &str) -> PathBuf {
        self.dir.join(name)
    }

    pub fn ram(&self) -> Result<Vec<u8>> {
        let p = self.file("ram.bin");
        io(&p, std::fs::read(&p))
    }

    /// Screen as RGB8, converting Redux's 16-bit BGR555 or 24-bit capture.
    pub fn screen_rgb(&self) -> Result<(u32, u32, Vec<u8>)> {
        let p = self.file("screen.bin");
        let raw = io(&p, std::fs::read(&p))?;
        let ScreenInfo { width, height, bpp } = self.report.screen;
        let rgb = if bpp == 16 {
            raw.chunks_exact(2)
                .flat_map(|c| {
                    let v = u16::from_le_bytes([c[0], c[1]]);
                    let e = |x: u16| ((x & 31) << 3 | (x & 31) >> 2) as u8;
                    [e(v), e(v >> 5), e(v >> 10)]
                })
                .collect()
        } else {
            raw
        };
        Ok((width, height, rgb))
    }
}

fn lua_string(s: &str) -> String {
    let mut out = String::from("\"");
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            c if (c as u32) < 0x20 => {
                let _ = write!(out, "\\{}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

fn params_lua(job: &Job, load_state: Option<&Path>) -> Result<String> {
    let mut s = String::from("return {\n");
    let _ = writeln!(s, "  frames = {},", job.frames);
    let _ = writeln!(s, "  save_state = {},", job.save_state);
    let _ = writeln!(s, "  max_watch_hits = {},", job.max_watch_hits);
    if let Some(p) = load_state {
        let _ = writeln!(s, "  load_state = {},", lua_string(&p.to_string_lossy()));
    }
    s.push_str("  replay = {\n");
    for e in &job.replay {
        for b in &e.buttons {
            if !BUTTONS.contains(&b.as_str()) {
                return Err(ReduxError::Setup(format!("unknown button {b:?}; use one of {BUTTONS:?}")));
            }
        }
        let names: Vec<String> = e.buttons.iter().map(|b| lua_string(b)).collect();
        let _ = writeln!(s, "    {{ frame = {}, buttons = {{ {} }} }},", e.frame, names.join(", "));
    }
    s.push_str("  },\n  watches = {\n");
    for w in &job.watches {
        let _ = writeln!(s, "    {{ addr = 0x{:08X}, width = {} }},", w.addr, w.width);
    }
    s.push_str("  },\n}\n");
    Ok(s)
}

fn tail(path: &Path, lines: usize) -> String {
    let text = std::fs::read_to_string(path).unwrap_or_default();
    let v: Vec<&str> = text.lines().collect();
    v[v.len().saturating_sub(lines)..].join("\n")
}

/// Run one job in `dir` (created; must not already contain a result).
pub fn run(backend: &Backend, job: &Job, dir: &Path) -> Result<JobOutput> {
    io(dir, std::fs::create_dir_all(dir.join("config")))?;
    let dir = io(dir, dir.canonicalize())?;
    // Redux runs inside the job directory, so every input path must be absolute.
    let abs = |p: &Path| io(p, p.canonicalize());
    let bios = abs(&job.bios)?;
    let state_copy = match &job.load_state {
        Some(src) => {
            let dst = dir.join("input-state.bin");
            io(src, std::fs::copy(src, &dst))?;
            Some(dst)
        }
        None => None,
    };
    // Private settings. Safe mode (-testmode) would skip this file, so it is not used.
    // NullSync makes an internal thread, not the host audio device, drive the
    // clock; Speed <= 0 removes the realtime limit. Neither changes emulated state.
    let speed = if job.realtime { 1 } else { 0 };
    let cfg = dir.join("config/pcsx.json");
    io(&cfg, std::fs::write(&cfg, format!("{{\"SPU\":{{\"NullSync\":true,\"Speed\":{speed}}}}}\n")))?;
    let params = dir.join("params.lua");
    io(&params, std::fs::write(&params, params_lua(job, state_copy.as_deref())?))?;

    let mut cmd = Command::new(&backend.binary);
    cmd.current_dir(&dir)
        .arg("-no-ui")
        // Without -run, a side-loaded EXE leaves the emulator paused after loading.
        .arg("-run")
        .arg("-portable")
        .arg(dir.join("config"))
        .arg("-bios")
        .arg(&bios);
    if backend.interpreter {
        cmd.arg("-interpreter").arg("-debugger");
    } else {
        if !job.watches.is_empty() {
            return Err(ReduxError::Setup("watches need the interpreter (drop --fast)".into()));
        }
        cmd.arg("-dynarec").arg("-no-debugger");
    }
    match &job.media {
        Media::Disc(p) => {
            cmd.arg("-iso").arg(abs(p)?);
        }
        Media::Exe(p) => {
            cmd.arg("-exe").arg(abs(p)?);
        }
        Media::None => {}
    }
    // Redux runs -dofile before -exec, so bind JOB_DIR inside the script itself.
    let script = io(&backend.job_script, std::fs::read_to_string(&backend.job_script))?;
    let job_lua = dir.join("job.lua");
    io(&job_lua, std::fs::write(&job_lua, format!("JOB_DIR = {}\n{script}", lua_string(&dir.to_string_lossy()))))?;
    cmd.arg("-dofile").arg(&job_lua);
    let log_path = dir.join("redux.log");
    let log = io(&log_path, std::fs::File::create(&log_path))?;
    let log_err = io(&log_path, log.try_clone())?;
    cmd.stdin(Stdio::null()).stdout(log).stderr(log_err);

    let start = Instant::now();
    let mut child = io(&backend.binary, cmd.spawn())?;
    let status = loop {
        if let Some(status) = io(&dir, child.try_wait())? {
            break status;
        }
        if start.elapsed() > job.timeout {
            let _ = child.kill();
            let _ = child.wait();
            return Err(ReduxError::Timeout { seconds: job.timeout.as_secs(), log_tail: tail(&log_path, 20) });
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    let result_path = dir.join("result.json");
    let Ok(text) = std::fs::read_to_string(&result_path) else {
        return Err(ReduxError::Failed { status: status.to_string(), log_tail: tail(&log_path, 30) });
    };
    let report: JobReport = serde_json::from_str(&text)
        .map_err(|e| ReduxError::Failed { status: format!("bad result.json: {e}"), log_tail: tail(&log_path, 30) })?;
    let hits_path = dir.join("writes.jsonl");
    let hits = std::fs::read_to_string(&hits_path)
        .unwrap_or_default()
        .lines()
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect();
    Ok(JobOutput { dir, report, hits, wall_ms: start.elapsed().as_millis() })
}

pub fn sha256_file(path: &Path) -> Result<String> {
    let bytes = io(path, std::fs::read(path))?;
    Ok(sha256_bytes(&bytes))
}

pub fn sha256_bytes(bytes: &[u8]) -> String {
    Sha256::digest(bytes).iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn params_are_valid_lua_shaped_and_reject_unknown_buttons() {
        let mut job = Job {
            bios: "bios".into(),
            media: Media::None,
            frames: 60,
            replay: vec![ReplayEvent { frame: 10, buttons: vec!["CROSS".into(), "LEFT".into()] }],
            watches: vec![Watch { addr: 0x8001_4808, width: 4 }],
            max_watch_hits: 100,
            load_state: None,
            save_state: true,
            timeout: Duration::from_secs(5),
            realtime: false,
        };
        let lua = params_lua(&job, Some(Path::new("/tmp/a \"b\".bin"))).unwrap();
        assert!(lua.contains("frame = 10, buttons = { \"CROSS\", \"LEFT\" }"));
        assert!(lua.contains("addr = 0x80014808, width = 4"));
        assert!(lua.contains("load_state = \"/tmp/a \\\"b\\\".bin\""));
        job.replay[0].buttons.push("X".into());
        assert!(params_lua(&job, None).is_err());
    }
}
