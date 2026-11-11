//! The debugger (docs/run-plan.md): a session that stops between calls.
//!
//! A debugging session is files, not a process: `.psx/debug/<id>/` holds
//! session.json (how it started, its stops and the one it is at) and, for
//! each stop, a checkpoint (the machine's state, and the session's place,
//! adjustments and driver beside it). Every call loads the stop it is at,
//! acts, and checkpoints a new stop when it moved or changed the machine,
//! so a session outlives the server that started it and going back is
//! loading an earlier stop.

use std::cell::RefCell;
use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::{Duration, Instant};

use psx_core::Machine;
use psx_core::cpu::StepInfo;
use psx_core::state::MediaId;
use psx_media::memmap::hex;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::Error;
use crate::commands::{Ctx, Op, Outcome};
use crate::session::{ByField, Driver, Flow, Observer, Out, Place, Session, Ticks};

#[derive(clap::Subcommand)]
pub enum DebugAction {
    /// Start a session where a run would start (media, state or recording, with the
    /// port's adjustments), stopped before its first field.
    Start(DebugStartArgs),
    /// Go on until a stop holds (pc, write, read, until, tick, fields, ticks,
    /// instructions); a new stop.
    Go(DebugGoArgs),
    /// Look at the stop: registers, memory, quantities, history, the screen, the stops.
    Show(DebugShowArgs),
    /// Write memory or a register, or set the pad, here; a new stop.
    Set(DebugSetArgs),
    /// Go back to an earlier stop (by default the one before).
    Back(DebugBackArgs),
    /// Write the stop's state to a file, to run from with run --from-state.
    Save(DebugSaveArgs),
    /// End a session and remove its files.
    End(DebugEndArgs),
}

impl DebugAction {
    pub fn exec(self, ctx: &Ctx) -> Outcome {
        match self {
            DebugAction::Start(a) => a.exec(ctx),
            DebugAction::Go(a) => a.exec(ctx),
            DebugAction::Show(a) => a.exec(ctx),
            DebugAction::Set(a) => a.exec(ctx),
            DebugAction::Back(a) => a.exec(ctx),
            DebugAction::Save(a) => a.exec(ctx),
            DebugAction::End(a) => a.exec(ctx),
        }
    }
}

/// Which session: by default the latest started.
#[derive(clap::Args, Clone, Debug, Default, Deserialize, schemars::JsonSchema)]
pub struct Which {
    /// Default: the latest session.
    #[arg(long)]
    #[serde(default)]
    pub id: Option<String>,
}

#[derive(clap::Args, Clone, Debug, Deserialize, Serialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DebugStartArgs {
    /// Disc or EXE, as run's.
    pub media: Option<PathBuf>,
    /// As run's.
    #[arg(long)]
    #[serde(default)]
    pub bios: Option<PathBuf>,
    /// A named state or state file.
    #[arg(long)]
    #[serde(default)]
    pub from_state: Option<PathBuf>,
    /// `SCENARIO/NAME`, as run's.
    #[arg(long)]
    #[serde(default)]
    pub recording: Option<String>,
    /// A JSON file of pad states by field, as run's replay.
    #[arg(long)]
    #[serde(default)]
    pub replay: Option<PathBuf>,
    /// As run's (default latch, steady, fix where each applies).
    #[arg(long = "adjust", value_delimiter = ',')]
    #[serde(default)]
    pub adjust: Option<Vec<String>>,
    /// As run's.
    #[arg(long = "fix-build")]
    #[serde(default)]
    pub fix_build: Option<PathBuf>,
    /// As run's.
    #[arg(long, num_args = 0..=1, require_equals = true, value_delimiter = ',')]
    #[serde(default)]
    pub decomp: Option<Vec<String>>,
}

#[derive(clap::Args, Clone, Debug, Default, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DebugGoArgs {
    #[command(flatten)]
    #[serde(flatten)]
    pub which: Which,
    /// About to run ADDR or a name.
    #[arg(long)]
    #[serde(default)]
    pub pc: Option<String>,
    /// A CPU store into `ADDR[:WIDTH]` or `LO-HI`.
    #[arg(long)]
    #[serde(default)]
    pub write: Option<String>,
    /// A CPU load, as write.
    #[arg(long)]
    #[serde(default)]
    pub read: Option<String>,
    /// As run's.
    #[arg(long)]
    #[serde(default)]
    pub until: Vec<String>,
    /// The binding's tick ends.
    #[arg(long)]
    #[serde(default)]
    pub tick: bool,
    /// After N fields.
    #[arg(long)]
    #[serde(default)]
    pub fields: Option<u64>,
    /// After N ticks.
    #[arg(long)]
    #[serde(default)]
    pub ticks: Option<u64>,
    /// After N instructions.
    #[arg(long)]
    #[serde(default)]
    pub instructions: Option<u64>,
    /// The end of the first whole field from here that draws: any (fills
    /// too), primitives or textured. Captures it as gpu does.
    #[arg(long, value_enum)]
    #[serde(default)]
    pub draw: Option<crate::run_native::gpu_capture::GpuDrawFilter>,
    /// Capture the GPU commands of the first whole field from here
    /// (debug_show gpu).
    #[arg(long)]
    #[serde(default)]
    pub gpu: bool,
    /// Trace the instructions run to the stop, as run's trace does.
    #[arg(long)]
    #[serde(default)]
    pub trace: bool,
    /// Keep the last N instructions.
    #[arg(long)]
    #[serde(default)]
    pub history: Option<usize>,
    /// As run's, to the stop.
    #[arg(long = "sample")]
    #[serde(default, rename = "sample")]
    pub samples: Vec<crate::ram::SampleSpec>,
    #[arg(long)]
    #[serde(default)]
    pub sample_every: Option<u64>,
    #[arg(long, value_enum)]
    #[serde(default)]
    pub sample_per: Option<crate::ram::SamplePer>,
    /// As run's, to the stop.
    #[arg(long)]
    #[serde(default)]
    pub watch: Vec<String>,
    #[arg(long)]
    #[serde(default)]
    pub watch_changes_only: bool,
    /// As run's, to the stop.
    #[arg(long)]
    #[serde(default)]
    pub watch_reads: Vec<String>,
    /// At most this many fields (3600).
    #[arg(long)]
    #[serde(default)]
    pub limit: Option<u64>,
    /// Seconds (120).
    #[arg(long)]
    #[serde(default)]
    pub timeout: Option<u64>,
}

