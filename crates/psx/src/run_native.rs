//! `psx run`: a bounded run on psx-core, with observations taken from the emulated hardware itself.

use std::path::Path;
use std::time::Instant;

use psx_core::Machine;
use psx_core::disasm::reg;
use psx_core::state::{MediaId, StateError, disc_fingerprint};
use psx_core::trace::{Namer, TraceConfig, Tracer};
use serde_json::{Value, json};

use crate::Error;
use crate::commands::addr_value;
use crate::run::{RunArgs, write_png};
use psx_media::memmap::{hex, region};

pub(crate) mod gpu_capture;
pub(crate) mod observe;
use gpu_capture::{CapturedFrame, GpuCapture};

use crate::session::{Out, Session};

fn sha256(bytes: &[u8]) -> String {
    psx_media::disc::sha256_bytes(bytes)
}

/// The glossary's binding, for its quantities, its probes and its tick:
/// built, loaded and checked against the run's media.
pub(crate) fn glossary_quantities(root: &Path, media: &Option<MediaId>) -> crate::Result<crate::ram::Quantities> {
    let (types, binding) = crate::scenario::glossary_binding(root)?;
    let image = &binding.described().source.image_sha256;
    if media.as_ref().map(|m| &m.sha256) != Some(image) {
        return Err(Error::new(
            "identity_mismatch",
            format!(
                "the glossary's binding is for image {image}, the run's media is {}: glossary quantities and ticks are read from the game the binding is for",
                media.as_ref().map_or("none", |m| m.sha256.as_str())
            ),
        ));
    }
    Ok(crate::ram::Quantities::new(binding, types))
}

/// A run's adjustments (not a recording's), the tick latch needs, and why
/// any of the defaults did not apply.
struct Chosen {
    set: crate::adjust::Adjustments,
    tick: Option<crate::binding::Tick>,
    unavailable: serde_json::Map<String, Value>,
}

/// The adjustments `o` asks for: by default latch (where the glossary's
/// binding gives the tick for this medium), steady, and fix (where fixes/
/// holds C, or the kept build `fix_build`); `stock` or none named for the
/// stock game; otherwise those named, each refused where it cannot apply.
/// `glossary` keeps the binding loaded for latch, for the run's quantities.
fn adjustments(
    root: &Path,
    o: &crate::run::ObserveArgs,
    media: &Option<MediaId>,
    glossary: &mut Option<crate::Result<crate::ram::Quantities>>,
) -> crate::Result<Chosen> {
    let mut c = Chosen { set: crate::adjust::Adjustments::stock(), tick: None, unavailable: serde_json::Map::new() };
    let named: Option<Vec<&str>> = o.adjust.as_ref().map(|v| v.iter().map(String::as_str).collect());
    if let Some(n) = &named {
        if n.contains(&"stock") && n.len() > 1 {
            return Err(Error::invalid("adjust: stock is the game without adjustments: name it alone"));
        }
        if let Some(x) = n.iter().find(|x| !["latch", "steady", "fix", "stock"].contains(x)) {
            return Err(Error::invalid(match *x {
                "off" => "adjust off: off turns off what a scenario's [off] names; run from a recording of it (recording SCENARIO/NAME)".to_string(),
                x => format!("adjust {x:?}: run knows latch, steady and fix (by default all three where they apply), or stock"),
            }));
        }
    }
    let wants = |x: &str| named.as_ref().is_none_or(|n| n.contains(&x));
    if o.fix_build.is_some() && !wants("fix") {
        return Err(Error::invalid("fix_build is a kept build of fixes/ to run: it goes with fix"));
    }
    c.set.steady = wants("steady");
    if wants("latch") {
        let q = glossary_quantities(root, media);
        match &q {
            Ok(q) => {
                c.set.latch = true;
                c.tick = Some(q.binding.described().tick.clone());
            }
            Err(e) if named.is_none() => {
                c.unavailable.insert("latch".into(), json!(format!("no tick for latch: {}", e.message)));
            }
            Err(e) => {
                return Err(Error::invalid(format!(
                    "adjust latch: latch needs the game's tick from the glossary's binding: {}",
                    e.message
                ))
                .with("problems", e.to_value()["problems"].clone()));
            }
        }
        *glossary = Some(q);
    }
    if wants("fix") {
        c.set.fix = match &o.fix_build {
            Some(p) => {
                let text = std::fs::read_to_string(p).map_err(|e| Error::io(p, e))?;
                let detail: Value = serde_json::from_str(&text)
                    .map_err(|e| Error::malformed(format!("{}: a fix build (fix.json): {e}", p.display())))?;
                let mut d = serde_json::Map::new();
                d.insert("adjust".into(), json!(["fix"]));
                d.insert("fix".into(), detail);
                let a = crate::adjust::Adjustments::from_details(&d)
                    .map_err(|e| Error::malformed(format!("{}: {e}", p.display())))?;
                a.fix
            }
            None => crate::decomp::fixes(root)?,
        };
        if c.set.fix.is_none() {
            if named.is_some() {
                return Err(Error::invalid(format!(
                    "adjust fix: {}/ holds no C: fix runs the agent's replacements of the functions that delay input",
                    crate::decomp::FIXES
                )));
            }
            c.unavailable.insert("fix".into(), json!(format!("{}/ holds no C", crate::decomp::FIXES)));
        }
    }
    Ok(c)
}

/// A run's start, which the debugger shares: the machine booted, restored or
/// at a recording's start (after its lead-in), the decomp installed and the
/// adjustments started, in a session whose driver the caller sets.
pub(crate) struct Begun {
    pub s: Session<'static>,
    pub bios_sha: String,
    pub media_id: Option<MediaId>,
    pub decomp: Option<crate::decomp::Replacement>,
    /// The recording run from: its name and pad by tick.
    pub recording: Option<(String, Vec<crate::replay::Input>)>,
    /// A run's own adjustments (not a recording's).
    chosen: Option<Chosen>,
    /// The adjustments' names.
    pub adjusted: Option<Vec<&'static str>>,
    glossary: Option<crate::Result<crate::ram::Quantities>>,
    pub started: Instant,
    pub deadline: std::time::Duration,
}

impl Begun {
    /// Why any of the port's default adjustments did not apply.
    pub fn unavailable(&self) -> Option<&serde_json::Map<String, Value>> {
        self.chosen.as_ref().map(|c| &c.unavailable).filter(|u| !u.is_empty())
    }

    /// The fix build run, for a chain step to keep.
    pub fn fix_detail(&self) -> Option<Value> {
        self.chosen.as_ref().and_then(|c| c.set.fix_detail())
    }
}

