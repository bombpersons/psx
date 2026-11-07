//! The port protocol: a port as a separate program, driven over JSON lines
//! on its stdin and stdout, so psx runs the scenarios and draws replays on
//! its own side whatever the port is written in. psx's `docs/scenarios.md`
//! defines it; in short, one reply per request:
//!
//! ```text
//! -> {"hello":1}                                    <- {"kinds":["level"]}
//! -> {"launch":{"kind":"level","parameters":{...}}} <- {"ok":true} | {"error":"..."}
//! -> {"tick":{"buttons":["CROSS"],"analog":false,"sticks":[128,128,128,128]}}
//!                                                   <- {}
//! -> {"observe":{}}                                 <- {"values":{"player_x":4096}}
//! -> {"screen":{"path":"/tmp/f/12.rgba"}}           <- {"width":320,"height":240} | {"none":true}
//! ```
//!
//! Every request may instead get `{"error":"..."}` (it could not be done)
//! or `{"panic":"..."}` (the port panicked doing it). Values are in the
//! recordings' encoding ([`Value`]); a quantity left out is one the port has
//! none of. A screen is written to the path as `width * height` RGBA
//! pixels. stdout carries the protocol alone: other output goes to stderr,
//! and a stdout line that is not a JSON object is taken as a log.
//!
//! [`serve`] is the port's side for a Rust [`Port`] (`--psx`, which
//! [`crate::replay::main`] answers); [`Process`] and [`ProcessPort`] are
//! psx's side, a [`Port`] the runner and the replay drive as they drive one
//! in process.

use std::cell::RefCell;
use std::collections::{BTreeMap, VecDeque};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::rc::Rc;
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{Value as Json, json};

use crate::replay::Frame;
use crate::runner::{Button, Observation, Pad, Parameters, Port, gather, panic_message};
use crate::scenario::Glossary;
use crate::{Type, Value};

/// The protocol's version, in `hello`.
pub const VERSION: u64 = 1;

/// Answers requests from `input` on `output` until `input` ends, with a
/// fresh port from `port` for each launch. Panics in the port are replied,
/// not raised.
pub fn serve<P: Port>(port: impl Fn() -> P, input: impl BufRead, mut output: impl Write) -> Result<(), String> {
    let mut current: Option<P> = None;
    for line in input.lines() {
        let line = line.map_err(|e| format!("stdin: {e}"))?;
        if line.trim().is_empty() {
            continue;
        }
        let reply = match serde_json::from_str::<Json>(&line) {
            Ok(Json::Object(m)) if m.len() == 1 => {
                let (name, body) = m.into_iter().next().expect("one entry");
                let run =
                    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| answer(&port, &mut current, &name, body)));
                match run {
                    Ok(Ok(reply)) => reply,
                    Ok(Err(e)) => json!({"error": e}),
                    Err(p) => {
                        // A port that panicked is not used again.
                        current = None;
                        json!({"panic": panic_message(&p)})
                    }
                }
            }
            _ => json!({"error": format!("not a request (an object with one key): {line}")}),
        };
        writeln!(output, "{reply}").and_then(|_| output.flush()).map_err(|e| format!("stdout: {e}"))?;
    }
    Ok(())
}

