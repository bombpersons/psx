//! `psx scenario play`: an agent asks a person to play. A window shows the
//! game and the agent's request; the person plays with a keyboard or gamepad
//! and presses F12 when it is done. With a scenario id the play starts at the
//! scenario's state (or the end of one of its recordings, `--from`) and
//! becomes a recording, made as `scenario record` makes one by replaying the
//! person's input, so it has the same frame origin and its coverage. Without
//! one it starts from media or a state, to reach a point the agent cannot
//! easily reach itself. The run directory then holds what the agent continues
//! from:
//!
//! - `state.bin`: the machine at the end (continue with `run --from-state`);
//! - `input.json`: everything the person pressed, as a `run --replay` file
//!   that starts where the handoff started, so the route can be replayed;
//! - `ram.bin`, `screen.png`, `note.txt` (the person's note), and
//!   `report.json` (the result);
//! - `marks/<n>/`: moments the person marked (F6) on the way, each with
//!   `state.bin`, `ram.bin`, `screen.png` and `mark.json` (label, frames);
//!   the report diffs each one's RAM against the mark before it on its route.
//!
//! The window runs in the calling process, or with `--detach` (always over MCP)
//! in a child process: the call returns a ticket at once and `scenario play --status`
//! reports progress (`live.json`) and the result.

use std::path::{Path, PathBuf};
use std::time::Duration;

use psx_core::Machine;
use psx_core::state::MediaId;
use serde_json::{Value, json};

use crate::Error;
use crate::commands::{Ctx, Op, Outcome};
use crate::replay::Input;

// Without the window only their tests use these.
#[cfg(feature = "window")]
mod audio;
#[cfg_attr(not(feature = "window"), allow(dead_code))]
pub mod bindings;
#[cfg_attr(not(feature = "window"), allow(dead_code))]
pub mod menu;
#[cfg(feature = "window")]
mod window;

#[derive(clap::Args, Clone, Debug, serde::Deserialize, serde::Serialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct HandoffArgs {
    /// What to do, shown in the window; be specific (where, facing what, holding what).
    #[arg(long, required_unless_present = "status", default_value = "")]
    #[serde(default)]
    pub request: String,
    /// A scenario (`scenarios/<ID>.toml`): start at its state and write the play as its
    /// recording NAME, with coverage, as scenario record does.
    #[arg(conflicts_with_all = ["media", "bios", "from_state", "memcard1", "memcard2", "pad", "verify"])]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    /// With a scenario: the recording's name (`scenarios/<ID>/<NAME>.jsonl`).
    #[arg(long, requires = "id")]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// With a scenario: start at the end of this recording of it instead of at its state.
    #[arg(long, requires = "id")]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from: Option<String>,
    /// With a scenario: what the recording does, in a sentence (default the request).
    #[arg(long, requires = "id")]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// With a scenario: write over an existing recording of that name.
    #[arg(long, requires = "id")]
    #[serde(default, skip_serializing_if = "is_false")]
    pub replace: bool,
    /// A play's run_dir (the ticket's): report its progress or result instead of starting one.
    #[arg(long, conflicts_with_all = ["request", "id", "media", "bios", "from_state", "out", "verify", "detach", "expect"])]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<PathBuf>,
    /// With --status: seconds to wait for the person to finish (max 600).
    #[arg(long, default_value_t = 0, requires = "status")]
    #[serde(default, skip_serializing_if = "is_zero")]
    pub wait: u64,
    /// Disc image or PS-X EXE, as for run (without a scenario).
    #[arg(long)]
    pub media: Option<PathBuf>,
    /// BIOS override, as for run.
    #[arg(long)]
    pub bios: Option<PathBuf>,
    /// Start from a state (run or handoff state.bin) instead of power-on.
    #[arg(long)]
    pub from_state: Option<PathBuf>,
    #[command(flatten)]
    #[serde(flatten)]
    pub cards: crate::run::CardArgs,
    /// Output directory (default .psx/handoffs/<id>).
    #[arg(long)]
    pub out: Option<PathBuf>,
    /// Afterwards, replay input.json headless and check it reproduces state.bin.
    /// Takes a while for long sessions.
    #[arg(long)]
    #[serde(default)]
    pub verify: bool,
    /// Return a ticket at once and run the window in its own process; poll with
    /// `--status`. Over MCP a play always detaches.
    #[arg(long)]
    #[serde(default)]
    pub detach: bool,
    /// The end state asked for, as run's until (`ADDR:FMT=VALUE`); repeatable. The result says
    /// whether each holds and whether to ask again.
    #[arg(long)]
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub expect: Vec<String>,
}

/// The expect conditions.
fn expectations(a: &HandoffArgs) -> crate::Result<Vec<crate::until::Until>> {
    let conds = a.expect.iter().map(|s| crate::until::parse(s));
    conds.collect::<Result<Vec<_>, _>>().map_err(|e| Error::invalid(e.replacen("--until", "--expect", 1)))
}

/// Whether the end state is what was asked for, and what to do about it.
fn expect_report(a: &HandoffArgs, m: &Machine, end: End) -> Value {
    let conds = match expectations(a) {
        Ok(c) if c.is_empty() => return Value::Null,
        Ok(c) => c,
        Err(e) => return e.to_value(),
    };
    let mut v = crate::until::check(&conds, &m.bus.ram, &m.bus.scratch);
    let met = v["met"] == true;
    v["verdict"] = json!(match (end, met) {
        (End::Done, true) => "The end state is what was asked for: continue from state.bin.",
        (End::Done, false) =>
            "The person said done, but the end state is not what was asked for: ask again, saying in game terms what differs, or continue from state.bin if it is close enough.",
        (_, true) =>
            "The person did not finish, but the end state is what was asked for: read the note, then continue from state.bin.",
        (_, false) => "Not reached: read the note, then ask again more precisely or from a start nearer the target.",
    });
    v["ask_again"] = json!(!met);
    v
}

#[derive(clap::Args, Clone, Debug, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct HandoffStatusArgs {
    /// The handoff ticket's run_dir.
    pub run_dir: PathBuf,
    /// Seconds to wait for the person to finish (max 600).
    #[arg(long, default_value_t = 0)]
    #[serde(default)]
    pub wait: u64,
}

/// Seconds without a `live.json` update after which a handoff counts as abandoned.
const STALE_S: u128 = 20;

