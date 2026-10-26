//! `psx protocol next`: the ledger, and what each role does now.
//!
//! The ledger is computed, never kept by hand: from the inventory and the
//! corpus coverage (the code units and whether a recording executes them),
//! the investigator's specs (front matter), provenance (which functions
//! each spec covers), case recipes (checked against the original on this
//! emulator), the publishing log (what crossed), the implementer's source
//! (which specs it cites) and its tests' verdicts (`NAME.result.json`, from
//! psx-bevy's case runner), the questions and choices, the bundles awaiting
//! review and the reviewer's audits. Each unit's stage is the highest whose
//! exit check holds, with every stage before it, so a unit whose tests
//! start failing drops back. `next` writes the ledger beside the inventory,
//! appends its totals to ledger_history.jsonl when they change, writes the
//! implementer's notes/NEXT.md (spec ids, titles and crossed case names
//! only), and says what the role asked for does now.
use crate::Error;
use crate::commands::{Ctx, Op, Outcome};
use crate::roles::Role;
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

#[derive(clap::Args, Clone, Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct ProtocolNextArgs {
    #[arg(long, value_enum)]
    pub role: Role,
}

/// Stage names, 0 to 6, and each one's credit.
const STAGES: [&str; 7] =
    ["discovered", "exercised", "specified", "evidenced", "implemented", "verified", "integrated"];
const CREDIT: [f64; 7] = [0.0, 0.05, 0.25, 0.40, 0.65, 0.85, 1.0];

/// One value of a spec's front matter.
#[derive(Debug, Clone, PartialEq)]
enum Fm {
    Str(String),
    List(Vec<String>),
    Map(Vec<(String, String)>),
}

impl Fm {
    fn str(&self) -> &str {
        match self {
            Fm::Str(s) => s,
            _ => "",
        }
    }
    fn list(&self) -> Vec<String> {
        match self {
            Fm::Str(s) if s.is_empty() => Vec::new(),
            Fm::Str(s) => vec![s.clone()],
            Fm::List(l) => l.clone(),
            Fm::Map(m) => m.iter().map(|(k, _)| k.clone()).collect(),
        }
    }
}

fn unquote(s: &str) -> String {
    let s = s.trim();
    let q = s.len() >= 2 && ((s.starts_with('"') && s.ends_with('"')) || (s.starts_with('\'') && s.ends_with('\'')));
    if q { s[1..s.len() - 1].to_string() } else { s.to_string() }
}

/// Items of a flow list or map body, split on top-level commas.
fn items(body: &str) -> Vec<String> {
    let (mut out, mut cur, mut depth, mut quote) = (Vec::new(), String::new(), 0i32, None);
    for c in body.chars() {
        match (c, quote) {
            ('"' | '\'', None) => quote = Some(c),
            (c, Some(q)) if c == q => quote = None,
            ('(' | '[' | '{', None) => depth += 1,
            (')' | ']' | '}', None) => depth -= 1,
            (',', None) if depth == 0 => {
                out.push(std::mem::take(&mut cur));
                continue;
            }
            _ => {}
        }
        cur.push(c);
    }
    out.push(cur);
    out.into_iter().map(|s| unquote(&s)).filter(|s| !s.is_empty()).collect()
}

/// A spec's front matter: scalars, flow lists (`[a, b]`, over several lines)
/// and flow maps (`{a: 1, b: 2}`).
fn front_matter(text: &str) -> Option<BTreeMap<String, Fm>> {
    let mut lines = text.lines();
    if lines.next()?.trim() != "---" {
        return None;
    }
    let mut out = BTreeMap::new();
    let mut open: Option<(String, String)> = None;
    for line in lines {
        if line.trim() == "---" {
            return Some(out);
        }
        if let Some((key, mut body)) = open.take() {
            body.push(' ');
            body.push_str(line.trim());
            open = Some((key, body));
        } else {
            match line.split_once(':') {
                Some((k, v)) if !k.starts_with(' ') && !k.trim().is_empty() => {
                    open = Some((k.trim().to_string(), v.trim().to_string()));
                }
                _ => continue,
            }
        }
        let Some((key, body)) = open.take() else { continue };
        let closes = |a: char, b: char| body.starts_with(a) && !body.ends_with(b);
        if closes('[', ']') || closes('{', '}') {
            open = Some((key, body));
            continue;
        }
        let v = if let Some(b) = body.strip_prefix('[').and_then(|b| b.strip_suffix(']')) {
            Fm::List(items(b))
        } else if let Some(b) = body.strip_prefix('{').and_then(|b| b.strip_suffix('}')) {
            Fm::Map(
                items(b)
                    .iter()
                    .map(|i| match i.split_once(':') {
                        Some((k, v)) => (unquote(k), unquote(v)),
                        None => (i.clone(), String::new()),
                    })
                    .collect(),
            )
        } else {
            Fm::Str(unquote(&body))
        };
        out.insert(key, v);
    }
    None
}

/// A spec of the investigator's.
#[derive(Debug, Clone)]
struct Spec {
    id: String,
    title: String,
    /// Under specs/.
    rel: String,
    sha: String,
    cases: Vec<String>,
    depends: Vec<String>,
    /// A format spec's record types, with their fields.
    records: Vec<(String, u32)>,
    /// A format spec's disc files (patterns, * and ?).
    files: Vec<String>,
}

impl Spec {
    fn format(&self) -> bool {
        self.id.starts_with("FMT-")
    }
}

fn specs(dir: &Path) -> crate::Result<Vec<Spec>> {
    let mut out = Vec::new();
    for p in crate::publish::files_under(dir) {
        if p.extension().and_then(|e| e.to_str()) != Some("md") {
            continue;
        }
        let bytes = std::fs::read(&p).map_err(|e| Error::io(&p, e))?;
        let text = String::from_utf8_lossy(&bytes);
        let Some(fm) = front_matter(&text) else { continue };
        let Some(id) = fm.get("id").map(|v| v.str().to_string()).filter(|s| !s.is_empty()) else { continue };
        let get = |k: &str| fm.get(k).map(Fm::list).unwrap_or_default();
        let records = match fm.get("records") {
            Some(Fm::Map(m)) => m.iter().map(|(k, v)| (k.clone(), v.trim().parse().unwrap_or(1))).collect(),
            Some(other) => other.list().into_iter().map(|k| (k, 1)).collect(),
            None => Vec::new(),
        };
        out.push(Spec {
            title: fm.get("title").map(|v| v.str().to_string()).unwrap_or_default(),
            rel: crate::publish::relative(dir, &p),
            sha: crate::publish::sha256(&bytes),
            cases: get("cases"),
            depends: get("depends"),
            records,
            files: get("files"),
            id,
        });
    }
    out.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(out)
}

/// `*` and `?` over a disc path, without its leading slash and ignoring case.
fn glob(pattern: &str, path: &str) -> bool {
    fn go(p: &[u8], s: &[u8]) -> bool {
        match (p.first(), s.first()) {
            (None, None) => true,
            (Some(b'*'), _) => go(&p[1..], s) || (!s.is_empty() && go(p, &s[1..])),
            (Some(b'?'), Some(_)) => go(&p[1..], &s[1..]),
            (Some(a), Some(b)) if a.eq_ignore_ascii_case(b) => go(&p[1..], &s[1..]),
            _ => false,
        }
    }
    go(pattern.trim_start_matches('/').as_bytes(), path.trim_start_matches('/').as_bytes())
}

