//! `psx mechanic capture`: run the game from a state on the native core,
//! bracket the mechanic's step as its binding says (a Rust binding built
//! and loaded by `plugin`), and write one trace line per step
//! (docs/mechanics.md, section 6).
//!
//! Every instruction is stepped singly. Before an instruction at a PC the
//! binding names runs, the reads it calls for are taken; they count only if
//! the instruction then runs (an interrupt taken there runs it again later).
//! While a step is open, every instruction also goes to the read audit and
//! branch coverage (`audit`).

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::Instant;

use psx_capture::abi;
use psx_core::Machine;
use psx_core::cpu::MemAccess;
use psx_mechanics::interface::CallKind;
use psx_mechanics::mechanic::Types;
use psx_mechanics::trace::{Audit, Call, Coverage, Full, Header, Producer, Source, SourceKind};
use psx_mechanics::{Interface, Mechanic, Trace, Type, Values};
use serde_json::{Value as Json, json};

use super::audit::{Accounts, Tracker};
use super::plugin::{self, Plugin, Reads};
use super::workspace::{self, Loaded, Resident};
use super::{CallSite, End, EndMode, Location, Psx, Reg, in_memory};
use crate::Error;

/// Refusals listed in the result; the rest are counted.
const MAX_REFUSALS: usize = 20;

#[derive(clap::Args, Clone, Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct MechanicCaptureArgs {
    /// The mechanic: `mechanics/<ID>/`.
    pub id: String,
    /// The trace's name: it is written to `mechanics/<ID>/traces/<NAME>.trace.jsonl`.
    #[arg(long)]
    pub name: String,
    /// The state the recording starts from (a state.bin).
    #[arg(long)]
    pub from_state: PathBuf,
    /// Frames (vblanks) to run.
    #[arg(long)]
    pub frames: u32,
    /// Pad input, as run's (frames from the recording's start).
    #[arg(long, conflicts_with = "replay")]
    #[serde(default)]
    pub input: Option<crate::replay::Events>,
    /// Pad input from a replay file.
    #[arg(long)]
    #[serde(default)]
    pub replay: Option<PathBuf>,
    /// The scenario the recording is made in.
    #[arg(long)]
    #[serde(default)]
    pub scenario: Option<String>,
    /// What the recording does, in a sentence.
    #[arg(long)]
    #[serde(default)]
    pub description: Option<String>,
    /// The disc; default the project's.
    #[arg(long)]
    #[serde(default)]
    pub media: Option<PathBuf>,
    /// BIOS override; otherwise the state's.
    #[arg(long)]
    #[serde(default)]
    pub bios: Option<PathBuf>,
    /// Write over an existing trace of that name.
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

/// CPU registers as `Reg` numbers them: r0-r31, hi, lo.
pub type Regs = [u32; 34];

pub fn regs(m: &Machine) -> Regs {
    let mut r = [0; 34];
    r[..32].copy_from_slice(&m.cpu.regs);
    r[32] = m.cpu.hi;
    r[33] = m.cpu.lo;
    r
}

/// `n` (at least 1) bytes at `a`, in main RAM or the scratchpad.
pub(crate) fn memory(m: &Machine, a: u32, n: u32) -> Result<Vec<u8>, String> {
    let last = a.wrapping_add(n - 1);
    if !in_memory(a) || !in_memory(last) || (a >= 0x1F80_0000) != (last >= 0x1F80_0000) {
        return Err(format!("{n} bytes at {a:#010x}: not in main RAM or the scratchpad"));
    }
    let bus = &m.bus;
    Ok(if (0x1F80_0000..0x1F80_0400).contains(&a) {
        bus.scratch[(a - 0x1F80_0000) as usize..][..n as usize].to_vec()
    } else {
        let o = (a & 0x1F_FFFF) as usize;
        bus.ram.get(o..o + n as usize).ok_or(format!("{n} bytes at {a:#010x}: past the end of RAM"))?.to_vec()
    })
}

/// Where reads are taken at one moment: memory and the GTE now, registers
/// now and as latched at the step's begin (or a call's entry), and an
/// inline call's load.
pub(crate) struct View<'a> {
    pub m: &'a Machine,
    pub now: &'a Regs,
    pub latched: &'a Regs,
    pub load: Option<MemAccess>,
}

/// Reads that did not give a value of the field's type.
#[derive(Default)]
struct Refusals {
    listed: Vec<Json>,
    count: u64,
}

impl Refusals {
    /// `when`: the step's `begin` or `end`, or a call's `entry`, `return` or `load`.
    fn push(&mut self, step: u64, when: &str, field: String, problem: String) {
        self.count += 1;
        if self.listed.len() < MAX_REFUSALS {
            self.listed.push(json!({"step": step, "when": when, "field": field, "problem": problem}));
        }
    }
}

/// One call the step makes: an interface's, or a used mechanic's step.
struct CallSpec<'a> {
    interface: String,
    call: String,
    kind: CallKind,
    on: CallSite,
    by: By<'a>,
}

/// Who reads a call's request and response.
enum By<'a> {
    /// The binding: its call `which`, with the interface's types.
    Binding { which: u32, request: BTreeMap<String, Type>, response: BTreeMap<String, Type> },
    /// A used mechanic's own binding: its inputs and state as its step
    /// begins, its state as the call returns.
    Used { plugin: &'a Plugin, types: Types },
}

impl CallSpec<'_> {
    /// `<interface>.<call>`, then `.<side>` when given.
    fn field(&self, side: &str) -> String {
        let call = format!("{}.{}", self.interface, self.call);
        if side.is_empty() { call } else { format!("{call}.{side}") }
    }
}

/// A call entered and not yet returned.
struct Pending {
    spec: usize,
    request: Values,
    ret: u32,
    sp: u32,
    /// The registers as the call was entered, which its response is read with.
    latched: Regs,
    /// Stores the call made to the step's state, `(pc, address, bytes)`:
    /// refused unless its response holds them.
    stores: Vec<(u32, u32, u32)>,
}

/// The step under way.
struct Open {
    latched: Regs,
    frame: u64,
    inputs: Values,
    before: Values,
    calls: Vec<Call>,
    pending: Option<Pending>,
    /// The memory the binding read the state from, physical, for stores
    /// made inside a call.
    state_bytes: Vec<(u32, u32)>,
}

/// What a PC the binding names stands for.
#[derive(Clone, Copy)]
enum Site {
    Begin,
    End,
    Call(usize),
}