#[derive(clap::Args, Clone, Debug, Default, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DebugShowArgs {
    #[command(flatten)]
    #[serde(flatten)]
    pub which: Which,
    /// The default.
    #[arg(long)]
    #[serde(default)]
    pub registers: bool,
    /// As run's sample.
    #[arg(long = "value")]
    #[serde(default, rename = "value")]
    pub values: Vec<crate::ram::SampleSpec>,
    /// Hex: `ADDR[:LEN]` or `LO-HI` (4096 at most).
    #[arg(long)]
    #[serde(default)]
    pub bytes: Option<String>,
    #[arg(long)]
    #[serde(default)]
    pub history: bool,
    #[arg(long)]
    #[serde(default)]
    pub screen: bool,
    #[arg(long)]
    #[serde(default)]
    pub stops: bool,
    /// The last GPU capture (debug_go gpu or draw).
    #[arg(long)]
    #[serde(default)]
    pub gpu: bool,
    /// CD, SPU, unhandled I/O, interrupts; the next two fields' hot PCs.
    #[arg(long)]
    #[serde(default)]
    pub hardware: bool,
    /// Calls the stop is in.
    #[arg(long)]
    #[serde(default)]
    pub stack: bool,
}

#[derive(clap::Args, Clone, Debug, Default, Deserialize, Serialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DebugSetArgs {
    #[command(flatten)]
    #[serde(flatten, skip_serializing)]
    pub which: Which,
    /// `ADDR=VALUE[:WIDTH|:FMT]`.
    #[arg(long)]
    #[serde(default)]
    pub poke: Vec<String>,
    /// `NAME=VALUE`: pc, hi, lo, a register.
    #[arg(long)]
    #[serde(default)]
    pub register: Vec<String>,
    /// Buttons to hold from here ([] releases).
    #[arg(long, value_delimiter = ',', num_args = 0..)]
    #[serde(default)]
    pub pad: Option<Vec<String>>,
    /// With pad: left x/y, right x/y.
    #[arg(long, value_delimiter = ',', num_args = 4)]
    #[serde(default)]
    pub sticks: Option<Vec<u8>>,
}

#[derive(clap::Args, Clone, Debug, Default, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DebugBackArgs {
    #[command(flatten)]
    #[serde(flatten)]
    pub which: Which,
    /// Default: the stop this came from.
    #[arg(long)]
    #[serde(default)]
    pub to: Option<usize>,
}

#[derive(clap::Args, Clone, Debug, Default, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DebugSaveArgs {
    #[command(flatten)]
    #[serde(flatten)]
    pub which: Which,
    /// Default: state-N.bin in the session.
    #[arg(long)]
    #[serde(default)]
    pub out: Option<PathBuf>,
}

#[derive(clap::Args, Clone, Debug, Default, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DebugEndArgs {
    #[command(flatten)]
    #[serde(flatten)]
    pub which: Which,
}

/// session.json: how the session started, its stops, and where it is.
#[derive(Serialize, Deserialize)]
struct Book {
    media: Option<PathBuf>,
    bios: PathBuf,
    media_id: Option<MediaId>,
    /// The recording it runs, whose ticks drive the pad; else the input by field.
    recording: Option<String>,
    adjust: Vec<String>,
    start: Value,
    stops: Vec<Value>,
    current: usize,
}

/// What a checkpoint keeps beside the machine's state.
#[derive(Serialize, Deserialize)]
struct Point {
    place: Place,
    adjust: Option<crate::adjust::Running>,
    driver: Value,
}

fn sessions(root: &Path) -> PathBuf {
    root.join(".psx/debug")
}

/// The session's directory: `which`'s, or the latest started.
fn dir_of(root: &Path, which: &Which) -> crate::Result<PathBuf> {
    let base = sessions(root);
    let id = match &which.id {
        Some(id) => {
            if id.is_empty() || id.contains(['/', '\\']) || id.starts_with('.') {
                return Err(Error::invalid(format!("id {id:?}: a session id, as debug_start returned it")));
            }
            id.clone()
        }
        None => {
            let mut ids: Vec<String> = std::fs::read_dir(&base)
                .into_iter()
                .flatten()
                .flatten()
                .filter(|e| e.path().join("session.json").is_file())
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .collect();
            // Ids begin with the time they started.
            ids.sort_by_key(|i| (i.split('-').next().and_then(|t| t.parse::<u128>().ok()), i.clone()));
            ids.pop().ok_or_else(|| Error::invalid("no debugging session: debug_start starts one"))?
        }
    };
    let dir = base.join(&id);
    if !dir.join("session.json").is_file() {
        return Err(Error::invalid(format!("no debugging session {id}: debug_start starts one")));
    }
    Ok(dir)
}

fn read_json<T: serde::de::DeserializeOwned>(p: &Path) -> crate::Result<T> {
    let text = std::fs::read_to_string(p).map_err(|e| Error::io(p, e))?;
    serde_json::from_str(&text).map_err(|e| Error::malformed(format!("{}: {e}", p.display())))
}

fn write_json(p: &Path, v: &impl Serialize) -> crate::Result<()> {
    std::fs::write(p, serde_json::to_vec_pretty(v).unwrap_or_default()).map_err(|e| Error::io(p, e))
}

/// A session loaded at its current stop.
struct Open {
    dir: PathBuf,
    id: String,
    book: Book,
    s: Session<'static>,
    scopes: Vec<crate::ScopedSymbols>,
}

fn stop_file(dir: &Path, n: usize, ext: &str) -> PathBuf {
    dir.join("stops").join(format!("{n}.{ext}"))
}

/// The files a go wrote on its way to stop `n` (GPU capture, trace).
fn stop_dir(dir: &Path, n: usize) -> PathBuf {
    dir.join("stops").join(n.to_string())
}

