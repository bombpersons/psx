//! `psx publish`: carrying specs and cases across the barrier, only as the
//! reviewer approved them.
//!
//! `stage` takes what changed in the investigator's `specs/` since the last
//! crossing (specs, the glossary, cases with their observations, start-state
//! descriptions), runs the leak checker over it and, if it is clean, writes a
//! bundle to `.psx/bundles/HASH/`: the files and `bundle.json`. The hash
//! covers every file's path and sha256 and the version each replaces, so an
//! approval is of exactly those bytes on exactly that base. The reviewer
//! writes `reviews/HASH.json` (`verdict` approve or reject). `cross` copies a
//! bundle into the implementer's `specs/` only when that approval exists,
//! and appends it to the implementer's `notes/published.jsonl`, each entry
//! carrying the sha256 of the one before. `check` (and every crossing)
//! tests the invariant: the implementer's `specs/` is exactly what the
//! approved bundles in that log add up to. Each crossing also writes
//! `notes/changes/HASH.md` there: the lines each spec lost and gained, so
//! the implementer finds the rules withdrawn without diffing every spec.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use crate::Error;
use crate::commands::{Ctx, Op, Outcome};
use crate::leak::{self, BarrierCheckArgs};

#[derive(clap::Args, Clone, Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct PublishStageArgs {
    /// Files or directories under specs to stage; default all of specs (and
    /// files gone from it are removed on the other side).
    #[serde(default)]
    pub paths: Vec<PathBuf>,
    /// The implementer's repository (default protocol.toml's).
    #[arg(long)]
    #[serde(default)]
    pub to: Option<PathBuf>,
    /// The investigator's specs (default specs).
    #[arg(long)]
    #[serde(default)]
    pub specs: Option<PathBuf>,
    /// A start state whose description to publish; repeatable.
    #[arg(long)]
    #[serde(default)]
    pub state: Vec<String>,
    /// Chains directory the states come from (default notes/states).
    #[arg(long)]
    #[serde(default)]
    pub chains: Option<PathBuf>,
    /// The notes, for copied text (default notes); repeatable.
    #[arg(long)]
    #[serde(default)]
    pub notes: Vec<PathBuf>,
    /// More .symbols.json or .sym files; repeatable.
    #[arg(long)]
    #[serde(default)]
    pub symbols: Vec<PathBuf>,
    /// A name to refuse (the executable's); repeatable.
    #[arg(long)]
    #[serde(default)]
    pub name: Vec<String>,
    /// A kind of finding to let through; repeatable. The reviewer sees each.
    #[arg(long)]
    #[serde(default)]
    pub allow: Vec<String>,
}

#[derive(clap::Args, Clone, Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct PublishCrossArgs {
    /// The bundle's hash (or a unique prefix of it).
    pub bundle: String,
    /// The implementer's repository (default protocol.toml's).
    #[arg(long)]
    #[serde(default)]
    pub to: Option<PathBuf>,
    /// The reviewer's verdicts (default protocol.toml's reviewer, reviews/).
    #[arg(long)]
    #[serde(default)]
    pub reviews: Option<PathBuf>,
}

#[derive(clap::Args, Clone, Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct PublishCheckArgs {
    /// The implementer's repository (default protocol.toml's).
    #[arg(long)]
    #[serde(default)]
    pub to: Option<PathBuf>,
    /// The reviewer's verdicts (default protocol.toml's reviewer, reviews/).
    #[arg(long)]
    #[serde(default)]
    pub reviews: Option<PathBuf>,
}

pub(crate) fn sha256(bytes: &[u8]) -> String {
    use sha2::Digest;
    format!("{:x}", sha2::Sha256::digest(bytes))
}

fn read(p: &Path) -> crate::Result<Vec<u8>> {
    std::fs::read(p).map_err(|e| Error::io(p, e))
}

fn write(p: &Path, bytes: &[u8]) -> crate::Result<()> {
    if let Some(d) = p.parent() {
        std::fs::create_dir_all(d).map_err(|e| Error::io(d, e))?;
    }
    std::fs::write(p, bytes).map_err(|e| Error::io(p, e))
}