/// The spec ids a text names.
fn cited(text: &str) -> BTreeSet<String> {
    let b = text.as_bytes();
    let mut out = BTreeSet::new();
    for (i, _) in text.match_indices('-') {
        let kind = if i >= 4 && &b[i - 4..i] == b"SPEC" {
            4
        } else if i >= 3 && &b[i - 3..i] == b"FMT" {
            3
        } else {
            continue;
        };
        let digits = b[i + 1..].iter().take_while(|c| c.is_ascii_digit()).count();
        let before = i - kind;
        if digits == 4 && (before == 0 || !b[before - 1].is_ascii_alphanumeric()) {
            out.insert(text[before..i + 5].to_string());
        }
    }
    out
}

/// Addresses (8 hex digits, with or without 0x) in a text.
fn addresses(text: &str) -> Vec<u32> {
    let b = text.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        if b[i].is_ascii_hexdigit()
            && (i == 0 || !b[i - 1].is_ascii_alphanumeric() || (b[i - 1] == b'x' && i >= 2 && b[i - 2] == b'0'))
        {
            let n = b[i..].iter().take_while(|c| c.is_ascii_hexdigit()).count();
            if n == 8 {
                out.extend(u32::from_str_radix(&text[i..i + 8], 16).ok());
            }
            i += n;
        } else {
            i += 1;
        }
    }
    out
}

fn read_json(p: &Path) -> Option<Value> {
    serde_json::from_slice(&std::fs::read(p).ok()?).ok()
}

fn mtime(p: &Path) -> u64 {
    std::fs::metadata(p)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_secs())
}

/// The protocol's settings: protocol.toml's `[protocol]`.
struct Settings {
    /// Specs crossed and not implemented yet before the investigator stops
    /// writing new ones.
    specs_ahead: usize,
    /// Implemented specs no audit covers before an audit is due.
    audit_every: usize,
    /// Where the implementer's tests leave their verdicts.
    results: PathBuf,
}

fn settings(root: &Path, implementer: &Path) -> Settings {
    let v: Option<toml::Value> =
        std::fs::read_to_string(root.join("protocol.toml")).ok().and_then(|t| toml::from_str(&t).ok());
    let p = v.as_ref().and_then(|v| v.get("protocol"));
    let n = |k: &str, d: usize| p.and_then(|p| p.get(k)?.as_integer()).map_or(d, |n| n.max(1) as usize);
    let results = p
        .and_then(|p| p.get("results")?.as_str())
        .map(PathBuf::from)
        .map_or_else(|| implementer.join("target/tmp"), |r| if r.is_absolute() { r } else { implementer.join(r) });
    Settings { specs_ahead: n("specs_ahead", 3), audit_every: n("audit_every", 3), results }
}

/// A test's verdict per spec, from psx-bevy's `write_result`.
struct Verdict {
    name: String,
    kind: String,
    time: u64,
    specs: BTreeMap<String, bool>,
}

fn verdicts(dir: &Path) -> Vec<Verdict> {
    let mut out = Vec::new();
    for p in crate::publish::files_under(dir) {
        if !p.to_string_lossy().ends_with(".result.json") {
            continue;
        }
        let Some(v) = read_json(&p) else { continue };
        let specs =
            v["specs"].as_object().into_iter().flatten().map(|(k, x)| (k.clone(), x.as_bool() == Some(true))).collect();
        out.push(Verdict {
            name: v["name"].as_str().unwrap_or_default().to_string(),
            kind: v["kind"].as_str().unwrap_or("case").to_string(),
            time: v["time"].as_u64().unwrap_or(0),
            specs,
        });
    }
    out
}

/// The implementer's source files' spec citations (target, data, specs,
/// questions, notes, vendor and psx-bevy left out).
fn citations(implementer: &Path) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    let mut todo = vec![implementer.to_path_buf()];
    while let Some(d) = todo.pop() {
        for e in std::fs::read_dir(&d).into_iter().flatten().flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            let p = e.path();
            if p.is_dir() {
                // psx-bevy is the template's library, not the port's code.
                let skip = ["target", "data", "specs", "questions", "notes", "vendor", "node_modules", "psx-bevy"];
                if !name.starts_with('.') && !(d == implementer && skip.contains(&name.as_str())) {
                    todo.push(p);
                }
            } else if name.ends_with(".rs") {
                out.extend(cited(&std::fs::read_to_string(&p).unwrap_or_default()));
            }
        }
    }
    out
}

/// A case the investigator holds: whether it was checked against the
/// original on this emulator.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Check {
    /// Recorded or checked on this emulator, and the same.
    Current,
    /// Checked on this emulator and different: triage it.
    Differs,
    /// Not checked since the emulator changed (or without a recipe).
    Stale,
    /// No such case.
    Missing,
}

fn case_check(cases: &Path, recipes: &Path, name: &str, emulator: &str) -> Check {
    if !cases.join(format!("{name}.json")).is_file() {
        return Check::Missing;
    }
    let Some(r) = read_json(&recipes.join(format!("{name}.json"))) else { return Check::Stale };
    if r["checked"]["emulator"].as_str() == Some(emulator) {
        return if r["checked"]["same"] == json!(true) { Check::Current } else { Check::Differs };
    }
    if r["emulator"].as_str() == Some(emulator) { Check::Current } else { Check::Stale }
}

/// The specs a case's JSON claims: its spec and its coverage's.
fn case_specs(v: &Value) -> BTreeSet<String> {
    let mut out: BTreeSet<String> = v["coverage"].as_object().into_iter().flatten().map(|(k, _)| k.clone()).collect();
    out.extend(v["spec"].as_str().map(str::to_string));
    out
}

/// Numbered items of CHOICES.md, and whether each has an answer.
fn choices(text: &str) -> Vec<(String, String, bool)> {
    let mut out: Vec<(String, String, bool)> = Vec::new();
    for line in text.lines() {
        let t = line.trim_start();
        let n = t.bytes().take_while(u8::is_ascii_digit).count();
        if line == t && n > 0 && t[n..].starts_with(". ") {
            out.push((t[..n].to_string(), t[n + 2..].trim().to_string(), false));
        } else if line.starts_with("##") {
            out.push((String::new(), String::new(), true));
        } else if t.trim_start_matches(['*', '_', '-', ' ']).to_ascii_lowercase().starts_with("answer") {
            if let Some(last) = out.last_mut() {
                last.2 = true;
            }
        }
    }
    out.retain(|c| !c.0.is_empty());
    out
}

fn short(s: &str) -> String {
    let s = s.replace("**", "");
    let mut t: String = s.chars().take(100).collect();
    if s.chars().count() > 100 {
        t.push('…');
    }
    t
}

/// The current emulator's chains: (state, made on another emulator).
fn chains(root: &Path, emulator: &str, version: &str) -> Vec<(String, bool)> {
    let mut out = Vec::new();
    for p in crate::publish::files_under(&root.join("notes/states")) {
        if p.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        let Some(v) = read_json(&p) else { continue };
        let name = p.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
        let stale = v["steps"].as_array().into_iter().flatten().any(|s| match s["emulator"].as_str() {
            Some(e) => e != emulator,
            None => s["made_with"].as_str() != Some(version),
        });
        out.push((name, stale));
    }
    out
}

