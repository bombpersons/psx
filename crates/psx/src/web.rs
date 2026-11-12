//! `psx web`: a read-only page on localhost over the projects' call logs,
//! progress and recordings.
//!
//! One embedded page ([`PAGE`]) and a small JSON API, served by a thread per
//! connection on 127.0.0.1. `/api/events` streams which files changed
//! (server-sent events), so the page fetches only what moved. A recording's
//! frames come from replaying it on psx-core in a child process
//! (`psx __web-replay`), cached per recording in the data directory.

use std::collections::{BTreeSet, HashMap};
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use serde_json::{Value, json};

use crate::commands::{Ctx, Outcome};

const PAGE: &str = include_str!("web/index.html");

/// Serve the page.
#[derive(clap::Args)]
pub struct WebArgs {
    /// Project roots to show (remembered for next time), beside the ones shown before.
    pub roots: Vec<PathBuf>,
    /// Stop showing these project roots.
    #[arg(long)]
    pub forget: Vec<PathBuf>,
    /// Port on 127.0.0.1.
    #[arg(long, default_value_t = 7878)]
    pub port: u16,
}

struct Project {
    root: PathBuf,
    name: String,
}

struct State {
    projects: Vec<Project>,
    port: u16,
    /// Given to the page it serves, and wanted on every POST: a page from
    /// elsewhere cannot read it, so cannot act.
    token: String,
    /// Replays started by this server, by cache key.
    replays: Mutex<HashMap<String, Arc<Mutex<Replay>>>>,
    /// Requests being played in the browser, by run directory.
    plays: Mutex<HashMap<PathBuf, Arc<crate::handoff::browser::Live>>>,
}

#[derive(Default)]
struct Replay {
    dir: PathBuf,
    running: bool,
    /// The child's result once it ended: differences or an error.
    done: Option<Value>,
    /// What a port replay is doing: building, or the tick it drew.
    progress: String,
}

pub fn main(a: WebArgs) -> ExitCode {
    let roots = remember(&a.roots, &a.forget);
    let projects: Vec<Project> = roots
        .into_iter()
        .filter(|r| r.is_dir())
        .map(|root| {
            let name = root
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| root.display().to_string());
            Project { root, name }
        })
        .collect();
    let listener = match TcpListener::bind(("127.0.0.1", a.port)) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("psx web: 127.0.0.1:{}: {e} (--port picks another)", a.port);
            return ExitCode::from(1);
        }
    };
    eprintln!("psx web: http://127.0.0.1:{}/", a.port);
    for p in &projects {
        eprintln!("  {}", p.root.display());
    }
    if projects.is_empty() {
        eprintln!("  no projects yet: psx web <ROOT>... adds them");
    }
    let state = Arc::new(State {
        projects,
        port: a.port,
        token: token(),
        replays: Mutex::new(HashMap::new()),
        plays: Mutex::new(HashMap::new()),
    });
    for stream in listener.incoming().flatten() {
        let state = state.clone();
        std::thread::spawn(move || handle(stream, &state));
    }
    ExitCode::SUCCESS
}

