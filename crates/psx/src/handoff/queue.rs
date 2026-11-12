//! The play queue. Every `scenario play` waits in its run directory under
//! `.psx/handoffs/` until a person answers it from `psx web` or `psx play`:
//!
//! - `queued.json`: when it was asked, and the note of the request it asks
//!   again after (the same scenario recording, declined before);
//! - `claim.json`: someone is playing it (`live.json` kept fresh while they
//!   do); a claim whose player stopped without answering lapses and the
//!   request is queued again;
//! - `report.json`: the answer, recorded or declined with the person's note;
//! - `read`: the agent has read the answer.
//!
//! A run directory without `queued.json` is from before the queue.

use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use super::STALE_S;
use crate::Error;

pub const DIR: &str = ".psx/handoffs";
const QUEUED: &str = "queued.json";
const CLAIM: &str = "claim.json";
const READ: &str = "read";
/// A claim with no `live.json` this long after it was made has lapsed.
const CLAIM_START_S: u128 = 120;

/// Where a request stands.
#[derive(Clone, Debug, PartialEq)]
pub enum State {
    Queued,
    /// Claimed, with the player's latest `live.json`.
    Playing(Option<Value>),
    /// The report: recorded, declined, or an error.
    Answered(Value),
}

impl State {
    pub fn name(&self) -> &'static str {
        match self {
            State::Queued => "queued",
            State::Playing(_) => "playing",
            State::Answered(_) => "answered",
        }
    }
}

/// A request in the queue.
#[derive(Clone, Debug)]
pub struct Request {
    pub dir: PathBuf,
    /// request.json: the play's arguments.
    pub args: Value,
    pub queued_unix_ms: u64,
    /// The declined request this one asks again after: `{run_dir, note}`.
    pub after: Option<Value>,
    pub state: State,
    pub read: bool,
}

impl Request {
    /// The directory's name, which `psx play` and the web take as its id.
    pub fn id(&self) -> String {
        self.dir.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
    }

    /// The scenario recording it asks for, as `ID/NAME`.
    pub fn recording(&self) -> Option<String> {
        Some(format!("{}/{}", self.args["id"].as_str()?, self.args["name"].as_str()?))
    }

    pub fn json(&self) -> Value {
        let mut v = json!({
            "id": self.id(), "run_dir": self.dir, "request": self.args["request"],
            "queued_unix_ms": self.queued_unix_ms, "state": self.state.name(), "read": self.read,
        });
        for k in ["id", "name", "from", "expect", "person", "held_out"] {
            if !self.args[k].is_null() {
                v[if k == "id" { "scenario" } else { k }] = self.args[k].clone();
            }
        }
        if let Some(a) = &self.after {
            v["after"] = a.clone();
        }
        match &self.state {
            State::Playing(live) => v["live"] = json!(live),
            State::Answered(r) => {
                v["answer"] = json!({"status": r["status"], "note": r["note"], "error": r["error"],
                    "recording": r["recording"]["status"].as_str().or(r["recording"]["written"].as_bool().map(|w| if w { "written" } else { "not written" }))});
            }
            State::Queued => {}
        }
        v
    }

    /// What the window shows: the request, and the note it asks again after.
    pub fn text(&self) -> String {
        let request = self.args["request"].as_str().unwrap_or_default().to_string();
        match self.after.as_ref().and_then(|a| a["note"].as_str()).filter(|n| !n.trim().is_empty()) {
            Some(note) => format!("{request}\n(asked again after: {note})"),
            None => request,
        }
    }
}

use super::read_json;

fn age_ms(unix_ms: Option<u64>) -> Option<u128> {
    unix_ms.map(|t| crate::now_ms().saturating_sub(t as u128))
}

/// Whether a process is alive, where that can be told (`/proc`).
fn gone(pid: Option<u64>) -> bool {
    let proc = Path::new("/proc");
    pid.is_some_and(|p| proc.is_dir() && !proc.join(p.to_string()).exists())
}

