//! `psx audit`: the mechanical audit of a clean-room port.
//!
//! Run by the reviewer (its psx serves the investigator's repository, where
//! protocol.toml names the others) when `protocol next` says an audit is
//! due. It checks what no person should have to: the implementer's specs/
//! are exactly the approved bundles; every number in the implementer's code
//! appears in a spec, a case, psx's template or the reviewer's allowances
//! (numbers traced to the game's data, each with its reason); no addresses, none of the original's
//! symbol names and no eight-word run shared with the investigator's notes
//! in the code, comments or commit messages; every message the person sent
//! past the barrier check was followed by a question; the logs' hash chains
//! hold and the history the last audit ended at is still there; and the
//! implementer's session transcripts show no refused tool calls. With the
//! reviewer's judgment (after its own report in audits/ names the bundles)
//! it records the audit in the reviewer's audits/psx/ and appends the date,
//! the commits and both verdicts to the port's AUDIT.md: no reasons.
use crate::Error;
use crate::commands::{Ctx, Op, Outcome};
use crate::leak::{Checker, Form};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

#[derive(clap::Args, Clone, Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct AuditArgs {
    /// Your verdict on the code: records the audit.
    #[arg(long, value_enum)]
    #[serde(default)]
    pub judgment: Option<Judgment>,
    /// The implementer's transcripts.
    #[arg(long)]
    #[serde(default)]
    pub transcripts: Option<PathBuf>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, clap::ValueEnum, serde::Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum Judgment {
    Pass,
    Fail,
}

fn read_json(p: &Path) -> Option<Value> {
    serde_json::from_slice(&std::fs::read(p).ok()?).ok()
}

/// A number's value as text, the same however it was written (0x10, 16,
/// 16.0 and 1_6 are all "16").
fn key(literal: &str, radix: u32) -> Option<String> {
    let s: String = literal.chars().filter(|c| *c != '_' && *c != ',').collect();
    if radix != 10 {
        return u128::from_str_radix(&s, radix).ok().map(|v| v.to_string());
    }
    if let Ok(v) = s.parse::<u128>() {
        return Some(v.to_string());
    }
    let f: f64 = s.parse().ok()?;
    Some(if f.fract() == 0.0 && f.abs() < 1e30 { format!("{}", f as u128) } else { format!("{f}") })
}

/// The numbers in a text, by line: (line, value, as written). Digits inside
/// identifiers, ids (SPEC-0004) and dates are not numbers; a minus sign is
/// dropped. `grouped` also reads 1,024 as one number.
pub(crate) fn numbers(text: &str, grouped: bool) -> Vec<(usize, String, String)> {
    let mut out = Vec::new();
    for (n, line) in text.lines().enumerate() {
        let b = line.as_bytes();
        let word = |c: u8| c.is_ascii_alphanumeric() || c == b'_';
        let mut i = 0;
        while i < b.len() {
            let prev = if i > 0 { b[i - 1] } else { b' ' };
            if !b[i].is_ascii_digit() || word(prev) {
                i += 1;
                continue;
            }
            let start = i;
            let radix = match b.get(i + 1).map(|c| c | 0x20) {
                Some(b'x') if b[i] == b'0' => 16,
                Some(b'b') if b[i] == b'0' && b.get(i + 2).is_some_and(|c| *c == b'0' || *c == b'1') => 2,
                Some(b'o') if b[i] == b'0' => 8,
                _ => 10,
            };
            let digits_at = if radix == 10 { i } else { i + 2 };
            let mut j = digits_at;
            if radix == 10 {
                let group = |j: usize| {
                    grouped
                        && b.get(j) == Some(&b',')
                        && b.get(j + 1..j + 4).is_some_and(|d| d.iter().all(u8::is_ascii_digit))
                        && !b.get(j + 4).is_some_and(u8::is_ascii_digit)
                };
                while j < b.len() && (b[j].is_ascii_digit() || b[j] == b'_' || group(j)) {
                    j += if b[j] == b',' { 4 } else { 1 };
                }
                if b.get(j) == Some(&b'.') && b.get(j + 1).is_some_and(u8::is_ascii_digit) {
                    j += 1;
                    while j < b.len() && (b[j].is_ascii_digit() || b[j] == b'_') {
                        j += 1;
                    }
                }
                if b.get(j).is_some_and(|c| c | 0x20 == b'e') {
                    let k = j + 1 + usize::from(matches!(b.get(j + 1), Some(b'+' | b'-')));
                    if b.get(k).is_some_and(u8::is_ascii_digit) {
                        j = k;
                        while j < b.len() && b[j].is_ascii_digit() {
                            j += 1;
                        }
                    }
                }
            } else {
                while j < b.len() && (b[j] == b'_' || char::from(b[j]).is_digit(radix)) {
                    j += 1;
                }
            }
            let literal = &line[digits_at..j];
            // A type suffix (u32, f64) belongs to the literal.
            let mut end = j;
            while end < b.len() && word(b[end]) {
                end += 1;
            }
            let id = prev == b'-' && start >= 2 && word(b[start - 2]);
            let date = b.get(end) == Some(&b'-') && b.get(end + 1).is_some_and(u8::is_ascii_digit);
            if !id && !date && !literal.is_empty() {
                if let Some(k) = key(literal, radix) {
                    out.push((n + 1, k, line[start..end].to_string()));
                }
            }
            i = end.max(i + 1);
        }
    }
    out
}

