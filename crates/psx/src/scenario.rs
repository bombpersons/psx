//! Scenarios (docs/scenarios.md): the glossary (`glossary/`, its quantities,
//! and `glossary/binding/`, where the PlayStation keeps them and what ends
//! a tick), `psx scenario record` (the original's quantities tick by tick,
//! from the scenario's state), `psx scenario check` (the determinism check)
//! and the facts `port next` reads.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use psx_core::Machine;
use psx_core::trace::{TraceConfig, Tracer};
use psx_scenario::Release;
use psx_scenario::scenario::{self as format, Full, Header, Producer, Recording, Scenario, Source, SourceKind};
use psx_scenario::{Glossary, Type, Values};
use serde_json::{Value as Json, json};

use crate::Error;

/// What sees the machine at the end of each recorded tick (the web viewer's
/// frames).
pub(crate) type OnTick<'a> = &'a mut dyn FnMut(&Full, &Machine);
use crate::adjust::Adjustments;
use crate::binding::{self, Plugin, Tick};
use crate::commands::{Ctx, Op, Outcome};
use crate::session::{LeadIn, Observer, Out, Session};

/// The glossary crate, `glossary/`; its binding is `glossary/binding/`.
pub const GLOSSARY: &str = "glossary";
pub const SCENARIOS: &str = "scenarios";
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

/// The glossary's quantities (glossary/src/lib.rs), or what is wrong with
/// it: what checking a port needs.
pub fn quantities(root: &Path) -> Result<Glossary, Vec<String>> {
    let mut problems = Vec::new();
    if root.join("glossary.toml").is_file() {
        problems.push(
            "glossary.toml: no longer read: the glossary is Rust, glossary/ and glossary/binding/ (psx guide, Recordings)"
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
    if problems.is_empty() { Ok(g) } else { Err(problems) }
}

/// [`quantities`], and the glossary's binding written: what recording needs.
pub fn glossary(root: &Path) -> Result<Glossary, Vec<String>> {
    let (g, mut problems) = match quantities(root) {
        Ok(g) => (Some(g), Vec::new()),
        Err(p) => (None, p),
    };
    let dir = root.join(GLOSSARY);
    if !binding::is_crate(&dir) {
        return Err(problems);
    }
    let binding = dir.join("binding/Cargo.toml");
    match std::fs::read_to_string(&binding) {
        Err(_) => problems.push(format!(
            "{GLOSSARY}/binding: not written yet (a cdylib package named glossary-binding, a member of the workspace; vendor/psx-capture's crate docs show it)"
        )),
        Ok(t) if psx_scenario::rust::package_name(&t).as_deref() != Some("glossary-binding") => {
            problems.push(format!("{GLOSSARY}/binding/Cargo.toml: the package is named \"glossary-binding\""))
        }
        Ok(_) => {}
    }
    match g {
        Some(g) if problems.is_empty() => Ok(g),
        _ => Err(problems),
    }
}

/// The glossary binding's answer to `DESCRIBE`.
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Described {
    /// The fingerprint of the glossary its glue was generated from.
    glossary: String,
    pub source: Release,
    pub tick: Tick,
    /// Where the binding's hooks run (docs/script-coverage-design.md).
    #[serde(default)]
    pub hooks: crate::script_cover::HookPoints,
}

/// The glossary's quantities by type, and its binding, built and loaded:
/// what `run --sample` reads glossary names and ticks with.
pub fn glossary_binding(root: &Path) -> crate::Result<(BTreeMap<String, Type>, Plugin<Described>)> {
    let g = glossary(root).map_err(|p| Error::invalid("the glossary has problems").with("problems", json!(p)))?;
    let b = binding(root, &g)?;
    Ok((g.types(), b))
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
        // A latched recording measures whether input reached the game in
        // time (late_fields); one from before that cannot say.
        let adjusted = r.header.producer.details.get("adjusted");
        if r.header.source.kind == SourceKind::Captured
            && adjusted.is_some_and(|a| a.get("buffer").is_some() && a.get("late_fields").is_none())
        {
            p.push(format!(
                "{at}: recorded before psx measured whether the game takes its input every tick; record it again"
            ));
        }
        // An adjusted recording is made with fixes/ as it is: the fixes
        // change what the game does, as the port's recordings must.
        if r.header.source.kind == SourceKind::Captured && r.header.producer.details.contains_key("adjust") {
            let made = r.header.producer.details.get("fix").and_then(|f| f["sources"].as_str());
            if made != crate::decomp::fixes_hash(root).as_deref() {
                p.push(format!(
                    "{at}: {}; record it again",
                    if made.is_none() { "made without fixes/" } else { "made with fixes/ as it was before" }
                ));
            }
        }
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

/// A machine at a state, on the chain's medium, with what saves and
/// restores it.
pub(crate) struct Booted {
    pub m: Machine,
    pub bios: Vec<u8>,
    pub media_id: Option<psx_core::state::MediaId>,
    pub media: Option<PathBuf>,
    pub bios_path: PathBuf,
}

/// The machine at a state, on the chain's medium, with its identity.
fn machine(ctx: &Ctx, chain: &Json, state: &Path, source: &Release) -> crate::Result<Booted> {
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
                "the glossary's binding is for image {}, the chain's media is {} (its IMAGE_SHA256 is psx context's resolved.image_sha256 for the media)",
                source.image_sha256, id.sha256
            ),
        ));
    }
    let m = crate::run_native::boot(
        Some(state),
        &Default::default(),
        bios.clone(),
        medium.exe.as_deref(),
        medium.disc,
        &medium.id,
    )?;
    Ok(Booted { m, bios, media_id: medium.id, media: resolved.media, bios_path })
}

/// The original's quantities at the end of every tick of a run.
struct Recorded {
    initial: Option<Values>,
    ticks: Vec<Full>,
    refusals: Vec<Json>,
    refusal_count: u64,
    timed_out: bool,
    /// The first tick that ended with the sticks moved on a pad not in
    /// analog mode, which the game does not read.
    unread_sticks: Option<u64>,
    /// What the adjustments did (`latch`: the pad buffer, lag frames).
    adjusted: Option<Json>,
}

/// What the glossary's binding is asked, and which quantities are kept.
struct Quantities<'a> {
    binding: &'a Plugin<Described>,
    /// Every quantity's type, which the binding's answers are checked against.
    types: BTreeMap<String, Type>,
    /// The quantities the recording keeps: the scenario's compared ones.
    kept: Vec<String>,
    /// What psx changes in how the original meets the machine.
    adjust: Adjustments,
}

/// What a recording keeps of what the original ran: its machine code
/// (every instruction, from the start) and its scripts.
pub(crate) struct Cover {
    pub tracer: Tracer,
    pub scripts: crate::script_cover::Scripts,
}

/// Record `frames` fields of ticks from `m`, with `events` by field. With
/// `ticks`, the pad each of a recording's ticks ended with, the input
/// follows the ticks instead, as the port's replay does: until a tick ends,
/// the pad is the one that tick had, and the run lasts as many ticks. A game
/// that falls a field behind, or catches up, on another route then reads
/// each press on the same tick. `cover` sees every instruction run, from
/// the start. `on_tick` sees the machine at the end of each recorded tick;
/// `look` sees the run as an observer of its own, after the recorder.
#[allow(clippy::too_many_arguments)]
fn record(
    m: &mut Machine,
    q: &Quantities,
    frames: u32,
    events: &[crate::replay::Input],
    ticks: Option<&[crate::replay::Input]>,
    deadline: Duration,
    cover: Option<&mut Cover>,
    on_tick: Option<OnTick<'_>>,
    look: Option<&mut dyn Observer>,
) -> Recorded {
    let started = Instant::now();
    let mut out = Recorded {
        initial: None,
        ticks: Vec::new(),
        refusals: Vec::new(),
        refusal_count: 0,
        timed_out: false,
        unread_sticks: None,
        adjusted: None,
    };
    // The pad is released until the first tick ends: `initial` is the moment
    // the port's launcher builds, before any input. The recording's fields
    // (and the input's frames) count from the field after that tick.
    crate::replay::apply(m, &crate::replay::Input { frame: 0, buttons: 0, sticks: [128; 4] });
    let driver: Box<dyn crate::session::Driver> = match ticks {
        Some(t) => Box::new(crate::session::Ticks { recording: String::new(), pads: t.to_vec() }),
        None => Box::new(crate::session::ByField { events: events.to_vec(), next: 0 }),
    };
    let machine = std::mem::replace(m, Machine::new(Vec::new()));
    let (machine, timed_out, adjusted) = {
        let mut s = Session::new(machine, u64::MAX, driver);
        s.adjust = Some(q.adjust.start(&mut s.m));
        s.tick = Some(q.binding.described().tick.clone());
        if let Some(c) = cover {
            c.scripts.declare(q.binding, &s.m);
            s.observe(Covering { c, binding: q.binding });
        }
        s.observe(Recorder { q, out: &mut out, on_tick });
        if let Some(look) = look {
            s.observe(Lent(look));
        }
        let lead_limit = s.m.bus.frames + frames as u64;
        let mut timed_out = false;
        match s.lead_in(lead_limit, started, deadline) {
            LeadIn::Ticked => {
                // Following ticks, the game may fall behind: room for it.
                let slack = if ticks.is_some() { frames as u64 / 8 + 16 } else { 0 };
                let target = s.start + frames as u64 + slack;
                // A stop is the driver's: the recording's last tick.
                let ended = s.advance(target, started, deadline).unwrap_or(crate::session::Ended::Limit);
                timed_out = ended == crate::session::Ended::Timeout;
            }
            LeadIn::Timeout => timed_out = true,
            // No tick within the recording's length: unrecorded() says so.
            LeadIn::NoTick => {}
        }
        let adjusted = s.adjust.as_ref().and_then(|a| a.report(&s.m));
        (s.m, timed_out, adjusted)
    };
    *m = machine;
    out.timed_out = timed_out;
    out.adjusted = adjusted;
    out
}

/// An observer the caller keeps.
struct Lent<'a>(&'a mut dyn Observer);

impl Observer for Lent<'_> {
    fn per_step(&self) -> bool {
        self.0.per_step()
    }
    fn step(&mut self, info: &psx_core::cpu::StepInfo, m: &mut Machine) -> crate::session::Flow {
        self.0.step(info, m)
    }
    fn tick(&mut self, rel: u64, m: &mut Machine) -> crate::session::Flow {
        self.0.tick(rel, m)
    }
    fn finish(self: Box<Self>, _m: &Machine, _out: &mut Out) -> crate::Result<()> {
        Ok(())
    }
}

/// The recording's coverage: every instruction run, and the scripts.
struct Covering<'a> {
    c: &'a mut Cover,
    binding: &'a Plugin<Described>,
}

impl Observer for Covering<'_> {
    fn per_step(&self) -> bool {
        true
    }
    fn step(&mut self, info: &psx_core::cpu::StepInfo, m: &mut Machine) -> crate::session::Flow {
        self.c.tracer.after_step(info, &m.cpu, &m.bus);
        self.c.scripts.step(info, self.binding, m);
        crate::session::Flow::Go
    }
    fn tick(&mut self, _rel: u64, m: &mut Machine) -> crate::session::Flow {
        self.c.scripts.declare(self.binding, m);
        crate::session::Flow::Go
    }
    fn finish(self: Box<Self>, _m: &Machine, _out: &mut Out) -> crate::Result<()> {
        Ok(())
    }
}

/// The recorder: each tick's quantities as the store left them, the first
/// tick's as the recording's initial values.
struct Recorder<'a, 'b> {
    q: &'a Quantities<'a>,
    out: &'a mut Recorded,
    on_tick: Option<OnTick<'b>>,
}

impl Observer for Recorder<'_, '_> {
    fn tick(&mut self, rel: u64, m: &mut Machine) -> crate::session::Flow {
        let (q, out) = (self.q, &mut *self.out);
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
            return crate::session::Flow::Go;
        }
        // A second tick in the first tick's field belongs to the lead-in
        // field, which the recording counts as field 0 (`rel` saturates).
        // The sticks as the game read them: centred unless the pad is in analog mode.
        let pad = &m.bus.sio0.pad;
        let sticks = if pad.analog { pad.axes() } else { format::CENTRED };
        if !pad.analog && pad.axes() != format::CENTRED {
            out.unread_sticks.get_or_insert(n);
        }
        let buttons = crate::replay::button_names(m.bus.sio0.buttons.into());
        out.ticks.push(Full { tick: n, field: rel, pad: buttons, analog: pad.analog, sticks, values });
        if let Some(f) = self.on_tick.as_deref_mut() {
            f(out.ticks.last().unwrap(), m);
        }
        crate::session::Flow::Go
    }
    fn finish(self: Box<Self>, _m: &Machine, _out: &mut Out) -> crate::Result<()> {
        Ok(())
    }
}

/// How many ticks took each number of fields, from the field each ended in
/// and the one the tick before ended in (so not the first): `{"2": 118,
/// "3": 4}` for a game at 30 Hz that slowed down 4 times.
fn fields_per_tick(ticks: &[Full]) -> Json {
    let mut h: BTreeMap<u64, u64> = BTreeMap::new();
    for w in ticks.windows(2) {
        *h.entry(w[1].field - w[0].field).or_default() += 1;
    }
    json!(h.into_iter().map(|(k, v)| (k.to_string(), json!(v))).collect::<serde_json::Map<_, _>>())
}

/// The pad held when each tick ended, from `events` by field: the last event
/// at or before the tick's field, or released before any.
fn held_by_tick(events: &[crate::replay::Input], ticks: &[Full]) -> Vec<crate::replay::Input> {
    let released = crate::replay::Input { frame: 0, buttons: 0, sticks: [0x80; 4] };
    ticks
        .iter()
        .map(|t| {
            let held = events.iter().take_while(|e| e.frame <= t.field).last().unwrap_or(&released);
            crate::replay::Input { frame: t.field, ..*held }
        })
        .collect()
}

/// What to do about code the inventory knows only from what ran, or not at
/// all.
pub(crate) const UNPACK_HINT: &str = "the inventory does not hold all the code that ran, so its functions there are not counted: if the game decompressed that code from the disc, unpack it (guide topic code, Decompressed code) and take the inventory again with --unpacked; if no snapshot held it, take the inventory again with a run that has it in RAM as --ram. Traced runs are credited anew; a scenario recording keeps only addresses there, so record it again";