pub(crate) fn begin(root: &Path, a: &mut RunArgs, events: &[crate::replay::Input]) -> crate::Result<Begun> {
    let bios = match std::fs::read(&a.bios) {
        Ok(b) => b,
        Err(e) => return Err(Error::io(&a.bios, e)),
    };
    let bios_sha = sha256(&bios);
    // Open the medium first: a restored state needs it to check identity.
    let Medium { exe, disc, id: media_id } = open_media(a.media.as_deref())?;
    let recording = a.recording.take();
    let m = match recording.as_ref() {
        Some(_) => None,
        None => Some(boot(a.from_state.as_deref(), &a.cards, bios, exe.as_deref(), disc, &media_id)?),
    };
    let (m, recording) = match recording {
        Some(r) => {
            let crate::run::FromRecording { m, name, adjust, pads, tick, frames } = r;
            (m, Some((name, adjust, pads, tick, frames)))
        }
        None => (m.unwrap(), None),
    };
    // A recording brings its own; otherwise the port's (latch, steady, fix) where each applies.
    let mut glossary = None;
    let chosen = match recording {
        Some(_) => None,
        None => Some(adjustments(root, &a.observe, &media_id, &mut glossary)?),
    };
    // Replay events: pad state from each frame (frames counted from the run start).
    if crate::replay::uses_sticks(events) && m.bus.sio0.pad.kind != psx_core::pad::PadKind::DualShock {
        return Err(Error::new(
            "invalid_arguments",
            "input sticks require a dualshock pad (configure it at boot; checkpoint pads are restored)",
        ));
    }
    let decomp = a.observe.decomp.as_ref().map(|w| crate::decomp::Replacement::new(root, w)).transpose()?;
    let started = Instant::now();
    let deadline = std::time::Duration::from_secs(a.timeout_s);
    // Frames count from power-on, including any BIOS boot spent before an EXE
    // was side-loaded; after --from-state they count from the restored state,
    // and from a recording from the field after its first tick.
    let start_frame = if a.from_state.is_some() { m.bus.frames } else { 0 };
    let mut s = Session::new(m, start_frame, Box::new(crate::session::NoInput));
    if let Some(d) = &decomp {
        d.install(&mut s.m);
    }
    let mut adjusted = None;
    if let Some((name, adjust, _, tick, frames)) = &recording {
        // As scenario record starts: the pad released, the adjustments on, the lead-in.
        crate::replay::apply(&mut s.m, &crate::replay::Input { frame: 0, buttons: 0, sticks: [128; 4] });
        s.adjust = Some(adjust.start(&mut s.m));
        s.tick = Some(tick.clone());
        let limit = s.m.bus.frames + *frames as u64;
        let led = s.lead_in(limit, started, deadline);
        if led == crate::session::LeadIn::Timeout {
            return Err(Error::new("timeout", "the recording's lead-in ran out of time"));
        }
        if led == crate::session::LeadIn::NoTick {
            return Err(Error::invalid(format!(
                "{name}: no tick ended within its {frames} fields from where it starts: the glossary's tick is not the one it was recorded with"
            )));
        }
        adjusted = Some(adjust.names());
    } else if let Some(c) = &chosen {
        if !c.set.is_stock() {
            s.adjust = Some(c.set.start(&mut s.m).falling_back());
        }
        s.tick = c.tick.clone();
        adjusted = Some(c.set.names());
    }
    Ok(Begun {
        s,
        bios_sha,
        media_id,
        decomp,
        recording: recording.map(|(name, _, pads, _, _)| (name, pads)),
        chosen,
        adjusted,
        glossary,
        started,
        deadline,
    })
}