fn git_head(repo: &Path) -> Option<String> {
    let o = std::process::Command::new("git").arg("-C").arg(repo).args(["rev-parse", "HEAD"]).output().ok()?;
    o.status.success().then(|| String::from_utf8_lossy(&o.stdout).trim().to_string())
}

/// 64-digit hashes named anywhere in the reviewer's audits.
fn audited(dir: &Path) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    for p in crate::publish::files_under(dir) {
        let Ok(text) = std::fs::read_to_string(&p) else { continue };
        let b = text.as_bytes();
        let mut i = 0;
        while i < b.len() {
            let n = b[i..].iter().take_while(|c| c.is_ascii_hexdigit()).count();
            if n == 64 {
                out.insert(text[i..i + 64].to_ascii_lowercase());
            }
            i += n.max(1);
        }
    }
    out
}

/// A code unit: one function of the inventory.
struct Unit {
    image: String,
    entry: u32,
    end: u32,
    instructions: u64,
    executed: bool,
    calls: u64,
    classified: Option<String>,
    specs: BTreeSet<String>,
}

fn hex(v: &Value) -> Option<u32> {
    u32::from_str_radix(v.as_str()?, 16).ok()
}

/// Every function of the inventory's executables and overlays, with what
/// coverage says of it.
fn code_units(inv: &Value, cov: Option<&Value>) -> Vec<Unit> {
    let mut seen: BTreeMap<(String, u32), (bool, u64)> = BTreeMap::new();
    for img in cov.and_then(|c| c["images"].as_array()).into_iter().flatten() {
        let name = img["name"].as_str().unwrap_or_default();
        for f in img["functions"].as_array().into_iter().flatten() {
            if let Some(e) = hex(&f["entry"]) {
                let ran = f["executed"].as_u64().unwrap_or(0) > 0;
                seen.insert((name.to_string(), e), (ran, f["calls"].as_u64().unwrap_or(0)));
            }
        }
    }
    let mut out = Vec::new();
    let exes = inv["executables"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|e| (e["path"].as_str().unwrap_or_default().to_string(), e));
    let overlays = inv["overlays"].as_array().into_iter().flatten().map(|o| {
        let lo = o["start"].as_str().unwrap_or_default();
        let name = match o["file"]["path"].as_str() {
            Some(p) => format!("{lo} {p}"),
            None => lo.to_string(),
        };
        (name, o)
    });
    for (image, e) in exes.chain(overlays) {
        for f in e["functions"].as_array().into_iter().flatten() {
            let (Some(entry), Some(end)) = (hex(&f["entry"]), hex(&f["end"])) else { continue };
            let (executed, calls) = seen.get(&(image.clone(), entry)).copied().unwrap_or((false, 0));
            out.push(Unit {
                image: image.clone(),
                entry,
                end,
                instructions: f["instructions"].as_u64().unwrap_or(0),
                executed,
                calls,
                classified: None,
                specs: BTreeSet::new(),
            });
        }
    }
    out
}

/// The image's file name, for matching a name given in provenance or a
/// classification.
fn image_file(image: &str) -> &str {
    image.rsplit(['/', ' ']).next().unwrap_or(image)
}

/// Provenance: per spec, the addresses of the routines behind it, each with
/// the image named beside it (or in the spec's `program`), if any.
fn provenance(v: &Value) -> Vec<(String, u32, Option<String>)> {
    let mut out = Vec::new();
    for (spec, p) in v.as_object().into_iter().flatten() {
        let program = p["program"].as_str().unwrap_or_default();
        for (key, _) in p["routines"].as_object().into_iter().flatten() {
            for a in addresses(key) {
                let named = [key.as_str(), program]
                    .into_iter()
                    .flat_map(|t| t.split(|c: char| c.is_whitespace() || c == '(' || c == ')' || c == ','))
                    .find(|w| w.contains('.') && !w.starts_with("0x"))
                    .map(|w| w.trim_start_matches('/').to_string());
                out.push((spec.clone(), a, named));
            }
        }
    }
    out
}

/// Ties provenance addresses and classifications to code units. Where
/// several images hold code at an address, the one named beside it; else
/// a function starting there; else the image provenance names most; else
/// the code a run executed.
fn attach(units: &mut [Unit], prov: &[(String, u32, Option<String>)], classified: &Value) {
    let mut named_most: BTreeMap<&str, usize> = BTreeMap::new();
    for (_, _, n) in prov {
        if let Some(n) = n {
            *named_most.entry(n.as_str()).or_default() += 1;
        }
    }
    let usual = named_most.iter().max_by_key(|(_, n)| **n).map(|(k, _)| k.to_string());
    let is = |u: &Unit, n: &str| image_file(&u.image).eq_ignore_ascii_case(n);
    for (spec, a, named) in prov {
        let mut holds: Vec<usize> = (0..units.len()).filter(|&i| units[i].entry <= *a && *a < units[i].end).collect();
        let narrow = |holds: &mut Vec<usize>, keep: &dyn Fn(&Unit) -> bool| {
            let kept: Vec<usize> = holds.iter().copied().filter(|&i| keep(&units[i])).collect();
            if !kept.is_empty() {
                *holds = kept;
            }
        };
        if let Some(n) = named {
            holds.retain(|&i| is(&units[i], n));
        }
        narrow(&mut holds, &|u| u.entry == *a);
        if let Some(n) = &usual {
            narrow(&mut holds, &|u| is(u, n));
        }
        narrow(&mut holds, &|u| u.executed);
        for i in holds {
            units[i].specs.insert(spec.clone());
        }
    }
    for (key, why) in classified.as_object().into_iter().flatten() {
        let mut words = key.split_whitespace().rev();
        let Some(entry) = words.next().and_then(|e| u32::from_str_radix(e.trim_start_matches("0x"), 16).ok()) else {
            continue;
        };
        let named = words.next().map(|n| n.trim_start_matches('/'));
        let how = why["as"].as_str().or(why.as_str()).unwrap_or("classified").to_string();
        for u in units.iter_mut().filter(|u| u.entry == entry) {
            if named.is_none_or(|n| image_file(&u.image).eq_ignore_ascii_case(n)) {
                u.classified = Some(how.clone());
            }
        }
    }
}

/// What a spec has reached, stage by stage.
#[derive(Debug, Default, Clone)]
struct SpecState {
    crossed: bool,
    changed: bool,
    cases: Vec<(String, Check)>,
    cited: bool,
    /// Crossed cases claiming it, with their verdict (None: no fresh one).
    port_cases: Vec<(String, Option<bool>)>,
    /// Verdicts of other kinds: (kind, name, passed).
    others: Vec<(String, String, bool)>,
    stage: usize,
}