fn is_zero(n: &u64) -> bool {
    *n == 0
}

fn is_false(b: &bool) -> bool {
    !*b
}

impl Op for HandoffArgs {
    fn exec(self, ctx: &Ctx) -> Outcome {
        if let Some(run_dir) = self.status {
            if !self.request.is_empty() {
                return Error::invalid("status reports a play: give it alone, without request").into();
            }
            return HandoffStatusArgs { run_dir, wait: self.wait }.exec(ctx);
        }
        if self.request.trim().is_empty() {
            return Error::invalid("request is required: what the person should do").into();
        }
        if let Err(e) = self.check_scenario() {
            return e.into();
        }
        Outcome::report(self.start(ctx))
    }
}

impl HandoffArgs {
    /// The arguments that go with a scenario, and those that do not.
    fn check_scenario(&self) -> crate::Result<()> {
        if self.id.is_some() {
            if self.name.is_none() {
                return Err(Error::invalid("name is required with a scenario id: the recording's name"));
            }
            if self.media.is_some() || self.bios.is_some() || self.from_state.is_some() || self.cards.given() {
                return Err(Error::invalid(
                    "a scenario starts at its own state on its chain's media: drop media, bios, from_state and the cards",
                ));
            }
            if self.verify {
                return Err(Error::invalid(
                    "with a scenario the recording is made by replaying the input, which checks it: drop verify",
                ));
            }
        } else if self.name.is_some() || self.from.is_some() || self.description.is_some() || self.replace {
            return Err(Error::invalid("name, from, description and replace go with a scenario id"));
        }
        Ok(())
    }

    fn start(self, ctx: &Ctx) -> crate::Result<(bool, Value)> {
        let mut a = self;
        if let Some(id) = &a.id {
            // What can fail does so now, not after the person has played.
            crate::scenario::prepare(ctx, id, a.name.as_deref().unwrap(), a.from.as_deref(), a.replace)?;
            expectations(&a)?;
            can_open_window()?;
            return a.open(ctx);
        }
        let resolved =
            crate::context::resolve(&ctx.root, a.media.take(), a.bios.take(), a.from_state.as_deref(), false)?;
        if let Some(e) = resolved.setup_error() {
            return Err(e);
        }
        a.media = resolved.media.map(|p| std::path::absolute(p).unwrap_or_default());
        a.bios = resolved.bios.map(|p| std::path::absolute(p).unwrap_or_default());
        a.from_state = a.from_state.map(|p| std::path::absolute(p).unwrap_or_default());
        // A bad condition fails now, not after the person has played.
        expectations(&a)?;
        can_open_window()?;
        a.open(ctx)
    }

    /// Write the request to its run directory and open the window, here or detached.
    fn open(mut self, ctx: &Ctx) -> crate::Result<(bool, Value)> {
        let a = &mut self;
        let dir = a.out.clone().unwrap_or_else(|| {
            ctx.root.join(".psx/handoffs").join(format!(
                "{}-{}{}",
                crate::now_ms(),
                std::process::id(),
                crate::instance_suffix()
            ))
        });
        let dir = std::path::absolute(&dir).unwrap_or(dir);
        std::fs::create_dir_all(&dir).map_err(|e| Error::io(&dir, e))?;
        a.out = Some(dir.clone());
        let request = dir.join("request.json");
        std::fs::write(&request, serde_json::to_vec_pretty(&a).unwrap()).map_err(|e| Error::io(&request, e))?;
        if a.detach {
            return detach(ctx, &dir);
        }
        Ok(run(ctx, self, &dir))
    }
}

/// `context` capabilities: whether a handoff window can open here.
pub fn capability() -> Value {
    match can_open_window() {
        Ok(()) => json!({"available": true, "gamepads": true}),
        Err(e) => json!({"available": false, "reason": e.message}),
    }
}

/// Fail early, with advice, when no window can be shown.
fn can_open_window() -> crate::Result<()> {
    if !cfg!(feature = "window") {
        return Err(Error::new(
            "unsupported",
            "this psx was built without the `window` feature (cargo build --features window)",
        ));
    }
    if cfg!(all(unix, not(target_os = "macos")))
        && std::env::var_os("DISPLAY").is_none_or(|v| v.is_empty())
        && std::env::var_os("WAYLAND_DISPLAY").is_none_or(|v| v.is_empty())
    {
        return Err(Error::new(
            "no_display",
            "no DISPLAY or WAYLAND_DISPLAY: the handoff window needs a desktop session on the machine running psx (start the psx MCP server from one, or set DISPLAY)",
        ));
    }
    Ok(())
}

/// Start the window in a child process and return a ticket.
fn detach(ctx: &Ctx, dir: &Path) -> crate::Result<(bool, Value)> {
    let exe =
        std::env::current_exe().map_err(|e| Error::new("io_error", format!("cannot find the psx executable: {e}")))?;
    let log_path = dir.join("launch.log");
    let log = std::fs::File::create(&log_path).map_err(|e| Error::io(&log_path, e))?;
    let child = std::process::Command::new(exe)
        .arg("--root")
        .arg(&ctx.root)
        .arg("__handoff-child")
        .arg(dir)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(log)
        .spawn();
    match child {
        Ok(c) => Ok((
            true,
            json!({
                "status": "waiting", "run_dir": dir, "pid": c.id(),
                "note": "A window is open for the person. Poll scenario_play with status (wait up to 600 s per call) until status is no longer waiting; the person may take several minutes.",
                "next_call": {"tool": "scenario_play", "arguments": {"status": dir, "wait": 300}},
            }),
        )),
        Err(e) => Err(Error::new("io_error", format!("could not start the handoff window: {e}"))),
    }
}

/// `__handoff-child <run_dir>`: the detached window process.
pub fn child(ctx: &Ctx, dir: &Path) -> Outcome {
    let a: HandoffArgs = match std::fs::read(dir.join("request.json"))
        .map_err(|e| e.to_string())
        .and_then(|b| serde_json::from_slice(&b).map_err(|e| e.to_string()))
    {
        Ok(a) => a,
        Err(e) => return Error::invalid(format!("{}: {e}", dir.display())).into(),
    };
    Outcome::report(Ok(run(ctx, a, dir)))
}

