//! Scenarios (docs/scenarios.md): the glossary (`glossary/`, its quantities,
//! and `glossary/binding/`, where the PlayStation keeps them and what ends
//! a tick), `psx scenario record` (the original's quantities tick by tick,
//! from the scenario's state), `psx scenario check` (the determinism check)
//! and the facts `port next` reads.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use psx_core::Machine;
use psx_scenario::Release;
use psx_scenario::scenario::{self as format, Full, Header, Producer, Recording, Scenario, Source, SourceKind};
use psx_scenario::{Glossary, Type, Values};
use serde_json::{Value as Json, json};

use crate::Error;
use crate::binding::{self, Plugin, Tick};
use crate::commands::{Ctx, Op, Outcome};
use crate::port::Entry;

/// The glossary crate, `glossary/`; its binding is `glossary/binding/`.
pub const GLOSSARY: &str = "glossary";
const SCENARIOS: &str = "scenarios";
/// Each scenario's last determinism check: `<id>.json`.
const CHECKS: &str = ".psx/scenarios";
const MAX_REFUSALS: usize = 20;
const MAX_DIFFERENCES: usize = 10;

fn rel(root: &Path, p: &Path) -> String {
    p.strip_prefix(root).unwrap_or(p).display().to_string()
}

fn check_name(what: &str, name: &str) -> crate::Result<()> {
    if name.is_empty() || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-') {
        return Err(Error::invalid(format!("{what} {name:?}: letters, digits, _ and - only")));
    }
    Ok(())
}

/// The glossary, or what is wrong with it.
pub fn glossary(root: &Path) -> Result<Glossary, Vec<String>> {
    let mut problems = Vec::new();
    if root.join("glossary.toml").is_file() {
        problems.push(
            "glossary.toml: no longer read: the glossary is Rust, glossary/ and glossary/binding/ (docs/scenarios.md)"
                .to_string(),
        );
    }
    let dir = root.join(GLOSSARY);
    if !binding::is_crate(&dir) {
        problems.push(format!("{GLOSSARY}/: not written yet (a crate whose src/lib.rs declares Quantities)"));
        return Err(problems);
    }
    let at = |e: String| format!("{GLOSSARY}/{e}");
    let read = || -> crate::Result<(String, Vec<(String, String)>)> {
        let p = dir.join("Cargo.toml");
        Ok((std::fs::read_to_string(&p).map_err(|e| Error::io(&p, e))?, binding::sources(&dir)?))
    };
    let (cargo_toml, files) = read().map_err(|e| vec![e.to_string()])?;
    let g = match psx_scenario::rust::glossary(&psx_scenario::rust::Crate { cargo_toml: &cargo_toml, files: &files }) {
        Ok((g, p)) => {
            problems.extend(p.into_iter().map(at));
            g
        }
        Err(p) => {
            problems.extend(p.into_iter().map(at));
            return Err(problems);
        }
    };
    if psx_scenario::rust::package_name(&cargo_toml).as_deref() != Some(GLOSSARY) {
        problems.push(at("Cargo.toml: the package is named \"glossary\"".into()));
    }
    let binding = dir.join("binding/Cargo.toml");
    match std::fs::read_to_string(&binding) {
        Err(_) => problems.push(format!("{GLOSSARY}/binding: not written yet")),
        Ok(t) if psx_scenario::rust::package_name(&t).as_deref() != Some("glossary-binding") => {
            problems.push(at("binding/Cargo.toml: the package is named \"glossary-binding\"".into()))
        }
        Ok(_) => {}
    }
    if problems.is_empty() { Ok(g) } else { Err(problems) }
}

/// The glossary binding's answer to `DESCRIBE`.
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Described {
    /// The fingerprint of the glossary its glue was generated from.
    glossary: String,
    source: Release,
    tick: Tick,
}

/// The glossary's binding, built, loaded and checked against `g`.
fn binding(root: &Path, g: &Glossary) -> crate::Result<Plugin<Described>> {
    let p: Plugin<Described> =
        Plugin::load(&binding::build(root, &root.join(GLOSSARY).join("binding"), "glossary-binding")?)?;
    let d = p.described();
    let mut problems = Vec::new();
    if d.glossary != g.fingerprint() {
        problems.push("its glue reads other quantities than glossary/ declares: psx port update brings vendor/psx-scenario to this psx's".to_string());
    }
    d.source.check(binding::PLATFORM, &mut problems);
    d.tick.check(&mut problems);
    if !problems.is_empty() {
        return Err(Error::invalid("the glossary's binding has problems").with("problems", json!(problems)));
    }
    Ok(p)
}

/// The scenarios on disk, by id.
pub fn ids(root: &Path) -> Vec<String> {
    let mut out: Vec<String> = std::fs::read_dir(root.join(SCENARIOS))
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| e.file_name().to_str()?.strip_suffix(".toml").map(String::from))
        .collect();
    out.sort();
    out
}