/// Numbers too common to say where they came from: small integers, powers
/// of two and one less, powers of ten, and halves to sixteenths.
fn trivial(k: &str) -> bool {
    let Ok(v) = k.parse::<f64>() else { return false };
    if v.fract() == 0.0 {
        let n = v as u128;
        return n <= 16
            || n.is_power_of_two()
            || n.checked_add(1).is_some_and(u128::is_power_of_two)
            || (0..=12).any(|e| 10u128.pow(e) == n);
    }
    (v * 16.0).fract() == 0.0 && v <= 16.0 || [0.1, 0.01, 0.001].contains(&v)
}

/// The implementer's code files.
fn code(implementer: &Path) -> Vec<(PathBuf, String)> {
    crate::protocol::code_files(implementer)
        .into_iter()
        .filter_map(|p| Some((p.clone(), std::fs::read_to_string(&p).ok()?)))
        .collect()
}

fn git(repo: &Path, args: &[&str]) -> Option<String> {
    let o = std::process::Command::new("git").arg("-C").arg(repo).args(args).output().ok()?;
    o.status.success().then(|| String::from_utf8_lossy(&o.stdout).into_owned())
}

/// Breaks in a log whose entries each carry the sha256 of the line before
/// (None: no such log).
fn chain_breaks(p: &Path) -> Option<Vec<String>> {
    let text = std::fs::read_to_string(p).ok()?;
    let mut last: Option<String> = None;
    let mut out = Vec::new();
    for (i, line) in text.lines().enumerate().filter(|(_, l)| !l.trim().is_empty()) {
        match serde_json::from_str::<Value>(line) {
            Ok(v) if v["prev"].as_str() == last.as_deref() => {}
            Ok(_) => out.push(format!("line {}: prev does not match the entry before", i + 1)),
            Err(_) => out.push(format!("line {}: not JSON", i + 1)),
        }
        last = Some(crate::publish::sha256(line.as_bytes()));
    }
    Some(out)
}

/// Claude Code's transcript folder for a project folder.
fn transcripts_of(dir: &Path) -> Option<PathBuf> {
    let abs = std::path::absolute(dir).ok()?;
    let name: String = abs.to_string_lossy().chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '-' }).collect();
    Some(PathBuf::from(std::env::var_os("HOME")?).join(".claude/projects").join(name))
}

/// Tool calls the host refused, from Claude Code transcripts.
fn refused(dir: &Path) -> (usize, Vec<Value>) {
    let mut sessions = 0;
    let mut out = Vec::new();
    for p in crate::publish::files_under(dir) {
        if p.extension().and_then(|e| e.to_str()) != Some("jsonl") {
            continue;
        }
        sessions += 1;
        let text = std::fs::read_to_string(&p).unwrap_or_default();
        for line in text.lines().filter(|l| l.contains("is_error")) {
            let Ok(v) = serde_json::from_str::<Value>(line) else { continue };
            for item in v["message"]["content"].as_array().into_iter().flatten() {
                if item["type"] != "tool_result" || item["is_error"] != json!(true) {
                    continue;
                }
                let said = match &item["content"] {
                    Value::String(s) => s.clone(),
                    Value::Array(a) => a.iter().filter_map(|x| x["text"].as_str()).collect::<Vec<_>>().join(" "),
                    _ => String::new(),
                };
                let lower = said.to_ascii_lowercase();
                if lower.contains("denied") && lower.contains("permission") {
                    out.push(json!({
                        "session": p.file_stem().map(|s| s.to_string_lossy().into_owned()),
                        "time": v["timestamp"], "said": said.chars().take(200).collect::<String>(),
                    }));
                }
            }
        }
    }
    (sessions, out)
}