impl Op for HandoffStatusArgs {
    fn exec(self, ctx: &Ctx) -> Outcome {
        // A handoff from before the project moved names the old folder.
        let run_dir = crate::moved::find(&ctx.root, &self.run_dir);
        Outcome::report(status(&run_dir, self.wait.min(600)).map(|(ok, mut v)| {
            crate::moved::relocate(&ctx.root, &mut v);
            (ok, v)
        }))
    }
}

fn status(dir: &Path, wait: u64) -> crate::Result<(bool, Value)> {
    let dir = &std::path::absolute(dir).unwrap_or_else(|_| dir.to_path_buf());
    if !dir.join("request.json").is_file() {
        return Err(Error::invalid(format!("{} is not a handoff directory (no request.json)", dir.display())));
    }
    let deadline = std::time::Instant::now() + Duration::from_secs(wait);
    loop {
        if let Some(report) = read_json(&dir.join("report.json")) {
            let ok = report.get("error").is_none();
            return Ok((ok, report));
        }
        let live = read_json(&dir.join("live.json"));
        let age_ms = |v: &Option<Value>| {
            v.as_ref().and_then(|v| v["updated_unix_ms"].as_u64()).map(|t| crate::now_ms().saturating_sub(t as u128))
        };
        let started_ms = std::fs::metadata(dir.join("request.json"))
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.elapsed().ok())
            .map(|d| d.as_millis());
        let stale = match age_ms(&live) {
            Some(age) => age > STALE_S * 1000,
            None => started_ms.is_none_or(|s| s > 60_000),
        };
        if stale {
            let log = std::fs::read_to_string(dir.join("launch.log")).unwrap_or_default();
            let tail: String =
                log.lines().rev().take(20).collect::<Vec<_>>().into_iter().rev().collect::<Vec<_>>().join("\n");
            return Ok((
                false,
                json!({"status": "abandoned", "run_dir": dir, "live": live,
                    "message": "the handoff window stopped without a result (crashed, killed, or never started)", "launch_log": tail}),
            ));
        }
        if std::time::Instant::now() >= deadline {
            return Ok((
                true,
                json!({"status": "waiting", "run_dir": dir, "live": live,
                "next_call": {"tool": "scenario_play", "arguments": {"status": dir, "wait": 300}}}),
            ));
        }
        std::thread::sleep(Duration::from_millis(500));
    }
}

fn read_json(p: &Path) -> Option<Value> {
    serde_json::from_slice(&std::fs::read(p).ok()?).ok()
}

/// The emulated game and everything the person pressed.
#[cfg_attr(not(feature = "window"), allow(dead_code))]
pub(crate) struct Session {
    pub m: Machine,
    bios: Vec<u8>,
    /// Absolute frame the handoff started at; run frames count from here.
    start_frame: u64,
    /// Complete pad states, one per change, at run frames.
    pub events: Vec<Input>,
    /// The quick save and the input recorded up to it.
    quick: Option<(Vec<u8>, Vec<Input>)>,
    /// The machine at the handoff start, for starting over.
    start: Vec<u8>,
    start_ram: Vec<u8>,
    media_id: Option<MediaId>,
    pub marks: Vec<Mark>,
    /// States every [`REWIND_EVERY`] frames along the current route, with
    /// the number of input events that reached each, for [`Session::rewind`].
    history: std::collections::VecDeque<(u64, Vec<u8>, usize)>,
}

/// Frames between rewind states (about 5 s), and how many are kept (2 minutes).
const REWIND_EVERY: u64 = 300;
const REWIND_KEEP: usize = 24;
/// A rewind goes back at least this far (1 s), so a press just after a
/// state was taken still undoes something.
const REWIND_MIN: u64 = 60;

/// A moment the person marked, written to `marks/<n>/` at once.
#[cfg_attr(not(feature = "window"), allow(dead_code))]
pub(crate) struct Mark {
    pub n: usize,
    pub label: String,
    /// Run frame and absolute frame.
    rel: u64,
    frame: u64,
    /// The input recorded up to the mark: whether the mark is on the route
    /// play ended on is whether this is that route's input up to it.
    events: Vec<Input>,
    dir: PathBuf,
}

impl Mark {
    /// Whether `events` (a route's input) reaches this mark's state at its frame.
    fn on(&self, events: &[Input], rel: u64) -> bool {
        let upto = |e: &[Input]| e.iter().take_while(|e| e.frame < self.rel).count();
        rel >= self.rel && upto(events) == self.events.len() && events[..self.events.len()] == self.events[..]
    }

    fn json(&self) -> Value {
        json!({"n": self.n, "label": self.label, "run_frame": self.rel, "frame": self.frame, "dir": self.dir})
    }
}

#[cfg_attr(not(feature = "window"), allow(dead_code))]
impl Session {
    pub fn rel(&self) -> u64 {
        self.m.bus.frames - self.start_frame
    }

    /// Whether the game has switched a DualShock into analog mode.
    pub fn analog(&self) -> bool {
        self.m.bus.sio0.pad.kind == psx_core::pad::PadKind::DualShock && self.m.bus.sio0.pad.analog
    }

    pub fn dualshock(&self) -> bool {
        self.m.bus.sio0.pad.kind == psx_core::pad::PadKind::DualShock
    }

    /// Run one frame with this pad state, recording it when it changed.
    /// Input applies at the frame boundary, exactly as `run --replay` applies it.
    pub fn frame(&mut self, buttons: u16, sticks: [u8; 4]) {
        let rel = self.rel();
        let input = Input { frame: rel, buttons, sticks };
        if self.events.last().is_none_or(|l| (l.buttons, l.sticks) != (buttons, sticks)) {
            self.events.push(input);
            crate::replay::apply(&mut self.m, &input);
        }
        let f = self.m.bus.frames;
        while self.m.bus.frames == f {
            self.m.run_frame_steps(1 << 22);
        }
        let rel = self.rel();
        if self.history.back().is_none_or(|h| rel >= h.0 + REWIND_EVERY) {
            if self.history.len() == REWIND_KEEP {
                self.history.pop_front();
            }
            self.history.push_back((rel, self.m.save_state(self.media_id.clone()), self.events.len()));
        }
    }

    /// Back to the latest rewind state at least [`REWIND_MIN`] frames ago:
    /// the frames gone back, or None when there is none.
    pub fn rewind(&mut self) -> Result<Option<u64>, String> {
        let now = self.rel();
        let Some(i) = self.history.iter().rposition(|h| h.0 + REWIND_MIN <= now) else { return Ok(None) };
        self.history.truncate(i + 1);
        let (rel, bytes, n) = self.history[i].clone();
        let events = self.events[..n].to_vec();
        self.restore(&bytes, events)?;
        Ok(Some(now - rel))
    }