fn answer<P: Port>(port: &impl Fn() -> P, current: &mut Option<P>, name: &str, body: Json) -> Result<Json, String> {
    let launched = |current: &mut Option<P>| current.take().ok_or_else(|| format!("{name} before a launch"));
    match name {
        "hello" => match body.as_u64() {
            Some(VERSION) => Ok(json!({"kinds": port().kinds()})),
            _ => Err(format!("this port speaks protocol {VERSION}, not {body}")),
        },
        "launch" => {
            *current = None;
            let kind = body.get("kind").and_then(Json::as_str).ok_or("launch: no kind")?;
            let parameters: Parameters = match body.get("parameters") {
                None => Parameters::new(),
                Some(Json::Object(p)) => p.clone(),
                Some(_) => return Err("launch: parameters are an object".into()),
            };
            let mut p = port();
            Ok(match p.launch(kind, &parameters) {
                Ok(()) => {
                    *current = Some(p);
                    json!({"ok": true})
                }
                Err(e) => json!({"error": e}),
            })
        }
        "tick" => {
            let pad = pad_of(&body)?;
            let mut p = launched(current)?;
            p.tick(&pad);
            *current = Some(p);
            Ok(json!({}))
        }
        "observe" => {
            let mut p = launched(current)?;
            let observed = p.observe();
            *current = Some(p);
            let values = gather(observed?.values())?;
            Ok(json!({"values": values}))
        }
        "screen" => {
            let path = body.get("path").and_then(Json::as_str).ok_or("screen: no path")?;
            let mut p = launched(current)?;
            let frame = p.screen();
            *current = Some(p);
            Ok(match frame {
                None => json!({"none": true}),
                Some(f) => {
                    std::fs::write(path, &f.rgba).map_err(|e| format!("{path}: {e}"))?;
                    json!({"width": f.width, "height": f.height})
                }
            })
        }
        _ => Err(format!("no request {name:?}")),
    }
}

/// A tick's pad on the wire: button names, the mode and the stick bytes.
fn pad_json(pad: &Pad) -> Json {
    json!({"buttons": pad.buttons.iter().map(|b| b.name()).collect::<Vec<_>>(),
           "analog": pad.analog, "sticks": pad.sticks})
}

fn pad_of(body: &Json) -> Result<Pad, String> {
    let mut pad = Pad::default();
    for b in body.get("buttons").and_then(Json::as_array).into_iter().flatten() {
        let name = b.as_str().ok_or("tick: buttons are names")?;
        pad.buttons.push(Button::parse(name).ok_or_else(|| format!("tick: no button {name:?}"))?);
    }
    pad.analog = body.get("analog").and_then(Json::as_bool).unwrap_or(false);
    if let Some(s) = body.get("sticks") {
        pad.sticks = serde_json::from_value(s.clone()).map_err(|e| format!("tick: sticks: {e}"))?;
    }
    Ok(pad)
}

/// Longest the port may take over one reply before it is stopped.
pub const REPLY_LIMIT: Duration = Duration::from_secs(120);

/// The stderr lines kept for a failure's message.
const TAIL: usize = 12;

/// A port program, started on demand and again after it dies; each
/// [`ProcessPort`] from [`Process::port`] drives it from its own launch.
pub struct Process {
    shared: Rc<RefCell<Shared>>,
    kinds: Vec<String>,
}

struct Shared {
    command: Vec<String>,
    dir: PathBuf,
    /// Where screens are written, removed with the process.
    screens: PathBuf,
    quantities: BTreeMap<&'static str, Type>,
    running: Option<Running>,
    /// The process's last stderr lines, for a failure.
    tail: Arc<Mutex<VecDeque<String>>>,
}

struct Running {
    child: Child,
    stdin: ChildStdin,
    lines: Receiver<String>,
}

impl Process {
    /// Starts `command` in `dir` and greets it; `glossary` names the
    /// quantities and their types, which each observation is checked
    /// against.
    pub fn start(command: &[String], dir: &Path, glossary: &Glossary) -> Result<Process, String> {
        if command.is_empty() {
            return Err("no command to run the port".into());
        }
        let quantities = glossary.types().into_iter().map(|(n, t)| (&*Box::leak(n.into_boxed_str()), t)).collect();
        static STARTED: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let n = STARTED.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let screens = std::env::temp_dir().join(format!("psx-port-screens-{}-{n}", std::process::id()));
        std::fs::create_dir_all(&screens).map_err(|e| format!("{}: {e}", screens.display()))?;
        let shared = Rc::new(RefCell::new(Shared {
            command: command.to_vec(),
            dir: dir.to_path_buf(),
            screens,
            quantities,
            running: None,
            tail: Default::default(),
        }));
        let reply = shared.borrow_mut().request(&json!({"hello": VERSION}))?;
        let kinds = reply
            .get("kinds")
            .and_then(|k| serde_json::from_value::<Vec<String>>(k.clone()).ok())
            .ok_or_else(|| format!("hello: expected {{\"kinds\":[...]}}, got {}", Json::Object(reply.clone())))?;
        Ok(Process { shared, kinds })
    }