fn at(ctx: &Ctx, p: &Path) -> PathBuf {
    let p = if p.is_absolute() { p.to_path_buf() } else { ctx.root.join(p) };
    p.canonicalize().unwrap_or(p)
}

/// The time now, UTC, as 2026-10-26T12:34:56Z.
pub(crate) fn utc_now() -> String {
    iso(std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0))
}

/// Seconds since 1970 as 2026-10-26T12:34:56Z.
pub(crate) fn iso(secs: u64) -> String {
    let (days, rest) = ((secs / 86400) as i64, secs % 86400);
    // Civil date from days since 1970-01-01 (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z", rest / 3600, rest / 60 % 60, rest % 60)
}

/// 2026-10-26T12:34:56Z (or a date alone) as seconds since 1970.
pub(crate) fn unix(iso: &str) -> Option<u64> {
    let n = |r: std::ops::Range<usize>| iso.get(r)?.parse::<i64>().ok();
    let (y, m, d) = (n(0..4)?, n(5..7)?, n(8..10)?);
    let (hh, mm, ss) = if iso.len() >= 19 { (n(11..13)?, n(14..16)?, n(17..19)?) } else { (0, 0, 0) };
    let y = y - i64::from(m <= 2);
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (m + if m > 2 { -3 } else { 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    u64::try_from(days * 86400 + hh * 3600 + mm * 60 + ss).ok()
}

/// Every file under a directory (names starting with . left out), by its
/// path relative to it with /, and its sha256.
pub(crate) fn tree(dir: &Path) -> crate::Result<BTreeMap<String, String>> {
    let mut out = BTreeMap::new();
    for p in files_under(dir) {
        let rel = relative(dir, &p);
        out.insert(rel, sha256(&read(&p)?));
    }
    Ok(out)
}

pub(crate) fn files_under(path: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut todo = vec![path.to_path_buf()];
    while let Some(p) = todo.pop() {
        if p.is_dir() {
            for e in std::fs::read_dir(&p).into_iter().flatten().flatten() {
                if !e.file_name().to_string_lossy().starts_with('.') {
                    todo.push(e.path());
                }
            }
        } else if p.is_file() {
            out.push(p);
        }
    }
    out.sort();
    out
}

pub(crate) fn relative(base: &Path, p: &Path) -> String {
    let r = p.strip_prefix(base).unwrap_or(p);
    r.components().map(|c| c.as_os_str().to_string_lossy()).collect::<Vec<_>>().join("/")
}

/// A bundle's files (path, sha256, the sha256 it replaces) and removals
/// (path, the sha256 removed).
type Changes = (Vec<(String, String, Option<String>)>, Vec<(String, String)>);

/// The hash a bundle is approved by: each file's path, sha256 and the sha256
/// of the version it replaces (- for a new file), then each removal.
fn bundle_hash(files: &[(String, String, Option<String>)], removed: &[(String, String)]) -> String {
    let mut s = String::from("psx bundle 1\n");
    for (p, sha, was) in files {
        s += &format!("{p}\t{sha}\t{}\n", was.as_deref().unwrap_or("-"));
    }
    for (p, was) in removed {
        s += &format!("-\t{p}\t{was}\n");
    }
    sha256(s.as_bytes())
}

/// The implementer's log of crossings, read back: the state it adds up to,
/// with any break in its chain.
pub(crate) struct Log {
    path: PathBuf,
    pub(crate) entries: Vec<Value>,
    pub(crate) state: BTreeMap<String, String>,
    breaks: Vec<String>,
    last: Option<String>,
}

/// The lines of `old` and `new` that differ, as `-`/`+` hunks with a line
/// of context either side; `None` when they are too long to compare.
fn line_diff(old: &str, new: &str) -> Option<String> {
    let (a, b): (Vec<&str>, Vec<&str>) = (old.lines().collect(), new.lines().collect());
    if a.len().saturating_mul(b.len()) > 16_000_000 {
        return None;
    }
    // lcs[i][j]: the longest common run of a[i..] and b[j..].
    let w = b.len() + 1;
    let mut lcs = vec![0u32; (a.len() + 1) * w];
    for i in (0..a.len()).rev() {
        for j in (0..b.len()).rev() {
            lcs[i * w + j] =
                if a[i] == b[j] { lcs[(i + 1) * w + j + 1] + 1 } else { lcs[(i + 1) * w + j].max(lcs[i * w + j + 1]) };
        }
    }
    let mut ops = Vec::new();
    let (mut i, mut j) = (0, 0);
    while i < a.len() || j < b.len() {
        if i < a.len() && j < b.len() && a[i] == b[j] {
            ops.push((' ', a[i]));
            (i, j) = (i + 1, j + 1);
        } else if i < a.len() && (j == b.len() || lcs[(i + 1) * w + j] >= lcs[i * w + j + 1]) {
            ops.push(('-', a[i]));
            i += 1;
        } else {
            ops.push(('+', b[j]));
            j += 1;
        }
    }
    let near = |k: usize| ops[k.saturating_sub(1)..(k + 2).min(ops.len())].iter().any(|o| o.0 != ' ');
    let mut out = String::new();
    let mut gap = false;
    for (k, (op, line)) in ops.iter().enumerate() {
        if *op != ' ' || near(k) {
            if gap && !out.is_empty() {
                out += "...\n";
            }
            gap = false;
            out += &format!("{op} {line}\n");
        } else {
            gap = true;
        }
    }
    Some(out)
}

/// What a crossing changes in the specs, for `notes/changes/HASH.md`:
/// each spec's lines lost and gained, and the cases by count.
fn changes_note(
    hash: &str,
    target: &Path,
    dir: &Path,
    files: &[(String, String, Option<String>)],
    removed: &[(String, String)],
) -> crate::Result<String> {
    let mut out = format!(
        "# What bundle {} changed\n\nCrossed {}. Lines a spec lost (-) are rules withdrawn or rewritten: code\nthat only they backed has lost its backing.\n",
        &hash[..16.min(hash.len())],
        utc_now()
    );
    let (mut new_cases, mut changed_cases) = (0, 0);
    for (rel, _, was) in files {
        if !rel.ends_with(".md") {
            match was {
                Some(_) => changed_cases += 1,
                None => new_cases += 1,
            }
            continue;
        }
        let new = String::from_utf8_lossy(&read(&dir.join("files").join(rel))?).into_owned();
        match std::fs::read_to_string(target.join(rel)).ok().filter(|_| was.is_some()) {
            None => out += &format!("\n## {rel} (new)\n"),
            Some(old) => match line_diff(&old, &new) {
                Some(d) if d.is_empty() => {}
                Some(d) => out += &format!("\n## {rel}\n\n```diff\n{d}```\n"),
                None => out += &format!("\n## {rel} (changed; too long to compare line by line)\n"),
            },
        }
    }
    for (rel, _) in removed.iter().filter(|(r, _)| r.ends_with(".md")) {
        out += &format!("\n## {rel} (removed)\n");
    }
    let gone = removed.iter().filter(|(r, _)| !r.ends_with(".md")).count();
    out += &format!("\nOther files (cases, observations): {new_cases} new, {changed_cases} changed, {gone} removed.\n");
    Ok(out)
}

fn log_path(to: &Path) -> PathBuf {
    to.join("notes/published.jsonl")
}

pub(crate) fn read_log(to: &Path) -> crate::Result<Log> {
    let path = log_path(to);
    let mut log =
        Log { path: path.clone(), entries: Vec::new(), state: BTreeMap::new(), breaks: Vec::new(), last: None };
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(log),
        Err(e) => return Err(Error::io(&path, e)),
    };
    for (i, line) in text.lines().enumerate().filter(|(_, l)| !l.trim().is_empty()) {
        let v: Value = serde_json::from_str(line)
            .map_err(|e| Error::malformed(format!("{} line {}: {e}", path.display(), i + 1)))?;
        if v["prev"].as_str() != log.last.as_deref() {
            log.breaks.push(format!("line {}: prev does not match the entry before", i + 1));
        }
        for f in v["files"].as_array().into_iter().flatten() {
            if let (Some(p), Some(s)) = (f["path"].as_str(), f["sha256"].as_str()) {
                log.state.insert(p.to_string(), s.to_string());
            }
        }
        for p in v["removed"].as_array().into_iter().flatten().filter_map(Value::as_str) {
            log.state.remove(p);
        }
        log.last = Some(sha256(line.as_bytes()));
        log.entries.push(v);
    }
    Ok(log)
}

/// The reviewer's verdict on a bundle, with the sha256 of its file.
pub(crate) fn verdict(reviews: &Path, hash: &str) -> crate::Result<Option<(Value, String)>> {
    let p = reviews.join(format!("{hash}.json"));
    let bytes = match std::fs::read(&p) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(Error::io(&p, e)),
    };
    let v: Value = serde_json::from_slice(&bytes).map_err(|e| Error::malformed(format!("{}: {e}", p.display())))?;
    if v["bundle"].as_str() != Some(hash) {
        return Err(Error::malformed(format!("{}: its bundle is {} rather than {hash}", p.display(), v["bundle"])));
    }
    Ok(Some((v, sha256(&bytes))))
}