    pub fn quick_save(&mut self) {
        self.quick = Some((self.m.save_state(self.media_id.clone()), self.events.clone()));
    }

    /// Back to the quick save, if there is one.
    pub fn quick_load(&mut self) -> Result<bool, String> {
        match self.quick.clone() {
            Some((bytes, events)) => {
                self.history.clear();
                self.restore(&bytes, events).map(|_| true)
            }
            None => Ok(false),
        }
    }

    /// Back to where the handoff started (the quick save is kept).
    pub fn restart(&mut self) -> Result<(), String> {
        let bytes = self.start.clone();
        self.history.clear();
        self.restore(&bytes, Vec::new())
    }

    /// Restore a state of this session with the input that reached it, so the
    /// recording still replays to wherever play continues from here.
    fn restore(&mut self, bytes: &[u8], events: Vec<Input>) -> Result<(), String> {
        let (_, mut m) = Machine::load_state(bytes, self.bios.clone()).map_err(|e| e.to_string())?;
        m.bus.cdrom.disc = self.m.bus.cdrom.disc.take();
        m.bus.spu.capture = self.m.bus.spu.capture;
        self.m = m;
        self.events = events;
        Ok(())
    }

    /// Mark this moment: its state, RAM and screen go to `marks/<n>/` in `dir`.
    pub fn mark(&mut self, dir: &Path) -> Result<usize, String> {
        let n = self.marks.len() + 1;
        let mdir = dir.join("marks").join(n.to_string());
        let mark = Mark {
            n,
            label: String::new(),
            rel: self.rel(),
            frame: self.m.bus.frames,
            events: self.events.clone(),
            dir: mdir.clone(),
        };
        let write =
            |name: &str, bytes: &[u8]| std::fs::write(mdir.join(name), bytes).map_err(|e| format!("{name}: {e}"));
        std::fs::create_dir_all(&mdir).map_err(|e| format!("{}: {e}", mdir.display()))?;
        write("state.bin", &self.m.save_state(self.media_id.clone()))?;
        write("ram.bin", &self.m.bus.ram[..])?;
        self.write_screen(&mdir.join("screen.png")).map_err(|e| format!("screen.png: {e}"))?;
        write("mark.json", mark.json().to_string().as_bytes())?;
        // The input up to the mark: with the handoff's start, how to make its state again.
        write("input.json", replay_json(&self.events).as_bytes())?;
        self.marks.push(mark);
        Ok(n)
    }

    /// Give mark `n` a label.
    pub fn label_mark(&mut self, n: usize, label: &str) -> Result<(), String> {
        let mark = &mut self.marks[n - 1];
        mark.label = label.to_string();
        std::fs::write(mark.dir.join("mark.json"), mark.json().to_string()).map_err(|e| format!("mark.json: {e}"))
    }

    fn write_screen(&self, p: &Path) -> std::io::Result<()> {
        let (w, h, rgba) = self.m.bus.gpu.display_rgba();
        crate::run::write_png(p, w, h, &rgba, png::ColorType::Rgba)
    }

    /// Progress for `scenario play --status`.
    pub fn write_live(&self, dir: &Path, state: &str) {
        let v = json!({"status": state, "run_frame": self.rel(), "frame": self.m.bus.frames,
            "input_events": self.events.len(), "marks": self.marks.iter().map(Mark::json).collect::<Vec<_>>(),
            "pid": std::process::id(), "updated_unix_ms": crate::now_ms() as u64});
        let _ = crate::write_atomic(&dir.join("live.json"), v.to_string().as_bytes());
    }
}

/// How the window was left.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(not(feature = "window"), allow(dead_code))]
pub(crate) enum End {
    /// F12: the person says the request is done.
    Done,
    /// F10: the person could not or would not do it.
    Declined,
    /// The window was closed.
    Closed,
}

impl End {
    fn name(self) -> &'static str {
        match self {
            End::Done => "done",
            End::Declined => "declined",
            End::Closed => "closed",
        }
    }
}

fn start(a: &HandoffArgs) -> crate::Result<Session> {
    let bios_path = a.bios.as_ref().ok_or_else(|| Error::invalid("no BIOS"))?;
    let bios = std::fs::read(bios_path).map_err(|e| Error::io(bios_path, e))?;
    let medium = crate::run_native::open_media(a.media.as_deref())?;
    let m = crate::run_native::boot(
        a.from_state.as_deref(),
        &a.cards,
        bios.clone(),
        medium.exe.as_deref(),
        medium.disc,
        &medium.id,
    )?;
    let start_frame = if a.from_state.is_some() { m.bus.frames } else { 0 };
    Ok(Session::new(m, bios, medium.id, start_frame))
}

impl Session {
    fn new(m: Machine, bios: Vec<u8>, media_id: Option<MediaId>, start_frame: u64) -> Session {
        let start = m.save_state(media_id.clone());
        let start_ram = m.bus.ram.to_vec();
        Session {
            m,
            bios,
            start_frame,
            events: Vec::new(),
            quick: None,
            start,
            start_ram,
            media_id,
            marks: Vec::new(),
            history: Default::default(),
        }
    }
}

/// Fields the first tick may take before a scenario's play gives up.
const LEAD_FIELDS: u64 = 600;
/// Wall-clock limit for reaching a scenario play's start and for recording it.
const RECORD_TIMEOUT: Duration = Duration::from_secs(3600);

/// A scenario play's session: at the scenario's start (or the end of the
/// recording it continues), run on to where a recording's fields count
/// from, so the person's input has `scenario record`'s frame origin.
fn scenario_start(ctx: &Ctx, a: &mut HandoffArgs) -> crate::Result<(Session, crate::scenario::Prepared)> {
    let p = crate::scenario::prepare(
        ctx,
        a.id.as_deref().unwrap(),
        a.name.as_deref().unwrap(),
        a.from.as_deref(),
        a.replace,
    )?;
    let mut b = p.start(ctx, RECORD_TIMEOUT)?;
    if !p.lead_in(&mut b.m, LEAD_FIELDS) {
        return Err(Error::invalid(format!(
            "the glossary binding's tick did not end within {LEAD_FIELDS} fields of the start: check its tick, and that the scenario's state is in the game's logic"
        )));
    }
    // The report's identity, so run --from-state finds the media and BIOS.
    a.media = b.media.clone().map(|p| std::path::absolute(p).unwrap_or_default());
    a.bios = Some(std::path::absolute(&b.bios_path).unwrap_or_default());
    a.from_state = Some(std::path::absolute(&p.state).unwrap_or_default());
    let frames = b.m.bus.frames;
    Ok((Session::new(b.m, b.bios, b.media_id, frames), p))
}

