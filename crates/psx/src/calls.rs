//! The call log `psx web` reads: every psx command, from the CLI or an MCP
//! client, appends a start line and an end line to `<root>/.psx/calls.jsonl`
//! and keeps its result in `.psx/calls/<id>.json`.
//!
//! Only a project that already has a `.psx` directory logs, so a command run
//! anywhere else leaves nothing behind. The log is rotated past
//! [`ROTATE_BYTES`] (one older file kept) and results past [`KEEP_RESULTS`]
//! are removed oldest first. Logging never fails a command: what cannot be
//! written is dropped.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use serde_json::{Value, json};

pub const LOG: &str = ".psx/calls.jsonl";
pub const RESULTS: &str = ".psx/calls";
const ROTATE_BYTES: u64 = 32 * 1024 * 1024;
const KEEP_RESULTS: usize = 2000;
/// Larger results are not kept (the line still says how large they were).
const RESULT_LIMIT: usize = 4 * 1024 * 1024;
/// Larger arguments are logged as their size only.
const ARGS_LIMIT: usize = 64 * 1024;

/// Who made a call: the front end, this process and the one that started it
/// (the agent's harness or a shell), and for MCP the client's name.
pub fn caller(via: &str, client: Option<String>) -> Value {
    let pid = std::process::id();
    let ppid = parent_id();
    let mut c = json!({"via": via, "pid": pid});
    if let Some(ppid) = ppid {
        c["ppid"] = json!(ppid);
        if let Some(name) = process_name(ppid) {
            c["parent"] = json!(name);
        }
    }
    if let Some(client) = client {
        c["client"] = json!(client);
    }
    if let Some((pid, name)) = session(ppid) {
        c["session"] = json!({"pid": pid, "name": name});
    }
    c
}

/// The process a call belongs to: the first one up from `ppid` that is not
/// a shell or a wrapper, so the commands an agent runs through its shell
/// tool group under the agent.
fn session(ppid: Option<u32>) -> Option<(u32, String)> {
    const PASS: [&str; 9] = ["sh", "bash", "dash", "zsh", "fish", "env", "timeout", "nice", "xargs"];
    let mut pid = ppid?;
    for _ in 0..8 {
        let name = process_name(pid)?;
        if !PASS.contains(&name.as_str()) {
            return Some((pid, name));
        }
        pid = parent_of(pid)?;
    }
    None
}

/// A process's parent, from `/proc/<pid>/stat` (after the name, which may
/// hold spaces: state, then the parent's id).
fn parent_of(pid: u32) -> Option<u32> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    stat.rsplit_once(')')?.1.split_whitespace().nth(1)?.parse().ok().filter(|p| *p > 1)
}

#[cfg(unix)]
fn parent_id() -> Option<u32> {
    Some(std::os::unix::process::parent_id())
}
#[cfg(not(unix))]
fn parent_id() -> Option<u32> {
    None
}

fn process_name(pid: u32) -> Option<String> {
    std::fs::read_to_string(format!("/proc/{pid}/comm")).ok().map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
}

/// A logged call in progress.
pub struct Call {
    root: PathBuf,
    id: String,
    started: std::time::Instant,
}

/// Log the start of `tool` (MCP's tool name; the CLI's subcommands joined
/// with `_`) in `root`, or nothing when `root` has no `.psx`.
pub fn start(root: &Path, tool: &str, args: Value, caller: Value) -> Option<Call> {
    if !root.join(".psx").is_dir() {
        return None;
    }
    static N: AtomicU64 = AtomicU64::new(0);
    let id = format!("{}-{}-{}", crate::now_ms(), std::process::id(), N.fetch_add(1, Ordering::Relaxed));
    let log = root.join(LOG);
    if std::fs::metadata(&log).is_ok_and(|m| m.len() > ROTATE_BYTES) {
        let _ = std::fs::rename(&log, root.join(".psx/calls.1.jsonl"));
    }
    let size = serde_json::to_string(&args).map_or(0, |s| s.len());
    // A reviewer's report is for the person alone (see `reports`): the log is
    // in the project, where the port's writer reads it.
    let args = if tool == "report" {
        json!({"omitted": "the report, for the person only"})
    } else if size > ARGS_LIMIT {
        json!({"omitted_bytes": size})
    } else {
        args
    };
    append(
        &log,
        &json!({"id": id, "t": crate::now_ms() as u64, "event": "start", "tool": tool, "args": args, "caller": caller}),
    );
    Some(Call { root: root.to_path_buf(), id, started: std::time::Instant::now() })
}

impl Call {
    /// Where the screen of a game this call runs goes ([`crate::live`]).
    pub fn live_path(&self) -> PathBuf {
        self.root.join(RESULTS).join(format!("{}.live.png", self.id))
    }