impl Open {
    fn load(root: &Path, which: &Which) -> crate::Result<Open> {
        let dir = dir_of(root, which)?;
        let book: Book = read_json(&dir.join("session.json"))?;
        let bios = std::fs::read(&book.bios).map_err(|e| Error::io(&book.bios, e))?;
        let medium = crate::run_native::open_media(book.media.as_deref())?;
        let n = book.current;
        let state = stop_file(&dir, n, "bin");
        let bytes = std::fs::read(&state).map_err(|e| Error::io(&state, e))?;
        let (_, mut m) = Machine::load_state(&bytes, bios).map_err(|e| Error::new("invalid_state", e))?;
        // The drive's state came with the machine's.
        m.bus.cdrom.disc = medium.disc;
        // A state leaves out what the decomp and the adjustments put on the machine.
        let decomp = dir.join("decomp.json");
        if decomp.is_file() {
            read_json::<crate::decomp::Replacement>(&decomp)?.install(&mut m);
        }
        let p: Point = read_json(&stop_file(&dir, n, "json"))?;
        if let Some(a) = &p.adjust {
            a.reinstall(&mut m);
        }
        let bad = |e: serde_json::Error| Error::malformed(format!("stop {n}'s driver: {e}"));
        let driver: Box<dyn Driver> = match &book.recording {
            Some(_) => Box::new(serde_json::from_value::<Ticks>(p.driver).map_err(bad)?),
            None => Box::new(serde_json::from_value::<ByField>(p.driver).map_err(bad)?),
        };
        let mut s = Session::new(m, p.place.start, driver);
        s.set_place(p.place);
        s.adjust = p.adjust;
        let scopes = crate::load_symbols(&[])?;
        let id = dir.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        Ok(Open { dir, id, book, s, scopes })
    }

    /// Checkpoint a new stop here, come to from the current one by `call`.
    fn stop(&mut self, why: &str, call: Value, detail: Value) -> crate::Result<Value> {
        let n = self.book.stops.len();
        let from = self.book.stops.get(self.book.current).map(|_| self.book.current);
        let state = self.s.m.save_state(self.book.media_id.clone());
        let p = stop_file(&self.dir, n, "bin");
        std::fs::write(&p, &state).map_err(|e| Error::io(&p, e))?;
        let point = Point { place: self.s.place(), adjust: self.s.adjust.take(), driver: self.s.driver.save() };
        write_json(&stop_file(&self.dir, n, "json"), &point)?;
        self.s.adjust = point.adjust;
        let mut v = self.here();
        v["stop"] = json!(n);
        v["from"] = json!(from);
        v["why"] = json!(why);
        if !detail.is_null() {
            v["detail"] = detail;
        }
        v["call"] = call;
        self.book.stops.push(v.clone());
        self.book.current = n;
        write_json(&self.dir.join("session.json"), &self.book)?;
        Ok(v)
    }

    /// Where the machine is: fields, ticks, instructions, the PC and its function.
    fn here(&self) -> Value {
        let m = &self.s.m;
        let pc = m.cpu.pc;
        let function = crate::name_pc(&self.scopes, pc, Some(&m.bus.ram))
            .map(|(name, check, source)| json!({"name": name, "exe": source, "check": check.as_str()}));
        let instruction = word_at(m, pc).map(|w| psx_core::disasm::disasm(w, pc));
        json!({
            "frame": m.bus.frames, "run_frame": self.s.rel(), "ticks": self.s.ticks,
            "instructions": self.s.steps, "pc": hex(pc), "function": function, "instruction": instruction,
        })
    }

    fn current(&self) -> Value {
        self.book.stops[self.book.current].clone()
    }

    fn result(&self, mut v: Value) -> Value {
        v["id"] = json!(self.id);
        v
    }
}

/// The instruction word at `pc`, in RAM or the BIOS.
fn word_at(m: &Machine, pc: u32) -> Option<u32> {
    let p = pc & 0x1FFF_FFFF;
    let (mem, o) = match p {
        0..0x0080_0000 => (&m.bus.ram[..], (p & 0x1F_FFFF) as usize),
        0x1FC0_0000..0x1FC8_0000 => (&m.bus.bios[..], (p - 0x1FC0_0000) as usize),
        _ => return None,
    };
    mem.get(o..o + 4).map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
}

/// A word of code or data: RAM, BIOS or the scratchpad.
fn data_word(m: &Machine, addr: u32) -> Option<u32> {
    match addr & 0x1FFF_FFFF {
        p @ 0x1F80_0000..0x1F80_0400 => {
            let o = (p - 0x1F80_0000) as usize;
            m.bus.scratch.get(o..o + 4).map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
        }
        _ => word_at(m, addr),
    }
}

/// One call on the stack: where it is, its sp and frame size (None
/// without one), and where it returns to.
#[derive(Debug, PartialEq)]
struct Frame {
    pc: u32,
    sp: u32,
    size: Option<u32>,
    returns_to: Option<u32>,
}

/// Walks the stack from a stop by the functions' prologues: back from each
/// PC to its `addiu sp,sp,-N` (a `jr ra` or `addiu sp,sp,+N` first means
/// no frame, ra still in its register), then on to the `sw ra,off(sp)`
/// before the PC for the return address. Each caller's PC is its call
/// (ra-8). Says why it ended.
fn unwind(read: impl Fn(u32) -> Option<u32>, pc: u32, sp: u32, ra: u32) -> (Vec<Frame>, String) {
    const DEPTH: usize = 32;
    const SCAN: u32 = 4096;
    let (mut pc, mut sp, mut ra) = (pc, sp, Some(ra));
    let mut frames = Vec::new();
    loop {
        let mut prologue = None;
        for i in 1..=SCAN {
            let a = pc.wrapping_sub(4 * i);
            let Some(w) = read(a) else { break };
            if w == 0x03E0_0008 {
                break;
            }
            if w >> 16 == 0x27BD {
                if (w as i16) < 0 {
                    prologue = Some((a, (w as i16).unsigned_abs() as u32));
                }
                break;
            }
        }
        let (size, saved) = match prologue {
            Some((at, size)) => {
                let saved =
                    (at..pc).step_by(4).find_map(|a| read(a).filter(|w| w >> 16 == 0xAFBF).map(|w| w as u16 as u32));
                (Some(size), saved)
            }
            None => (None, None),
        };
        // An outer call without its own saved ra cannot be followed.
        let back = match (saved, frames.is_empty()) {
            (Some(off), _) => read(sp.wrapping_add(off)),
            (None, true) => ra.take(),
            (None, false) => None,
        };
        frames.push(Frame { pc, sp, size, returns_to: back });
        let Some(back) = back else {
            let why = if size.is_some() { "no saved ra in the frame" } else { "no frame and ra already used" };
            return (frames, why.into());
        };
        let call = back.wrapping_sub(8);
        let is_call = read(call).is_some_and(|w| w >> 26 == 3 || (w >> 26 == 0 && w & 0x3F == 9));
        if !is_call {
            return (frames, format!("{} is not after a call", hex(back)));
        }
        if frames.len() == DEPTH {
            return (frames, format!("{DEPTH} frames"));
        }
        sp = sp.wrapping_add(size.unwrap_or(0));
        pc = call;
    }
}

