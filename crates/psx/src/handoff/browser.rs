//! Playing a queued request in the browser (`psx web`): the session runs on a
//! thread of the server, the page reads its frames from [`Live::stream`] and
//! posts the person's input and commands. Done, decline, marks, rewind and
//! the quick save are the window's, and as there the game waits while the
//! person writes the note or labels a mark; leaving (or no page watching for
//! [`IDLE`]) gives the request back to the queue.

use std::io::Write;
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use super::bindings::{self, Bindings};
use super::{End, Session};
use crate::Error;
use crate::commands::Ctx;

/// How long a session waits for a page to come back before the request goes
/// back to the queue.
const IDLE: Duration = Duration::from_secs(60);

/// Fields run per tick while fast forward is held.
const FAST: usize = 4;

/// What the page holds down: key names as the window names them, gamepad
/// buttons as gilrs names them, sticks -1..1 with +y up.
#[derive(Clone, Default)]
struct Held {
    keys: Vec<String>,
    pad: Vec<String>,
    sticks: [f32; 4],
    fast: bool,
    /// Pressed since the last field, so a tap shorter than one still counts.
    tapped: (Vec<String>, Vec<String>),
}

enum Cmd {
    Done(String),
    Decline(String),
    Leave,
    Mark,
    Label(usize, String),
    Discard(usize),
    Rewind,
    QuickSave,
    QuickLoad,
    Restart,
    Pause(bool),
    /// Writing the note before handing back: the game waits, as in the window.
    Writing(bool),
}

#[derive(Default)]
struct Out {
    frame_seq: u64,
    w: u32,
    h: u32,
    rgba: Vec<u8>,
    status_seq: u64,
    status: Value,
    ended: bool,
}

/// A request being played in the browser.
pub struct Live {
    held: Mutex<Held>,
    cmds: Mutex<Vec<Cmd>>,
    out: Mutex<Out>,
    cv: Condvar,
    /// Pages reading the stream, and when the last one left.
    clients: AtomicUsize,
    left: Mutex<Instant>,
}

impl Live {
    /// Claim the queued request in `dir` and start playing it on a thread.
    pub fn start(root: &Path, dir: &Path) -> crate::Result<Arc<Live>> {
        let (a, text) = super::claim(dir, "web")?;
        let live = Arc::new(Live {
            held: Mutex::default(),
            cmds: Mutex::default(),
            out: Mutex::new(Out { status: json!({"state": "starting", "request": text}), ..Default::default() }),
            cv: Condvar::new(),
            clients: AtomicUsize::new(0),
            left: Mutex::new(Instant::now()),
        });
        let (l, root, dir) = (live.clone(), root.to_path_buf(), dir.to_path_buf());
        std::thread::Builder::new()
            .name("psx-web-play".into())
            .spawn(move || {
                let ctx = Ctx { root, cancel: None };
                let person = a.person;
                let (_, v) = super::run(&ctx, a, &text, &dir, |s, text, dir| play(s, text, dir, &l));
                // The person's own play left without an answer is gone, not queued for someone.
                if person && v["status"] == "queued" {
                    let _ = std::fs::remove_dir_all(&dir);
                }
                let mut o = l.out.lock().unwrap();
                o.ended = true;
                o.status = json!({"state": "ended", "result": v});
                o.status_seq += 1;
                l.cv.notify_all();
            })
            .map_err(|e| Error::new("io_error", format!("could not start the play: {e}")))?;
        Ok(live)
    }

    pub fn ended(&self) -> bool {
        self.out.lock().unwrap().ended
    }

    /// The page's held input, replacing what it sent before.
    pub fn input(&self, v: &Value) {
        let names = |k: &str| -> Vec<String> {
            v[k].as_array().map(|a| a.iter().filter_map(|s| s.as_str().map(String::from)).collect()).unwrap_or_default()
        };
        let mut sticks = [0.0f32; 4];
        for (i, s) in sticks.iter_mut().enumerate() {
            *s = v["sticks"][i].as_f64().unwrap_or(0.0).clamp(-1.0, 1.0) as f32;
        }
        let (keys, pad) = (names("keys"), names("pad"));
        let mut h = self.held.lock().unwrap();
        let new =
            |now: &[String], before: &[String]| now.iter().filter(|k| !before.contains(k)).cloned().collect::<Vec<_>>();
        let (tk, tp) = (new(&keys, &h.keys), new(&pad, &h.pad));
        h.tapped.0.extend(tk);
        h.tapped.1.extend(tp);
        (h.keys, h.pad, h.sticks, h.fast) = (keys, pad, sticks, v["fast"] == true);
    }