/// A request's state; a lapsed claim is released, so it is queued again.
pub fn state(dir: &Path) -> State {
    if let Some(r) = read_json(&dir.join("report.json")) {
        return State::Answered(r);
    }
    let Some(claim) = read_json(&dir.join(CLAIM)) else { return State::Queued };
    let live = read_json(&dir.join("live.json"));
    let lapsed = match age_ms(live.as_ref().and_then(|l| l["updated_unix_ms"].as_u64())) {
        Some(age) => age > STALE_S * 1000 || gone(live.as_ref().and_then(|l| l["pid"].as_u64())),
        None => {
            age_ms(claim["unix_ms"].as_u64()).is_none_or(|a| a > CLAIM_START_S * 1000) || gone(claim["pid"].as_u64())
        }
    };
    if lapsed {
        release(dir);
        // An answer written as the claim lapsed.
        return read_json(&dir.join("report.json")).map_or(State::Queued, State::Answered);
    }
    State::Playing(live)
}

pub fn load(dir: &Path) -> Option<Request> {
    let queued = read_json(&dir.join(QUEUED))?;
    let args = read_json(&dir.join("request.json"))?;
    Some(Request {
        dir: dir.to_path_buf(),
        args,
        queued_unix_ms: queued["queued_unix_ms"].as_u64().unwrap_or(0),
        after: queued.get("after").filter(|a| !a.is_null()).cloned(),
        state: state(dir),
        read: dir.join(READ).exists(),
    })
}

/// The project's requests, oldest first.
pub fn list(root: &Path) -> Vec<Request> {
    let Ok(entries) = std::fs::read_dir(root.join(DIR)) else { return Vec::new() };
    let mut v: Vec<Request> = entries.filter_map(|e| e.ok()).filter_map(|e| load(&e.path())).collect();
    v.sort_by(|a, b| (a.queued_unix_ms, &a.dir).cmp(&(b.queued_unix_ms, &b.dir)));
    v
}

/// The requests still waiting for a person, oldest first.
pub fn waiting(root: &Path) -> Vec<Request> {
    list(root).into_iter().filter(|r| !matches!(r.state, State::Answered(_))).collect()
}

/// Put a request in the queue. A request for the same scenario recording
/// still queued is replaced; one declined before is carried as `after`.
pub fn enqueue(dir: &Path, args: &Value) -> crate::Result<Value> {
    let root_queue = dir.parent().map(Path::to_path_buf).unwrap_or_default();
    let recording = |a: &Value| Some((a["id"].as_str()?.to_string(), a["name"].as_str()?.to_string()));
    let mut after = Value::Null;
    let mut replaced = Vec::new();
    if let Some(rec) = recording(args) {
        for r in list_in(&root_queue).into_iter().filter(|r| recording(&r.args).as_ref() == Some(&rec)) {
            match &r.state {
                State::Queued => {
                    let _ = std::fs::remove_dir_all(&r.dir);
                    replaced.push(r.dir.clone());
                }
                State::Answered(report) if report["status"] == "declined" => {
                    after = json!({"run_dir": r.dir, "note": report["note"]});
                }
                _ => {}
            }
        }
    }
    let request = dir.join("request.json");
    std::fs::write(&request, serde_json::to_vec_pretty(args).unwrap()).map_err(|e| Error::io(&request, e))?;
    let queued = json!({"queued_unix_ms": crate::now_ms() as u64, "after": after});
    let q = dir.join(QUEUED);
    std::fs::write(&q, queued.to_string()).map_err(|e| Error::io(&q, e))?;
    let position = list_in(&root_queue).iter().filter(|r| r.state == State::Queued).count();
    let mut v = json!({
        "status": "queued", "run_dir": dir, "position": position,
        "note": "Queued for the person, who answers play requests when they can (psx web, or psx play). Go on with other work: port status lists it once they answer, recorded or declined with their note, and scenario_play with status returns the answer.",
        "status_call": {"tool": "scenario_play", "arguments": {"status": dir}},
    });
    if !after.is_null() {
        v["after"] = after;
    }
    if !replaced.is_empty() {
        v["replaced"] = json!(replaced);
    }
    Ok(v)
}

pub fn list_in(queue: &Path) -> Vec<Request> {
    match queue.parent().and_then(Path::parent) {
        Some(root) if queue.ends_with(DIR) => list(root),
        _ => std::fs::read_dir(queue)
            .map(|e| e.filter_map(|e| e.ok()).filter_map(|e| load(&e.path())).collect())
            .unwrap_or_default(),
    }
}