fn spec_stage(s: &SpecState, format: bool) -> usize {
    let mut stage = 1;
    let checks = [
        s.crossed,
        s.cases.iter().all(|(_, c)| *c == Check::Current) && (format || !s.cases.is_empty()),
        s.cited,
        s.port_cases.iter().all(|(_, v)| *v == Some(true))
            && !s.others.iter().any(|(_, _, ok)| !ok)
            && (!s.port_cases.is_empty() || s.others.iter().any(|(k, _, ok)| *ok && k != "corpus")),
        s.others.iter().any(|(k, _, ok)| *ok && k == if format { "round_trip" } else { "corpus" }),
    ];
    for ok in checks {
        if !ok {
            break;
        }
        stage += 1;
    }
    stage
}

struct Repos {
    investigator: PathBuf,
    implementer: PathBuf,
    reviewer: PathBuf,
}

/// Everything `next` decides from.
struct Ledger {
    value: Value,
    investigator: Vec<Value>,
    implementer: Vec<Value>,
    reviewer: Vec<Value>,
    /// The implementer's view, for its notes/NEXT.md.
    implementer_view: String,
}

fn work(what: &str, text: String, details: Value) -> Value {
    let mut v = json!({"do": what, "what": text});
    if let (Some(m), Some(d)) = (v.as_object_mut(), details.as_object()) {
        m.extend(d.clone());
    }
    v
}