/// A tracer that keeps only coverage (no trace lines), from now.
fn tracer(m: &mut Machine) -> Tracer {
    // The tracer copies a code page again when a store changed it.
    m.bus.page_writes = vec![0; psx_core::bus::RAM_SIZE / psx_core::trace::CODE_PAGE];
    let cfg = TraceConfig { from_frame: m.bus.frames, start_pc: None, frames: None, ranges: Vec::new(), max_lines: 0 };
    Tracer::new(cfg, Box::new(std::io::sink()), None)
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
    /// Start at the end of this recording of the scenario instead of at its state (a long
    /// play-through split in parts); its first tick runs with the pad released, as always.
    #[arg(long)]
    #[serde(default)]
    pub from: Option<String>,
    /// Write over an existing recording of that name.
    #[arg(long)]
    #[serde(default)]
    pub replace: bool,
    /// Record the stock game: none of the adjustments psx makes to how the original meets
    /// the machine (docs/master-plan.md), which recordings otherwise get and name.
    #[arg(long)]
    #[serde(default)]
    pub stock: bool,
    /// Wall-clock limit in seconds.
    #[arg(long, default_value_t = 600)]
    #[serde(default = "d_timeout")]
    pub timeout: u64,
}

fn d_timeout() -> u64 {
    600
}

/// A scenario ready to record from: its glossary and binding, the
/// inventory, its state and where the recording goes.
pub(crate) struct Prepared {
    pub id: String,
    pub name: String,
    /// The recording this one continues from the end of.
    pub from: Option<String>,
    l: Loaded,
    inv: Json,
    b: Plugin<Described>,
    out: PathBuf,
    pub state: PathBuf,
    chain: Json,
    /// What the recording is made with: every adjustment unless stock.
    pub adjust: Adjustments,
}

/// Everything a recording needs that can fail before anything runs.
pub(crate) fn prepare(ctx: &Ctx, id: &str, name: &str, from: Option<&str>, replace: bool) -> crate::Result<Prepared> {
    let root = &ctx.root;
    check_name("id", id)?;
    check_name("name", name)?;
    let l = Loaded::load(root, id);
    if !l.problems.is_empty() {
        return Err(Error::invalid("the scenario or glossary has problems").with("problems", json!(l.problems)));
    }
    let s = l.scenario.as_ref().unwrap();
    let out = recording_path(root, id, name);
    if out.exists() && !replace {
        return Err(Error::invalid(format!("{}: exists; replace=true writes over it", rel(root, &out))));
    }
    if let Some(f) = from {
        check_name("from", f)?;
        if f == name {
            return Err(Error::invalid("from names the recording being written: continue from another one"));
        }
        continued(&l, f).map_err(Error::invalid)?;
    }
    let (state, chain) = crate::states::chain_state(ctx, &root.join(&s.chain))?;
    // The recording keeps what the original ran against the inventory.
    let (_, inv) = crate::coverage::inventory(ctx, None).map_err(|e| {
        Error::invalid(format!("{e}: scenario record keeps what the original ran as the inventory's functions"))
    })?;
    let b = binding(root, l.glossary.as_ref().unwrap())?;
    let mut adjust = Adjustments::default_set();
    adjust.off = crate::adjust::Off::resolve(&s.off).map_err(|p| {
        Error::invalid(format!("scenarios/{id}.toml: [off] does not resolve")).with("problems", json!(p))
    })?;
    adjust.fix = crate::decomp::fixes(root)?;
    Ok(Prepared { id: id.into(), name: name.into(), from: from.map(Into::into), l, inv, b, out, state, chain, adjust })
}

/// A recording's frames and input, as its producer kept them.
fn kept_input(name: &str, rec: &Recording) -> Result<(u32, Vec<crate::replay::Input>), String> {
    let d = &rec.header.producer.details;
    let frames = d.get("frames").and_then(Json::as_u64).unwrap_or(0) as u32;
    let events: crate::replay::Events = serde_json::from_value(d["input"]["events"].clone())
        .map_err(|e| format!("{name}: producer.details.input: {e}"))?;
    let events = crate::replay::resolve(None, Some(&events), frames as u64)?;
    Ok((frames, events))
}

/// One of the recordings that lead to another's end: its name, frames,
/// input and adjustments.
type Continued = (String, u32, Vec<crate::replay::Input>, Adjustments);

/// The recordings that lead to the end of `name`, earliest first: `name`
/// and those it continues, each with its frames, input and adjustments.
fn continued(l: &Loaded, name: &str) -> Result<Vec<Continued>, String> {
    let mut out = Vec::new();
    let mut next = Some(name.to_string());
    while let Some(n) = next {
        if out.iter().any(|(m, _, _, _)| *m == n) {
            return Err(format!("{n} continues from itself, through the recordings it continues"));
        }
        let rec = match l.recordings.iter().find(|(m, _, _)| *m == n) {
            Some((_, _, Ok(r))) if r.header.source.kind == SourceKind::Captured => r,
            Some((_, f, Ok(_))) => return Err(format!("{}: not captured from the original", f.display())),
            Some((_, f, Err(e))) => return Err(format!("{}: {e}", f.display())),
            None => return Err(format!("{n}: no recording of that name in {SCENARIOS}/{}/", l.id)),
        };
        let (frames, events) = kept_input(&n, rec)?;
        let adjust = Adjustments::from_details(&rec.header.producer.details).map_err(|e| format!("{n}: {e}"))?;
        next = rec.header.producer.details.get("from").and_then(Json::as_str).map(Into::into);
        out.push((n, frames, events, adjust));
    }
    out.reverse();
    Ok(out)
}

/// Run `m` on to the end of recording `from` (and those it continues).
fn reach_end(m: &mut Machine, l: &Loaded, q: &Quantities, from: &str, deadline: Duration) -> crate::Result<()> {
    for (n, frames, events, adjust) in continued(l, from).map_err(Error::invalid)? {
        // Each as it was recorded.
        let q = Quantities { binding: q.binding, types: q.types.clone(), kept: q.kept.clone(), adjust };
        let r = record(m, &q, frames, &events, None, deadline, None, None, None);
        if let Some(why) = unrecorded(&r) {
            return Err(Error::invalid(format!(
                "replaying {n} to continue from its end gave no recording: {}",
                why["status"].as_str().unwrap_or("")
            )));
        }
    }
    Ok(())
}

impl Prepared {
    fn quantities(&self) -> Quantities<'_> {
        let g = self.l.glossary.as_ref().unwrap();
        Quantities {
            binding: &self.b,
            types: g.types(),
            kept: self.l.compared().keys().cloned().collect(),
            adjust: self.adjust.clone(),
        }
    }

    /// The machine where the recording starts: the scenario's state, or the
    /// end of the recording it continues.
    pub(crate) fn start(&self, ctx: &Ctx, timeout: Duration) -> crate::Result<Booted> {
        let mut b = machine(ctx, &self.chain, &self.state, &self.b.described().source)?;
        if let Some(from) = &self.from {
            reach_end(&mut b.m, &self.l, &self.quantities(), from, timeout)?;
        }
        Ok(b)
    }

    /// What ends one of the game's ticks, as the glossary binding says.
    pub(crate) fn tick(&self) -> crate::binding::Tick {
        self.b.described().tick.clone()
    }

    /// Record `frames` fields of `events` on `m` (where [`Prepared::start`]
    /// left it) and write the recording and its coverage.
    pub(crate) fn write(
        &self,
        ctx: &Ctx,
        m: &mut Machine,
        frames: u32,
        events: &[crate::replay::Input],
        description: Option<String>,
        timeout: Duration,
    ) -> crate::Result<(bool, Json)> {
        let root = &ctx.root;
        let mut cover =
            Cover { tracer: tracer(m), scripts: crate::script_cover::Scripts::new(&self.b.described().hooks) };
        let r = record(m, &self.quantities(), frames, events, None, timeout, Some(&mut cover), None, None);
        if let Some(why) = unrecorded(&r) {
            return Ok((false, why));
        }
        if let Some(n) = r.unread_sticks {
            return Err(Error::invalid(format!(
                "tick {n}: the sticks moved while the pad was not in analog mode, so the game did not read them: the chain needs a DualShock (pad dualshock) in analog mode before the sticks move: the game's own switch, or a press of ANALOG in the input"
            )));
        }
        let mut details = json!({
            "chain": self.l.scenario.as_ref().unwrap().chain,
            "from_state": rel(root, &self.state),
            "from_state_sha256": self.chain["state_sha256"],
            "frames": frames,
            "input": crate::replay::report(events),
        });
        if let Some(f) = &self.from {
            details["from"] = json!(f);
        }
        if let Some(a) = self.adjust.detail() {
            details["adjust"] = a;
        }
        if let Some(o) = self.adjust.off_detail() {
            details["off"] = o;
        }
        if let Some(f) = self.adjust.fix_detail() {
            details["fix"] = f;
        }
        if let Some(a) = &r.adjusted {
            details["adjusted"] = a.clone();
        }
        details["fields_per_tick"] = fields_per_tick(&r.ticks);
        let header = Header {
            format: format::RECORDING_FORMAT.into(),
            format_version: format::RECORDING_FORMAT_VERSION,
            scenario: self.id.clone(),
            source: Source {
                kind: SourceKind::Captured,
                platform: Some(binding::PLATFORM.into()),
                image_sha256: Some(self.b.described().source.image_sha256.clone()),
            },
            producer: Producer {
                name: "psx".into(),
                version: env!("CARGO_PKG_VERSION").into(),
                details: details.as_object().cloned().unwrap_or_default(),
            },
            quantities: self.l.compared(),
            initial: r.initial.unwrap(),
            description,
        };
        let rec = Recording::encode(header, &r.ticks);
        let problems = rec.check(&self.id);
        if !problems.is_empty() {
            return Err(Error::new("internal", "the recording does not check").with("problems", json!(problems)));
        }
        let out = &self.out;
        if let Some(d) = out.parent() {
            std::fs::create_dir_all(d).map_err(|e| Error::io(d, e))?;
        }
        std::fs::write(out, rec.to_jsonl()).map_err(|e| Error::io(out, e))?;
        // What the original ran while recording, for coverage.
        let (mut kept, partial) = crate::coverage::keep(root, &self.inv, &cover.tracer)?;
        if let Some(p) = cover.scripts.kept() {
            kept["programs"] = p;
        }
        kept["recording"] = json!(rel(root, out));
        let kept_path = out.with_extension("coverage.json");
        let text = serde_json::to_string_pretty(&kept).unwrap_or_default() + "\n";
        std::fs::write(&kept_path, text).map_err(|e| Error::io(&kept_path, e))?;
        let last = r.ticks.last();
        let mut result = json!({"status": "recorded", "recording": rel(root, out), "written": true,
               "adjust": self.adjust.names(), "adjusted": r.adjusted,
               "fields_per_tick": fields_per_tick(&r.ticks),
               "ticks": r.ticks.len(), "fields": last.map(|t| t.field), "coverage": rel(root, &kept_path),
               "next": "psx scenario check runs the determinism check on the scenario's recordings"});
        if let Some(f) = &self.from {
            result["from"] = json!(f);
        }
        if let Some(c) = catalogue_note(root, &self.id, &self.name) {
            result["catalogue"] = json!(c);
        }
        if !partial.is_empty() {
            result["coverage_partial"] = json!(partial);
            result["coverage_note"] = json!(UNPACK_HINT);
        }
        let sc = &cover.scripts;
        if !sc.programs.is_empty() {
            result["programs"] = json!(sc.programs.iter().map(|(n, p)| json!({"name": n, "bytes_read": p.read.len(), "sites": p.sites.len(), "decisions": p.ways.len()})).collect::<Vec<_>>());
        }
        let problems = sc.problems();
        if !problems.is_empty() {
            result["program_problems"] = json!(problems);
        }
        if sc.unlisted() > 0 {
            result["program_problems_unlisted"] = json!(sc.unlisted());
        }
        Ok((true, result))
    }
}

/// What the recording `id/name` is to the port: the person's play held
/// out (judged, never worked on) or a work recording. None outside a port.
fn catalogue_note(root: &Path, id: &str, name: &str) -> Option<String> {
    let port = crate::port::Port::load(root).ok()?;
    let target = format!("{id}/{name}");
    Some(if port.goal.held_out.contains(&target) {
        format!("{target} is held out (goal.toml's held_out): judged, not worked on")
    } else {
        format!("{target} is a work recording: the port is checked against it")
    })
}