pub fn capture(ctx: &crate::Ctx, a: MechanicCaptureArgs) -> crate::Result<(bool, Json)> {
    let root = &ctx.root;
    if a.name.is_empty() || !a.name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-') {
        return Err(Error::invalid(format!("name {:?}: letters, digits, _ and - only", a.name)));
    }
    let l = workspace::load(root, &a.id)?;
    if !l.problems.is_empty() {
        return Err(Error::invalid("the mechanic's files have problems; psx mechanic check lists them")
            .with("problems", json!(l.problems)));
    }
    let Loaded { mechanic, plugin, interfaces, dir, .. } = l;
    let plugin = loaded(root, &a.id, plugin.as_deref(), &mechanic, &interfaces)?;
    let out = dir.join("traces").join(format!("{}.trace.jsonl", a.name));
    if out.exists() && !a.replace {
        return Err(Error::invalid(format!("{}: exists; replace=true writes over it", rel(root, &out))));
    }
    let mut residents = workspace::residents(root, plugin.capture()).map_err(Error::invalid)?;
    let mut used: Vec<(String, Mechanic, Plugin)> = Vec::new();
    for r in &mechanic.uses {
        let id = r.split('@').next().unwrap_or_default();
        let u = workspace::load(root, id)?;
        if !u.problems.is_empty() {
            return Err(Error::invalid(format!("uses {r}: its files have problems; psx mechanic check lists them"))
                .with("problems", json!(u.problems)));
        }
        let p = loaded(root, id, u.plugin.as_deref(), &u.mechanic, &u.interfaces)
            .map_err(|e| Error::invalid(format!("uses {r}: {e}")))?;
        residents.extend(workspace::residents(root, p.capture()).map_err(Error::invalid)?);
        used.push((r.clone(), u.mechanic, p));
    }

    // The machine, as `run` starts one from a state.
    let resolved = crate::context::resolve(root, a.media.clone(), a.bios.clone(), Some(&a.from_state), false)?;
    if let Some(e) = resolved.setup_error() {
        return Err(e);
    }
    let bios_path = resolved.bios.unwrap();
    let bios = std::fs::read(&bios_path).map_err(|e| Error::io(&bios_path, e))?;
    let medium = crate::run_native::open_media(resolved.media.as_deref())?;
    let Some(media_id) = medium.id.clone() else {
        return Err(Error::invalid("capture needs the game's disc: give media"));
    };
    if media_id.sha256 != plugin.source().image_sha256 {
        return Err(Error::new(
            "identity_mismatch",
            format!(
                "the binding is for image {}, the media is {} (its IMAGE_SHA256 is the media identity psx context reports)",
                plugin.source().image_sha256,
                media_id.sha256
            ),
        ));
    }
    let mut m = crate::run_native::boot(
        Some(&a.from_state),
        &Default::default(),
        bios,
        medium.exe.as_deref(),
        medium.disc,
        &medium.id,
    )?;
    let events =
        crate::replay::resolve(a.replay.as_deref(), a.input.as_ref(), a.frames as u64).map_err(Error::invalid)?;

    let plan = Plan::new(&mechanic, &plugin, &interfaces, &used, &residents);
    let deadline = std::time::Duration::from_secs(a.timeout);
    let Recorded { steps, params, refusals, timed_out, summary, audit, coverage, analysis } =
        record(&mut m, &plan, a.frames, &events, deadline)?;
    if timed_out {
        return Ok((false, json!({"status": "timeout", "capture": summary, "written": false})));
    }
    if refusals.count > 0 {
        return Ok((
            false,
            json!({"status": "refused", "capture": summary, "written": false,
                   "refusals": refusals.listed, "refusal_count": refusals.count,
                   "how": "the binding gave no value of the field's type, or a call changed the step's state: fix the binding or the mechanic's type, then capture again"}),
        ));
    }
    if steps.is_empty() {
        return Ok((
            false,
            json!({"status": "no_steps", "capture": summary, "written": false,
                   "how": "the bracket's begin never ran with is_step saying yes: check its pc and overlay, is_step, and that the recording reaches the mechanic"}),
        ));
    }
    let header = Header {
        format: psx_mechanics::trace::FORMAT.into(),
        format_version: psx_mechanics::trace::FORMAT_VERSION,
        mechanic: mechanic.id.clone(),
        mechanic_version: mechanic.version,
        fingerprint: Some(mechanic.fingerprint()),
        source: Source {
            kind: SourceKind::Captured,
            platform: Some(Psx::NAME.into()),
            image_sha256: Some(media_id.sha256.clone()),
        },
        producer: Producer {
            name: "psx".into(),
            version: env!("CARGO_PKG_VERSION").into(),
            details: json!({
                "from_state": rel(root, &a.from_state),
                "from_state_sha256": crate::stamp::sha256(&std::fs::read(&a.from_state).unwrap_or_default()),
                "frames": a.frames,
                "input": crate::replay::report(&events),
            })
            .as_object()
            .cloned()
            .unwrap_or_default(),
        },
        scenario: a.scenario.clone(),
        params,
        initial_state: steps[0].before.clone(),
        unobserved: Vec::new(),
        audit: Some(audit),
        coverage: Some(coverage),
        description: a.description.clone(),
    };
    let trace = Trace::encode(header, &steps);
    let problems = trace.check(&mechanic, &interfaces);
    if !problems.is_empty() {
        return Err(Error::new("internal", "the captured trace does not check").with("problems", json!(problems)));
    }
    if let Some(d) = out.parent() {
        std::fs::create_dir_all(d).map_err(|e| Error::io(d, e))?;
    }
    std::fs::write(&out, trace.to_jsonl()).map_err(|e| Error::io(&out, e))?;
    Ok((
        true,
        json!({"status": "captured", "trace": rel(root, &out), "capture": summary, "written": true,
               "audit": analysis["audit"], "coverage": analysis["coverage"]}),
    ))
}

/// The binding of mechanic `id` in `crate_dir`, built, loaded and checked.
fn loaded(
    root: &Path,
    id: &str,
    crate_dir: Option<&Path>,
    mechanic: &Mechanic,
    interfaces: &[Interface],
) -> crate::Result<Plugin> {
    let crate_dir = crate_dir.ok_or_else(|| Error::invalid(format!("{id}: no binding/ yet")))?;
    let p = Plugin::load(&plugin::build(root, crate_dir, id)?)?;
    let mut problems = p.check(mechanic, interfaces);
    problems.extend(workspace::overlay_problems(root, p.capture()));
    if !problems.is_empty() {
        return Err(Error::invalid("the binding has problems").with("problems", json!(problems)));
    }
    Ok(p)
}

/// What capture asks the binding, and where.
pub(crate) struct Plan<'a> {
    plugin: &'a Plugin,
    /// The mechanic's types, which the binding's answers are checked against.
    types: Types,
    specs: Vec<CallSpec<'a>>,
    /// PC -> what it stands for, and the code that must be there.
    sites: HashMap<u32, Vec<(Site, Option<&'a Resident>)>>,
    end: &'a End,
    /// What the read audit takes as accounted for.
    accounts: Accounts<'a>,
    begin: u32,
}

/// What the binding is asked for.
#[derive(Clone, Copy)]
enum What {
    State,
    Inputs,
    Params,
}

