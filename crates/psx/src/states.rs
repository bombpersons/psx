//! `psx states`: named start states and how each was made from power-on.
//!
//! A start state is kept twice: the state itself in `.psx/states/NAME.bin`
//! (the console's whole memory, so it stays out of git and can be wiped), and
//! its chain in `notes/states/NAME.json`: every run from power-on that led to
//! it (frames, input, pokes, until conditions, cards and pad), with the
//! sha256 of the state each run ended in. `make` runs one more step from a
//! named state (or power-on) and saves the result; `save` names a state an
//! earlier run or handoff made, reading the chain back from the runs'
//! reports; `rebuild` replays chains from power-on on this psx and says
//! where the states differ.

use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use crate::Error;
use crate::commands::{Ctx, Op, Outcome};

#[derive(clap::Args, Clone, Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct StatesMakeArgs {
    /// The new state's name (letters, digits, _ and -).
    pub name: String,
    /// A named state or state file to start from; power-on when left out.
    #[arg(long)]
    #[serde(default)]
    pub from: Option<String>,
    /// Fields to run.
    #[arg(long)]
    pub frames: u32,
    /// Pad input, as run's.
    #[arg(long, conflicts_with = "replay")]
    #[serde(default)]
    pub input: Option<crate::replay::Events>,
    /// Pad input from a replay file.
    #[arg(long)]
    #[serde(default)]
    pub replay: Option<PathBuf>,
    /// Stop where these hold (run's until); repeatable.
    #[arg(long)]
    #[serde(default)]
    pub until: Vec<String>,
    /// Field map for until.
    #[arg(long)]
    #[serde(default)]
    pub map: Option<PathBuf>,
    /// Memory writes (run's poke); repeatable.
    #[arg(long = "poke")]
    #[serde(default, rename = "poke")]
    pub pokes: Vec<String>,
    /// What the state shows, in game terms.
    #[arg(long)]
    pub description: String,
    /// Disc image or EXE.
    #[arg(long)]
    #[serde(default)]
    pub media: Option<PathBuf>,
    /// From power-on: the pad (digital or dualshock).
    #[arg(long)]
    #[serde(default)]
    pub pad: Option<String>,
    /// From power-on: memory card 1 (`new` or an image).
    #[arg(long)]
    #[serde(default)]
    pub memcard1: Option<String>,
    /// From power-on: memory card 2.
    #[arg(long)]
    #[serde(default)]
    pub memcard2: Option<String>,
    #[command(flatten)]
    #[serde(flatten)]
    pub dirs: Dirs,
    /// Replace a state of this name.
    #[arg(long)]
    #[serde(default)]
    pub replace: bool,
}

#[derive(clap::Args, Clone, Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct StatesSaveArgs {
    /// A state a run or handoff wrote (state.bin, state_fN.bin, a mark's), or its directory.
    pub source: PathBuf,
    /// The state's name (letters, digits, _ and -).
    pub name: String,
    /// What the state shows, in game terms.
    #[arg(long)]
    pub description: String,
    #[command(flatten)]
    #[serde(flatten)]
    pub dirs: Dirs,
    /// Replace a state of this name.
    #[arg(long)]
    #[serde(default)]
    pub replace: bool,
}

#[derive(clap::Args, Clone, Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct StatesListArgs {
    #[command(flatten)]
    #[serde(flatten)]
    pub dirs: Dirs,
}

#[derive(clap::Args, Clone, Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct StatesRebuildArgs {
    /// States to rebuild; default every chain.
    #[serde(default)]
    pub names: Vec<String>,
    /// Field map names to compare, old and new; repeatable.
    #[arg(long = "sample")]
    #[serde(default, rename = "sample")]
    pub samples: Vec<String>,
    /// Field map for sample.
    #[arg(long)]
    #[serde(default)]
    pub map: Option<PathBuf>,
    /// Put rebuilt states in place of those that differ (old ones kept in .psx/old/).
    #[arg(long)]
    #[serde(default)]
    pub replace: bool,
    #[command(flatten)]
    #[serde(flatten)]
    pub dirs: Dirs,
}

#[derive(clap::Args, Clone, Debug, Default, serde::Deserialize, schemars::JsonSchema)]
pub struct Dirs {
    /// States directory (default .psx/states).
    #[arg(long)]
    #[serde(default)]
    pub states: Option<PathBuf>,
    /// Chains directory (default notes/states).
    #[arg(long)]
    #[serde(default)]
    pub chains: Option<PathBuf>,
}

