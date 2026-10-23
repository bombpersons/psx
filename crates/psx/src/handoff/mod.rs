//! `psx handoff`: an agent asks a person to play the game to a point it cannot
//! easily reach itself. A window shows the game and the agent's request; the
//! person plays with a keyboard or gamepad and presses F12 when it is done.
//! The run directory then holds what the agent continues from:
//!
//! - `state.bin`: the machine at the end (continue with `run --from-state`);
//! - `input.json`: everything the person pressed, as a `run --replay` file
//!   that starts where the handoff started, so the route can be replayed;
//! - `screen.png`, `note.txt` (the person's note), and `report.json` (the result).
//!
//! The window runs in the calling process, or with `--detach` (always over MCP)
//! in a child process: the call returns a ticket at once and `handoff_status`
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
    #[arg(long)]
    pub request: String,
    /// Disc image or PS-X EXE, as for run.
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
    /// `handoff-status`. Over MCP a handoff always detaches.
    #[arg(long)]
    #[serde(default)]
    pub detach: bool,
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

impl Op for HandoffArgs {
    fn exec(self, ctx: &Ctx) -> Outcome {
        Outcome::report(self.start(ctx))
    }
}

impl HandoffArgs {
    fn start(self, ctx: &Ctx) -> crate::Result<(bool, Value)> {
        let mut a = self;
        let resolved =
            crate::context::resolve(&ctx.root, a.media.take(), a.bios.take(), a.from_state.as_deref(), false)?;
        if let Some(e) = resolved.setup_error() {
            return Err(e);
        }
        a.media = resolved.media.map(|p| std::path::absolute(p).unwrap_or_default());
        a.bios = resolved.bios.map(|p| std::path::absolute(p).unwrap_or_default());
        a.from_state = a.from_state.map(|p| std::path::absolute(p).unwrap_or_default());
        can_open_window()?;
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
        Ok(run(a, &dir))
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
                "note": "A window is open for the person. Poll handoff_status (wait up to 600 s per call) until status is no longer waiting; the person may take several minutes.",
                "next_call": {"tool": "handoff_status", "arguments": {"run_dir": dir, "wait": 300}},
            }),
        )),
        Err(e) => Err(Error::new("io_error", format!("could not start the handoff window: {e}"))),
    }
}

/// `__handoff-child <run_dir>`: the detached window process.
pub fn child(dir: &Path) -> Outcome {
    let a: HandoffArgs = match std::fs::read(dir.join("request.json"))
        .map_err(|e| e.to_string())
        .and_then(|b| serde_json::from_slice(&b).map_err(|e| e.to_string()))
    {
        Ok(a) => a,
        Err(e) => return Error::invalid(format!("{}: {e}", dir.display())).into(),
    };
    Outcome::report(Ok(run(a, dir)))
}

impl Op for HandoffStatusArgs {
    fn exec(self, _ctx: &Ctx) -> Outcome {
        Outcome::report(status(&self.run_dir, self.wait.min(600)))
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
                "next_call": {"tool": "handoff_status", "arguments": {"run_dir": dir, "wait": 300}}}),
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
    quick: Option<Vec<u8>>,
    /// The machine at the handoff start, for starting over.
    start: Vec<u8>,
    media_id: Option<MediaId>,
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
    }

    pub fn quick_save(&mut self) {
        self.quick = Some(self.m.save_state(self.media_id.clone()));
    }

    /// Back to the quick save, if there is one.
    pub fn quick_load(&mut self) -> Result<bool, String> {
        match self.quick.clone() {
            Some(bytes) => self.restore(&bytes).map(|_| true),
            None => Ok(false),
        }
    }

    /// Back to where the handoff started (the quick save is kept).
    pub fn restart(&mut self) -> Result<(), String> {
        let bytes = self.start.clone();
        self.restore(&bytes)
    }

    /// Restore a state of this session. Input recorded after it is dropped, so
    /// the recording still replays to wherever play continues from here.
    fn restore(&mut self, bytes: &[u8]) -> Result<(), String> {
        let (_, mut m) = Machine::load_state(bytes, self.bios.clone()).map_err(|e| e.to_string())?;
        m.bus.cdrom.disc = self.m.bus.cdrom.disc.take();
        self.m = m;
        let rel = self.rel();
        self.events.retain(|e| e.frame < rel);
        Ok(())
    }

    /// Progress for `handoff_status`.
    pub fn write_live(&self, dir: &Path, state: &str) {
        let v = json!({"status": state, "run_frame": self.rel(), "frame": self.m.bus.frames,
            "input_events": self.events.len(), "pid": std::process::id(), "updated_unix_ms": crate::now_ms() as u64});
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
    let start = m.save_state(medium.id.clone());
    Ok(Session { m, bios, start_frame, events: Vec::new(), quick: None, start, media_id: medium.id })
}