impl<'a> Plan<'a> {
    /// `plugin` binds `mechanic`, and each of `used` a mechanic it uses;
    /// all of them checked.
    pub(crate) fn new(
        mechanic: &Mechanic,
        plugin: &'a Plugin,
        interfaces: &[Interface],
        used: &'a [(String, Mechanic, Plugin)],
        residents: &'a [(u32, Option<Resident>)],
    ) -> Plan<'a> {
        let capture = plugin.capture();
        let call_types: BTreeMap<String, _> =
            mechanic.required(interfaces).into_iter().map(|i| (i.reference(), i.types(&mut Vec::new()))).collect();
        let mut specs: Vec<CallSpec> = Vec::new();
        for (which, c) in capture.calls.iter().enumerate() {
            let t = &call_types[&c.interface][&c.call];
            specs.push(CallSpec {
                interface: c.interface.clone(),
                call: c.call.clone(),
                kind: c.kind,
                on: c.on.clone(),
                by: By::Binding { which: which as u32, request: t.request.clone(), response: t.response.clone() },
            });
        }
        // A used mechanic's step: from its begin to the return to its caller.
        for (r, um, up) in used {
            specs.push(CallSpec {
                interface: r.clone(),
                call: psx_mechanics::mechanic::STEP.into(),
                kind: CallKind::Query,
                on: CallSite::Entry { entry: up.capture().begin.clone() },
                by: By::Used { plugin: up, types: um.types(&mut Vec::new()) },
            });
        }
        let mut sites: HashMap<u32, Vec<(Site, Option<&Resident>)>> = HashMap::new();
        let resident = |pc: u32| residents.iter().find(|(p, _)| *p == pc).and_then(|(_, r)| r.as_ref());
        sites.entry(capture.begin.pc).or_default().push((Site::Begin, resident(capture.begin.pc)));
        if let End::At(p) = &capture.end {
            sites.entry(p.pc).or_default().push((Site::End, resident(p.pc)));
        }
        for (i, s) in specs.iter().enumerate() {
            let (CallSite::Entry { entry: p } | CallSite::Read { read: p }) = &s.on;
            sites.entry(p.pc).or_default().push((Site::Call(i), resident(p.pc)));
        }

        // The read audit: what the binding reads or explains is accounted for.
        let explained: Vec<(&Location, u32)> =
            capture.explained.iter().filter_map(|x| Some((x.at.as_ref()?, x.bytes?))).collect();
        let readers: HashSet<u32> = capture.explained.iter().filter_map(|x| Some(x.by.as_ref()?.pc)).collect();
        let (mut registers, mut gte) = (0u64, 0u64);
        for (at, _) in &explained {
            match at {
                Location::Register { register } => registers |= 1 << *register as u32,
                Location::Gte { gte: g } => gte |= 1 << g,
                _ => {}
            }
        }
        let accounts = Accounts {
            memory: explained
                .into_iter()
                .filter(|(at, _)| matches!(at, Location::Address { .. } | Location::Base { .. }))
                .collect(),
            readers,
            registers,
            gte,
            // Stack locals read as sp + offset; the binding's own bases are
            // added from what it reads at the first step.
            bases: vec![Reg::Sp],
            calls: specs.iter().map(|s| s.field("")).collect(),
            entries: specs
                .iter()
                .filter_map(|s| match &s.on {
                    CallSite::Entry { entry } => Some(entry.pc),
                    CallSite::Read { .. } => None,
                })
                .collect(),
            end: match &capture.end {
                End::At(p) => Some(p.pc),
                End::Mode(_) => None,
            },
        };
        Plan {
            plugin,
            types: mechanic.types(&mut Vec::new()),
            specs,
            sites,
            end: &capture.end,
            accounts,
            begin: capture.begin.pc,
        }
    }

    /// Whether the call at the begin PC is the step: the binding's
    /// `is_step`, and what it read to say so.
    fn is_step(&self, view: &View, step: u64, refusals: &mut Refusals) -> (bool, Reads) {
        let (answer, reads) = self.plugin.call(abi::IS_STEP, 0, view);
        match answer.and_then(|t| plugin::yes(&t)) {
            Ok(yes) => (yes, reads),
            Err(e) => {
                refusals.push(step, "begin", "is_step".into(), e);
                (false, reads)
            }
        }
    }

    /// The state, the inputs or the params, as the binding reads them with
    /// `view`, and what it read.
    fn read(&self, what: What, view: &View, when: &str, step: u64, refusals: &mut Refusals) -> (Values, Reads) {
        let (label, q, types) = match what {
            What::State => ("state", abi::STATE, &self.types.state),
            What::Inputs => ("inputs", abi::INPUTS, &self.types.inputs),
            What::Params => ("params", abi::PARAMS, &self.types.params),
        };
        ask(self.plugin, q, 0, types, view, label, when, step, refusals)
    }

    /// Whether the entry PC of call `i` is entered as that call: a used
    /// mechanic's binding chooses its step as at its own begin.
    fn enters(&self, i: usize, view: &View) -> bool {
        match &self.specs[i].by {
            By::Binding { .. } => true,
            By::Used { plugin, .. } => {
                plugin.call(abi::IS_STEP, 0, view).0.and_then(|t| plugin::yes(&t)).unwrap_or(false)
            }
        }
    }

    /// Call `i`'s request, and what was read for it.
    fn request(&self, i: usize, view: &View, when: &str, step: u64, refusals: &mut Refusals) -> (Values, Reads) {
        let s = &self.specs[i];
        let label = s.field("request");
        match &s.by {
            By::Binding { which, request, .. } => {
                ask(self.plugin, abi::REQUEST, *which, request, view, &label, when, step, refusals)
            }
            By::Used { plugin, types } => {
                let (mut v, mut r) = ask(plugin, abi::INPUTS, 0, &types.inputs, view, &label, when, step, refusals);
                let (state, rs) = ask(plugin, abi::STATE, 0, &types.state, view, &label, when, step, refusals);
                v.extend(state);
                r.extend(&rs);
                (v, r)
            }
        }
    }

    /// Call `i`'s response, and what was read for it.
    fn response(&self, i: usize, view: &View, when: &str, step: u64, refusals: &mut Refusals) -> (Values, Reads) {
        let s = &self.specs[i];
        let label = s.field("response");
        match &s.by {
            By::Binding { which, response, .. } => {
                ask(self.plugin, abi::RESPONSE, *which, response, view, &label, when, step, refusals)
            }
            By::Used { plugin, types } => ask(plugin, abi::STATE, 0, &types.state, view, &label, when, step, refusals),
        }
    }
}

/// Asks `plugin` question `q` (of call `which`) with `view`, and checks the
/// answer against `types`: its values, refused as `label` when they are
/// not, and what it read.
#[allow(clippy::too_many_arguments)]
fn ask(
    plugin: &Plugin,
    q: u32,
    which: u32,
    types: &BTreeMap<String, Type>,
    view: &View,
    label: &str,
    when: &str,
    step: u64,
    refusals: &mut Refusals,
) -> (Values, Reads) {
    let (answer, reads) = plugin.call(q, which, view);
    let values = match answer.map(|t| plugin::values(&t, types)) {
        Ok(Ok(v)) => v,
        Ok(Err(problems)) => {
            for (field, e) in problems {
                let f = if field.is_empty() { label.to_string() } else { format!("{label}.{field}") };
                refusals.push(step, when, f, e);
            }
            Values::new()
        }
        Err(e) => {
            refusals.push(step, when, label.into(), e);
            Values::new()
        }
    };
    (values, reads)
}

/// What a recording gave.
pub(crate) struct Recorded {
    steps: Vec<Full>,
    params: Values,
    refusals: Refusals,
    timed_out: bool,
    summary: Json,
    audit: Audit,
    coverage: Coverage,
    /// The audit's and coverage's report, with the draft of `explained`.
    analysis: Json,
}