impl Op for DebugStartArgs {
    fn exec(self, ctx: &Ctx) -> Outcome {
        Outcome::result(start(ctx, self))
    }
}

fn start(ctx: &Ctx, a: DebugStartArgs) -> crate::Result<Value> {
    let given = serde_json::to_value(&a).unwrap_or_default();
    let run: crate::run::RunCmdArgs =
        serde_json::from_value(given.clone()).map_err(|e| Error::invalid(format!("debug_start: {e}")))?;
    let mut r = run.start_args(ctx)?;
    // Nothing named a state: say so, since a mistyped argument lands here.
    let power_on = r.from_state.is_none().then(|| match &r.media {
        Some(m) => format!("power-on with {}: no recording or from_state given", m.display()),
        None => "power-on with no disc: no recording or from_state given".to_string(),
    });
    let events = crate::replay::resolve(r.replay.as_deref(), r.input.as_ref(), u64::MAX).map_err(Error::invalid)?;
    let b = crate::run_native::begin(&ctx.root, &mut r, &events)?;
    let unavailable = b.unavailable().cloned();
    let fix = b.fix_detail();
    let crate::run_native::Begun { mut s, media_id, decomp, recording, adjusted, .. } = b;
    s.driver = match &recording {
        Some((name, pads)) => Box::new(Ticks { recording: name.clone(), pads: pads.clone() }),
        None => Box::new(ByField { events, next: 0 }),
    };
    let id = crate::stamp();
    let dir = sessions(&ctx.root).join(&id);
    std::fs::create_dir_all(dir.join("stops")).map_err(|e| Error::io(&dir, e))?;
    if let Some(d) = &decomp {
        write_json(&dir.join("decomp.json"), d)?;
    }
    if let Some(f) = &fix {
        // The fix build it runs, as a run keeps it (fix_build runs it again).
        write_json(&dir.join("fix.json"), f)?;
    }
    let book = Book {
        media: r.media.clone(),
        bios: r.bios.clone(),
        media_id,
        recording: recording.map(|(name, _)| name),
        adjust: adjusted.unwrap_or_default().into_iter().map(String::from).collect(),
        start: given,
        stops: Vec::new(),
        current: 0,
    };
    let scopes = crate::load_symbols(&[])?;
    let mut o = Open { dir: dir.clone(), id, book, s, scopes };
    let mut v = o.stop("start", Value::Null, Value::Null)?;
    v["dir"] = json!(dir);
    v["adjust"] = json!(o.book.adjust);
    if let Some(u) = unavailable {
        v["adjust_unavailable"] = Value::Object(u);
    }
    if let Some(r) = &o.book.recording {
        v["recording"] = json!(r);
    }
    if let Some(p) = power_on {
        v["started"] = json!(p);
    }
    Ok(o.result(v))
}

/// What a stop found, read back once the advance ends.
#[derive(Default)]
struct Found {
    detail: Value,
    history: VecDeque<(u32, u32)>,
}

/// debug_go's stops, as an observer of the session.
struct Stops {
    pc: Option<u32>,
    write: Option<(u32, u32)>,
    read: Option<(u32, u32)>,
    until: Vec<crate::until::Until>,
    tick: bool,
    ticks: Option<u64>,
    instructions: Option<u64>,
    history: usize,
    steps: u64,
    ticked: u64,
    found: Rc<RefCell<Found>>,
}

impl Observer for Stops {
    fn per_step(&self) -> bool {
        self.pc.is_some()
            || self.write.is_some()
            || self.read.is_some()
            || self.tick
            || self.ticks.is_some()
            || self.instructions.is_some()
            || self.history > 0
    }

    fn field(&mut self, _rel: u64, m: &mut Machine) -> crate::Result<Flow> {
        if !self.until.is_empty() && self.until.iter().all(|u| u.holds(&m.bus.ram, &m.bus.scratch)) {
            return Ok(Flow::Stop("until"));
        }
        Ok(Flow::Go)
    }

    fn step(&mut self, info: &StepInfo, m: &mut Machine) -> Flow {
        self.steps += 1;
        if self.history > 0 {
            let h = &mut self.found.borrow_mut().history;
            if h.len() == self.history {
                h.pop_front();
            }
            h.push_back((info.pc, info.instruction));
        }
        if let Some(acc) = info.mem {
            let p = crate::run::fold(acc.addr);
            let range = if acc.store { self.write } else { self.read };
            if range.is_some_and(|(lo, hi)| p < hi && p + acc.width as u32 > lo) {
                self.found.borrow_mut().detail = json!({
                    "by": hex(info.pc), "address": hex(acc.addr), "width": acc.width,
                    "value": hex(acc.value),
                });
                return Flow::Stop(if acc.store { "write" } else { "read" });
            }
        }
        if self.instructions.is_some_and(|n| self.steps >= n) {
            return Flow::Stop("instructions");
        }
        if self.pc.is_some_and(|pc| pc == m.cpu.pc) {
            return Flow::Stop("pc");
        }
        Flow::Go
    }

    fn tick(&mut self, _rel: u64, _m: &mut Machine) -> Flow {
        self.ticked += 1;
        if self.tick {
            return Flow::Stop("tick");
        }
        if self.ticks.is_some_and(|n| self.ticked >= n) {
            return Flow::Stop("ticks");
        }
        Flow::Go
    }

    fn finish(self: Box<Self>, _m: &Machine, _out: &mut Out) -> crate::Result<()> {
        Ok(())
    }
}

impl Op for DebugGoArgs {
    fn exec(self, ctx: &Ctx) -> Outcome {
        Outcome::result(go(ctx, self))
    }
}