/// 128 random bits, as hex.
fn token() -> String {
    let mut b = [0u8; 16];
    let read = std::fs::File::open("/dev/urandom").and_then(|mut f| std::io::Read::read_exact(&mut f, &mut b));
    if read.is_err() {
        // No /dev/urandom: the clock and the process, hashed.
        use std::hash::{BuildHasher, Hasher};
        for (i, c) in b.chunks_mut(8).enumerate() {
            let mut h = std::collections::hash_map::RandomState::new().build_hasher();
            h.write_u128(crate::now_ms() + i as u128);
            h.write_u32(std::process::id());
            c.copy_from_slice(&h.finish().to_le_bytes());
        }
    }
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// The remembered roots with `add` and without `forget`, written back.
pub(crate) fn remember(add: &[PathBuf], forget: &[PathBuf]) -> Vec<PathBuf> {
    let file = crate::calls::projects_file();
    let mut roots: Vec<PathBuf> = file
        .as_ref()
        .and_then(|f| std::fs::read_to_string(f).ok())
        .map(|t| t.lines().filter(|l| !l.trim().is_empty()).map(PathBuf::from).collect())
        .unwrap_or_default();
    let abs = |p: &PathBuf| std::fs::canonicalize(p).or_else(|_| std::path::absolute(p)).unwrap_or(p.clone());
    for r in add {
        let r = abs(r);
        if !roots.contains(&r) {
            roots.push(r);
        }
    }
    let forget: Vec<PathBuf> = forget.iter().map(abs).collect();
    roots.retain(|r| !forget.contains(r));
    if let Some(f) = file {
        let text: String = roots.iter().map(|r| format!("{}\n", r.display())).collect();
        let _ = std::fs::create_dir_all(f.parent().unwrap());
        let _ = std::fs::write(f, text);
    }
    roots
}

// ---------------------------------------------------------------- HTTP

struct Request {
    path: String,
    query: HashMap<String, String>,
    /// A POST's body, as JSON.
    body: Value,
    /// Sent by the page this server gave out (its token, and its origin when
    /// the browser names one). Anything that starts work needs it, a GET too:
    /// any website can make the browser send a GET to 127.0.0.1.
    ours: bool,
}

/// Largest POST body read.
const BODY_LIMIT: usize = 64 * 1024;

fn handle(stream: TcpStream, st: &State) {
    let _ = stream.set_read_timeout(Some(Duration::from_secs(10)));
    let Ok(read) = stream.try_clone() else { return };
    let mut r = BufReader::new(read);
    let mut line = String::new();
    if r.read_line(&mut line).unwrap_or(0) == 0 {
        return;
    }
    let (mut host, mut origin, mut token, mut length) = (String::new(), None, String::new(), 0usize);
    loop {
        let mut h = String::new();
        if r.read_line(&mut h).unwrap_or(0) == 0 || h.trim().is_empty() {
            break;
        }
        let Some((k, v)) = h.split_once(':') else { continue };
        let v = v.trim().to_string();
        match k.to_ascii_lowercase().as_str() {
            "host" => host = v,
            "origin" => origin = Some(v),
            "x-psx-token" => token = v,
            "content-length" => length = v.parse().unwrap_or(usize::MAX),
            _ => {}
        }
    }
    let mut parts = line.split_whitespace();
    let (method, target) = (parts.next().unwrap_or(""), parts.next().unwrap_or("/"));
    let mut out = stream;
    // Only this machine's names: a page elsewhere cannot rebind a name to
    // 127.0.0.1 and read the projects.
    let hosts = [format!("127.0.0.1:{}", st.port), format!("localhost:{}", st.port)];
    if !hosts.contains(&host) {
        let _ = send(&mut out, "403 Forbidden", "text/plain", b"psx web answers 127.0.0.1 and localhost only");
        return;
    }
    let mut body = Value::Null;
    let ours = origin.as_ref().is_none_or(|o| hosts.iter().any(|h| o == &format!("http://{h}"))) && token == st.token;
    match method {
        "GET" => {}
        "POST" => {
            // Only the page this server gave out acts.
            if !ours {
                let _ = send(&mut out, "403 Forbidden", "text/plain", b"psx web acts only for the page it serves");
                return;
            }
            if length > BODY_LIMIT {
                let _ = send(&mut out, "413 Payload Too Large", "text/plain", b"too large");
                return;
            }
            let mut b = vec![0; length];
            if std::io::Read::read_exact(&mut r, &mut b).is_err() {
                return;
            }
            body = serde_json::from_slice(&b).unwrap_or(Value::Null);
        }
        _ => {
            let _ = send(&mut out, "405 Method Not Allowed", "text/plain", b"GET and POST only");
            return;
        }
    }
    let (path, query) = target.split_once('?').unwrap_or((target, ""));
    let query = query
        .split('&')
        .filter_map(|kv| kv.split_once('=').or(Some((kv, ""))))
        .filter(|(k, _)| !k.is_empty())
        .map(|(k, v)| (decode(k), decode(v)))
        .collect();
    let req = Request { path: decode(path), query, body, ours };
    if method == "POST" {
        let _ = match act(&req, st) {
            Ok(v) => send(&mut out, "200 OK", "application/json", v.to_string().as_bytes()),
            Err(e) => send(&mut out, "400 Bad Request", "application/json", e.to_value().to_string().as_bytes()),
        };
        return;
    }
    if req.path == "/api/events" {
        events(out, st);
        return;
    }
    if let Some(live) =
        req.path.strip_suffix("/stream").and_then(|p| request_dir(p, st).ok()).and_then(|(_, d)| st.live(&d))
    {
        let head = "HTTP/1.1 200 OK\r\nContent-Type: application/octet-stream\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n";
        let _ = out.set_nodelay(true);
        if out.write_all(head.as_bytes()).is_ok() {
            live.stream(&mut out);
        }
        return;
    }
    let _ = match route(&req, st) {
        Reply::Json(v) => send(&mut out, "200 OK", "application/json", v.to_string().as_bytes()),
        Reply::Bytes(ct, b) => send(&mut out, "200 OK", ct, &b),
        Reply::Missing(why) => {
            send(&mut out, "404 Not Found", "application/json", json!({"error": why}).to_string().as_bytes())
        }
    };
}

/// No other site may show the page in a frame and steer clicks onto it.
const NO_FRAMING: &str =
    "X-Frame-Options: DENY\r\nContent-Security-Policy: frame-ancestors 'none'\r\nX-Content-Type-Options: nosniff\r\n";

/// A scenario id (`a/b`, a path under scenarios/) and a recording name are
/// joined onto project paths: neither may climb out or name a drive.
fn recording_ok(id: &str, name: &str) -> bool {
    let part = |c: &str| !c.is_empty() && c != "." && c != ".." && !c.contains(['\\', ':', '\0']);
    id.split('/').all(part) && part(name) && !name.contains('/')
}

fn send(s: &mut TcpStream, status: &str, ctype: &str, body: &[u8]) -> std::io::Result<()> {
    let head = format!(
        "HTTP/1.1 {status}\r\nContent-Type: {ctype}\r\nContent-Length: {}\r\nCache-Control: no-store\r\n{NO_FRAMING}Connection: close\r\n\r\n",
        body.len()
    );
    s.write_all(head.as_bytes())?;
    s.write_all(body)?;
    s.flush()
}

/// `%XX` and `+` decoded.
fn decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'+' => out.push(b' '),
            b'%' if i + 2 < b.len() => {
                match u8::from_str_radix(std::str::from_utf8(&b[i + 1..i + 3]).unwrap_or(""), 16) {
                    Ok(v) => {
                        out.push(v);
                        i += 2;
                    }
                    Err(_) => out.push(b'%'),
                }
            }
            c => out.push(c),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

enum Reply {
    Json(Value),
    Bytes(&'static str, Vec<u8>),
    Missing(String),
}

fn route(req: &Request, st: &State) -> Reply {
    let path = req.path.as_str();
    if path == "/" || path == "/index.html" {
        return Reply::Bytes("text/html; charset=utf-8", PAGE.replace("__PSX_TOKEN__", &st.token).into_bytes());
    }
    if path == "/api/requests" {
        return Reply::Json(requests(st));
    }
    if path == "/api/bindings" {
        return Reply::Json(bindings());
    }
    if path == "/api/projects" {
        return Reply::Json(json!(
            st.projects
                .iter()
                .enumerate()
                .map(|(i, p)| json!({
                    "i": i, "name": p.name, "root": p.root, "port": p.root.join(crate::port::GOAL).is_file(),
                }))
                .collect::<Vec<_>>()
        ));
    }
    let Some(rest) = path.strip_prefix("/api/p/") else { return Reply::Missing(format!("{path}: no such page")) };
    let (i, what) = rest.split_once('/').unwrap_or((rest, ""));
    let Some(p) = i.parse::<usize>().ok().and_then(|i| st.projects.get(i)) else {
        return Reply::Missing(format!("project {i}: not shown"));
    };
    let root = &p.root;
    let q = |k: &str| req.query.get(k).map(String::as_str).unwrap_or("");
    if matches!(what, "recording" | "replay" | "frame" | "port_replay") && !recording_ok(q("scenario"), q("name")) {
        return Reply::Missing(format!("{}/{}: not a recording", q("scenario"), q("name")));
    }
    // Starting a replay builds and runs code: only for the page's own requests.
    let start = q("start") == "1";
    if start && !req.ours {
        return Reply::Missing("starting a replay needs the page's token".into());
    }
    match what {
        "overview" => Reply::Json(overview(root)),
        "calls" => Reply::Json(calls(root, q("since").parse().ok())),
        "recordings" => Reply::Json(recordings(root)),
        "code" => Reply::Json(crate::decomp::overview(root)),
        "recording" => match crate::scenario::plot(root, q("scenario"), q("name"), &last_check(root)) {
            Ok(v) => Reply::Json(v),
            Err(e) => Reply::Json(e.to_value()),
        },
        "replay" => Reply::Json(replay(st, root, q("scenario"), q("name"), start, q("decomp") == "1")),
        "frame" => frame(st, root, q("scenario"), q("name"), q("tick"), q("decomp") == "1"),
        "port_replay" => Reply::Json(port_replay(st, root, q("scenario"), q("name"), start)),
        "port_frame" => port_frame(root, q("key"), q("tick")),
        "messages" => Reply::Json(messages(root)),
        "reviews" => {
            let mut v = crate::port::review::rounds(root);
            v["reports"] = json!(crate::reports::all(root));
            Reply::Json(v)
        }
        "review_diff" => match crate::port::review::diff(root, q("from"), q("to")) {
            Some(d) => Reply::Bytes("text/plain; charset=utf-8", d.into_bytes()),
            None => Reply::Missing("no diff between those commits (or too large to show)".into()),
        },
        "stats" => Reply::Json(crate::stats::of(root)),
        "file" => project_file(root, q("path")),
        _ => {
            let id = what.strip_prefix("call/").unwrap_or("");
            // The screen of the game a running call runs (crate::live).
            let (id, live) = id.strip_suffix("/live").map_or((id, false), |id| (id, true));
            if !id.is_empty() && id.bytes().all(|c| c.is_ascii_digit() || c == b'-') && live {
                return match std::fs::read(root.join(crate::calls::RESULTS).join(format!("{id}.live.png"))) {
                    Ok(b) => Reply::Bytes("image/png", b),
                    Err(_) => Reply::Missing(format!("{id}: no game running")),
                };
            }
            if !id.is_empty() && id.bytes().all(|c| c.is_ascii_digit() || c == b'-') {
                return match std::fs::read(root.join(crate::calls::RESULTS).join(format!("{id}.json"))) {
                    Ok(b) => Reply::Bytes("application/json", b),
                    Err(_) => Reply::Missing(format!("{id}: its result was not kept")),
                };
            }
            Reply::Missing(format!("{what}: no such page"))
        }
    }
}

// ---------------------------------------------------------------- events

/// What changed, by project, once a second: the call log, the progress
/// line, the last check, the loop guard, the commits.
fn events(mut out: TcpStream, st: &State) {
    // The comment is the body's first bytes: Firefox opens the stream only
    // once some arrive.
    let head = "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nCache-Control: no-store\r\nConnection: keep-alive\r\n\r\n: open\n\n";
    if out.write_all(head.as_bytes()).is_err() {
        return;
    }
    let mut seen: Vec<Vec<(u64, u128)>> = st.projects.iter().map(|_| Vec::new()).collect();
    let mut beat = 0;
    loop {
        for (i, p) in st.projects.iter().enumerate() {
            let mut now: Vec<(u64, u128)> = WATCHED.iter().map(|(_, f)| stamp(&p.root.join(f))).collect();
            now.push(queue_stamp(&p.root));
            if seen[i] != now {
                let what: Vec<&str> = WATCHED
                    .iter()
                    .map(|(name, _)| *name)
                    .chain(["plays"])
                    .zip(now.iter().zip(seen[i].iter().chain(std::iter::repeat(&(0, 0)))))
                    .filter(|(_, (a, b))| a != b)
                    .map(|(name, _)| name)
                    .collect();
                let first = seen[i].is_empty();
                seen[i] = now;
                if first {
                    continue;
                }
                let msg = format!("event: change\ndata: {}\n\n", json!({"p": i, "what": what}));
                if out.write_all(msg.as_bytes()).is_err() {
                    return;
                }
            }
        }
        beat += 1;
        if beat % 15 == 0 && out.write_all(b": beat\n\n").is_err() {
            return;
        }
        std::thread::sleep(Duration::from_secs(1));
    }
}

const WATCHED: [(&str, &str); 7] = [
    ("messages", crate::messages::LOG),
    ("messages_seen", crate::messages::SEEN),
    ("calls", crate::calls::LOG),
    ("progress", crate::port::status::PROGRESS),
    ("check", ".psx/port/last.json"),
    ("commit", ".git/logs/HEAD"),
    ("goal", crate::port::GOAL),
];

/// The play queue's files, folded into one stamp: a request added,
/// claimed, given back, answered or read changes it.
fn queue_stamp(root: &Path) -> (u64, u128) {
    let Ok(entries) = std::fs::read_dir(root.join(crate::handoff::queue::DIR)) else { return (0, 0) };
    let (mut n, mut t) = (0u64, 0u128);
    for e in entries.flatten() {
        for f in ["queued.json", "claim.json", "report.json", "read"] {
            let (len, mt) = stamp(&e.path().join(f));
            if mt != 0 {
                n += 1 + len;
                t = t.wrapping_add(mt);
            }
        }
    }
    (n, t)
}

fn stamp(p: &Path) -> (u64, u128) {
    std::fs::metadata(p)
        .map(|m| {
            let t = m.modified().ok().and_then(|t| t.duration_since(SystemTime::UNIX_EPOCH).ok());
            (m.len(), t.map_or(0, |d| d.as_nanos()))
        })
        .unwrap_or((0, 0))
}

// ---------------------------------------------------------------- play requests

/// The person's controls (the window's, saved from its menu): what the page
/// shows beside a browser play. The server applies them to the input.
fn bindings() -> Value {
    use crate::handoff::bindings::{self, ACTIONS, Bindings};
    let b = match bindings::default_path().as_deref().map(bindings::load) {
        Some(Ok(Some(saved))) => saved,
        _ => Bindings::default(),
    };
    let map = |slots: &[Option<String>; 17]| -> Value {
        ACTIONS
            .iter()
            .zip(slots)
            .filter_map(|(a, s)| s.as_ref().map(|s| (a.to_string(), json!(s))))
            .collect::<serde_json::Map<_, _>>()
            .into()
    };
    json!({"keyboard": map(&b.keyboard), "gamepad": map(&b.gamepad), "stick_deadzone": b.stick_deadzone})
}

/// Answered requests shown per project, beside the waiting ones.
const ANSWERED_SHOWN: usize = 10;

/// Every project's play requests: the waiting ones oldest first, then the
/// latest answered.
fn requests(st: &State) -> Value {
    let mut waiting = Vec::new();
    let mut answered = Vec::new();
    for (i, p) in st.projects.iter().enumerate() {
        let all = crate::handoff::queue::list(&p.root);
        let with = |r: &crate::handoff::queue::Request| {
            let mut v = r.json();
            v["p"] = json!(i);
            v["project"] = json!(p.name);
            v["here"] = json!(st.live(&r.dir).is_some());
            v
        };
        let (done, open): (Vec<_>, Vec<_>) =
            all.iter().partition(|r| matches!(r.state, crate::handoff::queue::State::Answered(_)));
        waiting.extend(open.into_iter().map(with));
        answered.extend(done.into_iter().rev().take(ANSWERED_SHOWN).map(with));
    }
    let t = |v: &Value| v["queued_unix_ms"].as_u64().unwrap_or(0);
    waiting.sort_by_key(t);
    answered.sort_by_key(|v| std::cmp::Reverse(t(v)));
    json!({"waiting": waiting, "answered": answered, "window": crate::handoff::capability()})
}

/// `/api/p/I/request/ID`: the project root and the request's run directory.
fn request_dir(path: &str, st: &State) -> crate::Result<(PathBuf, PathBuf)> {
    let bad = || crate::Error::invalid(format!("{path}: no such action"));
    let rest = path.strip_prefix("/api/p/").ok_or_else(bad)?;
    let mut parts = rest.splitn(3, '/');
    let (i, kind, id) = (parts.next(), parts.next(), parts.next());
    let p = i.and_then(|i| i.parse::<usize>().ok()).and_then(|i| st.projects.get(i)).ok_or_else(bad)?;
    let ok = |id: &&str| !id.is_empty() && id.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_');
    let id = id.filter(|id| matches!(kind, Some("request" | "try")) && ok(id)).ok_or_else(bad)?;
    // A try with the agent's C waits apart from the queue.
    let queue = if kind == Some("try") { crate::handoff::TRIES } else { crate::handoff::queue::DIR };
    Ok((p.root.clone(), p.root.join(queue).join(id)))
}

impl State {
    /// The browser play of the request in `dir`, while it lasts.
    fn live(&self, dir: &Path) -> Option<Arc<crate::handoff::browser::Live>> {
        let mut plays = self.plays.lock().unwrap();
        plays.retain(|_, l| !l.ended());
        plays.get(dir).cloned()
    }
}

/// A POST: play a request in the browser (and its input and commands), open
/// its window, or decline it with a note.
fn act(req: &Request, st: &State) -> crate::Result<Value> {
    if let Some((i, what)) = req.path.strip_prefix("/api/p/").and_then(|r| r.split_once("/messages/")) {
        let p = i.parse::<usize>().ok().and_then(|i| st.projects.get(i));
        let p = p.ok_or_else(|| crate::Error::invalid(format!("project {i}: not shown")))?;
        return match what {
            "send" => crate::messages::post(
                &p.root,
                "person",
                req.body["text"].as_str().unwrap_or(""),
                req.body["reply_to"].as_str(),
            ),
            "seen" => {
                crate::messages::mark_seen(&p.root, "person", req.body["n"].as_u64().unwrap_or(0) as usize);
                Ok(json!({"status": "ok"}))
            }
            _ => Err(crate::Error::invalid(format!("{}: no such action", req.path))),
        };
    }
    // Play now: the person starts a play of their own, queued and claimed
    // at once so it is answered, recorded and reported as a request is.
    if let Some(i) = req.path.strip_prefix("/api/p/").and_then(|r| r.strip_suffix("/play")) {
        let p = i.parse::<usize>().ok().and_then(|i| st.projects.get(i));
        let p = p.ok_or_else(|| crate::Error::invalid(format!("project {i}: not shown")))?;
        let text = |k: &str| req.body[k].as_str().map(str::trim).filter(|s| !s.is_empty()).map(String::from);
        let ctx = crate::commands::Ctx { root: p.root.clone(), cancel: None };
        let dir = crate::handoff::person_play(
            &ctx,
            req.body["what"].as_str().unwrap_or(""),
            text("scenario"),
            text("name"),
            text("from"),
            // The adjustments to play with: a list (none checked: the stock game), or absent for the default.
            req.body["adjust"].as_array().map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect()),
            req.body["with_c"] == true,
            // The person's own play is held out unless they give it to the agent.
            req.body["for_agent"] != true,
        )?;
        let live = crate::handoff::browser::Live::start(&p.root, &dir)?;
        st.plays.lock().unwrap().insert(dir.clone(), live);
        let id = dir.file_name().unwrap_or_default().to_string_lossy().into_owned();
        return Ok(json!({"status": "playing", "id": id, "try": req.body["with_c"] == true}));
    }
    // A recording held out from the agent, or given back to its work.
    if let Some(i) = req.path.strip_prefix("/api/p/").and_then(|r| r.strip_suffix("/held_out")) {
        let p = i.parse::<usize>().ok().and_then(|i| st.projects.get(i));
        let p = p.ok_or_else(|| crate::Error::invalid(format!("project {i}: not shown")))?;
        let (s, n) = (req.body["scenario"].as_str().unwrap_or(""), req.body["name"].as_str().unwrap_or(""));
        if !recording_ok(s, n) {
            return Err(crate::Error::invalid(format!("{s}/{n}: not a recording")));
        }
        let (rec, on) = (format!("{s}/{n}"), req.body["on"] == true);
        let held_out = crate::port::held_out::set(&p.root, &rec, on)?;
        let mut v = json!({"status": "ok", "held_out": held_out});
        let message = if on { format!("Hold out {rec}") } else { format!("Give {rec} to the work") };
        crate::port::held_out::commit(&p.root, &rec, &message, &mut v);
        return Ok(v);
    }
    let (head, what) = req.path.rsplit_once('/').unwrap_or(("", ""));
    let (root, dir) = request_dir(head, st)?;
    let id = dir.file_name().unwrap_or_default().to_string_lossy().into_owned();
    let playing = || st.live(&dir).ok_or_else(|| crate::Error::new("ended", format!("{id} is not being played here")));
    match what {
        "input" => {
            playing()?.input(&req.body);
            return Ok(json!({"status": "ok"}));
        }
        "cmd" => return playing()?.command(&req.body),
        _ => {}
    }
    let r = crate::handoff::queue::load(&dir)
        .ok_or_else(|| crate::Error::invalid(format!("{id}: no such play request")))?;
    match what {
        "play" => {
            if st.live(&dir).is_some() {
                return Ok(json!({"status": "playing"}));
            }
            if r.state != crate::handoff::queue::State::Queued {
                return Err(crate::Error::new("busy", format!("{id} is {}", r.state.name())));
            }
            let live = crate::handoff::browser::Live::start(&root, &dir)?;
            st.plays.lock().unwrap().insert(dir, live);
            Ok(json!({"status": "playing"}))
        }
        "window" => {
            if r.state != crate::handoff::queue::State::Queued {
                return Err(crate::Error::new("busy", format!("{id} is {}", r.state.name())));
            }
            let pid = crate::handoff::spawn_window(&root, &dir)?;
            Ok(json!({"status": "opening", "pid": pid}))
        }
        "decline" => {
            let note = req.body["note"].as_str().unwrap_or("").trim();
            if note.is_empty() {
                return Err(crate::Error::invalid("a decline needs a note: it is what the agent gets"));
            }
            crate::handoff::queue::decline(&dir, note)
        }
        _ => Err(crate::Error::invalid(format!("{}: no such action", req.path))),
    }
}

// ---------------------------------------------------------------- data

/// The port's status (or why there is none), its progress lines and what
/// wants the person's attention.
/// A project's messages, with what each side has seen.
fn messages(root: &Path) -> Value {
    let all = crate::messages::all(root);
    let (agent, person) = (crate::messages::seen(root, "agent"), crate::messages::seen(root, "person"));
    let unread = all.iter().enumerate().filter(|(i, m)| *i >= person && m["from"] == "agent").count();
    json!({"messages": all, "agent_seen": agent, "person_seen": person, "unread": unread})
}

fn overview(root: &Path) -> Value {
    let port = root.join(crate::port::GOAL).is_file();
    let status = if port {
        match crate::port::status::look(root) {
            Ok(v) => v,
            Err(e) => json!({"error": e.to_value()["error"]}),
        }
    } else {
        Value::Null
    };
    let progress = jsonl(&root.join(crate::port::status::PROGRESS));
    let recent = recent_calls(root, 400);
    let attention = attention(root, &status, &progress, &recent);
    let last = recent.iter().rev().find_map(|c| c["t"].as_u64());
    let unread = messages(root)["unread"].clone();
    json!({"port": port, "status": status, "progress": progress, "attention": attention, "unread_messages": unread,
           "last_call": last, "last_commit": last_commit(root), "now": crate::now_ms() as u64})
}

fn jsonl(p: &Path) -> Vec<Value> {
    std::fs::read_to_string(p)
        .map(|t| t.lines().filter_map(|l| serde_json::from_str(l).ok()).collect())
        .unwrap_or_default()
}

/// The last commit's time (ms), from git.
fn last_commit(root: &Path) -> Option<u64> {
    let out = std::process::Command::new("git")
        .args(["log", "-1", "--format=%ct"])
        .current_dir(root)
        .stderr(std::process::Stdio::null())
        .output()
        .ok()?;
    String::from_utf8_lossy(&out.stdout).trim().parse::<u64>().ok().map(|s| s * 1000)
}

/// How much of the call log the page is sent at first.
const TAIL: u64 = 4 * 1024 * 1024;

/// The call log's lines from byte `since` (the page's last `next`), or its
/// last [`TAIL`] bytes. A log rotated since (shorter than `since`) starts
/// again: `reset` says so.
fn calls(root: &Path, since: Option<u64>) -> Value {
    let path = root.join(crate::calls::LOG);
    let Ok(mut f) = std::fs::File::open(&path) else { return json!({"lines": [], "next": 0, "reset": true}) };
    let len = f.metadata().map(|m| m.len()).unwrap_or(0);
    let (from, reset) = match since {
        Some(s) if s <= len => (s, false),
        _ => (len.saturating_sub(TAIL), true),
    };
    let mut text = String::new();
    let _ = f.seek(SeekFrom::Start(from)).and_then(|_| f.read_to_string(&mut text).map(|_| ()));
    // Only whole lines: one still being written waits for the next call.
    let whole = text.rfind('\n').map_or(0, |i| i + 1);
    let mut body = &text[..whole];
    if reset && from > 0 {
        body = body.split_once('\n').map_or("", |(_, b)| b);
    }
    let lines: Vec<Value> = body.lines().filter_map(|l| serde_json::from_str(l).ok()).collect();
    json!({"lines": lines, "next": from + whole as u64, "reset": reset})
}

/// The last `n` calls, start and end merged.
fn recent_calls(root: &Path, n: usize) -> Vec<Value> {
    let v = calls(root, None);
    let mut by_id: Vec<Value> = Vec::new();
    let mut index: HashMap<String, usize> = HashMap::new();
    for l in v["lines"].as_array().into_iter().flatten() {
        let id = l["id"].as_str().unwrap_or("").to_string();
        if l["event"] == "start" {
            index.insert(id, by_id.len());
            by_id.push(l.clone());
        } else if let Some(&i) = index.get(&id) {
            let c = &mut by_id[i];
            for k in ["exit", "ms", "bytes", "summary", "error", "kept"] {
                if let Some(x) = l.get(k) {
                    c[k] = x.clone();
                }
            }
            c["ended"] = l["t"].clone();
        }
    }
    let skip = by_id.len().saturating_sub(n);
    by_id.split_off(skip)
}

/// What wants the person: the port asking for them, a loop, passing
/// recordings falling, a call failing the same way again and again, psx
/// itself failing, work going on long without a commit, and quiet.
fn attention(root: &Path, status: &Value, progress: &[Value], calls: &[Value]) -> Vec<Value> {
    let mut out = Vec::new();
    // Play requests first: they are what the person can act on at once.
    let plays = crate::handoff::queue::waiting(root);
    let queued: Vec<_> = plays.iter().filter(|r| r.state == crate::handoff::queue::State::Queued).collect();
    if !queued.is_empty() {
        let n = queued.len();
        out.push(json!({"level": "act", "view": "requests",
            "title": format!("{n} play request{} for you", if n == 1 { "" } else { "s" }),
            "detail": queued.iter().take(3).map(|r| r.text().lines().next().unwrap_or("").to_string()).collect::<Vec<_>>().join("; ")}));
    }
    let mut add = |level: &str, title: String, detail: String| {
        out.push(json!({"level": level, "title": title, "detail": detail}))
    };
    if let Some(e) = status.get("error") {
        add("act", "port status fails".into(), e["message"].as_str().unwrap_or("").into());
    }
    if let Some(p) = status["problems"].as_array().filter(|p| !p.is_empty()) {
        add(
            "warn",
            format!("{} problem{} in the port", p.len(), if p.len() == 1 { "" } else { "s" }),
            p.iter().take(3).filter_map(|x| x["problem"].as_str()).collect::<Vec<_>>().join("; "),
        );
    }
    // The most passing recordings the port had, against now.
    let pass = |l: &Value| l["recordings"]["pass"].as_u64();
    let (best, now) = (progress.iter().filter_map(pass).max(), progress.last().and_then(pass));
    if let (Some(best), Some(now)) = (best.zip(now).filter(|(b, n)| n < b).map(|x| x.0), now) {
        add(
            "warn",
            format!("passing recordings fell from {best} to {now}"),
            "a change broke recordings that passed before: the progress history shows where".into(),
        );
    }
    // The same call failing again and again.
    let mut failing: HashMap<String, (usize, String)> = HashMap::new();
    for c in calls.iter().filter(|c| c["exit"].as_u64().is_some_and(|e| e != 0)) {
        let key = format!("{} {}", c["tool"].as_str().unwrap_or(""), c["args"]);
        let e = failing.entry(key).or_insert((0, String::new()));
        e.0 += 1;
        e.1 = c["summary"].as_str().unwrap_or("").to_string();
    }
    let mut repeated: Vec<(String, (usize, String))> = failing.into_iter().filter(|(_, (n, _))| *n >= 3).collect();
    repeated.sort_by_key(|r| std::cmp::Reverse(r.1.0));
    for (key, (n, summary)) in repeated.into_iter().take(3) {
        let tool = key.split_once(' ').map_or(key.as_str(), |(t, _)| t);
        add("warn", format!("{tool} failed {n} times with the same arguments"), summary);
    }
    for c in calls
        .iter()
        .rev()
        .filter(|c| matches!(c["error"].as_str(), Some("internal" | "worker_failed" | "panic" | "worker_crashed")))
        .take(2)
    {
        add(
            "warn",
            format!("psx failed inside: {}", c["tool"].as_str().unwrap_or("")),
            c["summary"].as_str().unwrap_or("").into(),
        );
    }
    let now = crate::now_ms() as u64;
    let last = calls.iter().rev().find_map(|c| c["t"].as_u64());
    if let (Some(last), Some(commit)) = (last, last_commit(root)) {
        let since_commit = calls.iter().filter(|c| c["t"].as_u64().is_some_and(|t| t > commit)).count();
        if now - last < 30 * 60 * 1000 && last > commit && last - commit > 3 * 3600 * 1000 && since_commit >= 20 {
            add(
                "info",
                format!("{since_commit} calls since the last commit, {} ago", ago(now - commit)),
                "work going on long without a commit".into(),
            );
        }
    }
    if let Some(last) = last.filter(|l| now - l > 3600 * 1000) {
        add("info", format!("quiet for {}", ago(now - last)), "no psx call since".into());
    }
    out
}

fn ago(ms: u64) -> String {
    let m = ms / 60_000;
    if m < 60 {
        format!("{m} min")
    } else if m < 48 * 60 {
        format!("{} h", m / 60)
    } else {
        format!("{} days", m / 1440)
    }
}

fn last_check(root: &Path) -> Value {
    std::fs::read(root.join(".psx/port/last.json"))
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

/// The scenarios and their recordings, each with what the last check said.
fn recordings(root: &Path) -> Value {
    let last = last_check(root);
    let reported = |id: &str, name: &str| {
        last["scenarios"]["scenarios"]
            .as_array()
            .into_iter()
            .flatten()
            .find(|s| s["id"] == id)
            .and_then(|s| s["recordings"].as_array()?.iter().find(|r| r["name"] == name).cloned())
    };
    let held_out = crate::port::held_out::listed(root);
    let scenarios: Vec<Value> = crate::scenario::ids(root)
        .into_iter()
        .map(|id| {
            let dir = root.join(crate::scenario::SCENARIOS).join(&id);
            let mut names: Vec<String> = std::fs::read_dir(&dir)
                .into_iter()
                .flatten()
                .flatten()
                .map(|e| e.path())
                .filter(|p| p.extension().is_some_and(|x| x == "jsonl"))
                .filter_map(|p| p.file_stem().map(|s| s.to_string_lossy().into_owned()))
                .collect();
            names.sort();
            let recs: Vec<Value> = names
                .iter()
                .map(|n| {
                    let r = reported(&id, n);
                    json!({"name": n, "checked": r.is_some(), "held_out": held_out.contains(&format!("{id}/{n}")),
                           "failures": r.as_ref().map_or(0, |r| r["failures"].as_array().map_or(0, Vec::len))})
                })
                .collect();
            json!({"id": id, "recordings": recs})
        })
        .collect();
    json!({"scenarios": scenarios, "check": {"commit": last["commit"], "at": last["at"]}})
}

// ---------------------------------------------------------------- replay

/// Where a recording's replay is cached: by what it replays (the scenario,
/// its chain, the recording, the glossary's binding sources) and psx's build.
fn replay_dir(root: &Path, id: &str, name: &str, decomp: bool) -> Option<PathBuf> {
    let read = |p: &Path| std::fs::read(p).unwrap_or_default();
    let scen = root.join(crate::scenario::SCENARIOS);
    let toml = read(&scen.join(format!("{id}.toml")));
    let mut all = root.display().to_string().into_bytes();
    all.extend(&toml);
    if let Ok(s) = psx_scenario::Scenario::parse(&String::from_utf8_lossy(&toml)) {
        all.extend(read(&root.join(&s.chain)));
    }
    all.extend(read(&scen.join(id).join(format!("{name}.jsonl"))));
    for dir in [root.join(crate::scenario::GLOSSARY), root.join(crate::scenario::GLOSSARY).join("binding")] {
        all.extend(read(&dir.join("Cargo.toml")));
        for (path, text) in crate::binding::sources(&dir).unwrap_or_default() {
            all.extend(format!("\n{path}\n{text}").bytes());
        }
    }
    all.extend(crate::context::version().to_string().bytes());
    if decomp {
        // With the agent's C: that C and the names it is built with.
        all.extend(
            format!("\ndecomp {} {}", crate::decomp::sources_hash(root), crate::decomp::names_hash(root)).bytes(),
        );
    }
    let key = crate::stamp::sha256(&all);
    Some(crate::bios::data_dir()?.join("web-replays").join(&key[..16]))
}

/// How many replays the cache keeps.
const KEEP_REPLAYS: usize = 12;

/// How far the recording's replay is; with `start`, start it unless it ran
/// or runs (again, when it failed: the state may be back).
fn replay(st: &State, root: &Path, id: &str, name: &str, start: bool, decomp: bool) -> Value {
    let Some(dir) = replay_dir(root, id, name, decomp) else {
        return json!({"error": "no data directory for the replay cache"});
    };
    let key = dir.display().to_string();
    let job = st
        .replays
        .lock()
        .unwrap()
        .entry(key)
        .or_insert_with(|| Arc::new(Mutex::new(Replay { dir: dir.clone(), ..Default::default() })))
        .clone();
    let mut j = job.lock().unwrap();
    if start && !j.running && j.done.as_ref().is_some_and(|d| d.get("error").is_some()) {
        j.done = None;
    }
    if start && j.done.is_none() && !j.running {
        if let Some(done) = std::fs::read(dir.join("done.json")).ok().and_then(|b| serde_json::from_slice(&b).ok()) {
            j.done = Some(done);
        } else {
            prune_replays(&dir);
            j.running = true;
            let (root, id, name, job) = (root.to_path_buf(), id.to_string(), name.to_string(), job.clone());
            std::thread::spawn(move || {
                let dir = job.lock().unwrap().dir.clone();
                let out = crate::self_exe().and_then(|exe| {
                    std::process::Command::new(exe)
                        .arg("--root")
                        .arg(&root)
                        .args(["__web-replay", &id, &name])
                        .arg(&dir)
                        .args(decomp.then_some("--decomp"))
                        .stdin(std::process::Stdio::null())
                        .stderr(std::process::Stdio::null())
                        .output()
                });
                let done = match out {
                    Ok(o) => serde_json::from_slice::<Value>(&o.stdout)
                        .unwrap_or_else(|_| json!({"error": {"code": "internal", "message": format!("the replay ended without a result ({})", o.status)}})),
                    Err(e) => json!({"error": {"code": "internal", "message": format!("psx: {e}")}}),
                };
                let mut j = job.lock().unwrap();
                j.running = false;
                j.done = Some(done);
            });
        }
    }
    let frames = std::fs::read_dir(&j.dir)
        .map_or(0, |d| d.flatten().filter(|e| e.path().extension().is_some_and(|x| x == "png")).count());
    json!({"running": j.running, "frames": frames, "done": j.done})
}

/// Room for one more replay: the oldest past [`KEEP_REPLAYS`] go.
fn prune_replays(keep: &Path) {
    let Some(parent) = keep.parent() else { return };
    let mut dirs: Vec<(SystemTime, PathBuf)> = std::fs::read_dir(parent)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.path() != keep)
        .filter_map(|e| Some((e.metadata().ok()?.modified().ok()?, e.path())))
        .collect();
    dirs.sort();
    while dirs.len() >= KEEP_REPLAYS {
        let (_, d) = dirs.remove(0);
        let _ = std::fs::remove_dir_all(d);
    }
}

// ----------------------------------------------------------- port replay

/// Where the port's frames for a recording are kept, in the project: by the
/// recording, its scenario and the port's code (the workspace as it is now).
const PORT_FRAMES: &str = ".psx/port-frames";

/// The project's notes and recordings: not the port's code.
const NOT_CODE: [&str; 8] =
    ["scenarios", "catalogue", "notes", "formats", "goal.toml", "review.md", "README.md", "AGENTS.md"];

fn port_key(root: &Path, id: &str, name: &str) -> String {
    let read = |p: &Path| std::fs::read(p).unwrap_or_default();
    let scen = root.join(crate::scenario::SCENARIOS);
    let mut all = read(&scen.join(format!("{id}.toml")));
    all.extend(read(&scen.join(id).join(format!("{name}.jsonl"))));
    all.extend(code_tree(root).unwrap_or_default());
    crate::stamp::sha256(&all)[..16].to_string()
}

/// The git tree of the workspace's files as they are, committed or not
/// (what .gitignore leaves), the notes left out: whatever the port is
/// written in, a change to it is a change to the tree. Staged in an index
/// of its own, so the project's index is untouched.
fn code_tree(root: &Path) -> Option<Vec<u8>> {
    let index = root.join(".psx/port-key.index");
    std::fs::create_dir_all(index.parent()?).ok()?;
    let _ = std::fs::remove_file(&index);
    let git = |args: &[&str]| {
        std::process::Command::new("git")
            .args(args)
            .current_dir(root)
            .env("GIT_INDEX_FILE", &index)
            .output()
            .ok()
            .filter(|o| o.status.success())
            .map(|o| o.stdout)
    };
    let mut add = vec!["add", "-A", "--", "."];
    let excludes: Vec<String> = NOT_CODE.iter().map(|p| format!(":(exclude){p}")).collect();
    add.extend(excludes.iter().map(String::as_str));
    let tree = git(&add).and_then(|_| git(&["write-tree"]));
    let _ = std::fs::remove_file(&index);
    tree
}

/// The port drawing the recording, as `replay` the original: the port
/// built and run by its psx-port.toml (psx_scenario::replay drives it), a
/// PNG a tick into `.psx/port-frames/<key>/`. `key` names the frames for
/// `port_frame`.
fn port_replay(st: &State, root: &Path, id: &str, name: &str, start: bool) -> Value {
    if !root.join(crate::port::manifest::FILE).is_file() && !root.join("game/Cargo.toml").is_file() {
        return json!({"error": {"code": "invalid", "message": "no port here (psx-port.toml)"}});
    }
    let jobs_key = format!("port:{}:{id}:{name}", root.display());
    // Held until this request has its job and has marked it running, so two
    // quick requests cannot both start a build into the same folder.
    let mut jobs = st.replays.lock().unwrap();
    let job = jobs.get(&jobs_key).cloned();
    // A job running; otherwise the key is worked out again, for the code now.
    let drawn = |dir: &Path| {
        std::fs::read(dir.join("frames.json"))
            .ok()
            .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
            .filter(|v| v.get("error").is_none())
    };
    let job = match job {
        Some(j) if j.lock().unwrap().running => j,
        last => {
            let dir = root.join(PORT_FRAMES).join(port_key(root, id, name));
            // How the last run for this code ended (a failure is not kept on disk).
            if let Some(j) = last.filter(|j| !start && j.lock().unwrap().dir == dir) {
                return port_state(&j.lock().unwrap());
            }
            // Without start, only frames drawn before for this code.
            let done = drawn(&dir);
            if !start && done.is_none() {
                return json!({"running": false, "frames": 0, "done": null});
            }
            let j = Arc::new(Mutex::new(Replay { dir, done, ..Default::default() }));
            jobs.insert(jobs_key, j.clone());
            j
        }
    };
    let mut j = job.lock().unwrap();
    if start && !j.running && j.done.is_none() {
        match drawn(&j.dir) {
            // Kept from before.
            Some(v) => j.done = Some(v),
            None => {
                prune_replays(&j.dir);
                let _ = std::fs::remove_dir_all(&j.dir);
                j.running = true;
                j.progress = "building the port".into();
                let (root, rec, job) =
                    (root.to_path_buf(), format!("{}/{id}/{name}.jsonl", crate::scenario::SCENARIOS), job.clone());
                std::thread::spawn(move || run_port_replay(&root, &rec, &job));
            }
        }
    }
    drop(jobs);
    port_state(&j)
}

fn port_state(j: &Replay) -> Value {
    let frames = std::fs::read_dir(&j.dir)
        .map_or(0, |d| d.flatten().filter(|e| e.path().extension().is_some_and(|x| x == "png")).count());
    let key = j.dir.file_name().map(|n| n.to_string_lossy().into_owned());
    json!({"key": key, "running": j.running, "progress": j.progress, "frames": frames, "done": j.done})
}

/// Draws a recording with the port: built, started in protocol mode
/// ([`crate::port::process`]) and driven by psx_scenario's replay, which
/// writes the frames and `frames.json`.
fn run_port_replay(root: &Path, recording: &str, job: &Arc<Mutex<Replay>>) {
    let dir = job.lock().unwrap().dir.clone();
    let done = match crate::port::process::start(root) {
        Err(e) => json!({"error": e}),
        Ok(process) => {
            let progress = |line: &str| {
                if line.starts_with("tick ") {
                    job.lock().unwrap().progress = format!("drawing {line}");
                }
            };
            // The port's panics come back as panics: the replay records a
            // tick's; one in its launch is the run's error.
            let run = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                psx_scenario::replay::run_with(process.port(), &root.join(recording), &dir, progress)
            }));
            match run {
                Err(p) => json!({"error": format!("the port panicked: {}", psx_scenario::runner::panic_message(&p))}),
                Ok(_) => std::fs::read(dir.join("frames.json"))
                    .ok()
                    .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
                    .unwrap_or_else(|| json!({"error": "the replay wrote no frames.json"})),
            }
        }
    };
    let mut j = job.lock().unwrap();
    j.running = false;
    j.progress.clear();
    j.done = Some(done);
}