/// Runs `m` for `frames` with `events`, recording each step `plan` brackets.
pub(crate) fn record(
    m: &mut Machine,
    plan: &Plan,
    frames: u32,
    events: &[crate::replay::Input],
    deadline: std::time::Duration,
) -> crate::Result<Recorded> {
    let Plan { specs, sites, end, .. } = plan;
    let mut params = Values::new();
    let started = Instant::now();
    let start_frame = m.bus.frames;
    let target = start_frame + frames as u64;
    let mut next_event = 0;
    let mut last_frame = u64::MAX;
    let mut refusals = Refusals::default();
    let mut steps: Vec<Full> = Vec::new();
    let mut open: Option<Open> = None;
    let mut begins_filtered: u64 = 0;
    let mut instructions: u64 = 0;
    let mut timed_out = false;
    let mut tracker = Tracker::new(&plan.accounts, plan.begin);

    while m.bus.frames < target {
        if m.bus.frames != last_frame {
            last_frame = m.bus.frames;
            let rel = m.bus.frames - start_frame;
            if **end == End::Mode(EndMode::Frame) && open.as_ref().is_some_and(|o| o.frame != rel) {
                let o = open.take().unwrap();
                let now = regs(m);
                let view = View { m, now: &now, latched: &o.latched, load: None };
                let after = plan.read(What::State, &view, "end", steps.len() as u64, &mut refusals).0;
                close_with(o, after, &mut steps, specs, &mut refusals, &mut tracker);
            }
            while next_event < events.len() && events[next_event].frame <= rel {
                crate::replay::apply(m, &events[next_event]);
                next_event += 1;
            }
            if started.elapsed() > deadline {
                timed_out = true;
                break;
            }
        }
        let pc = m.cpu.pc;
        let here: Vec<Site> = match sites.get(&pc) {
            Some(s) => s.iter().filter(|(_, r)| r.is_none_or(|r| r.holds(&m.bus.ram))).map(|(s, _)| *s).collect(),
            None => Vec::new(),
        };
        let returning =
            open.as_ref().and_then(|o| o.pending.as_ref()).is_some_and(|p| p.ret == pc && m.cpu.regs[29] == p.sp);
        if here.is_empty() && !returning {
            let info = m.step();
            instructions += 1;
            // A store made inside a call to the step's state: refused at its
            // return unless its response holds it.
            if let (Some(o), Some(acc)) = (open.as_mut(), info.mem.filter(|a| a.store)) {
                let at = crate::run::fold(acc.addr);
                let end = at + acc.width as u32;
                let hit = o.state_bytes.iter().any(|&(s, e)| at < e && end > s);
                if let Some(p) = o.pending.as_mut().filter(|_| hit) {
                    p.stores.push((info.pc, acc.addr, acc.width as u32));
                }
            }
            if let Some(o) = &open {
                tracker.observe(m, &info, o.pending.as_ref().map(|p| p.spec), false);
            }
            continue;
        }

        // Reads due before the instruction runs.
        let now = regs(m);
        let n = steps.len() as u64;
        let mut tentative = Refusals::default();
        let response = returning.then(|| {
            let p = open.as_ref().unwrap().pending.as_ref().unwrap();
            let view = View { m, now: &now, latched: &p.latched, load: None };
            plan.response(p.spec, &view, "return", n, &mut tentative)
        });
        let pending = open.as_ref().is_some_and(|o| o.pending.is_some()) && !returning;
        let ends = here.iter().any(|s| matches!(s, Site::End)) && open.is_some() && !pending;
        let begins = here.iter().any(|s| matches!(s, Site::Begin)) && !pending;
        // The binding chooses the step, with the registers as they are now.
        let (begins, chose) = if begins {
            let view = View { m, now: &now, latched: &now, load: None };
            let k = n + (open.is_some() && !ends) as u64;
            plan.is_step(&view, k, &mut tentative)
        } else {
            (false, Reads::default())
        };
        if here.iter().any(|s| matches!(s, Site::Begin)) && !begins && !pending {
            begins_filtered += 1;
        }
        let after = ends.then(|| {
            let o = open.as_ref().unwrap();
            let view = View { m, now: &now, latched: &o.latched, load: None };
            plan.read(What::State, &view, "end", n, &mut tentative).0
        });
        let opening = begins.then(|| {
            let view = View { m, now: &now, latched: &now, load: None };
            let k = n + (open.is_some() && !ends) as u64;
            let first = steps.is_empty() && open.is_none();
            let p = first.then(|| {
                let (p, read) = plan.read(What::Params, &view, "begin", k, &mut tentative);
                let ranges: Vec<(u32, u32)> = read.memory.iter().map(|&(s, e)| (s, e - s)).collect();
                (p, ranges)
            });
            let (i, read_inputs) = plan.read(What::Inputs, &view, "begin", k, &mut tentative);
            let (s, read_state) = plan.read(What::State, &view, "begin", k, &mut tentative);
            let bytes = read_state.memory.clone();
            let mut read = chose.clone();
            read.extend(&read_inputs);
            read.extend(&read_state);
            (p, i, s, bytes, read)
        });
        // Entries: a call starts (inline calls are read after the load).
        let entry = here.iter().find_map(|s| match s {
            Site::Call(i) if matches!(specs[*i].on, CallSite::Entry { .. }) => {
                let view = View { m, now: &now, latched: &now, load: None };
                plan.enters(*i, &view).then_some(*i)
            }
            _ => None,
        });
        let entering = entry.filter(|_| !pending && (open.is_some() || opening.is_some())).map(|i| {
            let view = View { m, now: &now, latched: &now, load: None };
            let (request, reads) = plan.request(i, &view, "entry", n, &mut tentative);
            // A used mechanic's request is its own binding's to account for;
            // an interface's is what the step passes it.
            let reads = if matches!(specs[i].by, By::Used { .. }) { Reads::default() } else { reads };
            (Pending { spec: i, request, ret: now[31], sp: now[29], latched: now, stores: Vec::new() }, reads)
        });
        // The world call this instruction belongs to, if any.
        let world = entering
            .as_ref()
            .map(|(p, _)| p.spec)
            .or_else(|| open.as_ref().and_then(|o| o.pending.as_ref()).filter(|_| pending).map(|p| p.spec));
        let inline = here.iter().any(|s| matches!(s, Site::Call(i) if matches!(specs[*i].on, CallSite::Read { .. })));

        let info = m.step();
        instructions += 1;
        if info.exception.is_some() || info.pc != pc {
            // Interrupted before it ran: it runs again, and is read again then.
            if open.is_some() {
                tracker.observe(m, &info, world, inline);
            }
            continue;
        }
        refusals.count += tentative.count;
        refusals.listed.extend(tentative.listed.into_iter().take(MAX_REFUSALS.saturating_sub(refusals.listed.len())));
        if let Some((r, reads)) = response {
            let o = open.as_mut().unwrap();
            let p = o.pending.take().unwrap();
            let s = &specs[p.spec];
            for (spc, addr, bytes) in p.stores {
                let at = crate::run::fold(addr);
                if !reads.memory.iter().any(|&(b, e)| b <= at && at + bytes <= e) {
                    refusals.push(
                        n,
                        "store",
                        s.field(""),
                        format!("the call changed the step's state outside its response (store at {spc:#010x} to {addr:#010x})"),
                    );
                }
            }
            tracker.returned(&reads);
            o.calls.push(Call {
                interface: s.interface.clone(),
                call: s.call.clone(),
                request: p.request,
                response: (s.kind == CallKind::Query).then_some(r),
            });
        }
        if let Some(after) = after {
            // The state as read before the instruction at the end PC ran.
            close_with(open.take().unwrap(), after, &mut steps, specs, &mut refusals, &mut tracker);
        }
        if let Some((p, i, s, bytes, read)) = opening {
            if let Some(o) = open.take() {
                if **end != End::Mode(EndMode::NextBegin) {
                    return Err(Error::invalid(format!(
                        "step {}: the bracket's begin ran again before its end: is the bracket right?",
                        steps.len()
                    ))
                    .with("pc", json!(format!("{pc:#010x}"))));
                }
                // Next begin: the step ends where the next one begins.
                close_with(o, s.clone(), &mut steps, specs, &mut refusals, &mut tracker);
            }
            let data = p.map(|(p, ranges)| {
                params.extend(p);
                ranges
            });
            open = Some(Open {
                latched: now,
                frame: m.bus.frames - start_frame,
                inputs: i,
                before: s,
                calls: Vec::new(),
                pending: None,
                state_bytes: bytes,
            });
            tracker.open(m, &now);
            tracker.binding_reads(&read);
            if let Some(ranges) = data {
                tracker.account(&ranges);
            }
        }
        if let (Some((p, reads)), Some(o)) = (entering, open.as_mut()) {
            tracker.entry(pc, p.spec, &reads, now[29]);
            o.pending = Some(p);
        }
        if open.is_some() {
            tracker.observe(m, &info, world, inline);
        }
        // Inline calls, from the load the instruction made.
        for s in &here {
            let Site::Call(i) = *s else { continue };
            let CallSite::Read { .. } = specs[i].on else { continue };
            let Some(o) = open.as_mut().filter(|o| o.pending.is_none()) else { continue };
            let n = steps.len() as u64;
            let Some(acc) = info.mem.filter(|a| !a.store) else {
                refusals.push(n, "load", specs[i].field(""), "the read PC made no load".into());
                continue;
            };
            let after_regs = regs(m);
            let view = View { m, now: &after_regs, latched: &o.latched, load: Some(acc) };
            let (request, read_request) = plan.request(i, &view, "load", n, &mut refusals);
            let (response, read_response) = plan.response(i, &view, "load", n, &mut refusals);
            tracker.binding_reads(&read_request);
            tracker.binding_reads(&read_response);
            o.calls.push(Call {
                interface: specs[i].interface.clone(),
                call: specs[i].call.clone(),
                request,
                response: (specs[i].kind == CallKind::Query).then_some(response),
            });
        }
    }
    // External changes: which fields code outside the step changed, in how many steps.
    let mut fields: BTreeMap<&String, u64> = BTreeMap::new();
    for w in steps.windows(2) {
        for (name, v) in &w[1].before {
            if w[0].after.get(name) != Some(v) {
                *fields.entry(name).or_default() += 1;
            }
        }
    }
    let first: Vec<usize> = steps.iter().enumerate().filter(|(_, s)| s.external).map(|(k, _)| k).take(10).collect();
    let summary = json!({
        "steps": steps.len(), "frames": m.bus.frames - start_frame, "instructions": instructions,
        "begins_filtered_out": begins_filtered,
        "open_step_dropped": open.is_some(),
        "external_changes": {
            "steps": steps.iter().filter(|s| s.external).count(),
            "fields": fields,
            "first_steps": first,
        },
        "wall_ms": started.elapsed().as_millis() as u64,
    });
    let (audit, coverage, analysis) = tracker.finish();
    Ok(Recorded { steps, params, refusals, timed_out, summary, audit, coverage, analysis })
}

