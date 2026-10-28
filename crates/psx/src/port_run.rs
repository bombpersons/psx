//! `psx port run`: start each role's agent when the ledger gives it work.
//!
//! The person runs it in the investigator's repository. It computes the
//! ledger, picks the next role (in turn: investigator, reviewer,
//! implementer) whose first item of work does not wait on another, and runs
//! that role's harness once, headless, in its repository, with a fixed
//! prompt naming the item. Each run starts fresh: what a role knows is in
//! its files. When the agent exits the ledger is computed again, and so on,
//! until no role has work, a run changes nothing (the role's work and its
//! repository are as before), a run fails (its harness exited with an
//! error, ran out of time, or its stream has no successful end), or
//! `max_runs` is reached. A role's next session is told what its failed
//! one left uncommitted.
//!
//! The harness is a command line per role in protocol.toml's `[agents]`,
//! so any agent harness with a headless mode serves. Each run's output goes
//! to .psx/port-run/ (the raw stream and a readable summary) and its record
//! to runs.jsonl; `--follow` shows the current run from another terminal.

use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use crate::Error;
use crate::commands::{Ctx, Op, Outcome};
use crate::roles::Role;

#[derive(clap::Args, Clone, Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct PortRunArgs {
    /// Runs at most (default: protocol.toml's [agents] max_runs, else 10).
    #[arg(long)]
    #[serde(default)]
    pub max_runs: Option<usize>,
    /// Say which role would run and the command, and run nothing.
    #[arg(long)]
    #[serde(default)]
    pub dry_run: bool,
    /// Show the current run's output as it comes (from another terminal), until port run ends.
    #[arg(long)]
    #[serde(default)]
    pub follow: bool,
    /// A shell in this role's sandbox, as its sessions have it: to log its harness in, or look.
    #[arg(long, value_enum)]
    #[serde(default)]
    pub shell: Option<Role>,
}

/// The order roles take turns in.
const ORDER: [Role; 3] = [Role::Investigator, Role::Reviewer, Role::Implementer];

/// One role's harness, from protocol.toml's `[agents.ROLE]`.
#[derive(Clone, Debug, Default)]
struct Agent {
    command: Vec<String>,
    model: Option<String>,
    prompt: Option<String>,
    /// From the person's home into the sandbox, writable and read-only.
    share: Vec<PathBuf>,
    share_ro: Vec<PathBuf>,
}

#[derive(Clone, Debug)]
struct Agents {
    max_runs: Option<usize>,
    timeout: Option<Duration>,
    /// Each session in bubblewrap (default).
    sandbox: bool,
    roles: Vec<(Role, Agent)>,
}

impl Default for Agents {
    fn default() -> Self {
        Agents { max_runs: None, timeout: None, sandbox: true, roles: vec![] }
    }
}

fn agents(root: &Path) -> crate::Result<Agents> {
    let p = root.join("protocol.toml");
    let text = std::fs::read_to_string(&p).map_err(|e| Error::io(&p, e))?;
    let v: toml::Value = toml::from_str(&text).map_err(|e| Error::invalid(format!("protocol.toml: {e}")))?;
    let mut out = Agents::default();
    let Some(a) = v.get("agents") else { return Ok(out) };
    let Some(a) = a.as_table() else { return Err(Error::invalid("protocol.toml: [agents] is not a table")) };
    for (k, x) in a {
        match k.as_str() {
            "max_runs" => out.max_runs = x.as_integer().map(|n| n.max(1) as usize),
            "timeout_minutes" => out.timeout = x.as_integer().map(|n| Duration::from_secs(n.max(1) as u64 * 60)),
            "sandbox" => {
                out.sandbox =
                    x.as_bool().ok_or_else(|| Error::invalid("protocol.toml: [agents] sandbox is not true or false"))?
            }
            "investigator" | "reviewer" | "implementer" => {
                let role = ORDER.into_iter().find(|r| r.name() == k).unwrap_or(Role::Investigator);
                let bad = |what: &str| Error::invalid(format!("protocol.toml: [agents.{k}] {what}"));
                let t = x.as_table().ok_or_else(|| bad("is not a table"))?;
                let mut agent = Agent::default();
                for (f, y) in t {
                    match f.as_str() {
                        "command" => {
                            agent.command = y
                                .as_array()
                                .and_then(|a| a.iter().map(|s| s.as_str().map(str::to_string)).collect())
                                .ok_or_else(|| bad("command is not a list of strings"))?
                        }
                        "model" => agent.model = Some(y.as_str().ok_or_else(|| bad("model is not a string"))?.into()),
                        "prompt" => {
                            agent.prompt = Some(y.as_str().ok_or_else(|| bad("prompt is not a string"))?.into())
                        }
                        "share" | "share_ro" => {
                            let list: Vec<PathBuf> = y
                                .as_array()
                                .and_then(|a| a.iter().map(|s| s.as_str().map(expand)).collect())
                                .ok_or_else(|| bad(&format!("{f} is not a list of paths")))?;
                            if f == "share" { agent.share = list } else { agent.share_ro = list }
                        }
                        other => {
                            return Err(bad(&format!("has {other}: write command, model, prompt, share or share_ro")));
                        }
                    }
                }
                if agent.command.is_empty() {
                    return Err(bad("has no command"));
                }
                out.roles.push((role, agent));
            }
            other => {
                return Err(Error::invalid(format!(
                    "protocol.toml: [agents] has {other}: write max_runs, timeout_minutes, sandbox, or [agents.ROLE]"
                )));
            }
        }
    }
    Ok(out)
}