    /// Log the end: exit code, duration, size, a one-line summary and the
    /// result kept beside the log.
    pub fn end(self, value: &Value, exit: u8) {
        let text = serde_json::to_string(value).unwrap_or_default();
        let mut line = json!({
            "id": self.id,
            "t": crate::now_ms() as u64,
            "event": "end",
            "exit": exit,
            "ms": self.started.elapsed().as_millis() as u64,
            "bytes": text.len(),
            "summary": summary(value),
        });
        if let Some(code) = value["error"]["code"].as_str() {
            line["error"] = json!(code);
        }
        if text.len() <= RESULT_LIMIT {
            let dir = self.root.join(RESULTS);
            if std::fs::create_dir_all(&dir)
                .and_then(|_| std::fs::write(dir.join(format!("{}.json", self.id)), &text))
                .is_ok()
            {
                line["kept"] = json!(true);
            }
            // Now and then, not on every call.
            if crate::now_ms().is_multiple_of(32) {
                prune(&dir);
            }
        }
        append(&self.root.join(LOG), &line);
    }
}

fn append(path: &Path, line: &Value) {
    let mut text = line.to_string();
    text.push('\n');
    // One write, so concurrent writers' lines do not interleave.
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
        let _ = f.write_all(text.as_bytes());
    }
}

/// Remove the oldest results past [`KEEP_RESULTS`] (ids start with the time).
fn prune(dir: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    let mut names: Vec<_> = entries.filter_map(|e| e.ok()).map(|e| e.file_name()).collect();
    if names.len() <= KEEP_RESULTS {
        return;
    }
    names.sort();
    for n in &names[..names.len() - KEEP_RESULTS] {
        let _ = std::fs::remove_file(dir.join(n));
    }
}

/// A result in one line: the error, or the result's status-like fields and
/// its other keys.
pub fn summary(v: &Value) -> String {
    let mut s = if let Some(e) = v.get("error") {
        format!("{}: {}", e["code"].as_str().unwrap_or("error"), e["message"].as_str().unwrap_or(""))
    } else if let Value::Object(o) = v {
        let mut parts = Vec::new();
        for k in ["status", "verdict", "ok", "recorded", "written", "item", "summary", "message"] {
            match o.get(k) {
                Some(Value::String(t)) => parts.push(format!("{k}: {t}")),
                Some(b @ (Value::Bool(_) | Value::Number(_))) => parts.push(format!("{k}: {b}")),
                Some(Value::Object(i)) if k == "item" => {
                    if let Some(t) = i.get("kind").or(i.get("title")).and_then(Value::as_str) {
                        parts.push(format!("item: {t}"));
                    }
                }
                _ => {}
            }
        }
        // A status: what it hands out next.
        if let Some(item) = o.get("next").and_then(|n| n["item"].as_str()) {
            let r = o["next"]["recording"].as_str().map(|r| format!(" {r}")).unwrap_or_default();
            parts.push(format!("next: {item}{r}"));
        }
        let mut others: Vec<&str> =
            o.keys().map(String::as_str).filter(|k| !parts.iter().any(|p| p.starts_with(*k))).collect();
        if others.len() > 6 {
            others.truncate(6);
            others.push("…");
        }
        if !others.is_empty() {
            parts.push(format!("[{}]", others.join(", ")));
        }
        parts.join("; ")
    } else {
        v.to_string()
    };
    if s.len() > 240 {
        let mut end = 240;
        while !s.is_char_boundary(end) {
            end -= 1;
        }
        s.truncate(end);
        s.push('…');
    }
    s
}

/// The projects `psx web` shows: the ones it was given, remembered in the
/// per-user data directory.
pub fn projects_file() -> Option<PathBuf> {
    crate::bios::data_dir().map(|d| d.join("web-projects.txt"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_report_is_not_copied_into_the_project_log() {
        let root = std::env::temp_dir().join(format!("psx-calls-report-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join(".psx")).unwrap();
        start(&root, "report", json!({"text": "secret verdict"}), json!({})).unwrap();
        start(&root, "report", json!(["report", "--text", "secret verdict"]), json!({})).unwrap();
        let log = std::fs::read_to_string(root.join(LOG)).unwrap();
        assert!(!log.contains("secret verdict"), "{log}");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn calls_are_logged_only_in_a_project() {
        let root = std::env::temp_dir().join(format!("psx-calls-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        assert!(start(&root, "context", json!({}), caller("cli", None)).is_none());
        std::fs::create_dir_all(root.join(".psx")).unwrap();
        let call = start(&root, "port_status", json!({"x": 1}), caller("mcp", Some("codex".into()))).unwrap();
        let id = call.id.clone();
        call.end(&json!({"status": "work", "item": {"kind": "record"}, "facts": {}}), 0);
        let lines: Vec<Value> = std::fs::read_to_string(root.join(LOG))
            .unwrap()
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        assert_eq!((lines[0]["event"].as_str(), lines[0]["tool"].as_str()), (Some("start"), Some("port_status")));
        assert_eq!(lines[0]["caller"]["client"], "codex");
        assert_eq!(lines[1]["summary"], "status: work; item: record; [facts]");
        assert!(root.join(RESULTS).join(format!("{id}.json")).is_file());
        assert_eq!(summary(&json!({"error": {"code": "timeout", "message": "too slow"}})), "timeout: too slow");
        assert_eq!(
            summary(&json!({"git": {}, "next": {"item": "Fix", "recording": "walk/a"}})),
            "next: Fix walk/a; [git]"
        );
        let _ = std::fs::remove_dir_all(root);
    }
}