fn ledger(ctx: &Ctx, r: &Repos) -> crate::Result<Ledger> {
    let emulator = crate::context::emulator();
    let version = crate::context::version();
    let set = settings(&r.investigator, &r.implementer);
    let inv_path = crate::inventory::default_out(ctx);
    let inv = read_json(&inv_path);
    let cov_path = inv_path.with_file_name("coverage.json");
    let cov = read_json(&cov_path);
    let notes = inv_path.parent().map(Path::to_path_buf).unwrap_or_else(|| r.investigator.join("notes"));

    // Specs, and what has crossed.
    let specs_dir = r.investigator.join("specs");
    let all = specs(&specs_dir)?;
    let log = crate::publish::read_log(&r.implementer)?;
    let theirs = |rel: &str| log.state.get(&format!("specs/{rel}")).or_else(|| log.state.get(rel)).cloned();
    let impl_specs = r.implementer.join("specs");
    let impl_cases = impl_specs.join("cases");
    let (cases_dir, recipes) = (specs_dir.join("cases"), r.investigator.join("notes/recordings"));
    let cites = citations(&r.implementer);
    let results = verdicts(&set.results);

    // The investigator's cases by the specs they claim.
    let mut claims: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for p in crate::publish::files_under(&cases_dir) {
        let v = if p.extension().and_then(|e| e.to_str()) == Some("json") { read_json(&p) } else { None };
        if let Some(v) = v {
            let name = p.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
            for s in case_specs(&v) {
                claims.entry(s).or_default().insert(name.clone());
            }
        }
    }
    // The crossed cases, by the specs they claim, with when they last changed.
    let mut port_claims: BTreeMap<String, Vec<(String, u64, bool)>> = BTreeMap::new();
    for p in crate::publish::files_under(&impl_cases) {
        if p.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        let Some(v) = read_json(&p) else { continue };
        let name = p.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
        let changed = mtime(&p).max(v["observations"].as_str().map_or(0, |o| mtime(&p.with_file_name(o))));
        let counts = !matches!(v["role"].as_str(), Some("diagnostic" | "exploratory" | "corpus"));
        for s in case_specs(&v) {
            port_claims.entry(s).or_default().push((name.clone(), changed, counts));
        }
    }

    let mut states: BTreeMap<String, SpecState> = BTreeMap::new();
    for s in &all {
        let mut st = SpecState::default();
        if let Some(sha) = theirs(&s.rel) {
            st.crossed = true;
            st.changed = sha != s.sha;
        }
        let mut names: BTreeSet<String> = s.cases.iter().cloned().collect();
        names.extend(claims.get(&s.id).cloned().unwrap_or_default());
        st.cases = names
            .into_iter()
            .map(|n| {
                let c = case_check(&cases_dir, &recipes, &n, emulator);
                (n, c)
            })
            .collect();
        st.cited = cites.contains(&s.id);
        for (name, changed, counts) in port_claims.get(&s.id).into_iter().flatten() {
            if !counts {
                continue;
            }
            let v = results
                .iter()
                .filter(|v| v.name == *name && v.kind == "case" && v.time >= *changed)
                .find_map(|v| v.specs.get(&s.id).copied());
            st.port_cases.push((name.clone(), v));
        }
        for v in results.iter().filter(|v| v.kind != "case" && v.kind != "diagnostic") {
            if let Some(ok) = v.specs.get(&s.id) {
                st.others.push((v.kind.clone(), v.name.clone(), *ok));
            }
        }
        st.stage = spec_stage(&st, s.format());
        states.insert(s.id.clone(), st);
    }

    // Code units.
    let mut units = inv.as_ref().map(|i| code_units(i, cov.as_ref())).unwrap_or_default();
    let prov = read_json(&r.investigator.join("notes/provenance.json")).unwrap_or(json!({}));
    let classified = read_json(&notes.join("classified.json")).unwrap_or(json!({}));
    attach(&mut units, &provenance(&prov), &classified);
    let unit_stage = |u: &Unit| -> usize {
        if u.classified.is_some() {
            return 6;
        }
        if !u.executed {
            return 0;
        }
        u.specs.iter().filter_map(|s| states.get(s)).map(|s| s.stage).max().unwrap_or(1).max(1)
    };

    // Format units: each format spec's record types.
    let mut formats = Vec::new();
    for s in all.iter().filter(|s| s.format()) {
        let stage = states[&s.id].stage;
        if s.records.is_empty() {
            formats.push((s.id.clone(), String::new(), 1u32, stage));
        }
        for (rec, fields) in &s.records {
            formats.push((s.id.clone(), rec.clone(), *fields, stage));
        }
    }
    let disc_files: Vec<(String, u64)> = inv
        .as_ref()
        .and_then(|i| i["disc"]["files"].as_array().cloned())
        .unwrap_or_default()
        .iter()
        .filter(|f| f["executable"] != json!(true))
        .filter_map(|f| Some((f["path"].as_str()?.to_string(), f["size"].as_u64().unwrap_or(0))))
        .filter(|(p, _)| !p.eq_ignore_ascii_case("/SYSTEM.CNF"))
        .collect();
    let mut unclaimed: BTreeMap<String, (usize, u64)> = BTreeMap::new();
    for (p, size) in &disc_files {
        if !all.iter().filter(|s| s.format()).any(|s| s.files.iter().any(|g| glob(g, p))) {
            let ext = p.rsplit_once('.').map_or("(none)".to_string(), |(_, e)| format!(".{}", e.to_ascii_uppercase()));
            let e = unclaimed.entry(ext).or_default();
            e.0 += 1;
            e.1 += size;
        }
    }

    // Totals by kind and stage.
    let by_stage = |items: &mut dyn Iterator<Item = (u64, usize)>| -> Value {
        let (mut weight, mut credit, mut counts) = (0u64, 0f64, [(0u64, 0u64); 7]);
        for (w, s) in items {
            weight += w;
            credit += w as f64 * CREDIT[s];
            counts[s].0 += 1;
            counts[s].1 += w;
        }
        let stages: serde_json::Map<String, Value> =
            STAGES.iter().zip(counts).map(|(n, (u, w))| (n.to_string(), json!({"units": u, "weight": w}))).collect();
        json!({"weight": weight, "complete": if weight == 0 { 0.0 } else { (credit / weight as f64 * 10000.0).round() / 10000.0 }, "stages": stages})
    };
    let code_total = by_stage(&mut units.iter().map(|u| (u.instructions, unit_stage(u))));
    let format_total = by_stage(&mut formats.iter().map(|f| (u64::from(f.2) * 10, f.3)));
    let all_total = by_stage(
        &mut units
            .iter()
            .map(|u| (u.instructions, unit_stage(u)))
            .chain(formats.iter().map(|f| (u64::from(f.2) * 10, f.3))),
    );
    let classified_units = units.iter().filter(|u| u.classified.is_some()).count();

    // Questions and choices.
    let qdir = r.implementer.join("questions");
    let mut questions = Vec::new();
    for p in crate::publish::files_under(&qdir) {
        let name = p.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
        if !(name.starts_with("Q-") && name.ends_with(".md")) {
            continue;
        }
        let text = std::fs::read_to_string(&p).unwrap_or_default();
        let answered = text.lines().any(|l| l.starts_with('#') && l.to_ascii_lowercase().contains("answer"));
        let title = text.lines().find(|l| l.starts_with('#')).unwrap_or_default().trim_start_matches('#').trim();
        questions.push((name.trim_end_matches(".md").to_string(), short(title), cited(&text), answered));
    }
    let choice_list = choices(&std::fs::read_to_string(qdir.join("CHOICES.md")).unwrap_or_default());

    // Bundles: crossed, or current (staged on what has crossed) and waiting.
    let crossed: BTreeSet<String> =
        log.entries.iter().filter_map(|e| e["bundle"].as_str().map(str::to_string)).collect();
    let reviews = r.reviewer.join("reviews");
    let mut pending: Option<(String, String, Value)> = None;
    let bundles = crate::publish::bundles_dir(ctx);
    for d in std::fs::read_dir(&bundles).into_iter().flatten().flatten() {
        let Some(b) = read_json(&d.path().join("bundle.json")) else { continue };
        let hash = b["bundle"].as_str().unwrap_or_default().to_string();
        if crossed.contains(&hash) {
            continue;
        }
        let current =
            b["files"].as_array().into_iter().flatten().chain(b["removed"].as_array().into_iter().flatten()).all(|f| {
                let p = f["path"].as_str().unwrap_or_default();
                log.state.get(p).map(String::as_str) == f["was"].as_str()
            });
        let staged = b["staged"].as_str().unwrap_or_default().to_string();
        if current && pending.as_ref().is_none_or(|(_, s, _)| staged > *s) {
            pending = Some((hash, staged, b));
        }
    }
    let pending_verdict = match &pending {
        Some((h, _, _)) => crate::publish::verdict(&reviews, h)?.map(|(v, _)| v),
        None => None,
    };
    // What differs from what crossed, staged or not.
    let mut kept: BTreeSet<String> = BTreeSet::new();
    for p in crate::publish::files_under(&cases_dir) {
        if let Some(v) = read_json(&p).filter(|v| v["kept_back"] == json!(true)) {
            let rel = crate::publish::relative(&specs_dir, &p);
            kept.insert(rel.clone());
            if let Some(o) = v["observations"].as_str() {
                kept.insert(crate::publish::relative(&specs_dir, &p.with_file_name(o)));
            }
        }
    }
    let mine = crate::publish::tree(&specs_dir)?;
    let in_pending: BTreeMap<String, String> = pending
        .as_ref()
        .map(|(_, _, b)| {
            b["files"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|f| Some((f["path"].as_str()?.to_string(), f["sha256"].as_str()?.to_string())))
                .collect()
        })
        .unwrap_or_default();
    let unstaged: Vec<String> = mine
        .iter()
        .filter(|(p, _)| !kept.contains(*p))
        .filter(|(p, s)| log.state.get(*p) != Some(s) && in_pending.get(*p) != Some(s))
        .map(|(p, _)| p.clone())
        .collect();

    // Audits.
    let audits = audited(&r.reviewer.join("audits"));
    let mut unaudited_specs: BTreeSet<String> = BTreeSet::new();
    let mut unaudited = Vec::new();
    for e in &log.entries {
        let hash = e["bundle"].as_str().unwrap_or_default();
        if audits.contains(&hash.to_ascii_lowercase()) {
            continue;
        }
        unaudited.push(hash.to_string());
        for f in e["files"].as_array().into_iter().flatten() {
            let p = f["path"].as_str().unwrap_or_default();
            if let Some(s) = all.iter().find(|s| p == s.rel || p.strip_prefix("specs/") == Some(&s.rel)) {
                unaudited_specs.insert(s.id.clone());
            }
        }
    }
    let crossed_specs: Vec<&Spec> = all.iter().filter(|s| states[&s.id].crossed).collect();
    let open_q = |id: &str| questions.iter().any(|q| !q.3 && q.2.contains(id));
    let loop_ended =
        !crossed_specs.is_empty() && crossed_specs.iter().all(|s| states[&s.id].stage >= 5 || open_q(&s.id));
    let implemented_unaudited: Vec<&String> = unaudited_specs.iter().filter(|s| states[*s].cited).collect();
    let audit_due = !implemented_unaudited.is_empty() && (implemented_unaudited.len() >= set.audit_every || loop_ended);

    // Emulator: the start states and the cases on another emulator.
    let chain_list = chains(&r.investigator, emulator, &version);
    let stale_chains: Vec<&String> = chain_list.iter().filter(|c| c.1).map(|c| &c.0).collect();
    let mut all_cases: Vec<(String, Check)> = Vec::new();
    for p in crate::publish::files_under(&cases_dir) {
        if p.extension().and_then(|e| e.to_str()) == Some("json") {
            let name = p.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
            let c = case_check(&cases_dir, &recipes, &name, emulator);
            all_cases.push((name, c));
        }
    }
    let stale_cases: Vec<&String> = all_cases.iter().filter(|c| c.1 == Check::Stale).map(|c| &c.0).collect();
    let differing: Vec<&String> = all_cases.iter().filter(|c| c.1 == Check::Differs).map(|c| &c.0).collect();

    // The investigator's work, in order.
    let mut inv_work = Vec::new();
    let few = |v: &[&String]| -> Vec<String> { v.iter().take(12).map(|s| s.to_string()).collect() };
    if !stale_chains.is_empty() {
        inv_work.push(work(
            "rebuild_states",
            format!("the emulator changed since {} start states were made: rebuild them from power-on (states rebuild), and replace those that differ once you understand why", stale_chains.len()),
            json!({"states": few(&stale_chains)}),
        ));
    }
    if !stale_cases.is_empty() {
        inv_work.push(work(
            "check_cases",
            format!("{} cases were not checked against the original on this emulator: case check records them again (write=true replaces those that differ, after you understand why); until then they are not evidence", stale_cases.len()),
            json!({"cases": few(&stale_cases), "count": stale_cases.len()}),
        ));
    }
    if audit_due {
        inv_work.push(work(
            "wait_for_audit",
            format!("an audit is due ({} implemented specs no audit covers): no new work until the reviewer's report names the bundles", implemented_unaudited.len()),
            json!({"specs": implemented_unaudited}),
        ));
    }
    match (&pending, &pending_verdict) {
        (Some((h, _, _)), Some(v)) if !crate::publish::approved(v) => inv_work.push(work(
            "revise",
            format!(
                "the reviewer rejected bundle {}: change what the reasons say and stage again",
                &h[..12.min(h.len())]
            ),
            json!({"bundle": h, "reasons": v["reasons"]}),
        )),
        (Some((h, _, _)), Some(_)) => inv_work.push(work(
            "cross",
            format!("the reviewer approved bundle {}: publish cross it", &h[..12.min(h.len())]),
            json!({"bundle": h}),
        )),
        _ => {}
    }
    if inv.is_none() {
        inv_work.push(work(
            "inventory",
            "no inventory: take one (inventory, with snapshots of the places the game goes)".into(),
            json!({}),
        ));
    } else if cov.is_none() {
        inv_work.push(work(
            "coverage",
            "no corpus coverage: record play-throughs with run --trace and run coverage over them".into(),
            json!({}),
        ));
    } else if let (Some(i), Some(c)) = (&inv, &cov) {
        if c["inventory"]["made"] != i["made"] {
            inv_work.push(work(
                "coverage",
                "the inventory changed since coverage was taken: run coverage again".into(),
                json!({}),
            ));
        }
        let missing = c["in_no_image"].as_array().map_or(0, Vec::len);
        let outside = c["totals"]["executed_outside_functions"].as_u64().unwrap_or(0);
        if missing + outside as usize > 0 {
            inv_work.push(work(
                "inventory",
                format!("the corpus executed code the inventory lacks ({missing} stretches in no image, {outside} words in no function): take the inventory again, with snapshots from the run frames coverage gives"),
                json!({}),
            ));
        }
    }
    let open_questions: Vec<&(String, String, BTreeSet<String>, bool)> = questions.iter().filter(|q| !q.3).collect();
    if !open_questions.is_empty() {
        inv_work.push(work(
            "answer_questions",
            format!("{} questions from the implementer: answer each in game terms by changing a spec or recording a case, then append the answer naming what changed", open_questions.len()),
            json!({"questions": open_questions.iter().map(|q| json!({"question": q.0, "title": q.1, "specs": q.2})).collect::<Vec<_>>()}),
        ));
    }
    let open_choices: Vec<&(String, String, bool)> = choice_list.iter().filter(|c| !c.2).collect();
    if !open_choices.is_empty() {
        inv_work.push(work(
            "answer_choices",
            format!("{} choices the implementer made that the specs did not settle (questions/CHOICES.md): settle each in a spec or a case, and add an Answer line under it", open_choices.len()),
            json!({"choices": open_choices.iter().take(20).map(|c| json!({"choice": c.0, "text": short(&c.1)})).collect::<Vec<_>>()}),
        ));
    }
    if !differing.is_empty() {
        inv_work.push(work(
            "triage_cases",
            format!("{} cases differ from the original on this emulator: an instrument change or a case that was wrong; record them again once understood", differing.len()),
            json!({"cases": few(&differing)}),
        ));
    }
    let without_cases: Vec<Value> = all
        .iter()
        .filter_map(|s| {
            let st = &states[&s.id];
            let missing: Vec<&String> = st.cases.iter().filter(|c| c.1 == Check::Missing).map(|c| &c.0).collect();
            if !missing.is_empty() || (st.cases.is_empty() && !s.format()) {
                Some(json!({"spec": s.id, "title": s.title, "missing": missing}))
            } else {
                None
            }
        })
        .collect();
    if !without_cases.is_empty() {
        inv_work.push(work(
            "record_cases",
            format!("{} specs lack the cases their claims need (front matter cases, or none at all): record them (case record)", without_cases.len()),
            json!({"specs": without_cases}),
        ));
    }
    if !unstaged.is_empty() && pending.as_ref().is_none_or(|_| pending_verdict.is_none()) {
        inv_work.push(work(
            "stage",
            format!("{} files in specs differ from what crossed: publish stage them for the reviewer", unstaged.len()),
            json!({"files": unstaged.iter().take(20).collect::<Vec<_>>()}),
        ));
    }
    if let (Some((h, _, _)), None) = (&pending, &pending_verdict) {
        inv_work.push(work(
            "awaiting_review",
            format!("bundle {} waits for the reviewer", &h[..12.min(h.len())]),
            json!({"bundle": h}),
        ));
    }
    let behind: Vec<&str> = crossed_specs.iter().filter(|s| !states[&s.id].cited).map(|s| s.id.as_str()).collect();
    if behind.len() >= set.specs_ahead {
        inv_work.push(work(
            "hold",
            format!("{} crossed specs are not implemented yet: write no new specs until the implementer catches up (a few per loop); classify unexercised code or record the corpus meanwhile", behind.len()),
            json!({"specs": behind}),
        ));
    } else {
        if !unclaimed.is_empty() {
            let mut kinds: Vec<(&String, &(usize, u64))> = unclaimed.iter().collect();
            kinds.sort_by_key(|a| std::cmp::Reverse(a.1.1));
            inv_work.push(work(
                "specify_formats",
                format!("{} kinds of disc file no format spec names (files: in its front matter): formats come first", kinds.len()),
                json!({"kinds": kinds.iter().take(10).map(|(k, (n, b))| json!({"kind": k, "files": n, "bytes": b})).collect::<Vec<_>>()}),
            ));
        }
        let mut next: Vec<&Unit> =
            units.iter().filter(|u| u.executed && u.classified.is_none() && u.specs.is_empty()).collect();
        next.sort_by(|a, b| b.calls.cmp(&a.calls).then(b.instructions.cmp(&a.instructions)));
        if !next.is_empty() {
            inv_work.push(work(
                "specify",
                format!("{} exercised functions no spec covers (by provenance): a few at a time, as system units, bottom-up; the most called first", next.len()),
                json!({"functions": next.iter().take(8).map(|u| json!({"image": u.image, "entry": format!("{:08X}", u.entry), "instructions": u.instructions, "calls": u.calls})).collect::<Vec<_>>()}),
            ));
        }
    }
    let unreached = units.iter().filter(|u| !u.executed && u.classified.is_none()).count();
    if unreached > 0 {
        inv_work.push(work(
            "classify",
            format!("{unreached} functions no corpus run executes: record the play-through that reaches them, or classify them in notes/classified.json (dead, debug, unreachable)"),
            json!({}),
        ));
    }
    // Waiting on nothing but the gate.
    if audit_due {
        let keep = ["rebuild_states", "check_cases", "wait_for_audit"];
        inv_work.retain(|w| keep.contains(&w["do"].as_str().unwrap_or_default()));
    }

    // The implementer's work: spec ids, titles and crossed case names only.
    let mut imp_work = Vec::new();
    if audit_due {
        imp_work.push(work(
            "wait_for_audit",
            "an audit of the port is due: wait until the reviewer has done it".into(),
            json!({}),
        ));
    }
    let mut failing = Vec::new();
    let mut untested = Vec::new();
    for s in &crossed_specs {
        let st = &states[&s.id];
        if !st.cited {
            continue;
        }
        for (case, v) in &st.port_cases {
            match v {
                Some(false) => failing.push(json!({"spec": s.id, "case": case})),
                None => untested.push(json!({"spec": s.id, "case": case})),
                Some(true) => {}
            }
        }
        for (kind, name, ok) in &st.others {
            if !ok {
                failing.push(json!({"spec": s.id, "test": name, "kind": kind}));
            }
        }
    }
    if !failing.is_empty() {
        imp_work.push(work(
            "fix",
            format!(
                "failing, {} cases or tests: fix the port, or write a question where the spec is wrong or silent",
                failing.len()
            ),
            json!({"failing": failing}),
        ));
    }
    let done: BTreeSet<&str> = crossed_specs.iter().filter(|s| states[&s.id].cited).map(|s| s.id.as_str()).collect();
    let mut todo: Vec<&&Spec> = crossed_specs.iter().filter(|s| !states[&s.id].cited).collect();
    // Specs whose crossed dependencies are built come first.
    todo.sort_by_key(|s| {
        s.depends.iter().filter(|d| crossed_specs.iter().any(|c| c.id == **d) && !done.contains(d.as_str())).count()
    });
    if !todo.is_empty() {
        imp_work.push(work(
            "implement",
            format!("{} crossed specs the port does not cite yet: build them until their cases pass, citing the spec id in the code", todo.len()),
            json!({"specs": todo.iter().map(|s| json!({"spec": s.id, "title": s.title})).collect::<Vec<_>>()}),
        ));
    }
    if !untested.is_empty() {
        imp_work.push(work(
            "run_cases",
            format!("{} crossed cases of implemented specs have no verdict since they last changed: run the case tests (write_run records each verdict)", untested.len()),
            json!({"cases": untested}),
        ));
    }
    let open_mine: Vec<&String> = open_questions.iter().map(|q| &q.0).collect();
    if audit_due {
        // Only recording verdicts, for the audit to see, until it is done.
        imp_work.retain(|w| matches!(w["do"].as_str(), Some("wait_for_audit" | "run_cases")));
    }
    if crossed_specs.is_empty() {
        imp_work.push(work(
            "wait_for_specs",
            "no spec has crossed yet: read specs/README.md and the template, and wait for the first specs".into(),
            json!({}),
        ));
    } else if imp_work.is_empty() {
        imp_work.push(work(
            "wait_for_specs",
            "every crossed spec is built and its cases pass: list in questions/CHOICES.md each choice the specs did not settle, write a question wherever a spec is silent, then wait for the next specs".into(),
            json!({"open_questions": open_mine}),
        ));
    }

    // The reviewer's.
    let mut rev_work = Vec::new();
    if let (Some((h, _, _)), None) = (&pending, &pending_verdict) {
        rev_work.push(work(
            "review",
            format!(
                "bundle {} waits for a verdict: read it (guide topic review) and write reviews/HASH.json",
                &h[..12.min(h.len())]
            ),
            json!({"bundle": h, "dir": bundles.join(h)}),
        ));
    }
    if audit_due {
        rev_work.push(work(
            "audit",
            format!("an audit is due: read the implementer's code for the specs below against the investigator's notes, and write a report in audits/ naming the bundle hashes it covers ({} bundles) and the implementer's commit", unaudited.len()),
            json!({"bundles": unaudited, "specs": implemented_unaudited, "implementer_commit": git_head(&r.implementer)}),
        ));
    }
    if rev_work.is_empty() {
        rev_work.push(work("nothing", "no bundle waits for review and no audit is due".into(), json!({})));
    }

    // A spec that has not crossed is a draft, whatever covers it.
    let stage_of = |id: &str| states.get(id).map_or("", |s| if s.stage < 2 { "draft" } else { STAGES[s.stage] });
    let value = json!({
        "ledger": 1, "made": crate::publish::utc_now(), "psx": version, "emulator": emulator,
        "complete": {"code": code_total["complete"], "formats": format_total["complete"], "all": all_total["complete"]},
        "totals": {"code": code_total, "formats": format_total, "all": all_total, "classified": classified_units},
        "how": "code units are the inventory's functions weighted by instructions, format units each format spec's record types weighted by fields times ten; a unit's stage is the highest whose exit check holds with every one before it (guide topic protocol)",
        "specs": all.iter().map(|s| {
            let st = &states[&s.id];
            json!({
                "id": s.id, "title": s.title, "stage": stage_of(&s.id), "crossed": st.crossed,
                "changed_since_crossing": st.changed, "cited": st.cited,
                "cases": st.cases.iter().map(|(n, c)| json!({"case": n, "check": format!("{c:?}").to_lowercase()})).collect::<Vec<_>>(),
                "port_cases": st.port_cases.iter().map(|(n, v)| json!({"case": n, "passed": v})).collect::<Vec<_>>(),
                "code_units": units.iter().filter(|u| u.specs.contains(&s.id)).count(),
            })
        }).collect::<Vec<_>>(),
        "formats": formats.iter().map(|(s, rec, f, _)| json!({"spec": s, "record": rec, "fields": f, "stage": stage_of(s)})).collect::<Vec<_>>(),
        "unclaimed_files": unclaimed.iter().map(|(k, (n, b))| json!({"kind": k, "files": n, "bytes": b})).collect::<Vec<_>>(),
        "code": units.iter().map(|u| {
            let mut v = json!({"image": u.image, "entry": format!("{:08X}", u.entry), "instructions": u.instructions, "stage": STAGES[unit_stage(u)]});
            if !u.specs.is_empty() { v["specs"] = json!(u.specs); }
            if let Some(c) = &u.classified { v["classified"] = json!(c); }
            v
        }).collect::<Vec<_>>(),
        "questions": questions.iter().map(|q| json!({"question": q.0, "specs": q.2, "answered": q.3})).collect::<Vec<_>>(),
        "choices": {"total": choice_list.len(), "open": open_choices.len()},
        "audits": {"due": audit_due, "unaudited_bundles": unaudited, "loop_ended": loop_ended},
        "emulator_changes": {"states": stale_chains, "cases": stale_cases.len(), "differing": differing},
        "inputs": {"inventory": inv.as_ref().map(|_| &inv_path), "coverage": cov.as_ref().map(|_| &cov_path), "results": set.results},
    });

    // The implementer's view.
    let mut md = String::from("# Next\n\nWritten by psx protocol next; read it at the start of every session.\n\n");
    for (i, w) in imp_work.iter().enumerate() {
        md += &format!("{}. {}\n", i + 1, w["what"].as_str().unwrap_or_default());
        for key in ["specs", "failing", "cases"] {
            let list = w[key].as_array().cloned().unwrap_or_default();
            if list.len() > 15 {
                md += &format!("   - ({} in all; the first 15)\n", list.len());
            }
            for x in list.iter().take(15) {
                let line = match (x["spec"].as_str(), x["title"].as_str(), x["case"].as_str(), x["test"].as_str()) {
                    (Some(s), Some(t), _, _) => format!("{s}, {t}"),
                    (Some(s), _, Some(c), _) => format!("{s}: case {c}"),
                    (Some(s), _, _, Some(t)) => format!("{s}: {t}"),
                    _ => x.as_str().unwrap_or_default().to_string(),
                };
                md += &format!("   - {line}\n");
            }
        }
    }
    md += "\n## Specs\n\n| Spec | Title | Stage | Cases passing |\n|---|---|---|---|\n";
    for s in &crossed_specs {
        let st = &states[&s.id];
        let pass = st.port_cases.iter().filter(|c| c.1 == Some(true)).count();
        md += &format!("| {} | {} | {} | {pass} of {} |\n", s.id, s.title, stage_of(&s.id), st.port_cases.len());
    }
    md += &format!(
        "\nQuestions open: {}. Choices listed: {} ({} not settled yet).\n",
        open_questions.len(),
        choice_list.len(),
        open_choices.len()
    );

    Ok(Ledger { value, investigator: inv_work, implementer: imp_work, reviewer: rev_work, implementer_view: md })
}