/// A scenario file, its glossary and its recordings, with what is wrong
/// with them.
pub struct Loaded {
    pub id: String,
    pub scenario: Option<Scenario>,
    pub glossary: Option<Glossary>,
    /// Name, file, and the recording or why it does not parse.
    pub recordings: Vec<(String, PathBuf, Result<Recording, String>)>,
    pub problems: Vec<String>,
}

impl Loaded {
    pub fn load(root: &Path, id: &str) -> Loaded {
        let file = format!("{SCENARIOS}/{id}.toml");
        let mut l =
            Loaded { id: id.into(), scenario: None, glossary: None, recordings: Vec::new(), problems: Vec::new() };
        let text = match std::fs::read_to_string(root.join(&file)) {
            Ok(t) => t,
            Err(_) => {
                l.problems.push(format!("{file}: not written yet"));
                return l;
            }
        };
        match glossary(root) {
            Ok(g) => l.glossary = Some(g),
            Err(p) => l.problems.extend(p),
        }
        match Scenario::parse(&text) {
            Ok(s) => {
                let types = l.glossary.as_ref().map(|g| g.types()).unwrap_or_default();
                let mut p = if l.glossary.is_some() {
                    s.check(&types)
                } else {
                    s.check(&types).into_iter().filter(|p| !p.ends_with("not a glossary quantity")).collect()
                };
                if !root.join(&s.chain).is_file() {
                    p.push(format!("chain: no file {} (psx run --save-as writes notes/states/NAME.json)", s.chain));
                }
                if !crate::port::is_feature_id(&s.feature) {
                    p.push(format!("feature {:?}: a catalogue id, F-NNNN", s.feature));
                }
                l.problems.extend(p.into_iter().map(|p| format!("{file}: {p}")));
                l.scenario = Some(s);
            }
            Err(e) => l.problems.push(format!("{file}: {e}")),
        }
        let dir = root.join(SCENARIOS).join(id);
        let mut files: Vec<PathBuf> = std::fs::read_dir(&dir)
            .into_iter()
            .flatten()
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|x| x == "jsonl"))
            .collect();
        files.sort();
        for f in files {
            let name = f.file_stem().and_then(|s| s.to_str()).unwrap_or_default().to_string();
            let r = std::fs::read_to_string(&f).map_err(|e| e.to_string()).and_then(|t| Recording::parse(&t));
            l.recordings.push((name, f, r));
        }
        l
    }

    /// The compared quantities' glossary declarations.
    fn compared(&self) -> BTreeMap<String, psx_scenario::Field> {
        let (Some(s), Some(g)) = (&self.scenario, &self.glossary) else { return BTreeMap::new() };
        s.compare.keys().filter_map(|n| Some((n.clone(), g.quantities.get(n)?.clone()))).collect()
    }

    /// What is wrong with a recording as this scenario and glossary stand:
    /// its own problems, and a header recorded for other quantities.
    fn recording_problems(&self, root: &Path, file: &Path, r: &Result<Recording, String>) -> Vec<String> {
        let at = rel(root, file);
        let r = match r {
            Ok(r) => r,
            Err(e) => return vec![format!("{at}: {e}")],
        };
        let mut p: Vec<String> = r.check(&self.id).into_iter().map(|p| format!("{at}: {p}")).collect();
        // Only the types count: a unit or description edited in the glossary
        // leaves the recorded values as they were.
        let typed = |f: &psx_scenario::Field| psx_scenario::Field { unit: None, description: None, ..f.clone() };
        let declared: BTreeMap<String, psx_scenario::Field> =
            self.compared().iter().map(|(n, f)| (n.clone(), typed(f))).collect();
        let recorded: BTreeMap<String, psx_scenario::Field> =
            r.header.quantities.iter().map(|(n, f)| (n.clone(), typed(f))).collect();
        if self.scenario.is_some() && self.glossary.is_some() && recorded != declared {
            p.push(format!(
                "{at}: recorded for other quantities than the scenario compares, or with other types than the glossary declares; record it again"
            ));
        }
        p
    }
}

/// Where a scenario's recording is written.
fn recording_path(root: &Path, id: &str, name: &str) -> PathBuf {
    root.join(SCENARIOS).join(id).join(format!("{name}.jsonl"))
}