pub fn run(root: &Path, mut a: RunArgs, events: Vec<crate::replay::Input>) -> crate::Result<(bool, Value)> {
    let b = begin(root, &mut a, &events)?;
    let Begun { mut s, bios_sha, media_id, decomp, recording, chosen, adjusted, mut glossary, started, deadline } = b;
    s.driver = match &recording {
        Some((name, pads)) => Box::new(crate::session::Ticks { recording: name.clone(), pads: pads.clone() }),
        None => Box::new(observe::Events::new(events)),
    };
    let start_frame = s.start;
    let m = &mut s.m;
    attach_observers(m, &a)?;
    let obs = &a.observe;
    let cols = obs.samples.iter().map(|spec| spec.parse()).collect::<Result<Vec<_>, _>>().map_err(Error::invalid)?;
    let until = obs.until.iter().map(|s| crate::until::parse(s)).collect::<Result<Vec<_>, _>>();
    let until = until.map_err(Error::invalid)?;
    let census = obs.find_tick.then(|| observe::Census::new(m));
    let per = obs.sample_per.unwrap_or_default();
    if per == crate::ram::SamplePer::Tick && cols.is_empty() {
        return Err(Error::invalid("--sample-per tick: give the values to sample with --sample"));
    }
    let named = cols.iter().any(|c| matches!(c, crate::ram::Column::Quantity(_)));
    let quantities = if per == crate::ram::SamplePer::Tick || named {
        Some(match glossary.take() {
            Some(q) => q?,
            None => glossary_quantities(root, &media_id)?,
        })
    } else {
        None
    };
    let tick =
        quantities.as_ref().filter(|_| per == crate::ram::SamplePer::Tick).map(|q| q.binding.described().tick.clone());
    let sampler =
        (!cols.is_empty()).then(|| crate::ram::Sampler::new(cols, obs.sample_every.unwrap_or(1), per, quantities));
    let read_ranges = obs.watch_reads.iter().map(|s| crate::run::parse_range_spec(s)).collect::<Result<Vec<_>, _>>();
    let read_ranges = read_ranges.map_err(|e| Error::invalid(format!("--watch-reads: {e}")))?;
    if obs.snapshot_every == Some(0) || obs.sample_every == Some(0) {
        return Err(Error::invalid("--snapshot-every and --sample-every take N >= 1"));
    }
    let scopes = std::rc::Rc::new(crate::load_symbols(&a.symbols)?);
    let dir = a.out.clone().unwrap_or_else(|| {
        root.join(".psx/runs").join(format!(
            "{}-{}-native{}",
            crate::now_ms(),
            std::process::id(),
            crate::instance_suffix()
        ))
    });
    if let Err(e) = std::fs::create_dir_all(&dir) {
        return Err(Error::io(dir, e));
    }
    let audio = if a.audio { Some(observe::Audio::new(m, dir.join("audio.wav"))?) } else { None };
    let profile = a.profile.then(|| observe::Profile::new(m, scopes.clone()));
    let tracer = if a.trace.trace { Some(tracer(&a.trace, a.max_events, &scopes, m.bus.frames, &dir)?) } else { None };
    let trace = tracer.map(|t| observe::Trace::new(m, t, scopes.clone()));

    let Some(target) = start_frame.checked_add(a.frames as u64) else {
        return Err(Error::invalid("run frame range overflows u64"));
    };

    // At each boundary the observers look in this order, then the input applies.
    s.tick = s.tick.take().or(tick);
    if !until.is_empty() {
        s.observe(observe::Until { conds: until, met: None });
    }
    if let Some(o) = audio {
        s.observe(o);
    }
    if let Some(o) = census {
        s.observe(o);
    }
    if obs.pad_reads {
        s.observe(observe::PadFlow { flow: crate::pad_reads::PadReads::new(), scopes: scopes.clone() });
    }
    if let Some(o) = sampler {
        s.observe(observe::Samples(o));
    }
    if let Some(o) = profile {
        s.observe(o);
    }
    if let Some(o) = trace {
        s.observe(o);
    }
    if !read_ranges.is_empty() {
        s.observe(observe::Reads {
            ranges: read_ranges,
            max: a.max_events as usize,
            start: start_frame,
            scopes: scopes.clone(),
            reads: Vec::new(),
            hits: 0,
            readers: Default::default(),
        });
    }
    if let Some(every) = obs.snapshot_every {
        s.observe(observe::Screens { every, dir: dir.clone(), shots: Vec::new() });
    }
    s.observe(observe::Writes {
        enabled: !a.watches.is_empty(),
        changes_only: obs.watch_changes_only,
        scopes: scopes.clone(),
    });
    s.observe(observe::Logs { scopes });

    let termination = s.advance(target, started, deadline)?.termination();
    let complete = matches!(termination, "frames_reached" | "until_met" | "recording_ended");
    s.last_field(complete)?;
    let wall_ms = started.elapsed().as_millis();

    let mut files = serde_json::Map::new();
    let mut report = serde_json::Map::new();
    let mut out = Out { dir: &dir, files: &mut files, report: &mut report, start: start_frame, termination };
    let m = &s.m;
    if a.save_state {
        out.put("state.bin", &m.save_state(media_id.clone()));
    }
    let mut cards = Vec::new();
    for (slot, card) in m.bus.sio0.cards.iter().enumerate() {
        if let Some(c) = card {
            let name = format!("memcard{}.mcd", slot + 1);
            if c.writes > 0 {
                out.put(&name, &c.data);
            }
            cards.push(json!({"slot": slot + 1, "sectors_written": c.writes, "file": (c.writes > 0).then_some(name)}));
        }
    }
    out.put("ram.bin", &m.bus.ram);
    out.put("scratch.bin", &m.bus.scratch);
    out.put("vram.bin", &m.bus.gpu.vram.lock().to_bytes());
    let (w, h, rgba) = m.bus.gpu.display_rgba();
    let screen = dir.join("screen.png");
    if write_png(&screen, w, h, &rgba, png::ColorType::Rgba).is_ok() {
        out.files.insert("screen.png".into(), json!({"path": screen}));
    }
    let vram = dir.join("vram.png");
    if write_png(&vram, 1024, 512, &m.bus.gpu.vram.lock().rgba15(0, 0, 1024, 512), png::ColorType::Rgba).is_ok() {
        out.files.insert("vram.png".into(), json!({"path": vram}));
    }
    if let Some(f) = chosen.as_ref().and_then(|c| c.set.fix_detail()) {
        // The build it ran, for a chain step to keep (fix_build runs it again).
        out.put("fix.json", serde_json::to_string_pretty(&f).unwrap_or_default().as_bytes());
    }
    s.finish(&mut out)?;
    let m = &s.m;

    let registers = registers(m);
    let from_state_sha256 = a.from_state.as_deref().and_then(|p| std::fs::read(p).ok()).map(|b| sha256(&b));
    // How the machine was set up at power-on (a state carries its cards and pad).
    let cards_at_power_on = if a.from_state.is_some() { Value::Null } else { json!(a.cards) };
    for key in observe::SECTIONS {
        report.entry(key).or_insert(Value::Null);
    }
    let core = json!({
        "status": if complete { "complete" } else { "partial" },
        "run_dir": dir,
        "termination": termination,
        "warnings": crate::stall::warnings(m),
        "frames": {"requested": a.frames, "reached": m.bus.frames - start_frame, "unit": "vsync", "start": start_frame, "end": m.bus.frames,
            "numbering": "run frames (--frames, --replay, --trace-from-frame, samples run_frame) count from the run start; `frame` in writes.jsonl, trace.txt, samples.csv and the other logs is absolute (vblanks since power-on) = run_frame + start"},
        "cpu_cycles": m.bus.now,
        "instructions": s.steps,
        "registers": registers,
        "screen": {"width": w, "height": h, "bpp": if m.bus.gpu.display.rgb24() {24} else {16}},
        "video": {"region_at_capture":if m.bus.pal(){"PAL"}else{"NTSC"},"mode":m.bus.gpu.display.mode,
            "cycles_per_vblank_at_capture":m.bus.frame_cycles(),"interlaced":m.bus.gpu.display.interlaced_480(),
            "display_start":m.bus.gpu.display.start,"horizontal_range":m.bus.gpu.display.h_range,"vertical_range":m.bus.gpu.display.v_range,
            "note":"GPU mode at capture; raw raster dimensions may change during boot."},
        "memcards": cards,
        "pad": {"kind": format!("{:?}", m.bus.sio0.pad.kind).to_lowercase(), "analog": m.bus.sio0.pad.analog, "locked": m.bus.sio0.pad.locked,
            "buttons":crate::replay::button_names(m.bus.sio0.buttons.into()),
            "sticks":m.bus.sio0.pad.axes(),
            "motors": [m.bus.sio0.pad.motors.0, m.bus.sio0.pad.motors.1]},
        "bios_call_counts": m.bus.obs.bios_counts,
        "files": files,
        "identity": {
            "backend": "psx-core", "backend_version": env!("CARGO_PKG_VERSION"),
            "timing_model": "Mednafen's video, CPU, DMA, CD-ROM, GPU command, timer, controller port, SPU and MDEC timing",
            "bios": a.bios, "bios_sha256": bios_sha, "media": a.media, "media_identity": media_id,
            "from_state": a.from_state, "state_format": psx_core::state::FORMAT,
            "from_state_sha256": from_state_sha256, "cards": cards_at_power_on,
        },
        "wall_ms": wall_ms,
    });
    if let Value::Object(core) = core {
        report.extend(core);
    }
    if let Some(names) = adjusted {
        report.insert("adjust".into(), json!(names));
        report.insert("adjusted".into(), s.adjust.as_ref().and_then(|r| r.report(m)).unwrap_or(Value::Null));
        match &recording {
            Some(r) => {
                report.insert("recording".into(), json!(r.0));
            }
            None => {
                let c = chosen.as_ref().expect("a run's adjustments");
                if !c.unavailable.is_empty() {
                    report.insert("adjust_unavailable".into(), Value::Object(c.unavailable.clone()));
                }
            }
        }
    }
    if let Some(d) = &decomp {
        report.insert("decomp".into(), d.report(m));
    }
    Ok((complete, Value::Object(report)))
}