fn port_frame(root: &Path, key: &str, tick: &str) -> Reply {
    let (true, Ok(tick)) = (key.len() == 16 && key.bytes().all(|b| b.is_ascii_hexdigit()), tick.parse::<u64>()) else {
        return Reply::Missing("no such frame".into());
    };
    match std::fs::read(root.join(PORT_FRAMES).join(key).join(format!("{tick}.png"))) {
        Ok(b) => Reply::Bytes("image/png", b),
        Err(_) => Reply::Missing(format!("tick {tick}: not drawn yet")),
    }
}

/// `/api/p/I/file?path=P`: a PNG or a CSV in the project (a run's screen,
/// a mark's, a decoded texture, a run's samples) for the log's cards; P is
/// relative to the root or absolute inside it.
fn project_file(root: &Path, path: &str) -> Reply {
    const LIMIT: u64 = 16 * 1024 * 1024;
    let p = if Path::new(path).is_absolute() { PathBuf::from(path) } else { root.join(path) };
    let kind = match p.extension().and_then(|e| e.to_str()).map(str::to_ascii_lowercase).as_deref() {
        Some("png") => "image/png",
        Some("csv") => "text/csv; charset=utf-8",
        _ => "",
    };
    let inside = match (std::fs::canonicalize(&p), std::fs::canonicalize(root)) {
        (Ok(p), Ok(r)) => p.starts_with(r) && std::fs::metadata(&p).is_ok_and(|m| m.is_file() && m.len() <= LIMIT),
        _ => false,
    };
    match (!kind.is_empty() && inside).then(|| std::fs::read(&p).ok()).flatten() {
        Some(b) => Reply::Bytes(kind, b),
        None => Reply::Missing(format!("{path}: no such image or table in the project")),
    }
}