/// Ends a step with `after` as its state.
fn close_with(
    o: Open,
    after: Values,
    steps: &mut Vec<Full>,
    specs: &[CallSpec],
    refusals: &mut Refusals,
    tracker: &mut Tracker,
) {
    tracker.close();
    let n = steps.len() as u64;
    if let Some(p) = &o.pending {
        let s = &specs[p.spec];
        refusals.push(n, "end", s.field(""), "the step ended before the call returned".into());
    }
    let external = steps.last().is_some_and(|p| p.after != o.before);
    steps.push(Full {
        inputs: o.inputs,
        calls: o.calls,
        before: o.before,
        after,
        external,
        source_frame: Some(o.frame),
    });
}

fn rel(root: &Path, p: &Path) -> String {
    p.strip_prefix(root).unwrap_or(p).to_string_lossy().into_owned()
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;

    use psx_capture::reg::{A0, A1, S0, V0};
    use psx_capture::{At, Bracket, Machine as Host, Query, Site as At_};
    use psx_mechanics::Value;
    use psx_mechanics::rust::{Crate, LIB};

    use super::*;

    const COUNT: &str = r#"
//! Count
//!
//! Adds one, and asks the world.

pub const ID: &str = "test.count";
pub const VERSION: u32 = 1;
pub const RATE: &str = "event";
pub const REQUIRES: &[&str] = &["world.add@1"];

pub struct State {
    pub count: i32,
    pub last: i32,
}
"#;

    const WORLD: &str = r#"
//! Adds 100

pub const ID: &str = "world.add";
pub const VERSION: u32 = 1;

pub struct Added {
    pub m: i32,
}

pub struct Peeked {
    pub v: i32,
}

pub trait World {
    fn add(&mut self, n: i32) -> Added;
    fn peek(&mut self, at: u32) -> Peeked;
}
"#;

    const BUMP: &str = r#"
//! Bump
//!
//! Adds 5 to last.

pub const ID: &str = "test.bump";
pub const VERSION: u32 = 1;
pub const RATE: &str = "event";

pub struct State {
    pub last: i32,
}
"#;

    fn mechanic(lib: &str) -> Mechanic {
        let files = [(LIB.to_string(), lib.to_string())];
        let (m, problems) =
            psx_mechanics::rust::mechanic(&Crate { cargo_toml: "[package]\nname = \"t\"\n", files: &files }).unwrap();
        assert_eq!(problems, Vec::<String>::new());
        m
    }

    fn world_add() -> Interface {
        let files = [(LIB.to_string(), WORLD.to_string())];
        psx_mechanics::rust::interface(&Crate { cargo_toml: "[package]\nname = \"world-add\"\n", files: &files })
            .unwrap()
            .0
    }

    /// How the test bindings bracket and read, per test (each runs on its own thread).
    #[derive(Clone)]
    struct Conf {
        end: Option<u32>,
        object: u32,
        peek: u32,
        calls: bool,
        response_in_memory: bool,
        explained: Vec<(At, u32)>,
        explained_reader: Option<u32>,
        table: bool,
        /// The bits the binding says `last` has: a stale glue when it is
        /// wider than the mechanic's.
        last_bits: u32,
    }

    impl Default for Conf {
        fn default() -> Conf {
            Conf {
                end: Some(0x8001_012C),
                object: 0x8002_0000,
                peek: 0x8001_0108,
                calls: true,
                response_in_memory: false,
                explained: Vec::new(),
                explained_reader: None,
                table: false,
                last_bits: 32,
            }
        }
    }

    thread_local! {
        static CONF: RefCell<Conf> = RefCell::new(Conf::default());
    }

    fn conf() -> Conf {
        CONF.with(|c| c.borrow().clone())
    }

    struct State {
        count: i32,
        last: i64,
    }

    struct CountMechanic;

    impl psx_capture::Mechanic for CountMechanic {
        const ID: &'static str = "test.count";
        const VERSION: u32 = 1;
        const FINGERPRINT: &'static str = "";
        type State = State;
        type Inputs = ();
        type Params = Vec<i32>;
        fn state(s: &State) -> psx_capture::Value {
            psx_capture::Value::Record(vec![
                ("count", psx_capture::Value::Int(s.count as i128)),
                ("last", psx_capture::Value::Int(s.last as i128)),
            ])
        }
        fn inputs(_: &()) -> psx_capture::Value {
            psx_capture::Value::Record(Vec::new())
        }
        fn params(p: &Vec<i32>) -> psx_capture::Value {
            let t = p.iter().map(|v| psx_capture::Value::Int(*v as i128)).collect();
            psx_capture::Value::Record(if p.is_empty() { Vec::new() } else { vec![("t", psx_capture::Value::List(t))] })
        }
    }

    struct Add;
    impl psx_capture::Call for Add {
        const INTERFACE: &'static str = "world.add@1";
        const NAME: &'static str = "add";
        type Request = i32;
        type Response = i32;
        fn request(n: &i32) -> psx_capture::Value {
            psx_capture::Value::Record(vec![("n", psx_capture::Value::Int(*n as i128))])
        }
        fn response(m: &i32) -> psx_capture::Value {
            psx_capture::Value::Record(vec![("m", psx_capture::Value::Int(*m as i128))])
        }
    }
    impl Query for Add {}

    struct Peek;
    impl psx_capture::Call for Peek {
        const INTERFACE: &'static str = "world.add@1";
        const NAME: &'static str = "peek";
        type Request = u32;
        type Response = i32;
        fn request(at: &u32) -> psx_capture::Value {
            psx_capture::Value::Record(vec![("at", psx_capture::Value::Int(*at as i128))])
        }
        fn response(v: &i32) -> psx_capture::Value {
            psx_capture::Value::Record(vec![("v", psx_capture::Value::Int(*v as i128))])
        }
    }
    impl Query for Peek {}

    struct Count;

    impl psx_capture::Binding for Count {
        type Mechanic = CountMechanic;
        const TITLE: &'static str = "test";
        const IMAGE_SHA256: &'static str = "0000000000000000000000000000000000000000000000000000000000000000";

        fn bracket(&self, b: &mut Bracket) {
            let c = conf();
            b.begin(0x8001_0100u32);
            match c.end {
                Some(pc) => b.end(pc),
                None => b.end_at_next_begin(),
            };
            for (at, n) in c.explained {
                b.explain(at, n, "t");
            }
            if let Some(pc) = c.explained_reader {
                b.explain_reader(pc, "t");
            }
            if c.calls {
                b.query::<Peek>(At_::load(c.peek), |m| m.load().address, |m| m.load().value as i32);
                b.query::<Add>(
                    At_::entry(0x8001_0200u32),
                    |m| m.reg(A1) as i32,
                    |m| if conf().response_in_memory { m.i32(m.latched(A0) + 4) } else { m.reg(V0) as i32 },
                );
            }
        }

        fn is_step(&self, m: &Host) -> bool {
            m.reg(A0) == conf().object
        }

        fn state(&self, m: &Host) -> State {
            let a0 = m.latched(A0);
            let last = if conf().last_bits == 32 { m.i32(a0 + 4) as i64 } else { m.i32(a0 + 4) as i64 + (1 << 40) };
            State { count: m.i32(a0), last }
        }

        fn inputs(&self, _: &Host) {}

        fn params(&self, m: &Host) -> Vec<i32> {
            let a0 = m.latched(A0);
            if conf().table { (0..3).map(|k| m.i32(a0 + 4 * k)).collect() } else { Vec::new() }
        }
    }

    #[allow(unsafe_code)]
    unsafe extern "C" fn count_call(what: u32, which: u32, host: *const psx_capture::abi::Host) -> i32 {
        // SAFETY: the caller's.
        unsafe { psx_capture::abi::call(&Count, what, which, host) }
    }

    struct BumpMechanic;

    impl psx_capture::Mechanic for BumpMechanic {
        const ID: &'static str = "test.bump";
        const VERSION: u32 = 1;
        const FINGERPRINT: &'static str = "";
        type State = i32;
        type Inputs = ();
        type Params = ();
        fn state(s: &i32) -> psx_capture::Value {
            psx_capture::Value::Record(vec![("last", psx_capture::Value::Int(*s as i128))])
        }
        fn inputs(_: &()) -> psx_capture::Value {
            psx_capture::Value::Record(Vec::new())
        }
        fn params(_: &()) -> psx_capture::Value {
            psx_capture::Value::Record(Vec::new())
        }
    }

    /// test.bump's step is the world routine at 0x80010200: it adds 5 to [a0 + 4].
    struct Bump;

    impl psx_capture::Binding for Bump {
        type Mechanic = BumpMechanic;
        const TITLE: &'static str = "test";
        const IMAGE_SHA256: &'static str = "0000000000000000000000000000000000000000000000000000000000000000";

        fn bracket(&self, b: &mut Bracket) {
            b.begin(0x8001_0200u32).end(0x8001_0120u32);
        }

        fn state(&self, m: &Host) -> i32 {
            m.i32(m.latched(A0) + 4)
        }

        fn inputs(&self, _: &Host) {}

        fn params(&self, _: &Host) {}
    }

    #[allow(unsafe_code)]
    unsafe extern "C" fn bump_call(what: u32, which: u32, host: *const psx_capture::abi::Host) -> i32 {
        // SAFETY: the caller's.
        unsafe { psx_capture::abi::call(&Bump, what, which, host) }
    }

    /// A loop calling a step that adds one to [a0] and stores what the world
    /// answers (a1 + 100) at [a0 + 4]. `world` replaces the world's code.
    fn machine(world: &[u32]) -> Machine {
        let mut m = Machine::new(vec![0; 0x8_0000]);
        let mut put = |at: u32, words: &[u32]| {
            for (i, w) in words.iter().enumerate() {
                let o = (at & 0x1F_FFFF) as usize + 4 * i;
                m.bus.ram[o..o + 4].copy_from_slice(&w.to_le_bytes());
            }
        };
        // main: jal step; nop; j main; nop
        put(0x8001_0000, &[0x0C00_4040, 0, 0x0800_4000, 0]);
        #[rustfmt::skip]
        put(0x8001_0100, &[
            0x27BD_FFF8, // addiu sp, sp, -8
            0xAFBF_0000, // sw ra, 0(sp)
            0x8C88_0000, // lw t0, 0(a0)
            0x0000_0000, // nop
            0x2508_0001, // addiu t0, t0, 1
            0x0C00_4080, // jal world
            0x0100_2821, // addu a1, t0, zero
            0xAC82_0004, // sw v0, 4(a0)
            0xAC88_0000, // sw t0, 0(a0)
            0x8FBF_0000, // lw ra, 0(sp)
            0x27BD_0008, // addiu sp, sp, 8
            0x03E0_0008, // jr ra
            0x0000_0000, // nop
        ]);
        put(0x8001_0200, world);
        m.cpu.regs[4] = 0x8002_0000;
        m.cpu.regs[29] = 0x801F_FF00;
        m.cpu.set_pc(0x8001_0000);
        m
    }

    /// world: addiu v0, a1, 100; jr ra; nop
    const ADD: [u32; 3] = [0x24A2_0064, 0x03E0_0008, 0];

    fn run(c: Conf, world: &[u32]) -> crate::Result<Recorded> {
        run_patched(c, world, &[])
    }

    /// With these words at these addresses replaced.
    fn run_patched(c: Conf, world: &[u32], patches: &[(u32, u32)]) -> crate::Result<Recorded> {
        CONF.with(|x| *x.borrow_mut() = c.clone());
        let mut lib = COUNT.to_string();
        if c.table {
            lib += "pub struct Params {\n    pub t: [i32; 3],\n}\n";
        }
        if c.last_bits != 32 {
            lib = lib.replace("pub last: i32", "pub last: i8");
        }
        let (mechanic, interfaces) = (mechanic(&lib), vec![world_add()]);
        let p = Plugin::new(count_call, None).unwrap();
        assert_eq!(
            p.check(&mechanic, &interfaces),
            [
                "its glue reads the mechanic's fields differently from this psx: psx template update brings vendor/psx-mechanics to this psx's"
            ]
        );
        let plan = Plan::new(&mechanic, &p, &interfaces, &[], &[]);
        let mut m = machine(world);
        for (at, w) in patches {
            let o = (at & 0x1F_FFFF) as usize;
            m.bus.ram[o..o + 4].copy_from_slice(&w.to_le_bytes());
        }
        record(&mut m, &plan, 1, &[], std::time::Duration::from_secs(60))
    }

    fn int(n: i128) -> Value {
        Value::Int(n)
    }

    #[test]
    fn steps_and_calls_are_recorded() {
        let r = run(Conf::default(), &ADD).unwrap();
        assert_eq!((r.refusals.count, r.timed_out), (0, false), "{:?}", r.refusals.listed);
        assert!(r.steps.len() > 100, "{} steps", r.steps.len());
        for (k, s) in r.steps.iter().enumerate().take(3) {
            let k = k as i128;
            assert_eq!(s.before.get("count"), Some(&int(k)));
            assert_eq!(s.after, [("count".into(), int(k + 1)), ("last".into(), int(k + 101))].into());
            let peek = Call {
                interface: "world.add@1".into(),
                call: "peek".into(),
                request: [("at".into(), int(0x8002_0000))].into(),
                response: Some([("v".into(), int(k))].into()),
            };
            let add = Call {
                interface: "world.add@1".into(),
                call: "add".into(),
                request: [("n".into(), int(k + 1))].into(),
                response: Some([("m".into(), int(k + 101))].into()),
            };
            assert_eq!(s.calls, vec![peek, add]);
            assert!(!s.external);
        }
    }

    #[test]
    fn a_binding_is_checked_against_its_mechanic_and_interfaces() {
        CONF.with(|x| *x.borrow_mut() = Conf { calls: false, ..Conf::default() });
        let p = Plugin::new(count_call, None).unwrap();
        let (m, i) = (mechanic(COUNT), vec![world_add()]);
        let problems = p.check(&m, &i);
        assert!(problems.contains(&"world.add@1: add is not bound: declare it in bracket".to_string()), "{problems:?}");
        assert!(
            problems.contains(&"world.add@1: peek is not bound: declare it in bracket".to_string()),
            "{problems:?}"
        );
        let other = mechanic(&COUNT.replace("VERSION: u32 = 1", "VERSION: u32 = 2"));
        assert_eq!(p.check(&other, &i)[0], "binds test.count@1, the mechanic is test.count@2");
    }

    #[test]
    fn end_modes() {
        let r = run(Conf { end: None, ..Conf::default() }, &ADD).unwrap();
        assert_eq!(r.summary["open_step_dropped"], true);
        assert_eq!(r.steps[1].before.get("count"), Some(&int(1)));
        let e = run(Conf { end: Some(0x8001_0300), ..Conf::default() }, &ADD).err().unwrap();
        assert!(e.to_string().contains("the bracket's begin ran again before its end"), "{e}");
    }

    #[test]
    fn is_step_picks_the_object() {
        let r = run(Conf { object: 0x8002_0004, ..Conf::default() }, &ADD).unwrap();
        assert!(r.steps.is_empty());
        assert!(r.summary["begins_filtered_out"].as_u64() > Some(100));
    }

    #[test]
    fn a_call_that_changes_the_state_is_refused() {
        // world: addiu v0, a1, 100; sw v0, 0(a0); jr ra; nop
        let r = run(Conf::default(), &[0x24A2_0064, 0xAC82_0000, 0x03E0_0008, 0]).unwrap();
        assert!(r.refusals.count > 0);
        assert_eq!(
            r.refusals.listed[0],
            json!({"step": 0, "when": "store", "field": "world.add@1.add",
                   "problem": "the call changed the step's state outside its response (store at 0x80010204 to 0x80020000)"})
        );
    }

    #[test]
    fn a_call_may_change_the_state_its_response_holds() {
        // world: addiu v0, a1, 100; sw v0, 4(a0); jr ra; nop, its response in memory.
        let c = Conf { response_in_memory: true, ..Conf::default() };
        let r = run(c, &[0x24A2_0064, 0xAC82_0004, 0x03E0_0008, 0]).unwrap();
        assert_eq!(r.refusals.count, 0, "{:?}", r.refusals.listed);
        let add = r.steps[1].calls.iter().find(|c| c.call == "add").unwrap();
        assert_eq!(add.response.as_ref().unwrap()["m"], int(102));
    }

    #[test]
    fn a_value_that_does_not_fit_is_refused() {
        let r = run(Conf { last_bits: 8, ..Conf::default() }, &ADD).unwrap();
        assert_eq!(r.refusals.listed[0]["step"], 0);
        assert_eq!(r.refusals.listed[0]["when"], "begin");
        assert_eq!(r.refusals.listed[0]["field"], "state.last");
    }

    #[test]
    fn the_audit_lists_what_the_binding_leaves_out() {
        let r = run(Conf::default(), &ADD).unwrap();
        assert_eq!(r.audit.unaccounted, vec![], "the stack, ra and sp, a0 and the response are accounted for");
        // lw t1, 8(a0) in the load's delay slot, never used: not a read.
        let r = run_patched(Conf::default(), &ADD, &[(0x8001_010C, 0x8C89_0008)]).unwrap();
        assert_eq!(r.audit.unaccounted, vec![]);
        // Used: addu a1, t1, s0 in the call's delay slot.
        let patches = [(0x8001_010C, 0x8C89_0008), (0x8001_0118, 0x0130_2821)];
        let r = run_patched(Conf::default(), &ADD, &patches).unwrap();
        let n = r.steps.len();
        let found: Vec<Json> = r.audit.unaccounted.iter().map(|f| json!(f)).collect();
        assert_eq!(
            found,
            vec![
                json!({"at": {"pc": "0x8001010c", "address": "0x80020008", "base": "a0", "offset": "0x8"},
                       "note": format!("4 bytes, {n} of {n} steps")}),
                json!({"at": {"pc": "0x80010118", "register": "s0"},
                       "note": format!("read before the step wrote it, {n} of {n} steps")}),
            ]
        );
        assert_eq!(
            r.analysis["audit"]["draft"],
            "b.explain(At::Base(A0, 0x8), 4, \"\"); // read at 0x8001010c\nb.explain(At::Register(S0), 4, \"\"); // read at 0x80010118\n"
        );
        let c = Conf { explained: vec![(At::Base(A0, 8), 4), (At::Register(S0), 4)], ..Conf::default() };
        let r = run_patched(c, &ADD, &patches).unwrap();
        assert_eq!(r.audit.unaccounted, vec![]);
        // Or by the reading instruction, wherever it reads.
        let c = Conf { explained: vec![(At::Register(S0), 4)], explained_reader: Some(0x8001_010C), ..Conf::default() };
        let r = run_patched(c, &ADD, &patches).unwrap();
        assert_eq!(r.audit.unaccounted, vec![]);
    }

    #[test]
    fn params_the_binding_reads_are_accounted_for_in_every_step() {
        // The step reads 8(a0) and uses it; a table of three words at a0 holds it.
        let patches = [(0x8001_010C, 0x8C89_0008), (0x8001_0118, 0x0130_2821)];
        let r = run_patched(Conf { table: true, ..Conf::default() }, &ADD, &patches).unwrap();
        assert_eq!(r.refusals.count, 0, "{:?}", r.refusals.listed);
        assert_eq!(json!(r.params["t"]), json!([0, 0, 0]));
        let found: Vec<Json> = r.audit.unaccounted.iter().map(|f| json!(f)).collect();
        assert_eq!(found.len(), 1, "only s0 is left: {found:?}");
        assert_eq!(found[0]["at"]["register"], "s0");
    }

    #[test]
    fn a_register_the_call_changed_outside_its_response_is_listed() {
        // world: addiu v0, a1, 100; addiu v1, zero, 7; jr ra; nop. The step stores v1.
        let r =
            run_patched(Conf::default(), &[0x24A2_0064, 0x2403_0007, 0x03E0_0008, 0], &[(0x8001_011C, 0xAC83_0004)])
                .unwrap();
        let n = r.steps.len();
        assert_eq!(
            json!(r.audit.unaccounted),
            json!([{"at": {"pc": "0x8001011c", "register": "v1"},
                    "note": format!("{n} of {n} steps, changed by the call world.add@1.add (not in its response)")}])
        );
    }

    #[test]
    fn a_hardware_read_is_explained_or_bound_as_an_inline_call() {
        // lui t1, 0x1F80; lw t2, 0x1108(t1) (timer 0's target); addu a1, t2, t0.
        let patches = [(0x8001_010C, 0x3C09_1F80), (0x8001_0110, 0x8D2A_1108), (0x8001_0118, 0x0148_2821)];
        let r = run_patched(Conf::default(), &ADD, &patches).unwrap();
        let n = r.steps.len();
        assert_eq!(
            json!(r.audit.unaccounted),
            json!([{"at": {"pc": "0x80010110", "address": "0x1f801108"},
                    "note": format!("4 bytes, {n} of {n} steps, a hardware register: bind it as an inline read if the result depends on it, else explain it")}])
        );
        assert_eq!(
            r.analysis["audit"]["draft"],
            "b.explain(At::Address(0x1f801108), 4, \"\"); // read at 0x80010110; or bind it as an inline read if the result depends on it\n"
        );
        let c = Conf { explained: vec![(At::Address(0x1F80_1108), 4)], ..Conf::default() };
        let r = run_patched(c, &ADD, &patches).unwrap();
        assert_eq!(r.audit.unaccounted, vec![]);

        // Bound as the inline call peek: the trace holds what the hardware gave.
        CONF.with(|x| *x.borrow_mut() = Conf { peek: 0x8001_0110, ..Conf::default() });
        let (mechanic, interfaces) = (mechanic(COUNT), vec![world_add()]);
        let p = Plugin::new(count_call, None).unwrap();
        let plan = Plan::new(&mechanic, &p, &interfaces, &[], &[]);
        let mut m = machine(&ADD);
        for (at, w) in patches {
            let o = (at & 0x1F_FFFF) as usize;
            m.bus.ram[o..o + 4].copy_from_slice(&w.to_le_bytes());
        }
        m.bus.write(0x1F80_1108, psx_core::Width::Word, 0x1234).unwrap();
        let r = record(&mut m, &plan, 1, &[], std::time::Duration::from_secs(60)).unwrap();
        assert_eq!((r.refusals.count, r.audit.unaccounted.len()), (0, 0), "{:?}", r.refusals.listed);
        let peek = &r.steps[0].calls[0];
        assert_eq!(peek.call, "peek");
        assert_eq!(peek.request, [("at".into(), int(0x1F80_1108))].into());
        assert_eq!(peek.response, Some([("v".into(), int(0x1234))].into()));
    }

    #[test]
    fn coverage_counts_each_way_of_each_branch() {
        let r = run(Conf::default(), &ADD).unwrap();
        assert_eq!((r.coverage.reached, r.coverage.total), (0, 0));
        // bne a0, zero, +3 in the load's delay slot: always taken, past the call.
        let r = run_patched(Conf::default(), &ADD, &[(0x8001_010C, 0x1480_0003)]).unwrap();
        assert_eq!((r.coverage.reached, r.coverage.total), (1, 2));
        assert_eq!(json!(r.coverage.missed), json!([{"at": {"pc": "0x8001010c"}, "note": "never fell through"}]));
    }

    /// The step of `test.count` using `test.bump`, whose step is the world
    /// routine at 0x80010200.
    fn run_using(world: &[u32]) -> Recorded {
        CONF.with(|x| *x.borrow_mut() = Conf { calls: false, ..Conf::default() });
        let lib = COUNT.replace(
            r#"pub const REQUIRES: &[&str] = &["world.add@1"];"#,
            r#"pub const USES: &[&str] = &["test.bump@1"];"#,
        );
        let (counting, bump) = (mechanic(&lib), mechanic(BUMP));
        let interfaces = vec![bump.step_interface()];
        assert_eq!(counting.check(&interfaces), Vec::<String>::new());
        let p = Plugin::new(count_call, None).unwrap();
        let used = [("test.bump@1".to_string(), bump, Plugin::new(bump_call, None).unwrap())];
        let plan = Plan::new(&counting, &p, &interfaces, &used, &[]);
        let mut m = machine(world);
        m.bus.ram[0x2_0004..0x2_0008].copy_from_slice(&7u32.to_le_bytes());
        // The step leaves [a0 + 4] to test.bump: sw v0, 4(a0) becomes a nop.
        m.bus.ram[0x1_011C..0x1_0120].copy_from_slice(&0u32.to_le_bytes());
        record(&mut m, &plan, 1, &[], std::time::Duration::from_secs(60)).unwrap()
    }

    #[test]
    fn a_used_mechanic_is_recorded_as_its_step() {
        #[rustfmt::skip]
        let bump = [
            0x8C89_0004, // lw t1, 4(a0)
            0x0000_0000, // nop
            0x2529_0005, // addiu t1, t1, 5
            0xAC89_0004, // sw t1, 4(a0)
            0x03E0_0008, // jr ra
            0x24A2_0064, // addiu v0, a1, 100
        ];
        let r = run_using(&bump);
        assert_eq!(r.refusals.count, 0, "{:?}", r.refusals.listed);
        let c = &r.steps[0].calls[0];
        assert_eq!((c.interface.as_str(), c.call.as_str()), ("test.bump@1", "step"));
        assert_eq!((c.request["last"].clone(), c.response.as_ref().unwrap()["last"].clone()), (int(7), int(12)));
        // Its own reads are its binding's; the step's are accounted for.
        assert_eq!(r.audit.unaccounted, vec![]);
        // A store outside its response is refused.
        let mut wild = bump;
        wild[3] = 0xAC89_0000; // sw t1, 0(a0)
        let r = run_using(&wild);
        assert_eq!(
            r.refusals.listed[0],
            json!({"step": 0, "when": "store", "field": "test.bump@1.step",
                   "problem": "the call changed the step's state outside its response (store at 0x8001020c to 0x80020000)"})
        );
    }
}