/// What a role's session is told: its role, the item, and to leave what it
/// learned in its files before it stops, since the next session starts
/// fresh.
fn default_prompt(role: Role) -> &'static str {
    match role {
        Role::Investigator => {
            "You are the investigator in this clean-room port; AGENTS.md is your role. psx port run started this session and starts a fresh one for the next item, so what you leave in your files is all the next session knows. Start with protocol next (role investigator) and do its first item of work, as AGENTS.md says: {work}. Do that one item. Before you stop, add to notes/LOOP.md, write what you learned into your notes, and commit. Then stop."
        }
        Role::Reviewer => {
            "You are the reviewer in this clean-room port; AGENTS.md is your role. psx port run started this session and starts a fresh one for the next item. Start with protocol next (role reviewer) and do its first item of work, as AGENTS.md says: {work}. Do that one item, commit your verdict or audit, and stop."
        }
        Role::Implementer => {
            "You are the implementer in this clean-room port; AGENTS.md is your role. psx port run started this session and starts a fresh one for the next item, so what you leave in your files is all the next session knows. Start with clean mode's next and notes/NEXT.md, and do the first item of work, as AGENTS.md says: {work}. Do that one item. Before you stop, add to questions/LOOP.md and commit. Then stop."
        }
    }
}

/// The item as a prompt names it: what to do, and the guide topics it
/// needs (read those, not every topic).
fn item_text(item: &Value) -> String {
    let what = item["what"].as_str().unwrap_or_default();
    let topics: Vec<&str> = item["guide"].as_array().into_iter().flatten().filter_map(Value::as_str).collect();
    if topics.is_empty() {
        what.to_string()
    } else {
        format!("{what} (guide topics for it: {}; read only those unless the work turns to another)", topics.join(", "))
    }
}

/// A path protocol.toml names: ~/ is the home folder.
fn expand(p: &str) -> PathBuf {
    match (p.strip_prefix("~/"), std::env::var_os("HOME")) {
        (Some(rest), Some(h)) => PathBuf::from(h).join(rest),
        _ => PathBuf::from(p),
    }
}

/// The implementer's home in its sandbox: its harness's settings and
/// transcripts, kept apart from the person's (one per port).
#[cfg(unix)]
fn implementer_home(implementer: &Path) -> Option<PathBuf> {
    let data = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")))?;
    Some(data.join("psx/agents").join(crate::roles::host_key(implementer)))
}

/// The servers a sandboxed session reaches outside it, kept until it ends.
#[cfg(unix)]
type Held = crate::sandbox::Servers;
#[cfg(not(unix))]
type Held = ();

/// The session's command line: in its sandbox, with psx's servers outside
/// it started (none for a dry run), or as it is.
#[cfg(unix)]
#[allow(clippy::too_many_arguments)]
fn sandboxed(
    sandbox: bool,
    role: Role,
    agent: &Agent,
    repo: &Path,
    others: &[PathBuf],
    cmd: Vec<String>,
    log: Option<&Path>,
    interactive: bool,
) -> crate::Result<(Vec<std::ffi::OsString>, Option<Held>)> {
    if !sandbox {
        return Ok((cmd.into_iter().map(Into::into).collect(), None));
    }
    let servers = match log {
        Some(l) => crate::sandbox::Servers::start(repo, l)?,
        None => crate::sandbox::Servers::none(),
    };
    let implementer = if role == Role::Implementer { Some(repo) } else { None };
    let home = implementer.and_then(implementer_home);
    let policy = crate::sandbox::Policy {
        role,
        repo,
        others,
        share: &agent.share,
        share_ro: &agent.share_ro,
        home: home.as_deref(),
        harness: agent.command.first().map(String::as_str),
        interactive,
    };
    Ok((crate::sandbox::wrap(&policy, &cmd, &servers)?, Some(servers)))
}

#[cfg(not(unix))]
#[allow(clippy::too_many_arguments)]
fn sandboxed(
    sandbox: bool,
    _: Role,
    _: &Agent,
    _: &Path,
    _: &[PathBuf],
    cmd: Vec<String>,
    _: Option<&Path>,
    _: bool,
) -> crate::Result<(Vec<std::ffi::OsString>, Option<Held>)> {
    if sandbox {
        return Err(Error::invalid(
            "port run's sandbox is bubblewrap's, on Linux: write sandbox = false under [agents] to run the sessions with only the harness's own access rules",
        ));
    }
    Ok((cmd.into_iter().map(Into::into).collect(), None))
}

/// The command line with its placeholders filled.
fn command_line(role: Role, agent: &Agent, repo: &Path, prompt: &str) -> crate::Result<Vec<String>> {
    agent
        .command
        .iter()
        .map(|a| {
            if a.contains("{model}") && agent.model.is_none() {
                return Err(Error::invalid(format!(
                    "protocol.toml: [agents.{}] command names {{model}} and there is no model",
                    role.name()
                )));
            }
            Ok(a.replace("{model}", agent.model.as_deref().unwrap_or_default())
                .replace("{role}", role.name())
                .replace("{repo}", &repo.display().to_string())
                .replace("{prompt}", prompt))
        })
        .collect()
}

fn git(repo: &Path, args: &[&str]) -> String {
    Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .stderr(Stdio::null())
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim_end().to_string())
        .unwrap_or_default()
}

/// What a run must change to count as progress: the role's work and its
/// repository (commit and working tree), leaving out psx's own .psx/,
/// where port run keeps its logs.
fn state(repo: &Path, work: &[Value]) -> (String, String, String) {
    (
        serde_json::to_string(work).unwrap_or_default(),
        git(repo, &["rev-parse", "HEAD"]),
        git(repo, &["status", "--porcelain", "--", ".", ":!.psx"]),
    )
}

fn dir(ctx: &Ctx) -> PathBuf {
    ctx.root.join(".psx/port-run")
}

fn alive(pid: u32) -> bool {
    if cfg!(target_os = "linux") { Path::new(&format!("/proc/{pid}")).exists() } else { true }
}

