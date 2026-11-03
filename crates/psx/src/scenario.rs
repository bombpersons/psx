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
use crate::port::Entry;

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

/// The glossary, or what is wrong with it.
pub fn glossary(root: &Path) -> Result<Glossary, Vec<String>> {
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
    let binding = dir.join("binding/Cargo.toml");
    match std::fs::read_to_string(&binding) {
        Err(_) => problems.push(format!(
            "{GLOSSARY}/binding: not written yet (a cdylib package named glossary-binding, a member of the workspace; vendor/psx-capture's crate docs show it)"
        )),
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
    /// Where the ticks' pads came from: None before the first recorded tick.
    pad_from: Option<PadFrom>,
}

/// Where a recording's pads came from, which its details name as `pad`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PadFrom {
    /// The game's own input word, from the binding's answer to INPUT.
    Game,
    /// The controller's buttons, a field ahead of or behind the game's.
    Controller,
}

impl PadFrom {
    fn name(self) -> &'static str {
        match self {
            PadFrom::Game => "game",
            PadFrom::Controller => "controller",
        }
    }
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
/// the start. `on_tick` sees the machine at the end of each recorded tick.
#[allow(clippy::too_many_arguments)]
fn record(
    m: &mut Machine,
    q: &Quantities,
    frames: u32,
    events: &[crate::replay::Input],
    ticks: Option<&[crate::replay::Input]>,
    deadline: Duration,
    mut cover: Option<&mut Cover>,
    mut on_tick: Option<OnTick<'_>>,
) -> Recorded {
    let started = Instant::now();
    // The pad is released until the first tick ends: `initial` is the moment
    // the port's launcher builds, before any input. The recording's fields
    // (and the input's frames) count from the field after that tick.
    crate::replay::apply(m, &crate::replay::Input { frame: 0, buttons: 0, sticks: [128; 4] });
    let mut adjust = q.adjust.start(m);
    let mut start: Option<u64> = None;
    let mut target = u64::MAX;
    let tick = &q.binding.described().tick;
    let mut out = Recorded {
        initial: None,
        ticks: Vec::new(),
        refusals: Vec::new(),
        refusal_count: 0,
        timed_out: false,
        unread_sticks: None,
        pad_from: None,
    };
    let (mut next_event, mut last_frame) = (0, m.bus.frames);
    let lead_limit = m.bus.frames + frames as u64;
    if let Some(c) = cover.as_deref_mut() {
        c.scripts.declare(q.binding, m);
    }
    while m.bus.frames < target {
        if m.bus.frames != last_frame {
            last_frame = m.bus.frames;
            crate::live::field(m);
            if out.initial.is_some() && start.is_none() {
                start = Some(m.bus.frames);
                // Following ticks, the game may fall behind: room for it.
                let slack = if ticks.is_some() { frames as u64 / 8 + 16 } else { 0 };
                target = m.bus.frames + frames as u64 + slack;
            }
            if let Some(start) = start {
                let field = m.bus.frames - start;
                if let Some(pad) = ticks.and_then(|t| t.get(out.ticks.len())) {
                    crate::replay::apply(m, pad);
                }
                while ticks.is_none() && next_event < events.len() && events[next_event].frame <= field {
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
        if let Some(c) = cover.as_deref_mut() {
            c.tracer.after_step(&info, &m.cpu, &m.bus);
            c.scripts.step(&info, q.binding, m);
        }
        if !tick.ended_by(&info) {
            continue;
        }
        if let Some(c) = cover.as_deref_mut() {
            c.scripts.declare(q.binding, m);
        }
        // A tick ended: its quantities, as the store left them.
        let n = out.ticks.len() as u64 + out.initial.is_some() as u64;
        // The pad as the game's rules read it, when the binding says where
        // that is; otherwise the controller's, which leads or trails the
        // game's by a field that depends on the game's phase. One recording
        // keeps one or the other: a binding that fails, or changes its
        // answer's kind, is refused rather than mixing them.
        let (mut game_pad, mut pad_problem) = (None, None);
        if out.initial.is_some() {
            match q.binding.input(m) {
                Ok(p) => {
                    let from = if p.is_some() { PadFrom::Game } else { PadFrom::Controller };
                    match out.pad_from {
                        Some(was) if was != from => {
                            pad_problem = Some(format!(
                                "the binding gave the {} pad here and the {} pad on earlier ticks",
                                from.name(),
                                was.name()
                            ));
                        }
                        _ => out.pad_from = Some(from),
                    }
                    game_pad = p;
                }
                Err(e) => pad_problem = Some(e),
            }
        }
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
        if let Some(problem) = pad_problem {
            refuse("pad", problem);
        }
        if out.initial.is_none() {
            out.initial = Some(values);
            adjust.tick_ended(n, m);
        } else {
            // A second tick in the first tick's field belongs to the lead-in
            // field, which the recording counts as field 0.
            let field = start.map_or(0, |s| m.bus.frames - s);
            // The sticks as the game read them: centred unless the pad is in analog mode.
            let pad = &m.bus.sio0.pad;
            let sticks = if pad.analog { pad.axes() } else { format::CENTRED };
            if !pad.analog && pad.axes() != format::CENTRED {
                out.unread_sticks.get_or_insert(n);
            }
            let bits = game_pad.map_or_else(|| m.bus.sio0.buttons.into(), u32::from);
            let buttons = crate::replay::button_names(bits);
            out.ticks.push(Full { tick: n, field, pad: buttons, analog: pad.analog, sticks, values });
            if let Some(f) = on_tick.as_deref_mut() {
                f(out.ticks.last().unwrap(), m);
            }
            adjust.tick_ended(n, m);
            if ticks.is_some_and(|t| out.ticks.len() >= t.len()) {
                break;
            }
        }
    }
    out
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
    let adjust = Adjustments::default_set();
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
        let r = record(m, &q, frames, &events, None, deadline, None, None);
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
            adjust: self.adjust,
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

    /// Run `m` from where the recording starts to where its fields count
    /// from: the pad released until the first tick ends, then on to the
    /// next field. False when no tick ended within `fields`.
    pub(crate) fn lead_in(&self, m: &mut Machine, fields: u64) -> bool {
        crate::replay::apply(m, &crate::replay::Input { frame: 0, buttons: 0, sticks: [128; 4] });
        let tick = &self.b.described().tick;
        let (mut ticked, mut last_frame) = (false, m.bus.frames);
        let limit = m.bus.frames + fields;
        loop {
            // As `record` counts fields: from the first field change after the tick.
            if m.bus.frames != last_frame {
                last_frame = m.bus.frames;
                if ticked {
                    return true;
                }
                if m.bus.frames >= limit {
                    return false;
                }
            }
            ticked |= tick.ended_by(&m.step());
        }
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
        let r = record(m, &self.quantities(), frames, events, None, timeout, Some(&mut cover), None);
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
            "pad": r.pad_from.unwrap_or(PadFrom::Controller).name(),
        });
        if let Some(f) = &self.from {
            details["from"] = json!(f);
        }
        if let Some(a) = self.adjust.detail() {
            details["adjust"] = a;
        }
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
               "adjust": self.adjust.names(),
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

/// Which catalogue entries name the recording `id/name`, or where it
/// could go: the features naming the scenario's other recordings. None
/// outside a port.
fn catalogue_note(root: &Path, id: &str, name: &str) -> Option<String> {
    let port = crate::port::Port::load(root).ok()?;
    let target = format!("{id}/{name}");
    let named: Vec<String> = port
        .entries
        .values()
        .flat_map(|e| {
            let r = &e.recordings;
            let w = r.work.contains(&target).then(|| format!("{} (work)", e.id));
            let h = r.held_out.contains(&target).then(|| format!("{} (held_out)", e.id));
            w.into_iter().chain(h)
        })
        .collect();
    if !named.is_empty() {
        return Some(format!("{target} is named by {}", named.join(", ")));
    }
    let using: Vec<&str> =
        port.entries.values().filter(|e| e.scenarios().iter().any(|s| s == id)).map(|e| e.id.as_str()).collect();
    Some(if using.is_empty() {
        format!(
            "{target} is in no catalogue entry's recordings: add it to recordings.work (or held_out) of the feature it is for"
        )
    } else {
        format!(
            "{target} is in no catalogue entry's recordings; {} {} this scenario's other recordings: add it to recordings.work or held_out of the feature it is for",
            using.join(", "),
            if using.len() == 1 { "names" } else { "name" }
        )
    })
}

fn record_cmd(ctx: &Ctx, a: ScenarioRecordArgs) -> crate::Result<(bool, Json)> {
    let events =
        crate::replay::resolve(a.replay.as_deref(), a.input.as_ref(), a.frames as u64).map_err(Error::invalid)?;
    let mut p = prepare(ctx, &a.id, &a.name, a.from.as_deref(), a.replace)?;
    if a.stock {
        p.adjust = Adjustments::stock();
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
    for (name, rec) in captured {
        // As it was recorded: checked above.
        let adjust = Adjustments::from_details(&rec.header.producer.details).unwrap_or_default();
        let q = Quantities { binding: &b, types: g.types(), kept: l.compared().keys().cloned().collect(), adjust };
        let (frames, events) = kept_input(name, rec).map_err(Error::malformed)?;
        let mut m = machine(ctx, &chain, &second, &b.described().source)?.m;
        if let Some(from) = rec.header.producer.details.get("from").and_then(Json::as_str) {
            reach_end(&mut m, &l, &q, from, timeout)?;
        }
        let pads = held_by_tick(&events, &rec.expand());
        let r = record(&mut m, &q, frames, &events, Some(&pads), timeout, None, None);
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

/// One scenario's measurement of the adjustments: each captured recording
/// made again from the scenario's state, stock and adjusted, driven by the
/// same ticks, and compared.
fn adjust_check(ctx: &Ctx, id: &str, timeout: Duration) -> crate::Result<Json> {
    let root = &ctx.root;
    let adjust = Adjustments::default_set();
    let l = Loaded::load(root, id);
    let mut problems = l.problems.clone();
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
            Ok(record(&mut m, &q, frames, &events, Some(&pads), timeout, None, None))
        };
        let (stock, adjusted) = (run(Adjustments::stock())?, run(adjust)?);
        let differences = match (unrecorded(&stock), unrecorded(&adjusted)) {
            (Some(why), _) => vec![json!({"stock": why})],
            (_, Some(why)) => vec![json!({"adjusted": why})],
            _ => adjusted_differences(&stock, &adjusted),
        };
        same &= differences.is_empty();
        recordings.push(json!({"name": name, "differences": differences}));
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
}

fn adjust_check_cmd(ctx: &Ctx, a: ScenarioAdjustCheckArgs) -> crate::Result<(bool, Json)> {
    let ids = if a.ids.is_empty() { ids(&ctx.root) } else { a.ids.clone() };
    if ids.is_empty() {
        return Err(Error::invalid("no scenarios: write scenarios/<id>.toml"));
    }
    let mut results = Vec::new();
    for id in ids {
        check_name("id", &id)?;
        let mut r = adjust_check(ctx, &id, Duration::from_secs(a.timeout))?;
        let kept = json!({"at": crate::stamp::utc_now(), "psx": crate::context::version(), "result": r.clone()});
        let p = ctx.root.join(CHECKS).join(format!("{id}.adjust.json"));
        std::fs::create_dir_all(p.parent().unwrap()).map_err(|e| Error::io(&p, e))?;
        crate::write_atomic(&p, &serde_json::to_vec_pretty(&kept).unwrap_or_default()).map_err(|e| Error::io(&p, e))?;
        if r["status"] == "differs" {
            r["how"] = json!(
                "an adjustment changed what the original did: a shift (the same values a tick or two earlier or later, as a press taking effect sooner under latch) is the adjustment's expected cost; a value the stock run never reaches is a rule that depends on what was adjusted: record that scenario --stock, and say why in the feature's notes"
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
    let port_values: Vec<psx_scenario::Values> = reported
        .as_ref()
        .and_then(|r| serde_json::from_value(r["values"].clone()).ok())
        .unwrap_or_default();
    let mut quantities = Vec::new();
    let mut types = BTreeMap::new();
    for (q, f) in &rec.header.quantities {
        let Ok(t) = f.ty() else { continue };
        let at = |v: Option<&psx_scenario::Value>| v.and_then(|v| plotted(&t, v));
        let text = |v: Option<&psx_scenario::Value>| v.map(|v| shown(&t, v, false)).unwrap_or(Json::Null);
        let values: Vec<Option<f64>> =
            std::iter::once(at(rec.header.initial.get(q))).chain(full.iter().map(|x| at(x.values.get(q)))).collect();
        let shown_values: Vec<Json> =
            std::iter::once(text(rec.header.initial.get(q))).chain(full.iter().map(|x| text(x.values.get(q)))).collect();
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
    // "tick 91 croc_x: expected 14990, got 15026" or "initial x: …".
    let failures: Vec<Json> = reported
        .as_ref()
        .and_then(|r| r["failures"].as_array().cloned())
        .unwrap_or_default()
        .iter()
        .filter_map(Json::as_str)
        .map(|line| {
            let (tick, rest) = match line.strip_prefix("tick ").and_then(|r| r.split_once(' ')) {
                Some((n, rest)) if n.parse::<u64>().is_ok() => (n.parse::<u64>().ok(), rest),
                _ => (line.strip_prefix("initial ").map(|_| 0), line.strip_prefix("initial ").unwrap_or(line)),
            };
            let (q, what) = rest.split_once(": ").unwrap_or(("", rest));
            let number = |s: &str| {
                let v: psx_scenario::Value = serde_json::from_str(s).ok()?;
                plotted(types.get(q)?, &v)
            };
            let (expected, got) = match what.strip_prefix("expected ").and_then(|w| w.split_once(", got ")) {
                Some((e, g)) => (number(e), number(g)),
                None => (None, None),
            };
            let known = types.contains_key(q);
            json!({"text": line, "tick": tick.filter(|_| known), "quantity": known.then_some(q), "expected": expected, "got": got})
        })
        .collect();
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

/// Replay a captured recording from the scenario's state, following its
/// ticks as the port's replay does; `on_tick` sees the machine at the end of
/// each. What differs from the recording comes back (nothing when the replay
/// is the recording).
pub(crate) fn replay(
    ctx: &Ctx,
    id: &str,
    name: &str,
    timeout: Duration,
    on_tick: OnTick<'_>,
) -> crate::Result<Vec<String>> {
    let root = &ctx.root;
    check_name("id", id)?;
    check_name("name", name)?;
    let l = Loaded::load(root, id);
    if !l.problems.is_empty() {
        return Err(Error::invalid("the scenario or glossary has problems").with("problems", json!(l.problems)));
    }
    let rec = match l.recordings.iter().find(|(n, _, _)| n == name) {
        Some((_, _, Ok(r))) if r.header.source.kind == SourceKind::Captured => r,
        Some((_, f, Ok(_))) => return Err(Error::invalid(format!("{}: not captured from the original", rel(root, f)))),
        Some((_, f, Err(e))) => return Err(Error::malformed(format!("{}: {e}", rel(root, f)))),
        None => return Err(Error::invalid(format!("{name}: no recording of that name in {SCENARIOS}/{id}/"))),
    };
    let (s, g) = (l.scenario.as_ref().unwrap(), l.glossary.as_ref().unwrap());
    let (state, chain) = crate::states::chain_state(ctx, &root.join(&s.chain))?;
    let b = binding(root, g)?;
    let adjust = Adjustments::from_details(&rec.header.producer.details).map_err(Error::malformed)?;
    let q = Quantities { binding: &b, types: g.types(), kept: l.compared().keys().cloned().collect(), adjust };
    let (frames, events) = kept_input(name, rec).map_err(Error::malformed)?;
    let mut m = machine(ctx, &chain, &state, &b.described().source)?.m;
    if let Some(from) = rec.header.producer.details.get("from").and_then(Json::as_str) {
        reach_end(&mut m, &l, &q, from, timeout)?;
    }
    let pads = held_by_tick(&events, &rec.expand());
    let r = record(&mut m, &q, frames, &events, Some(&pads), timeout, None, Some(on_tick));
    Ok(match unrecorded(&r) {
        Some(why) => vec![format!("the replay gave no recording: {}", why["status"].as_str().unwrap_or(""))],
        None => {
            let again = Header { initial: r.initial.clone().unwrap(), ..rec.header.clone() };
            let (mut d, schedule) = differences(rec, &again, &r.ticks, "the replay");
            d.extend(schedule);
            d
        }
    })
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

#[derive(clap::Args, Clone, Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct ScenarioShowArgs {
    /// The scenario: `scenarios/<ID>.toml`.
    pub id: String,
    /// The recording: `scenarios/<ID>/<NAME>.jsonl`.
    #[arg(long)]
    pub name: String,
    /// Quantities to show, comma-separated; default every one recorded.
    #[arg(long, value_delimiter = ',')]
    #[serde(default)]
    pub quantities: Vec<String>,
    /// Ticks to show, inclusive: `A..B`, `A..`, `..B` or `N` (0 is the initial moment).
    #[arg(long)]
    #[serde(default)]
    pub ticks: Option<String>,
    /// Values as recorded (a fixed's integer, a float's bits) instead of as numbers.
    #[arg(long)]
    #[serde(default)]
    pub raw: bool,
    /// Plain-text table instead of JSON.
    #[arg(long)]
    #[serde(default)]
    pub text: bool,
}

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
        out["sticks"] =
            json!("the stick axes away from centre (128), as the game read them: 0 is left or up, 255 right or down; digital: the pad not in analog mode, so it sends no sticks");
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

/// What stops one scenario from counting toward its feature's stage 2:
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

/// What stops the feature's scenarios (those its recordings come from)
/// from counting toward stage 2; empty when it names a recording and each
/// scenario is deterministic.
pub fn feature_unready(root: &Path, e: &Entry) -> Vec<String> {
    let scenarios = e.scenarios();
    if scenarios.is_empty() {
        return vec![format!("{}: names no recordings (recordings.work: <scenario>/<name>)", e.file)];
    }
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

    /// Demo, whose game keeps its pad at 0x80020030 (psx's button bits):
    /// once x, which counts the ticks, reaches `from`.
    struct Padded {
        from: i32,
    }

    impl psx_capture::Glossary for Padded {
        type Glue = Glue;
        const TITLE: &'static str = "Padded";
        const IMAGE_SHA256: &'static str = "0000000000000000000000000000000000000000000000000000000000000000";

        fn tick(&self, t: &mut psx_capture::Tick) {
            Demo.tick(t)
        }

        fn quantities(&self, m: &psx_capture::Machine) -> Q {
            Demo.quantities(m)
        }

        fn input(&self, m: &psx_capture::Machine) -> Option<u16> {
            (m.i32(0x8002_0000) >= self.from).then(|| m.u16(0x8002_0030))
        }
    }

    #[allow(unsafe_code)]
    unsafe extern "C" fn padded_call(what: u32, host: *const psx_capture::abi::Host) -> i32 {
        // SAFETY: the caller's.
        unsafe { psx_capture::abi::call(&Padded { from: 0 }, what, host) }
    }

    #[allow(unsafe_code)]
    unsafe extern "C" fn padded_late_call(what: u32, host: *const psx_capture::abi::Host) -> i32 {
        // SAFETY: the caller's.
        unsafe { psx_capture::abi::call(&Padded { from: 5 }, what, host) }
    }

    /// A machine running Demo's loop: x counts up, and each pass ends a
    /// tick; the game's pad word holds `pad`.
    fn looping(pad: u16) -> Machine {
        let mut m = Machine::new(vec![0; 0x8_0000]);
        #[rustfmt::skip]
        let loop_ = [
            0x3C09_8002u32, // lui t1, 0x8002
            0x8D28_0000, // lw t0, 0(t1)
            0x2508_0001, // addiu t0, t0, 1
            0xAD28_0000, // sw t0, 0(t1): x
            0xAD28_0010, // sw t0, 0x10(t1): the tick ends
            0x0800_4000, // j 0x80010000
            0x0000_0000, // nop
        ];
        for (i, w) in loop_.iter().enumerate() {
            m.bus.ram[0x1_0000 + 4 * i..][..4].copy_from_slice(&w.to_le_bytes());
        }
        m.bus.ram[0x2_0030..][..2].copy_from_slice(&pad.to_le_bytes());
        m.cpu.set_pc(0x8001_0000);
        m
    }

    fn recorded_with(call: binding::Call, m: &mut Machine) -> Recorded {
        let b: Plugin<Described> = Plugin::new(call, None).unwrap();
        let types: BTreeMap<String, Type> =
            [("x".to_string(), Type::Int { bits: 32, signed: true }), ("dead".to_string(), Type::Bool)].into();
        let q = Quantities { binding: &b, types, kept: vec!["x".into()], adjust: Adjustments::stock() };
        record(m, &q, 1, &[], None, Duration::from_secs(60), None, None)
    }

    #[test]
    fn a_binding_that_keeps_the_games_pad_records_it() {
        let held = (1 << 4) | (1 << 14);
        let r = recorded_with(padded_call, &mut looping(held));
        assert_eq!(r.refusal_count, 0, "{:?}", r.refusals);
        assert_eq!(r.pad_from, Some(PadFrom::Game));
        assert!(!r.ticks.is_empty());
        let names = crate::replay::button_names(held as u32);
        assert_eq!(names.len(), 2);
        assert!(r.ticks.iter().all(|t| t.pad == names), "{:?}", r.ticks[0].pad);
    }

    #[test]
    fn a_binding_without_the_games_pad_records_the_controllers() {
        // The controller is released: the word in RAM is not read.
        let r = recorded_with(demo_call, &mut looping(1 << 4));
        assert_eq!(r.refusal_count, 0, "{:?}", r.refusals);
        assert_eq!(r.pad_from, Some(PadFrom::Controller));
        assert!(r.ticks.iter().all(|t| t.pad.is_empty()));
    }

    #[test]
    fn a_binding_that_starts_answering_partway_is_refused() {
        let r = recorded_with(padded_late_call, &mut looping(1 << 4));
        assert!(r.refusal_count > 0);
        assert_eq!(r.refusals[0]["quantity"], "pad");
        assert!(r.refusals[0]["problem"].as_str().unwrap().contains("the game pad here and the controller pad"));
    }

    #[test]
    fn the_input_answer_is_null_or_buttons() {
        assert_eq!(binding::input_answer("null"), Ok(None));
        assert_eq!(binding::input_answer(r#"{"buttons":16}"#), Ok(Some(16)));
        assert!(binding::input_answer(r#"{"buttons":70000}"#).is_err());
        assert!(binding::input_answer(r#"{"pad":16}"#).is_err());
        assert!(binding::input_answer("16").is_err());
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
        record(&mut m, &q, 1, &[], None, Duration::from_secs(60), None, None)
    }

    #[test]
    fn an_adjustment_is_measured_against_the_stock_game() {
        use crate::adjust::TestAdjust;
        let stock = demo_recorded(Adjustments::stock());
        let nothing = demo_recorded(Adjustments { test: Some(TestAdjust::Nothing) });
        assert_eq!(adjusted_differences(&stock, &nothing), Vec::<Json>::new());
        // One more on x when tick 3 ends: tick 4 is the first to differ.
        let nudged = demo_recorded(Adjustments { test: Some(TestAdjust::Nudge { tick: 3, addr: 0x8002_0000 }) });
        let d = adjusted_differences(&stock, &nudged);
        assert_eq!(d.len(), 1, "{d:?}");
        assert_eq!((d[0]["quantity"].as_str(), d[0]["tick"].as_u64()), (Some("x"), Some(4)));
        assert_eq!(d[0]["adjusted"].as_i64(), d[0]["stock"].as_i64().map(|v| v + 1));
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
        let r = record(&mut m, &q, 1, &[], None, Duration::from_secs(60), Some(&mut c), None);
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
        let r = record(&mut m, &q, 2, &[], None, Duration::from_secs(60), Some(&mut c), None);
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
        let q = Quantities { binding, types, refusals: Vec::new(), refusal_count: 0 };
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
        let rows = [r#"{"tick":1,"field":2,"pad":["RIGHT"],"values":{"x":-4096,"m":"walk"}}"#, r#"{"tick":2,"field":4}"#];
        std::fs::write(root.join("scenarios/walk/w.jsonl"), format!("{header}\n{}\n", rows.join("\n"))).unwrap();
        let last = json!({"commit": "c0", "at": "t", "scenarios": {"scenarios": [{"id": "walk", "recordings": [{"name": "w",
            "failures": ["tick 2 x: expected -4096, got -2048"],
            "values": [{"x": -6144, "m": "idle"}, {"x": -4096, "m": "walk"}, {"x": -2048, "m": "walk"}]}]}]}});
        let v = plot(root, "walk", "w", &last).unwrap();
        assert_eq!(v["ticks"][0]["pad"], json!(["RIGHT"]));
        let q = |n: &str| v["quantities"].as_array().unwrap().iter().find(|q| q["name"] == n).unwrap().clone();
        assert_eq!(q("x")["values"], json!([-1.5, -1.0, -1.0]));
        assert_eq!(q("x")["port"], json!([-1.5, -1.0, -0.5]));
        assert_eq!((q("m")["values"].clone(), q("m")["variants"].clone()), (json!([0.0, 1.0, 1.0]), json!(["idle", "walk"])));
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