fn go(ctx: &Ctx, a: DebugGoArgs) -> crate::Result<Value> {
    let mut o = Open::load(&ctx.root, &a.which)?;
    let mem = (&o.s.m.bus.ram[..], &o.s.m.bus.scratch[..]);
    let range = |s: &Option<String>, what: &str| {
        s.as_deref()
            .map(|s| crate::run::parse_range_at(s, Some(mem)).map_err(|e| Error::invalid(format!("{what}: {e}"))))
            .transpose()
    };
    let pc = match &a.pc {
        Some(p) => Some(crate::run_native::resolve(p, &o.scopes)?.0),
        None => None,
    };
    let until = a.until.iter().map(|s| crate::until::parse(s)).collect::<Result<Vec<_>, _>>();
    let stops = Stops {
        pc,
        write: range(&a.write, "write")?,
        read: range(&a.read, "read")?,
        until: until.map_err(Error::invalid)?,
        tick: a.tick,
        ticks: a.ticks.filter(|&n| n > 0),
        instructions: a.instructions.filter(|&n| n > 0),
        history: a.history.unwrap_or(0).min(100_000),
        steps: 0,
        ticked: 0,
        found: Rc::default(),
    };
    let watched = stops.per_step() || !stops.until.is_empty() || a.fields.is_some() || a.draw.is_some();
    if !watched {
        return Err(Error::invalid(
            "debug_go: name a stop (pc, write, read, until, tick, draw, fields, ticks or instructions)",
        ));
    }
    if (a.tick || a.ticks.is_some()) && o.s.tick.is_none() {
        let q = crate::run_native::glossary_quantities(&ctx.root, &o.book.media_id)?;
        o.s.tick = Some(q.binding.described().tick.clone());
    }
    let found = stops.found.clone();
    let limit = a.limit.unwrap_or(3600).max(1);
    let fields = a.fields.map_or(limit, |f| f.min(limit));
    // What the go keeps on its way goes beside the stop it makes.
    let n = o.book.stops.len();
    let kept = stop_dir(&o.dir, n);
    let keeps = a.gpu
        || a.draw.is_some()
        || a.trace
        || !a.samples.is_empty()
        || !a.watch.is_empty()
        || !a.watch_reads.is_empty();
    if keeps {
        let _ = std::fs::remove_dir_all(&kept);
        std::fs::create_dir_all(&kept).map_err(|e| Error::io(&kept, e))?;
    }
    // A stop at a field's start (until, draw) leaves that field's look done:
    // the observers that look at fields take it here, as the first from here.
    let place = o.s.place();
    let at_field = place.driver_due && place.last_field == o.s.m.bus.frames;
    let scopes = Rc::new(crate::load_symbols(&[])?);
    const MAX_EVENTS: usize = 100_000;
    if !a.samples.is_empty() {
        let mut sampler = sampler(ctx, &mut o, &a)?;
        if at_field {
            sampler.field(o.s.rel(), &o.s.m, true);
        }
        // Before the stops, so a stop at a boundary has its row.
        o.s.observe(crate::run_native::observe::Samples(sampler));
    }
    o.s.observe(stops);
    if !a.watch.is_empty() {
        crate::run_native::watch(&mut o.s.m, &a.watch, MAX_EVENTS)?;
        let changes_only = a.watch_changes_only;
        o.s.observe(crate::run_native::observe::Writes { enabled: true, changes_only, scopes: scopes.clone() });
    }
    if !a.watch_reads.is_empty() {
        let mem = Some((&o.s.m.bus.ram[..], &o.s.m.bus.scratch[..]));
        let ranges = a.watch_reads.iter().map(|s| crate::run::parse_range_at(s, mem)).collect::<Result<Vec<_>, _>>();
        o.s.observe(crate::run_native::observe::Reads {
            ranges: ranges.map_err(|e| Error::invalid(format!("watch_reads: {e}")))?,
            max: MAX_EVENTS,
            start: o.s.start,
            scopes: scopes.clone(),
            reads: Vec::new(),
            hits: 0,
            readers: Default::default(),
        });
    }
    if a.gpu || a.draw.is_some() {
        let n = u32::try_from(fields).unwrap_or(u32::MAX);
        let capture = crate::run_native::gpu_capture::GpuCapture::new(0, a.draw, n, n);
        let mut gpu = crate::run_native::observe::Gpu { capture, start: None, stop: a.draw.is_some() };
        if at_field {
            gpu.field(o.s.rel(), &mut o.s.m)?;
        }
        o.s.observe(gpu);
    }
    if a.trace {
        let trace = crate::run::TraceArgs { trace: true, ..Default::default() };
        let t = crate::run_native::tracer(&trace, 200_000, &scopes, o.s.m.bus.frames, &kept)?;
        let t = crate::run_native::observe::Trace::new(&mut o.s.m, t, scopes);
        o.s.observe(t);
    }
    let target = o.s.m.bus.frames + fields;
    let deadline = Duration::from_secs(a.timeout.unwrap_or(120));
    let ended = o.s.advance(target, Instant::now(), deadline)?;
    let why = match ended {
        crate::session::Ended::Limit if a.fields.is_some_and(|f| f <= limit) => "fields",
        crate::session::Ended::Limit => "limit",
        e => e.termination(),
    };
    let found = std::mem::take(&mut *found.borrow_mut());
    let mut detail = found.detail;
    if keeps {
        // Ended where a field starts: its look is the last one.
        let place = o.s.place();
        let complete =
            matches!(ended, crate::session::Ended::Limit) || (place.driver_due && place.last_field == o.s.m.bus.frames);
        let rel = o.s.rel();
        for ob in &mut o.s.observers {
            ob.last_field(rel, &mut o.s.m, complete)?;
        }
        let (mut files, mut report) = (serde_json::Map::new(), serde_json::Map::new());
        let start = o.s.start;
        let mut out = Out { dir: &kept, files: &mut files, report: &mut report, start, termination: why };
        for ob in std::mem::take(&mut o.s.observers) {
            ob.finish(&o.s.m, &mut out)?;
        }
        report.insert("files".into(), Value::Object(files));
        write_json(&kept.join("report.json"), &report)?;
        if detail.is_null() {
            detail = json!({});
        }
        detail["dir"] = json!(kept);
        if let Some(g) = report.get("gpu_frame") {
            let keys = ["status", "reason", "absolute_frame", "complete", "draw_commands", "content", "commands"];
            detail["gpu"] =
                Value::Object(keys.iter().filter_map(|&k| g.get(k).map(|v| (k.into(), v.clone()))).collect());
        }
        for (k, keys) in [
            ("samples", &["rows", "file"][..]),
            ("watch", &["hits", "recorded", "complete", "file", "writers", "first"]),
            ("read_watch", &["hits", "recorded", "complete", "file", "readers"]),
        ] {
            if let Some(r) = report.get(k) {
                let mut r: serde_json::Map<String, Value> =
                    keys.iter().filter_map(|&k| r.get(k).map(|v| (k.into(), v.clone()))).collect();
                if let Some(Value::Array(first)) = r.get_mut("first") {
                    first.truncate(8);
                }
                for list in ["readers", "writers"] {
                    if let Some(Value::Array(l)) = r.get_mut(list) {
                        l.truncate(8);
                    }
                }
                detail[k] = Value::Object(r);
            }
        }
        if let Some(t) = report.get("trace") {
            detail["trace"] = json!({"instructions": t["instructions"], "lines": t["lines"],
                "lines_dropped": t["lines_dropped"], "file": kept.join("trace.txt")});
        }
    }
    o.s.observers.clear();
    let sampled: Vec<String> = a.samples.iter().filter_map(|s| s.parse().ok()).map(|c| c.name().to_string()).collect();
    let call = json!({"go": {"pc": a.pc, "write": a.write, "read": a.read, "until": a.until, "tick": a.tick,
        "draw": a.draw, "gpu": a.gpu, "trace": a.trace, "sample": sampled, "watch": a.watch,
        "watch_reads": a.watch_reads,
        "fields": a.fields, "ticks": a.ticks, "instructions": a.instructions, "limit": a.limit}});
    let v = o.stop(why, call, detail)?;
    if !found.history.is_empty() {
        let lines: Vec<Value> = found
            .history
            .iter()
            .map(|&(pc, w)| {
                let f = crate::name_pc(&o.scopes, pc, Some(&o.s.m.bus.ram)).map(|n| n.0);
                json!({"pc": hex(pc), "word": hex(w), "instruction": psx_core::disasm::disasm(w, pc), "function": f})
            })
            .collect();
        let n = o.book.current;
        write_json(&stop_file(&o.dir, n, "history.json"), &lines)?;
    }
    Ok(o.result(v))
}