/// The machine at a state, on the chain's medium, with its identity.
fn machine(ctx: &Ctx, chain: &Json, state: &Path, source: &Release) -> crate::Result<Machine> {
    let media = chain["media"].as_str().map(|m| crate::moved::find(&ctx.root, Path::new(m)));
    let resolved = crate::context::resolve(&ctx.root, media, None, Some(state), false)?;
    if let Some(e) = resolved.setup_error() {
        return Err(e);
    }
    let bios_path = resolved.bios.unwrap();
    let bios = std::fs::read(&bios_path).map_err(|e| Error::io(&bios_path, e))?;
    let medium = crate::run_native::open_media(resolved.media.as_deref())?;
    let Some(id) = medium.id.clone() else {
        return Err(Error::invalid("a scenario is recorded on the game's disc: the chain names no media"));
    };
    if id.sha256 != source.image_sha256 {
        return Err(Error::new(
            "identity_mismatch",
            format!(
                "the glossary's binding is for image {}, the chain's media is {} (its IMAGE_SHA256 is the media identity psx context reports)",
                source.image_sha256, id.sha256
            ),
        ));
    }
    crate::run_native::boot(Some(state), &Default::default(), bios, medium.exe.as_deref(), medium.disc, &medium.id)
}

/// The original's quantities at the end of every tick of a run.
struct Recorded {
    initial: Option<Values>,
    ticks: Vec<Full>,
    refusals: Vec<Json>,
    refusal_count: u64,
    timed_out: bool,
}

/// What the glossary's binding is asked, and which quantities are kept.
struct Quantities<'a> {
    binding: &'a Plugin<Described>,
    /// Every quantity's type, which the binding's answers are checked against.
    types: BTreeMap<String, Type>,
    /// The quantities the recording keeps: the scenario's compared ones.
    kept: Vec<String>,
}

/// The quantities at the end of every tick of a run of `frames` fields
/// after the first tick, with `events` as the pad's input.
fn record(
    m: &mut Machine,
    q: &Quantities,
    frames: u32,
    events: &[crate::replay::Input],
    deadline: Duration,
) -> Recorded {
    let started = Instant::now();
    // The pad is released until the first tick ends: `initial` is the moment
    // the port's launcher builds, before any input. The recording's fields
    // (and the input's frames) count from the field after that tick.
    crate::replay::apply(m, &crate::replay::Input { frame: 0, buttons: 0, sticks: [128; 4] });
    let mut start: Option<u64> = None;
    let mut target = u64::MAX;
    let tick = &q.binding.described().tick;
    let (lo, hi) = (crate::run::fold(tick.at), crate::run::fold(tick.at) + tick.bytes);
    let mut out =
        Recorded { initial: None, ticks: Vec::new(), refusals: Vec::new(), refusal_count: 0, timed_out: false };
    let (mut next_event, mut last_frame) = (0, m.bus.frames);
    let lead_limit = m.bus.frames + frames as u64;
    while m.bus.frames < target {
        if m.bus.frames != last_frame {
            last_frame = m.bus.frames;
            if out.initial.is_some() && start.is_none() {
                start = Some(m.bus.frames);
                target = m.bus.frames + frames as u64;
            }
            if let Some(start) = start {
                let field = m.bus.frames - start;
                while next_event < events.len() && events[next_event].frame <= field {
                    crate::replay::apply(m, &events[next_event]);
                    next_event += 1;
                }
            } else if m.bus.frames >= lead_limit {
                // No tick within the recording's length: unrecorded() says so.
                break;
            }
            if started.elapsed() > deadline {
                out.timed_out = true;
                break;
            }
        }
        let info = m.step();
        let Some(acc) = info.mem.filter(|a| a.store) else { continue };
        let a = crate::run::fold(acc.addr);
        if a >= hi || a + acc.width as u32 <= lo || tick.pc.is_some_and(|pc| pc != info.pc) {
            continue;
        }
        // A tick ended: its quantities, as the store left them.
        let n = out.ticks.len() as u64 + out.initial.is_some() as u64;
        let mut values = Values::new();
        let mut refuse = |quantity: &str, problem: String| {
            out.refusal_count += 1;
            if out.refusals.len() < MAX_REFUSALS {
                out.refusals.push(json!({"tick": n, "quantity": quantity, "problem": problem}));
            }
        };
        match q.binding.call(psx_capture::abi::QUANTITIES, m).map(|t| binding::values(&t, &q.types)) {
            Ok(Ok(all)) => {
                values = all.into_iter().filter(|(name, _)| q.kept.contains(name)).collect();
            }
            Ok(Err(problems)) => {
                for (name, e) in problems {
                    refuse(&name, e);
                }
            }
            Err(e) => refuse("", e),
        }
        if out.initial.is_none() {
            out.initial = Some(values);
        } else {
            // A second tick in the first tick's field belongs to the lead-in
            // field, which the recording counts as field 0.
            let field = start.map_or(0, |s| m.bus.frames - s);
            out.ticks.push(Full { tick: n, field, pad: crate::replay::button_names(m.bus.sio0.buttons), values });
        }
    }
    out
}

