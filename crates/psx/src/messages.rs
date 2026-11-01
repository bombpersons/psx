//! Messages between the person (in `psx web`) and the agent working in a
//! project: `.psx/messages.jsonl`, one message per line, each either the
//! start of a thread or a reply in one. Nothing interrupts the agent: a
//! message the person wrote reaches it in the result of its next psx call
//! (any tool's, as `messages`), and it answers with the `message` tool when
//! it has something to say. `.psx/messages.seen.json` holds how many
//! messages each side has seen, for the page's unread count and its "read"
//! marks.

use std::path::Path;

use serde_json::{Value, json};

use crate::Error;
use crate::commands::{Ctx, Op, Outcome};

pub const LOG: &str = ".psx/messages.jsonl";
pub const SEEN: &str = ".psx/messages.seen.json";

/// Longest message kept.
const TEXT_LIMIT: usize = 8 * 1024;
/// Threads `message` lists without text.
const THREADS_SHOWN: usize = 12;
/// Longest `--wait`.
const WAIT_LIMIT: u64 = 3600;

#[derive(clap::Args, Clone, Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct MessageArgs {
    /// To send to the person; without it, the latest threads (with their replies).
    #[arg(long)]
    pub text: Option<String>,
    /// A message's id: send as a reply in its thread (default: a new thread).
    #[arg(long)]
    pub reply_to: Option<String>,
    /// Block until the person writes (after sending text, if given), at most
    /// SECONDS (default 600, at most 3600), then return what they wrote. Run
    /// it in the background from a shell: it ends when they answer. Not over
    /// MCP, where it would hold the call.
    #[arg(long, value_name = "SECONDS", num_args = 0..=1, default_missing_value = "600")]
    #[serde(skip)]
    #[schemars(skip)]
    pub wait: Option<u64>,
}

/// Every message, oldest first.
pub fn all(root: &Path) -> Vec<Value> {
    std::fs::read_to_string(root.join(LOG))
        .map(|s| s.lines().filter_map(|l| serde_json::from_str(l).ok()).collect())
        .unwrap_or_default()
}

/// How many messages `side` ("agent" or "person") has seen.
pub fn seen(root: &Path, side: &str) -> usize {
    std::fs::read(root.join(SEEN))
        .ok()
        .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
        .and_then(|v| v[side].as_u64())
        .unwrap_or(0) as usize
}

/// `side` has seen the first `n` messages (never fewer than before).
pub fn mark_seen(root: &Path, side: &str, n: usize) {
    let p = root.join(SEEN);
    let mut v = std::fs::read(&p).ok().and_then(|b| serde_json::from_slice::<Value>(&b).ok()).unwrap_or(json!({}));
    if v[side].as_u64().unwrap_or(0) as usize >= n {
        return;
    }
    v[side] = json!(n);
    let _ = crate::write_atomic(&p, v.to_string().as_bytes());
}

/// Add a message from `from`; `reply_to` puts it in that message's thread.
pub fn post(root: &Path, from: &str, text: &str, reply_to: Option<&str>) -> crate::Result<Value> {
    let text = text.trim();
    if text.is_empty() {
        return Err(Error::invalid("an empty message"));
    }
    if text.len() > TEXT_LIMIT {
        return Err(Error::invalid(format!("the message is {} bytes; at most {TEXT_LIMIT}", text.len())));
    }
    if !root.join(".psx").is_dir() {
        return Err(Error::invalid(format!("{}: not a project (no .psx)", root.display())));
    }
    let msgs = all(root);
    let thread = match reply_to {
        None => None,
        Some(r) => {
            let m = msgs.iter().find(|m| m["id"] == r).ok_or_else(|| Error::invalid(format!("no message {r}")))?;
            Some(m["thread"].as_str().or(m["id"].as_str()).unwrap_or(r).to_string())
        }
    };
    let id = format!("m{}-{}", crate::now_ms(), std::process::id());
    let mut m = json!({"id": id, "t": crate::now_ms() as u64, "from": from, "text": text});
    if let Some(t) = &thread {
        m["thread"] = json!(t);
    }
    let mut line = m.to_string();
    line.push('\n');
    use std::io::Write;
    let p = root.join(LOG);
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&p)
        .and_then(|mut f| f.write_all(line.as_bytes()))
        .map_err(|e| Error::io(&p, e))?;
    // A side has seen what it wrote, and everything before it.
    mark_seen(root, from, msgs.len() + 1);
    Ok(m)
}

/// A message as the agent reads it: a reply carries its thread's first message.
fn for_agent(m: &Value, msgs: &[Value]) -> Value {
    let mut v = json!({"id": m["id"], "text": m["text"], "at": ago(m["t"].as_u64().unwrap_or(0))});
    if let Some(t) = m["thread"].as_str() {
        if let Some(start) = msgs.iter().find(|s| s["id"] == t) {
            let text: String = start["text"].as_str().unwrap_or("").chars().take(200).collect();
            v["in_reply_to"] = json!({"id": t, "from": start["from"], "text": text});
        }
    }
    v
}

fn ago(t: u64) -> String {
    let s = (crate::now_ms() as u64).saturating_sub(t) / 1000;
    match s {
        0..60 => format!("{s} s ago"),
        60..3600 => format!("{} min ago", s / 60),
        3600..86400 => format!("{} h ago", s / 3600),
        _ => format!("{} days ago", s / 86400),
    }
}