pub(crate) fn approved(v: &Value) -> bool {
    v["verdict"].as_str() == Some("approve")
}

/// The invariant: the implementer's specs/ is what the log's approved
/// bundles add up to, with every one of them still approved as it was.
pub(crate) fn invariant(to: &Path, reviews: &Path, log: &Log) -> crate::Result<Value> {
    let have = tree(&to.join("specs"))?;
    let mut unapproved = Vec::new();
    let mut missing = Vec::new();
    for (p, sha) in &have {
        match log.state.get(p) {
            Some(s) if s == sha => {}
            Some(_) => unapproved.push(json!({"path": p, "why": "changed since it crossed"})),
            None => unapproved.push(json!({"path": p, "why": "no approved bundle carried it"})),
        }
    }
    for p in log.state.keys() {
        if !have.contains_key(p) {
            missing.push(p.clone());
        }
    }
    let mut reviews_broken = Vec::new();
    for e in &log.entries {
        let hash = e["bundle"].as_str().unwrap_or_default();
        let why = match verdict(reviews, hash)? {
            None => Some("its review is gone"),
            Some((v, _)) if !approved(&v) => Some("its review no longer approves it"),
            Some((_, sha)) if e["review_sha256"].as_str() != Some(&sha) => {
                Some("its review was rewritten after it crossed")
            }
            Some(_) => None,
        };
        if let Some(why) = why {
            reviews_broken.push(json!({"bundle": hash, "why": why}));
        }
    }
    let holds = unapproved.is_empty() && missing.is_empty() && reviews_broken.is_empty() && log.breaks.is_empty();
    Ok(json!({
        "holds": holds, "files": have.len(), "crossings": log.entries.len(),
        "unapproved": unapproved, "missing": missing, "reviews": reviews_broken, "log_breaks": log.breaks,
    }))
}