fn write(p: &Path, bytes: &[u8]) -> crate::Result<()> {
    if let Some(d) = p.parent() {
        std::fs::create_dir_all(d).map_err(|e| Error::io(d, e))?;
    }
    std::fs::write(p, bytes).map_err(|e| Error::io(p, e))
}

impl ProtocolNextArgs {
    fn run(&self, ctx: &Ctx) -> crate::Result<Value> {
        let (implementer, reviewer, _) = crate::roles::protocol(&ctx.root).ok_or_else(|| {
            Error::invalid("run in the investigator's repository: its protocol.toml names the others (psx port init)")
        })?;
        let r = Repos { investigator: ctx.root.clone(), implementer, reviewer };
        let l = ledger(ctx, &r)?;
        let out = crate::inventory::default_out(ctx).with_file_name("ledger.json");
        write(&out, format!("{}\n", serde_json::to_string_pretty(&l.value).unwrap_or_default()).as_bytes())?;
        // Totals over time, for the estimate: a line whenever they change.
        let history = out.with_file_name("ledger_history.jsonl");
        let line = json!({"made": l.value["made"], "complete": l.value["complete"], "totals": {
            "code": l.value["totals"]["code"]["stages"], "formats": l.value["totals"]["formats"]["stages"]}});
        let last = std::fs::read_to_string(&history)
            .ok()
            .and_then(|t| t.lines().last().and_then(|l| serde_json::from_str::<Value>(l).ok()));
        if last.as_ref().is_none_or(|v| v["totals"] != line["totals"]) {
            let mut text = std::fs::read_to_string(&history).unwrap_or_default();
            text += &format!("{}\n", serde_json::to_string(&line).unwrap_or_default());
            write(&history, text.as_bytes())?;
        }
        if r.implementer.is_dir() {
            write(&r.implementer.join("notes/NEXT.md"), l.implementer_view.as_bytes())?;
        }
        let work = match self.role {
            Role::Investigator => l.investigator,
            Role::Implementer => l.implementer,
            Role::Reviewer => l.reviewer,
        };
        let mut v = json!({
            "role": self.role.name(), "now": work.first(), "work": work,
            "complete": l.value["complete"], "ledger": out,
        });
        if self.role == Role::Implementer {
            v["written"] = json!(r.implementer.join("notes/NEXT.md"));
        }
        Ok(v)
    }
}