    /// The scenario kinds the port launches, from its `hello`.
    pub fn kinds(&self) -> &[String] {
        &self.kinds
    }

    /// A port to launch a scenario in: its launch starts the program's
    /// world afresh.
    pub fn port(&self) -> ProcessPort {
        ProcessPort { shared: self.shared.clone(), kinds: self.kinds.clone(), screen: 0 }
    }
}

impl Drop for Shared {
    fn drop(&mut self) {
        self.stop();
        let _ = std::fs::remove_dir_all(&self.screens);
    }
}

impl Shared {
    fn spawn(&mut self) -> Result<&mut Running, String> {
        if self.running.is_none() {
            let mut child = Command::new(&self.command[0])
                .args(&self.command[1..])
                .current_dir(&self.dir)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .map_err(|e| format!("{}: {e}", self.command.join(" ")))?;
            let (stdin, stdout, stderr) =
                (child.stdin.take().expect("piped"), child.stdout.take().expect("piped"), child.stderr.take());
            let (send, lines) = std::sync::mpsc::channel();
            std::thread::spawn(move || {
                for l in BufReader::new(stdout).lines().map_while(Result::ok) {
                    if send.send(l).is_err() {
                        break;
                    }
                }
            });
            let tail = self.tail.clone();
            tail.lock().unwrap().clear();
            if let Some(stderr) = stderr {
                std::thread::spawn(move || {
                    for l in BufReader::new(stderr).lines().map_while(Result::ok) {
                        let mut t = tail.lock().unwrap();
                        if t.len() == TAIL {
                            t.pop_front();
                        }
                        t.push_back(l);
                    }
                });
            }
            self.running = Some(Running { child, stdin, lines });
        }
        Ok(self.running.as_mut().expect("just started"))
    }

    fn stop(&mut self) {
        if let Some(mut r) = self.running.take() {
            let _ = r.child.kill();
            let _ = r.child.wait();
        }
    }

    /// Sends one request and gives its reply, an object. `panic` replies
    /// are given too, for the caller to raise. A process that dies, says
    /// something else or takes too long is stopped, and the error says what
    /// it last wrote to stderr.
    fn request(&mut self, request: &Json) -> Result<serde_json::Map<String, Json>, String> {
        let reply = self.exchange(request);
        if reply.is_err() {
            self.stop();
            // Give the stderr reader a moment to catch the last words.
            std::thread::sleep(Duration::from_millis(50));
        }
        reply.map_err(|e| {
            let tail: Vec<String> = self.tail.lock().unwrap().iter().cloned().collect();
            if tail.is_empty() { e } else { format!("{e}; it said: {}", tail.join(" / ")) }
        })
    }

    fn exchange(&mut self, request: &Json) -> Result<serde_json::Map<String, Json>, String> {
        let r = self.spawn()?;
        writeln!(r.stdin, "{request}").and_then(|_| r.stdin.flush()).map_err(|_| exited(r))?;
        loop {
            match r.lines.recv_timeout(REPLY_LIMIT) {
                Ok(line) => match serde_json::from_str::<Json>(&line) {
                    Ok(Json::Object(m)) => return Ok(m),
                    // A log on stdout.
                    _ => continue,
                },
                Err(RecvTimeoutError::Timeout) => {
                    return Err(format!("the port did not answer in {} s", REPLY_LIMIT.as_secs()));
                }
                Err(RecvTimeoutError::Disconnected) => return Err(exited(r)),
            }
        }
    }
}

fn exited(r: &mut Running) -> String {
    let status = r.child.wait().map(|s| s.to_string()).unwrap_or_else(|e| e.to_string());
    format!("the port exited ({status})")
}