/// The implementer's repository and the reviewer's verdicts: as given, else
/// from the investigator's protocol.toml (`psx port init`).
fn sides(ctx: &Ctx, to: &Option<PathBuf>, reviews: &Option<PathBuf>) -> crate::Result<(PathBuf, PathBuf)> {
    let protocol = crate::roles::protocol(&ctx.root);
    let to = match (to, &protocol) {
        (Some(p), _) => at(ctx, p),
        (None, Some((i, _, _))) => i.clone(),
        (None, None) => {
            return Err(Error::invalid("give to (the implementer's repository): there is no protocol.toml"));
        }
    };
    let reviews = match (reviews, &protocol) {
        (Some(p), _) => at(ctx, p),
        (None, Some((_, r, _))) => r.join("reviews"),
        (None, None) => PathBuf::new(),
    };
    Ok((to, reviews))
}

fn require_reviews(reviews: &Path) -> crate::Result<()> {
    if reviews.as_os_str().is_empty() {
        return Err(Error::invalid("give reviews (the reviewer's reviews/): there is no protocol.toml"));
    }
    Ok(())
}

pub(crate) fn bundles_dir(ctx: &Ctx) -> PathBuf {
    ctx.root.join(".psx/bundles")
}

impl PublishStageArgs {
    fn run(&self, ctx: &Ctx) -> crate::Result<(bool, Value)> {
        let specs = at(ctx, self.specs.as_deref().unwrap_or(Path::new("specs")));
        let (to, _) = sides(ctx, &self.to, &None)?;
        if !specs.is_dir() {
            return Err(Error::invalid(format!("no specs at {}", specs.display())));
        }
        // Start-state descriptions become specs/states/NAME.json first.
        let chains = at(ctx, self.chains.as_deref().unwrap_or(Path::new("notes/states")));
        let mut forced = Vec::new();
        for name in &self.state {
            let p = chains.join(format!("{name}.json"));
            let chain: Value =
                serde_json::from_slice(&read(&p)?).map_err(|e| Error::malformed(format!("{}: {e}", p.display())))?;
            let description = chain["description"].as_str().unwrap_or_default().trim();
            if description.is_empty() {
                return Err(Error::invalid(format!("state {name} has no description to publish")));
            }
            let out = specs.join(format!("states/{name}.json"));
            let text = format!(
                "{}\n",
                serde_json::to_string_pretty(&json!({"name": name, "description": description})).unwrap_or_default()
            );
            if std::fs::read_to_string(&out).ok().as_deref() != Some(text.as_str()) {
                write(&out, text.as_bytes())?;
            }
            forced.push(out);
        }
        // What to stage: the paths given (each a file or directory under
        // specs), or all of it. A case brings its observations.
        let whole = self.paths.is_empty();
        let mut picked: Vec<PathBuf> = Vec::new();
        if whole {
            picked = files_under(&specs);
        } else {
            for q in &self.paths {
                // Relative to the project or to specs.
                let p = [at(ctx, q), specs.join(q)]
                    .into_iter()
                    .find(|p| p.exists())
                    .ok_or_else(|| Error::io(q, std::io::Error::from(std::io::ErrorKind::NotFound)))?;
                let canon = |q: &Path| q.canonicalize().unwrap_or_else(|_| q.to_path_buf());
                if !canon(&p).starts_with(canon(&specs)) {
                    return Err(Error::invalid(format!("{} is not under {}", p.display(), specs.display())));
                }
                picked.extend(files_under(&p));
            }
            picked.extend(forced);
        }
        let mut source = BTreeMap::new();
        let mut kept_back = Vec::new();
        let mut kept_files = Vec::new();
        for p in &picked {
            if p.extension().and_then(|e| e.to_str()) == Some("json") {
                let Ok(v) = serde_json::from_slice::<Value>(&read(p)?) else {
                    source.insert(relative(&specs, p), p.clone());
                    continue;
                };
                let observations = v["observations"].as_str().map(|o| p.with_file_name(o));
                if v["kept_back"] == json!(true) {
                    kept_back.push(relative(&specs, p));
                    kept_files.push(p.clone());
                    kept_files.extend(observations);
                    continue;
                }
                if let Some(o) = observations.filter(|o| o.is_file()) {
                    source.insert(relative(&specs, &o), o);
                }
            }
            source.insert(relative(&specs, p), p.clone());
        }
        for k in &kept_files {
            source.remove(&relative(&specs, k));
        }
        // Against what has crossed: the files that differ, and (staging all
        // of specs) the ones gone from it.
        let log = read_log(&to)?;
        let mut files = Vec::new();
        for (rel, p) in &source {
            let sha = sha256(&read(p)?);
            let was = log.state.get(rel).cloned();
            if was.as_deref() != Some(&sha) {
                files.push((rel.clone(), sha, was));
            }
        }
        let removed: Vec<(String, String)> = if whole {
            log.state.iter().filter(|(p, _)| !source.contains_key(*p)).map(|(p, s)| (p.clone(), s.clone())).collect()
        } else {
            Vec::new()
        };
        if files.is_empty() && removed.is_empty() {
            return Ok((
                true,
                json!({"status": "nothing_to_stage", "kept_back": kept_back,
                       "next": "everything here has crossed as it is"}),
            ));
        }
        // The checks, as barrier check runs them, with the implementer's
        // published specs and questions as published text.
        let mut notes = self.notes.clone();
        if notes.is_empty() && ctx.root.join("notes").is_dir() {
            notes.push(PathBuf::from("notes"));
        }
        let barrier = BarrierCheckArgs {
            paths: Vec::new(),
            text: None,
            notes,
            published: vec![to.join("specs"), to.join("questions")],
            symbols: self.symbols.clone(),
            name: {
                let mut n = self.name.clone();
                n.extend(crate::roles::protocol(&ctx.root).map(|p| p.2).unwrap_or_default());
                n
            },
            allow: self.allow.clone(),
        };
        for k in &self.allow {
            if !leak::KINDS.iter().any(|(n, _)| n == k) {
                let all: Vec<&str> = leak::KINDS.iter().map(|(n, _)| *n).collect();
                return Err(Error::invalid(format!("allow {k:?}: one of {}", all.join(", "))));
            }
        }
        let (checker, sources) = barrier.checker(ctx)?;
        let (mut refused, mut let_through, mut unreadable) = (Vec::new(), Vec::new(), Vec::new());
        for (rel, _, _) in &files {
            let p = &source[rel];
            let Some(text) = leak::read_text(p, u64::MAX) else {
                unreadable.push(rel.clone());
                continue;
            };
            for f in checker.check(&text, leak::form_of(p)) {
                let v = json!({"file": rel, "line": f.line, "kind": f.kind, "text": f.text});
                if self.allow.iter().any(|a| a == f.kind) { let_through.push(v) } else { refused.push(v) }
            }
        }
        if !refused.is_empty() || !unreadable.is_empty() {
            return Ok((
                false,
                json!({
                    "status": "refused", "findings": refused.len(), "results": refused,
                    "unreadable": unreadable, "allowed": let_through.len(), "sources": sources,
                    "next": "rewrite each finding in game terms (what the player sees, values as observed) and stage again; only text crosses",
                }),
            ));
        }
        let hash = bundle_hash(&files, &removed);
        let dir = bundles_dir(ctx).join(&hash);
        for (rel, sha, _) in &files {
            let out = dir.join("files").join(rel);
            let bytes = read(&source[rel])?;
            if sha256(&bytes) != *sha {
                return Err(Error::invalid(format!("{rel} changed while staging; stage again")));
            }
            write(&out, &bytes)?;
        }
        let manifest = json!({
            "bundle": hash, "staged": utc_now(), "specs": specs, "to": to,
            "files": files.iter().map(|(p, s, w)| json!({"path": p, "sha256": s, "was": w})).collect::<Vec<_>>(),
            "removed": removed.iter().map(|(p, w)| json!({"path": p, "was": w})).collect::<Vec<_>>(),
            "kept_back": kept_back,
            "checks": {"checked": files.len(), "allowed": self.allow, "let_through": let_through, "sources": sources},
        });
        write(
            &dir.join("bundle.json"),
            format!("{}\n", serde_json::to_string_pretty(&manifest).unwrap_or_default()).as_bytes(),
        )?;
        let new = files.iter().filter(|f| f.2.is_none()).count();
        Ok((
            true,
            json!({
                "status": "staged", "bundle": hash, "dir": dir,
                "new": new, "changed": files.len() - new, "removed": removed.len(),
                "kept_back": kept_back, "let_through": let_through.len(),
                "next": "the reviewer reads the bundle (files/ against what has crossed) and writes reviews/HASH.json: {\"bundle\": HASH, \"verdict\": \"approve\" or \"reject\", \"reasons\": [...]}; then publish cross HASH",
            }),
        ))
    }
}