/// Run `f` while keeping `live.json` fresh with `state`, so the status
/// keeps waiting through work that takes a while.
fn alive<T>(dir: &Path, state: &str, f: impl FnOnce() -> T) -> T {
    let stop = std::sync::atomic::AtomicBool::new(false);
    std::thread::scope(|sc| {
        sc.spawn(|| {
            while !stop.load(std::sync::atomic::Ordering::Relaxed) {
                let v = json!({"status": state, "pid": std::process::id(), "updated_unix_ms": crate::now_ms() as u64});
                let _ = crate::write_atomic(&dir.join("live.json"), v.to_string().as_bytes());
                std::thread::sleep(Duration::from_millis(500));
            }
        });
        let r = f();
        stop.store(true, std::sync::atomic::Ordering::Relaxed);
        r
    })
}

/// The play as the scenario's recording: replayed from the same start,
/// recorded as `scenario record` records, and checked to end where the
/// play did.
fn record_play(
    ctx: &Ctx,
    a: &HandoffArgs,
    p: &crate::scenario::Prepared,
    events: &[Input],
    frames: u64,
    end_state: &[u8],
) -> crate::Result<Value> {
    let mut b = p.start(ctx, RECORD_TIMEOUT)?;
    let description = a.description.clone().or_else(|| Some(a.request.clone()));
    let (ok, mut v) = p.write(ctx, &mut b.m, frames as u32, events, description, RECORD_TIMEOUT)?;
    if ok {
        let matches = b.m.save_state(b.media_id.clone()) == end_state;
        v["matches_play"] = json!(matches);
        if !matches {
            v["warning"] = json!(
                "replaying the input did not end where the play did: the recording is the original's run of that input, which differs from what the person saw; scenario check shows whether the scenario is deterministic"
            );
        }
    }
    Ok(v)
}

/// The handoff's report, also written to report.json; a failure is recorded
/// there as an error so `scenario play --status` can return it.
fn run(ctx: &Ctx, mut a: HandoffArgs, dir: &Path) -> (bool, Value) {
    let result = (|| -> crate::Result<Value> {
        let (mut s, scenario) = match a.id {
            Some(_) => alive(dir, "starting", || scenario_start(ctx, &mut a)).map(|(s, p)| (s, Some(p)))?,
            None => (start(&a)?, None),
        };
        let (end, note) = play(&mut s, &a, dir).map_err(|e| Error::new("window_failed", e))?;
        // The person is done; keep the status from calling this abandoned while
        // the files are written (and verify replays, which can take minutes).
        s.write_live(dir, "finishing");
        let (events, frames) = (s.events.clone(), s.rel());
        let state = s.m.save_state(s.media_id.clone());
        let mut v = finish(&a, s, end, note, dir)?;
        if let Some(p) = scenario {
            scenario_report(ctx, &a, &p, &mut v, end, &events, frames, &state, dir);
        }
        Ok(v)
    })();
    let (ok, v) = match result {
        Ok(v) => (true, v),
        Err(e) => (false, e.with("run_dir", dir).to_value()),
    };
    // report.json is what the status returns and what `run --from-state` reads identity from.
    let _ = std::fs::write(dir.join("report.json"), serde_json::to_vec_pretty(&v).unwrap());
    (ok, v)
}

/// A scenario play's recording (when the person finished) and how to make it again.
#[allow(clippy::too_many_arguments)]
fn scenario_report(
    ctx: &Ctx,
    a: &HandoffArgs,
    p: &crate::scenario::Prepared,
    v: &mut Value,
    end: End,
    events: &[Input],
    frames: u64,
    state: &[u8],
    dir: &Path,
) {
    let mut args = json!({"id": p.id, "name": p.name, "frames": frames, "replay": dir.join("input.json"),
        "description": a.description.clone().unwrap_or_else(|| a.request.clone())});
    if let Some(f) = &p.from {
        args["from"] = json!(f);
    }
    v["input"]["frame_origin"] = json!("scenario_record");
    v["input"]["note"] = json!(
        "Complete pad states at fields counted as scenario record counts them: from the field after the recording's first tick, which runs with the pad released."
    );
    v["reproduce"] = json!({"tool": "scenario_record", "arguments": args.clone(),
        "note": "Records the person's input again from the same start, as the recording was made."});
    v["recording"] = if end == End::Done {
        alive(dir, "recording", || record_play(ctx, a, p, events, frames, state)).unwrap_or_else(|e| e.to_value())
    } else {
        args["replace"] = json!(a.replace);
        json!({"written": false, "note": "the person did not finish, so the play is not recorded: ask again, or record what they did with scenario_record and these arguments",
               "arguments": args})
    };
}

#[cfg(feature = "window")]
fn play(s: &mut Session, a: &HandoffArgs, dir: &Path) -> Result<(End, String), String> {
    window::play(s, &a.request, dir)
}

#[cfg(not(feature = "window"))]
fn play(_: &mut Session, _: &HandoffArgs, _: &Path) -> Result<(End, String), String> {
    Err("this psx was built without the `window` feature".into())
}

/// The recording as a `run --replay` file: one event per line.
fn replay_json(events: &[Input]) -> String {
    let lines: Vec<String> = events
        .iter()
        .map(|e| {
            let mut v = json!({"frame": e.frame, "buttons": crate::replay::button_names(e.buttons)});
            if e.sticks != [0x80; 4] {
                v["sticks"] = json!(e.sticks);
            }
            v.to_string()
        })
        .collect();
    format!("[\n{}\n]\n", lines.join(",\n"))
}