impl Op for ProtocolNextArgs {
    fn exec(self, ctx: &Ctx) -> Outcome {
        Outcome::result(self.run(ctx))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn front_matter_reads_lists_over_lines_and_maps() {
        let fm = front_matter(
            "---\nid: FMT-0002\ntitle: \"Lara's: things\"\ndepends: [SPEC-0002, a (to be written)]\ncases: [a, b,\n        c]\nrecords: {room: 12, box: 4}\nfiles: [DATA/*.PSX]\n---\n# body\n",
        )
        .unwrap();
        assert_eq!(fm["title"], Fm::Str("Lara's: things".into()));
        assert_eq!(fm["cases"].list(), ["a", "b", "c"]);
        assert_eq!(fm["depends"].list(), ["SPEC-0002", "a (to be written)"]);
        assert_eq!(fm["records"], Fm::Map(vec![("room".into(), "12".into()), ("box".into(), "4".into())]));
        assert!(glob(&fm["files"].list()[0], "/DATA/LEVEL1.PSX"));
        assert!(!glob("DATA/*.PSX", "/DATA/LEVEL1.TXT"));
        assert!(front_matter("# no front matter\n").is_none());
    }

    #[test]
    fn citations_addresses_and_choices_are_read_from_text() {
        assert_eq!(
            cited("// SPEC-0004 and FMT-0001, not XSPEC-0001 or SPEC-01"),
            BTreeSet::from(["FMT-0001".into(), "SPEC-0004".into()])
        );
        assert_eq!(addresses("0x8006DD98 (effects loop in 8001D7FC) 0x8008B542abc"), vec![0x8006_DD98, 0x8001_D7FC]);
        let c = choices(
            "# Choices\n\n1. **Walk** is R1.\n   Answer: SPEC-0006 now says so.\n2. Steps leave her state alone.\n",
        );
        assert_eq!(c.len(), 2);
        assert!(c[0].2 && !c[1].2);
    }

    #[test]
    fn a_stage_holds_only_with_every_stage_before_it() {
        let mut s =
            SpecState { crossed: true, cases: vec![("a".into(), Check::Current)], cited: true, ..Default::default() };
        s.port_cases = vec![("a".into(), Some(true))];
        assert_eq!(STAGES[spec_stage(&s, false)], "verified");
        s.others.push(("corpus".into(), "level1".into(), true));
        assert_eq!(STAGES[spec_stage(&s, false)], "integrated");
        // A case not checked on this emulator drops it back to specified.
        s.cases[0].1 = Check::Stale;
        assert_eq!(STAGES[spec_stage(&s, false)], "specified");
        s.cases[0].1 = Check::Current;
        s.port_cases[0].1 = Some(false);
        assert_eq!(STAGES[spec_stage(&s, false)], "implemented");
        // A format spec needs no case, and integrates by round trip.
        let f = SpecState {
            crossed: true,
            cited: true,
            others: vec![("round_trip".into(), "levels".into(), true)],
            ..Default::default()
        };
        assert_eq!(STAGES[spec_stage(&f, true)], "integrated");
    }

    #[test]
    fn provenance_names_the_image_and_prefers_executed_code() {
        let prov = provenance(&json!({
            "SPEC-0001": {"routines": {"0x80011000": "x", "TOMB2.EXE 0x80012000": "y"}},
        }));
        let unit = |image: &str, entry, executed| Unit {
            image: image.into(),
            entry,
            end: entry + 0x100,
            instructions: 64,
            executed,
            calls: 0,
            classified: None,
            specs: BTreeSet::new(),
        };
        let mut units = vec![
            unit("/MAIN.EXE", 0x8001_1000, false),
            unit("/TOMB2.EXE", 0x8001_1000, true),
            unit("/MAIN.EXE", 0x8001_2000, true),
            unit("/TOMB2.EXE", 0x8001_2000, false),
        ];
        attach(&mut units, &prov, &json!({"MAIN.EXE 80011000": "dead"}));
        let specs: Vec<bool> = units.iter().map(|u| !u.specs.is_empty()).collect();
        assert_eq!(specs, [false, true, false, true]);
        assert_eq!(units[0].classified.as_deref(), Some("dead"));
        assert!(units[1].classified.is_none());
    }
}