impl Op for PublishStageArgs {
    fn exec(self, ctx: &Ctx) -> Outcome {
        Outcome::report(self.run(ctx))
    }
}

/// A staged bundle by its hash or a unique prefix.
fn find_bundle(ctx: &Ctx, key: &str) -> crate::Result<PathBuf> {
    let dir = bundles_dir(ctx);
    let names: Vec<String> = std::fs::read_dir(&dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| !key.is_empty() && n.starts_with(key))
        .collect();
    match names.as_slice() {
        [one] => Ok(dir.join(one)),
        [] => Err(Error::invalid(format!("no staged bundle {key} in {}", dir.display()))),
        _ => Err(Error::invalid(format!("{key} names {} bundles; give more of the hash", names.len()))),
    }
}

/// A bundle's manifest, checked: its hash from its files, and the files it
/// holds against their sha256.
fn load_bundle(dir: &Path) -> crate::Result<(String, Changes)> {
    let p = dir.join("bundle.json");
    let m: Value = serde_json::from_slice(&read(&p)?).map_err(|e| Error::malformed(format!("{}: {e}", p.display())))?;
    let s = |v: &Value| v.as_str().unwrap_or_default().to_string();
    let files: Vec<_> = m["files"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|f| (s(&f["path"]), s(&f["sha256"]), f["was"].as_str().map(str::to_string)))
        .collect();
    let removed: Vec<_> =
        m["removed"].as_array().into_iter().flatten().map(|f| (s(&f["path"]), s(&f["was"]))).collect();
    let hash = bundle_hash(&files, &removed);
    let name = dir.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    if hash != name || s(&m["bundle"]) != name {
        return Err(Error::malformed(format!("{}: its files hash to {hash}, not its name", dir.display())));
    }
    for (rel, sha, _) in &files {
        if sha256(&read(&dir.join("files").join(rel))?) != *sha {
            return Err(Error::malformed(format!("{}: {rel} is not the file that was staged", dir.display())));
        }
    }
    Ok((hash, (files, removed)))
}