fn finish(a: &HandoffArgs, s: Session, end: End, note: String, dir: &Path) -> crate::Result<Value> {
    let frames = s.rel();
    let state = s.m.save_state(s.media_id.clone());
    let mut files = serde_json::Map::new();
    let mut put = |name: &str, bytes: &[u8]| -> crate::Result<()> {
        let p = dir.join(name);
        std::fs::write(&p, bytes).map_err(|e| Error::io(&p, e))?;
        files.insert(name.into(), json!({"path": p, "sha256": psx_media::disc::sha256_bytes(bytes)}));
        Ok(())
    };
    let replay = replay_json(&s.events);
    put("note.txt", note.as_bytes())
        .and_then(|_| put("state.bin", &state))
        .and_then(|_| put("input.json", replay.as_bytes()))
        .and_then(|_| put("ram.bin", &s.m.bus.ram[..]))
        .and_then(|_| {
            for (slot, card) in s.m.bus.sio0.cards.iter().enumerate() {
                if let Some(c) = card.as_ref().filter(|c| c.writes > 0) {
                    put(&format!("memcard{}.mcd", slot + 1), &c.data)?;
                }
            }
            Ok(())
        })?;
    let screen = dir.join("screen.png");
    if s.write_screen(&screen).is_ok() {
        files.insert("screen.png".into(), json!({"path": screen}));
    }
    let mut warnings = Vec::new();
    if s.events.len() > crate::replay::FILE_EVENT_LIMIT {
        warnings.push(format!(
            "{} input events exceed the replay limit ({}); input.json cannot be replayed whole",
            s.events.len(),
            crate::replay::FILE_EVENT_LIMIT
        ));
    }
    let mut replay_args = json!({"replay": dir.join("input.json"), "frames": frames, "save_state": true});
    if let Some(m) = &a.media {
        replay_args["media"] = json!(m);
    }
    match &a.from_state {
        Some(p) => replay_args["from_state"] = json!(p),
        None => {
            for (k, v) in [("memcard1", &a.cards.memcard1), ("memcard2", &a.cards.memcard2), ("pad", &a.cards.pad)] {
                if let Some(v) = v {
                    replay_args[k] = json!(v);
                }
            }
        }
    }
    let verify = if a.verify && end != End::Closed { verify(a, &s.events, frames, &state, dir) } else { Value::Null };
    if verify["matches"] == false {
        warnings.push("replaying input.json did not reproduce state.bin; continue from state.bin, and treat the replay as approximate".into());
    }
    let marks = marks_report(&s, a, frames, dir);
    let expect = expect_report(a, &s.m, end);
    if expect["ask_again"] == true && end == End::Done {
        warnings.push("the end state is not what was asked for (expect)".into());
    }
    let mut continue_args = json!({"from_state": dir.join("state.bin")});
    if let Some(m) = &a.media {
        continue_args["media"] = json!(m);
    }
    let summary = match end {
        End::Done => "The person says the request is done. Continue from state.bin.",
        End::Declined => "The person declined or could not finish; read their note. state.bin is where they stopped.",
        End::Closed => "The window was closed without finishing; state.bin is where it stood.",
    };
    Ok(json!({
            "status": end.name(),
            "summary": summary,
            "request": a.request,
            "note": note,
            "run_dir": dir,
            "frames": {"reached": frames, "start": s.start_frame, "end": s.m.bus.frames, "unit": "vsync"},
            "input": {"recorded_events": s.events.len(), "file": dir.join("input.json"), "frame_origin": "handoff_start",
                "note": "Complete pad states at run frames from the handoff start, in run --replay format."},
            "pad": {"kind": format!("{:?}", s.m.bus.sio0.pad.kind).to_lowercase(), "analog": s.m.bus.sio0.pad.analog, "locked": s.m.bus.sio0.pad.locked,
                "buttons": crate::replay::button_names(s.m.bus.sio0.buttons),
                "sticks": [s.m.bus.sio0.pad.sticks[2], s.m.bus.sio0.pad.sticks[3], s.m.bus.sio0.pad.sticks[0], s.m.bus.sio0.pad.sticks[1]],
                "motors": [s.m.bus.sio0.pad.motors.0, s.m.bus.sio0.pad.motors.1]},
            "files": files,
            "marks": marks,
            "expect": expect,
            "continue": {"tool": "run", "arguments": continue_args},
            "reproduce": {"tool": "run", "arguments": replay_args,
                "note": "Replays the person's input from the handoff start; ends in the same state when emulation is deterministic (verify=true checks)."},
            "verify": verify,
            "warnings": warnings,
            // `run --from-state <this dir>/state.bin` finds the media and BIOS here.
            "identity": {"backend": "psx-core", "backend_version": env!("CARGO_PKG_VERSION"),
                "media": a.media, "bios": a.bios, "bios_sha256": psx_media::disc::sha256_bytes(&s.bios),
                "from_state": a.from_state, "media_identity": s.media_id, "state_format": psx_core::state::FORMAT},
            "report": dir.join("report.json"),
            "build": crate::context::build(),
    }))
}

/// The marks, each with its RAM diffed against the mark before it on its
/// route (or the handoff start).
fn marks_report(s: &Session, a: &HandoffArgs, frames: u64, dir: &Path) -> Value {
    if s.marks.is_empty() {
        return json!({"count": 0, "list": []});
    }
    let start_ram = dir.join("marks/start/ram.bin");
    let wrote_start =
        std::fs::create_dir_all(dir.join("marks/start")).and_then(|_| std::fs::write(&start_ram, &s.start_ram)).is_ok();
    let list: Vec<Value> = s
        .marks
        .iter()
        .map(|m| {
            let before = s
                .marks
                .iter()
                .filter(|p| (p.rel, p.n) < (m.rel, m.n) && p.on(&m.events, m.rel))
                .max_by_key(|p| (p.rel, p.n));
            let against = before.map_or(start_ram.clone(), |p| p.dir.join("ram.bin"));
            let opts =
                crate::ram::DiffOpts { width: 4, range: None, min_delta: None, signed: false, limit: 12, offset: 0 };
            let diff = match crate::ram::diff(&against, &m.dir.join("ram.bin"), opts) {
                Ok(d) if before.is_some() || wrote_start => json!({
                    "against": before.map_or(json!("start"), |p| json!(p.n)),
                    "differing_words": d["differing"],
                    "clusters": d["clusters"],
                    "changes": d["changes"],
                }),
                Ok(_) => Value::Null,
                Err(e) => e.to_value(),
            };
            let mut continue_args = json!({"from_state": m.dir.join("state.bin")});
            if let Some(media) = &a.media {
                continue_args["media"] = json!(media);
            }
            let mut v = m.json();
            v["on_route"] = json!(m.on(&s.events, frames));
            v["files"] = json!({"state": m.dir.join("state.bin"), "ram": m.dir.join("ram.bin"),
                "screen": m.dir.join("screen.png")});
            v["ram_diff"] = diff;
            v["continue"] = json!({"tool": "run", "arguments": continue_args});
            v
        })
        .collect();
    // Snapshots along the route in order, start to end, for ram_search.
    let mut on: Vec<&Mark> = s.marks.iter().filter(|m| m.on(&s.events, frames)).collect();
    on.sort_by_key(|m| (m.rel, m.n));
    let mut route = vec![json!(dir.join("marks/start"))];
    route.extend(on.iter().map(|m| json!(m.dir)));
    route.push(json!(dir));
    json!({
        "count": s.marks.len(),
        "list": list,
        "route_snapshots": route,
        "note": "Each mark dir holds ram.bin, so ram_diff and ram_search take mark dirs as snapshots. A mark off the route was made before a quick load or start over took play elsewhere; its files still hold that moment. ram_diff lists the first 12 changed words; call ram_diff on the dirs for more.",
    })
}