impl Dirs {
    fn get(&self, ctx: &Ctx) -> (PathBuf, PathBuf) {
        let at = |d: &Option<PathBuf>, default: &str| match d {
            Some(d) if d.is_absolute() => d.clone(),
            Some(d) => ctx.root.join(d),
            None => ctx.root.join(default),
        };
        (at(&self.states, ".psx/states"), at(&self.chains, "notes/states"))
    }
}

fn sha256(bytes: &[u8]) -> String {
    psx_media::disc::sha256_bytes(bytes)
}

fn read(p: &Path) -> crate::Result<Vec<u8>> {
    std::fs::read(p).map_err(|e| Error::io(p, e))
}

fn write(p: &Path, bytes: &[u8]) -> crate::Result<()> {
    if let Some(d) = p.parent() {
        std::fs::create_dir_all(d).map_err(|e| Error::io(d, e))?;
    }
    crate::write_atomic(p, bytes).map_err(|e| Error::io(p, e))
}

fn read_json(p: &Path) -> crate::Result<Value> {
    serde_json::from_slice(&read(p)?).map_err(|e| Error::malformed(format!("{}: {e}", p.display())))
}

fn pretty(v: &Value) -> String {
    serde_json::to_string_pretty(v).unwrap_or_default() + "\n"
}

fn check_name(name: &str) -> crate::Result<()> {
    if name.is_empty() || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-') {
        return Err(Error::invalid(format!("state name {name:?}: letters, digits, _ and - only")));
    }
    Ok(())
}

/// A path as the chain notes it: relative to the project root when inside it.
/// A path as a chain keeps it: relative to the root inside the project, so
/// the project can move (a path from an older report is found first).
fn noted(ctx: &Ctx, p: &Path) -> String {
    crate::moved::store(&ctx.root, &crate::moved::find(&ctx.root, p)).to_string_lossy().into_owned()
}

fn absolute(ctx: &Ctx, p: &Path) -> PathBuf {
    if p.is_absolute() { p.to_owned() } else { ctx.root.join(p) }
}

/// The build a report says made its run (`version+id`), or its core version.
fn made_with(report: &Value) -> String {
    match (report["build"]["version"].as_str(), report["build"]["id"].as_str()) {
        (Some(v), Some(id)) => format!("{v}+{id}"),
        _ => report["identity"]["backend_version"].as_str().unwrap_or("unknown").to_string(),
    }
}

/// Resolved input events (complete pad states) back in run's input format.
fn events(resolved: &Value) -> Value {
    let list = resolved.as_array().cloned().unwrap_or_default();
    json!(
        list.into_iter()
            .map(|mut e| {
                if e["sticks"] == json!([128, 128, 128, 128]) {
                    e.as_object_mut().map(|o| o.remove("sticks"));
                }
                e
            })
            .collect::<Vec<_>>()
    )
}

/// A chain: the steps from power-on to a state, and what they ran on.
struct Chain {
    steps: Vec<Value>,
    media: Value,
    media_identity: Value,
    bios_sha256: Value,
}

/// A state a run started from, and the sha256 its report notes for it.
type Parent = Option<(PathBuf, Option<String>)>;