impl PublishCrossArgs {
    fn run(&self, ctx: &Ctx) -> crate::Result<(bool, Value)> {
        let (to, reviews) = sides(ctx, &self.to, &self.reviews)?;
        require_reviews(&reviews)?;
        let dir = find_bundle(ctx, &self.bundle)?;
        let (hash, (files, removed)) = load_bundle(&dir)?;
        let review_sha = match verdict(&reviews, &hash)? {
            None => {
                return Ok((
                    false,
                    json!({"status": "awaiting_review", "bundle": hash,
                           "next": format!("the reviewer reads {} and writes {}", dir.display(), reviews.join(format!("{hash}.json")).display())}),
                ));
            }
            Some((v, _)) if !approved(&v) => {
                return Ok((
                    false,
                    json!({"status": "rejected", "bundle": hash, "verdict": v["verdict"], "reasons": v["reasons"],
                           "next": "fix the specs for the reasons given and stage again"}),
                ));
            }
            Some((_, sha)) => sha,
        };
        let mut log = read_log(&to)?;
        if log.entries.iter().any(|e| e["bundle"].as_str() == Some(&hash)) {
            return Ok((
                true,
                json!({"status": "already_crossed", "bundle": hash, "invariant": invariant(&to, &reviews, &log)?}),
            ));
        }
        // Each file must replace the version it was staged against.
        let mut stale = Vec::new();
        for (p, _, was) in &files {
            if log.state.get(p) != was.as_ref() {
                stale.push(p.clone());
            }
        }
        for (p, was) in &removed {
            if log.state.get(p) != Some(was) {
                stale.push(p.clone());
            }
        }
        if !stale.is_empty() {
            return Ok((
                false,
                json!({"status": "stale", "bundle": hash, "files": stale,
                       "next": "another bundle crossed with these files since this one was staged; stage again and have that reviewed"}),
            ));
        }
        // Files that had arrived another way and that this bundle replaces
        // with something else: the breach would not show afterwards.
        let target = to.join("specs");
        let have = tree(&target)?;
        let replaced: Vec<&String> = files
            .iter()
            .filter(|(p, sha, _)| have.get(p).is_some_and(|h| log.state.get(p) != Some(h) && h != sha))
            .map(|(p, _, _)| p)
            .collect();
        // Read against the specs as they were, before they are replaced.
        let note = changes_note(&hash, &target, &dir, &files, &removed)?;
        for (rel, _, _) in &files {
            write(&target.join(rel), &read(&dir.join("files").join(rel))?)?;
        }
        for (rel, _) in &removed {
            let p = target.join(rel);
            if p.exists() {
                std::fs::remove_file(&p).map_err(|e| Error::io(&p, e))?;
            }
        }
        let entry = json!({
            "time": utc_now(), "bundle": hash, "review_sha256": review_sha,
            "files": files.iter().map(|(p, s, _)| json!({"path": p, "sha256": s})).collect::<Vec<_>>(),
            "removed": removed.iter().map(|(p, _)| p).collect::<Vec<_>>(),
            "prev": log.last,
        });
        let line = serde_json::to_string(&entry).unwrap_or_default();
        let mut text = std::fs::read_to_string(&log.path).unwrap_or_default();
        if !text.is_empty() && !text.ends_with('\n') {
            text.push('\n');
        }
        text += &line;
        text.push('\n');
        write(&log.path, text.as_bytes())?;
        let changes = to.join("notes/changes").join(format!("{hash}.md"));
        write(&changes, note.as_bytes())?;
        log = read_log(&to)?;
        let after = invariant(&to, &reviews, &log)?;
        let holds = after["holds"] == json!(true);
        Ok((
            holds,
            json!({
                "status": "crossed", "bundle": hash, "written": files.len(), "removed": removed.len(),
                "log": log.path, "changes": changes, "replaced_unapproved": replaced, "invariant": after,
                "next": if holds { "the implementer's specs are the approved bundles".to_string() }
                    else { "the implementer's specs differ from the approved bundles: see invariant; a file that arrived another way is a breach to report".to_string() },
            }),
        ))
    }
}