/// The 64-digit hashes in a text.
pub(crate) fn hashes(text: &str) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    let b = text.as_bytes();
    let mut i = 0;
    while i < b.len() {
        let n = b[i..].iter().take_while(|c| c.is_ascii_hexdigit()).count();
        if n == 64 {
            out.insert(text[i..i + 64].to_ascii_lowercase());
        }
        i += n.max(1);
    }
    out
}

/// psx audit's records in the reviewer's audits/psx/, oldest first.
pub(crate) fn records(reviewer: &Path) -> Vec<Value> {
    let mut out: Vec<Value> = crate::publish::files_under(&reviewer.join("audits/psx"))
        .iter()
        .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("json"))
        .filter_map(|p| read_json(p))
        .filter(|v| v["audit"] == json!(1))
        .collect();
    out.sort_by(|a, b| a["time"].as_str().cmp(&b["time"].as_str()));
    out
}

fn check(name: &str, ok: Option<bool>, what: &str, details: Value) -> Value {
    let mut v = json!({"check": name, "ok": ok, "what": what});
    if let (Some(m), Some(d)) = (v.as_object_mut(), details.as_object()) {
        m.extend(d.clone());
    }
    v
}

impl AuditArgs {
    fn run(&self, ctx: &Ctx) -> crate::Result<(bool, Value)> {
        let (implementer, reviewer, executables) = crate::roles::protocol(&ctx.root).ok_or_else(|| {
            Error::invalid("run in the investigator's repository: its protocol.toml names the others (psx port init)")
        })?;
        let investigator = ctx.root.clone();
        let reviews = reviewer.join("reviews");
        let mut checks = Vec::new();

        // The implementer's specs/ is the approved bundles.
        let log = crate::publish::read_log(&implementer)?;
        let inv = crate::publish::invariant(&implementer, &reviews, &log)?;
        checks.push(check(
            "specs_are_approved_bundles",
            Some(
                inv["unapproved"].as_array().is_none_or(Vec::is_empty)
                    && inv["missing"].as_array().is_none_or(Vec::is_empty)
                    && inv["reviews"].as_array().is_none_or(Vec::is_empty),
            ),
            "the implementer's specs/ is exactly the approved bundles that crossed, each review unchanged",
            json!({"unapproved": inv["unapproved"], "missing": inv["missing"], "reviews": inv["reviews"]}),
        ));

        // Every number in the code has somewhere else to come from.
        let files = code(&implementer);
        let mut known: BTreeMap<String, &str> = BTreeMap::new();
        let specs_dir = implementer.join("specs");
        for p in crate::publish::files_under(&specs_dir) {
            if let Some(t) = crate::leak::read_text(&p, u64::MAX) {
                let md = p.extension().and_then(|e| e.to_str()) == Some("md");
                for (_, k, _) in numbers(&t, md) {
                    known.entry(k).or_insert("specs");
                }
            }
        }
        for (_, t) in crate::template::FILES {
            for (_, k, _) in numbers(t, false) {
                known.entry(k).or_insert("psx's template");
            }
        }
        // The reviewer's allowances, each with its reason.
        let allowed_path = reviewer.join("audits/allowed.json");
        let allowed = read_json(&allowed_path).unwrap_or(json!({}));
        for (n, _) in allowed["numbers"].as_object().into_iter().flatten() {
            if let Some(k) = key(n, 10) {
                known.entry(k).or_insert("the reviewer's numbers");
            }
        }
        let mut unexplained: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for (p, text) in &files {
            let rel = crate::publish::relative(&implementer, p);
            for (line, k, written) in numbers(text, false) {
                if !trivial(&k) && !known.contains_key(&k) {
                    unexplained.entry(k).or_default().push(format!("{rel}:{line} {written}"));
                }
            }
        }
        let mut by_count: Vec<(&String, &Vec<String>)> = unexplained.iter().collect();
        by_count.sort_by_key(|(_, at)| std::cmp::Reverse(at.len()));
        checks.push(check(
            "numbers",
            Some(unexplained.is_empty()),
            "every number in the code appears in a spec or a case (or psx's template, or audits/allowed.json's numbers: those the reviewer traced to the game's data or found harmless, each with its reason); a constant leaked from the original has nowhere else to come from",
            json!({
                "files": files.len(), "unexplained": unexplained.len(),
                "numbers": by_count.iter().take(60).map(|(k, at)| json!({"number": k, "uses": at.len(), "at": at.iter().take(3).collect::<Vec<_>>()})).collect::<Vec<_>>(),
                "allowed": allowed_path,
            }),
        ));

        // Addresses, symbol names and text copied from the notes.
        let mut c = Checker::default();
        for n in crate::leak::project_symbols(&investigator).0 {
            c.add_symbol(&n);
        }
        let barrier = read_json(&implementer.join(".claude/barrier.json")).unwrap_or(json!({}));
        c.names = executables.into_iter().collect();
        c.names
            .extend(barrier["names"].as_array().into_iter().flatten().filter_map(|n| n.as_str().map(str::to_string)));
        for f in crate::leak::text_files(&investigator.join("notes")) {
            if let Some(t) = crate::leak::read_text(&f, crate::leak::NOTES_LIMIT) {
                c.runs.extend(crate::leak::runs(&t));
            }
        }
        for f in crate::leak::text_files(&specs_dir) {
            if let Some(t) = crate::leak::read_text(&f, crate::leak::NOTES_LIMIT) {
                for r in crate::leak::runs(&t) {
                    c.runs.remove(&r);
                }
            }
        }
        let let_through = |t: &str| allowed["text"].get(t).is_some();
        let (mut named, mut copied) = (Vec::new(), Vec::new());
        for (p, text) in &files {
            let rel = crate::publish::relative(&implementer, p);
            for f in c.check(text, Form::Prose) {
                let at = json!({"file": rel, "line": f.line, "kind": f.kind, "text": f.text});
                match f.kind {
                    "address" | "symbol" | "name" if !let_through(&f.text) => named.push(at),
                    "copied" => copied.push(at),
                    _ => {}
                }
            }
        }
        // The commits since the last audit, and their messages.
        let previous = records(&reviewer);
        let since = previous.last().and_then(|r| r["implementer_commits"]["to"].as_str().map(str::to_string));
        let head = git(&implementer, &["rev-parse", "HEAD"]).map(|h| h.trim().to_string());
        let history_kept = match (&since, &head) {
            (Some(s), Some(_)) => git(&implementer, &["merge-base", "--is-ancestor", s, "HEAD"]).is_some(),
            _ => true,
        };
        let range = match (&since, history_kept) {
            (Some(s), true) => format!("{s}..HEAD"),
            _ => "HEAD".to_string(),
        };
        let log_text = head.as_ref().and_then(|_| git(&implementer, &["log", "--format=%H%x1f%B%x1e", &range]));
        let mut commits = 0;
        for entry in log_text.as_deref().unwrap_or_default().split('\x1e') {
            let Some((hash, msg)) = entry.trim_start().split_once('\x1f') else { continue };
            commits += 1;
            for f in c.check(msg, Form::Prose) {
                if matches!(f.kind, "address" | "symbol" | "name" | "copied") && !let_through(&f.text) {
                    let at =
                        json!({"commit": &hash[..12.min(hash.len())], "line": f.line, "kind": f.kind, "text": f.text});
                    if f.kind == "copied" { copied.push(at) } else { named.push(at) }
                }
            }
        }
        checks.push(check(
            "addresses_and_symbols",
            Some(named.is_empty()),
            "no addresses, none of the original's symbol names and not the executable's name, in the code, its comments or the commit messages (audits/allowed.json's text lets one through, with its reason)",
            json!({"symbols": c.symbols.len(), "found": named}),
        ));
        checks.push(check(
            "copied_from_notes",
            Some(copied.is_empty()),
            "no run of eight words shared between the code, comments or commit messages and the investigator's notes (what crossed in specs aside)",
            json!({"found": copied}),
        ));

        // Messages past the barrier check, each followed by a question.
        let guidance = implementer.join(barrier["log"].as_str().unwrap_or("notes/guidance.jsonl"));
        let qdir = implementer.join("questions");
        let mut asked: Vec<u64> = Vec::new();
        for p in crate::publish::files_under(&qdir) {
            let name = p.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
            if !(name.starts_with("Q-") && name.ends_with(".md")) {
                continue;
            }
            let rel = crate::publish::relative(&implementer, &p);
            let added = git(&implementer, &["log", "--diff-filter=A", "--format=%ct", "--", &rel])
                .and_then(|t| t.lines().last().and_then(|l| l.trim().parse().ok()));
            asked.push(added.unwrap_or_else(|| {
                std::fs::metadata(&p)
                    .and_then(|m| m.modified())
                    .ok()
                    .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                    .map_or(0, |d| d.as_secs())
            }));
        }
        let mut unanswered = Vec::new();
        let mut crossings = 0;
        for line in std::fs::read_to_string(&guidance).unwrap_or_default().lines() {
            let Ok(v) = serde_json::from_str::<Value>(line) else { continue };
            if v["verdict"] != "crossing" {
                continue;
            }
            crossings += 1;
            let t = v["time"].as_str().and_then(crate::publish::unix).unwrap_or(0);
            if !asked.iter().any(|a| *a >= t) {
                unanswered.push(v["time"].clone());
            }
        }
        checks.push(check(
            "crossings_followed_by_questions",
            Some(unanswered.is_empty()),
            "every message the person sent past the barrier check ([cross]) was followed by a question for the investigator",
            json!({"crossings": crossings, "without_a_question": unanswered}),
        ));

        // The logs' chains, and the history the last audit ended at.
        let mut breaks = serde_json::Map::new();
        let pl = inv["log_breaks"].as_array().cloned().unwrap_or_default();
        if !pl.is_empty() {
            breaks.insert("notes/published.jsonl".into(), json!(pl));
        }
        for rel in [crate::publish::relative(&implementer, &guidance), "notes/oracle.jsonl".to_string()] {
            if let Some(b) = chain_breaks(&implementer.join(&rel)).filter(|b| !b.is_empty()) {
                breaks.insert(rel, json!(b));
            }
        }
        if !history_kept {
            breaks.insert(
                "git".into(),
                json!([format!(
                    "the commit the last audit ended at ({}) is no longer in the history",
                    since.clone().unwrap_or_default()
                )]),
            );
        }
        checks.push(check(
            "logs_intact",
            Some(breaks.is_empty()),
            "each entry of the publishing, guidance and clean-mode logs carries the hash of the one before, and the implementer's history still holds the last audited commit",
            json!({"breaks": breaks}),
        ));

        // Refused tool calls in the implementer's sessions.
        let tdir = self.transcripts.as_ref().map(|p| ctx.root.join(p)).or_else(|| transcripts_of(&implementer));
        let denied = match tdir.as_ref().filter(|d| d.is_dir()) {
            Some(d) => {
                let (sessions, found) = refused(d);
                check(
                    "refused_tool_calls",
                    Some(found.is_empty()),
                    "the implementer's session transcripts show no tool call the host refused (an attempt on a denied path is evidence even when it failed)",
                    json!({"transcripts": d, "sessions": sessions, "found": found}),
                )
            }
            None => check(
                "refused_tool_calls",
                None,
                "not checked: no transcripts where the implementer's host keeps them (give transcripts)",
                json!({"looked_in": tdir}),
            ),
        };
        checks.push(denied);

        let passed = checks.iter().all(|c| c["ok"] != json!(false));
        let audited = crate::protocol::audited(&reviewer);
        let bundles: Vec<String> = log
            .entries
            .iter()
            .filter_map(|e| e["bundle"].as_str())
            .filter(|h| !audited.contains(&h.to_ascii_lowercase()))
            .map(str::to_string)
            .collect();
        let commits_v = json!({"from": if history_kept { since.clone() } else { None }, "to": head, "count": commits});
        let mut out = json!({
            "mechanical": if passed { "pass" } else { "fail" }, "checks": checks,
            "bundles": bundles, "implementer_commits": commits_v,
        });
        let Some(judgment) = self.judgment else {
            out["next"] = json!(if bundles.is_empty() {
                "no crossed bundle waits for an audit"
            } else {
                "read the implementer's new code against the investigator's notes (guide topic review), write your report in audits/ naming these bundle hashes, then run audit again with judgment pass or fail to record it"
            });
            return Ok((passed, out));
        };

        // Recording: the reviewer's own report must name every bundle.
        let mut reports = Vec::new();
        for p in crate::publish::files_under(&reviewer.join("audits")) {
            let rel = crate::publish::relative(&reviewer.join("audits"), &p);
            if rel.starts_with("psx/") || rel == "README.md" || rel == "allowed.json" {
                continue;
            }
            let named = hashes(&std::fs::read_to_string(&p).unwrap_or_default());
            if bundles.iter().all(|b| named.contains(&b.to_ascii_lowercase())) {
                reports.push(rel);
            }
        }
        if reports.is_empty() {
            return Err(Error::invalid(format!(
                "no report in {} names every bundle this audit covers ({}): write yours first",
                reviewer.join("audits").display(),
                bundles.join(", ")
            )));
        }
        let judgment = if judgment == Judgment::Pass { "pass" } else { "fail" };
        let time = crate::publish::utc_now();
        let record = json!({
            "audit": 1, "time": time, "psx": crate::context::version(),
            "bundles": bundles, "implementer_commits": commits_v,
            "mechanical": out["mechanical"], "judgment": judgment, "reports": reports, "checks": out["checks"],
        });
        let stamp: String = time.chars().filter(|c| c.is_ascii_digit()).collect();
        let path = reviewer.join("audits/psx").join(format!("{stamp}.json"));
        crate::protocol::write(
            &path,
            format!("{}\n", serde_json::to_string_pretty(&record).unwrap_or_default()).as_bytes(),
        )?;

        // The port's record: date, commits and verdicts, no reasons.
        let audit_md = implementer.join("AUDIT.md");
        let mut text = std::fs::read_to_string(&audit_md).unwrap_or_else(|_| {
            "# Audits\n\nWritten by psx audit: each audit's date, the commits it covered and its verdicts, \
             psx's checks and the reviewer's reading. The reasons stay with the reviewer.\n\n\
             | Date | Commits | Bundles | Checks | Reviewer |\n|---|---|---|---|---|\n"
                .to_string()
        });
        let short = |h: &Option<String>| h.as_deref().map_or("start".to_string(), |h| h[..7.min(h.len())].to_string());
        if !text.ends_with('\n') {
            text.push('\n');
        }
        text += &format!(
            "| {} | {}..{} ({commits}) | {} | {} | {judgment} |\n",
            &time[..10],
            short(&commits_v["from"].as_str().map(str::to_string)),
            short(&head),
            record["bundles"].as_array().map_or(0, Vec::len),
            out["mechanical"].as_str().unwrap_or_default(),
        );
        crate::protocol::write(&audit_md, text.as_bytes())?;
        out["recorded"] = json!(path);
        out["port_record"] = json!(audit_md);
        out["judgment"] = json!(judgment);
        out["next"] = json!(if passed && judgment == "pass" {
            "recorded; commit audits/ (Role: reviewer). The implementer commits AUDIT.md"
        } else {
            "recorded with findings: the investigator fixes the specs they trace to (protocol next gives it as work), and the implementer rebuilds from the corrected specs"
        });
        Ok((passed && judgment == "pass", out))
    }
}