/// Each register as a hex word.
pub(crate) fn registers(m: &Machine) -> serde_json::Map<String, Value> {
    let mut registers = serde_json::Map::new();
    registers.insert("pc".into(), json!(hex(m.cpu.pc)));
    for i in 0..32 {
        registers.insert(reg(i as u32).into(), json!(hex(m.cpu.regs[i])));
    }
    registers.insert("hi".into(), json!(hex(m.cpu.hi)));
    registers.insert("lo".into(), json!(hex(m.cpu.lo)));
    registers.insert("cop0_sr".into(), json!(hex(m.cpu.cop0[12])));
    registers.insert("cop0_cause".into(), json!(hex(m.cpu.cop0[13])));
    registers.insert("cop0_epc".into(), json!(hex(m.cpu.cop0[14])));
    registers
}

/// A write hit as JSON with hex strings next to the numbers and signed views
/// of the stored value (by store width) for CPU writes.
/// The run's medium: EXE bytes or an opened disc, with the identity states record.
#[derive(Default)]
pub(crate) struct Medium {
    pub exe: Option<Vec<u8>>,
    pub disc: Option<psx_media::Disc>,
    pub id: Option<MediaId>,
}

pub(crate) fn open_media(media: Option<&Path>) -> crate::Result<Medium> {
    let Some(p) = media else { return Ok(Medium::default()) };
    if psx_media::exe::is_exe_path(p) {
        let bytes = std::fs::read(p).map_err(|e| Error::io(p, e))?;
        let id = MediaId { kind: "exe".into(), sha256: sha256(&bytes) };
        Ok(Medium { exe: Some(bytes), disc: None, id: Some(id) })
    } else {
        let d = psx_media::Disc::open(p).map_err(Error::from)?;
        let id = MediaId { kind: "disc".into(), sha256: disc_fingerprint(&d) };
        Ok(Medium { exe: None, disc: Some(d), id: Some(id) })
    }
}

/// A machine restored from `--from-state` (checked against the medium) or booted fresh.
pub(crate) fn boot(
    from_state: Option<&Path>,
    cards: &crate::run::CardArgs,
    bios: Vec<u8>,
    exe: Option<&[u8]>,
    disc: Option<psx_media::Disc>,
    media_id: &Option<MediaId>,
) -> crate::Result<Machine> {
    let invalid = |e: String| Error::invalid(e);
    if let Some(p) = from_state {
        let bytes = std::fs::read(p).map_err(|e| Error::io(p, e))?;
        let (header, mut m) = Machine::load_state(&bytes, bios).map_err(|e| match e {
            StateError::BiosMismatch { .. } => Error::new("identity_mismatch", e),
            e => Error::new("invalid_state", e),
        })?;
        if header.media != *media_id {
            let show = |m: &Option<MediaId>| serde_json::to_string(m).unwrap_or_default();
            return Err(Error::new(
                "identity_mismatch",
                format!("state was saved with media {}, but --media is {}", show(&header.media), show(media_id)),
            ));
        }
        // Reinsert without resetting the drive: its state came from the file.
        m.bus.cdrom.disc = disc;
        if cards.given() {
            return Err(invalid("memory cards and the pad are part of the save state; drop --memcard1/--memcard2/--pad with --from-state".into()));
        }
        return Ok(m);
    }
    let mut m = Machine::new(bios);
    m.bus.sio0.cards = cards.load().map_err(invalid)?;
    m.bus.sio0.pad.kind = cards.pad_kind().map_err(invalid)?;
    if let Some(bytes) = exe {
        m.sideload_exe(bytes, 500_000_000).map_err(|e| Error::new("backend_failed", e))?;
    } else if disc.is_some() {
        m.insert_disc(disc);
    }
    Ok(m)
}

/// Write watches and the CD/BIOS/SPU event logs the run asked for.
fn attach_observers(m: &mut Machine, a: &RunArgs) -> crate::Result<()> {
    for w in &a.watches {
        let (addr, width) = crate::commands::split_width(w);
        let width = width.unwrap_or("4");
        let addr = crate::commands::addr_arg(addr, "watch address")?;
        // Any byte length: the native watch covers [ADDR, ADDR+WIDTH).
        let width: u32 = match width.parse() {
            Ok(w) if w >= 1 => w,
            _ => return Err(Error::invalid(format!("bad watch width {width:?} (bytes, e.g. 4)"))),
        };
        m.bus.obs.watch(addr, width);
    }
    // Observer::watch has a diagnostic default; investigation runs honor the
    // caller's recording budget, including zero, just as read watches do.
    m.bus.obs.max_hits = a.max_events as usize;
    if a.trace_cd {
        m.bus.obs.cd = Some(Vec::new());
    }
    if a.trace_bios {
        m.bus.obs.bios = Some(Vec::new());
    }
    if a.trace_spu {
        m.bus.obs.spu = Some(psx_core::observe::SpuLog::new(a.max_events as usize));
    }
    Ok(())
}

fn annotate_hit(h: &psx_core::observe::WriteHit) -> Value {
    let mut v = serde_json::to_value(h).unwrap();
    v["region"] = json!(region(h.address));
    if let Some(g) = crate::names_bind::global_at(&crate::names_bind::project_names(), h.address) {
        v["global"] = json!(g);
    }
    let w = h.width as usize;
    if w <= 4 {
        for (key, val) in [("value", h.value), ("old", h.old)] {
            if let Some(x) = val {
                v[format!("{key}_hex")] = json!(format!("0x{x:0n$X}", n = w * 2));
                v[format!("{key}_signed")] = json!(crate::ram::signed(x, w));
            }
        }
    }
    v
}

/// Per-boundary outputs: screens every N run frames and states at chosen run frames.
/// Hex address (optionally 0x-prefixed), or a function's name from the
/// project's names or --symbols, or any name `address` takes:
/// `(start, end of the function if known)`.
pub(crate) fn resolve(spec: &str, scopes: &[crate::ScopedSymbols]) -> crate::Result<(u32, Option<u32>)> {
    let bare = spec.trim_end_matches('?');
    if !spec.starts_with("0x") {
        if let Some(f) = scopes.iter().find_map(|s| s.table.find(bare).or_else(|| s.table.find(&format!("{bare}?")))) {
            return Ok(f);
        }
    }
    crate::names_bind::address(spec)
        .map(|v| (v, None))
        .map_err(|e| Error::invalid(format!("{e} (nor a function in --symbols)")))
}