/// Claim a queued request for a player; fails when it is not queued.
pub fn claim(dir: &Path, by: &str) -> crate::Result<()> {
    match state(dir) {
        State::Queued => {}
        State::Playing(_) => return Err(Error::new("busy", "someone is playing this request")),
        State::Answered(_) => return Err(Error::new("answered", "this request is already answered")),
    }
    let p = dir.join(CLAIM);
    let claim = json!({"by": by, "pid": std::process::id(), "unix_ms": crate::now_ms() as u64});
    let mut f = std::fs::OpenOptions::new().write(true).create_new(true).open(&p).map_err(|e| match e.kind() {
        std::io::ErrorKind::AlreadyExists => Error::new("busy", "someone is playing this request"),
        _ => Error::io(&p, e),
    })?;
    std::io::Write::write_all(&mut f, claim.to_string().as_bytes()).map_err(|e| Error::io(&p, e))
}

/// Give a claimed request back to the queue, leaving what the play left.
pub fn release(dir: &Path) {
    for f in ["live.json", "marks"] {
        let p = dir.join(f);
        let _ = if p.is_dir() { std::fs::remove_dir_all(&p) } else { std::fs::remove_file(&p) };
    }
    let _ = std::fs::remove_file(dir.join(CLAIM));
}

/// Answer a request without playing it: the person's note goes to the agent.
pub fn decline(dir: &Path, note: &str) -> crate::Result<Value> {
    let r = load(dir).ok_or_else(|| Error::invalid(format!("{} is not a queued play request", dir.display())))?;
    if let State::Answered(_) = r.state {
        return Err(Error::new("answered", "this request is already answered"));
    }
    if let State::Playing(_) = r.state {
        return Err(Error::new("busy", "someone is playing this request"));
    }
    let mut v = json!({
        "status": "declined",
        "summary": "The person declined without playing; read their note.",
        "request": r.args["request"],
        "note": note,
        "run_dir": dir,
        "report": dir.join("report.json"),
    });
    if let Some(rec) = r.recording() {
        v["recording"] = json!({"written": false, "name": rec,
            "note": "declined without playing: read the note, then ask again differently or go on without this recording"});
    }
    let p = dir.join("report.json");
    crate::write_atomic(&p, serde_json::to_vec_pretty(&v).unwrap().as_slice()).map_err(|e| Error::io(&p, e))?;
    Ok(v)
}

/// The agent has read the answer.
pub fn mark_read(dir: &Path) {
    let _ = std::fs::write(dir.join(READ), "");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_request_is_queued_claimed_released_and_answered() {
        let root = std::env::temp_dir().join(format!("psx-queue-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let dir = |n: &str| {
            let d = root.join(DIR).join(n);
            std::fs::create_dir_all(&d).unwrap();
            d
        };
        let args = json!({"request": "walk to the door", "id": "s1", "name": "door"});
        let a = dir("1");
        assert_eq!(enqueue(&a, &args).unwrap()["position"], 1);
        assert_eq!(state(&a), State::Queued);
        claim(&a, "test").unwrap();
        assert_eq!(claim(&a, "test").unwrap_err().code, "busy");
        // No live.json yet: playing until the claim lapses.
        assert!(matches!(state(&a), State::Playing(None)));
        std::fs::write(a.join("live.json"), json!({"updated_unix_ms": 0}).to_string()).unwrap();
        assert_eq!(state(&a), State::Queued, "a stale player gives the request back");
        assert!(!a.join(CLAIM).exists() && !a.join("live.json").exists());
        let declined = decline(&a, "the door is locked").unwrap();
        assert_eq!(declined["recording"]["name"], "s1/door");
        assert!(matches!(state(&a), State::Answered(_)));
        assert_eq!(decline(&a, "again").unwrap_err().code, "answered");
        // Asked again: it carries the note; asked a third time while queued, it replaces it.
        let b = dir("2");
        assert_eq!(enqueue(&b, &args).unwrap()["after"]["note"], "the door is locked");
        let c = dir("3");
        let v = enqueue(&c, &args).unwrap();
        assert_eq!((v["replaced"][0].clone(), v["position"].clone()), (json!(b), json!(1)));
        let all = list(&root);
        assert_eq!(all.iter().map(|r| r.id()).collect::<Vec<_>>(), ["1", "3"]);
        assert!(all[1].text().ends_with("(asked again after: the door is locked)"));
        assert_eq!(waiting(&root).len(), 1);
        assert!(!all[0].read);
        mark_read(&a);
        assert!(load(&a).unwrap().read);
        let _ = std::fs::remove_dir_all(root);
    }
}