    /// A command from the page: done, decline (each with a note), leave,
    /// mark, label (n, label), discard (n: a mark), rewind, quick_save, quick_load, restart, pause,
    /// writing (on: the person is writing the note; the game waits).
    pub fn command(&self, v: &Value) -> crate::Result<Value> {
        if self.ended() {
            return Err(Error::new("ended", "this play has ended"));
        }
        let note = || v["note"].as_str().unwrap_or("").trim().to_string();
        let cmd = match v["cmd"].as_str().unwrap_or("") {
            "done" => Cmd::Done(note()),
            "decline" if note().is_empty() => {
                return Err(Error::invalid("a decline needs a note: it is what the agent gets"));
            }
            "decline" => Cmd::Decline(note()),
            "leave" => Cmd::Leave,
            "mark" => Cmd::Mark,
            "label" => Cmd::Label(v["n"].as_u64().unwrap_or(0) as usize, v["label"].as_str().unwrap_or("").to_string()),
            "discard" => Cmd::Discard(v["n"].as_u64().unwrap_or(0) as usize),
            "rewind" => Cmd::Rewind,
            "quick_save" => Cmd::QuickSave,
            "quick_load" => Cmd::QuickLoad,
            "restart" => Cmd::Restart,
            "pause" => Cmd::Pause(v["paused"] == true),
            "writing" => Cmd::Writing(v["on"] == true),
            c => return Err(Error::invalid(format!("{c}: no such command"))),
        };
        self.cmds.lock().unwrap().push(cmd);
        Ok(json!({"status": "sent"}))
    }

    /// Send the play to a page until it ends or the page goes: messages of a
    /// kind byte, a little-endian u32 length and the payload; `F` is a frame
    /// (u16 width, u16 height, RGBA), `S` the status as JSON.
    pub fn stream(&self, out: &mut impl Write) {
        self.clients.fetch_add(1, Ordering::SeqCst);
        let _gone = Gone(self);
        let (mut frame_seq, mut status_seq) = (0, u64::MAX);
        loop {
            let (frame, status, ended) = {
                let mut o = self.out.lock().unwrap();
                let mut quiet = false;
                if o.frame_seq == frame_seq && o.status_seq == status_seq && !o.ended {
                    // A status now and then even when nothing changes, to find a closed page.
                    let (g, t) = self.cv.wait_timeout(o, Duration::from_secs(2)).unwrap();
                    (o, quiet) = (g, t.timed_out());
                }
                let frame = (o.frame_seq != frame_seq).then(|| {
                    let mut b = Vec::with_capacity(4 + o.rgba.len());
                    b.extend_from_slice(&(o.w as u16).to_le_bytes());
                    b.extend_from_slice(&(o.h as u16).to_le_bytes());
                    b.extend_from_slice(&o.rgba);
                    b
                });
                let status = (o.status_seq != status_seq || quiet || o.ended).then(|| o.status.to_string());
                frame_seq = o.frame_seq;
                status_seq = o.status_seq;
                (frame, status, o.ended)
            };
            let mut send = |kind: u8, b: &[u8]| -> std::io::Result<()> {
                out.write_all(&[kind])?;
                out.write_all(&(b.len() as u32).to_le_bytes())?;
                out.write_all(b)
            };
            let sent = frame
                .map_or(Ok(()), |f| send(b'F', &f))
                .and_then(|_| status.map_or(Ok(()), |s| send(b'S', s.as_bytes())));
            if sent.and_then(|_| out.flush()).is_err() || ended {
                return;
            }
        }
    }

    fn publish(&self, s: &Session, status: Option<Value>) {
        let mut o = self.out.lock().unwrap();
        let (w, h, rgba) = s.m.bus.gpu.display_rgba();
        (o.w, o.h, o.rgba) = (w, h, rgba);
        o.frame_seq += 1;
        if let Some(v) = status {
            o.status = v;
            o.status_seq += 1;
        }
        self.cv.notify_all();
    }

    fn set_status(&self, v: Value) {
        let mut o = self.out.lock().unwrap();
        o.status = v;
        o.status_seq += 1;
        self.cv.notify_all();
    }

    /// Whether no page has watched for [`IDLE`].
    fn abandoned(&self) -> bool {
        self.clients.load(Ordering::SeqCst) == 0 && self.left.lock().unwrap().elapsed() > IDLE
    }
}

/// A page stopped reading.
struct Gone<'a>(&'a Live);

impl Drop for Gone<'_> {
    fn drop(&mut self) {
        *self.0.left.lock().unwrap() = Instant::now();
        self.0.clients.fetch_sub(1, Ordering::SeqCst);
    }
}