/// One scenario's run in a [`Process`]. Errors the port replies are the
/// calls' errors; its panics, and a process that fails, are raised as
/// panics, as an in-process port's would be.
pub struct ProcessPort {
    shared: Rc<RefCell<Shared>>,
    kinds: Vec<String>,
    screen: u64,
}

impl ProcessPort {
    /// The reply to `request`; a panic or a failed process is raised.
    fn call(&mut self, request: Json) -> Result<serde_json::Map<String, Json>, String> {
        let reply = self.shared.borrow_mut().request(&request);
        let mut reply = reply.unwrap_or_else(|e| std::panic::resume_unwind(Box::new(e)));
        if let Some(p) = reply.remove("panic") {
            let why = p.as_str().map(String::from).unwrap_or_else(|| p.to_string());
            std::panic::resume_unwind(Box::new(why));
        }
        match reply.remove("error") {
            Some(e) => Err(e.as_str().map(String::from).unwrap_or_else(|| e.to_string())),
            None => Ok(reply),
        }
    }
}

impl Port for ProcessPort {
    type Observed = Vec<(&'static str, Option<Value>)>;

    fn kinds(&self) -> Vec<String> {
        self.kinds.clone()
    }

    fn launch(&mut self, kind: &str, p: &Parameters) -> Result<(), String> {
        self.call(json!({"launch": {"kind": kind, "parameters": p}})).map(|_| ())
    }

    fn tick(&mut self, pad: &Pad) {
        if let Err(e) = self.call(json!({"tick": pad_json(pad)})) {
            std::panic::resume_unwind(Box::new(format!("the port refused the tick: {e}")));
        }
    }

    fn observe(&mut self) -> Result<Self::Observed, String> {
        let mut reply = self.call(json!({"observe": {}}))?;
        let Some(Json::Object(values)) = reply.remove("values") else {
            return Err("observe: expected {\"values\":{...}}".into());
        };
        let shared = self.shared.borrow();
        let mut out = Vec::new();
        for (name, v) in values {
            let Some((&name, ty)) = shared.quantities.get_key_value(name.as_str()) else {
                return Err(format!("{name}: not a quantity in the glossary"));
            };
            let v: Value = serde_json::from_value(v).map_err(|e| format!("{name}: {e}"))?;
            ty.check(&v).map_err(|e| format!("{name}: {e}"))?;
            out.push((name, Some(v)));
        }
        Ok(out)
    }

    fn screen(&mut self) -> Option<Frame> {
        self.screen += 1;
        let path = self.shared.borrow().screens.join(format!("{}.rgba", self.screen));
        let reply = self.call(json!({"screen": {"path": path}}));
        let frame = match reply {
            Err(e) => {
                eprintln!("screen: {e}");
                None
            }
            Ok(r) if r.contains_key("none") => None,
            Ok(r) => {
                let size = |k: &str| r.get(k).and_then(Json::as_u64).and_then(|n| u32::try_from(n).ok());
                match (size("width"), size("height"), std::fs::read(&path)) {
                    (Some(width), Some(height), Ok(rgba)) => Some(Frame { width, height, rgba }),
                    _ => {
                        eprintln!("screen: expected {{\"width\",\"height\"}} and the pixels at {}", path.display());
                        None
                    }
                }
            }
        };
        let _ = std::fs::remove_file(&path);
        frame
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runner::Scenarios;
    use std::io::IsTerminal;

    /// A walker moving right while RIGHT is held (slower on an analog pad,
    /// and by the left stick from centre), from `x`. `floor` makes it panic
    /// past it, `stray` observes a quantity the glossary lacks and `huge`
    /// one outside its type; `screen` draws it on a 4x1 screen.
    #[derive(Default)]
    struct Walk {
        x: i64,
        floor: Option<i64>,
        stray: bool,
        huge: bool,
    }

    impl Port for Walk {
        type Observed = Vec<(&'static str, Option<Value>)>;

        fn kinds(&self) -> Vec<String> {
            vec!["level".into()]
        }

        fn launch(&mut self, _: &str, p: &Parameters) -> Result<(), String> {
            let get = |k: &str| p.get(k).and_then(Json::as_i64);
            self.x = get("x").ok_or("x: a number")?;
            self.floor = get("floor");
            self.stray = get("stray").is_some();
            self.huge = get("huge").is_some();
            Ok(())
        }

        fn tick(&mut self, pad: &Pad) {
            if pad.held(Button::Right) {
                self.x += if pad.analog { 50 } else { 100 };
            }
            self.x += i64::from(pad.sticks[0]) - 128;
            if self.floor.is_some_and(|f| self.x > f) {
                panic!("walked off the floor");
            }
        }

        fn observe(&mut self) -> Result<Self::Observed, String> {
            let x = if self.huge { 1 << 40 } else { self.x };
            let mut v = vec![("x", Some(Value::Int(x.into()))), ("y", None)];
            if self.stray {
                v.push(("stray", Some(Value::Int(1))));
            }
            Ok(v)
        }

        fn screen(&mut self) -> Option<Frame> {
            let mut rgba = vec![0; 16];
            rgba[(self.x as usize / 100) % 4 * 4] = 255;
            Some(Frame { width: 4, height: 1, rgba })
        }
    }

    /// The port's side: run alone by [`child`] with `--ignored`, it serves
    /// on stdin (and ends at once when stdin is closed or a terminal).
    #[test]
    #[ignore]
    fn child() {
        if !std::io::stdin().is_terminal() {
            // Off the line libtest started ("test ... ").
            println!();
            serve(Walk::default, std::io::stdin().lock(), std::io::stdout().lock()).unwrap();
        }
    }

    fn process() -> Process {
        let exe = std::env::current_exe().unwrap().to_string_lossy().into_owned();
        let command: Vec<String> =
            [exe.as_str(), "--exact", "protocol::tests::child", "--ignored", "--nocapture", "--test-threads=1"]
                .map(String::from)
                .into();
        let x: crate::Field = serde_json::from_str(r#"{"type":"int","bits":32,"signed":true}"#).unwrap();
        let glossary =
            Glossary { title: "Walk".into(), quantities: [("x".to_string(), x.clone()), ("y".to_string(), x)].into() };
        Process::start(&command, &std::env::temp_dir(), &glossary).unwrap()
    }

    const HEADER: &str = r#"{"format":"scenario-recording","format_version":1,"scenario":"ID","source":{"kind":"captured","platform":"psx","image_sha256":"00"},"producer":{"name":"psx","version":"0"},"quantities":{"x":{"type":"int","bits":32,"signed":true}},"initial":{"x":4096}}"#;

    fn workspace(scenarios: &[(&str, &str, &[&str])]) -> PathBuf {
        static N: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!("psx-scenario-protocol-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        for (id, parameters, rows) in scenarios {
            std::fs::create_dir_all(root.join("scenarios").join(id)).unwrap();
            let toml = format!(
                "moment = \"m\"\nchain = \"c\"\nkind = \"level\"\n[compare]\nx = 0\n[parameters]\n{parameters}\n"
            );
            std::fs::write(root.join(format!("scenarios/{id}.toml")), toml).unwrap();
            let rec = format!("{}\n{}\n", HEADER.replace("ID", id), rows.join("\n"));
            std::fs::write(root.join(format!("scenarios/{id}/a.jsonl")), rec).unwrap();
        }
        root
    }

    #[test]
    fn a_port_over_the_protocol_reports_as_in_process() {
        let root = workspace(&[
            (
                "walk",
                "x = 4096",
                &[
                    r#"{"tick":1,"field":1,"pad":["RIGHT"],"values":{"x":4196}}"#,
                    r#"{"tick":2,"field":2,"analog":true,"sticks":{"lx":160},"values":{"x":4278}}"#,
                ],
            ),
            ("differs", "x = 4096", &[r#"{"tick":1,"field":1,"pad":["RIGHT"],"values":{"x":4200}}"#]),
            (
                "fragile",
                "x = 4096\nfloor = 4100",
                &[
                    r#"{"tick":1,"field":1,"values":{"x":4096}}"#,
                    r#"{"tick":2,"field":2,"pad":["RIGHT"],"values":{"x":4196}}"#,
                ],
            ),
            ("unlaunched", "y = 1", &[r#"{"tick":1,"field":1,"values":{"x":4096}}"#]),
        ]);
        let p = process();
        let over = serde_json::to_value(Scenarios::new(&root, move || p.port()).report()).unwrap();
        let within = serde_json::to_value(Scenarios::new(&root, Walk::default).report()).unwrap();
        assert_eq!(over, within);
        let failures = |id: &str| {
            over["scenarios"].as_array().unwrap().iter().find(|s| s["id"] == id).unwrap()["recordings"][0]["failures"]
                .clone()
        };
        assert_eq!(failures("walk"), json!([]));
        assert_eq!(failures("differs"), json!(["differs from the recording in x"]));
        assert_eq!(failures("fragile"), json!(["tick 2 panicked: walked off the floor"]));
        assert_eq!(failures("unlaunched"), json!(["the launcher failed: x: a number"]));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn observations_are_checked_against_the_glossary() {
        let p = process();
        let observe = |parameters: Json| {
            let mut port = p.port();
            port.launch("level", parameters.as_object().unwrap()).unwrap();
            port.observe()
        };
        assert_eq!(observe(json!({"x": 7})), Ok(vec![("x", Some(Value::Int(7)))]));
        assert_eq!(observe(json!({"x": 7, "stray": 1})), Err("stray: not a quantity in the glossary".into()));
        assert_eq!(
            observe(json!({"x": 7, "huge": 1})),
            Err("x: 1099511627776 is outside -2147483648..=2147483647".into())
        );
    }

    #[test]
    fn screens_come_back_and_a_dead_port_is_started_again() {
        let p = process();
        let mut port = p.port();
        port.launch("level", json!({"x": 200}).as_object().unwrap()).unwrap();
        let f = port.screen().unwrap();
        assert_eq!((f.width, f.height, f.rgba[8]), (4, 1, 255));
        // The child killed under it: that call fails as a panic, the next
        // port starts it again.
        p.shared.borrow_mut().running.as_mut().unwrap().child.kill().unwrap();
        let died = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| port.tick(&Pad::default())));
        let why = panic_message(&died.unwrap_err());
        assert!(why.starts_with("the port exited"), "{why}");
        let mut again = p.port();
        again.launch("level", json!({"x": 1}).as_object().unwrap()).unwrap();
        assert_eq!(again.observe(), Ok(vec![("x", Some(Value::Int(1)))]));
    }

    #[test]
    fn serve_answers_a_request_a_line() {
        let input = [
            r#"{"hello":1}"#,
            r#"{"tick":{}}"#,
            r#"{"launch":{"kind":"level","parameters":{"x":5}}}"#,
            r#"{"tick":{"buttons":["CROSS","TURBO"]}}"#,
            r#"{"tick":{"buttons":["right"]}}"#,
            r#"{"observe":{}}"#,
            r#"{"hello":2}"#,
            "not json",
        ]
        .join("\n");
        let mut out = Vec::new();
        serve(Walk::default, input.as_bytes(), &mut out).unwrap();
        let replies: Vec<Json> =
            out.split(|b| *b == b'\n').filter(|l| !l.is_empty()).map(|l| serde_json::from_slice(l).unwrap()).collect();
        assert_eq!(
            replies,
            [
                json!({"kinds": ["level"]}),
                json!({"error": "tick before a launch"}),
                json!({"ok": true}),
                json!({"error": "tick: no button \"TURBO\""}),
                json!({}),
                json!({"values": {"x": 105}}),
                json!({"error": "this port speaks protocol 1, not 2"}),
                json!({"error": "not a request (an object with one key): not json"}),
            ]
        );
    }
}