impl Op for PublishCrossArgs {
    fn exec(self, ctx: &Ctx) -> Outcome {
        Outcome::report(self.run(ctx))
    }
}

impl Op for PublishCheckArgs {
    fn exec(self, ctx: &Ctx) -> Outcome {
        Outcome::report((|| {
            let (to, reviews) = sides(ctx, &self.to, &self.reviews)?;
            require_reviews(&reviews)?;
            let log = read_log(&to)?;
            let v = invariant(&to, &reviews, &log)?;
            let holds = v["holds"] == json!(true);
            let mut out = json!({"status": if holds { "holds" } else { "broken" }, "log": log.path});
            if let (Value::Object(o), Value::Object(m)) = (&mut out, v) {
                o.extend(m);
            }
            Ok((holds, out))
        })())
    }
}

#[cfg(test)]
mod tests {
    use super::{bundle_hash, utc_now};

    #[test]
    fn hash_covers_the_base() {
        let a = bundle_hash(&[("a.md".into(), "11".into(), None)], &[]);
        let b = bundle_hash(&[("a.md".into(), "11".into(), Some("22".into()))], &[]);
        let c = bundle_hash(&[("a.md".into(), "11".into(), None)], &[("b.md".into(), "33".into())]);
        assert!(a != b && a != c && b != c);
        assert_eq!(a, bundle_hash(&[("a.md".into(), "11".into(), None)], &[]));
    }

    #[test]
    fn line_diff_shows_lines_lost_and_gained() {
        let old = "# S\na\nb\nc\nd\ne\nf\ng\n";
        let new = "# S\na\nb\nC\nd\ne\nf\ng\nh\n";
        assert_eq!(super::line_diff(old, new).unwrap(), "  b\n- c\n+ C\n  d\n...\n  g\n+ h\n");
        assert_eq!(super::line_diff(old, old).unwrap(), "");
    }

    #[test]
    fn time_is_iso() {
        assert_eq!(super::unix(&super::iso(1_792_999_999)), Some(1_792_999_999));
        assert_eq!(super::unix("1970-01-02"), Some(86400));
        let t = utc_now();
        assert_eq!(t.len(), 20);
        assert!(t.starts_with("20") && t.ends_with('Z'));
    }
}