/// The step that made `file` (a state a run or handoff wrote), and the state
/// it started from.
fn step_of(ctx: &Ctx, file: &Path) -> crate::Result<(Value, Value, Parent)> {
    let dir = file.parent().unwrap_or(Path::new("."));
    let sha = sha256(&read(file)?);
    // A handoff mark: marks/N/ inside the handoff's directory.
    if dir.join("mark.json").is_file() {
        let mark = read_json(&dir.join("mark.json"))?;
        let handoff = dir.parent().and_then(Path::parent).unwrap_or(Path::new("."));
        let report = read_json(&handoff.join("report.json"))?;
        let input = read_json(&dir.join("input.json"))?;
        let mut run = report["reproduce"]["arguments"].clone();
        run["frames"] = mark["run_frame"].clone();
        run["input"] = input;
        return finish_step(ctx, &report, run, json!("end"), sha);
    }
    let report_path = dir.join("report.json");
    if !report_path.is_file() {
        return Err(Error::new(
            "no_chain",
            format!(
                "{}: no report.json beside it and no chain notes its sha256; make start states with `psx states make`, or name a run's state with `psx states save`",
                file.display()
            ),
        ));
    }
    let report = read_json(&report_path)?;
    if !report["reproduce"].is_null() {
        // A handoff: the person's input, replayed.
        let mut run = report["reproduce"]["arguments"].clone();
        let replay = run["replay"].as_str().map(PathBuf::from).unwrap_or_else(|| dir.join("input.json"));
        run["input"] = read_json(&replay)?;
        return finish_step(ctx, &report, run, json!("end"), sha);
    }
    let name = file.file_name().and_then(|n| n.to_str()).unwrap_or("");
    let state = match name.strip_prefix("state_f").and_then(|n| n.strip_suffix(".bin")) {
        Some(n) => json!(n.parse::<u64>().map_err(|_| Error::invalid(format!("{name}: not a checkpoint name")))?),
        None => json!("end"),
    };
    let mut run = json!({"frames": report["frames"]["requested"], "input": events(&report["input"]["events"])});
    if let Some(n) = state.as_u64() {
        run["save_state_at"] = json!([n]);
    }
    let pokes: Vec<Value> = report["pokes"].as_array().into_iter().flatten().map(|p| p["spec"].clone()).collect();
    if !pokes.is_empty() {
        run["poke"] = json!(pokes);
    }
    if let Some(conds) = report["until"]["conditions"].as_array() {
        run["until"] = json!(conds);
        run["map"] = report["until"]["map"].clone();
    }
    let id = &report["identity"];
    if id["from_state"].is_null() {
        match id["cards"].as_object() {
            Some(cards) => {
                for (k, v) in cards.iter().filter(|(_, v)| !v.is_null()) {
                    run[k] = v.clone();
                }
            }
            // Older reports: the pad's kind, and new cards in the slots that had one.
            None => {
                run["pad"] = json!(report["pad"]["kind"].as_str().unwrap_or("digital"));
                for card in report["memcards"].as_array().into_iter().flatten() {
                    run[format!("memcard{}", card["slot"])] = json!("new");
                }
            }
        }
    }
    finish_step(ctx, &report, run, state, sha)
}

fn finish_step(
    ctx: &Ctx,
    report: &Value,
    mut run: Value,
    state: Value,
    sha: String,
) -> crate::Result<(Value, Value, Parent)> {
    let id = &report["identity"];
    // The report may be from before the project moved.
    let parent = id["from_state"].as_str().map(|p| crate::moved::find(&ctx.root, Path::new(p)));
    if let Some(m) = run["map"].as_str().map(|m| noted(ctx, Path::new(m))) {
        run["map"] = json!(m);
    }
    if let Some(o) = run.as_object_mut() {
        for k in ["media", "from_state", "replay", "save_state", "out"] {
            o.remove(k);
        }
        if parent.is_some() {
            for k in ["pad", "memcard1", "memcard2"] {
                o.remove(k);
            }
        }
    }
    let step = json!({"run": run, "state": state, "state_sha256": sha, "made_with": made_with(report),
        "emulator": report["build"]["emulator"]});
    let media = json!({"media": id["media"], "media_identity": id["media_identity"], "bios_sha256": id["bios_sha256"]});
    let noted_sha = id["from_state_sha256"].as_str().map(str::to_string);
    Ok((step, media, parent.map(|p| (p, noted_sha))))
}