/// The handoff's report, also written to report.json; a failure is recorded
/// there as an error so `handoff_status` can return it.
fn run(a: HandoffArgs, dir: &Path) -> (bool, Value) {
    let result = start(&a).and_then(|mut s| {
        let (end, note) = play(&mut s, &a, dir).map_err(|e| Error::new("window_failed", e))?;
        // The person is done; keep handoff_status from calling this abandoned while
        // the files are written (and verify replays, which can take minutes).
        s.write_live(dir, "finishing");
        finish(&a, s, end, note, dir)
    });
    let (ok, v) = match result {
        Ok(v) => (true, v),
        Err(e) => (false, e.with("run_dir", dir).to_value()),
    };
    // report.json is what handoff_status returns and what `run --from-state` reads identity from.
    let _ = std::fs::write(dir.join("report.json"), serde_json::to_vec_pretty(&v).unwrap());
    (ok, v)
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
        .and_then(|_| {
            for (slot, card) in s.m.bus.sio0.cards.iter().enumerate() {
                if let Some(c) = card.as_ref().filter(|c| c.writes > 0) {
                    put(&format!("memcard{}.mcd", slot + 1), &c.data)?;
                }
            }
            Ok(())
        })?;
    let (w, h, rgba) = s.m.bus.gpu.display_rgba();
    let screen = dir.join("screen.png");
    if crate::run::write_png(&screen, w, h, &rgba, png::ColorType::Rgba).is_ok() {
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

/// Replay the recording headless from the same start and compare the end state.
/// Keeps `live.json` in `dir` fresh ("verifying") so handoff_status keeps waiting.
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

    /// Plays a real disc headless the way the window does: input, a quick save
    /// and load, starting over, then hand-back with verify. Needs a disc:
    /// PSX_TEST_DISC=media/game.cue cargo test -p psx --release handoff_session -- --ignored
    #[test]
    #[ignore]
    fn handoff_session_replays_to_the_same_state() {
        let Some(disc) = std::env::var_os("PSX_TEST_DISC") else { return };
        let root = std::env::current_dir().unwrap().join("../..");
        let r = crate::context::resolve(&root, Some(disc.into()), None, None, false).unwrap();
        let a = HandoffArgs {
            request: "test".into(),
            media: r.media,
            bios: r.bios,
            from_state: None,
            cards: Default::default(),
            out: None,
            verify: true,
            detach: false,
        };
        let mut s = start(&a).unwrap();
        let cross = psx_core::sio0::button::CROSS;
        let start_btn = psx_core::sio0::button::START;
        let press = |s: &mut Session, frames: u64, buttons: u16| (0..frames).for_each(|_| s.frame(buttons, [0x80; 4]));
        press(&mut s, 300, 0);
        press(&mut s, 5, start_btn);
        press(&mut s, 100, 0);
        s.quick_save();
        let saved_at = s.rel();
        press(&mut s, 10, cross);
        press(&mut s, 50, 0);
        assert!(s.quick_load().unwrap());
        assert_eq!(s.rel(), saved_at);
        assert!(s.events.iter().all(|e| e.frame < saved_at), "input after the quick save is dropped");
        press(&mut s, 7, cross);
        press(&mut s, 60, 0);
        s.restart().unwrap();
        assert_eq!((s.rel(), s.events.len()), (0, 0));
        press(&mut s, 200, 0);
        press(&mut s, 3, start_btn);
        press(&mut s, 120, cross);
        press(&mut s, 30, 0);
        s.quick_load().unwrap();
        press(&mut s, 40, cross);
        let dir = std::env::temp_dir().join(format!("psx-handoff-session-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let v = finish(&a, s, End::Done, "note".into(), &dir).unwrap();
        assert_eq!(v["verify"]["matches"], true, "{}", v["verify"]);
        assert_eq!(v["status"], "done");
        std::fs::remove_dir_all(dir).unwrap();
    }
}