fn frame(st: &State, root: &Path, id: &str, name: &str, tick: &str, decomp: bool) -> Reply {
    let _ = st;
    let (Some(dir), Ok(tick)) = (replay_dir(root, id, name, decomp), tick.parse::<u64>()) else {
        return Reply::Missing("no such frame".into());
    };
    match std::fs::read(dir.join(format!("{tick}.png"))) {
        Ok(b) => Reply::Bytes("image/png", b),
        Err(_) => Reply::Missing(format!("tick {tick}: not replayed yet")),
    }
}

/// `psx __web-replay ID NAME DIR [--decomp]`: replay the recording (with
/// the agent's C for the functions whose check passed), the screen at the
/// end of each tick to `DIR/<tick>.png`, then (when it ran) `DIR/done.json`.
pub fn replay_child(ctx: &Ctx, id: &str, name: &str, dir: &Path, decomp: bool) -> Outcome {
    Outcome::result(replay_frames(ctx, id, name, dir, decomp, None))
}

/// The original's replay of the recording into `dir` (a PNG a tick, then
/// `done.json` when it ran), as [`replay_child`] gives it.
fn replay_frames(
    ctx: &Ctx,
    id: &str,
    name: &str,
    dir: &Path,
    decomp: bool,
    only: Option<&BTreeSet<u64>>,
) -> crate::Result<Value> {
    std::fs::create_dir_all(dir).map_err(|e| crate::Error::io(dir, e))?;
    let c = decomp.then(|| crate::decomp::Replacement::new(&ctx.root, &[])).transpose()?;
    let mut install = |m: &mut psx_core::Machine| {
        if let Some(c) = &c {
            c.install(m);
        }
    };
    let mut failed = None;
    let r = crate::scenario::replay(
        ctx,
        id,
        name,
        Duration::from_secs(600),
        &mut |t, m| {
            if only.is_some_and(|s| !s.contains(&t.tick)) {
                return;
            }
            let (w, h, rgba) = m.bus.gpu.display_rgba();
            let p = dir.join(format!("{}.png", t.tick));
            if let Err(e) = crate::run::write_png(&p, w, h, &rgba, png::ColorType::Rgba) {
                failed.get_or_insert(format!("{}: {e}", p.display()));
            }
        },
        Some(&mut install),
        None,
    );
    let v = match (r, failed) {
        (Ok((diffs, m)), None) => {
            let mut v = json!({"differences": diffs});
            if let Some(c) = &c {
                v["decomp"] = c.report(&m);
            }
            v
        }
        (Ok(_), Some(e)) => crate::Error::invalid(e).to_value(),
        (Err(e), _) => e.to_value(),
    };
    // Only a replay that ran is kept: a failed one runs again next time.
    if v.get("error").is_none() {
        let _ = crate::write_atomic(&dir.join("done.json"), v.to_string().as_bytes());
    }
    Ok(v)
}