/// A tracer for `t`'s window, writing trace.txt into `dir`.
pub(crate) fn tracer(
    t: &crate::run::TraceArgs,
    max_events: u32,
    scopes: &std::rc::Rc<Vec<crate::ScopedSymbols>>,
    start_frame: u64,
    dir: &Path,
) -> crate::Result<Tracer> {
    let mut ranges = Vec::new();
    for r in &t.trace_ranges {
        let (lo, hi) = match r.split_once('-') {
            Some((lo, hi)) => (resolve(lo, scopes)?.0, resolve(hi, scopes)?.0),
            None => {
                let (lo, end) = resolve(r, scopes)?;
                (lo, end.ok_or_else(|| Error::invalid(format!("{r:?}: no following symbol to bound it; use LO-HI")))?)
            }
        };
        if hi <= lo {
            return Err(Error::invalid(format!("empty trace range {r:?}")));
        }
        ranges.push((lo & 0x1FFF_FFFF, (lo & 0x1FFF_FFFF) + (hi - lo)));
    }
    let cfg = TraceConfig {
        from_frame: start_frame + t.trace_from_frame,
        start_pc: None,
        frames: t.trace_frames,
        ranges,
        max_lines: max_events as u64,
    };
    let path = dir.join("trace.txt");
    let mut file = std::fs::File::create(&path).map_err(|e| Error::io(&path, e))?;
    {
        use std::io::Write;
        writeln!(
            file,
            "# frame cycle pc word disassembly ; effects. `frame` counts vblanks from power-on; run frame = frame - {start_frame}"
        )
        .map_err(|e| Error::io(&path, e))?;
    }
    let namer: Option<Namer> = if scopes.is_empty() {
        None
    } else {
        let scopes = scopes.clone();
        Some(Box::new(move |pc, ram: &[u8]| crate::name_pc(&scopes, pc, Some(ram)).map(|(n, _, _)| n)))
    };
    Ok(Tracer::new(cfg, Box::new(std::io::BufWriter::with_capacity(1 << 20, file)), namer))
}

fn trace_outputs(
    t: &mut Tracer,
    m: &Machine,
    scopes: &[crate::ScopedSymbols],
    dir: &Path,
    start_frame: u64,
    files: &mut serde_json::Map<String, Value>,
) -> crate::Result<Value> {
    t.finish(&m.bus)?;
    let name = |pc: u32| {
        let v = crate::final_name(scopes, pc, Some(&m.bus.ram));
        (!v.is_null()).then_some(v)
    };
    let coverage = t.coverage();
    let mut text = String::from("# start end instructions executions [function at start]\n");
    for &(s, e, n, total) in &coverage {
        let f = name(s).and_then(|v| v["name"].as_str().map(|n| format!(" {n}"))).unwrap_or_default();
        text += &format!("{s:08x} {e:08x} {n} {total}{f}\n");
    }
    let mut calls: Vec<((u32, u32), &psx_core::trace::CallStat)> = t.calls.iter().map(|(k, v)| (*k, v)).collect();
    calls.sort_by_key(|((copy, addr), c)| (std::cmp::Reverse(c.count), *addr, *copy));
    // A copy is an index into code.bin; null where none holds the code.
    let copy = |c: u32| (c != psx_core::trace::NO_COPY).then_some(c);
    let calls_json: Vec<Value> = calls
        .iter()
        .map(|((target_copy, addr), c)| {
            let mut callers: Vec<(&(u32, u32), &u64)> = c.callers.iter().collect();
            callers.sort_by_key(|((sc, site), n)| (std::cmp::Reverse(**n), *site, *sc));
            json!({
                "address": hex(*addr), "copy": copy(*target_copy), "first_moment": c.first_moment, "moments": c.moments,
                "function": name(*addr), "count": c.count,
                "first_frame": c.first_frame, "last_frame": c.last_frame,
                "first_run_frame": c.first_frame as i64 - start_frame as i64, "last_run_frame": c.last_frame as i64 - start_frame as i64,
                "callers": callers.iter().take(16).map(|((sc, site), n)| json!({"pc": hex(*site), "copy": copy(*sc), "count": n})).collect::<Vec<_>>(),
            })
        })
        .collect();
    // What each word ran inside: a call (by its target, as in calls.json),
    // an exception (by its handler's first word), no call yet ("top"), or a
    // landing: a `jr ra` to an address the stack did not hold, by where it
    // landed. Its body is `[copy, first, end]` ranges of words run inside
    // it; its jumps the words a jump entered, `[copy, address, how]` (1:
    // `sp` back to its value at the owner's first word, 2: not, 3: both
    // seen). A landing adds what the run says of where it landed: the
    // owners (indices in this list) it ran inside after a call two words
    // before returned there through the stack, in returned_copy (the
    // landing's copy or the nearest before it with such returns), `sp`
    // then, and `sp` at each landing (each up to 16 values; seen_more when
    // there were more).
    use psx_core::trace::Owner;
    let bodies = t.bodies();
    let mut owners = String::from("[\n");
    let (mut top_words, mut landings) = (0u64, 0u64);
    let (mut landed_words, mut landed_words_one_owner, mut landed_words_same_sp) = (0u64, 0u64, 0u64);
    for (k, (o, body, jumps)) in bodies.iter().enumerate() {
        let words: u64 = body.iter().map(|r| (r.2 - r.1) as u64 / 4).sum();
        let (kind, at) = match *o {
            Owner::Top => {
                top_words = words;
                ("top", None)
            }
            Owner::Landed(c, a) => ("landed", Some((c, a))),
            Owner::Call(c, a) => ("call", Some((c, a))),
            Owner::Exception(c, a) => ("exception", Some((c, a))),
        };
        let mut v = json!({
            "owner": kind,
            "address": at.map(|a| hex(a.1)), "copy": at.and_then(|a| copy(a.0)),
            "body": body.iter().map(|r| json!([copy(r.0), hex(r.1), hex(r.2)])).collect::<Vec<_>>(),
            "jumps": jumps.iter().map(|j| json!([copy(j.0), hex(j.1), j.2])).collect::<Vec<_>>(),
        });
        if let Some(l) = t.landing(k as u32) {
            landings += 1;
            landed_words += words;
            // R ran inside one owner after returns through the stack; and
            // every landing's `sp` was one seen then (the same frame).
            let all = !(l.returned_inside.more || l.returned_sp.more || l.landed_sp.more);
            if all && l.returned_inside.values.len() == 1 {
                landed_words_one_owner += words;
                if l.landed_sp.values.iter().all(|sp| l.returned_sp.values.contains(sp)) {
                    landed_words_same_sp += words;
                }
            }
            v["returned_copy"] = json!(l.returned_copy.and_then(copy));
            v["returned_inside"] = json!(l.returned_inside.values);
            v["returned_sp"] = json!(l.returned_sp.values.iter().map(|&sp| hex(sp)).collect::<Vec<_>>());
            v["landed_sp"] = json!(l.landed_sp.values.iter().map(|&sp| hex(sp)).collect::<Vec<_>>());
            v["seen_more"] = json!(!all);
        }
        owners += &v.to_string();
        owners += if k + 1 < bodies.len() { ",\n" } else { "\n" };
    }
    owners += "]\n";
    // Each executed RAM word with the code page copy it ran from (code.bin's
    // index, all ones for none), the instruction it executed as, the moment
    // it first ran (the latest copy then: RAM's code was each page's latest
    // copy), flags (1: it continues the code before it, run straight on
    // from the word before, no delay slot, or returned to by a call two
    // words before) and its executions.
    let mut executed = b"PSXEXEC4".to_vec();
    for (copy, addr, word, r) in t.executed() {
        executed.extend(copy.to_le_bytes());
        executed.extend(addr.to_le_bytes());
        executed.extend(word.to_le_bytes());
        executed.extend(r.first.to_le_bytes());
        executed.extend((r.continues as u32).to_le_bytes());
        executed.extend(r.count.to_le_bytes());
    }
    let branches = crate::coverage::branches_bin(t);
    // Each code page's history: a copy whenever code ran in it changed,
    // numbered by its order here. A page's first copy is whole (the copy
    // before it all ones), a later one the words changed since the copy
    // before it, as (byte offset, word). After each, its flags: 1 when it
    // was taken because RAM no longer held the page's copy before as
    // another page was copied, not because code ran in it (RAM's code at
    // each moment is each page's latest copy).
    let mut code = b"PSXCODE3".to_vec();
    for c in &t.code_pages {
        code.extend(c.frame.to_le_bytes());
        code.extend(c.addr.to_le_bytes());
        match &c.data {
            psx_core::trace::CopyData::Whole(b) => {
                code.extend(psx_core::trace::NO_COPY.to_le_bytes());
                code.extend((b.len() as u32).to_le_bytes());
                code.extend(&b[..]);
            }
            psx_core::trace::CopyData::Changed(before, words) => {
                code.extend(before.to_le_bytes());
                code.extend((8 * words.len() as u32).to_le_bytes());
                for &(j, w) in words {
                    code.extend((4 * j as u32).to_le_bytes());
                    code.extend(w.to_le_bytes());
                }
            }
        }
        code.extend((c.refresh as u32).to_le_bytes());
    }
    for (file, bytes) in [
        ("coverage.txt", text.into_bytes()),
        ("calls.json", serde_json::to_vec_pretty(&calls_json).unwrap()),
        ("owners.json", owners.into_bytes()),
        ("executed.bin", executed),
        ("branches.bin", branches),
        ("code.bin", code),
    ] {
        let p = dir.join(file);
        std::fs::write(&p, &bytes).map_err(|e| Error::io(&p, e))?;
        files.insert(file.into(), json!({"path": p, "sha256": sha256(&bytes)}));
    }
    files.insert("trace.txt".into(), json!({"path": dir.join("trace.txt")}));
    let exceptions: serde_json::Map<String, Value> =
        t.exceptions.iter().map(|(code, n)| (psx_core::trace::exception_name(*code).to_string(), json!(n))).collect();
    let window = |w: Option<(u64, u64)>| {
        w.map(|(f, c)| json!({"frame": f, "run_frame": f as i64 - start_frame as i64, "cycle": c}))
    };
    // The window as run's arguments give it, for a recipe to run again.
    let options = json!({
        "trace_from_frame": t.cfg.from_frame.saturating_sub(start_frame),
        "trace_frames": t.cfg.frames,
        "trace_range": t.cfg.ranges.iter().map(|(lo, hi)| format!("{}-{}", hex(*lo), hex(*hi))).collect::<Vec<_>>(),
        "max_events": t.cfg.max_lines,
    });
    Ok(json!({
        "options": options,
        "started": window(t.started), "stopped": window(t.stopped),
        "instructions": t.instructions,
        "lines": t.lines, "lines_dropped": t.lines_dropped, "max_lines": t.cfg.max_lines,
        "interrupts": t.interrupts, "exceptions": exceptions,
        "coverage": {"runs": coverage.len(), "unique_instructions": coverage.iter().map(|c| c.2 as u64).sum::<u64>()},
        "top_calls": calls_json.iter().take(20).cloned().collect::<Vec<_>>(),
        "distinct_call_targets": calls_json.len(),
        "code_pages": {"kept": t.code_pages.len(), "refreshed": t.code_pages.iter().filter(|c| c.refresh).count(), "dropped": t.code_pages_dropped, "ran_from_cache": t.ran_from_cache},
        "owners": {
            "top_level_words": top_words, "landings": landings, "landed_words": landed_words,
            "landed_words_one_owner": landed_words_one_owner, "landed_words_same_sp": landed_words_same_sp,
            "returns_not_held": t.stack.returns_not_held, "frames_dropped": t.stack.frames_dropped,
            "exceptions_left_elsewhere": t.stack.exceptions_left_elsewhere,
            "rfe_without_exception": t.stack.rfe_without_exception,
        },
    }))
}