/// The person's messages the agent has not seen, for the result of any call,
/// marked seen: nothing when there are none (one stat when there is no log).
pub fn deliver(root: &Path) -> Option<Value> {
    if !root.join(LOG).is_file() {
        return None;
    }
    let msgs = all(root);
    let from = seen(root, "agent");
    let new: Vec<Value> =
        msgs.iter().skip(from).filter(|m| m["from"] == "person").map(|m| for_agent(m, &msgs)).collect();
    mark_seen(root, "agent", msgs.len());
    (!new.is_empty()).then(|| {
        json!({"from_person": new,
            "note": "From the person, who reads what you send in psx web. Not an interruption: finish what you are doing unless it changes that work. Answer with message (text, reply_to: its id; psx message --text ... --reply-to ID) when you have something to say."})
    })
}

impl Op for MessageArgs {
    fn exec(self, ctx: &Ctx) -> Outcome {
        let root = &ctx.root;
        if self.reply_to.is_some() && self.text.is_none() {
            return Outcome::from(Error::invalid("reply_to needs text"));
        }
        let sent = match &self.text {
            None => None,
            Some(text) => match post(root, "agent", text, self.reply_to.as_deref()) {
                Ok(m) => Some(json!({"id": m["id"], "thread": m["thread"].as_str().or(m["id"].as_str())})),
                Err(e) => return Outcome::from(e),
            },
        };
        if let Some(secs) = self.wait {
            return wait(root, secs.min(WAIT_LIMIT), sent);
        }
        if let Some(mut v) = sent {
            v["status"] = json!("sent");
            v["note"] = json!("The person sees it in psx web; their answer arrives with a later psx call's result.");
            return Outcome::ok(v);
        }
        let msgs = all(root);
        mark_seen(root, "agent", msgs.len());
        let starts: Vec<&Value> = msgs.iter().filter(|m| m["thread"].is_null()).collect();
        let threads: Vec<Value> = starts
            .iter()
            .rev()
            .take(THREADS_SHOWN)
            .rev()
            .map(|s| {
                let replies: Vec<Value> = msgs
                    .iter()
                    .filter(|m| m["thread"] == s["id"])
                    .map(|m| json!({"id": m["id"], "from": m["from"], "text": m["text"], "at": ago(m["t"].as_u64().unwrap_or(0))}))
                    .collect();
                json!({"id": s["id"], "from": s["from"], "text": s["text"], "at": ago(s["t"].as_u64().unwrap_or(0)), "replies": replies})
            })
            .collect();
        Outcome::ok(json!({"threads": threads, "count": starts.len(),
            "note": if starts.is_empty() { "No messages yet." } else { "Oldest first; reply with text and reply_to." }}))
    }
}

/// The person's next messages, once they write or `secs` pass.
fn wait(root: &Path, secs: u64, sent: Option<Value>) -> Outcome {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(secs);
    loop {
        if let Some(m) = deliver(root) {
            let mut v = json!({"status": "message", "messages": m});
            if let Some(s) = sent {
                v["sent"] = s;
            }
            return Outcome::ok(v);
        }
        if std::time::Instant::now() >= deadline {
            let mut v = json!({"status": "timeout", "waited_s": secs,
                "note": "Nothing from the person yet. Wait again when there is nothing else to do."});
            if let Some(s) = sent {
                v["sent"] = s;
            }
            return Outcome::ok(v);
        }
        std::thread::sleep(std::time::Duration::from_millis(500));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_message_reaches_the_agent_once_and_threads_its_replies() {
        let dir = std::env::temp_dir().join(format!("psx-messages-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join(".psx")).unwrap();
        assert!(deliver(&dir).is_none());
        let q = post(&dir, "person", "How is the jump going?", None).unwrap();
        let got = deliver(&dir).unwrap();
        assert_eq!(got["from_person"][0]["text"], "How is the jump going?");
        assert!(deliver(&dir).is_none(), "delivered once");
        let a = post(&dir, "agent", "Matching to tick 40", q["id"].as_str()).unwrap();
        assert_eq!(a["thread"], q["id"]);
        // A reply to a reply stays in the thread.
        let b = post(&dir, "person", "Nice", a["id"].as_str()).unwrap();
        assert_eq!(b["thread"], q["id"]);
        let got = deliver(&dir).unwrap();
        assert_eq!(got["from_person"][0]["in_reply_to"]["text"], "How is the jump going?");
        assert_eq!(seen(&dir, "person"), 3);
        assert!(post(&dir, "agent", "x", Some("nope")).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn wait_returns_when_the_person_writes() {
        let dir = std::env::temp_dir().join(format!("psx-messages-wait-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join(".psx")).unwrap();
        let out = wait(&dir, 0, None);
        assert_eq!(out.value["status"], "timeout");
        let d = dir.clone();
        let writer = std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(300));
            post(&d, "person", "Try the ledge again", None).unwrap();
        });
        let out = wait(&dir, 10, Some(json!({"id": "m1"})));
        writer.join().unwrap();
        assert_eq!(out.value["status"], "message");
        assert_eq!(out.value["messages"]["from_person"][0]["text"], "Try the ledge again");
        assert_eq!(out.value["sent"]["id"], "m1");
        assert!(deliver(&dir).is_none(), "delivered once");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