impl Op for DebugShowArgs {
    fn exec(self, ctx: &Ctx) -> Outcome {
        Outcome::result(show(ctx, self))
    }
}

fn show(ctx: &Ctx, a: DebugShowArgs) -> crate::Result<Value> {
    let mut o = Open::load(&ctx.root, &a.which)?;
    let mut v = json!({"at": o.current()});
    let nothing = a.values.is_empty()
        && a.bytes.is_none()
        && !a.history
        && !a.screen
        && !a.stops
        && !a.gpu
        && !a.hardware
        && !a.stack;
    if a.registers || nothing {
        v["registers"] = Value::Object(crate::run_native::registers(&o.s.m));
    }
    if !a.values.is_empty() {
        v["values"] = values(ctx, &mut o, &a.values)?;
    }
    if let Some(b) = &a.bytes {
        v["bytes"] = bytes(&o.s.m, b)?;
    }
    if a.history {
        let p = stop_file(&o.dir, o.book.current, "history.json");
        v["history"] = if p.is_file() {
            read_json::<Value>(&p)?
        } else {
            json!("none kept: debug_go history=N keeps the last N instructions to a stop")
        };
    }
    if a.screen {
        let (w, h, rgba) = o.s.m.bus.gpu.display_rgba();
        let p = stop_file(&o.dir, o.book.current, "screen.png");
        crate::run::write_png(&p, w, h, &rgba, png::ColorType::Rgba).map_err(|e| Error::io(&p, e))?;
        v["screen"] = json!(p);
    }
    if a.stops {
        v["stops"] = json!(o.book.stops);
        v["current"] = json!(o.book.current);
    }
    if a.gpu {
        v["gpu"] = gpu_capture(&o)?;
    }
    if a.stack {
        let m = &o.s.m;
        let (frames, ended) = unwind(|addr| data_word(m, addr), m.cpu.pc, m.cpu.regs[29], m.cpu.regs[31]);
        let frames: Vec<Value> = frames
            .iter()
            .map(|f| {
                let mut v = json!({"pc": hex(f.pc), "function": crate::name_pc(&o.scopes, f.pc, Some(&m.bus.ram)).map(|n| n.0),
                    "sp": hex(f.sp), "frame": f.size});
                if let Some(ra) = f.returns_to {
                    v["returns_to"] = json!(hex(ra));
                }
                v
            })
            .collect();
        v["stack"] = json!({"frames": frames, "ended": ended});
    }
    if a.hardware {
        // Last: the two fields it looks ahead run on this copy of the stop.
        let disc = o.book.media.clone();
        let mut h = crate::native::hardware(&mut o.s.m, disc.as_deref());
        h["next_fields"] = crate::native::hot(&mut o.s.m, 2);
        v["hardware"] = h;
    }
    Ok(o.result(v))
}

/// The GPU capture of the go that came to this stop, or the latest before it.
fn gpu_capture(o: &Open) -> crate::Result<Value> {
    let mut at = Some(o.book.current);
    while let Some(n) = at {
        let dir = stop_dir(&o.dir, n);
        let report = dir.join("report.json");
        if report.is_file() {
            let r: Value = read_json(&report)?;
            if !r["gpu_frame"].is_null() {
                let files: Vec<&String> = r["files"].as_object().map(|f| f.keys().collect()).unwrap_or_default();
                return Ok(json!({"stop": n, "dir": dir, "capture": r["gpu_frame"], "files": files,
                    "textures": "assets_textures with run_dir = dir decodes its texture pages"}));
            }
        }
        at = o.book.stops[n]["from"].as_u64().map(|f| f as usize);
    }
    Ok(json!("none: debug_go gpu or draw captures a field's GPU commands"))
}

/// The go's sampler: rows per field (or per tick, which takes the
/// binding's tick) from here.
fn sampler(ctx: &Ctx, o: &mut Open, a: &DebugGoArgs) -> crate::Result<crate::ram::Sampler> {
    use crate::ram::SamplePer;
    let cols = a.samples.iter().map(|s| s.parse()).collect::<Result<Vec<_>, _>>().map_err(Error::invalid)?;
    if a.sample_every == Some(0) {
        return Err(Error::invalid("sample_every takes N >= 1"));
    }
    let per = a.sample_per.unwrap_or_default();
    let (cols, q) = crate::ram::with_glossary(cols, per == SamplePer::Tick, || {
        crate::run_native::glossary_quantities(&ctx.root, &o.book.media_id)
    })?;
    if per == SamplePer::Tick && o.s.tick.is_none() {
        o.s.tick = q.as_ref().map(|q| q.binding.described().tick.clone());
    }
    Ok(crate::ram::Sampler::new(cols, a.sample_every.unwrap_or(1), per, q))
}