/// Replay the recording headless from the same start and compare the end state.
/// Keeps `live.json` in `dir` fresh ("verifying") so the status keeps waiting.
fn verify(a: &HandoffArgs, events: &[Input], frames: u64, expected: &[u8], dir: &Path) -> Value {
    let started = std::time::Instant::now();
    let mut s = match start(a) {
        Ok(s) => s,
        Err(e) => return e.to_value(),
    };
    let mut next = 0;
    let mut live_at = started;
    while s.rel() < frames {
        if live_at.elapsed() >= Duration::from_secs(1) {
            live_at = std::time::Instant::now();
            s.write_live(dir, "verifying");
        }
        while next < events.len() && events[next].frame <= s.rel() {
            crate::replay::apply(&mut s.m, &events[next]);
            next += 1;
        }
        s.m.run_frame_steps(1 << 22);
    }
    let state = s.m.save_state(s.media_id.clone());
    json!({"matches": state == expected, "ram_matches": s.m.bus.ram[..] == expected_ram(expected, &s.bios).unwrap_or_default()[..],
        "frames": frames, "wall_ms": started.elapsed().as_millis() as u64})
}

fn expected_ram(state: &[u8], bios: &[u8]) -> Option<Vec<u8>> {
    Machine::load_state(state, bios.to_vec()).ok().map(|(_, m)| m.bus.ram.to_vec())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replay_file_is_valid_run_input() {
        let events = [
            Input { frame: 0, buttons: 0, sticks: [0x80; 4] },
            Input { frame: 3, buttons: psx_core::sio0::button::CROSS, sticks: [0x80; 4] },
            Input { frame: 9, buttons: 0, sticks: [0, 0x80, 0x80, 0x80] },
        ];
        let text = replay_json(&events);
        let parsed: crate::replay::Events = text.parse().unwrap();
        assert_eq!(parsed.0.len(), 3);
        let dir = std::env::temp_dir().join(format!("psx-handoff-replay-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("input.json");
        std::fs::write(&p, text).unwrap();
        assert_eq!(crate::replay::resolve(Some(&p), None, 10).unwrap(), events);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn marks_are_on_the_route_whose_input_reaches_them() {
        let ev = |frame, buttons| Input { frame, buttons, sticks: [0x80; 4] };
        let mark =
            |rel, events: Vec<Input>| Mark { n: 1, label: String::new(), rel, frame: rel, events, dir: PathBuf::new() };
        let route = [ev(0, 0), ev(10, 1), ev(20, 0), ev(30, 2)];
        // Made at frame 25 with the route's input so far; later input does not matter.
        assert!(mark(25, route[..3].to_vec()).on(&route, 40));
        assert!(mark(20, route[..2].to_vec()).on(&route, 40), "input at the mark's frame comes after it");
        // Play ended before it, or reached its frame by other input.
        assert!(!mark(25, route[..3].to_vec()).on(&route, 24));
        assert!(!mark(25, vec![ev(0, 0), ev(10, 4)]).on(&route, 40));
        assert!(!mark(25, route[..2].to_vec()).on(&route, 40));
    }

    #[test]
    fn expect_conditions_are_read_before_the_window_opens() {
        let args = |expect: &str| -> HandoffArgs {
            serde_json::from_value(json!({"request": "x", "expect": [expect]})).unwrap()
        };
        assert_eq!(expectations(&args("80010000:u8=3")).unwrap().len(), 1);
        let e = expectations(&args("stage=3")).unwrap_err();
        assert!(e.message.starts_with("--expect") && e.message.contains("stage"), "{}", e.message);
    }

    #[test]
    fn status_reports_waiting_then_abandoned_then_the_report() {
        let dir = std::env::temp_dir().join(format!("psx-handoff-status-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        assert_eq!(status(&dir, 0).unwrap_err().code, "invalid_arguments");
        std::fs::write(dir.join("request.json"), "{}").unwrap();
        std::fs::write(dir.join("live.json"), json!({"updated_unix_ms": crate::now_ms() as u64}).to_string()).unwrap();
        assert_eq!(status(&dir, 0).unwrap().1["status"], "waiting");
        std::fs::write(dir.join("live.json"), json!({"updated_unix_ms": 0}).to_string()).unwrap();
        assert_eq!(status(&dir, 0).unwrap().1["status"], "abandoned");
        std::fs::write(dir.join("report.json"), json!({"status": "done"}).to_string()).unwrap();
        assert_eq!(status(&dir, 0).unwrap().1["status"], "done");
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// Plays a scenario headless the way the window does and records it: the
    /// recording ends where the play did, and is the one scenario record makes
    /// from the play's input.json. Needs a port workspace with an inventory:
    /// PSX_TEST_WORKSPACE=DIR PSX_TEST_SCENARIO=ID cargo test -p psx --release scenario_play -- --ignored
    #[test]
    #[ignore]
    fn scenario_play_records_what_was_played() {
        let (Some(root), Some(id)) = (std::env::var_os("PSX_TEST_WORKSPACE"), std::env::var("PSX_TEST_SCENARIO").ok())
        else {
            return;
        };
        let ctx = Ctx { root: PathBuf::from(root), cancel: None };
        let dir = std::env::temp_dir().join(format!("psx-scenario-play-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut a: HandoffArgs =
            serde_json::from_value(json!({"request": "walk up", "id": id, "name": "play_test", "replace": true}))
                .unwrap();
        let (mut s, p) = scenario_start(&ctx, &mut a).unwrap();
        let up = psx_core::sio0::button::UP;
        let press = |s: &mut Session, frames: u64, buttons: u16| (0..frames).for_each(|_| s.frame(buttons, [0x80; 4]));
        press(&mut s, 3, up);
        s.quick_save();
        press(&mut s, 20, 0);
        s.quick_load().unwrap();
        press(&mut s, 40, up);
        press(&mut s, 10, 0);
        let (events, frames) = (s.events.clone(), s.rel());
        let state = s.m.save_state(s.media_id.clone());
        let mut v = finish(&a, s, End::Done, String::new(), &dir).unwrap();
        scenario_report(&ctx, &a, &p, &mut v, End::Done, &events, frames, &state, &dir);
        assert_eq!(v["recording"]["matches_play"], true, "{}", v["recording"]);
        let at = |name: &str| ctx.root.join("scenarios").join(&id).join(name);
        let read = |name: &str| {
            let r = psx_scenario::Recording::parse(&std::fs::read_to_string(at(name)).unwrap()).unwrap();
            (r.header.initial, r.rows)
        };
        // scenario record from the play's input: the same frame origin, so the same recording.
        let record = |args: Value| {
            let a: crate::scenario::ScenarioRecordArgs = serde_json::from_value(args).unwrap();
            a.exec(&ctx).value
        };
        let mut args = v["reproduce"]["arguments"].clone();
        args["name"] = json!("play_test_again");
        args["replace"] = json!(true);
        assert_eq!(record(args)["status"], "recorded");
        assert_eq!(read("play_test.jsonl"), read("play_test_again.jsonl"));
        // A recording continuing from the play's end.
        let r = record(json!({"id": id, "name": "play_test_on", "from": "play_test", "frames": 20, "replace": true}));
        assert_eq!((r["status"].clone(), r["from"].clone()), (json!("recorded"), json!("play_test")));
        for n in ["play_test", "play_test_again", "play_test_on"] {
            std::fs::remove_file(at(&format!("{n}.jsonl"))).unwrap();
            std::fs::remove_file(at(&format!("{n}.coverage.json"))).unwrap();
        }
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// Plays a real disc headless the way the window does: input, a quick save
    /// and load, starting over, then hand-back with verify. Needs a disc:
    /// PSX_TEST_DISC=$PWD/media/game.cue cargo test -p psx --release handoff_session -- --ignored
    #[test]
    #[ignore]
    fn handoff_session_replays_to_the_same_state() {
        let Some(disc) = std::env::var_os("PSX_TEST_DISC") else { return };
        let root = std::env::current_dir().unwrap().join("../..");
        let r = crate::context::resolve(&root, Some(disc.into()), None, None, false).unwrap();
        let a = HandoffArgs {
            request: "test".into(),
            id: None,
            name: None,
            from: None,
            description: None,
            replace: false,
            media: r.media,
            bios: r.bios,
            from_state: None,
            cards: Default::default(),
            out: None,
            verify: true,
            detach: false,
            expect: vec!["80000000:u32>=0".into(), "80000000:u32<0".into()],
            status: None,
            wait: 0,
        };
        let mut s = start(&a).unwrap();
        let cross = psx_core::sio0::button::CROSS;
        let start_btn = psx_core::sio0::button::START;
        let press = |s: &mut Session, frames: u64, buttons: u16| (0..frames).for_each(|_| s.frame(buttons, [0x80; 4]));
        press(&mut s, 300, 0);
        press(&mut s, 5, start_btn);
        press(&mut s, 100, 0);
        let dir = std::env::temp_dir().join(format!("psx-handoff-session-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        assert_eq!(s.mark(&dir).unwrap(), 1);
        s.label_mark(1, "before the save").unwrap();
        s.quick_save();
        let saved_at = s.rel();
        press(&mut s, 10, cross);
        press(&mut s, 50, 0);
        assert!(s.quick_load().unwrap());
        assert_eq!(s.rel(), saved_at);
        assert!(s.events.iter().all(|e| e.frame < saved_at), "input after the quick save is dropped");
        press(&mut s, 7, cross);
        press(&mut s, 60, 0);
        s.mark(&dir).unwrap();
        s.restart().unwrap();
        assert_eq!((s.rel(), s.events.len()), (0, 0));
        press(&mut s, 200, 0);
        press(&mut s, 3, start_btn);
        press(&mut s, 120, cross);
        press(&mut s, 30, 0);
        // Back to the save made on the first route: its input comes back too.
        s.quick_load().unwrap();
        press(&mut s, 40, cross);
        s.mark(&dir).unwrap();
        // Rewind past input: it is dropped, and play goes on from there.
        press(&mut s, 330, 0);
        press(&mut s, 20, start_btn);
        let (at, events) = (s.rel(), s.events.len());
        let back = s.rewind().unwrap().unwrap();
        assert!(back >= REWIND_MIN && s.events.len() < events, "{back} {}", s.events.len());
        assert_eq!(s.rel(), at - back);
        press(&mut s, 30, cross);
        let v = finish(&a, s, End::Done, "note".into(), &dir).unwrap();
        assert_eq!(v["verify"]["matches"], true, "{}", v["verify"]);
        assert_eq!(v["status"], "done");
        // One condition holds and one cannot: done, but ask again.
        assert_eq!((v["expect"]["met"].clone(), v["expect"]["ask_again"].clone()), (false.into(), true.into()));
        assert_eq!(v["expect"]["conditions"][0]["holds"], true);
        assert!(v["warnings"].to_string().contains("not what was asked"));
        let marks = &v["marks"]["list"];
        let on: Vec<bool> = (0..3).map(|i| marks[i]["on_route"].as_bool().unwrap()).collect();
        assert_eq!(on, [true, false, true]);
        assert_eq!(marks[0]["label"], "before the save");
        assert_eq!(marks[0]["ram_diff"]["against"], "start");
        assert_eq!(marks[2]["ram_diff"]["against"], 1);
        assert_eq!(v["marks"]["route_snapshots"].as_array().unwrap().len(), 4);
        std::fs::remove_dir_all(dir).unwrap();
    }
}
