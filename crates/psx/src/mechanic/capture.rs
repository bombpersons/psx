//! `psx mechanic capture`: run the game from a state on the native core,
//! bracket the mechanic's step as `binding.toml` says, and write one trace
//! line per step (docs/mechanics.md, section 6).
//!
//! Every instruction is stepped singly. Before an instruction at a PC the
//! binding names runs, the reads it calls for are taken; they count only if
//! the instruction then runs (an interrupt taken there runs it again later).
//! While a step is open, every instruction also goes to the read audit and
//! branch coverage (`audit`).

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::Instant;

use psx_core::Machine;
use psx_core::cpu::MemAccess;
use psx_mechanics::binding::{CallBinding, Encoding, Param, Read};
use psx_mechanics::interface::CallKind;
use psx_mechanics::trace::{Audit, Call, Coverage, Full, Header, Producer, Source, SourceKind};
use psx_mechanics::{Interface, Mechanic, Trace, Type, Value, Values};
use serde_json::{Value as Json, json};

use super::audit::{Accounts, Tracker};
use super::workspace::{self, Loaded, Resident};
use super::{Binding, CallSite, End, EndMode, Load, Location, Psx, Reg, in_memory};
use crate::Error;
use psx_mechanics::Platform;

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

/// Where reads are taken at one moment: memory and the GTE now, registers
/// now and as latched at the step's begin, and an inline read's load.
struct View<'a> {
    m: &'a Machine,
    now: &'a Regs,
    latched: &'a Regs,
    load: Option<MemAccess>,
}

impl View<'_> {
    fn memory(&self, a: u32, n: u32) -> Result<Vec<u8>, String> {
        let last = a.wrapping_add(n - 1);
        if !in_memory(a) || !in_memory(last) || (a >= 0x1F80_0000) != (last >= 0x1F80_0000) {
            return Err(format!("{n} bytes at {a:#010x}: not in main RAM or the scratchpad"));
        }
        let bus = &self.m.bus;
        Ok(if (0x1F80_0000..0x1F80_0400).contains(&a) {
            bus.scratch[(a - 0x1F80_0000) as usize..][..n as usize].to_vec()
        } else {
            let o = (a & 0x1F_FFFF) as usize;
            bus.ram.get(o..o + n as usize).ok_or(format!("{n} bytes at {a:#010x}: past the end of RAM"))?.to_vec()
        })
    }

    fn fetch(&self, at: &Location, offset: u32, n: u32) -> Result<Vec<u8>, String> {
        let word = |v: u32, what: &str| {
            if offset != 0 || n > 4 {
                Err(format!("{what} holds 4 bytes"))
            } else {
                Ok(v.to_le_bytes()[..n as usize].to_vec())
            }
        };
        match at {
            Location::Address { address } => self.memory(address.wrapping_add(offset), n),
            Location::Base { base, offset: o } => {
                let b = self.latched[*base as usize];
                self.memory(b.wrapping_add(*o as u32).wrapping_add(offset), n)
                    .map_err(|e| format!("{} = {b:#010x}: {e}", base.name()))
            }
            Location::Register { register } => word(self.now[*register as usize], "a register"),
            Location::Gte { gte } => {
                let g = &self.m.cpu.gte;
                let v = if *gte < 32 { g.read_data(*gte as usize) } else { g.read_ctrl(*gte as usize - 32) };
                word(v, "a GTE register")
            }
            Location::Load { load } => {
                let a = self.load.ok_or("the instruction made no load")?;
                match load {
                    Load::Address => word(a.addr, "an address"),
                    Load::Value if n > a.width as u32 => Err(format!("the load is {} bytes", a.width)),
                    Load::Value => word(a.value, "a load"),
                }
            }
        }
    }

    fn read(&self, r: &Read<Location>, ty: &Type) -> Result<Value, String> {
        r.decode(ty, Psx::ENDIAN, &mut |at, offset, n| self.fetch(at, offset, n))
    }
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

/// A set of bound fields with their types.
type Fields<'a> = Vec<(&'a String, &'a Read<Location>, Type)>;