/// Write the decoded GPU commands of one frame, with VRAM before/after (the
/// after image and gpu_frame_vram.bin are the VRAM at the end of that frame,
/// not of the run), the frame's drawing area and a wireframe of every
/// primitive drawn over the after image.
fn gpu_frame_outputs(
    capture: &CapturedFrame,
    dir: &Path,
    files: &mut serde_json::Map<String, Value>,
) -> crate::Result<Value> {
    let start = &capture.state;
    let before = &capture.before;
    use psx_gpu::capture::decode;
    use std::collections::{BTreeMap, HashMap};
    let mut decoded = decode(start, &capture.commands);
    // Lift polygon vertices back to 3D: each vertex's screen position (minus the
    // drawing offset) is matched to GTE SXY results. A polygon's vertices are
    // projected together (typically one RTPT), so among all candidates the set
    // closest together in the projection log is taken, and polygons whose
    // vertices lie far apart in the log (coincidental matches) are not lifted.
    const MAX_SPREAD: usize = 64;
    let projections = &capture.projections;
    let mut by_xy: HashMap<(i32, i32), Vec<usize>> = HashMap::new();
    for (k, p) in projections.iter().enumerate() {
        by_xy.entry((p.sx as i32, p.sy as i32)).or_default().push(k);
    }
    let (mut polys, mut lifted, mut rejected) = (0u64, 0u64, 0u64);
    for c in decoded.iter_mut().filter(|c| c.kind == "poly3" || c.kind == "poly4") {
        polys += 1;
        let [ox, oy] = c.offset;
        let cands: Vec<&Vec<usize>> =
            match c.vertices.iter().map(|v| by_xy.get(&(v.x - ox, v.y - oy))).collect::<Option<Vec<_>>>() {
                Some(c) => c,
                None => continue,
            };
        // Anchor on each candidate of vertex 0; take the nearest candidate of each other vertex.
        let mut best: Option<(usize, Vec<usize>)> = None;
        for &a in cands[0] {
            let mut pick = vec![a];
            for list in &cands[1..] {
                let near = *list.iter().min_by_key(|&&k| k.abs_diff(a)).unwrap();
                pick.push(near);
            }
            let spread = pick.iter().max().unwrap() - pick.iter().min().unwrap();
            if best.as_ref().is_none_or(|(s, _)| spread < *s) {
                best = Some((spread, pick));
            }
        }
        match best {
            Some((spread, pick)) if spread <= MAX_SPREAD => {
                lifted += 1;
                for (v, k) in c.vertices.iter_mut().zip(pick) {
                    v.view = Some(projections[k].view);
                }
            }
            _ => rejected += 1,
        }
    }
    let [ofx, ofy, h] = capture.ctrl;
    let gte = json!({
        "projections_logged": projections.len(),
        "polygons": polys,
        "polygons_lifted_to_3d": lifted,
        "polygons_rejected_spread": rejected,
        // Projection at the end of the frame: sx = ofx + H·x/z, sy = ofy + H·y/z.
        "h": h as u16,
        "ofx": ofx as i32 as f64 / 65536.0,
        "ofy": ofy as i32 as f64 / 65536.0,
    });
    let mut kinds: BTreeMap<&str, u64> = BTreeMap::new();
    let mut pages: BTreeMap<(i32, i32, u8), u64> = BTreeMap::new();
    let mut cluts: BTreeMap<(i32, i32), u64> = BTreeMap::new();
    let (mut textured, mut semi) = (0u64, 0u64);
    for c in &decoded {
        *kinds.entry(c.kind).or_default() += 1;
        if c.textured {
            textured += 1;
            if let Some(t) = c.texpage {
                *pages.entry((t.x, t.y, t.depth)).or_default() += 1;
            }
            if let Some([x, y]) = c.clut {
                *cluts.entry((x, y)).or_default() += 1;
            }
        }
        semi += c.semi as u64;
    }
    let mut summary = json!({
        "commands": decoded.len(),
        "by_kind": kinds,
        "textured": textured,
        "semi_transparent": semi,
        "texture_pages": pages.iter().map(|((x, y, d), n)| json!({"x": x, "y": y, "depth": d, "uses": n})).collect::<Vec<_>>(),
        "cluts": cluts.iter().map(|((x, y), n)| json!({"x": x, "y": y, "uses": n})).collect::<Vec<_>>(),
        "draw_state_at_start": {"offset": start.offset, "area": start.area, "texpage": start.texpage},
        "display": {"width": capture.display.width(), "height": capture.display.height(), "start": capture.display.start},
        "gte": gte,
    });
    summary.as_object_mut().unwrap().extend(capture.metadata.as_object().unwrap().clone());
    let doc = json!({"summary": summary, "commands": decoded});
    let p = dir.join("gpu_frame.json");
    let bytes = serde_json::to_vec_pretty(&doc).unwrap();
    std::fs::write(&p, &bytes).map_err(|e| Error::io(&p, e))?;
    files.insert("gpu_frame.json".into(), json!({"path": p, "sha256": sha256(&bytes)}));

    let png = |name: &str, rgba: &[u8], files: &mut serde_json::Map<String, Value>| {
        let p = dir.join(name);
        if write_png(&p, 1024, 512, rgba, png::ColorType::Rgba).is_ok() {
            files.insert(name.into(), json!({"path": p}));
        }
    };
    png("gpu_frame_before.png", &before.rgba15(0, 0, 1024, 512), files);
    let (end_vram, area) = (&capture.after, capture.area);
    let vram_bytes = end_vram.to_bytes();
    let p = dir.join("gpu_frame_vram.bin");
    std::fs::write(&p, &vram_bytes).map_err(|e| Error::io(&p, e))?;
    files.insert("gpu_frame_vram.bin".into(), json!({"path": p, "sha256": sha256(&vram_bytes)}));
    let mut after = end_vram.rgba15(0, 0, 1024, 512);
    png("gpu_frame_after.png", &after, files);
    // The captured frame as drawn: its drawing area at the end of the frame.
    let (x1, y1, x2, y2) = area;
    let (aw, ah) = ((x2 - x1 + 1).clamp(0, 1024), (y2 - y1 + 1).clamp(0, 512));
    if aw > 0 && ah > 0 && x1 >= 0 && y1 >= 0 && x1 + aw <= 1024 && y1 + ah <= 512 {
        let p = dir.join("gpu_frame_screen.png");
        if write_png(
            &p,
            aw as u32,
            ah as u32,
            &end_vram.rgba15(x1 as _, y1 as _, aw as _, ah as _),
            png::ColorType::Rgba,
        )
        .is_ok()
        {
            files.insert("gpu_frame_screen.png".into(), json!({"path": p, "area": [x1, y1, aw, ah]}));
        }
    }
    // Wireframe: primitives green, rects yellow, lines cyan, fills red, uploads/copies magenta.
    let mut put = |x: i32, y: i32, c: [u8; 3]| {
        if (0..1024).contains(&x) && (0..512).contains(&y) {
            let i = ((y * 1024 + x) * 4) as usize;
            after[i..i + 4].copy_from_slice(&[c[0], c[1], c[2], 255]);
        }
    };
    let mut seg = |a: (i32, i32), b: (i32, i32), c: [u8; 3]| {
        let n = (b.0 - a.0).abs().max((b.1 - a.1).abs()).clamp(1, 2048);
        for k in 0..=n {
            put(a.0 + (b.0 - a.0) * k / n, a.1 + (b.1 - a.1) * k / n, c);
        }
    };
    for c in &decoded {
        let v: Vec<(i32, i32)> = c.vertices.iter().map(|v| (v.x, v.y)).collect();
        match c.kind {
            "poly3" | "poly4" => {
                let col = [0, 255, 0];
                let order: &[usize] = if v.len() == 4 { &[0, 1, 3, 2, 0] } else { &[0, 1, 2, 0] };
                for w in order.windows(2) {
                    seg(v[w[0]], v[w[1]], col);
                }
            }
            "line" | "polyline" => {
                for w in v.windows(2) {
                    seg(w[0], w[1], [0, 255, 255]);
                }
            }
            _ => {
                if let Some([x, y, w, h]) = c.area {
                    let col = match c.kind {
                        "rect" => [255, 255, 0],
                        "fill" => [255, 0, 0],
                        _ => [255, 0, 255],
                    };
                    let (x1, y1) = (x + w - 1, y + h - 1);
                    seg((x, y), (x1, y), col);
                    seg((x1, y), (x1, y1), col);
                    seg((x1, y1), (x, y1), col);
                    seg((x, y1), (x, y), col);
                }
            }
        }
    }
    png("gpu_frame_wire.png", &after, files);
    Ok(summary)
}