/// What `record` says about a run that gave no recording.
fn unrecorded(r: &Recorded) -> Option<Json> {
    if r.timed_out {
        return Some(json!({"status": "timeout", "written": false}));
    }
    if r.refusal_count > 0 {
        return Some(
            json!({"status": "refused", "written": false, "refusals": r.refusals, "refusal_count": r.refusal_count,
            "how": "the glossary's binding gave no value of a quantity's type, or a read was refused (a pointer that is null at that tick): fix glossary/binding/ or the quantity's type"}),
        );
    }
    if r.initial.is_none() {
        return Some(json!({"status": "no_ticks", "written": false,
            "how": "the glossary binding's tick store never ran: check its address, bytes and pc, and that the recording reaches the game's logic"}));
    }
    None
}

#[derive(clap::Args, Clone, Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct ScenarioRecordArgs {
    /// The scenario: `scenarios/<ID>.toml`.
    pub id: String,
    /// The recording's name: it is written to `scenarios/<ID>/<NAME>.jsonl`.
    #[arg(long)]
    pub name: String,
    /// Fields (vblanks) to record after the first tick, which runs with the pad released.
    #[arg(long)]
    pub frames: u32,
    /// Pad input, as run's (frames from the field after the first tick).
    #[arg(long, conflicts_with = "replay")]
    #[serde(default)]
    pub input: Option<crate::replay::Events>,
    /// Pad input from a replay file.
    #[arg(long)]
    #[serde(default)]
    pub replay: Option<PathBuf>,
    /// What the recording does, in a sentence.
    #[arg(long)]
    #[serde(default)]
    pub description: Option<String>,
    /// Write over an existing recording of that name.
    #[arg(long)]
    #[serde(default)]
    pub replace: bool,
    /// Wall-clock limit in seconds.
    #[arg(long, default_value_t = 600)]
    #[serde(default = "d_timeout")]
    pub timeout: u64,
}

fn d_timeout() -> u64 {
    600
}

fn record_cmd(ctx: &Ctx, a: ScenarioRecordArgs) -> crate::Result<(bool, Json)> {
    let root = &ctx.root;
    check_name("id", &a.id)?;
    check_name("name", &a.name)?;
    let l = Loaded::load(root, &a.id);
    if !l.problems.is_empty() {
        return Err(Error::invalid("the scenario or glossary has problems").with("problems", json!(l.problems)));
    }
    let (s, g) = (l.scenario.as_ref().unwrap(), l.glossary.as_ref().unwrap());
    let out = recording_path(root, &a.id, &a.name);
    if out.exists() && !a.replace {
        return Err(Error::invalid(format!("{}: exists; replace=true writes over it", rel(root, &out))));
    }
    let chain_file = root.join(&s.chain);
    let (state, chain) = crate::states::chain_state(ctx, &chain_file)?;
    let events =
        crate::replay::resolve(a.replay.as_deref(), a.input.as_ref(), a.frames as u64).map_err(Error::invalid)?;
    if crate::replay::uses_sticks(&events) {
        return Err(Error::invalid("the port's scenario runner has digital buttons only: record without the sticks"));
    }
    let b = binding(root, g)?;
    let mut m = machine(ctx, &chain, &state, &b.described().source)?;
    let compared = l.compared();
    let q = Quantities { binding: &b, types: g.types(), kept: compared.keys().cloned().collect() };
    let r = record(&mut m, &q, a.frames, &events, Duration::from_secs(a.timeout));
    if let Some(why) = unrecorded(&r) {
        return Ok((false, why));
    }
    let header = Header {
        format: format::RECORDING_FORMAT.into(),
        format_version: format::RECORDING_FORMAT_VERSION,
        scenario: a.id.clone(),
        source: Source {
            kind: SourceKind::Captured,
            platform: Some(binding::PLATFORM.into()),
            image_sha256: Some(b.described().source.image_sha256.clone()),
        },
        producer: Producer {
            name: "psx".into(),
            version: env!("CARGO_PKG_VERSION").into(),
            details: json!({
                "chain": s.chain,
                "from_state": rel(root, &state),
                "from_state_sha256": chain["state_sha256"],
                "frames": a.frames,
                "input": crate::replay::report(&events),
            })
            .as_object()
            .cloned()
            .unwrap_or_default(),
        },
        quantities: compared,
        initial: r.initial.unwrap(),
        description: a.description.clone(),
    };
    let rec = Recording::encode(header, &r.ticks);
    let problems = rec.check(&a.id);
    if !problems.is_empty() {
        return Err(Error::new("internal", "the recording does not check").with("problems", json!(problems)));
    }
    if let Some(d) = out.parent() {
        std::fs::create_dir_all(d).map_err(|e| Error::io(d, e))?;
    }
    std::fs::write(&out, rec.to_jsonl()).map_err(|e| Error::io(&out, e))?;
    let last = r.ticks.last();
    Ok((
        true,
        json!({"status": "recorded", "recording": rel(root, &out), "written": true,
               "ticks": r.ticks.len(), "fields": last.map(|t| t.field),
               "next": "psx scenario check runs the determinism check on the scenario's recordings"}),
    ))
}