/// Each value now, by name, as sample would write it.
fn values(ctx: &Ctx, o: &mut Open, specs: &[crate::ram::SampleSpec]) -> crate::Result<Value> {
    let cols = specs.iter().map(|s| s.parse()).collect::<Result<Vec<_>, _>>().map_err(Error::invalid)?;
    let (cols, q) =
        crate::ram::with_glossary(cols, false, || crate::run_native::glossary_quantities(&ctx.root, &o.book.media_id))?;
    let mut sampler = crate::ram::Sampler::new(cols, 1, crate::ram::SamplePer::Field, q);
    sampler.field(o.s.rel(), &o.s.m, true);
    let mut lines = sampler.out.lines();
    let (names, row) = (lines.next().unwrap_or_default(), lines.next().unwrap_or_default());
    let mut out = serde_json::Map::new();
    // The first two columns are run_frame and frame.
    for (n, cell) in names.split(',').zip(row.split(',')).skip(2) {
        out.insert(n.into(), json!(cell));
    }
    Ok(Value::Object(out))
}

/// Bytes in RAM or the scratchpad as hex.
fn bytes(m: &Machine, spec: &str) -> crate::Result<Value> {
    let (lo, hi) = crate::run::parse_range_at(spec, Some((&m.bus.ram, &m.bus.scratch)))
        .map_err(|e| Error::invalid(format!("bytes: {e}")))?;
    if hi - lo > 4096 {
        return Err(Error::invalid("bytes: at most 4096 at a time"));
    }
    let (mem, o) = match lo {
        0..0x20_0000 => (&m.bus.ram[..], lo as usize),
        0x1F80_0000..0x1F80_0400 => (&m.bus.scratch[..], (lo - 0x1F80_0000) as usize),
        _ => return Err(Error::invalid(format!("bytes: {spec:?} is not in RAM or the scratchpad"))),
    };
    let b = mem
        .get(o..o + (hi - lo) as usize)
        .ok_or_else(|| Error::invalid(format!("bytes: {spec:?} runs past the end")))?;
    Ok(json!({"address": hex(lo), "length": b.len(), "hex": b.iter().map(|x| format!("{x:02x}")).collect::<String>()}))
}

impl Op for DebugSetArgs {
    fn exec(self, ctx: &Ctx) -> Outcome {
        Outcome::result(set(ctx, self))
    }
}

/// A register by name: pc, hi, lo, a register's name or rN.
fn register(m: &mut Machine, spec: &str) -> crate::Result<()> {
    let (name, value) = spec.split_once('=').ok_or_else(|| Error::invalid(format!("register {spec:?}: NAME=VALUE")))?;
    let value = value.trim();
    let v = match value.strip_prefix("0x") {
        Some(h) => u32::from_str_radix(h, 16).ok(),
        None => value.parse::<i64>().ok().map(|n| n as u32),
    }
    .ok_or_else(|| Error::invalid(format!("register {spec:?}: VALUE is decimal or 0xHEX")))?;
    let name = name.trim().trim_start_matches('$').to_ascii_lowercase();
    match name.as_str() {
        "pc" => m.cpu.set_pc(v),
        "hi" => m.cpu.hi = v,
        "lo" => m.cpu.lo = v,
        n => {
            let i = (0..32u32)
                .find(|&i| psx_core::disasm::reg(i) == n || format!("r{i}") == n)
                .ok_or_else(|| Error::invalid(format!("register {name:?}: pc, hi, lo or a register's name")))?;
            if i == 0 {
                return Err(Error::invalid("register zero is always 0"));
            }
            m.cpu.regs[i as usize] = v;
        }
    }
    Ok(())
}

fn set(ctx: &Ctx, a: DebugSetArgs) -> crate::Result<Value> {
    let mut o = Open::load(&ctx.root, &a.which)?;
    if a.poke.is_empty() && a.register.is_empty() && a.pad.is_none() {
        return Err(Error::invalid("debug_set: give poke, register or pad"));
    }
    let mut written = Vec::new();
    for p in &a.poke {
        if p.contains('@') {
            return Err(Error::invalid(format!("poke {p:?}: debug_set writes here; leave out @FRAME")));
        }
        let k = crate::ram::parse_poke(p).map_err(Error::invalid)?;
        let m = &mut o.s.m;
        k.apply(&mut m.bus.ram, &mut m.bus.scratch);
        // Code the poke changed runs as changed.
        m.bus.ram_written_range(0, psx_core::bus::RAM_SIZE);
        written.push(k.report());
    }
    for r in &a.register {
        register(&mut o.s.m, r)?;
    }
    if let Some(buttons) = &a.pad {
        if let Some(r) = &o.book.recording {
            return Err(Error::invalid(format!("pad: {r}'s ticks set the pad in this session")));
        }
        let sticks = match a.sticks.as_deref() {
            None => None,
            Some(&[a, b, c, d]) => Some([a, b, c, d]),
            Some(_) => return Err(Error::invalid("sticks: four values, left x/y and right x/y")),
        };
        let buttons = buttons
            .iter()
            .filter(|b| !b.is_empty())
            .map(|b| serde_json::from_value::<crate::replay::Button>(json!(b)))
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| Error::invalid(format!("pad: {e}")))?;
        let e = crate::replay::Event { frame: 0, buttons, sticks, until: None };
        let i = crate::replay::resolve(None, Some(&crate::replay::Events(vec![e])), 1).map_err(Error::invalid)?;
        if crate::replay::uses_sticks(&i) && o.s.m.bus.sio0.pad.kind != psx_core::pad::PadKind::DualShock {
            return Err(Error::invalid("sticks need a dualshock pad"));
        }
        if let Some(i) = i.first() {
            o.s.pad(i);
        }
    }
    let call = json!({"set": serde_json::to_value(&a).unwrap_or_default()});
    let detail = if written.is_empty() { Value::Null } else { json!({"poked": written}) };
    let v = o.stop("set", call, detail)?;
    Ok(o.result(v))
}

impl Op for DebugBackArgs {
    fn exec(self, ctx: &Ctx) -> Outcome {
        Outcome::result(back(ctx, self))
    }
}

fn back(ctx: &Ctx, a: DebugBackArgs) -> crate::Result<Value> {
    let dir = dir_of(&ctx.root, &a.which)?;
    let mut book: Book = read_json(&dir.join("session.json"))?;
    let to = match a.to {
        Some(n) if n < book.stops.len() => n,
        Some(n) => {
            return Err(Error::invalid(format!("to: the session has stops 0 to {}, not {n}", book.stops.len() - 1)));
        }
        None => book.stops[book.current]["from"]
            .as_u64()
            .ok_or_else(|| Error::invalid("this is the session's start: there is no stop before it"))?
            as usize,
    };
    book.current = to;
    write_json(&dir.join("session.json"), &book)?;
    let id = dir.file_name().map(|n| n.to_string_lossy().into_owned());
    Ok(json!({"id": id, "at": book.stops[to]}))
}