/// The browser's play loop, at the console's field rate as the window's.
fn play(s: &mut Session, text: &str, dir: &Path, live: &Live) -> Result<(End, String), String> {
    let mut message = String::new();
    let b = match bindings::default_path().as_deref().map(bindings::load) {
        Some(Ok(Some(saved))) => saved,
        Some(Err(e)) => {
            message = format!("Controls file ignored: {e}");
            Bindings::default()
        }
        _ => Bindings::default(),
    };
    *live.left.lock().unwrap() = Instant::now();
    let mut paused = false;
    // The note being written before handing back: the game waits.
    let mut writing = false;
    // A mark waiting for its label: the game waits too.
    let mut labelling: Option<usize> = None;
    let mut message_seq = 0u64;
    let mut changed = true;
    let mut next = Instant::now();
    let mut live_at = Instant::now() - Duration::from_secs(5);
    loop {
        if live.abandoned() {
            return Ok((End::Closed, String::new()));
        }
        let cmds = std::mem::take(&mut *live.cmds.lock().unwrap());
        for c in cmds {
            let mut say = |m: String| {
                message = m;
                message_seq += 1;
            };
            match c {
                Cmd::Done(note) => return Ok((End::Done, note)),
                Cmd::Decline(note) => return Ok((End::Declined, note)),
                Cmd::Leave => return Ok((End::Closed, String::new())),
                Cmd::Mark => match s.mark(dir) {
                    Ok(n) => {
                        labelling = Some(n);
                        say(format!("Marked moment {n}: label it for the agent."));
                    }
                    Err(e) => say(format!("Could not mark: {e}")),
                },
                Cmd::Discard(n) => {
                    match s.discard_mark(n) {
                        Ok(()) => say(format!("Mark {n} discarded.")),
                        Err(e) => say(format!("Could not discard mark {n}: {e}")),
                    }
                    if labelling == Some(n) {
                        labelling = None;
                    }
                }
                Cmd::Label(n, label) => {
                    if s.marks.iter().any(|m| m.n == n) {
                        match s.label_mark(n, label.trim()) {
                            Ok(()) => say(format!("Mark {n} labelled.")),
                            Err(e) => say(format!("Could not label mark {n}: {e}")),
                        }
                    }
                    if labelling == Some(n) {
                        labelling = None;
                    }
                }
                Cmd::Rewind => match s.rewind() {
                    Ok(Some(f)) => {
                        let secs = f as f64 * s.m.bus.frame_cycles() as f64 / psx_core::bus::SYSCLK as f64;
                        say(format!("Went back {secs:.0} s. Rewind again goes further."));
                    }
                    Ok(None) => say("Nothing to rewind to yet.".into()),
                    Err(e) => say(format!("Could not rewind: {e}")),
                },
                Cmd::QuickSave => {
                    s.quick_save();
                    say("Quick saved. Quick load comes back here.".into());
                }
                Cmd::QuickLoad => match s.quick_load() {
                    Ok(true) => say("Back at the quick save.".into()),
                    Ok(false) => say("No quick save yet.".into()),
                    Err(e) => say(format!("Could not load the quick save: {e}")),
                },
                Cmd::Restart => match s.restart() {
                    Ok(()) => say("Started over from the beginning of the request.".into()),
                    Err(e) => say(format!("Could not start over: {e}")),
                },
                Cmd::Pause(p) => paused = p,
                Cmd::Writing(w) => writing = w,
            }
            changed = true;
        }

        let running = !paused && !writing && labelling.is_none();
        // Taps while the game waits are dropped.
        let mut held = std::mem::take(&mut live.held.lock().unwrap().tapped);
        if running {
            let h = live.held.lock().unwrap().clone();
            held.0.extend(h.keys);
            held.1.extend(h.pad);
            for _ in 0..if h.fast { FAST } else { 1 } {
                let (buttons, sticks) = bindings::pad_state(&b, s.analog(), &held.0, &held.1, h.sticks);
                s.frame(buttons, sticks);
            }
        }
        let state = match (labelling, writing, paused) {
            (Some(_), _, _) => "labelling_mark",
            (None, true, _) => "writing_note",
            (None, false, true) => "paused",
            (None, false, false) => "playing",
        };
        let status = |s: &Session| {
            let mode = match (s.dualshock(), s.analog()) {
                (false, _) => "digital pad",
                (true, false) => "DualShock, digital mode",
                (true, true) => "DualShock, analog mode",
            };
            json!({"state": state, "request": text, "message": message, "message_seq": message_seq,
                "labelling": labelling, "writing": writing, "paused": paused, "mode": mode, "run_frame": s.rel(), "quick_save": s.quick.is_some(),
                "marks": s.marks.iter().map(|m| json!({"n": m.n, "label": m.label})).collect::<Vec<_>>()})
        };
        let second = live_at.elapsed() >= Duration::from_secs(1);
        if second {
            live_at = Instant::now();
            s.write_live(dir, state);
        }
        if running {
            live.publish(s, (changed || second).then(|| status(s)));
        } else if changed || second {
            // Paused: the screen as it is (a restart or quick load changed it).
            if changed {
                live.publish(s, Some(status(s)));
            } else {
                live.set_status(status(s));
            }
        }
        changed = false;

        let period = Duration::from_secs_f64(s.m.bus.frame_cycles() as f64 / psx_core::bus::SYSCLK as f64);
        next += period;
        let now = Instant::now();
        if next > now {
            std::thread::sleep(next - now);
        } else if now - next > Duration::from_millis(250) {
            next = now;
        }
    }
}