/// What identifies the original's replay of a recording, for keeping its
/// frames: changes with the recording, its scenario or the glossary.
pub(crate) fn replay_key(root: &Path, id: &str, name: &str) -> Option<String> {
    Some(replay_dir(root, id, name, false)?.file_name()?.to_string_lossy().into_owned())
}

/// The original's screens at `only` into `dir`, unless already there.
pub(crate) fn sampled_original(
    ctx: &Ctx,
    id: &str,
    name: &str,
    only: &BTreeSet<u64>,
    dir: &Path,
) -> Result<(), String> {
    if dir.join("done.json").is_file() {
        return Ok(());
    }
    let v = replay_frames(ctx, id, name, dir, false, Some(only)).unwrap_or_else(|e| e.to_value());
    match v.get("error") {
        Some(e) => Err(e["message"].as_str().unwrap_or("the original's replay failed").to_string()),
        None => Ok(()),
    }
}

/// The port's screens at `only` into `dir`, drawn afresh. A port that
/// stopped part way keeps what it drew, with why as the note.
pub(crate) fn sampled_port(
    root: &Path,
    id: &str,
    name: &str,
    only: &BTreeSet<u64>,
    dir: &Path,
) -> Result<Option<String>, String> {
    let _ = std::fs::remove_dir_all(dir);
    let rec = root.join(crate::scenario::SCENARIOS).join(id).join(format!("{name}.jsonl"));
    let process = crate::port::process::start(root)?;
    let run = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        psx_scenario::replay::run_sampled(process.port(), &rec, dir, only)
    }));
    if let Err(p) = run {
        return Ok(Some(format!("the port panicked: {}", psx_scenario::runner::panic_message(&p))));
    }
    let frames = std::fs::read(dir.join("frames.json")).ok().and_then(|b| serde_json::from_slice::<Value>(&b).ok());
    match frames {
        Some(v) if v["no_screen"] == true => Err("the port draws no screen yet".into()),
        Some(v) => Ok(v["error"].as_str().map(str::to_string)),
        None => Ok(None),
    }
}