impl Op for ScenarioRecordArgs {
    fn exec(self, ctx: &Ctx) -> Outcome {
        Outcome::report(record_cmd(ctx, self))
    }
}

/// The ways a recording made again differs from the recorded one: the
/// quantities' values, tick by tick (the differences), and when the ticks
/// ended (the schedule). The schedule is a note, not a difference: the game
/// falling behind depends on timing no parameter holds (where its loop is
/// when a field ends, how long a disc read takes), and the port replays the
/// recorded schedule anyway. `run` names the run compared, in the lines: `the
/// second route`.
fn differences(recorded: &Recording, again: &Header, ticks: &[Full], run: &str) -> (Vec<String>, Vec<String>) {
    let (mut out, mut schedule) = (Vec::new(), Vec::new());
    for (n, v) in &recorded.header.initial {
        if again.initial.get(n) != Some(v) {
            out.push(format!("initial {n}: recorded {}, {run} {}", show(Some(v)), show(again.initial.get(n))));
        }
    }
    let full = recorded.expand();
    if full.len() != ticks.len() {
        schedule.push(format!("recorded {} ticks, {run} ran {} in the same fields", full.len(), ticks.len()));
    }
    if let Some((a, b)) = full.iter().zip(ticks).find(|(a, b)| a.field != b.field) {
        schedule.push(format!("tick {}: ended in field {}, on {run} in field {}", a.tick, a.field, b.field));
    }
    for n in recorded.header.quantities.keys() {
        if let Some((a, b)) = full.iter().zip(ticks).find(|(a, b)| a.values.get(n) != b.values.get(n)) {
            out.push(format!(
                "tick {} {n}: recorded {}, {run} {}",
                a.tick,
                show(a.values.get(n)),
                show(b.values.get(n))
            ));
        }
    }
    out.truncate(MAX_DIFFERENCES);
    (out, schedule)
}

fn show(v: Option<&psx_scenario::Value>) -> String {
    v.map(|v| serde_json::to_string(v).unwrap_or_default()).unwrap_or_else(|| "nothing".into())
}

/// The sha256 of everything a determinism check judged: the scenario, its
/// chain, the glossary and the recordings.
fn key(root: &Path, l: &Loaded) -> String {
    let read = |p: &Path| std::fs::read(p).unwrap_or_default();
    let mut all = read(&root.join(SCENARIOS).join(format!("{}.toml", l.id)));
    if let Some(s) = &l.scenario {
        all.extend(read(&root.join(&s.chain)));
    }
    for dir in [root.join(GLOSSARY), root.join(GLOSSARY).join("binding")] {
        all.extend(read(&dir.join("Cargo.toml")));
        for (path, text) in binding::sources(&dir).unwrap_or_default() {
            all.extend(format!("\n{path}\n{text}").bytes());
        }
    }
    for (name, f, _) in &l.recordings {
        all.extend(format!("\n{name}\n").bytes());
        all.extend(read(f));
    }
    crate::stamp::sha256(&all)
}

/// One scenario's determinism check: each recording made again from the
/// state the second route reaches, compared tick by tick.
fn determinism(ctx: &Ctx, id: &str, timeout: Duration) -> crate::Result<Json> {
    let root = &ctx.root;
    let l = Loaded::load(root, id);
    let mut problems = l.problems.clone();
    for (_, f, r) in &l.recordings {
        problems.extend(l.recording_problems(root, f, r));
    }
    let captured: Vec<(&String, &Recording)> = l
        .recordings
        .iter()
        .filter_map(|(n, _, r)| {
            r.as_ref().ok().filter(|r| r.header.source.kind == SourceKind::Captured).map(|r| (n, r))
        })
        .collect();
    if captured.is_empty() && problems.is_empty() {
        problems.push(format!("{SCENARIOS}/{id}/: no captured recording (psx scenario record)"));
    }
    if !problems.is_empty() {
        return Ok(json!({"id": id, "status": "problems", "problems": problems}));
    }
    let (s, g) = (l.scenario.as_ref().unwrap(), l.glossary.as_ref().unwrap());
    let chain_file = root.join(&s.chain);
    let (_, chain) = crate::states::chain_state(ctx, &chain_file)?;
    let second = crate::states::reach_idling(ctx, &chain_file, s.idle())?;
    let b = binding(root, g)?;
    let q = Quantities { binding: &b, types: g.types(), kept: l.compared().keys().cloned().collect() };
    let mut recordings = Vec::new();
    let mut same = true;
    for (name, rec) in captured {
        let d = &rec.header.producer.details;
        let frames = d.get("frames").and_then(Json::as_u64).unwrap_or(0) as u32;
        let events: crate::replay::Events = serde_json::from_value(d["input"]["events"].clone())
            .map_err(|e| Error::malformed(format!("{name}: producer.details.input: {e}")))?;
        let events = crate::replay::resolve(None, Some(&events), frames as u64).map_err(Error::invalid)?;
        let mut m = machine(ctx, &chain, &second, &b.described().source)?;
        let r = record(&mut m, &q, frames, &events, timeout);
        let (diffs, schedule) = match unrecorded(&r) {
            Some(why) => (
                vec![format!("the second route gave no recording: {}", why["status"].as_str().unwrap_or(""))],
                Vec::new(),
            ),
            None => {
                let again = Header { initial: r.initial.clone().unwrap(), ..rec.header.clone() };
                differences(rec, &again, &r.ticks, "the second route")
            }
        };
        same &= diffs.is_empty();
        let mut r = json!({"name": name, "differences": diffs});
        if !schedule.is_empty() {
            r["schedule"] = json!(schedule);
        }
        recordings.push(r);
    }
    Ok(json!({"id": id, "status": if same { "deterministic" } else { "differs" }, "idle": s.idle(),
              "recordings": recordings}))
}