fn read_all(fields: &Fields, view: &View, what: &str, when: &str, step: u64, refusals: &mut Refusals) -> Values {
    let mut out = Values::new();
    for (name, r, ty) in fields {
        match view.read(r, ty) {
            Ok(v) => {
                out.insert((*name).clone(), v);
            }
            Err(e) => refusals.push(step, when, format!("{what}.{name}"), e),
        }
    }
    out
}

/// One interface call the binding sees.
struct CallSpec<'a> {
    interface: String,
    call: String,
    kind: CallKind,
    on: &'a CallSite,
    request: Fields<'a>,
    response: Fields<'a>,
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
}

/// The step under way.
struct Open {
    latched: Regs,
    frame: u64,
    inputs: Values,
    before: Values,
    calls: Vec<Call>,
    pending: Option<Pending>,
    /// The state's bytes, physical, for stores made inside a call.
    state_bytes: Vec<(u32, u32, String)>,
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
    let Loaded { mechanic, binding, interfaces, dir, .. } = l;
    let b = binding.expect("checked");
    let out = dir.join("traces").join(format!("{}.trace.jsonl", a.name));
    if out.exists() && !a.replace {
        return Err(Error::invalid(format!("{}: exists; replace=true writes over it", rel(root, &out))));
    }
    let residents = workspace::residents(root, &b).map_err(Error::invalid)?;

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
    if media_id.sha256 != b.source.image_sha256 {
        return Err(Error::new(
            "identity_mismatch",
            format!(
                "the binding is for image {}, the media is {} (source.image_sha256 is the media identity psx context reports)",
                b.source.image_sha256, media_id.sha256
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

    let plan = Plan::new(&mechanic, &b, &interfaces, &residents);
    let deadline = std::time::Duration::from_secs(a.timeout);
    let Recorded { steps, params, refusals, timed_out, summary, audit, coverage, analysis } =
        record(&mut m, &plan, a.frames, &events, deadline)?;
    let unobserved = plan.unobserved.clone();
    if timed_out {
        return Ok((false, json!({"status": "timeout", "capture": summary, "written": false})));
    }
    if refusals.count > 0 {
        return Ok((
            false,
            json!({"status": "refused", "capture": summary, "written": false,
                   "refusals": refusals.listed, "refusal_count": refusals.count,
                   "how": "a read gave no value of the field's type: fix the binding (parts, mask, map, stored_frac_bits) or the mechanic's type, then capture again"}),
        ));
    }
    if steps.is_empty() {
        return Ok((
            false,
            json!({"status": "no_steps", "capture": summary, "written": false,
                   "how": "capture.begin never ran with its filters matching: check its pc, overlay and filters, and that the recording reaches the mechanic"}),
        ));
    }
    let header = Header {
        format: psx_mechanics::trace::FORMAT.into(),
        format_version: psx_mechanics::trace::FORMAT_VERSION,
        mechanic: mechanic.id.clone(),
        mechanic_version: mechanic.version,
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
        unobserved,
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

/// What capture reads and where, from the checked files.
pub(crate) struct Plan<'a> {
    state: Fields<'a>,
    unobserved: Vec<String>,
    inputs: Fields<'a>,
    /// Params given as values; those read are added at the first step.
    params: Values,
    param_reads: Fields<'a>,
    specs: Vec<CallSpec<'a>>,
    /// PC -> what it stands for, and the code that must be there.
    sites: HashMap<u32, Vec<(Site, Option<&'a Resident>)>>,
    /// Register filters on begin: (register number, value).
    filters: Vec<(usize, u32)>,
    end: &'a End,
    /// What the read audit takes as accounted for.
    accounts: Accounts<'a>,
    begin: u32,
}

/// Each part's location and size.
fn parts<'a>(f: &Fields<'a>) -> Vec<(&'a Location, u32)> {
    f.iter().flat_map(|(_, r, _)| r.parts.iter().map(|p| (&p.at, p.bytes))).collect()
}

impl<'a> Plan<'a> {
    pub(crate) fn new(
        mechanic: &Mechanic,
        b: &'a Binding,
        interfaces: &[Interface],
        residents: &'a [(u32, Option<Resident>)],
    ) -> Plan<'a> {
        let types = mechanic.types(&mut Vec::new());
        let mut state: Fields = Vec::new();
        let mut unobserved = Vec::new();
        for (name, e) in &b.state {
            match e {
                Encoding::Read(r) => state.push((name, r, types.state[name].clone())),
                Encoding::Unobserved(_) => unobserved.push(name.clone()),
            }
        }
        let inputs: Fields = b.inputs.iter().map(|(n, r)| (n, r, types.inputs[n].clone())).collect();
        let mut params = Values::new();
        let mut param_reads: Fields = Vec::new();
        for (name, p) in &b.params {
            match p {
                Param::Value(v) => {
                    params.insert(name.clone(), v.clone());
                }
                Param::Read(r) => param_reads.push((name, r, types.params[name].clone())),
            }
        }
        let mut specs: Vec<CallSpec> = Vec::new();
        for i in mechanic.required(interfaces) {
            let r = i.reference();
            let call_types = i.types(&mut Vec::new());
            for (name, cb) in &b.interfaces[&r] {
                let ct = &call_types[name];
                let CallBinding { on, request, response } = cb;
                fn typed<'a>(side: &'a BTreeMap<String, Read<Location>>, t: &BTreeMap<String, Type>) -> Fields<'a> {
                    side.iter().map(|(n, r)| (n, r, t[n].clone())).collect()
                }
                specs.push(CallSpec {
                    interface: r.clone(),
                    call: name.clone(),
                    kind: ct.kind,
                    on,
                    request: typed(request, &ct.request),
                    response: typed(response, &ct.response),
                });
            }
        }
        // PC -> what it stands for, and the code that must be there.
        let mut sites: HashMap<u32, Vec<(Site, Option<&Resident>)>> = HashMap::new();
        let resident = |pc: u32| residents.iter().find(|(p, _)| *p == pc).and_then(|(_, r)| r.as_ref());
        sites.entry(b.capture.begin.pc).or_default().push((Site::Begin, resident(b.capture.begin.pc)));
        if let End::At(p) = &b.capture.end {
            sites.entry(p.pc).or_default().push((Site::End, resident(p.pc)));
        }
        for (i, s) in specs.iter().enumerate() {
            let (CallSite::Entry { entry: p } | CallSite::Read { read: p }) = s.on;
            sites.entry(p.pc).or_default().push((Site::Call(i), resident(p.pc)));
        }
        let filters: Vec<(usize, u32)> =
            b.capture.begin.filters.iter().map(|(r, v)| (super::reg_name(r).expect("checked") as usize, *v)).collect();

        // The read audit: what the binding reads or explains is accounted for.
        let explained: Vec<(&Location, u32)> = b.capture.explained.iter().map(|x| (&x.at, x.bytes)).collect();
        let begin_reads: Vec<(&Location, u32)> = [&inputs, &param_reads].into_iter().flat_map(parts).collect();
        let mut all: Vec<(&Location, u32)> =
            [&state, &inputs, &param_reads].into_iter().flat_map(parts).chain(explained.iter().copied()).collect();
        for s in &specs {
            all.extend(parts(&s.request));
            all.extend(parts(&s.response));
        }
        let mut bases: Vec<Reg> = Vec::new();
        for (at, _) in &all {
            if let Location::Base { base, .. } = at {
                if !bases.contains(base) {
                    bases.push(*base);
                }
            }
        }
        let mut registers: u64 = filters.iter().map(|(r, _)| 1u64 << r).sum::<u64>();
        registers |= bases.iter().map(|r| 1u64 << *r as u32).sum::<u64>();
        let mut gte: u64 = 0;
        for (at, _) in begin_reads.iter().chain(&explained) {
            match at {
                Location::Register { register } => registers |= 1 << *register as u32,
                Location::Gte { gte: g } => gte |= 1 << g,
                _ => {}
            }
        }
        // Stack locals read as sp + offset.
        if !bases.contains(&Reg::Sp) {
            bases.push(Reg::Sp);
        }
        let entries: HashSet<u32> = specs
            .iter()
            .filter_map(|s| match s.on {
                CallSite::Entry { entry } => Some(entry.pc),
                CallSite::Read { .. } => None,
            })
            .collect();
        let accounts = Accounts {
            memory: all
                .into_iter()
                .filter(|(at, _)| matches!(at, Location::Address { .. } | Location::Base { .. }))
                .collect(),
            registers,
            gte,
            bases,
            calls: specs.iter().map(|s| s.field("")).collect(),
            entries,
            end: match &b.capture.end {
                End::At(p) => Some(p.pc),
                End::Mode(_) => None,
            },
        };
        Plan {
            state,
            unobserved,
            inputs,
            params,
            param_reads,
            specs,
            sites,
            filters,
            end: &b.capture.end,
            accounts,
            begin: b.capture.begin.pc,
        }
    }
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
    let Plan { state, inputs, param_reads, specs, sites, filters, end, .. } = plan;
    let mut params = plan.params.clone();
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
    let none: Regs = [0; 34];
    let mut tracker = Tracker::new(&plan.accounts, plan.begin);

    while m.bus.frames < target {
        if m.bus.frames != last_frame {
            last_frame = m.bus.frames;
            let rel = m.bus.frames - start_frame;
            if **end == End::Mode(EndMode::Frame) && open.as_ref().is_some_and(|o| o.frame != rel) {
                let o = open.take().unwrap();
                let now = regs(m);
                let view = View { m, now: &now, latched: &o.latched, load: None };
                let after = read_all(state, &view, "state", "end", steps.len() as u64, &mut refusals);
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
            // A store made inside a call to the mechanic's state.
            if let (Some(o), Some(acc)) = (open.as_mut(), info.mem.filter(|a| a.store)) {
                if let Some(p) = &o.pending {
                    let at = crate::run::fold(acc.addr);
                    if let Some((_, _, field)) =
                        o.state_bytes.iter().find(|(s, e, _)| at < *e && at + acc.width as u32 > *s)
                    {
                        let s = &specs[p.spec];
                        refusals.push(
                            steps.len() as u64,
                            "store",
                            s.field(""),
                            format!(
                                "the call changed state.{field} (store at {:#010x} to {:#010x})",
                                info.pc, acc.addr
                            ),
                        );
                    }
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
            let o = open.as_ref().unwrap();
            let p = o.pending.as_ref().unwrap();
            let view = View { m, now: &now, latched: &o.latched, load: None };
            read_all(&specs[p.spec].response, &view, &specs[p.spec].field("response"), "return", n, &mut tentative)
        });
        let pending = open.as_ref().is_some_and(|o| o.pending.is_some()) && !returning;
        let ends = here.iter().any(|s| matches!(s, Site::End)) && open.is_some() && !pending;
        let begins = here.iter().any(|s| matches!(s, Site::Begin)) && !pending;
        let begins = begins && filters.iter().all(|&(r, v)| now[r] == v);
        if here.iter().any(|s| matches!(s, Site::Begin)) && !begins && !pending {
            begins_filtered += 1;
        }
        let after = ends.then(|| {
            let o = open.as_ref().unwrap();
            let view = View { m, now: &now, latched: &o.latched, load: None };
            read_all(state, &view, "state", "end", n, &mut tentative)
        });
        let opening = begins.then(|| {
            let view = View { m, now: &now, latched: &now, load: None };
            let k = n + (open.is_some() && !ends) as u64;
            let first = steps.is_empty() && open.is_none();
            let p = first.then(|| read_all(param_reads, &view, "params", "begin", k, &mut tentative));
            let i = read_all(inputs, &view, "inputs", "begin", k, &mut tentative);
            let s = read_all(state, &view, "state", "begin", k, &mut tentative);
            let bytes: Vec<(u32, u32, String)> = state
                .iter()
                .flat_map(|(name, r, _)| {
                    r.parts.iter().filter_map(move |p| {
                        let a = match p.at {
                            Location::Address { address } => address,
                            Location::Base { base, offset } => now[base as usize].wrapping_add(offset as u32),
                            _ => return None,
                        };
                        let a = crate::run::fold(a);
                        Some((a, a + p.bytes, (*name).clone()))
                    })
                })
                .collect();
            (p, i, s, bytes)
        });
        // Entries: a call starts (inline reads are taken after the load).
        let entry = here.iter().find_map(|s| match s {
            Site::Call(i) if matches!(specs[*i].on, CallSite::Entry { .. }) => Some(*i),
            _ => None,
        });
        let entering = entry.filter(|_| !pending && (open.is_some() || opening.is_some())).map(|i| {
            let latched = match (&opening, &open) {
                (Some(_), _) => &now,
                (None, Some(o)) => &o.latched,
                _ => &none,
            };
            let view = View { m, now: &now, latched, load: None };
            let request = read_all(&specs[i].request, &view, &specs[i].field("request"), "entry", n, &mut tentative);
            Pending { spec: i, request, ret: now[31], sp: now[29] }
        });
        // The world call this instruction belongs to, if any.
        let world = entering
            .as_ref()
            .map(|p| p.spec)
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
        if let Some(r) = response {
            let o = open.as_mut().unwrap();
            let p = o.pending.take().unwrap();
            let s = &specs[p.spec];
            tracker.returned(&parts(&s.response));
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
        if let Some((p, i, s, bytes)) = opening {
            if let Some(o) = open.take() {
                if **end != End::Mode(EndMode::NextBegin) {
                    return Err(Error::invalid(format!(
                        "step {}: capture.begin ran again before capture.end: is the bracket right?",
                        steps.len()
                    ))
                    .with("pc", json!(format!("{pc:#010x}"))));
                }
                // Next begin: the step ends where the next one begins.
                close_with(o, s.clone(), &mut steps, specs, &mut refusals, &mut tracker);
            }
            if let Some(p) = p {
                params.extend(p);
            }
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
        }
        if let (Some(p), Some(o)) = (entering, open.as_mut()) {
            tracker.entry(pc, p.spec, &parts(&specs[p.spec].request), now[29]);
            o.pending = Some(p);
        }
        if open.is_some() {
            tracker.observe(m, &info, world, inline);
        }
        // Inline reads, from the load the instruction made.
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
            let request = read_all(&specs[i].request, &view, &specs[i].field("request"), "load", n, &mut refusals);
            let response = read_all(&specs[i].response, &view, &specs[i].field("response"), "load", n, &mut refusals);
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
    use super::*;

    const MECHANIC: &str = r#"
id = "test.count"
version = 1
title = "Count"
summary = "Adds one, and asks the world."
rate = { per = "event" }
requires = ["world.add@1"]
state.count = { type = "int", bits = 32, signed = true }
state.last = { type = "int", bits = 32, signed = true }
"#;

    const WORLD: &str = r#"
id = "world.add"
version = 1
title = "Adds 100"
calls.add = { kind = "query", request.n = { type = "int", bits = 32, signed = true }, response.m = { type = "int", bits = 32, signed = true } }
calls.peek = { kind = "query", request.at = { type = "int", bits = 32, signed = false }, response.v = { type = "int", bits = 32, signed = true } }
"#;

    const BINDING: &str = r#"
mechanic = "test.count"
mechanic_version = 1
source = { platform = "psx", title = "test", image_sha256 = "0000000000000000000000000000000000000000000000000000000000000000" }
state.count = { parts = [{ at = { base = "a0", offset = 0 }, bytes = 4 }], stored_signed = true }
state.last = { parts = [{ at = { base = "a0", offset = 4 }, bytes = 4 }], stored_signed = true }

[interfaces."world.add@1".add]
on = { entry = { pc = 0x80010200 } }
request.n = { parts = [{ at = { register = "a1" }, bytes = 4 }], stored_signed = true }
response.m = { parts = [{ at = { register = "v0" }, bytes = 4 }], stored_signed = true }

[interfaces."world.add@1".peek]
on = { read = { pc = 0x80010108 } }
request.at = { parts = [{ at = { load = "address" }, bytes = 4 }] }
response.v = { parts = [{ at = { load = "value" }, bytes = 4 }], stored_signed = true }

[capture]
begin = { pc = 0x80010100, a0 = 0x80020000 }
end = { pc = 0x8001012C }
"#;

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

    fn files(binding: &str) -> (Mechanic, Binding, Vec<Interface>) {
        let (m, i) = (Mechanic::parse(MECHANIC).unwrap(), vec![Interface::parse(WORLD).unwrap()]);
        let b: Binding = toml::from_str(binding).unwrap();
        assert_eq!(b.check::<Psx>(&m, &i), Vec::<String>::new());
        (m, b, i)
    }

    fn run(binding: &str, world: &[u32]) -> crate::Result<Recorded> {
        run_patched(binding, world, &[])
    }

    /// With the step's words at these addresses replaced.
    fn run_patched(binding: &str, world: &[u32], patches: &[(u32, u32)]) -> crate::Result<Recorded> {
        let (mechanic, b, interfaces) = files(binding);
        let residents = vec![];
        let plan = Plan::new(&mechanic, &b, &interfaces, &residents);
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
        let r = run(BINDING, &ADD).unwrap();
        assert_eq!((r.refusals.count, r.timed_out), (0, false));
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
    fn end_modes() {
        let r = run(&BINDING.replace("end = { pc = 0x8001012C }", "end = \"next_begin\""), &ADD).unwrap();
        assert_eq!(r.summary["open_step_dropped"], true);
        assert_eq!(r.steps[1].before.get("count"), Some(&int(1)));
        let e = run(&BINDING.replace("0x8001012C", "0x80010300"), &ADD).err().unwrap();
        assert!(e.to_string().contains("capture.begin ran again before capture.end"), "{e}");
    }

    #[test]
    fn filters_pick_the_object() {
        let r = run(&BINDING.replace("a0 = 0x80020000", "a0 = 0x80020004"), &ADD).unwrap();
        assert!(r.steps.is_empty());
        assert!(r.summary["begins_filtered_out"].as_u64() > Some(100));
    }

    #[test]
    fn a_call_that_changes_the_state_is_refused() {
        // world: addiu v0, a1, 100; sw v0, 0(a0); jr ra; nop
        let r = run(BINDING, &[0x24A2_0064, 0xAC82_0000, 0x03E0_0008, 0]).unwrap();
        assert!(r.refusals.count > 0);
        assert_eq!(
            r.refusals.listed[0],
            json!({"step": 0, "when": "store", "field": "world.add@1.add",
                   "problem": "the call changed state.count (store at 0x80010204 to 0x80020000)"})
        );
    }

    #[test]
    fn a_value_that_does_not_fit_is_refused() {
        let r = run(
            &BINDING.replace("offset = 4 }, bytes = 4 }], stored_signed = true", "offset = 4 }, bytes = 4 }]"),
            &ADD,
        );
        assert_eq!(r.unwrap().refusals.count, 0, "unsigned 101.. fits a signed 32-bit int");
        let narrow =
            MECHANIC.replace("state.last = { type = \"int\", bits = 32", "state.last = { type = \"int\", bits = 7");
        let (m, i) = (Mechanic::parse(&narrow).unwrap(), vec![Interface::parse(WORLD).unwrap()]);
        let b: Binding = toml::from_str(BINDING).unwrap();
        let residents = vec![];
        let plan = Plan::new(&m, &b, &i, &residents);
        let r = record(&mut machine(&ADD), &plan, 1, &[], std::time::Duration::from_secs(60)).unwrap();
        assert_eq!(
            r.refusals.listed[0],
            json!({"step": 0, "when": "end", "field": "state.last", "problem": "stored value 101: 101 is outside -64..=63"})
        );
    }

    #[test]
    fn the_audit_lists_what_the_binding_leaves_out() {
        let r = run(BINDING, &ADD).unwrap();
        assert_eq!(r.audit.unaccounted, vec![], "the stack, ra and sp, a0 and the response are accounted for");
        // lw t1, 8(a0) in the load's delay slot, never used: not a read.
        let r = run_patched(BINDING, &ADD, &[(0x8001_010C, 0x8C89_0008)]).unwrap();
        assert_eq!(r.audit.unaccounted, vec![]);
        // Used: addu a1, t1, s0 in the call's delay slot.
        let patches = [(0x8001_010C, 0x8C89_0008), (0x8001_0118, 0x0130_2821)];
        let r = run_patched(BINDING, &ADD, &patches).unwrap();
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
            "explained = [\n  { at = { base = \"a0\", offset = 0x8 }, bytes = 4, why = \"\" },  # read at 0x8001010c\n  { at = { register = \"s0\" }, bytes = 4, why = \"\" },  # read at 0x80010118\n]\n"
        );
        let explained = BINDING.replace(
            "end = { pc = 0x8001012C }",
            "end = { pc = 0x8001012C }\nexplained = [{ at = { base = \"a0\", offset = 8 }, bytes = 4, why = \"t\" }, { at = { register = \"s0\" }, bytes = 4, why = \"t\" }]",
        );
        let r = run_patched(&explained, &ADD, &patches).unwrap();
        assert_eq!(r.audit.unaccounted, vec![]);
    }

    #[test]
    fn a_register_the_call_changed_outside_its_response_is_listed() {
        // world: addiu v0, a1, 100; addiu v1, zero, 7; jr ra; nop. The step stores v1.
        let r =
            run_patched(BINDING, &[0x24A2_0064, 0x2403_0007, 0x03E0_0008, 0], &[(0x8001_011C, 0xAC83_0004)]).unwrap();
        let n = r.steps.len();
        assert_eq!(
            json!(r.audit.unaccounted),
            json!([{"at": {"pc": "0x8001011c", "register": "v1"},
                    "note": format!("{n} of {n} steps, changed by the call world.add@1.add (not in its response)")}])
        );
    }

    #[test]
    fn a_hardware_read_is_explained_or_bound_as_an_inline_read() {
        // lui t1, 0x1F80; lw t2, 0x1108(t1) (timer 0's target); addu a1, t2, t0.
        let patches = [(0x8001_010C, 0x3C09_1F80), (0x8001_0110, 0x8D2A_1108), (0x8001_0118, 0x0148_2821)];
        let r = run_patched(BINDING, &ADD, &patches).unwrap();
        let n = r.steps.len();
        assert_eq!(
            json!(r.audit.unaccounted),
            json!([{"at": {"pc": "0x80010110", "address": "0x1f801108"},
                    "note": format!("4 bytes, {n} of {n} steps, a hardware register: bind it as an inline read if the result depends on it, else explain it")}])
        );
        assert_eq!(
            r.analysis["audit"]["draft"],
            "explained = [\n  { at = { address = 0x1f801108 }, bytes = 4, why = \"\" },  # read at 0x80010110; or bind it as an inline read if the result depends on it\n]\n"
        );
        let explained = BINDING.replace(
            "end = { pc = 0x8001012C }",
            "end = { pc = 0x8001012C }\nexplained = [{ at = { address = 0x1F801108 }, bytes = 4, why = \"t\" }]",
        );
        let r = run_patched(&explained, &ADD, &patches).unwrap();
        assert_eq!(r.audit.unaccounted, vec![]);

        // Bound as the inline read peek: the trace holds what the hardware gave.
        let inline = BINDING.replace("read = { pc = 0x80010108 }", "read = { pc = 0x80010110 }");
        let (mechanic, b, interfaces) = files(&inline);
        let plan = Plan::new(&mechanic, &b, &interfaces, &[]);
        let mut m = machine(&ADD);
        for (at, w) in patches {
            let o = (at & 0x1F_FFFF) as usize;
            m.bus.ram[o..o + 4].copy_from_slice(&w.to_le_bytes());
        }
        m.bus.write(0x1F80_1108, psx_core::Width::Word, 0x1234).unwrap();
        let r = record(&mut m, &plan, 1, &[], std::time::Duration::from_secs(60)).unwrap();
        assert_eq!((r.refusals.count, r.audit.unaccounted.len()), (0, 0));
        let peek = &r.steps[0].calls[0];
        assert_eq!(peek.call, "peek");
        assert_eq!(peek.request, [("at".into(), int(0x1F80_1108))].into());
        assert_eq!(peek.response, Some([("v".into(), int(0x1234))].into()));
    }

    #[test]
    fn coverage_counts_each_way_of_each_branch() {
        let r = run(BINDING, &ADD).unwrap();
        assert_eq!((r.coverage.reached, r.coverage.total), (0, 0));
        // bne a0, zero, +3 in the load's delay slot: always taken, past the call.
        let r = run_patched(BINDING, &ADD, &[(0x8001_010C, 0x1480_0003)]).unwrap();
        assert_eq!((r.coverage.reached, r.coverage.total), (1, 2));
        assert_eq!(json!(r.coverage.missed), json!([{"at": {"pc": "0x8001010c"}, "note": "never fell through"}]));
    }
}