/// `scenario show --screens`: the original's screen and the port's at
/// each of `ticks`, copied into `out` as `<tick>-original.png` and
/// `<tick>-port.png` from the frames psx web keeps (drawn first when they
/// are not there). What kept either from drawing is said, not failed.
pub(crate) fn screens(ctx: &Ctx, id: &str, name: &str, ticks: &[u64], out: &Path) -> crate::Result<Value> {
    let root = &ctx.root;
    std::fs::create_dir_all(out).map_err(|e| crate::Error::io(out, e))?;
    let mut notes = serde_json::Map::new();
    let original =
        replay_dir(root, id, name, false).ok_or_else(|| crate::Error::invalid("no data directory for the replays"))?;
    if !original.join("done.json").is_file() {
        prune_replays(&original);
        let v = replay_frames(ctx, id, name, &original, false, None).unwrap_or_else(|e| e.to_value());
        if let Some(e) = v.get("error") {
            notes.insert("original".into(), e["message"].clone());
        }
    }
    let port = root.join(PORT_FRAMES).join(port_key(root, id, name));
    let drawn =
        |dir: &Path| std::fs::read(dir.join("frames.json")).ok().and_then(|b| serde_json::from_slice::<Value>(&b).ok());
    if !root.join(crate::port::manifest::FILE).is_file() && !root.join("game/Cargo.toml").is_file() {
        notes.insert("port".into(), json!("no port here (psx-port.toml)"));
    } else {
        if drawn(&port).is_none_or(|v| v.get("error").is_some()) {
            prune_replays(&port);
            let _ = std::fs::remove_dir_all(&port);
            let rec = root.join(crate::scenario::SCENARIOS).join(id).join(format!("{name}.jsonl"));
            match crate::port::process::start(root) {
                Err(e) => {
                    notes.insert("port".into(), json!(e));
                }
                Ok(process) => {
                    let run = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        psx_scenario::replay::run_with(process.port(), &rec, &port, |_| {})
                    }));
                    if let Err(p) = run {
                        notes.insert(
                            "port".into(),
                            json!(format!("the port panicked: {}", psx_scenario::runner::panic_message(&p))),
                        );
                    }
                }
            }
        }
        match drawn(&port) {
            Some(v) if v["no_screen"] == true => {
                notes.insert("port".into(), json!("the port draws no screen yet"));
            }
            Some(v) if v.get("error").is_some() => {
                notes.entry("port").or_insert(json!(format!(
                    "drew {} ticks, then: {}",
                    v["drawn"],
                    v["error"].as_str().unwrap_or("")
                )));
            }
            _ => {}
        }
    }
    let copy = |from: &Path, tick: u64, side: &str| {
        let to = out.join(format!("{tick}-{side}.png"));
        std::fs::copy(from.join(format!("{tick}.png")), &to).ok().map(|_| to.display().to_string())
    };
    let shots: Vec<Value> = ticks
        .iter()
        .map(|&t| json!({"tick": t, "original": copy(&original, t, "original"), "port": copy(&port, t, "port")}))
        .collect();
    let mut v = json!({"run_dir": out, "screens": shots});
    if !notes.is_empty() {
        v["not_drawn"] = Value::Object(notes);
    }
    Ok(v)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_port_key_follows_the_code_whatever_its_language() {
        let dir = crate::port::tests::tempdir::Dir::new();
        let root = dir.0.as_path();
        let write = |p: &str, text: &str| {
            std::fs::create_dir_all(root.join(p).parent().unwrap()).unwrap();
            std::fs::write(root.join(p), text).unwrap();
        };
        write(".gitignore", ".psx/\nbuild/\n");
        write("game/main.c", "int main(void) { return 0; }\n");
        write("notes/a.md", "a\n");
        write("scenarios/s/r.jsonl", "{}\n");
        let git = |args: &[&str]| {
            let ok = std::process::Command::new("git")
                .args(["-c", "user.email=t@example.com", "-c", "user.name=t"])
                .args(args)
                .current_dir(root)
                .output()
                .unwrap()
                .status
                .success();
            assert!(ok, "git {args:?}");
        };
        git(&["init", "-q"]);
        git(&["add", "-A"]);
        git(&["commit", "-qm", "c"]);
        let key = || port_key(root, "s", "r");
        let first = key();
        assert_eq!(key(), first);
        // Notes and what .gitignore leaves out are not the code.
        write("notes/a.md", "b\n");
        write("build/game", "binary");
        assert_eq!(key(), first);
        // An edit, committed or not, and a new file are.
        write("game/main.c", "int main(void) { return 1; }\n");
        let edited = key();
        assert_ne!(edited, first);
        git(&["commit", "-qam", "d"]);
        assert_eq!(key(), edited);
        write("game/new.c", "\n");
        assert_ne!(key(), edited);
        // The recording is part of the key.
        assert_ne!(port_key(root, "s", "other"), key());
        // The project's own index is untouched.
        let status =
            std::process::Command::new("git").args(["status", "--porcelain"]).current_dir(root).output().unwrap();
        assert_eq!(String::from_utf8_lossy(&status.stdout), "?? game/new.c\n");
    }

    #[test]
    fn queries_are_decoded() {
        assert_eq!(decode("a%20b+c"), "a b c");
        assert_eq!(decode("100%"), "100%");
        assert_eq!(decode("%e2%9c%93"), "✓");
    }

    #[test]
    fn the_call_log_is_read_from_where_the_page_left_it() {
        let root = std::env::temp_dir().join(format!("psx-web-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join(".psx")).unwrap();
        let a = crate::calls::start(&root, "port_status", json!({}), json!({"via": "cli"})).unwrap();
        a.end(&json!({"item": "Record"}), 0);
        let first = calls(&root, None);
        assert_eq!(first["lines"].as_array().unwrap().len(), 2);
        let next = first["next"].as_u64().unwrap();
        assert_eq!(calls(&root, Some(next))["lines"], json!([]));
        // Half a line waits for the rest.
        let mut f = std::fs::OpenOptions::new().append(true).open(root.join(crate::calls::LOG)).unwrap();
        f.write_all(b"{\"id\":\"x\"").unwrap();
        assert_eq!(calls(&root, Some(next))["next"].as_u64(), Some(next));
        f.write_all(b",\"event\":\"start\",\"tool\":\"run\"}\n").unwrap();
        assert_eq!(calls(&root, Some(next))["lines"][0]["tool"], "run");
        // Rotated: shorter than where the page was.
        assert_eq!(calls(&root, Some(next * 10))["reset"], true);
        assert_eq!(recent_calls(&root, 10)[0]["summary"], "item: Record");
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn the_same_failing_call_wants_attention() {
        let failed = |n: u64| json!({"t": n, "tool": "run", "args": ["run", "--frames", "9"], "exit": 1, "summary": "invalid: no"});
        let calls: Vec<Value> = (0..3).map(failed).collect();
        let a = attention(Path::new("/nonexistent"), &Value::Null, &[], &calls);
        assert!(a.iter().any(|x| x["title"] == "run failed 3 times with the same arguments"), "{a:?}");
        let progress = [json!({"recordings": {"pass": 5}}), json!({"recordings": {"pass": 3}})];
        let a = attention(Path::new("/nonexistent"), &Value::Null, &progress, &[]);
        let titles: Vec<&str> = a.iter().filter_map(|x| x["title"].as_str()).collect();
        assert_eq!(titles, ["passing recordings fell from 5 to 3"]);
    }
}

#[cfg(test)]
mod recording_name_tests {
    use super::recording_ok;

    #[test]
    fn recording_names_cannot_climb_out_of_the_project() {
        assert!(recording_ok("start_1_1", "jump_tap"));
        assert!(recording_ok("world/start_1_1", "jump_tap"));
        for (id, name) in [
            ("..", "x"),
            ("a/../..", "x"),
            ("a", ".."),
            ("a", "b/c"),
            ("", "x"),
            ("a", ""),
            ("C:", "x"),
            ("a\\..", "x"),
        ] {
            assert!(!recording_ok(id, name), "{id} {name}");
        }
    }
}