/// The chains in `chains`, by name.
fn chains(chains: &Path) -> Vec<(String, Value)> {
    let mut out = Vec::new();
    for e in std::fs::read_dir(chains).into_iter().flatten().flatten() {
        let p = e.path();
        if p.extension().is_some_and(|x| x == "json") {
            if let (Some(name), Ok(v)) = (p.file_stem().and_then(|s| s.to_str()), read_json(&p)) {
                out.push((name.to_string(), v));
            }
        }
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

/// The chains a state's sha256 is looked up in: the named states', and
/// those of the states they replaced (notes/states/replaced/), so a run from
/// a state since replaced keeps its chain.
fn known_chains(chain_dir: &Path) -> Vec<(String, Value)> {
    let mut all = chains(chain_dir);
    all.extend(chains(&chain_dir.join("replaced")));
    all
}

/// Keeps the chain of a named state about to be replaced, by its sha256.
fn keep_replaced(chain_dir: &Path, name: &str, old: &Value) -> crate::Result<()> {
    let Some(sha) = old["state_sha256"].as_str() else { return Ok(()) };
    let kept = chain_dir.join("replaced").join(format!("{name}-{}.json", &sha[..sha.len().min(12)]));
    if kept.is_file() {
        return Ok(());
    }
    let mut old = old.clone();
    old["replaced"] = json!(crate::publish::utc_now());
    write(&kept, pretty(&old).as_bytes())
}

/// The chain from power-on to `file`: from the runs' reports, back to power-on
/// or to a state a chain names (by sha256).
fn chain_of(ctx: &Ctx, file: &Path, known: &[(String, Value)]) -> crate::Result<Chain> {
    let mut steps = Vec::new();
    let mut media = Value::Null;
    let mut at = Some(file.to_path_buf());
    let mut seen = 0;
    while let Some(f) = at.take() {
        seen += 1;
        if seen > 10_000 {
            return Err(Error::new("chain_broken", "the chain loops"));
        }
        let sha = sha256(&read(&f)?);
        let beside = f.parent().is_some_and(|d| d.join("report.json").is_file() || d.join("mark.json").is_file());
        if let Some((_, c)) = known.iter().find(|(_, c)| c["state_sha256"] == sha).filter(|_| !beside) {
            let mut before: Vec<Value> = c["steps"].as_array().cloned().unwrap_or_default();
            before.append(&mut steps);
            steps = before;
            if media.is_null() {
                media = json!({"media": c["media"], "media_identity": c["media_identity"], "bios_sha256": c["bios_sha256"]});
            }
            break;
        }
        let (step, m, parent) = step_of(ctx, &f)?;
        steps.insert(0, step);
        if media.is_null() {
            media = m;
        }
        let Some((p, noted_sha)) = parent else { break };
        match noted_sha {
            // The state it started from has changed since (replaced): its chain
            // by the sha256 the run noted, or none.
            Some(n) if sha256(&read(&p)?) != n => {
                let Some((_, c)) = known.iter().find(|(_, c)| c["state_sha256"] == n.as_str()) else {
                    return Err(Error::new(
                        "chain_broken",
                        format!("{} changed since the run that started from it, and no chain names the state it was", p.display()),
                    ));
                };
                let mut before: Vec<Value> = c["steps"].as_array().cloned().unwrap_or_default();
                before.append(&mut steps);
                steps = before;
                break;
            }
            _ => at = Some(p),
        }
    }
    Ok(Chain {
        steps,
        media: media["media"].as_str().map_or(Value::Null, |p| json!(noted(ctx, Path::new(p)))),
        media_identity: media["media_identity"].clone(),
        bios_sha256: media["bios_sha256"].clone(),
    })
}

/// Name `file`: copy it into the states directory and write its chain.
fn save(ctx: &Ctx, file: &Path, name: &str, description: &str, dirs: &Dirs, replace: bool) -> crate::Result<Value> {
    check_name(name)?;
    let (states, chain_dir) = dirs.get(ctx);
    let (state_path, chain_path) = (states.join(format!("{name}.bin")), chain_dir.join(format!("{name}.json")));
    let bytes = read(file)?;
    let sha = sha256(&bytes);
    if !replace && chain_path.exists() && read_json(&chain_path)?["state_sha256"] != sha {
        return Err(Error::invalid(format!("a state named {name} exists; pass replace to replace it")));
    }
    // The state being replaced may be the one this one started from.
    let known = known_chains(&chain_dir);
    let c = chain_of(ctx, file, &known)?;
    let chain = json!({
        "name": name, "description": description,
        "state": noted(ctx, &state_path), "state_sha256": sha,
        "media": c.media, "media_identity": c.media_identity, "bios_sha256": c.bios_sha256,
        "steps": c.steps,
        "how": "psx states rebuild replays the steps from power-on, each continuing from the state the one before ended in",
    });
    if let Some(old) = known.iter().find(|(n, c)| n == name && c["state_sha256"] != sha.as_str()) {
        keep_replaced(&chain_dir, name, &old.1)?;
    }
    write(&state_path, &bytes)?;
    write(&chain_path, pretty(&chain).as_bytes())?;
    Ok(json!({"name": name, "state": state_path, "chain": chain_path, "state_sha256": sha,
        "steps": chain["steps"].as_array().map_or(0, Vec::len)}))
}

/// Run one step from `parent` (or power-on) into `out`; returns the state it made.
fn run_step(
    ctx: &Ctx,
    media: &Value,
    step: &Value,
    parent: Option<&Path>,
    out: &Path,
) -> crate::Result<(PathBuf, Value)> {
    let mut run = step["run"].clone();
    if let Some(m) = media.as_str() {
        run["media"] = json!(crate::moved::find(&ctx.root, Path::new(m)));
    }
    if let Some(m) = run["map"].as_str() {
        run["map"] = json!(crate::moved::find(&ctx.root, Path::new(m)));
    }
    run["from_state"] = json!(parent);
    run["out"] = json!(out);
    run["summary"] = json!(true);
    run["timeout"] = json!(7200);
    let state = match step["state"].as_u64() {
        Some(n) => out.join(format!("state_f{n:06}.bin")),
        None => {
            run["save_state"] = json!(true);
            out.join("state.bin")
        }
    };
    let input = run["input"].as_array().map_or(0, Vec::len);
    if input > crate::replay::INLINE_EVENT_LIMIT {
        let p = out.join("input.json");
        write(&p, run["input"].to_string().as_bytes())?;
        run["replay"] = json!(p);
        run.as_object_mut().map(|o| o.remove("input"));
    }
    let args: crate::run::RunCmdArgs =
        serde_json::from_value(run).map_err(|e| Error::malformed(format!("a chain step's run: {e}")))?;
    let (complete, report) = args.run(ctx)?;
    if !complete || !state.is_file() {
        return Err(Error::new("run_incomplete", format!("the step's run ended early ({})", report["termination"])));
    }
    Ok((state, report))
}

impl Op for StatesMakeArgs {
    fn exec(self, ctx: &Ctx) -> Outcome {
        Outcome::result(make(ctx, self))
    }
}

fn make(ctx: &Ctx, a: StatesMakeArgs) -> crate::Result<Value> {
    check_name(&a.name)?;
    let (states, chain_dir) = a.dirs.get(ctx);
    if !a.replace && chain_dir.join(format!("{}.json", a.name)).exists() {
        return Err(Error::invalid(format!("a state named {} exists; pass replace to replace it", a.name)));
    }
    let from = a.from.as_deref().map(|f| {
        let named = states.join(format!("{f}.bin"));
        if !f.contains(['/', '\\']) && !f.ends_with(".bin") { named } else { absolute(ctx, Path::new(f)) }
    });
    if from.is_some() && (a.pad.is_some() || a.memcard1.is_some() || a.memcard2.is_some()) {
        return Err(Error::invalid("the pad and memory cards are part of the state: give them only from power-on"));
    }
    let input = match (&a.input, &a.replay) {
        (Some(e), _) => json!(e),
        (None, Some(p)) => read_json(&absolute(ctx, p))?,
        (None, None) => json!([]),
    };
    let mut run = json!({"media": a.media, "from_state": from, "frames": a.frames, "input": input,
        "until": a.until, "map": a.map, "poke": a.pokes, "save_state": true, "summary": true, "timeout": 7200});
    for (k, v) in [("pad", &a.pad), ("memcard1", &a.memcard1), ("memcard2", &a.memcard2)] {
        if let Some(v) = v {
            run[k] = json!(v);
        }
    }
    let args: crate::run::RunCmdArgs =
        serde_json::from_value(run).map_err(|e| Error::invalid(format!("states make: {e}")))?;
    let (complete, report) = args.run(ctx)?;
    if !complete {
        return Err(Error::new("run_incomplete", format!("the run ended early ({})", report["termination"])));
    }
    let dir = PathBuf::from(report["run_dir"].as_str().unwrap_or_default());
    let mut out = save(ctx, &dir.join("state.bin"), &a.name, &a.description, &a.dirs, a.replace)?;
    out["run_dir"] = json!(dir);
    out["frames"] = report["frames"].clone();
    if !report["until"].is_null() {
        out["until"] = report["until"].clone();
        if report["until"]["met"] == false {
            out["warnings"] = json!(["the until conditions never held: the state is where the frames ran out"]);
        }
    }
    out["screen"] = json!(dir.join("screen.png"));
    Ok(out)
}

impl Op for StatesSaveArgs {
    fn exec(self, ctx: &Ctx) -> Outcome {
        let source = absolute(ctx, &self.source);
        let file = if source.is_dir() { source.join("state.bin") } else { source };
        Outcome::result(save(ctx, &file, &self.name, &self.description, &self.dirs, self.replace))
    }
}

impl Op for StatesListArgs {
    fn exec(self, ctx: &Ctx) -> Outcome {
        let (states, chain_dir) = self.dirs.get(ctx);
        let list: Vec<Value> = chains(&chain_dir)
            .into_iter()
            .map(|(name, c)| {
                let p = states.join(format!("{name}.bin"));
                let have = std::fs::read(&p).ok().map(|b| sha256(&b));
                let state = match have {
                    None => "missing: psx states rebuild makes it again",
                    Some(s) if c["state_sha256"] == s => "present",
                    Some(_) => "differs from the chain's sha256",
                };
                let made_with: std::collections::BTreeSet<&str> =
                    c["steps"].as_array().into_iter().flatten().filter_map(|s| s["made_with"].as_str()).collect();
                json!({"name": name, "description": c["description"], "state": state,
                    "steps": c["steps"].as_array().map_or(0, Vec::len), "made_with": made_with})
            })
            .collect();
        Outcome::result(Ok::<_, Error>(json!({"states": list, "chains": chain_dir, "dir": states})))
    }
}

impl Op for StatesRebuildArgs {
    fn exec(self, ctx: &Ctx) -> Outcome {
        Outcome::report(rebuild(ctx, self))
    }
}

/// The named values and how much of main RAM differs between two states.
fn compare(ctx: &Ctx, old: &[u8], new: &[u8], bios: &Path, cols: &[crate::ram::Sample]) -> Value {
    let Ok(bios) = std::fs::read(absolute(ctx, bios)) else { return json!({"error": "the BIOS is unreadable"}) };
    let (_, n) = match psx_core::Machine::load_state(new, bios.clone()) {
        Ok(m) => m,
        Err(e) => return json!({"error": format!("the new state: {e}")}),
    };
    let o = match psx_core::Machine::load_state(old, bios) {
        Ok((_, m)) => m,
        Err(e) => {
            return json!({"old_unreadable": format!("{e}"), "note": "an older psx made it: compare by cases (psx case check)"});
        }
    };
    let value = |m: &psx_core::Machine, c: &crate::ram::Sample| c.json(&m.bus.ram, &m.bus.scratch);
    let values: serde_json::Map<String, Value> =
        cols.iter().map(|c| (c.name.clone(), json!({"old": value(&o, c), "new": value(&n, c)}))).collect();
    let differ: Vec<usize> = (0..o.bus.ram.len()).filter(|&i| o.bus.ram[i] != n.bus.ram[i]).collect();
    json!({"values": values, "ram_bytes_differing": differ.len(),
        "first_differing": differ.first().map(|i| format!("0x{:08X}", 0x8000_0000 + i))})
}

fn rebuild(ctx: &Ctx, a: StatesRebuildArgs) -> crate::Result<(bool, Value)> {
    let (states, chain_dir) = a.dirs.get(ctx);
    let all = chains(&chain_dir);
    if all.is_empty() {
        return Err(Error::invalid(format!("no chains in {}", chain_dir.display())));
    }
    for n in &a.names {
        if !all.iter().any(|(name, _)| name == n) {
            return Err(Error::invalid(format!("no chain named {n} in {}", chain_dir.display())));
        }
    }
    let map = crate::fieldmap::FieldMap::load(a.map.as_deref()).map_err(Error::invalid)?;
    let mut cols = Vec::new();
    for s in &a.samples {
        cols.extend(crate::fieldmap::select(s, map.as_ref()).map_err(|e| Error::invalid(format!("--sample {e}")))?);
    }
    let version = crate::context::version();
    let work = ctx.root.join(".psx/rebuild").join(&version);
    let mut results = Vec::new();
    let mut all_same = true;
    for (name, mut chain) in all.into_iter().filter(|(n, _)| a.names.is_empty() || a.names.contains(n)) {
        let steps = chain["steps"].as_array().cloned().unwrap_or_default();
        // A chain from an older psx may keep its disc absolute: written back relative.
        let kept_media = chain["media"].clone();
        if let Some(m) = kept_media.as_str() {
            chain["media"] = json!(noted(ctx, Path::new(m)));
        }
        let (mut parent, mut key, mut ran, mut reused, mut first_differing) =
            (None::<PathBuf>, String::new(), 0, 0, None);
        let mut bios = None;
        let mut new_shas = Vec::new();
        for (i, step) in steps.iter().enumerate() {
            // Steps shared by chains (or by an earlier rebuild on this psx) run once.
            key = sha256(format!("{key}\n{}\n{}\n{}", chain["media_identity"], step["run"], step["state"]).as_bytes());
            let out = work.join(&key[..16]);
            let file = match step["state"].as_u64() {
                Some(n) => out.join(format!("state_f{n:06}.bin")),
                None => out.join("state.bin"),
            };
            if file.is_file() && out.join("report.json").is_file() {
                reused += 1;
            } else {
                let _ = std::fs::remove_dir_all(&out);
                run_step(ctx, &chain["media"], step, parent.as_deref(), &out)
                    .map_err(|e| e.with("at", json!({"state": &name, "step": i})))?;
                ran += 1;
            }
            let report = read_json(&out.join("report.json"))?;
            if bios.is_none() {
                bios = report["identity"]["bios"].as_str().map(PathBuf::from);
                if chain["bios_sha256"] != report["identity"]["bios_sha256"] {
                    return Err(Error::new(
                        "identity_mismatch",
                        format!(
                            "{name}: the chain was made with BIOS {}, this run used {}",
                            chain["bios_sha256"], report["identity"]["bios_sha256"]
                        ),
                    ));
                }
            }
            let sha = sha256(&read(&file)?);
            if first_differing.is_none() && step["state_sha256"] != sha {
                first_differing = Some(i);
            }
            new_shas.push((sha, made_with(&report), report["build"]["emulator"].clone()));
            parent = Some(file);
        }
        let Some(new_file) = parent else { continue };
        let new = read(&new_file)?;
        let state_path = states.join(format!("{name}.bin"));
        let old = std::fs::read(&state_path).ok();
        let new_sha = sha256(&new);
        let mut r = json!({"name": name, "steps": steps.len(), "ran": ran, "reused": reused,
            "first_differing_step": first_differing, "new_sha256": new_sha, "rebuilt": new_file});
        let status = match &old {
            None => "missing",
            Some(o) if *o == new => "same",
            Some(_) => "differs",
        };
        if let Some(o) = &old {
            r["old_sha256"] = json!(sha256(o));
            if status == "differs" {
                r["compare"] = compare(ctx, o, &new, bios.as_deref().unwrap_or(Path::new("")), &cols);
            }
        }
        // The rebuild made what the chain notes: a state file that differs was changed by hand.
        let as_noted = first_differing.is_none() && chain["state_sha256"] == new_sha;
        r["matches_chain"] = json!(as_noted);
        let differs = status == "differs" || !as_noted;
        all_same &= !differs;
        r["status"] = json!(status);
        if status == "differs" && as_noted {
            r["note"] = json!(
                "the rebuild matches the chain, so the state file was changed since it was made: replace puts the chain's state back"
            );
        }
        if status == "missing" || (a.replace && differs) {
            if let Some(o) = &old {
                // Kept by the psx that made it.
                let by = steps.last().and_then(|s| s["made_with"].as_str()).unwrap_or("unknown").to_string();
                let kept = ctx.root.join(".psx/old").join(by).join(format!("states/{name}.bin"));
                write(&kept, o)?;
                r["old_kept"] = json!(kept);
            }
            if chain["state_sha256"] != new_sha.as_str() {
                keep_replaced(&chain_dir, &name, &chain)?;
            }
            write(&state_path, &new)?;
            for (step, (sha, by, emulator)) in chain["steps"].as_array_mut().into_iter().flatten().zip(&new_shas) {
                step["state_sha256"] = json!(sha);
                step["made_with"] = json!(by);
                step["emulator"] = emulator.clone();
            }
            chain["state_sha256"] = json!(new_sha);
            write(&chain_dir.join(format!("{name}.json")), pretty(&chain).as_bytes())?;
            r["status"] = json!(if status == "missing" { "restored" } else { "replaced" });
        } else if !differs {
            // Rebuilt the same on this emulator: the chain holds on it.
            let mut changed = false;
            for (step, (_, _, emulator)) in chain["steps"].as_array_mut().into_iter().flatten().zip(&new_shas) {
                changed |= step["emulator"] != *emulator;
                step["emulator"] = emulator.clone();
            }
            if changed || chain["media"] != kept_media {
                write(&chain_dir.join(format!("{name}.json")), pretty(&chain).as_bytes())?;
            }
        }
        results.push(r);
    }
    let mut out = json!({"status": if all_same { "same" } else { "differs" }, "states": results, "work": work});
    if !all_same {
        out["next"] = json!(if a.replace {
            "psx case check: the cases that differ now are the triage list (an instrument change, or a real divergence)"
        } else {
            "compare names the values that moved; pass replace to put the rebuilt states in place, then psx case check"
        });
    }
    Ok((all_same, out))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn events_drop_centred_sticks() {
        let e = events(&json!([{"frame": 0, "buttons": ["UP"], "sticks": [128, 128, 128, 128]},
            {"frame": 4, "buttons": [], "sticks": [0, 128, 128, 128]}]));
        assert_eq!(
            e,
            json!([{"frame": 0, "buttons": ["UP"]}, {"frame": 4, "buttons": [], "sticks": [0, 128, 128, 128]}])
        );
        let parsed: crate::replay::Events = serde_json::from_value(e).unwrap();
        assert_eq!(parsed.0.len(), 2);
    }

    /// A run's directory as run writes it: its state and its report.
    fn fake_run(dir: &Path, state: &[u8], from: Option<&Path>) {
        std::fs::create_dir_all(dir).unwrap();
        std::fs::write(dir.join("state.bin"), state).unwrap();
        let from_sha = from.map(|f| sha256(&std::fs::read(f).unwrap()));
        let report = json!({"frames": {"requested": 10}, "input": {"events": []},
            "build": {"version": "0", "id": "t"},
            "identity": {"from_state": from, "from_state_sha256": from_sha, "cards": {"pad": "digital"},
                "media": "game.chd", "media_identity": "g", "bios_sha256": "b"}});
        std::fs::write(dir.join("report.json"), report.to_string()).unwrap();
    }

    #[test]
    fn a_run_from_a_replaced_state_keeps_its_chain() {
        let root = std::env::temp_dir().join(format!("psx-states-replaced-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let ctx = Ctx { root: root.clone(), cancel: None };
        let dirs = Dirs { states: None, chains: None };
        let named = root.join(".psx/states/title.bin");
        // title from power-on; a run from it (later), and one further on.
        fake_run(&root.join("r1"), b"boot", None);
        save(&ctx, &root.join("r1/state.bin"), "title", "black boot", &dirs, false).unwrap();
        fake_run(&root.join("r2"), b"menu", Some(&named));
        fake_run(&root.join("r3"), b"map", Some(&named));
        // Replacing the ancestor with its descendant.
        let out = save(&ctx, &root.join("r2/state.bin"), "title", "the title menu", &dirs, true).unwrap();
        assert_eq!(out["steps"], 2);
        assert!(root.join(format!("notes/states/replaced/title-{}.json", &sha256(b"boot")[..12])).is_file());
        // A run from the state title was before: named, with the chain it had.
        let out = save(&ctx, &root.join("r3/state.bin"), "map", "the world map", &dirs, false).unwrap();
        assert_eq!(out["steps"], 2);
        // The replaced chain is not a named state.
        let names: Vec<String> = chains(&root.join("notes/states")).into_iter().map(|c| c.0).collect();
        assert_eq!(names, ["map", "title"]);
        // A state no chain names is still refused.
        std::fs::write(&named, b"edited").unwrap();
        fake_run(&root.join("r4"), b"x", Some(&named));
        std::fs::write(&named, b"edited again").unwrap();
        let e = save(&ctx, &root.join("r4/state.bin"), "x", "x", &dirs, false).unwrap_err();
        assert!(e.to_string().contains("no chain names"), "{e}");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn names_are_checked() {
        assert!(check_name("open_ground-2").is_ok());
        assert!(check_name("../x").is_err());
        assert!(check_name("").is_err());
    }
}