impl Op for DebugSaveArgs {
    fn exec(self, ctx: &Ctx) -> Outcome {
        Outcome::result(save(ctx, self))
    }
}

fn save(ctx: &Ctx, a: DebugSaveArgs) -> crate::Result<Value> {
    let dir = dir_of(&ctx.root, &a.which)?;
    let book: Book = read_json(&dir.join("session.json"))?;
    let n = book.current;
    let out = a.out.unwrap_or_else(|| dir.join(format!("state-{n}.bin")));
    let from = stop_file(&dir, n, "bin");
    let bytes = std::fs::read(&from).map_err(|e| Error::io(&from, e))?;
    std::fs::write(&out, &bytes).map_err(|e| Error::io(&out, e))?;
    Ok(json!({"state": out, "sha256": psx_media::disc::sha256_bytes(&bytes), "at": book.stops[n],
        "note": "run --from-state continues from it, with the adjustments run chooses; the session's own are not in the state"}))
}

impl Op for DebugEndArgs {
    fn exec(self, ctx: &Ctx) -> Outcome {
        Outcome::result(end(ctx, self))
    }
}

fn end(ctx: &Ctx, a: DebugEndArgs) -> crate::Result<Value> {
    let dir = dir_of(&ctx.root, &a.which)?;
    std::fs::remove_dir_all(&dir).map_err(|e| Error::io(&dir, e))?;
    Ok(json!({"ended": dir.file_name().map(|n| n.to_string_lossy().into_owned())}))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// main calls f, which calls the leaf g; the stop is in g, past the ra
    /// main's caller left.
    #[test]
    fn the_stack_unwinds_through_prologues_to_a_leaf() {
        let mut mem = std::collections::HashMap::new();
        let mut code = |at: u32, words: &[u32]| {
            for (i, w) in words.iter().enumerate() {
                mem.insert(at + 4 * i as u32, *w);
            }
        };
        let jal = |to: u32| 0x0C00_0000 | (to >> 2 & 0x03FF_FFFF);
        // 0x80010000 main: addiu sp,-24; sw ra,20(sp); nop; jal f; nop; ...
        code(0x8001_0000, &[0x27BD_FFE8, 0xAFBF_0014, 0, jal(0x8001_0100), 0, 0x03E0_0008, 0]);
        // 0x80010100 f: addiu sp,-32; sw ra,28(sp); jal g; nop; ...
        code(0x8001_0100, &[0x27BD_FFE0, 0xAFBF_001C, jal(0x8001_0200), 0, 0x03E0_0008, 0]);
        // 0x80010200 g: a leaf without a frame.
        code(0x8001_0200, &[0, 0, 0x03E0_0008, 0]);
        // Stack: main's frame at 0x801FFFE8 (ra 0xBFC00100, a BIOS return
        // not after a call), f's at 0x801FFFC8.
        mem.insert(0x801F_FFE8 + 20, 0xBFC0_0100);
        mem.insert(0x801F_FFC8 + 28, 0x8001_0014);
        let (frames, ended) = unwind(|a| Some(*mem.get(&a).unwrap_or(&0)), 0x8001_0204, 0x801F_FFC8, 0x8001_0110);
        assert_eq!(
            frames,
            [
                Frame { pc: 0x8001_0204, sp: 0x801F_FFC8, size: None, returns_to: Some(0x8001_0110) },
                Frame { pc: 0x8001_0108, sp: 0x801F_FFC8, size: Some(32), returns_to: Some(0x8001_0014) },
                Frame { pc: 0x8001_000C, sp: 0x801F_FFE8, size: Some(24), returns_to: Some(0xBFC0_0100) },
            ]
        );
        assert_eq!(ended, "0xBFC00100 is not after a call");
    }

    /// Stops at the tick, at a PC and at a write, goes back a stop and on
    /// again, from a recording in a port's workspace:
    /// PSX_TEST_WORKSPACE=DIR PSX_TEST_RECORDING=SCENARIO/NAME cargo test -p psx --release debugger_ -- --ignored
    #[test]
    #[ignore]
    fn debugger_stops_and_goes_back_to_the_same_registers() {
        let (Some(root), Ok(recording)) = (std::env::var_os("PSX_TEST_WORKSPACE"), std::env::var("PSX_TEST_RECORDING"))
        else {
            return;
        };
        let ctx = Ctx { root: PathBuf::from(root), cancel: None };
        fn call<A: Op>(ctx: &Ctx, args: Value) -> Value {
            let v = serde_json::from_value::<A>(args).unwrap().exec(ctx).value;
            assert!(v.get("error").is_none(), "{v}");
            v
        }
        let v = call::<DebugStartArgs>(&ctx, json!({"recording": recording}));
        let id = v["id"].clone();
        let go = |args: Value| {
            let mut args = args;
            args["id"] = id.clone();
            call::<DebugGoArgs>(&ctx, args)
        };
        let registers = || call::<DebugShowArgs>(&ctx, json!({"id": id}))["registers"].clone();
        let t = go(json!({"tick": true}));
        assert_eq!((t["why"].clone(), t["ticks"].clone()), (json!("tick"), json!(1)), "{t}");
        // Where the tick ended, a tick later.
        let p = go(json!({"pc": t["pc"]}));
        assert_eq!((p["why"].clone(), p["pc"].clone()), (json!("pc"), t["pc"].clone()), "{p}");
        assert_eq!(registers()["pc"], t["pc"]);
        let w = go(json!({"write": "80000000-80200000"}));
        assert_eq!(w["why"], "write", "{w}");
        let first = registers();
        let b = call::<DebugBackArgs>(&ctx, json!({"id": id}));
        assert_eq!(b["at"]["stop"], p["stop"]);
        assert_eq!(registers()["pc"], p["pc"]);
        let again = go(json!({"write": "80000000-80200000"}));
        assert_eq!((again["detail"].clone(), again["from"].clone()), (w["detail"].clone(), p["stop"].clone()));
        assert_eq!(registers(), first);
        call::<DebugEndArgs>(&ctx, json!({"id": id}));
    }
}