#[derive(clap::Args, Clone, Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct ScenarioCheckArgs {
    /// Scenario ids; default every one.
    #[serde(default)]
    pub ids: Vec<String>,
    /// Wall-clock limit per recording, in seconds.
    #[arg(long, default_value_t = 600)]
    #[serde(default = "d_timeout")]
    pub timeout: u64,
}

fn check_cmd(ctx: &Ctx, a: ScenarioCheckArgs) -> crate::Result<(bool, Json)> {
    let ids = if a.ids.is_empty() { ids(&ctx.root) } else { a.ids.clone() };
    if ids.is_empty() {
        return Err(Error::invalid("no scenarios: write scenarios/<id>.toml"));
    }
    let mut results = Vec::new();
    let mut ok = true;
    for id in ids {
        check_name("id", &id)?;
        let mut r = determinism(ctx, &id, Duration::from_secs(a.timeout))?;
        ok &= r["status"] == "deterministic";
        let l = Loaded::load(&ctx.root, &id);
        let kept = json!({"key": key(&ctx.root, &l), "at": crate::stamp::utc_now(), "psx": crate::context::version(),
                          "result": r.clone()});
        let p = ctx.root.join(CHECKS).join(format!("{id}.json"));
        std::fs::create_dir_all(p.parent().unwrap()).map_err(|e| Error::io(&p, e))?;
        crate::write_atomic(&p, &serde_json::to_vec_pretty(&kept).unwrap_or_default()).map_err(|e| Error::io(&p, e))?;
        if r["status"] == "differs" {
            r["how"] = json!(
                "the second route gave other values: check that the chain's last run ends at a game moment (until on the game's own state, not a count of fields) and that the glossary binding's tick is the game's logic step, not its render loop; what still differs is state no parameter holds (a seed taken from the time since power-on): make it one, then record again"
            );
        }
        results.push(r);
    }
    Ok((ok, json!({"ok": ok, "scenarios": results})))
}

impl Op for ScenarioCheckArgs {
    fn exec(self, ctx: &Ctx) -> Outcome {
        Outcome::report(check_cmd(ctx, self))
    }
}