/// profile.json: every function by cycles, named from --symbols, and the
/// report's summary of the top ones.
fn profile_outputs(
    p: &psx_core::profile::Profiler,
    start_pc: u32,
    m: &Machine,
    scopes: &[crate::ScopedSymbols],
    dir: &Path,
    files: &mut serde_json::Map<String, Value>,
) -> crate::Result<Value> {
    let total = p.total_cycles.max(1) as f64;
    let functions: Vec<Value> = p
        .functions(start_pc)
        .iter()
        .map(|f| {
            // Code runs from KSEG0 (or the BIOS from KSEG1), as names are recorded.
            let at = |phys: u32| if phys >= 0x1FC0_0000 { phys | 0xA000_0000 } else { phys | 0x8000_0000 };
            let name = crate::name_pc(scopes, at(f.entry), Some(&m.bus.ram)).map(|(n, ..)| n);
            json!({
                "entry": hex(at(f.entry)), "name": name,
                "cycles": f.cycles, "share": (f.cycles as f64 / total * 10_000.0).round() / 100.0,
                "instructions": f.instructions, "calls": f.calls,
                "cycles_per_call": (f.calls > 0).then(|| f.cycles / f.calls),
                "hottest_pc": hex(at(f.hottest_pc)), "hottest_pc_cycles": f.hottest_cycles,
            })
        })
        .collect();
    let path = dir.join("profile.json");
    let body = json!({
        "cycles": p.total_cycles, "instructions": p.instructions, "outside_ram_and_bios_cycles": p.other_cycles(),
        "note": "cycles are Mednafen's CPU timing, including time DMA held the CPU; a function is a call target \
                 (or the exception vector), and code reached only by a plain jump counts towards the entry below it; \
                 share is percent of all cycles",
        "functions": functions,
    });
    let bytes = serde_json::to_vec_pretty(&body).unwrap_or_default();
    std::fs::write(&path, &bytes).map_err(|e| Error::io(path.clone(), e))?;
    files.insert("profile.json".into(), json!({"path": path, "sha256": sha256(&bytes)}));
    Ok(json!({"file": path, "cycles": p.total_cycles, "functions": body["functions"].as_array().map_or(0, |f| f.len()),
              "top": body["functions"].as_array().map(|f| f.iter().take(10).cloned().collect::<Vec<_>>())}))
}