fn record_cmd(ctx: &Ctx, a: ScenarioRecordArgs) -> crate::Result<(bool, Json)> {
    let events =
        crate::replay::resolve(a.replay.as_deref(), a.input.as_ref(), a.frames as u64).map_err(Error::invalid)?;
    let mut p = prepare(ctx, &a.id, &a.name, a.from.as_deref(), a.replace)?;
    if a.stock {
        p.adjust = p.adjust.without_timing();
    }
    let timeout = Duration::from_secs(a.timeout);
    let mut b = p.start(ctx, timeout)?;
    p.write(ctx, &mut b.m, a.frames, &events, a.description.clone(), timeout)
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
    if let Some((a, b)) = full.iter().zip(ticks).find(|(a, b)| a.analog != b.analog) {
        let mode = |analog| if analog { "analog" } else { "digital" };
        out.push(format!("tick {} pad mode: recorded {}, {run} {}", a.tick, mode(a.analog), mode(b.analog)));
    }
    if let Some((a, b)) = full.iter().zip(ticks).find(|(a, b)| a.sticks != b.sticks) {
        out.push(format!("tick {} sticks: recorded {:?}, {run} {:?}", a.tick, a.sticks, b.sticks));
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
        all.extend(chain_for_key(&read(&root.join(&s.chain))));
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

/// A chain as a determinism check judged it: without the emulator each
/// step was last rebuilt on, which a rebuild to the same state changes.
fn chain_for_key(bytes: &[u8]) -> Vec<u8> {
    let Ok(mut chain) = serde_json::from_slice::<Json>(bytes) else { return bytes.to_vec() };
    for step in chain["steps"].as_array_mut().into_iter().flatten() {
        if let Some(o) = step.as_object_mut() {
            o.remove("emulator");
        }
    }
    chain.to_string().into_bytes()
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
    for (name, rec) in &captured {
        if let Err(e) = Adjustments::from_details(&rec.header.producer.details) {
            problems.push(format!("{SCENARIOS}/{id}/{name}.jsonl: {e}"));
        }
    }
    if !problems.is_empty() {
        return Ok(json!({"id": id, "status": "problems", "problems": problems}));
    }
    let (s, g) = (l.scenario.as_ref().unwrap(), l.glossary.as_ref().unwrap());
    let chain_file = root.join(&s.chain);
    let (_, chain) = crate::states::chain_state(ctx, &chain_file)?;
    let second = crate::states::reach_idling(ctx, &chain_file, s.idle())?;
    let b = binding(root, g)?;
    let mut recordings = Vec::new();
    let mut same = true;
    let fixes_now = crate::decomp::fixes_hash(root);
    for (name, rec) in captured {
        // As it was recorded: checked above.
        let adjust = Adjustments::from_details(&rec.header.producer.details).unwrap_or_default();
        let fix_changed = adjust.fix.as_ref().map(|f| f.sources.clone()) != fixes_now;
        let q = Quantities { binding: &b, types: g.types(), kept: l.compared().keys().cloned().collect(), adjust };
        let (frames, events) = kept_input(name, rec).map_err(Error::malformed)?;
        let mut m = machine(ctx, &chain, &second, &b.described().source)?.m;
        if let Some(from) = rec.header.producer.details.get("from").and_then(Json::as_str) {
            reach_end(&mut m, &l, &q, from, timeout)?;
        }
        let pads = held_by_tick(&events, &rec.expand());
        let r = record(&mut m, &q, frames, &events, Some(&pads), timeout, None, None, None);
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
        if fix_changed {
            // Checked with the fixes it was made with; new recordings take fixes/ as it is.
            r["fix_changed"] =
                json!("fixes/ differs from what this recording was made with: record it again to take fixes/ as it is");
        }
        recordings.push(r);
    }
    Ok(json!({"id": id, "status": if same { "deterministic" } else { "differs" }, "idle": s.idle(),
              "recordings": recordings}))
}

/// What an adjustment changed: each quantity's first tick where the
/// adjusted run's value differs from the stock run's, with both values.
fn adjusted_differences(stock: &Recorded, adjusted: &Recorded) -> Vec<Json> {
    let mut out = Vec::new();
    let (a, b) = (stock.initial.clone().unwrap_or_default(), adjusted.initial.clone().unwrap_or_default());
    for (q, v) in &a {
        if b.get(q) != Some(v) {
            out.push(json!({"quantity": q, "tick": 0, "stock": v, "adjusted": b.get(q)}));
        }
    }
    let names: Vec<&String> = a.keys().collect();
    for q in names {
        if out.iter().any(|d| d["quantity"] == json!(q)) {
            continue;
        }
        let first = stock.ticks.iter().zip(&adjusted.ticks).find(|(s, t)| s.values.get(q) != t.values.get(q));
        if let Some((s, t)) = first {
            out.push(json!({"quantity": q, "tick": s.tick, "stock": s.values.get(q), "adjusted": t.values.get(q)}));
        }
    }
    if stock.ticks.len() != adjusted.ticks.len() {
        out.push(json!({"ticks": {"stock": stock.ticks.len(), "adjusted": adjusted.ticks.len()}}));
    }
    out.sort_by_key(|d| d["tick"].as_u64().unwrap_or(u64::MAX));
    out
}

/// One scenario's measurement of `adjust` (with the scenario's `off` when
/// `off`): each captured recording made again from the scenario's state,
/// stock and adjusted, driven by the same ticks, and compared.
fn adjust_check(ctx: &Ctx, id: &str, mut adjust: Adjustments, off: bool, timeout: Duration) -> crate::Result<Json> {
    let root = &ctx.root;
    let l = Loaded::load(root, id);
    let mut problems = l.problems.clone();
    if let Some(s) = l.scenario.as_ref().filter(|_| off) {
        match crate::adjust::Off::resolve(&s.off) {
            Ok(off) => adjust.off = off,
            Err(p) => problems.extend(p.into_iter().map(|p| format!("{SCENARIOS}/{id}.toml: {p}"))),
        }
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
    if adjust.is_stock() {
        return Ok(json!({"id": id, "status": "nothing_to_measure", "adjust": adjust.names(),
                         "why": "this psx makes no adjustments yet"}));
    }
    let (s, g) = (l.scenario.as_ref().unwrap(), l.glossary.as_ref().unwrap());
    let (state, chain) = crate::states::chain_state(ctx, &root.join(&s.chain))?;
    let b = binding(root, g)?;
    let mut recordings = Vec::new();
    let mut same = true;
    for (name, rec) in captured {
        let (frames, events) = kept_input(name, rec).map_err(Error::malformed)?;
        let pads = held_by_tick(&events, &rec.expand());
        let run = |adjust| -> crate::Result<Recorded> {
            let q = Quantities { binding: &b, types: g.types(), kept: l.compared().keys().cloned().collect(), adjust };
            let mut m = machine(ctx, &chain, &state, &b.described().source)?.m;
            if let Some(from) = rec.header.producer.details.get("from").and_then(Json::as_str) {
                reach_end(&mut m, &l, &q, from, timeout)?;
            }
            Ok(record(&mut m, &q, frames, &events, Some(&pads), timeout, None, None, None))
        };
        let (stock, adjusted) = (run(Adjustments::stock())?, run(adjust.clone())?);
        let differences = match (unrecorded(&stock), unrecorded(&adjusted)) {
            (Some(why), _) => vec![json!({"stock": why})],
            (_, Some(why)) => vec![json!({"adjusted": why})],
            _ => adjusted_differences(&stock, &adjusted),
        };
        same &= differences.is_empty();
        let mut entry = json!({"name": name, "differences": differences});
        if let Some(late) = adjusted.adjusted.as_ref().and_then(|a| a.get("late_input")) {
            entry["late_input"] = late.clone();
        }
        for k in ["off_calls", "fix_calls"] {
            if let Some(calls) = adjusted.adjusted.as_ref().and_then(|a| a.get(k)) {
                entry[k] = calls.clone();
            }
        }
        recordings.push(entry);
    }
    Ok(json!({"id": id, "status": if same { "same" } else { "differs" }, "adjust": adjust.names(),
              "recordings": recordings}))
}

#[derive(clap::Args, Clone, Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct ScenarioAdjustCheckArgs {
    /// Scenario ids; default every one.
    #[serde(default)]
    pub ids: Vec<String>,
    /// Wall-clock limit per run, in seconds.
    #[arg(long, default_value_t = 600)]
    #[serde(default = "d_timeout")]
    pub timeout: u64,
    /// Measure only these adjustments (`latch`, `steady`, `off`: the
    /// scenario's `[off]` table, `fix`: fixes/), to tell which one a
    /// difference comes from; repeatable. Default: all of them, as
    /// recordings are made.
    #[arg(long = "adjust", value_parser = ["latch", "steady", "off", "fix"])]
    #[serde(default)]
    pub adjust: Vec<String>,
}

fn adjust_check_cmd(ctx: &Ctx, a: ScenarioAdjustCheckArgs) -> crate::Result<(bool, Json)> {
    let ids = if a.ids.is_empty() { ids(&ctx.root) } else { a.ids.clone() };
    if ids.is_empty() {
        return Err(Error::invalid("no scenarios: write scenarios/<id>.toml"));
    }
    let (adjust, off) = if a.adjust.is_empty() {
        (Adjustments { fix: crate::decomp::fixes(&ctx.root)?, ..Adjustments::default_set() }, true)
    } else {
        let (mut only, mut off) = (Adjustments::stock(), false);
        for n in &a.adjust {
            match n.as_str() {
                "latch" => only.latch = true,
                "steady" => only.steady = true,
                "off" => off = true,
                "fix" => {
                    only.fix = crate::decomp::fixes(&ctx.root)?;
                    if only.fix.is_none() {
                        return Err(Error::invalid("adjust: fix measures fixes/, which holds no C"));
                    }
                }
                _ => return Err(Error::invalid(format!("adjust: {n:?} is not latch, steady, off or fix"))),
            }
        }
        (only, off)
    };
    let mut results = Vec::new();
    for id in ids {
        check_name("id", &id)?;
        let mut r = adjust_check(ctx, &id, adjust.clone(), off, Duration::from_secs(a.timeout))?;
        let kept = json!({"at": crate::stamp::utc_now(), "psx": crate::context::version(), "result": r.clone()});
        let p = ctx.root.join(CHECKS).join(format!("{id}.adjust.json"));
        std::fs::create_dir_all(p.parent().unwrap()).map_err(|e| Error::io(&p, e))?;
        crate::write_atomic(&p, &serde_json::to_vec_pretty(&kept).unwrap_or_default()).map_err(|e| Error::io(&p, e))?;
        if r["status"] == "differs" {
            r["how"] = json!(
                "an adjustment changed what the original did: a shift (the same values a tick or two earlier or later, as a press taking effect sooner under latch) is the adjustment's expected cost; a value the stock run never reaches is a rule that depends on what was adjusted: record that scenario --stock, and say why in the notes"
            );
        }
        results.push(r);
    }
    let ok = results.iter().all(|r| r["status"] != "problems");
    Ok((ok, json!({"ok": ok, "scenarios": results})))
}

impl Op for ScenarioAdjustCheckArgs {
    fn exec(self, ctx: &Ctx) -> Outcome {
        Outcome::report(adjust_check_cmd(ctx, self))
    }
}

/// A value as a number to plot: in the quantity's units, a bool as 0 or 1,
/// an enum as its variant's index.
fn plotted(t: &Type, v: &psx_scenario::Value) -> Option<f64> {
    use psx_scenario::Value as V;
    match (t, v) {
        (_, V::Bool(b)) => Some(f64::from(u8::from(*b))),
        (Type::Enum(names), V::Variant(n)) => names.iter().position(|x| x == n).map(|i| i as f64),
        (Type::Fixed { frac_bits, .. }, V::Int(n)) => Some(*n as f64 / 2f64.powi(*frac_bits as i32)),
        (Type::Float { bits }, V::Int(n)) => float_number(*bits, *n),
        (Type::Int { .. }, V::Int(n)) => Some(*n as f64),
        _ => None,
    }
}

/// A recording for `psx web`'s viewer: each tick's pad, each compared
/// quantity's values over the ticks (index 0 the initial ones), and what the
/// last `port check` said of it (`last`, `.psx/port/last.json`): its
/// failures, and the port's values when the port's report kept them.
pub(crate) fn plot(root: &Path, id: &str, name: &str, last: &Json) -> crate::Result<Json> {
    check_name("id", id)?;
    check_name("name", name)?;
    let l = Loaded::load(root, id);
    let rec = match l.recordings.iter().find(|(n, _, _)| n == name) {
        Some((_, _, Ok(r))) => r,
        Some((_, f, Err(e))) => return Err(Error::malformed(format!("{}: {e}", rel(root, f)))),
        None => return Err(Error::invalid(format!("{name}: no recording of that name in {SCENARIOS}/{id}/"))),
    };
    let full = rec.expand();
    let declared = l.glossary.as_ref().map(|g| g.quantities.clone()).unwrap_or_default();
    let tolerance = |q: &str| l.scenario.as_ref().and_then(|s| s.compare.get(q).cloned());
    let reported = last["scenarios"]["scenarios"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|s| s["id"] == id)
        .and_then(|s| s["recordings"].as_array()?.iter().find(|r| r["name"] == name).cloned());
    let port_values: Vec<psx_scenario::Values> =
        reported.as_ref().and_then(|r| serde_json::from_value(r["values"].clone()).ok()).unwrap_or_default();
    let mut quantities = Vec::new();
    let mut types = BTreeMap::new();
    for (q, f) in &rec.header.quantities {
        let Ok(t) = f.ty() else { continue };
        let at = |v: Option<&psx_scenario::Value>| v.and_then(|v| plotted(&t, v));
        let text = |v: Option<&psx_scenario::Value>| v.map(|v| shown(&t, v, false)).unwrap_or(Json::Null);
        let values: Vec<Option<f64>> =
            std::iter::once(at(rec.header.initial.get(q))).chain(full.iter().map(|x| at(x.values.get(q)))).collect();
        let shown_values: Vec<Json> = std::iter::once(text(rec.header.initial.get(q)))
            .chain(full.iter().map(|x| text(x.values.get(q))))
            .collect();
        let g = declared.get(q);
        let mut row = json!({
            "name": q, "type": f.kind, "unit": g.and_then(|g| g.unit.clone()).or(f.unit.clone()),
            "description": g.and_then(|g| g.description.clone()), "tolerance": tolerance(q),
            "values": values, "shown": shown_values,
        });
        if let Type::Enum(names) = &t {
            row["variants"] = json!(names);
        }
        if !port_values.is_empty() {
            row["port"] = json!(port_values.iter().map(|v| at(v.get(q))).collect::<Vec<_>>());
            row["port_shown"] = json!(port_values.iter().map(|v| text(v.get(q))).collect::<Vec<_>>());
        }
        types.insert(q.clone(), t);
        quantities.push(row);
    }
    // Where each quantity first differed, for the plots: the failures
    // name the quantities alone, the differences say where.
    let mut failures: Vec<Json> = reported
        .as_ref()
        .and_then(|r| r["differences"].as_array().cloned())
        .unwrap_or_default()
        .iter()
        .map(|d| {
            let (q, at) = (d["quantity"].as_str().unwrap_or(""), d["at"].as_str().unwrap_or(""));
            let tick =
                if at == "initial" { Some(0) } else { at.strip_prefix("tick ").and_then(|n| n.parse::<u64>().ok()) };
            let number = |v: &Json| {
                let v: psx_scenario::Value = serde_json::from_value(v.clone()).ok()?;
                plotted(types.get(q)?, &v)
            };
            let got = if d["got"].is_null() { "the port has none".to_string() } else { format!("got {}", d["got"]) };
            let known = types.contains_key(q);
            json!({"text": format!("{at} {q}: expected {}, {got}", d["expected"]), "tick": tick.filter(|_| known),
                   "quantity": known.then_some(q), "expected": number(&d["expected"]), "got": number(&d["got"])})
        })
        .collect();
    // The rest (a panic, the launcher) as they are.
    let lines = reported.as_ref().and_then(|r| r["failures"].as_array().cloned()).unwrap_or_default();
    for line in lines.iter().filter_map(Json::as_str) {
        if !line.starts_with("differs from the recording in ") && !line.starts_with("the port has no ") {
            failures.push(json!({"text": line, "tick": null, "quantity": null, "expected": null, "got": null}));
        }
    }
    let d = &rec.header.producer.details;
    Ok(json!({
        "scenario": id, "name": name, "description": rec.header.description,
        "captured": rec.header.source.kind == SourceKind::Captured,
        "from": d.get("from"), "frames": d.get("frames"),
        "moment": l.scenario.as_ref().map(|s| &s.moment),
        "ticks": full.iter().map(|t| json!({"tick": t.tick, "field": t.field, "pad": t.pad, "analog": t.analog, "sticks": t.sticks})).collect::<Vec<_>>(),
        "quantities": quantities,
        "check": reported.map(|_| json!({"commit": last["commit"], "at": last["at"], "failures": failures})),
    }))
}

/// A captured recording, ready to run again: its scenario, binding,
/// adjustments, input and the pad each of its ticks ended with.
pub(crate) struct Captured {
    l: Loaded,
    index: usize,
    b: Plugin<Described>,
    state: PathBuf,
    chain: Json,
    pub adjust: Adjustments,
    /// Its fields, and its input by field.
    pub frames: u32,
    pub events: Vec<crate::replay::Input>,
    /// The pad each of its ticks ended with.
    pub pads: Vec<crate::replay::Input>,
}

impl Captured {
    pub(crate) fn load(ctx: &Ctx, id: &str, name: &str) -> crate::Result<Captured> {
        let root = &ctx.root;
        check_name("id", id)?;
        check_name("name", name)?;
        let l = Loaded::load(root, id);
        if !l.problems.is_empty() {
            return Err(Error::invalid("the scenario or glossary has problems").with("problems", json!(l.problems)));
        }
        let Some(index) = l.recordings.iter().position(|(n, _, _)| n == name) else {
            return Err(Error::invalid(format!("{name}: no recording of that name in {SCENARIOS}/{id}/")));
        };
        let rec = match &l.recordings[index] {
            (_, _, Ok(r)) if r.header.source.kind == SourceKind::Captured => r,
            (_, f, Ok(_)) => return Err(Error::invalid(format!("{}: not captured from the original", rel(root, f)))),
            (_, f, Err(e)) => return Err(Error::malformed(format!("{}: {e}", rel(root, f)))),
        };
        let (s, g) = (l.scenario.as_ref().unwrap(), l.glossary.as_ref().unwrap());
        let (state, chain) = crate::states::chain_state(ctx, &root.join(&s.chain))?;
        let b = binding(root, g)?;
        let adjust = Adjustments::from_details(&rec.header.producer.details).map_err(Error::malformed)?;
        let (frames, events) = kept_input(name, rec).map_err(Error::malformed)?;
        let pads = held_by_tick(&events, &rec.expand());
        Ok(Captured { l, index, b, state, chain, adjust, frames, events, pads })
    }

    fn recording(&self) -> &Recording {
        self.l.recordings[self.index].2.as_ref().unwrap()
    }

    /// The scenario's chain state.
    pub(crate) fn state(&self) -> &Path {
        &self.state
    }

    /// What ends one of the game's ticks, as the glossary binding says.
    pub(crate) fn tick(&self) -> crate::binding::Tick {
        self.b.described().tick.clone()
    }

    /// The machine where the recording starts: the scenario's state, past
    /// the recordings it continues.
    pub(crate) fn at_start(&self, ctx: &Ctx, timeout: Duration) -> crate::Result<Booted> {
        let mut b = machine(ctx, &self.chain, &self.state, &self.b.described().source)?;
        if let Some(from) = self.recording().header.producer.details.get("from").and_then(Json::as_str) {
            let g = self.l.glossary.as_ref().unwrap();
            let kept = self.l.compared().keys().cloned().collect();
            let q = Quantities { binding: &self.b, types: g.types(), kept, adjust: self.adjust.clone() };
            reach_end(&mut b.m, &self.l, &q, from, timeout)?;
        }
        Ok(b)
    }
}

/// Replay a captured recording from the scenario's state, following its
/// ticks as the port's replay does; `prepare` sees the machine once it is
/// at the recording's start, and `on_tick` at the end of each tick. What
/// differs from the recording comes back (nothing when the replay is the
/// recording), with the machine as the replay left it.
pub(crate) fn replay(
    ctx: &Ctx,
    id: &str,
    name: &str,
    timeout: Duration,
    on_tick: OnTick<'_>,
    prepare: Option<&mut dyn FnMut(&mut Machine)>,
    look: Option<&mut dyn Observer>,
) -> crate::Result<(Vec<String>, Machine)> {
    let c = Captured::load(ctx, id, name)?;
    let rec = c.recording();
    let g = c.l.glossary.as_ref().unwrap();
    let q = Quantities {
        binding: &c.b,
        types: g.types(),
        kept: c.l.compared().keys().cloned().collect(),
        adjust: c.adjust.clone(),
    };
    let mut m = c.at_start(ctx, timeout)?.m;
    if let Some(prepare) = prepare {
        prepare(&mut m);
    }
    let r = record(&mut m, &q, c.frames, &c.events, Some(&c.pads), timeout, None, Some(on_tick), look);
    let d = match unrecorded(&r) {
        Some(why) => vec![format!("the replay gave no recording: {}", why["status"].as_str().unwrap_or(""))],
        None => {
            let again = Header { initial: r.initial.clone().unwrap(), ..rec.header.clone() };
            let (mut d, schedule) = differences(rec, &again, &r.ticks, "the replay");
            d.extend(schedule);
            d
        }
    };
    Ok((d, m))
}

/// The quantities difference lines name (`tick N q: ...`, `initial q:
/// ...`), in their order, without where or the values.
fn differing(diffs: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for d in diffs {
        let rest = d.strip_prefix("initial ").or_else(|| d.strip_prefix("tick ")?.split_once(' ').map(|(_, r)| r));
        if let Some((q, _)) = rest.and_then(|r| r.split_once(':')) {
            if !out.iter().any(|o| o == q) {
                out.push(q.to_string());
            }
        }
    }
    out
}

/// Each captured recording of scenario `id` (or those of `only`) replayed
/// on the original with the agent's C replacing `d`'s functions
/// (docs/decomp-plan.md, step 13). A recording that differs is replayed
/// again without the C: one that then replays as recorded diverges with
/// the C, in the quantities it names (not where or by how much: the C is
/// understood, not tuned). False when one diverges.
pub(crate) fn with_c(
    ctx: &Ctx,
    id: &str,
    only: Option<&[String]>,
    d: &crate::decomp::Replacement,
    timeout: Duration,
) -> crate::Result<(bool, Json)> {
    let l = Loaded::load(&ctx.root, id);
    let names: Vec<String> = l
        .recordings
        .iter()
        .filter(|(n, _, r)| {
            r.as_ref().is_ok_and(|r| r.header.source.kind == SourceKind::Captured)
                && only.is_none_or(|o| o.iter().any(|x| x == n))
        })
        .map(|(n, _, _)| n.clone())
        .collect();
    let (mut ok, mut out) = (true, Vec::new());
    let (mut steps, mut in_c) = (0u64, 0u64);
    for name in names {
        let mut prepare = |m: &mut Machine| d.install(m);
        let (diffs, m) = replay(ctx, id, &name, timeout, &mut |_, _| {}, Some(&mut prepare), None)?;
        let report = d.report(&m);
        steps += report["steps"].as_u64().unwrap_or(0);
        in_c += report["steps_in_c"].as_u64().unwrap_or(0);
        let mut r = json!({"name": name, "replaced": report["replaced"], "share_in_c": report["share_in_c"]});
        if diffs.is_empty() {
            r["status"] = json!("same");
        } else {
            let (without, _) = replay(ctx, id, &name, timeout, &mut |_, _| {}, None, None)?;
            if without.is_empty() {
                ok = false;
                r["status"] = json!("diverges_with_c");
                r["differs_in"] = json!(differing(&diffs));
                r["how"] = json!(
                    "the replay differs with the C and not without it: some function it replaces does not do what the original's does on this recording; code check them, and compare the C with the original's code where it writes these quantities (the debugger on the recording)"
                );
            } else {
                r["status"] = json!("differs_without_c_too");
                r["without_c"] = json!(without);
                r["differences"] = json!(diffs);
            }
        }
        crate::decomp::log_run(
            &ctx.root,
            id,
            r["name"].as_str().unwrap_or(""),
            r["status"].as_str().unwrap_or(""),
            &r["share_in_c"],
            &r["replaced"],
        );
        out.push(r);
    }
    let share = if steps == 0 { 0.0 } else { ((in_c as f64 / steps as f64) * 1e6).round() / 1e6 };
    Ok((ok, json!({"ok": ok, "share_in_c": share, "recordings": out})))
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
    /// Also replay each captured recording on the original with the
    /// agent's C (decomp/) in place of the functions it defines: alone,
    /// those whose last code check passed on this C and names;
    /// `--decomp=all` every one; `--decomp=a,b` those named.
    #[arg(long, num_args = 0..=1, require_equals = true, value_delimiter = ',')]
    #[serde(default)]
    pub decomp: Option<Vec<String>>,
}

fn check_cmd(ctx: &Ctx, a: ScenarioCheckArgs) -> crate::Result<(bool, Json)> {
    let ids = if a.ids.is_empty() { ids(&ctx.root) } else { a.ids.clone() };
    if ids.is_empty() {
        return Err(Error::invalid("no scenarios: write scenarios/<id>.toml"));
    }
    let decomp = a.decomp.as_ref().map(|w| crate::decomp::Replacement::new(&ctx.root, w)).transpose()?;
    let mut results = Vec::new();
    let mut ok = true;
    for id in ids {
        check_name("id", &id)?;
        let mut r = determinism(ctx, &id, Duration::from_secs(a.timeout))?;
        ok &= r["status"] == "deterministic";
        if let Some(d) = &decomp {
            let (same, c) = with_c(ctx, &id, None, d, Duration::from_secs(a.timeout))?;
            ok &= same;
            r["decomp"] = c;
        }
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
    let mut out = json!({"ok": ok, "scenarios": results});
    if let Some(d) = &decomp {
        out["decomp"] = json!({"functions": d.functions.iter().map(|f| &f.name).collect::<Vec<_>>(),
            "left_out": d.left_out.iter().map(|(f, why)| json!({"function": f, "why": why})).collect::<Vec<_>>()});
    }
    Ok((ok, out))
}

impl Op for ScenarioCheckArgs {
    fn exec(self, ctx: &Ctx) -> Outcome {
        Outcome::report(check_cmd(ctx, self))
    }
}

#[derive(clap::Args, Clone, Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct ScenarioShowArgs {
    /// The scenario: `scenarios/<ID>.toml`.
    pub id: String,
    /// The recording: `scenarios/<ID>/<NAME>.jsonl`.
    #[arg(long)]
    pub name: String,
    /// Quantities to show, comma-separated; default all.
    #[arg(long, value_delimiter = ',')]
    #[serde(default)]
    pub quantities: Vec<String>,
    /// Ticks to show, inclusive: `A..B`, `A..`, `..B` or `N`.
    #[arg(long)]
    #[serde(default)]
    pub ticks: Option<String>,
    /// Values as recorded (a fixed's integer, a float's bits).
    #[arg(long)]
    #[serde(default)]
    pub raw: bool,
    /// Plain-text table instead of JSON.
    #[arg(long)]
    #[serde(default)]
    pub text: bool,
    /// Up to 8 of the shown ticks.
    #[arg(long)]
    #[serde(default)]
    pub screens: bool,
}

/// How many screens `show --screens` gives, spread over the ticks shown.
const SCREENS: usize = 8;

/// `A..B`, `A..`, `..B` or `N`, inclusive.
fn tick_range(s: &str) -> crate::Result<(u64, u64)> {
    let bad = || Error::invalid(format!("--ticks {s:?}: expected A..B, A.., ..B or N"));
    let num = |t: &str, d: u64| if t.is_empty() { Ok(d) } else { t.parse::<u64>().map_err(|_| bad()) };
    let (a, b) = match s.split_once("..") {
        Some((a, b)) => (num(a, 0)?, num(b, u64::MAX)?),
        None => {
            let n = num(s, 0)?;
            (n, n)
        }
    };
    if b < a {
        return Err(bad());
    }
    Ok((a, b))
}

/// A fixed's raw integer as its exact decimal: `raw / 2^frac`.
fn fixed_decimal(raw: i128, frac: u32) -> String {
    if frac == 0 {
        return raw.to_string();
    }
    // frac × 5^frac < 10^frac fits a u128 up to 38 fraction digits.
    if frac > 38 {
        return format!("{}", raw as f64 / 2f64.powi(frac as i32));
    }
    let abs = raw.unsigned_abs();
    let (int, part) = (abs >> frac, abs & ((1u128 << frac) - 1));
    let digits = format!("{:0w$}", part * 5u128.pow(frac), w = frac as usize);
    let digits = digits.trim_end_matches('0');
    let sign = if raw < 0 { "-" } else { "" };
    if digits.is_empty() { format!("{sign}{int}") } else { format!("{sign}{int}.{digits}") }
}

/// A float's bit pattern as its number (80-bit ones stay bits).
fn float_number(bits: u32, raw: i128) -> Option<f64> {
    match bits {
        32 => Some(f32::from_bits(raw as u32) as f64),
        64 => Some(f64::from_bits(raw as u64)),
        _ => None,
    }
}

/// A recorded value as shown: a number in the quantity's own units, or as
/// recorded with `raw`.
fn shown(t: &Type, v: &psx_scenario::Value, raw: bool) -> Json {
    use psx_scenario::Value as V;
    match (t, v) {
        (_, V::Bool(b)) => json!(b),
        (_, V::Variant(n)) => json!(n),
        (Type::Fixed { frac_bits, .. }, V::Int(n)) if !raw => json!(fixed_decimal(*n, *frac_bits)),
        (Type::Float { bits }, V::Int(n)) if !raw => match float_number(*bits, *n) {
            Some(f) => json!(f),
            None => json!(n.to_string()),
        },
        (_, V::Int(n)) => i64::try_from(*n).map(|n| json!(n)).unwrap_or_else(|_| json!(n.to_string())),
        (_, v) => json!(v),
    }
}

/// Whether a quantity gets a delta: an int, a fixed, or a float shown as a number.
fn moves(t: &Type, raw: bool) -> bool {
    matches!(t, Type::Int { .. } | Type::Fixed { .. }) || matches!(t, Type::Float { bits: 32 | 64 } if !raw)
}

/// How a numeric quantity moved from `prev` to `v`, shown as its values are.
fn delta(t: &Type, prev: &psx_scenario::Value, v: &psx_scenario::Value, raw: bool) -> Option<Json> {
    use psx_scenario::Value as V;
    let (V::Int(a), V::Int(b)) = (prev, v) else { return None };
    match t {
        Type::Int { .. } | Type::Fixed { .. } => Some(shown(t, &V::Int(b - a), raw)),
        Type::Float { bits } if !raw => Some(json!(float_number(*bits, *b)? - float_number(*bits, *a)?)),
        _ => None,
    }
}

fn show_cmd(ctx: &Ctx, a: &ScenarioShowArgs) -> crate::Result<Json> {
    check_name("id", &a.id)?;
    check_name("name", &a.name)?;
    let path = recording_path(&ctx.root, &a.id, &a.name);
    let text = std::fs::read_to_string(&path).map_err(|e| Error::io(&path, e))?;
    let rec = Recording::parse(&text).map_err(|e| Error::malformed(format!("{}: {e}", rel(&ctx.root, &path))))?;
    let mut types = BTreeMap::new();
    for (n, f) in &rec.header.quantities {
        let t = f.ty().map_err(|e| Error::malformed(format!("{}: quantities.{n}: {e}", rel(&ctx.root, &path))))?;
        types.insert(n.clone(), t);
    }
    let names: Vec<String> =
        if a.quantities.is_empty() { types.keys().cloned().collect() } else { a.quantities.clone() };
    if let Some(n) = names.iter().find(|n| !types.contains_key(*n)) {
        let have: Vec<&str> = types.keys().map(String::as_str).collect();
        return Err(Error::invalid(format!("{n} is not recorded here; the recording holds {}", have.join(", "))));
    }
    let (lo, hi) = a.ticks.as_deref().map(tick_range).transpose()?.unwrap_or((0, u64::MAX));

    let initial = Full { values: rec.header.initial.clone(), ..Full::default() };
    let full = rec.expand();
    let ticks = full.len() as u64;
    // Only a recording with the pad in analog mode shows the sticks.
    let sticks = full.iter().any(|f| f.analog);
    let mut columns = vec![json!("tick"), json!("field"), json!("pad")];
    if sticks {
        columns.push(json!("sticks"));
    }
    for n in &names {
        columns.push(json!(n));
        if moves(&types[n], a.raw) {
            columns.push(json!(format!("Δ{n}")));
        }
    }
    let mut rows = Vec::new();
    let mut prev = &initial;
    for f in std::iter::once(&initial).chain(&full) {
        if (lo..=hi).contains(&f.tick) {
            let mut row =
                vec![json!(f.tick), if f.tick == 0 { Json::Null } else { json!(f.field) }, json!(f.pad.join("+"))];
            if sticks {
                row.push(json!(if f.analog { off_centre(f.sticks) } else { "digital".into() }));
            }
            for n in &names {
                let t = &types[n];
                let v = f.values.get(n);
                row.push(v.map(|v| shown(t, v, a.raw)).unwrap_or(Json::Null));
                if moves(t, a.raw) {
                    let d = match (prev.values.get(n), v) {
                        (Some(p), Some(v)) if f.tick > 0 => delta(t, p, v, a.raw),
                        _ => None,
                    };
                    row.push(d.unwrap_or(Json::Null));
                }
            }
            rows.push(Json::Array(row));
        }
        prev = f;
    }
    let units: serde_json::Map<String, Json> =
        names.iter().filter_map(|n| Some((n.clone(), json!(rec.header.quantities[n].unit.clone()?)))).collect();
    let mut out = json!({
        "recording": rel(&ctx.root, &path),
        "scenario": rec.header.scenario,
        "ticks": ticks,
        "shown": rows.len(),
        "values": if a.raw { "as recorded: a fixed's integer, a float's bits" }
                  else { "in the quantity's units: a fixed as its exact decimal (raw: as recorded)" },
        "deltas": "Δ is the change since the tick before; tick 0 is the initial moment, the end of the first tick, with the pad released",
        "columns": columns,
        "rows": rows,
    });
    if sticks {
        out["sticks"] = json!(
            "the stick axes away from centre (128), as the game read them: 0 is left or up, 255 right or down; digital: the pad not in analog mode, so it sends no sticks"
        );
    }
    if let Some(d) = &rec.header.description {
        out["description"] = json!(d);
    }
    if !units.is_empty() {
        out["units"] = Json::Object(units);
    }
    if a.text {
        out["text"] = json!(table(&out));
    }
    if a.screens {
        let mut shown: Vec<u64> = out["rows"].as_array().into_iter().flatten().filter_map(|r| r[0].as_u64()).collect();
        // The original's replay draws after each tick it runs: tick 0, the
        // initial moment, only when nothing else is shown.
        if shown.len() > 1 {
            shown.retain(|&t| t > 0);
        }
        let n = shown.len().min(SCREENS);
        // Spread over them, the first and the last among them.
        let ticks: Vec<u64> = (0..n).map(|i| shown[if n == 1 { 0 } else { i * (shown.len() - 1) / (n - 1) }]).collect();
        let dir = ctx.root.join(".psx/runs").join(format!("{}-{}-screens", crate::now_ms(), std::process::id()));
        out["screens"] = crate::web::screens(ctx, &a.id, &a.name, &ticks, &dir)?;
    }
    Ok(out)
}

/// The axes of `sticks` (lx, ly, rx, ry) away from centre, as `lx=143 ry=0`.
fn off_centre(sticks: [u8; 4]) -> String {
    let names = ["lx", "ly", "rx", "ry"];
    let moved = names.iter().zip(sticks).filter(|(_, v)| *v != format::CENTRED[0]);
    moved.map(|(n, v)| format!("{n}={v}")).collect::<Vec<_>>().join(" ")
}

/// `show`'s columns and rows as aligned text.
fn table(out: &Json) -> String {
    let cell = |v: &Json| match v {
        Json::Null => "-".to_string(),
        Json::String(s) if s.is_empty() => "-".to_string(),
        Json::String(s) => s.clone(),
        v => v.to_string(),
    };
    let head: Vec<String> = out["columns"].as_array().into_iter().flatten().map(cell).collect();
    let body: Vec<Vec<String>> = out["rows"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|r| r.as_array().into_iter().flatten().map(cell).collect())
        .collect();
    let width: Vec<usize> = (0..head.len())
        .map(|i| std::iter::once(&head).chain(&body).map(|r| r[i].chars().count()).max().unwrap_or(0))
        .collect();
    let mut text = format!(
        "; {}: {} ticks; {}\n",
        out["recording"].as_str().unwrap_or(""),
        out["ticks"],
        out["values"].as_str().unwrap_or("")
    );
    for r in std::iter::once(&head).chain(&body) {
        let line: Vec<String> = r.iter().zip(&width).map(|(c, w)| format!("{c:>w$}")).collect();
        text += line.join("  ").trim_end();
        text += "\n";
    }
    text
}

impl Op for ScenarioShowArgs {
    fn exec(self, ctx: &Ctx) -> Outcome {
        let text = self.text;
        Outcome::result_text(show_cmd(ctx, &self), text)
    }
}

/// What stops one scenario's recordings from counting toward the goal:
/// problems in its files, no captured recording, or no passing determinism
/// check of the files as they are.
pub fn unready(root: &Path, id: &str) -> Vec<String> {
    let l = Loaded::load(root, id);
    let mut out = l.problems.clone();
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

/// What a recording whose original took its input late asks for, by
/// what it was made with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Late {
    /// fixes/ holds no C: the delay is to be found and fixed.
    Unfixed,
    /// Made without fixes/ as it is now: recorded again, it takes them.
    Before,
    /// Made with fixes/ as it is and still late: the fix does not work.
    Fails,
}

/// The recordings of `scenarios` in which the original's own driver kept
/// the tick's input from the game (`late_input`), each with how often and
/// what it asks for.
pub fn scenarios_late_input(root: &Path, scenarios: &[String]) -> Vec<(Late, String)> {
    let now = crate::decomp::fixes_hash(root);
    scenarios.iter().flat_map(|s| late_input(root, s, now.as_deref())).collect()
}

fn late_input(root: &Path, id: &str, fixes_now: Option<&str>) -> Vec<(Late, String)> {
    let mut out = Vec::new();
    let l = Loaded::load(root, id);
    for (_, f, r) in &l.recordings {
        let Ok(r) = r else { continue };
        let details = &r.header.producer.details;
        let Some(late) = details.get("adjusted").and_then(|a| a.get("late_input")) else {
            continue;
        };
        let ticks: Vec<String> =
            late["ticks"].as_array().into_iter().flatten().take(5).map(|t| t.to_string()).collect();
        let mut line = format!(
            "{}: the game ran {} ticks on input a tick old (ticks {}{})",
            rel(root, f),
            late["tick_count"],
            ticks.join(", "),
            if late["tick_count"].as_u64().unwrap_or(0) > ticks.len() as u64 { ", .." } else { "" }
        );
        let made = details.get("fix").and_then(|f| f["sources"].as_str());
        let kind = match fixes_now {
            None => Late::Unfixed,
            Some(now) if made != Some(now) => {
                line.push_str(if made.is_none() {
                    ", made without fixes/"
                } else {
                    ", made with fixes/ as it was before"
                });
                Late::Before
            }
            Some(_) => {
                let calls: Vec<String> = details
                    .get("adjusted")
                    .and_then(|a| a["fix_calls"].as_array())
                    .into_iter()
                    .flatten()
                    .map(|c| match c["calls"].as_u64() {
                        Some(0) | None => format!("{} never ran", c["name"].as_str().unwrap_or("?")),
                        Some(n) => format!("{} ran {n} times", c["name"].as_str().unwrap_or("?")),
                    })
                    .collect();
                line.push_str(&format!(", made with fixes/ as it is ({})", calls.join(", ")));
                Late::Fails
            }
        };
        out.push((kind, line));
    }
    out
}

/// What keeps `scenarios`' recordings from counting: each scenario's
/// problems and determinism check.
pub fn scenarios_unready(root: &Path, scenarios: &[String]) -> Vec<String> {
    scenarios.iter().flat_map(|s| unready(root, s)).collect()
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
            "moment = \"standing\"\nchain = \"notes/states/start.json\"\nkind = \"level\"\n[compare]\nx = 0\n",
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
                "glossary.toml: no longer read: the glossary is Rust, glossary/ and glossary/binding/ (psx guide, Recordings)",
                "glossary/binding: not written yet (a cdylib package named glossary-binding, a member of the workspace; vendor/psx-capture's crate docs show it)"
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

    /// A game that polls its pad from an interrupt handler (here entered by
    /// `syscall`) into a buffer at 0x80020000, and reads the buffer twice
    /// in each tick, with a poll between: x is the first read, dead whether
    /// the second read saw anything else. Ticks end at the store to
    /// 0x80020010 by 0x80010020.
    struct TwoReads;

    impl psx_capture::Glossary for TwoReads {
        type Glue = Glue;
        const TITLE: &'static str = "TwoReads";
        const IMAGE_SHA256: &'static str = "0000000000000000000000000000000000000000000000000000000000000000";

        fn tick(&self, t: &mut psx_capture::Tick) {
            t.store(0x8002_0010, 4).by(0x8001_0020);
        }

        fn quantities(&self, m: &psx_capture::Machine) -> Q {
            Q { x: m.u16(0x8002_0030) as i32, dead: m.u16(0x8002_0030) != m.u16(0x8002_0034) }
        }
    }

    #[allow(unsafe_code)]
    unsafe extern "C" fn two_reads_call(what: u32, host: *const psx_capture::abi::Host) -> i32 {
        // SAFETY: the caller's.
        unsafe { psx_capture::abi::call(&TwoReads, what, host) }
    }

    /// TwoReads' game, keeping its buffer as `keep` says, recorded for
    /// `fields` fields with CROSS pressed and released at each field.
    fn two_reads(keep: psx_core::padflow::test_program::Keep, adjust: Adjustments, fields: u32) -> Recorded {
        let (mut m, b, types, events) = two_reads_game(keep, fields);
        let q = Quantities { binding: &b, types, kept: vec!["x".into(), "dead".into()], adjust };
        record(&mut m, &q, fields, &events, None, Duration::from_secs(120), None, None, None)
    }

    /// TwoReads' game, its binding and quantities' types, and input for `fields` fields.
    #[allow(clippy::type_complexity)]
    fn two_reads_game(
        keep: psx_core::padflow::test_program::Keep,
        fields: u32,
    ) -> (Machine, Plugin<Described>, BTreeMap<String, Type>, Vec<crate::replay::Input>) {
        use psx_core::padflow::test_program::asm::*;
        const S1: u32 = 17;
        const T0: u32 = 8;
        const T1: u32 = 9;
        let main = [
            SYSCALL,        // 80010000: a poll
            lhu(T0, 0, S1), // the first read
            NOP,
            sh(T0, 0x30, S1),
            SYSCALL,        // 80010010: another poll
            lhu(T1, 0, S1), // the second read
            NOP,
            sh(T1, 0x34, S1),
            i(0x2B, S1, T0, 0x10), // 80010020: sw t0, 0x10(s1): the tick ends
            j(0x8001_0000),
            NOP,
            NOP,
            lui(S1, 0x8002), // 80010030
            j(0x8001_0000),
            NOP,
        ];
        let m = psx_core::padflow::test_program::with_main(keep, &main, 0x8001_0030);
        let b: Plugin<Described> = Plugin::new(two_reads_call, None).unwrap();
        let types: BTreeMap<String, Type> =
            [("x".to_string(), Type::Int { bits: 32, signed: true }), ("dead".to_string(), Type::Bool)].into();
        let cross = psx_core::sio0::button::CROSS;
        let events: Vec<crate::replay::Input> = (0..fields as u64)
            .map(|f| crate::replay::Input {
                frame: f,
                buttons: if f % 2 == 0 { cross as u32 } else { 0 },
                sticks: [128; 4],
            })
            .collect();
        (m, b, types, events)
    }

    /// Samples per tick, as run's sample observer takes them.
    struct TickSamples(crate::ram::Sampler);

    impl crate::session::Observer for TickSamples {
        fn tick(&mut self, rel: u64, m: &mut Machine) -> crate::session::Flow {
            self.0.tick(rel, m);
            crate::session::Flow::Go
        }
        fn finish(self: Box<Self>, _m: &Machine, out: &mut crate::session::Out) -> crate::Result<()> {
            out.section("rows", json!(self.0.out));
            Ok(())
        }
    }

    /// run from a recording: the session, driven by the recording's ticks
    /// under its adjustments, samples on every tick what was recorded there.
    #[test]
    fn a_latched_recording_run_again_samples_what_it_recorded() {
        use crate::ram::{Column, SamplePer, Sampler};
        use psx_core::padflow::test_program::Keep;
        let fields = 30;
        let recorded = two_reads(Keep::Bytes, Adjustments::default_set(), fields);
        assert!(recorded.ticks.len() > 100, "{} ticks", recorded.ticks.len());
        let (m, b, types, events) = two_reads_game(Keep::Bytes, fields);
        let pads = held_by_tick(&events, &recorded.ticks);
        let tick = b.described().tick.clone();
        let cols = vec![Column::Quantity("x".into()), Column::Quantity("dead".into())];
        let sampler = Sampler::new(cols, 1, SamplePer::Tick, Some(crate::ram::Quantities::new(b, types)));
        let ticks = Box::new(crate::session::Ticks { recording: "two_reads/latched".into(), pads });
        let mut s = crate::session::Session::new(m, 0, ticks);
        crate::replay::apply(&mut s.m, &crate::replay::Input { frame: 0, buttons: 0, sticks: [128; 4] });
        s.adjust = Some(Adjustments::default_set().start(&mut s.m));
        s.tick = Some(tick);
        let (started, deadline) = (Instant::now(), Duration::from_secs(120));
        assert_eq!(s.lead_in(s.m.bus.frames + fields as u64, started, deadline), crate::session::LeadIn::Ticked);
        s.observe(TickSamples(sampler));
        let limit = s.start + fields as u64 * 2;
        let ended = s.advance(limit, started, deadline).unwrap();
        assert_eq!(ended, crate::session::Ended::Stopped("recording_ended"));
        assert_eq!(s.ticks, recorded.ticks.len() as u64);
        let mut want = String::from("run_frame,tick,x,dead\n");
        for t in &recorded.ticks {
            let (x, dead) = match (&t.values["x"], &t.values["dead"]) {
                (psx_scenario::Value::Int(x), psx_scenario::Value::Bool(d)) => (*x, *d),
                v => panic!("{v:?}"),
            };
            want += &format!("{},{},{x},{dead}\n", t.field, t.tick);
        }
        let (mut files, mut report) = (serde_json::Map::new(), serde_json::Map::new());
        let dir = std::env::temp_dir();
        let mut out =
            crate::session::Out { dir: &dir, files: &mut files, report: &mut report, start: s.start, termination: "" };
        s.finish(&mut out).unwrap();
        // The absolute frame aside, the rows are the recording's ticks.
        let rows = report["rows"].as_str().unwrap().lines().map(|l| {
            let mut c: Vec<&str> = l.split(',').collect();
            c.remove(1);
            c.join(",") + "\n"
        });
        assert_eq!(rows.collect::<String>(), want);
        // The adjustments did what they do in scenario check's replay, which
        // also ends where the last tick does.
        let (mut m, b, types, _) = two_reads_game(Keep::Bytes, fields);
        let q = Quantities {
            binding: &b,
            types,
            kept: vec!["x".into(), "dead".into()],
            adjust: Adjustments::default_set(),
        };
        let pads = held_by_tick(&events, &recorded.ticks);
        let replayed = record(&mut m, &q, fields, &events, Some(&pads), deadline, None, None, None);
        assert_eq!(replayed.ticks.len(), recorded.ticks.len());
        assert_eq!(s.adjust.as_ref().unwrap().report(&s.m), replayed.adjusted);
        assert_eq!(s.m.bus.ram, m.bus.ram);
    }

    fn reads(r: &Recorded) -> Vec<(i128, bool)> {
        use psx_scenario::Value as V;
        r.ticks
            .iter()
            .map(|t| match (&t.values["x"], &t.values["dead"]) {
                (V::Int(x), V::Bool(d)) => (*x, *d),
                v => panic!("{v:?}"),
            })
            .collect()
    }

    #[test]
    fn latched_every_read_in_a_tick_sees_the_tick_s_input() {
        use psx_core::padflow::test_program::Keep;
        let cross = psx_core::sio0::button::CROSS as i128;
        // Stock, a poll between the two reads sometimes brings new input.
        let stock = two_reads(Keep::Bytes, Adjustments::stock(), 30);
        assert_eq!(stock.refusal_count, 0, "{:?}", stock.refusals);
        assert!(reads(&stock).iter().any(|(_, d)| *d), "the stock game never read two pads in a tick");
        assert!(stock.adjusted.is_none());
        // Latched, never; and what the game read is the tick's recorded pad
        // (the buffer keeps the wire's bytes, inverted).
        let latched = two_reads(Keep::Bytes, Adjustments::default_set(), 30);
        assert_eq!(latched.refusal_count, 0, "{:?}", latched.refusals);
        let r = reads(&latched);
        assert!(r.len() > 100, "{} ticks", r.len());
        assert!(r.iter().all(|(_, d)| !d), "a latched tick read two pads");
        for (t, (x, _)) in latched.ticks.iter().zip(&r) {
            let held = if t.pad.is_empty() { 0 } else { cross };
            assert_eq!(*x, !held & 0xFFFF, "tick {}: pad {:?}", t.tick, t.pad);
        }
        assert!(latched.ticks.iter().any(|t| t.pad.is_empty()) && latched.ticks.iter().any(|t| !t.pad.is_empty()));
        let a = latched.adjusted.as_ref().unwrap();
        assert_eq!(a["lag_frames"], 0);
        // Its two loads a tick of the latched byte, none of them stale.
        assert!(a["buffer_reads"].as_u64().unwrap() >= 2 * r.len() as u64, "{a}");
        assert!(a.get("stale_reads").is_none(), "{a}");
        assert!(a.get("late_input").is_none(), "{a}");
        assert_eq!(a["buffer"][0], json!({"address": "0x80020000", "byte": "buttons_lo", "mask": "0x00"}));
        assert_eq!(a["by"], "the pad buffer and the controller");
    }

    /// A game whose work for a tick takes `n` passes of a loop, then waits
    /// for vblank, recorded for `fields` fields with Demo's binding.
    fn slow_game(n: u32, adjust: Adjustments, fields: u32) -> Recorded {
        use psx_core::padflow::test_program::asm::*;
        const T2: u32 = 10;
        const T3: u32 = 11;
        const T4: u32 = 12;
        const S0: u32 = 16;
        const S1: u32 = 17;
        let main = [
            ori(T2, 0, n),          // 80010000: the work
            addiu(T2, T2, 0xFFFF),  // 80010004: one pass
            i(0x05, T2, 0, 0xFFFE), // bne t2, zero, the pass
            NOP,
            i(0x2B, S1, 8, 0x10), // 80010010: sw t0, 0x10(s1): the tick ends
            addiu(T4, 0, 0xFFFE),
            i(0x2B, S0, T4, 0x70), // clear vblank in I_STAT
            i(0x23, S0, T3, 0x70), // 8001001C: lw t3, I_STAT
            NOP,
            andi(T3, T3, 1),
            i(0x04, T3, 0, 0xFFFC), // beq t3, zero, 8001001C: wait for the next vblank
            NOP,
            j(0x8001_0000),
            NOP,
            NOP,
            NOP,
            lui(S1, 0x8002), // 80010040
            lui(S0, 0x1F80),
            ori(S0, S0, 0x1000),
            j(0x8001_0000),
            NOP,
        ];
        let mut m = Machine::new(vec![0; 0x8_0000]);
        psx_core::padflow::test_program::load(&mut m, 0x8001_0000, &main);
        m.cpu.set_pc(0x8001_0040);
        let b: Plugin<Described> = Plugin::new(demo_call, None).unwrap();
        let types: BTreeMap<String, Type> =
            [("x".to_string(), Type::Int { bits: 32, signed: true }), ("dead".to_string(), Type::Bool)].into();
        let q = Quantities { binding: &b, types, kept: vec!["x".into()], adjust };
        record(&mut m, &q, fields, &[], None, Duration::from_secs(120), None, None, None)
    }

    /// A game that calls a function returning 7 each tick and keeps what
    /// it returned as x, recorded with Demo's binding and `adjust`.
    fn calls_seven(adjust: Adjustments) -> Recorded {
        use psx_core::padflow::test_program::asm::*;
        const V0: u32 = 2;
        const S1: u32 = 17;
        const RA: u32 = 31;
        let main = [
            0x0C00_400C, // 80010000: jal 80010030
            NOP,
            i(0x2B, S1, V0, 0), // sw v0, 0(s1): x
            NOP,
            i(0x2B, S1, V0, 0x10), // 80010010: the tick ends
            j(0x8001_0000),
            NOP,
            NOP,
            NOP,
            NOP,
            NOP,
            NOP,
            addiu(V0, 0, 7), // 80010030: the function
            jr(RA),
            NOP,
            NOP,
            lui(S1, 0x8002), // 80010040
            j(0x8001_0000),
            NOP,
        ];
        let mut m = Machine::new(vec![0; 0x8_0000]);
        psx_core::padflow::test_program::load(&mut m, 0x8001_0000, &main);
        m.cpu.set_pc(0x8001_0040);
        let b: Plugin<Described> = Plugin::new(demo_call, None).unwrap();
        let types: BTreeMap<String, Type> =
            [("x".to_string(), Type::Int { bits: 32, signed: true }), ("dead".to_string(), Type::Bool)].into();
        let q = Quantities { binding: &b, types, kept: vec!["x".into()], adjust };
        record(&mut m, &q, 1, &[], None, Duration::from_secs(60), None, None, None)
    }

    #[test]
    fn the_quantities_differences_name() {
        let d = ["initial x: recorded 1, the replay 2", "tick 40 y: recorded 1, the replay 2", "tick 41 x: ..."]
            .map(String::from);
        assert_eq!(super::differing(&d), ["x", "y"]);
    }

    #[test]
    fn a_function_turned_off_returns_its_value_at_once() {
        use psx_scenario::Value as V;
        let stock = calls_seven(Adjustments::stock());
        assert!(stock.ticks.iter().all(|t| t.values["x"] == V::Int(7)));
        let table = [("0x80010030".to_string(), 42i64)].into();
        let off = Adjustments { off: crate::adjust::Off::resolve(&table).unwrap(), ..Adjustments::stock() };
        let r = calls_seven(off);
        assert!(!r.ticks.is_empty() && r.ticks.iter().all(|t| t.values["x"] == V::Int(42)));
        let calls = &r.adjusted.as_ref().unwrap()["off_calls"][0];
        assert_eq!(calls["name"], "0x80010030");
        assert!(calls["calls"].as_u64().unwrap() >= r.ticks.len() as u64);
    }

    #[test]
    fn steady_ticks_do_not_overrun_their_fields() {
        // About a field and a half of work on the console: each tick takes two.
        let n = 56_000;
        let stock = slow_game(n, Adjustments::stock(), 12);
        let h = fields_per_tick(&stock.ticks);
        assert_eq!(h.as_object().unwrap().keys().collect::<Vec<_>>(), ["2"], "{h}");
        let steady = slow_game(n, Adjustments { steady: true, ..Adjustments::stock() }, 12);
        let h = fields_per_tick(&steady.ticks);
        assert_eq!(h.as_object().unwrap().keys().collect::<Vec<_>>(), ["1"], "{h}");
        assert!(steady.ticks.len() > stock.ticks.len());
    }

    #[test]
    fn a_game_that_never_reads_its_pad_is_all_lag_frames() {
        let r = demo_recorded(Adjustments::default_set());
        let a = r.adjusted.as_ref().unwrap();
        assert_eq!(a["buffer"], json!([]));
        assert!(a["by"].as_str().unwrap().contains("no pad buffer found"));
        assert_eq!(a["lag_frames"], a["ticks"]);
        assert_eq!(a["ticks"].as_u64(), Some(r.ticks.len() as u64));
        assert!(a.get("ticks_ended_in_handler").is_none());
    }

    #[test]
    fn latched_through_an_inverted_buffer() {
        use psx_core::padflow::test_program::Keep;
        let cross = psx_core::sio0::button::CROSS as i128;
        let latched = two_reads(Keep::Inverted, Adjustments::default_set(), 20);
        let r = reads(&latched);
        assert!(r.iter().all(|(_, d)| !d), "a latched tick read two pads");
        // This game keeps the buttons held, as bits set.
        for (t, (x, _)) in latched.ticks.iter().zip(&r) {
            assert_eq!(*x, if t.pad.is_empty() { 0 } else { cross }, "tick {}", t.tick);
        }
        let a = latched.adjusted.as_ref().unwrap();
        assert_eq!(a["buffer"][1]["mask"], "0xFF");
        assert!(a["buffer_reads"].as_u64().unwrap() > 0 && a.get("stale_reads").is_none(), "{a}");
        assert!(a.get("late_input").is_none(), "{a}");
    }

    /// A game that polls every other tick (the run up to the first tick's
    /// end, tick 0, polls), each tick spinning for a few fields, and with
    /// `reads` loads its buffer where the tick ends; recorded latched for
    /// 60 fields with CROSS held for 3 and released for 3.
    fn skipping(reads: bool) -> Recorded {
        use psx_core::padflow::test_program::{Keep, asm::*};
        const S1: u32 = 17;
        const T0: u32 = 8;
        const S2: u32 = 18;
        const T3: u32 = 11;
        let main = [
            i(0x0E, T3, T3, 1), // 80010000: xori t3, t3, 1
            beq(T3, 0, 1, 4),   // no poll in every other tick
            lui(S2, 0x0002),
            SYSCALL,
            addiu(S2, S2, 0xFFFF),          // 80010010
            i(0x05, S2, 0, (-2i32) as u32), // bne t2, zero, back
            NOP,
            if reads { lhu(T0, 0, S1) } else { NOP },
            i(0x2B, S1, T0, 0x10), // 80010020: sw t0, 0x10(s1): the tick ends
            j(0x8001_0000),
            NOP,
            NOP,
            lui(S1, 0x8002), // 80010030
            j(0x8001_0000),
            NOP,
        ];
        let mut m = psx_core::padflow::test_program::with_main(Keep::Bytes, &main, 0x8001_0030);
        let b: Plugin<Described> = Plugin::new(two_reads_call, None).unwrap();
        let types: BTreeMap<String, Type> =
            [("x".to_string(), Type::Int { bits: 32, signed: true }), ("dead".to_string(), Type::Bool)].into();
        let adjust = Adjustments { latch: true, ..Adjustments::stock() };
        let q = Quantities { binding: &b, types, kept: vec!["x".into(), "dead".into()], adjust };
        let cross = psx_core::sio0::button::CROSS as u32;
        let events: Vec<crate::replay::Input> = (0..60)
            .map(|f| crate::replay::Input { frame: f, buttons: if f % 6 < 3 { cross } else { 0 }, sticks: [128; 4] })
            .collect();
        let r = record(&mut m, &q, 60, &events, None, Duration::from_secs(120), None, None, None);
        assert_eq!(r.refusal_count, 0, "{:?}", r.refusals);
        r
    }

    #[test]
    fn a_game_that_skips_polls_is_reported_late() {
        let r = skipping(false);
        let a = r.adjusted.as_ref().unwrap();
        let late = &a["late_input"];
        assert!(late["fields"].as_u64().unwrap() > 0, "{a}");
        // Only ticks without a poll: the odd ones.
        let ticks: Vec<u64> = late["ticks"].as_array().unwrap().iter().map(|t| t.as_u64().unwrap()).collect();
        assert!(ticks.len() >= 3 && ticks.iter().all(|t| t % 2 == 1), "{a}");
        assert_eq!(late["tick_count"], ticks.len());
        // The same game reading its buffer before the tick ends sees the
        // latch: nothing late.
        let r = skipping(true);
        let a = r.adjusted.as_ref().unwrap();
        assert!(a["buffer_reads"].as_u64().unwrap() > 0 && a.get("stale_reads").is_none(), "{a}");
        assert!(a.get("late_input").is_none(), "{a}");
    }

    /// A loop for Demo's binding that counts x up a tick at a time from
    /// what RAM holds (a `nop` after the load, so the count reads it),
    /// recorded with `adjust`.
    fn demo_recorded(adjust: Adjustments) -> Recorded {
        let b: Plugin<Described> = Plugin::new(demo_call, None).unwrap();
        let mut m = Machine::new(vec![0; 0x8_0000]);
        #[rustfmt::skip]
        let loop_ = [
            0x8D28_0000u32, // 80010000: lw t0, 0(t1)
            0x0000_0000, // nop
            0x2508_0001, // addiu t0, t0, 1
            0xAD28_0000, // sw t0, 0(t1): x
            0xAD28_0010, // 80010010: sw t0, 0x10(t1): the tick ends
            0x0800_4000, // j 0x80010000
            0x0000_0000, // nop
            0x0000_0000,
            0x3C09_8002, // 80010020: lui t1, 0x8002
            0x0800_4000, // j 0x80010000
            0x0000_0000, // nop
        ];
        for (i, w) in loop_.iter().enumerate() {
            m.bus.ram[0x1_0000 + 4 * i..][..4].copy_from_slice(&w.to_le_bytes());
        }
        m.cpu.set_pc(0x8001_0020);
        let types: BTreeMap<String, Type> =
            [("x".to_string(), Type::Int { bits: 32, signed: true }), ("dead".to_string(), Type::Bool)].into();
        let q = Quantities { binding: &b, types, kept: vec!["x".into()], adjust };
        record(&mut m, &q, 1, &[], None, Duration::from_secs(60), None, None, None)
    }

    #[test]
    fn an_adjustment_is_measured_against_the_stock_game() {
        use crate::adjust::TestAdjust;
        let stock = demo_recorded(Adjustments::stock());
        let nothing = demo_recorded(Adjustments { test: Some(TestAdjust::Nothing), ..Adjustments::stock() });
        assert_eq!(adjusted_differences(&stock, &nothing), Vec::<Json>::new());
        // One more on x when tick 3 ends: tick 4 is the first to differ.
        let nudged = demo_recorded(Adjustments {
            test: Some(TestAdjust::Nudge { tick: 3, addr: 0x8002_0000 }),
            ..Adjustments::stock()
        });
        let d = adjusted_differences(&stock, &nudged);
        assert_eq!(d.len(), 1, "{d:?}");
        assert_eq!((d[0]["quantity"].as_str(), d[0]["tick"].as_u64()), (Some("x"), Some(4)));
        assert_eq!(d[0]["adjusted"].as_i64(), d[0]["stock"].as_i64().map(|v| v + 1));
    }

    /// adjust-check on a real game: a do-nothing adjustment measures the
    /// same, and adding 1 to a word when tick 3 ends is reported at tick 4.
    /// Needs a port workspace with a recorded scenario and the word to nudge
    /// (one of its compared quantities, which the game keeps from tick to
    /// tick): PSX_TEST_WORKSPACE=DIR PSX_TEST_SCENARIO=ID
    /// PSX_TEST_NUDGE=ADDR:QUANTITY cargo test -p psx --release
    /// adjust_check_on_a_game -- --ignored
    #[test]
    #[ignore]
    fn adjust_check_on_a_game() {
        use crate::adjust::TestAdjust;
        let (Some(root), Ok(id), Ok(nudge)) = (
            std::env::var_os("PSX_TEST_WORKSPACE"),
            std::env::var("PSX_TEST_SCENARIO"),
            std::env::var("PSX_TEST_NUDGE"),
        ) else {
            return;
        };
        let (addr, quantity) = nudge.split_once(':').expect("PSX_TEST_NUDGE=ADDR:QUANTITY");
        let addr = u32::from_str_radix(addr.trim_start_matches("0x"), 16).unwrap();
        let ctx = Ctx { root: PathBuf::from(root), cancel: None };
        let timeout = Duration::from_secs(600);
        let nothing = Adjustments { test: Some(TestAdjust::Nothing), ..Adjustments::stock() };
        let r = adjust_check(&ctx, &id, nothing, true, timeout).unwrap();
        assert_eq!(r["status"], "same", "{r:#}");
        let nudged = Adjustments { test: Some(TestAdjust::Nudge { tick: 3, addr }), ..Adjustments::stock() };
        let r = adjust_check(&ctx, &id, nudged, true, timeout).unwrap();
        assert_eq!(r["status"], "differs", "{r:#}");
        for rec in r["recordings"].as_array().unwrap() {
            let first = &rec["differences"][0];
            assert_eq!((first["quantity"].as_str(), first["tick"].as_u64()), (Some(quantity), Some(4)), "{rec:#}");
        }
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
        let q = Quantities { binding: &b, types, kept: vec!["x".into()], adjust: Adjustments::stock() };
        let mut c = Cover { tracer: tracer(&mut m), scripts: crate::script_cover::Scripts::new(&Default::default()) };
        let r = record(&mut m, &q, 1, &[], None, Duration::from_secs(60), Some(&mut c), None, None);
        let t = &c.tracer;
        assert_eq!((r.refusal_count, r.timed_out), (0, false), "{:?}", r.refusals);
        assert_eq!(r.initial, Some([("x".to_string(), psx_scenario::Value::Int(1))].into()));
        assert_eq!(r.ticks[0].values, [("x".to_string(), psx_scenario::Value::Int(2))].into());
        assert!(r.ticks.len() > 10, "{} ticks", r.ticks.len());
        // What the recording keeps for coverage: every word of the loop, at
        // least once per tick (the last pass stops partway).
        let words = crate::coverage::summed(t);
        assert_eq!(words.len(), loop_.len());
        assert_eq!((words[3].0, words[3].1), (0x8001_000C, loop_[3]));
        assert!(words.iter().all(|w| w.2 as usize >= r.ticks.len()), "{words:x?}");
    }

    /// A toy interpreter: its program is 8 bytes at 0x80030000, of which it
    /// runs the first 4 over and over; each byte is an `if`, taken when the
    /// byte is 0. The tick ends at the store the `if` skips.
    struct Toy;

    impl psx_capture::Glossary for Toy {
        type Glue = Glue;
        const TITLE: &'static str = "Toy";
        const IMAGE_SHA256: &'static str = "0000000000000000000000000000000000000000000000000000000000000000";

        fn tick(&self, t: &mut psx_capture::Tick) {
            t.store(0x8002_0010, 4).by(0x8001_0018);
        }

        fn quantities(&self, m: &psx_capture::Machine) -> Q {
            Q { x: m.i32(0x8002_0000), dead: false }
        }

        fn programs(&self, p: &mut psx_capture::Programs, _m: &psx_capture::Machine) {
            p.program("toy", 0x8003_0000, 8);
        }

        fn hooks(&self, h: &mut psx_capture::Hooks) {
            h.at(0x8001_0008).branch(0x8001_0010);
        }

        fn hook(&self, at: psx_capture::Hook, m: &psx_capture::Machine, s: &mut psx_capture::Script) {
            use psx_capture::reg::T2;
            match at {
                // The dispatch read the byte at t2. A hook is told its pc as
                // given, not folded.
                psx_capture::Hook::At(0x8001_0008) => s.site("toy", m.reg(T2) - 0x8003_0000),
                // The delay slot moved t2 on past the byte decided.
                psx_capture::Hook::Branch { pc: 0x8001_0010, taken } => {
                    s.way("toy", m.reg(T2) - 0x8003_0001, taken as u32, 2)
                }
                other => panic!("a hook at {other:?}, which the toy never asked for"),
            }
        }

        /// Its decoder: each byte below 9 is an `if`; 9 is data.
        fn map(&self, name: &str, code: &[u8], map: &mut psx_capture::Map) {
            assert_eq!((name, code), ("toy", &[1, 0, 1, 1, 9, 9, 9, 9][..]));
            for (i, _) in code.iter().enumerate().filter(|(_, b)| **b < 9) {
                map.statement(i as u32, i as u32 + 1).decision(i as u32, 2);
            }
            map.routine("main", 0, 4);
        }
    }

    #[allow(unsafe_code)]
    unsafe extern "C" fn toy_call(what: u32, host: *const psx_capture::abi::Host) -> i32 {
        // SAFETY: the caller's.
        unsafe { psx_capture::abi::call(&Toy, what, host) }
    }

    #[test]
    fn a_recording_keeps_what_the_binding_says_its_scripts_ran() {
        let b: Plugin<Described> = Plugin::new(toy_call, None).unwrap();
        assert_eq!(
            (b.described().hooks.at.clone(), b.described().hooks.branch.clone()),
            (vec![0x8001_0008], vec![0x8001_0010])
        );
        let mut m = Machine::new(vec![0; 0x8_0000]);
        #[rustfmt::skip]
        let interpreter = [
            0x3C09_8002, // lui t1, 0x8002
            0x3C0A_8003, // lui t2, 0x8003: the program
            0x9148_0000, // lbu t0, 0(t2): the dispatch
            0x0000_0000, // nop
            0x1100_0002, // beq t0, zero, +2: the `if`
            0x254A_0001, // addiu t2, t2, 1
            0xAD28_0010, // sw t0, 0x10(t1): the tick ends
            0x314B_0003, // andi t3, t2, 3
            0x1560_FFF9, // bne t3, zero, the dispatch
            0x0000_0000, // nop
            0x0800_4001, // j 0x80010004: from the start of the program
            0x0000_0000, // nop
        ];
        for (i, w) in interpreter.iter().enumerate() {
            m.bus.ram[0x1_0000 + 4 * i..][..4].copy_from_slice(&u32::to_le_bytes(*w));
        }
        m.bus.ram[0x3_0000..0x3_0008].copy_from_slice(&[1, 0, 1, 1, 9, 9, 9, 9]);
        m.cpu.set_pc(0x8001_0000);
        let types: BTreeMap<String, Type> =
            [("x".to_string(), Type::Int { bits: 32, signed: true }), ("dead".to_string(), Type::Bool)].into();
        let q = Quantities { binding: &b, types, kept: vec!["x".into()], adjust: Adjustments::stock() };
        let mut c = Cover { tracer: tracer(&mut m), scripts: crate::script_cover::Scripts::new(&b.described().hooks) };
        let r = record(&mut m, &q, 2, &[], None, Duration::from_secs(60), Some(&mut c), None, None);
        assert_eq!((r.refusal_count, r.timed_out), (0, false), "{:?}", r.refusals);
        assert!(c.scripts.problems().is_empty(), "{:?}", c.scripts.problems());
        let toy = &c.scripts.programs["toy"];
        assert_eq!(toy.bytes, Some(8));
        assert_eq!(toy.read, [0, 1, 2, 3].into(), "the last 4 bytes are never read");
        assert_eq!(toy.sites.keys().copied().collect::<Vec<_>>(), [0, 1, 2, 3]);
        let ways: Vec<(u32, Vec<u32>)> = toy
            .ways
            .iter()
            .map(|(&a, (of, w))| {
                assert_eq!(*of, 2);
                (a, w.iter().copied().collect())
            })
            .collect();
        assert_eq!(ways, [(0, vec![0]), (1, vec![1]), (2, vec![0]), (3, vec![0])]);
        let kept = c.scripts.kept().unwrap();
        assert_eq!(kept[0]["read"], json!([["0x0", "0x4"]]));
        assert_eq!(kept[0]["ways"][1], json!(["0x1", 2, [1]]));
        // The map, from the binding's decoder: positions, never bytes.
        assert_eq!(
            kept[0]["map"],
            json!({"statements": [["0x0", [1, 1, 1, 1]]], "decisions": [["0x0", 2], ["0x1", 2], ["0x2", 2], ["0x3", 2]],
                   "routines": [["main", "0x0", "0x4"]]})
        );
        // With it, the data bytes are no gap: only decisions' ways are.
        let mut merged = crate::script_cover::Merged::default();
        merged.add(&json!({ "programs": kept }), "toy");
        let (items, problems) =
            crate::script_cover::items(&merged, &[crate::script_cover::Stretch::parse("toy+0x0..0x8").unwrap()]);
        assert!(problems.is_empty(), "{problems:?}");
        let never: Vec<String> =
            items.iter().filter(|i| !i.never.is_empty()).map(|i| format!("{} {}", i.at, i.never)).collect();
        assert_eq!(
            never,
            [
                "toy+0x0 went way 1 of 2",
                "toy+0x1 went way 0 of 2",
                "toy+0x2 went way 1 of 2",
                "toy+0x3 went way 1 of 2"
            ]
        );
        assert!(items.iter().all(|i| i.routine.as_deref() == Some("main")));
    }

    #[test]
    fn run_samples_glossary_quantities_once_per_tick() {
        use crate::ram::{Column, Quantities, SamplePer, Sampler};
        let binding: Plugin<Described> = Plugin::new(demo_call, None).unwrap();
        let tick = binding.described().tick.clone();
        let types: BTreeMap<String, Type> =
            [("x".to_string(), Type::Int { bits: 32, signed: true }), ("dead".to_string(), Type::Bool)].into();
        let q = Quantities::new(binding, types);
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
        let x = crate::ram::parse_sample("80020000:i32:x_in_ram").unwrap();
        let cols = vec![Column::Quantity("x".into()), Column::Memory(x), Column::Quantity("dead".into())];
        let mut s = Sampler::new(cols, 2, SamplePer::Tick, Some(q));
        while s.ticks < 6 {
            if tick.ended_by(&m.step()) {
                s.tick(0, &m);
            }
            s.field(0, &m, true); // rows are per tick: no row here
        }
        assert_eq!(s.out, "run_frame,frame,tick,x,x_in_ram,dead\n0,0,2,2,2,false\n0,0,4,4,4,false\n0,0,6,6,6,false\n");
        assert_eq!(s.quantities.unwrap().refusal_count, 0);
    }

    #[test]
    fn a_scenario_is_ready_once_its_recordings_are_checked_deterministic() {
        let d = crate::port::tests::tempdir::Dir::new();
        let root = d.0.as_path();
        write_scenario(root);
        assert_eq!(unready(root, "walk"), ["scenarios/walk/: no captured recording (psx scenario record)"]);
        std::fs::write(root.join("scenarios/walk/a.jsonl"), recording(1)).unwrap();
        assert_eq!(unready(root, "walk"), ["walk: no determinism check (psx scenario check walk)"]);
        let l = Loaded::load(root, "walk");
        let ok = json!({"key": key(root, &l), "result": {"status": "deterministic"}});
        std::fs::create_dir_all(root.join(CHECKS)).unwrap();
        std::fs::write(root.join(CHECKS).join("walk.json"), ok.to_string()).unwrap();
        assert_eq!(unready(root, "walk"), Vec::<String>::new());
        // The chain rebuilt to the same state on another emulator still stands.
        let chain = |emulator: &str| json!({"steps": [{"emulator": emulator, "run": {"frames": 5}}]}).to_string();
        std::fs::write(root.join("notes/states/start.json"), chain("emu-a")).unwrap();
        let l = Loaded::load(root, "walk");
        let ok = json!({"key": key(root, &l), "result": {"status": "deterministic"}});
        std::fs::write(root.join(CHECKS).join("walk.json"), ok.to_string()).unwrap();
        std::fs::write(root.join("notes/states/start.json"), chain("emu-b")).unwrap();
        assert_eq!(unready(root, "walk"), Vec::<String>::new());
        // Any change to what was judged makes the check stale.
        std::fs::write(root.join("scenarios/walk/a.jsonl"), recording(2)).unwrap();
        assert_eq!(unready(root, "walk"), ["walk: changed since its determinism check (psx scenario check walk)"]);
        // A unit or description is documentation: the recording still stands.
        std::fs::write(root.join("scenarios/walk/a.jsonl"), recording(1).replace(",\"unit\":\"units\"", "")).unwrap();
        assert_eq!(unready(root, "walk"), ["walk: changed since its determinism check (psx scenario check walk)"]);
        // A recording of a type the glossary no longer declares is recorded again.
        std::fs::write(root.join("scenarios/walk/a.jsonl"), recording(1).replace("\"bits\":32", "\"bits\":16"))
            .unwrap();
        assert_eq!(
            unready(root, "walk"),
            [
                "scenarios/walk/a.jsonl: recorded for other quantities than the scenario compares, or with other types than the glossary declares; record it again"
            ]
        );
        // A latched recording from before psx measured late input.
        let latched = |adjusted: &str| {
            recording(1)
                .replace("\"version\":\"0\"}", &format!("\"version\":\"0\",\"details\":{{\"adjusted\":{adjusted}}}}}"))
        };
        std::fs::write(root.join("scenarios/walk/a.jsonl"), latched(r#"{"buffer":[]}"#)).unwrap();
        assert_eq!(
            unready(root, "walk"),
            [
                "scenarios/walk/a.jsonl: recorded before psx measured whether the game takes its input every tick; record it again"
            ]
        );
        assert!(late_input(root, "walk", None).is_empty());
        std::fs::write(root.join("scenarios/walk/a.jsonl"), latched(r#"{"buffer":[],"late_fields":0}"#)).unwrap();
        assert_eq!(unready(root, "walk"), ["walk: changed since its determinism check (psx scenario check walk)"]);
        assert!(late_input(root, "walk", None).is_empty());
        let late =
            r#"{"buffer":[],"late_fields":7,"late_input":{"fields":7,"ticks":[3,5,8,9,11,14,20],"tick_count":7}}"#;
        std::fs::write(root.join("scenarios/walk/a.jsonl"), latched(late)).unwrap();
        let line = "scenarios/walk/a.jsonl: the game ran 7 ticks on input a tick old (ticks 3, 5, 8, 9, 11, ..)";
        assert_eq!(late_input(root, "walk", None), [(Late::Unfixed, line.to_string())]);
        // An adjusted recording is made with fixes/ as it is.
        let adjusted = latched(r#"{"buffer":[],"late_fields":0}"#)
            .replace("\"details\":{", "\"details\":{\"adjust\":[\"latch\"],");
        std::fs::write(root.join("scenarios/walk/a.jsonl"), &adjusted).unwrap();
        let without = "scenarios/walk/a.jsonl: made without fixes/; record it again";
        assert!(!unready(root, "walk").iter().any(|p| p.contains("fixes/")));
        std::fs::create_dir_all(root.join("fixes")).unwrap();
        std::fs::write(root.join("fixes/poll.c"), "int poll(void) { return 0; }\n").unwrap();
        assert!(unready(root, "walk").contains(&without.to_string()), "{:?}", unready(root, "walk"));
        let now = crate::decomp::fixes_hash(root).unwrap();
        let with = adjusted.replace("\"details\":{", &format!("\"details\":{{\"fix\":{{\"sources\":\"{now}\"}},"));
        std::fs::write(root.join("scenarios/walk/a.jsonl"), &with).unwrap();
        assert!(!unready(root, "walk").iter().any(|p| p.contains("fixes/")));
        std::fs::write(root.join("fixes/poll.c"), "int poll(void) { return 1; }\n").unwrap();
        assert!(
            unready(root, "walk")
                .contains(&"scenarios/walk/a.jsonl: made with fixes/ as it was before; record it again".to_string())
        );
        std::fs::remove_dir_all(root.join("fixes")).unwrap();
        std::fs::write(root.join("scenarios/walk/a.jsonl"), latched(late)).unwrap();
        assert_eq!(late_input(root, "walk", Some("f1")), [(Late::Before, format!("{line}, made without fixes/"))]);
        // Made with the fix as it is and still late: the fix does not work,
        // and only a change to fixes/ asks for it again.
        let fixed = |sources: &str, calls: u64| {
            latched(&late.replace(
                "\"late_fields\"",
                &format!("\"fix_calls\":[{{\"name\":\"poll\",\"calls\":{calls}}}],\"late_fields\""),
            ))
            .replace("\"details\":{", &format!("\"details\":{{\"fix\":{{\"sources\":\"{sources}\"}},"))
        };
        std::fs::write(root.join("scenarios/walk/a.jsonl"), fixed("f1", 40)).unwrap();
        assert_eq!(
            late_input(root, "walk", Some("f1")),
            [(Late::Fails, format!("{line}, made with fixes/ as it is (poll ran 40 times)"))]
        );
        assert_eq!(
            late_input(root, "walk", Some("f2")),
            [(Late::Before, format!("{line}, made with fixes/ as it was before"))]
        );
        std::fs::write(root.join("scenarios/walk/a.jsonl"), fixed("f1", 0)).unwrap();
        assert_eq!(
            late_input(root, "walk", Some("f1")),
            [(Late::Fails, format!("{line}, made with fixes/ as it is (poll never ran)"))]
        );
    }

    #[test]
    fn show_lists_each_tick_with_its_deltas() {
        let d = crate::port::tests::tempdir::Dir::new();
        let root = d.0.as_path();
        std::fs::create_dir_all(root.join("scenarios/walk")).unwrap();
        let header = r#"{"format":"scenario-recording","format_version":1,"scenario":"walk","source":{"kind":"captured","platform":"psx","image_sha256":"00"},"producer":{"name":"psx","version":"0"},"quantities":{"x":{"type":"fixed","signed":true,"int_bits":20,"frac_bits":12,"unit":"units"},"n":{"type":"int","bits":8,"signed":false},"m":{"type":"enum","variants":["idle","walk"]}},"initial":{"x":-6144,"n":3,"m":"idle"}}"#;
        let rows = [
            r#"{"tick":1,"field":2,"pad":["right"],"values":{"x":-4096,"m":"walk"}}"#,
            r#"{"tick":2,"field":4,"values":{"x":-1}}"#,
        ];
        std::fs::write(root.join("scenarios/walk/w.jsonl"), format!("{header}\n{}\n", rows.join("\n"))).unwrap();
        let ctx = Ctx { root: root.to_path_buf(), cancel: None };
        let args = |ticks: Option<&str>, raw| ScenarioShowArgs {
            id: "walk".into(),
            name: "w".into(),
            quantities: vec![],
            ticks: ticks.map(Into::into),
            raw,
            text: true,
            screens: false,
        };
        let out = show_cmd(&ctx, &args(None, false)).unwrap();
        assert_eq!(out["columns"], json!(["tick", "field", "pad", "m", "n", "Δn", "x", "Δx"]));
        assert_eq!(
            out["rows"],
            json!([
                [0, null, "", "idle", 3, null, "-1.5", null],
                [1, 2, "right", "walk", 3, 0, "-1", "0.5"],
                [2, 4, "right", "walk", 3, 0, "-0.000244140625", "0.999755859375"],
            ])
        );
        assert_eq!(
            out["text"].as_str().unwrap().lines().nth(2).unwrap(),
            "   0      -      -  idle  3   -             -1.5               -"
        );
        let out = show_cmd(&ctx, &args(Some("2.."), true)).unwrap();
        assert_eq!(out["rows"], json!([[2, 4, "right", "walk", 3, 0, -1, 4095]]));
        let bad = ScenarioShowArgs { quantities: vec!["y".into()], ..args(None, false) };
        assert!(show_cmd(&ctx, &bad).unwrap_err().to_string().contains("the recording holds m, n, x"));
    }

    #[test]
    fn the_viewer_gets_each_quantity_over_the_ticks_with_the_port_beside_it() {
        let d = crate::port::tests::tempdir::Dir::new();
        let root = d.0.as_path();
        write_scenario(root);
        let header = r#"{"format":"scenario-recording","format_version":1,"scenario":"walk","source":{"kind":"captured","platform":"psx","image_sha256":"00"},"producer":{"name":"psx","version":"0"},"quantities":{"x":{"type":"fixed","signed":true,"int_bits":20,"frac_bits":12},"m":{"type":"enum","variants":["idle","walk"]}},"initial":{"x":-6144,"m":"idle"}}"#;
        let rows =
            [r#"{"tick":1,"field":2,"pad":["RIGHT"],"values":{"x":-4096,"m":"walk"}}"#, r#"{"tick":2,"field":4}"#];
        std::fs::write(root.join("scenarios/walk/w.jsonl"), format!("{header}\n{}\n", rows.join("\n"))).unwrap();
        let last = json!({"commit": "c0", "at": "t", "scenarios": {"scenarios": [{"id": "walk", "recordings": [{"name": "w",
            "failures": ["differs from the recording in x"],
            "differences": [{"quantity": "x", "at": "tick 2", "expected": -4096, "got": -2048}],
            "values": [{"x": -6144, "m": "idle"}, {"x": -4096, "m": "walk"}, {"x": -2048, "m": "walk"}]}]}]}});
        let v = plot(root, "walk", "w", &last).unwrap();
        assert_eq!(v["ticks"][0]["pad"], json!(["RIGHT"]));
        let q = |n: &str| v["quantities"].as_array().unwrap().iter().find(|q| q["name"] == n).unwrap().clone();
        assert_eq!(q("x")["values"], json!([-1.5, -1.0, -1.0]));
        assert_eq!(q("x")["port"], json!([-1.5, -1.0, -0.5]));
        assert_eq!(
            (q("m")["values"].clone(), q("m")["variants"].clone()),
            (json!([0.0, 1.0, 1.0]), json!(["idle", "walk"]))
        );
        assert_eq!(
            v["check"]["failures"][0],
            json!({"text": "tick 2 x: expected -4096, got -2048", "tick": 2, "quantity": "x", "expected": -1.0, "got": -0.5})
        );
        // Not in the last check: nothing to overlay.
        assert_eq!(plot(root, "walk", "w", &Json::Null).unwrap()["check"], Json::Null);
    }

    #[test]
    fn each_tick_holds_the_pad_it_ended_with() {
        let input = |frame, buttons| crate::replay::Input { frame, buttons, sticks: [128; 4] };
        let tick = |field| Full { field, ..Full::default() };
        let events = [input(1, 1), input(3, 2), input(4, 0)];
        let held = held_by_tick(&events, &[tick(0), tick(2), tick(4), tick(6)]);
        let buttons: Vec<u32> = held.iter().map(|i| i.buttons).collect();
        assert_eq!(buttons, [0, 1, 0, 0], "a press and release inside one tick is not held at its end");
        assert_eq!(held_by_tick(&events, &[tick(3)])[0].buttons, 2);
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
