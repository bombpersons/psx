//! The numbers behind `psx web`'s Stats tab, from what a project already
//! keeps: the port's standing over time (`.psx/progress.jsonl`), the call
//! log (`.psx/calls.jsonl` and the rotated `calls.1.jsonl`), the git
//! history (commits, the code's lines) and the disc inventory's
//! history. Nothing here writes.

use std::collections::{BTreeMap, HashMap};
use std::path::Path;

use serde_json::{Value, json};

/// One hour, in ms: the call log's buckets.
const HOUR: u64 = 3_600_000;
/// The port's own code, for its lines (as the review counts them).
const CODE: [&str; 2] = ["game/", "tools/"];

fn jsonl(path: &Path) -> Vec<Value> {
    std::fs::read_to_string(path)
        .map(|s| s.lines().filter_map(|l| serde_json::from_str(l).ok()).collect())
        .unwrap_or_default()
}

pub fn of(root: &Path) -> Value {
    json!({
        "now": crate::now_ms() as u64,
        "progress": jsonl(&root.join(crate::port::status::PROGRESS)),
        "inventory": jsonl(&root.join(".psx/inventory_history.jsonl")),
        "calls": calls(root),
        "git": git(root),
        "decomp": crate::decomp::counts(root),
        "messages": crate::messages::all(root).iter()
            .map(|m| json!({"t": m["t"], "from": m["from"]})).collect::<Vec<_>>(),
    })
}

#[derive(Default)]
struct Tally {
    calls: u64,
    failed: u64,
    ms: u64,
}

impl Tally {
    fn add(&mut self, failed: bool, ms: u64) {
        self.calls += 1;
        self.failed += u64::from(failed);
        self.ms += ms;
    }
    fn value(&self) -> Value {
        json!({"calls": self.calls, "failed": self.failed, "ms": self.ms})
    }
}

/// The call log by hour, by tool and by the agent (session) that made them.
fn calls(root: &Path) -> Value {
    let mut starts: HashMap<String, (String, String, u64)> = HashMap::new();
    let mut hours: BTreeMap<u64, Tally> = BTreeMap::new();
    let mut tools: BTreeMap<String, Tally> = BTreeMap::new();
    let mut sessions: BTreeMap<String, (Tally, u64, u64)> = BTreeMap::new();
    for file in [".psx/calls.1.jsonl", crate::calls::LOG] {
        for l in jsonl(&root.join(file)) {
            let id = l["id"].as_str().unwrap_or("").to_string();
            let t = l["t"].as_u64().unwrap_or(0);
            if l["event"] == "start" {
                let c = &l["caller"];
                let who = match c["session"]["name"].as_str() {
                    Some(n) => format!("{n} {}", c["session"]["pid"]),
                    None => format!("{} {}", c["via"].as_str().unwrap_or("?"), c["pid"]),
                };
                starts.insert(id, (l["tool"].as_str().unwrap_or("?").to_string(), who, t));
            } else if let Some((tool, who, t0)) = starts.remove(&id) {
                let failed = l["exit"].as_u64().unwrap_or(0) != 0;
                let ms = l["ms"].as_u64().unwrap_or(t.saturating_sub(t0));
                hours.entry(t0 / HOUR * HOUR).or_default().add(failed, ms);
                tools.entry(tool).or_default().add(failed, ms);
                let s = sessions.entry(who).or_insert((Tally::default(), t0, t));
                s.0.add(failed, ms);
                s.1 = s.1.min(t0);
                s.2 = s.2.max(t);
            }
        }
    }
    json!({
        "hours": hours.iter().map(|(t, n)| { let mut v = n.value(); v["t"] = json!(t); v }).collect::<Vec<_>>(),
        "tools": tools.iter().map(|(k, n)| { let mut v = n.value(); v["tool"] = json!(k); v }).collect::<Vec<_>>(),
        "sessions": sessions.iter().map(|(k, (n, a, b))| {
            let mut v = n.value();
            v["session"] = json!(k);
            v["first"] = json!(a);
            v["last"] = json!(b);
            v
        }).collect::<Vec<_>>(),
        "running": starts.len(),
    })
}

/// Every commit, oldest first: when, its subject, and the lines it added and removed in the code and elsewhere.
fn git(root: &Path) -> Value {
    let out = std::process::Command::new("git")
        .args(["log", "--reverse", "--no-merges", "--format=%x1e%h%x1f%ct%x1f%s", "--numstat"])
        .current_dir(root)
        .output()
        .ok()
        .filter(|o| o.status.success());
    let Some(out) = out else { return json!([]) };
    let text = String::from_utf8_lossy(&out.stdout);
    let mut commits = Vec::new();
    for rec in text.split('\x1e').filter(|r| !r.trim().is_empty()) {
        let mut lines = rec.lines();
        let mut head = lines.next().unwrap_or("").split('\x1f');
        let (h, t, subject) = (head.next().unwrap_or(""), head.next().unwrap_or("0"), head.next().unwrap_or(""));
        let (mut code_add, mut code_del, mut other_add, mut other_del) = (0u64, 0u64, 0u64, 0u64);
        for l in lines {
            let mut f = l.split('\t');
            let (Some(a), Some(d), Some(path)) = (f.next(), f.next(), f.next()) else { continue };
            // Binary files show "-".
            let (a, d) = (a.parse().unwrap_or(0), d.parse().unwrap_or(0));
            if CODE.iter().any(|c| path.starts_with(c)) {
                code_add += a;
                code_del += d;
            } else {
                other_add += a;
                other_del += d;
            }
        }
        let subject: String = subject.chars().take(160).collect();
        commits.push(json!({"h": h, "t": t.parse::<u64>().unwrap_or(0) * 1000, "subject": subject,
            "code": [code_add, code_del], "other": [other_add, other_del]}));
    }
    json!(commits)
}