/// What stops one scenario from counting toward its feature's stage 2:
/// problems in its files, no captured recording, or no passing determinism
/// check of the files as they are.
pub fn unready(root: &Path, id: &str, feature: &str) -> Vec<String> {
    let l = Loaded::load(root, id);
    let mut out = l.problems.clone();
    if let Some(s) = l.scenario.as_ref().filter(|s| s.feature != feature) {
        out.push(format!("{SCENARIOS}/{id}.toml: names feature {}, but {feature} lists it", s.feature));
    }
    for (_, f, r) in &l.recordings {
        out.extend(l.recording_problems(root, f, r));
    }
    if !l.recordings.iter().any(|(_, _, r)| r.as_ref().is_ok_and(|r| r.header.source.kind == SourceKind::Captured)) {
        out.push(format!("{SCENARIOS}/{id}/: no captured recording (psx scenario record)"));
    }
    if !out.is_empty() {
        return out;
    }
    let kept: Option<Json> =
        std::fs::read(root.join(CHECKS).join(format!("{id}.json"))).ok().and_then(|b| serde_json::from_slice(&b).ok());
    match kept {
        None => out.push(format!("{id}: no determinism check (psx scenario check {id})")),
        Some(k) if k["key"].as_str() != Some(key(root, &l).as_str()) => {
            out.push(format!("{id}: changed since its determinism check (psx scenario check {id})"))
        }
        Some(k) => match k["result"]["status"].as_str() {
            Some("deterministic") => {}
            Some("differs") => {
                let first = k["result"]["recordings"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .flat_map(|r| r["differences"].as_array().cloned().unwrap_or_default())
                    .next();
                out.push(format!(
                    "{id}: not deterministic ({})",
                    first.as_ref().and_then(Json::as_str).unwrap_or("the second route differs")
                ));
            }
            _ => out.extend(
                k["result"]["problems"].as_array().into_iter().flatten().filter_map(|p| p.as_str().map(String::from)),
            ),
        },
    }
    out
}

/// What stops the feature's scenarios from counting toward stage 2; empty
/// when it lists at least one and each is deterministic.
pub fn feature_unready(root: &Path, e: &Entry) -> Vec<String> {
    if e.scenarios.is_empty() {
        return vec![format!(
            "{}: lists no scenarios (write scenarios/<id>.toml and list its id under scenarios)",
            e.file
        )];
    }
    e.scenarios.iter().flat_map(|s| unready(root, s, &e.id)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const QUANTITIES: &str = r#"
//! Demo: the quantities scenarios compare.

pub struct Quantities {
    /// Unit: units
    pub x: i32,
    pub dead: bool,
}
"#;

    pub(crate) fn write_scenario(root: &Path) {
        std::fs::create_dir_all(root.join("glossary/src")).unwrap();
        std::fs::create_dir_all(root.join("glossary/binding")).unwrap();
        std::fs::write(root.join("glossary/Cargo.toml"), "[package]\nname = \"glossary\"\n").unwrap();
        std::fs::write(root.join("glossary/src/lib.rs"), QUANTITIES).unwrap();
        std::fs::write(root.join("glossary/binding/Cargo.toml"), "[package]\nname = \"glossary-binding\"\n").unwrap();
        std::fs::create_dir_all(root.join("scenarios/walk")).unwrap();
        std::fs::create_dir_all(root.join("notes/states")).unwrap();
        std::fs::write(root.join("notes/states/start.json"), "{}").unwrap();
        std::fs::write(
            root.join("scenarios/walk.toml"),
            "feature = \"F-0001\"\nmoment = \"standing\"\nchain = \"notes/states/start.json\"\nkind = \"level\"\n[compare]\nx = 0\n",
        )
        .unwrap();
    }

    fn recording(x: i64) -> String {
        format!(
            "{{\"format\":\"scenario-recording\",\"format_version\":1,\"scenario\":\"walk\",\"source\":{{\"kind\":\"captured\",\"platform\":\"psx\",\"image_sha256\":\"00\"}},\"producer\":{{\"name\":\"psx\",\"version\":\"0\"}},\"quantities\":{{\"x\":{{\"type\":\"int\",\"bits\":32,\"signed\":true,\"unit\":\"units\"}}}},\"initial\":{{\"x\":{x}}}}}\n{{\"tick\":1,\"field\":2,\"values\":{{\"x\":5}}}}\n"
        )
    }

    #[test]
    fn the_glossary_is_its_crate_and_binding() {
        let d = crate::port::tests::tempdir::Dir::new();
        let root = d.0.as_path();
        assert_eq!(
            glossary(root).unwrap_err(),
            ["glossary/: not written yet (a crate whose src/lib.rs declares Quantities)"]
        );
        write_scenario(root);
        let g = glossary(root).unwrap();
        assert_eq!((g.title.as_str(), g.types().len()), ("Demo: the quantities scenarios compare", 2));
        std::fs::write(root.join("glossary.toml"), "").unwrap();
        std::fs::remove_file(root.join("glossary/binding/Cargo.toml")).unwrap();
        assert_eq!(
            glossary(root).unwrap_err(),
            [
                "glossary.toml: no longer read: the glossary is Rust, glossary/ and glossary/binding/ (docs/scenarios.md)",
                "glossary/binding: not written yet"
            ]
        );
    }

    struct Q {
        x: i32,
        dead: bool,
    }

    struct Glue;

    impl psx_capture::Quantities for Glue {
        const FINGERPRINT: &'static str = "";
        type Quantities = Q;
        fn quantities(q: &Q) -> psx_capture::Value {
            psx_capture::Value::Record(vec![
                ("x", psx_capture::Value::Int(q.x as i128)),
                ("dead", psx_capture::Value::Bool(q.dead)),
            ])
        }
    }

    /// Ticks end at the store to 0x80020010 by 0x80010010; x is at
    /// 0x80020000.
    struct Demo;

    impl psx_capture::Glossary for Demo {
        type Glue = Glue;
        const TITLE: &'static str = "Demo";
        const IMAGE_SHA256: &'static str = "0000000000000000000000000000000000000000000000000000000000000000";

        fn tick(&self, t: &mut psx_capture::Tick) {
            t.store(0x8002_0010, 4).by(0x8001_0010);
        }

        fn quantities(&self, m: &psx_capture::Machine) -> Q {
            Q { x: m.i32(0x8002_0000), dead: m.u8(0x8002_0020) != 0 }
        }
    }

    #[allow(unsafe_code)]
    unsafe extern "C" fn demo_call(what: u32, host: *const psx_capture::abi::Host) -> i32 {
        // SAFETY: the caller's.
        unsafe { psx_capture::abi::call(&Demo, what, host) }
    }

    #[test]
    fn a_recording_holds_the_kept_quantities_at_each_tick() {
        let b: Plugin<Described> = Plugin::new(demo_call, None).unwrap();
        assert_eq!((b.described().tick.at, b.described().tick.pc), (0x8002_0010, Some(0x8001_0010)));
        let mut m = Machine::new(vec![0; 0x8_0000]);
        #[rustfmt::skip]
        let loop_ = [
            0x3C09_8002, // lui t1, 0x8002
            0x8D28_0000, // lw t0, 0(t1)
            0x2508_0001, // addiu t0, t0, 1
            0xAD28_0000, // sw t0, 0(t1): x
            0xAD28_0010, // sw t0, 0x10(t1): the tick ends
            0x0800_4000, // j 0x80010000
            0x0000_0000, // nop
        ];
        for (i, w) in loop_.iter().enumerate() {
            m.bus.ram[0x1_0000 + 4 * i..][..4].copy_from_slice(&u32::to_le_bytes(*w));
        }
        m.cpu.set_pc(0x8001_0000);
        let types: BTreeMap<String, Type> =
            [("x".to_string(), Type::Int { bits: 32, signed: true }), ("dead".to_string(), Type::Bool)].into();
        let q = Quantities { binding: &b, types, kept: vec!["x".into()] };
        let r = record(&mut m, &q, 1, &[], Duration::from_secs(60));
        assert_eq!((r.refusal_count, r.timed_out), (0, false), "{:?}", r.refusals);
        assert_eq!(r.initial, Some([("x".to_string(), psx_scenario::Value::Int(1))].into()));
        assert_eq!(r.ticks[0].values, [("x".to_string(), psx_scenario::Value::Int(2))].into());
        assert!(r.ticks.len() > 10, "{} ticks", r.ticks.len());
    }

    #[test]
    fn a_scenario_is_ready_once_its_recordings_are_checked_deterministic() {
        let d = crate::port::tests::tempdir::Dir::new();
        let root = d.0.as_path();
        write_scenario(root);
        assert_eq!(unready(root, "walk", "F-0001"), ["scenarios/walk/: no captured recording (psx scenario record)"]);
        std::fs::write(root.join("scenarios/walk/a.jsonl"), recording(1)).unwrap();
        assert_eq!(unready(root, "walk", "F-0001"), ["walk: no determinism check (psx scenario check walk)"]);
        let l = Loaded::load(root, "walk");
        let ok = json!({"key": key(root, &l), "result": {"status": "deterministic"}});
        std::fs::create_dir_all(root.join(CHECKS)).unwrap();
        std::fs::write(root.join(CHECKS).join("walk.json"), ok.to_string()).unwrap();
        assert_eq!(unready(root, "walk", "F-0001"), Vec::<String>::new());
        assert_eq!(unready(root, "walk", "F-0002"), ["scenarios/walk.toml: names feature F-0001, but F-0002 lists it"]);
        // Any change to what was judged makes the check stale.
        std::fs::write(root.join("scenarios/walk/a.jsonl"), recording(2)).unwrap();
        assert_eq!(
            unready(root, "walk", "F-0001"),
            ["walk: changed since its determinism check (psx scenario check walk)"]
        );
        // A unit or description is documentation: the recording still stands.
        std::fs::write(root.join("scenarios/walk/a.jsonl"), recording(1).replace(",\"unit\":\"units\"", "")).unwrap();
        assert_eq!(
            unready(root, "walk", "F-0001"),
            ["walk: changed since its determinism check (psx scenario check walk)"]
        );
        // A recording of a type the glossary no longer declares is recorded again.
        std::fs::write(root.join("scenarios/walk/a.jsonl"), recording(1).replace("\"bits\":32", "\"bits\":16"))
            .unwrap();
        assert_eq!(
            unready(root, "walk", "F-0001"),
            [
                "scenarios/walk/a.jsonl: recorded for other quantities than the scenario compares, or with other types than the glossary declares; record it again"
            ]
        );
    }

    #[test]
    fn differences_name_the_tick_and_quantity() {
        let rec = Recording::parse(&recording(1)).unwrap();
        let mut again = rec.header.clone();
        let mut ticks = rec.expand();
        assert_eq!(differences(&rec, &again, &ticks, "the second route"), (vec![], vec![]));
        again.initial.insert("x".into(), psx_scenario::Value::Int(3));
        ticks[0].field = 3;
        ticks[0].values.insert("x".into(), psx_scenario::Value::Int(6));
        let (diffs, schedule) = differences(&rec, &again, &ticks, "the second route");
        assert_eq!(diffs, ["initial x: recorded 1, the second route 3", "tick 1 x: recorded 5, the second route 6"]);
        // When a tick ended is a note: the port replays the recorded schedule.
        assert_eq!(schedule, ["tick 1: ended in field 2, on the second route in field 3"]);
    }
}