impl Op for AuditArgs {
    fn exec(self, ctx: &Ctx) -> Outcome {
        Outcome::report(self.run(ctx))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers_are_read_by_value_and_ids_and_dates_are_not_numbers() {
        let got: Vec<String> =
            numbers("let x = 0x1_0u32 + 4096 * 1.50f32 - abc123 + v.0; // SPEC-0123 2026-10-26 3e2", false)
                .into_iter()
                .map(|n| n.1)
                .collect();
        assert_eq!(got, ["16", "4096", "1.5", "0", "300"]);
        assert_eq!(numbers("about 1,024 units", true)[0].1, "1024");
        assert_eq!(numbers("1,024", false).len(), 2);
        assert_eq!(numbers("for i in 0..100 {}", false).iter().map(|n| n.1.as_str()).collect::<Vec<_>>(), ["0", "100"]);
        assert!(trivial("255") && trivial("0.25") && trivial("1000") && trivial("12"));
        assert!(!trivial("4097") && !trivial("0.3") && !trivial("17"));
    }

    #[test]
    fn chains_and_hashes() {
        let dir = std::env::temp_dir().join(format!("psx-audit-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let a = json!({"x": 1, "prev": null}).to_string();
        let b = json!({"x": 2, "prev": crate::publish::sha256(a.as_bytes())}).to_string();
        std::fs::write(dir.join("log.jsonl"), format!("{a}\n{b}\n")).unwrap();
        assert_eq!(chain_breaks(&dir.join("log.jsonl")).unwrap().len(), 0);
        std::fs::write(dir.join("log.jsonl"), format!("{b}\n{a}\n")).unwrap();
        assert_eq!(chain_breaks(&dir.join("log.jsonl")).unwrap().len(), 2);
        assert!(chain_breaks(&dir.join("none.jsonl")).is_none());
        let h = "d26ff517a7f50ff4b74ff96a82081950ace4d16604f7f86f5780b87a75f71fba";
        assert_eq!(hashes(&format!("bundle {h}, and {}x", &h[..63])), BTreeSet::from([h.to_string()]));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