/// The run's lock: one port run at a time, removed when it ends. A lock
/// whose process is gone is taken over.
struct Lock(PathBuf);

impl Lock {
    fn take(dir: &Path) -> crate::Result<Lock> {
        std::fs::create_dir_all(dir).map_err(|e| Error::io(dir, e))?;
        let p = dir.join("lock");
        if let Some(pid) =
            std::fs::read_to_string(&p).ok().and_then(|t| t.trim().parse::<u32>().ok()).filter(|&pid| alive(pid))
        {
            return Err(Error::invalid(format!(
                "port run is already running (process {pid}): port run --follow shows it"
            )));
        }
        std::fs::write(&p, std::process::id().to_string()).map_err(|e| Error::io(&p, e))?;
        Ok(Lock(p))
    }
}

impl Drop for Lock {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

fn clip(s: &str, n: usize) -> String {
    let line = s.lines().find(|l| !l.trim().is_empty()).unwrap_or_default().trim();
    let more = s.trim().lines().count() > 1;
    match line.char_indices().nth(n) {
        Some((i, _)) => format!("{}...", &line[..i]),
        None if more => format!("{line} ..."),
        None => line.to_string(),
    }
}

/// A tool call's input in a few words: the command, the file, else the
/// input itself.
fn tool_input(v: &Value) -> String {
    for k in ["command", "file_path", "path", "pattern", "url", "query"] {
        if let Some(s) = v[k].as_str() {
            return clip(s, 160);
        }
    }
    match v {
        Value::Null => String::new(),
        v => clip(&v.to_string(), 160),
    }
}

/// One line of a harness's output as the person reads it: the agent's text,
/// its tool calls and the end, from the event streams harnesses write
/// (Claude Code's stream-json, Codex's exec --json, Gemini's stream-json);
/// plain text as it is. None for events with nothing to show.
pub(crate) fn render(line: &str) -> Option<String> {
    let Ok(v) = serde_json::from_str::<Value>(line) else {
        return Some(line.to_string());
    };
    let t = v["type"].as_str().unwrap_or_default();
    let item = &v["item"];
    let out = match t {
        // Claude Code
        "system" if v["subtype"] == "init" => format!(
            "[session {} {}]",
            v["session_id"].as_str().unwrap_or_default(),
            v["model"].as_str().unwrap_or_default()
        ),
        "assistant" => {
            let parts: Vec<String> = v["message"]["content"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|c| match c["type"].as_str()? {
                    "text" => Some(clip(c["text"].as_str()?, 240)),
                    "tool_use" => Some(format!("> {} {}", c["name"].as_str()?, tool_input(&c["input"]))),
                    _ => None,
                })
                .filter(|s| !s.is_empty())
                .collect();
            if parts.is_empty() {
                return None;
            }
            parts.join("\n")
        }
        "user" => {
            let errors: Vec<String> = v["message"]["content"]
                .as_array()
                .into_iter()
                .flatten()
                .filter(|c| c["type"] == "tool_result" && c["is_error"] == true)
                .map(|c| {
                    format!(
                        "  ! {}",
                        clip(
                            &c["content"].as_str().map(str::to_string).unwrap_or_else(|| c["content"].to_string()),
                            200
                        )
                    )
                })
                .collect();
            if errors.is_empty() {
                return None;
            }
            errors.join("\n")
        }
        "result" if v.get("num_turns").is_some() => format!(
            "[end: {}, {} turns, {:.0} s{}]",
            v["subtype"].as_str().unwrap_or_default(),
            v["num_turns"],
            v["duration_ms"].as_f64().unwrap_or(0.0) / 1000.0,
            v["total_cost_usd"].as_f64().map(|c| format!(", ${c:.2}")).unwrap_or_default()
        ),
        // Codex
        "thread.started" => format!("[session {}]", v["thread_id"].as_str().unwrap_or_default()),
        "item.completed" => match item["type"].as_str().unwrap_or_default() {
            "agent_message" => clip(item["text"].as_str().unwrap_or_default(), 240),
            "command_execution" => format!(
                "> $ {}{}",
                clip(item["command"].as_str().unwrap_or_default(), 160),
                item["exit_code"].as_i64().filter(|c| *c != 0).map(|c| format!("  (exit {c})")).unwrap_or_default()
            ),
            "file_change" => format!(
                "> files {}",
                item["changes"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|c| c["path"].as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            "mcp_tool_call" => format!(
                "> {}.{}",
                item["server"].as_str().unwrap_or_default(),
                item["tool"].as_str().unwrap_or_default()
            ),
            "web_search" => format!("> search {}", item["query"].as_str().unwrap_or_default()),
            "error" => format!("  ! {}", item["message"].as_str().unwrap_or_default()),
            _ => return None,
        },
        "turn.completed" => {
            format!("[turn: {} tokens in, {} out]", v["usage"]["input_tokens"], v["usage"]["output_tokens"])
        }
        "turn.failed" | "error" => format!(
            "  ! {}",
            clip(
                &v["error"]["message"]
                    .as_str()
                    .or(v["message"].as_str())
                    .map(str::to_string)
                    .unwrap_or_else(|| v.to_string()),
                240
            )
        ),
        // Gemini
        "init" => format!(
            "[session {} {}]",
            v["session_id"].as_str().unwrap_or_default(),
            v["model"].as_str().unwrap_or_default()
        ),
        "message" if v["role"] == "assistant" => clip(v["content"].as_str().unwrap_or_default(), 240),
        "tool_use" => format!("> {} {}", v["tool_name"].as_str().unwrap_or_default(), tool_input(&v["parameters"])),
        "tool_result" if v["status"] == "error" => format!("  ! {}", clip(&v["error"].to_string(), 200)),
        "result" => format!("[end: {}]", v["status"].as_str().unwrap_or_default()),
        "item.started" | "item.updated" | "turn.started" | "message" | "tool_result" | "stream_event" => return None,
        _ => clip(line, 200),
    };
    (!out.trim().is_empty()).then_some(out)
}

/// Writes what the person reads to the terminal and the run's summary file.
struct Shown(Mutex<std::fs::File>);

impl Shown {
    fn line(&self, s: &str) {
        eprintln!("{s}");
        if let Ok(mut f) = self.0.lock() {
            let _ = writeln!(f, "{s}");
        }
    }
}

/// Runs one session: the command in the role's repository, its output to
/// the raw log and, readably, to the terminal. Returns its exit code (None
/// when killed or not started) and whether it ran out of time.
fn session(
    cmd: &[std::ffi::OsString],
    repo: &Path,
    raw: &Path,
    shown: &Arc<Shown>,
    timeout: Option<Duration>,
) -> crate::Result<(Option<i32>, bool)> {
    let mut child = match Command::new(&cmd[0])
        .args(&cmd[1..])
        .current_dir(repo)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
    {
        Ok(c) => c,
        Err(e) => {
            shown.line(&format!("  ! could not start {}: {e}", cmd[0].to_string_lossy()));
            return Ok((None, false));
        }
    };
    let raw_file = Arc::new(Mutex::new(std::fs::File::create(raw).map_err(|e| Error::io(raw, e))?));
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    let child = Arc::new(Mutex::new(child));
    let done = Arc::new(AtomicBool::new(false));
    let timed_out = Arc::new(AtomicBool::new(false));
    let timer = timeout.map(|t| {
        let (child, done, timed_out) = (child.clone(), done.clone(), timed_out.clone());
        std::thread::spawn(move || {
            let start = Instant::now();
            while !done.load(Ordering::Relaxed) {
                if start.elapsed() >= t {
                    timed_out.store(true, Ordering::Relaxed);
                    if let Ok(mut c) = child.lock() {
                        let _ = c.kill();
                    }
                    return;
                }
                std::thread::sleep(Duration::from_millis(500));
            }
        })
    });
    let errs = stderr.map(|e| {
        let (raw_file, shown) = (raw_file.clone(), shown.clone());
        std::thread::spawn(move || {
            for l in std::io::BufReader::new(e).lines().map_while(Result::ok) {
                if let Ok(mut f) = raw_file.lock() {
                    let _ = writeln!(f, "{l}");
                }
                shown.line(&format!("  {l}"));
            }
        })
    });
    if let Some(o) = stdout {
        for l in std::io::BufReader::new(o).lines().map_while(Result::ok) {
            if let Ok(mut f) = raw_file.lock() {
                let _ = writeln!(f, "{l}");
            }
            if let Some(s) = render(&l) {
                for part in s.lines() {
                    shown.line(&format!("  {part}"));
                }
            }
        }
    }
    if let Some(e) = errs {
        let _ = e.join();
    }
    let status = child.lock().ok().and_then(|mut c| c.wait().ok());
    done.store(true, Ordering::Relaxed);
    if let Some(t) = timer {
        let _ = t.join();
    }
    Ok((status.and_then(|s| s.code()), timed_out.load(Ordering::Relaxed)))
}

/// How a session's stream ended: Some(true) with a successful final
/// result, Some(false) with a failed one or none, None when the stream is
/// no harness's we know (only its exit code tells).
fn ended(raw: &Path) -> Option<bool> {
    let text = std::fs::read_to_string(raw).ok()?;
    let (mut known, mut end) = (false, None);
    for v in text.lines().filter_map(|l| serde_json::from_str::<Value>(l).ok()) {
        match v["type"].as_str().unwrap_or_default() {
            // Claude
            "result" if v.get("num_turns").is_some() => end = Some(v["is_error"] != true),
            // Gemini
            "result" if v.get("status").is_some() => end = Some(v["status"] == "success"),
            // Codex: each turn ends completed or failed.
            "turn.completed" => end = Some(true),
            "turn.failed" => end = Some(false),
            "system" | "assistant" | "user" | "init" | "message" | "tool_use" | "tool_result" | "thread.started"
            | "turn.started" | "item.started" | "item.completed" => known = true,
            _ => {}
        }
    }
    end.or(known.then_some(false))
}

/// The tokens a session used, in its harness's own terms: Codex's
/// turn.completed usage summed over its turns, Claude Code's result usage
/// (and cost). None when the stream says nothing we read (Gemini's, or
/// output that is no stream).
fn usage(raw: &Path) -> Option<Value> {
    let text = std::fs::read_to_string(raw).ok()?;
    let mut sum = serde_json::Map::new();
    for v in text.lines().filter_map(|l| serde_json::from_str::<Value>(l).ok()) {
        match v["type"].as_str().unwrap_or_default() {
            "turn.completed" => {
                for (k, n) in v["usage"].as_object().into_iter().flatten() {
                    if let Some(n) = n.as_u64() {
                        let had = sum.get(k).and_then(Value::as_u64).unwrap_or(0);
                        sum.insert(k.clone(), json!(had + n));
                    }
                }
            }
            "result" if v.get("num_turns").is_some() => {
                let mut u = v["usage"].as_object().cloned().unwrap_or_default();
                if let Some(c) = v.get("total_cost_usd") {
                    u.insert("total_cost_usd".into(), c.clone());
                }
                return Some(u.into());
            }
            _ => {}
        }
    }
    (!sum.is_empty()).then(|| sum.into())
}

/// A session's usage on one line: its counts, key=value.
fn usage_line(u: &Value) -> String {
    u.as_object()
        .into_iter()
        .flatten()
        .filter(|(_, n)| n.is_number())
        .map(|(k, n)| format!("{k}={n}"))
        .collect::<Vec<_>>()
        .join(" ")
}

/// Why a session failed, or None when it did not.
fn failure(exit: Option<i32>, timed_out: bool, ended: Option<bool>) -> Option<String> {
    if timed_out {
        Some("it ran out of time (timeout_minutes)".into())
    } else if exit != Some(0) {
        Some(exit.map_or("it was killed or did not start".into(), |c| format!("its harness exited with {c}")))
    } else if ended == Some(false) {
        Some("its stream has no successful end".into())
    } else {
        None
    }
}

/// The role's failed runs in runs.jsonl since its last run that did not
/// fail, oldest first: each record and why it failed.
fn last_failed(jl: &Path, role: Role) -> Vec<(Value, String)> {
    let text = std::fs::read_to_string(jl).unwrap_or_default();
    let mut out: Vec<(Value, String)> = text
        .lines()
        .rev()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .filter(|r| r["role"] == role.name())
        .map_while(|rec| {
            // Records before failed was kept have only the exit and the timeout.
            let why = match rec.get("failed") {
                Some(f) => f.as_str().map(str::to_string),
                None => failure(rec["exit"].as_i64().map(|c| c as i32), rec["timed_out"] == true, None),
            }?;
            Some((rec, why))
        })
        .collect();
    out.reverse();
    out
}

/// What a session is told before its item when the role's last sessions
/// failed and its repository has uncommitted changes.
fn left_note(failed: &[(Value, String)], uncommitted: &[String]) -> String {
    const SHOWN: usize = 30;
    let mut files = uncommitted.iter().take(SHOWN).cloned().collect::<Vec<_>>().join(", ");
    if uncommitted.len() > SHOWN {
        files += &format!(" and {} more", uncommitted.len() - SHOWN);
    }
    let sessions: Vec<String> = failed
        .iter()
        .map(|(rec, why)| {
            format!(
                "started {}, item {}: {} ({why})",
                rec["started"].as_str().unwrap_or_default(),
                rec["item"].as_str().unwrap_or_default(),
                rec["what"].as_str().unwrap_or_default(),
            )
        })
        .collect();
    let which = if failed.len() == 1 {
        "This role's last session stopped before it finished".to_string()
    } else {
        format!("This role's last {} sessions stopped before they finished", failed.len())
    };
    format!(
        "{which}: {}. Its repository has uncommitted changes (git status): {files}. Before your item, look at them: finish and commit what those sessions left, or set right what is half done.",
        sessions.join("; ")
    )
}

/// The commits a run made, from HEAD before it to HEAD after; a
/// repository with no commit before the run has all of them new.
fn commits(repo: &Path, before: &str, after: &str) -> Vec<String> {
    if after.is_empty() || before == after {
        return vec![];
    }
    let range = if before.is_empty() { after.to_string() } else { format!("{before}..{after}") };
    git(repo, &["log", "--format=%h %s", &range]).lines().map(str::to_string).collect()
}

fn uncommitted(repo: &Path) -> Vec<String> {
    git(repo, &["status", "--porcelain", "--", ".", ":!.psx"]).lines().map(|l| l.trim().to_string()).collect()
}

fn stamp() -> String {
    crate::publish::utc_now().replace([':', '-'], "")
}

/// The roles' first items, for the end of a run.
fn firsts(lists: &[(Role, PathBuf, Vec<Value>)]) -> Value {
    lists
        .iter()
        .map(|(r, _, w)| {
            (r.name().to_string(), w.first().map_or(json!(null), |w| json!({"do": w["do"], "what": w["what"]})))
        })
        .collect::<serde_json::Map<_, _>>()
        .into()
}

impl PortRunArgs {
    fn run(&self, ctx: &Ctx) -> crate::Result<Value> {
        if self.follow {
            return follow(ctx);
        }
        let cfg = agents(&ctx.root)?;
        if let Some(role) = self.shell {
            return shell(ctx, &cfg, role);
        }
        let max = self.max_runs.or(cfg.max_runs).unwrap_or(10).max(1);
        let dir = dir(ctx);
        let _lock = if self.dry_run { None } else { Some(Lock::take(&dir)?) };
        #[cfg(unix)]
        if cfg.sandbox && !crate::sandbox::available() {
            return Err(Error::invalid(
                "port run runs each session in bubblewrap's sandbox, and bwrap does not run here: install bubblewrap, or write sandbox = false under [agents] to run the sessions with only the harness's own access rules",
            ));
        }
        let mut runs = Vec::new();
        let mut turn = 0;
        let stopped = loop {
            let lists = crate::protocol::work_lists(ctx)?;
            let busy = |k: &usize| !crate::protocol::waits(lists[*k].2.first());
            let agent_of = |k: usize| cfg.roles.iter().find(|a| a.0 == lists[k].0).map(|a| &a.1);
            // A role with no [agents] entry is the person's to run.
            let by_hand: Vec<&str> =
                (0..ORDER.len()).filter(busy).filter(|&k| agent_of(k).is_none()).map(|k| lists[k].0.name()).collect();
            let Some(k) =
                (0..ORDER.len()).map(|i| (turn + i) % ORDER.len()).filter(busy).find(|&k| agent_of(k).is_some())
            else {
                if cfg.roles.is_empty() && !by_hand.is_empty() {
                    return Err(Error::invalid(format!(
                        "the {} has work and protocol.toml has no [agents]: write the command line that runs one headless session in a role's repository under [agents.ROLE], e.g. command = [\"claude\", \"-p\", \"{{prompt}}\", \"--output-format\", \"stream-json\", \"--verbose\"] (guide topic agents)",
                        by_hand[0]
                    )));
                }
                let why = if by_hand.is_empty() {
                    "no role has work: each waits on another, or on the person (protocol report)".to_string()
                } else {
                    format!(
                        "only roles with no [agents] entry have work, which the person runs: {}",
                        by_hand.join(", ")
                    )
                };
                break json!({"why": why, "roles": firsts(&lists)});
            };
            if runs.len() >= max {
                break json!({"why": format!("{max} runs (max_runs)"), "roles": firsts(&lists)});
            }
            let (role, repo, work) = &lists[k];
            let others: Vec<PathBuf> = lists.iter().filter(|l| l.0 != *role).map(|l| l.1.clone()).collect();
            let item = &work[0];
            let Some(agent) = agent_of(k) else { unreachable!("a role is chosen only with an agent") };
            let jl = dir.join("runs.jsonl");
            let mut prompt = agent
                .prompt
                .as_deref()
                .unwrap_or(default_prompt(*role))
                .replace("{work}", &item_text(item))
                .replace("{role}", role.name());
            let left = uncommitted(repo);
            let failed_before = last_failed(&jl, *role);
            if !failed_before.is_empty() && !left.is_empty() {
                prompt = format!("{} {prompt}", left_note(&failed_before, &left));
            }
            let cmd = command_line(*role, agent, repo, &prompt)?;
            if self.dry_run {
                let (argv, _) = sandboxed(cfg.sandbox, *role, agent, repo, &others, cmd.clone(), None, false)?;
                let argv: Vec<String> = argv.iter().map(|a| a.to_string_lossy().into_owned()).collect();
                return Ok(
                    json!({"would_run": role.name(), "repository": repo, "item": item["do"], "what": item["what"], "command": cmd, "sandboxed": cfg.sandbox.then_some(argv), "roles": firsts(&lists)}),
                );
            }
            let before = state(repo, work);
            let name = format!("{}-{}", stamp(), role.name());
            let (argv, _servers) = sandboxed(
                cfg.sandbox,
                *role,
                agent,
                repo,
                &others,
                cmd.clone(),
                Some(&dir.join(format!("{name}.mcp.log"))),
                false,
            )?;
            let txt = dir.join(format!("{name}.txt"));
            let raw = dir.join(format!("{name}.log"));
            let shown = Arc::new(Shown(Mutex::new(std::fs::File::create(&txt).map_err(|e| Error::io(&txt, e))?)));
            let start = Instant::now();
            let started = crate::publish::utc_now();
            shown.line(&format!(
                "== {} {}: {} ({})",
                started,
                role.name(),
                item["do"].as_str().unwrap_or_default(),
                repo.display()
            ));
            let (exit, timed_out) = session(&argv, repo, &raw, &shown, cfg.timeout)?;
            let seconds = start.elapsed().as_secs();

            let after_lists = crate::protocol::work_lists(ctx)?;
            let after_work = &after_lists[k].2;
            let after = state(repo, after_work);
            let commits = commits(repo, &before.1, &after.1);
            let changed = uncommitted(repo);
            let progress = before != after;
            let failed = failure(exit, timed_out, ended(&raw));
            let used = usage(&raw);
            shown.line(&format!(
                "== {} {} s, exit {}{}{}; {} commits, {} files uncommitted; now: {}",
                role.name(),
                seconds,
                exit.map_or("none".into(), |c| c.to_string()),
                if timed_out { " (timed out)" } else { "" },
                failed.as_deref().map(|f| format!(" (failed: {f})")).unwrap_or_default(),
                commits.len(),
                changed.len(),
                after_work.first().and_then(|w| w["what"].as_str()).unwrap_or("nothing")
            ));
            if let Some(u) = &used {
                shown.line(&format!("== tokens: {}", usage_line(u)));
            }
            let rec = json!({
                "started": started, "role": role.name(), "repository": repo, "item": item["do"], "what": item["what"],
                "command": cmd, "sandbox": cfg.sandbox, "exit": exit, "timed_out": timed_out, "failed": failed, "usage": used, "seconds": seconds,
                "commits": commits, "uncommitted": changed, "progress": progress,
                "now": after_work.first().map(|w| w["do"].clone()), "log": raw, "shown": txt,
            });
            let mut f =
                std::fs::OpenOptions::new().create(true).append(true).open(&jl).map_err(|e| Error::io(&jl, e))?;
            let _ = writeln!(f, "{}", serde_json::to_string(&rec).unwrap_or_default());
            runs.push(rec);
            if let Some(f) = &failed {
                let left = if changed.is_empty() {
                    String::new()
                } else {
                    format!(
                        "; it left {} files uncommitted, which the {}'s next session is told about",
                        changed.len(),
                        role.name()
                    )
                };
                break json!({"why": format!("the {}'s run failed: {f}{left}. Read its log, {}, and start port run again when the harness can run", role.name(), raw.display()), "roles": firsts(&after_lists)});
            }
            if !progress {
                break json!({"why": format!("the {}'s run changed nothing (its work and its repository are as before): read its log, {}", role.name(), raw.display()), "roles": firsts(&after_lists)});
            }
            turn = k + 1;
        };
        if !self.dry_run {
            eprintln!("== stopped: {}", stopped["why"].as_str().unwrap_or_default());
        }
        Ok(json!({"runs": runs, "stopped": stopped, "log": dir.join("runs.jsonl")}))
    }
}

/// An interactive shell in a role's sandbox, with its servers.
fn shell(ctx: &Ctx, cfg: &Agents, role: Role) -> crate::Result<Value> {
    if !cfg.sandbox {
        return Err(Error::invalid("protocol.toml's [agents] has sandbox = false: there is no sandbox to open"));
    }
    #[cfg(unix)]
    if !crate::sandbox::available() {
        return Err(Error::invalid("bwrap does not run here: install bubblewrap"));
    }
    let lists = crate::protocol::work_lists(ctx)?;
    let k = lists.iter().position(|l| l.0 == role).unwrap_or(0);
    let repo = &lists[k].1;
    let others: Vec<PathBuf> = lists.iter().filter(|l| l.0 != role).map(|l| l.1.clone()).collect();
    let agent = cfg.roles.iter().find(|a| a.0 == role).map(|a| a.1.clone()).unwrap_or_default();
    let sh = std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".into());
    let dir = dir(ctx);
    std::fs::create_dir_all(&dir).map_err(|e| Error::io(&dir, e))?;
    let log = dir.join(format!("{}-{}-shell.mcp.log", stamp(), role.name()));
    let (argv, _servers) = sandboxed(true, role, &agent, repo, &others, vec![sh], Some(&log), true)?;
    eprintln!("port run: a shell in the {}'s sandbox ({}); exit to leave", role.name(), repo.display());
    let status = Command::new(&argv[0]).args(&argv[1..]).status().map_err(|e| Error::invalid(format!("bwrap: {e}")))?;
    Ok(json!({"shell": role.name(), "repository": repo, "exit": status.code()}))
}

/// The newest run's summary file.
fn newest(dir: &Path) -> Option<PathBuf> {
    std::fs::read_dir(dir)
        .ok()?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|e| e == "txt"))
        .max()
}

/// Prints the current run's summary as it grows, then each run after it,
/// until port run ends.
fn follow(ctx: &Ctx) -> crate::Result<Value> {
    let dir = dir(ctx);
    let lock = dir.join("lock");
    let mut at: Option<(PathBuf, u64)> = None;
    let mut shown = Vec::new();
    loop {
        let running = lock.is_file();
        if let Some(p) = newest(&dir) {
            if at.as_ref().is_none_or(|a| a.0 != p) {
                // Finish the one before, then start on this.
                if let Some((old, pos)) = at.take() {
                    print_from(&old, pos);
                }
                shown.push(p.clone());
                at = Some((p, 0));
            }
            if let Some((p, pos)) = &mut at {
                *pos = print_from(p, *pos);
            }
        }
        if !running {
            break;
        }
        std::thread::sleep(Duration::from_millis(500));
    }
    Ok(json!({"followed": shown, "running": false}))
}

fn print_from(p: &Path, pos: u64) -> u64 {
    use std::io::{Read, Seek, SeekFrom};
    let Ok(mut f) = std::fs::File::open(p) else { return pos };
    let _ = f.seek(SeekFrom::Start(pos));
    let mut s = String::new();
    let _ = f.read_to_string(&mut s);
    // Whole lines only; the rest comes next time.
    let end = s.rfind('\n').map_or(0, |i| i + 1);
    if end > 0 {
        eprint!("{}", &s[..end]);
    }
    pos + end as u64
}

impl Op for PortRunArgs {
    fn exec(self, ctx: &Ctx) -> Outcome {
        Outcome::result(self.run(ctx))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn harness_streams_read_as_lines() {
        let claude = r#"{"type":"assistant","message":{"content":[{"type":"text","text":"Reading the specs.\nMore."},{"type":"tool_use","name":"Bash","input":{"command":"cargo test"}}]}}"#;
        assert_eq!(render(claude).unwrap(), "Reading the specs. ...\n> Bash cargo test");
        let end = r#"{"type":"result","subtype":"success","num_turns":12,"duration_ms":61000,"total_cost_usd":0.5}"#;
        assert_eq!(render(end).unwrap(), "[end: success, 12 turns, 61 s, $0.50]");
        let codex =
            r#"{"type":"item.completed","item":{"type":"command_execution","command":"cargo test","exit_code":101}}"#;
        assert_eq!(render(codex).unwrap(), "> $ cargo test  (exit 101)");
        assert_eq!(render(r#"{"type":"item.started","item":{}}"#), None);
        assert_eq!(render(r#"{"type":"user","message":{"content":[{"type":"tool_result","content":"ok"}]}}"#), None);
        assert_eq!(render("plain words").unwrap(), "plain words");
    }

    #[test]
    fn agents_are_read_and_mistakes_named() {
        let d = std::env::temp_dir().join(format!("psx-agents-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        let w = |t: &str| std::fs::write(d.join("protocol.toml"), t).unwrap();
        w(
            "game = \"g\"\n[agents]\nmax_runs = 4\n[agents.implementer]\ncommand = [\"claude\", \"-p\", \"{prompt}\", \"--model\", \"{model}\"]\nmodel = \"m\"\n",
        );
        let a = agents(&d).unwrap();
        assert_eq!(a.max_runs, Some(4));
        let cmd = command_line(Role::Implementer, &a.roles[0].1, Path::new("/r"), "do it").unwrap();
        assert_eq!(cmd, ["claude", "-p", "do it", "--model", "m"]);
        w("[agents.implementer]\ncommand = [\"x\", \"{model}\"]\n");
        let a = agents(&d).unwrap();
        assert!(command_line(Role::Implementer, &a.roles[0].1, Path::new("/r"), "").is_err());
        w("[agents.implementer]\ncmd = [\"x\"]\n");
        assert!(agents(&d).unwrap_err().to_string().contains("cmd"));
        w("[agents]\nmaxruns = 3\n");
        assert!(agents(&d).unwrap_err().to_string().contains("maxruns"));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_session_fails_by_its_exit_its_time_or_its_stream_end() {
        let d = std::env::temp_dir().join(format!("psx-ended-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        let end = |name: &str, lines: &[&str]| {
            let p = d.join(name);
            std::fs::write(&p, lines.join("\n")).unwrap();
            ended(&p)
        };
        let codex = [r#"{"type":"thread.started","thread_id":"t"}"#, r#"{"type":"turn.started"}"#];
        assert_eq!(end("ok", &[codex[0], codex[1], r#"{"type":"turn.completed","usage":{}}"#]), Some(true));
        assert_eq!(
            end(
                "limit",
                &[codex[0], codex[1], r#"{"type":"error","message":"usage"}"#, r#"{"type":"turn.failed","error":{}}"#]
            ),
            Some(false)
        );
        // A known stream cut off before its end failed.
        assert_eq!(end("cut", &codex), Some(false));
        assert_eq!(
            end("claude", &[r#"{"type":"result","subtype":"error_max_turns","is_error":true,"num_turns":3}"#]),
            Some(false)
        );
        assert_eq!(end("gemini", &[r#"{"type":"result","status":"success"}"#]), Some(true));
        assert_eq!(end("plain", &["done", "bye"]), None);
        let _ = std::fs::remove_dir_all(&d);

        assert_eq!(failure(Some(0), false, Some(true)), None);
        assert_eq!(failure(Some(0), false, None), None);
        assert!(failure(Some(0), false, Some(false)).is_some());
        assert!(failure(Some(1), false, Some(true)).unwrap().contains("exited with 1"));
        assert!(failure(None, true, None).unwrap().contains("time"));
    }

    #[test]
    fn the_next_session_is_told_what_a_failed_one_left() {
        let d = std::env::temp_dir().join(format!("psx-left-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        let jl = d.join("runs.jsonl");
        let rec = |role: &str, extra: Value| {
            let mut r = json!({"started": "2026-10-28T18:43:28Z", "role": role, "item": "specify", "what": "specify the names"});
            r.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
            r.to_string()
        };
        // Older records have only exit and timed_out; the failures since the
        // role's last run that did not fail count, oldest first.
        let lines = [
            rec("investigator", json!({"item": "first", "exit": 1, "timed_out": false})),
            rec("investigator", json!({"item": "fine", "exit": 0, "timed_out": false})),
            rec("investigator", json!({"item": "specify", "exit": 1, "timed_out": false})),
            rec("reviewer", json!({"exit": 0, "timed_out": false, "failed": null})),
            rec(
                "investigator",
                json!({"item": "cross", "exit": 0, "timed_out": false, "failed": "its stream has no successful end"}),
            ),
        ];
        std::fs::write(&jl, lines.join("\n")).unwrap();
        let failed = last_failed(&jl, Role::Investigator);
        let items: Vec<&str> = failed.iter().map(|f| f.0["item"].as_str().unwrap()).collect();
        assert_eq!(items, ["specify", "cross"]);
        assert_eq!(failed[0].1, "its harness exited with 1");
        assert!(last_failed(&jl, Role::Reviewer).is_empty());
        assert!(last_failed(&jl, Role::Implementer).is_empty());
        let files: Vec<String> = (0..32).map(|i| format!("M notes/f{i}.md")).collect();
        let note = left_note(&failed, &files);
        assert!(note.starts_with("This role's last 2 sessions"), "{note}");
        assert!(note.contains("item specify: specify the names (its harness exited with 1); started"), "{note}");
        assert!(note.contains("M notes/f29.md and 2 more") && !note.contains("f30"), "{note}");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_session_s_usage_is_read_from_its_stream() {
        let d = std::env::temp_dir().join(format!("psx-usage-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        let used = |name: &str, lines: &[&str]| {
            let p = d.join(name);
            std::fs::write(&p, lines.join("\n")).unwrap();
            usage(&p)
        };
        // Codex: each turn's usage, summed.
        let turn =
            r#"{"type":"turn.completed","usage":{"input_tokens":100,"cached_input_tokens":80,"output_tokens":7}}"#;
        let u = used("codex", &[r#"{"type":"thread.started"}"#, turn, turn]).unwrap();
        assert_eq!(u, json!({"input_tokens": 200, "cached_input_tokens": 160, "output_tokens": 14}));
        assert_eq!(usage_line(&u), "cached_input_tokens=160 input_tokens=200 output_tokens=14");
        // Claude Code: the result's usage and cost.
        let claude = r#"{"type":"result","subtype":"success","num_turns":4,"total_cost_usd":0.5,"usage":{"input_tokens":9,"output_tokens":3}}"#;
        assert_eq!(
            used("claude", &[claude]),
            Some(json!({"input_tokens": 9, "output_tokens": 3, "total_cost_usd": 0.5}))
        );
        // A session that ran out before a turn ended used nothing we can read.
        assert_eq!(used("cut", &[r#"{"type":"thread.started"}"#, r#"{"type":"turn.failed"}"#]), None);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_prompt_names_the_item_and_its_guide_topics() {
        assert_eq!(item_text(&json!({"what": "wait"})), "wait");
        assert_eq!(
            item_text(&json!({"what": "cross it", "guide": ["barrier"]})),
            "cross it (guide topics for it: barrier; read only those unless the work turns to another)"
        );
    }

    #[test]
    fn a_first_commit_in_an_empty_repository_counts() {
        let d = std::env::temp_dir().join(format!("psx-first-commit-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        let g = |args: &[&str]| {
            Command::new("git").arg("-C").arg(&d).args(args).output().unwrap();
        };
        g(&["init", "-q"]);
        let before = git(&d, &["rev-parse", "HEAD"]);
        std::fs::write(d.join("a"), "a").unwrap();
        g(&["add", "a"]);
        g(&["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-qm", "first"]);
        let after = git(&d, &["rev-parse", "HEAD"]);
        assert_eq!(before, "");
        let c = commits(&d, &before, &after);
        assert!(c.len() == 1 && c[0].ends_with(" first"), "{c:?}");
        assert!(commits(&d, &after, &after).is_empty());
        let _ = std::fs::remove_dir_all(&d);
    }
}