/// The `--trace-spu` report: event counts, and recorded key ons grouped by the
/// sample they play, each linked to the upload that placed it in sound RAM.
fn spu_summary(log: &psx_core::observe::SpuLog, events: &[Value], dir: &Path) -> Value {
    let total: u64 = log.counts.values().sum();
    let mut uploads: Vec<&Value> = Vec::new();
    let mut samples: Vec<Value> = Vec::new();
    let mut voices = std::collections::BTreeSet::new();
    for e in events {
        match e["event"].as_str() {
            Some("upload") => uploads.push(e),
            Some("key_on") => {
                voices.insert(e["voice"].as_u64());
                let start = addr_value(&e["start"]).unwrap_or(0);
                let hash = &e["sample_sha256"];
                let pc = hex(addr_value(&e["pc"]).unwrap_or(0));
                if let Some(s) = samples.iter_mut().find(|s| s["sample_sha256"] == *hash) {
                    s["key_ons"] = json!(s["key_ons"].as_u64().unwrap() + 1);
                    s["last_run_frame"] = e["run_frame"].clone();
                    for (key, value) in
                        [("voices", e["voice"].clone()), ("pcs", json!(pc)), ("starts", json!(hex(start)))]
                    {
                        let list = s[key].as_array_mut().unwrap();
                        if !list.contains(&value) && list.len() < 8 {
                            list.push(value);
                        }
                    }
                    continue;
                }
                // The latest recorded upload covering the start address.
                let upload = uploads.iter().rev().find(|u| {
                    let (at, len) = (addr_value(&u["address"]).unwrap_or(0), u["bytes"].as_u64().unwrap_or(0) as u32);
                    (at..at + len).contains(&start)
                });
                let source = upload.map(|u| {
                    let offset = start - addr_value(&u["address"]).unwrap_or(0);
                    let mut v = json!({"source": u["source"], "run_frame": u["run_frame"], "upload_address": u["address"], "offset": offset});
                    if let Some(ram) = addr_value(&u["ram"]) {
                        v["ram"] = json!(hex(ram + offset));
                    }
                    v
                });
                samples.push(json!({
                    "sample_sha256": hash, "bytes": e["sample_bytes"], "looped": e["looped"],
                    "key_ons": 1, "first_run_frame": e["run_frame"], "last_run_frame": e["run_frame"],
                    "voices": [e["voice"]], "pcs": [pc], "starts": [hex(start)], "rate_hz": e["rate_hz"],
                    "function": e.get("function"), "upload": source,
                }));
            }
            _ => {}
        }
    }
    let shown = samples.len().min(32);
    json!({
        "enabled": true, "file": dir.join("spu.jsonl"), "events": log.counts, "recorded": log.events.len(),
        "complete": total == log.events.len() as u64, "truncated": total > log.events.len() as u64,
        "voices_keyed": voices.into_iter().flatten().collect::<Vec<_>>(),
        "samples_page": crate::page::metadata(samples.len(), 0, shown, 32),
        "samples": &samples[..shown],
        "matching": "sample_sha256 equals assets_extract adpcm_sha256 for the VAB/VAG waveform with the same ADPCM bytes; upload.ram is the sample's main RAM source for loads/trace_cd provenance",
        "timing": "frame/run_frame as in other logs; cycle is the system clock; end events are detected per device step",
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chosen(root: &Path, v: Value) -> crate::Result<Chosen> {
        let o: crate::run::ObserveArgs = serde_json::from_value(v).unwrap();
        adjustments(root, &o, &None, &mut None)
    }

    /// By default the port's adjustments where each applies, the rest said
    /// why; named ones are refused where they cannot apply.
    #[test]
    fn a_run_takes_the_ports_adjustments_where_they_apply() {
        let root = std::env::temp_dir().join(format!("psx-run-adjust-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let c = chosen(&root, json!({})).unwrap();
        assert_eq!(c.set.names(), ["steady"]);
        assert!(c.tick.is_none());
        assert!(c.unavailable["latch"].as_str().unwrap().starts_with("no tick for latch"), "{:?}", c.unavailable);
        assert_eq!(c.unavailable["fix"], "fixes/ holds no C");
        assert!(chosen(&root, json!({"adjust": ["stock"]})).unwrap().set.is_stock());
        assert!(chosen(&root, json!({"adjust": []})).unwrap().set.is_stock());
        assert_eq!(chosen(&root, json!({"adjust": ["steady"]})).unwrap().unavailable.len(), 0);
        let refused = |v: Value, says: &str| {
            let e = chosen(&root, v).err().unwrap();
            assert!(e.to_string().contains(says), "{e}");
        };
        refused(json!({"adjust": ["stock", "steady"]}), "name it alone");
        refused(json!({"adjust": ["off"]}), "scenario's [off]");
        refused(json!({"adjust": ["fast"]}), "run knows latch, steady and fix");
        refused(json!({"adjust": ["latch"]}), "latch needs the game's tick");
        refused(json!({"adjust": ["fix"]}), "fixes/ holds no C");
        refused(json!({"adjust": ["steady"], "fix_build": "fix.json"}), "it goes with fix");
        // A kept build runs without fixes/.
        std::fs::write(root.join("fix.json"), r#"{"functions": [], "image": "00", "sources": "s"}"#).unwrap();
        let c = chosen(&root, json!({"adjust": ["fix"], "fix_build": root.join("fix.json")})).unwrap();
        assert_eq!(c.set.fix.unwrap().sources, "s");
        let _ = std::fs::remove_dir_all(&root);
    }
}
