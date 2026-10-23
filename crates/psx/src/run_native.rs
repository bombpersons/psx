//! `psx run`: a bounded run on psx-core, with observations taken from the emulated hardware itself.

use std::path::Path;
use std::time::Instant;

use psx_core::Machine;
use psx_core::disasm::reg;
use psx_core::state::{MediaId, StateError, disc_fingerprint};
use psx_core::trace::{Namer, TraceConfig, Tracer};
use serde_json::{Value, json};

use crate::Error;
use crate::run::{RunArgs, write_png};

mod gpu_capture;
use gpu_capture::{CapturedFrame, GpuCapture};

fn sha256(bytes: &[u8]) -> String {
    psx_media::disc::sha256_bytes(bytes)
}

pub fn run(root: &Path, a: RunArgs, events: Vec<crate::replay::Input>) -> crate::Result<(bool, Value)> {
    let bios = match std::fs::read(&a.bios) {
        Ok(b) => b,
        Err(e) => return Err(Error::io(a.bios, e)),
    };
    let bios_sha = sha256(&bios);
    // Open the medium first: a restored state needs it to check identity.
    let Medium { exe, disc, id: media_id } = open_media(a.media.as_deref())?;
    let mut m = boot(a.from_state.as_deref(), &a.cards, bios, exe.as_deref(), disc, &media_id)?;
    // Replay events: pad state from each frame (frames counted from the run start).
    if crate::replay::uses_sticks(&events) && m.bus.sio0.pad.kind != psx_core::pad::PadKind::DualShock {
        return Err(Error::new(
            "invalid_arguments",
            "input sticks require a dualshock pad (configure it at boot; checkpoint pads are restored)",
        ));
    }
    attach_observers(&mut m, &a)?;
    let obs = &a.observe;
    let mut sampler = match obs.samples.iter().map(crate::ram::SampleSpec::parse).collect::<Result<Vec<_>, _>>() {
        Ok(cols) if cols.is_empty() => None,
        Ok(cols) => Some(crate::ram::Sampler::new(cols, obs.sample_every.unwrap_or(1))),
        Err(e) => return Err(Error::invalid(e)),
    };
    let mut pokes = match obs.pokes.iter().map(|s| crate::ram::parse_poke(s)).collect::<Result<Vec<_>, _>>() {
        Ok(p) => p,
        Err(e) => return Err(Error::invalid(e)),
    };
    let read_ranges =
        match obs.watch_reads.iter().map(|s| crate::run::parse_range_spec(s)).collect::<Result<Vec<_>, _>>() {
            Ok(r) => r,
            Err(e) => return Err(Error::invalid(format!("--watch-reads: {e}"))),
        };
    let mut reads: Vec<Value> = Vec::new();
    let mut read_hits: u64 = 0;
    let mut readers: std::collections::HashMap<u32, u64> = Default::default();
    if obs.snapshot_every == Some(0) || obs.sample_every == Some(0) {
        return Err(Error::invalid("--snapshot-every and --sample-every take N >= 1"));
    }
    let scopes = {
        let t = crate::load_symbols(&a.symbols)?;
        std::rc::Rc::new(t)
    };
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
    let mut tracer = if a.trace.trace { Some(make_tracer(&a, &scopes, m.bus.frames, &dir)?) } else { None };

    let started = Instant::now();
    // Frames count from power-on, including any BIOS boot spent before an EXE
    // was side-loaded; after --from-state they count from the restored state.
    let start_frame = if a.from_state.is_some() { m.bus.frames } else { 0 };
    let mut gpu_capture = GpuCapture::new(&a.trace, a.frames);
    let Some(target) = start_frame.checked_add(a.frames as u64) else {
        return Err(Error::invalid("run frame range overflows u64"));
    };
    let deadline = std::time::Duration::from_secs(a.timeout_s);
    let mut next = 0;
    let mut applied_input = Vec::new();
    let mut termination = "frames_reached";
    let mut steps: u64 = 0;
    let bound_watches = scopes.iter().any(|s| s.bound.is_some()) && !a.watches.is_empty();
    let mut hit_names: Vec<Value> = Vec::new();
    let observed_name = |pc, ram: &[u8]| {
        crate::name_pc(&scopes, pc, Some(ram))
            .map(|(name, check, source)| json!({"name":name,"exe":source,"check":check.as_str()}))
            .unwrap_or(Value::Null)
    };
    let mut last_rel = u64::MAX;
    let mut screens: Vec<Value> = Vec::new();
    let mut states: Vec<Value> = Vec::new();
    let mut boundary_err: Option<Error> = None;
    while m.bus.frames < target {
        let rel = m.bus.frames - start_frame;
        if rel != last_rel {
            // A frame boundary (vblank), or the run start: observe first, then intervene.
            last_rel = rel;
            if let Some(c) = &mut gpu_capture {
                c.boundary(&mut m, start_frame, false);
            }
            if let Some(s) = &mut sampler {
                s.row(rel, m.bus.frames, &m.bus.ram, &m.bus.scratch, false);
            }
            if let Err(e) = frame_outputs(&m, rel, obs, &dir, &media_id, &mut screens, &mut states) {
                boundary_err = Some(e);
                break;
            }
            for p in pokes.iter_mut().filter(|p| p.due(rel)) {
                let bus = &mut m.bus;
                p.apply(rel, &mut bus.ram, &mut bus.scratch);
            }
        }
        apply_due(&mut m, &events, &mut next, rel, &mut applied_input);
        // Everything above acts only when the frame counter changes, so without
        // per-instruction observers (tracer, read watches) the machine can run the
        // rest of the frame in one call (stopping at the next timeout check).
        if tracer.is_some() || !read_ranges.is_empty() || bound_watches {
            let info = m.step();
            if bound_watches {
                for hit in &m.bus.obs.hits[hit_names.len()..] {
                    hit_names.push(hit.pc.map(|pc| observed_name(pc, &m.bus.ram)).unwrap_or(Value::Null));
                }
            }
            if let Some(t) = &mut tracer {
                t.after_step(&info, &m.cpu, &m.bus);
            }
            if !read_ranges.is_empty() {
                if let Some(acc) = info.mem.filter(|acc| !acc.store) {
                    let p = crate::run::fold(acc.addr);
                    if read_ranges.iter().any(|&(lo, hi)| p < hi && p + acc.width as u32 > lo) {
                        read_hits += 1;
                        *readers.entry(info.pc).or_default() += 1;
                        if reads.len() < a.max_events as usize {
                            reads.push(json!({
                                "frame": m.bus.frames, "run_frame": m.bus.frames - start_frame, "cycle": m.bus.now,
                                "pc": info.pc, "pc_hex": format!("0x{:08X}", info.pc),
                                "function": observed_name(info.pc, &m.bus.ram),
                                "address": acc.addr, "address_hex": format!("0x{:08X}", acc.addr), "width": acc.width,
                                "value": acc.value, "value_hex": format!("0x{:0n$X}", acc.value, n = acc.width as usize * 2),
                            }));
                        }
                    }
                }
            }
            steps += 1;
        } else {
            let check = 1u64 << 20;
            steps += m.run_frame_steps(check - steps % check);
        }
        if steps.is_multiple_of(1 << 20) && started.elapsed() > deadline {
            termination = "timeout";
            break;
        }
    }
    if let Some(c) = &mut gpu_capture {
        c.boundary(&mut m, start_frame, true);
    }
    if let Some(e) = boundary_err {
        return Err(e);
    }
    if termination == "frames_reached" {
        // The final boundary: the state the run's files capture.
        let rel = m.bus.frames - start_frame;
        apply_due(&mut m, &events, &mut next, rel, &mut applied_input);
        if let Some(s) = &mut sampler {
            s.row(rel, m.bus.frames, &m.bus.ram, &m.bus.scratch, true);
        }
        frame_outputs(&m, rel, obs, &dir, &media_id, &mut screens, &mut states)?;
    }
    let wall_ms = started.elapsed().as_millis();

    let mut files = serde_json::Map::new();
    let mut put = |name: &str, bytes: &[u8]| {
        let p = dir.join(name);
        if std::fs::write(&p, bytes).is_ok() {
            files.insert(name.into(), json!({"path": p, "sha256": sha256(bytes)}));
        }
    };
    if a.save_state {
        put("state.bin", &m.save_state(media_id.clone()));
    }
    let mut cards = Vec::new();
    for (slot, card) in m.bus.sio0.cards.iter().enumerate() {
        if let Some(c) = card {
            let name = format!("memcard{}.mcd", slot + 1);
            if c.writes > 0 {
                put(&name, &c.data);
            }
            cards.push(json!({"slot": slot + 1, "sectors_written": c.writes, "file": (c.writes > 0).then_some(name)}));
        }
    }
    put("ram.bin", &m.bus.ram);
    put("scratch.bin", &m.bus.scratch);
    put("vram.bin", &m.bus.gpu.vram.to_bytes());
    let jsonl = |items: Vec<Value>| items.iter().map(|v| v.to_string() + "\n").collect::<String>();
    // Every log carries `frame` (absolute, from power-on, as the core counts) and
    // `run_frame` (from the run start, as --frames/--replay/--poke/--gpu-frame count).
    let with_run_frame = |mut v: Value| {
        if let Some(f) = v["frame"].as_u64() {
            v["run_frame"] = json!(f as i64 - start_frame as i64);
        }
        v
    };
    let hits: Vec<Value> = m
        .bus
        .obs
        .hits
        .iter()
        .enumerate()
        .filter(|(_, h)| !(obs.watch_changes_only && h.old.is_some() && h.old == h.value))
        .map(|(i, h)| {
            let mut v = with_run_frame(annotate_hit(h));
            if let Some(name) = hit_names.get(i).filter(|v| !v.is_null()) {
                v["function"] = name.clone();
            }
            v
        })
        .collect();
    if !a.watches.is_empty() {
        put("writes.jsonl", jsonl(hits.clone()).as_bytes());
    }
    let mut event_logs = serde_json::Map::new();
    if let Some(cd) = &m.bus.obs.cd {
        put(
            "cd_native.jsonl",
            jsonl(cd.iter().map(|t| with_run_frame(serde_json::to_value(t).unwrap())).collect()).as_bytes(),
        );
        event_logs.insert("cd_native".into(), json!({"events": cd.len(), "recorded": cd.len()}));
    }
    if let Some(b) = &m.bus.obs.bios {
        put(
            "bios.jsonl",
            jsonl(b.iter().map(|t| with_run_frame(serde_json::to_value(t).unwrap())).collect()).as_bytes(),
        );
        event_logs.insert("bios".into(), json!({"events": b.len(), "recorded": b.len()}));
    }
    let spu_report = match &m.bus.obs.spu {
        Some(log) => {
            let events: Vec<Value> = log
                .events
                .iter()
                .map(|e| {
                    let mut v = with_run_frame(serde_json::to_value(e).unwrap());
                    if let Some(pc) = e.pc {
                        let name = crate::final_name(&scopes, pc, Some(&m.bus.ram));
                        if !name.is_null() {
                            v["function"] = name;
                        }
                    }
                    v
                })
                .collect();
            put("spu.jsonl", jsonl(events.clone()).as_bytes());
            spu_summary(log, &events, &dir)
        }
        None => Value::Null,
    };
    let samples_report = match &sampler {
        Some(s) => {
            put("samples.csv", s.out.as_bytes());
            json!({
                "file": dir.join("samples.csv"), "rows": s.rows, "every": s.every,
                "columns": s.columns.iter().map(|c| json!({"name": c.name, "address_hex": format!("0x{:08X}", c.addr), "format": c.fmt.name()})).collect::<Vec<_>>(),
                "timing": "row N = memory at the Nth vblank of the run (row 0 = the starting memory), before that frame's replay input and pokes",
            })
        }
        None => Value::Null,
    };
    let reads_report = if read_ranges.is_empty() {
        Value::Null
    } else {
        put("reads.jsonl", jsonl(std::mem::take(&mut reads)).as_bytes());
        let mut top: Vec<(u32, u64)> = readers.into_iter().collect();
        top.sort_by_key(|&(pc, n)| (std::cmp::Reverse(n), pc));
        json!({
            "enabled":true,"hits": read_hits, "recorded": read_hits.min(a.max_events as u64), "file": dir.join("reads.jsonl"),
            "complete":read_hits<=a.max_events as u64,"truncated":read_hits>a.max_events as u64,
            "readers_page":crate::page::metadata(top.len(),0,top.len().min(16),16),
            "readers": top.iter().take(16).map(|&(pc, n)| {
                let mut v = json!({"pc_hex": format!("0x{pc:08X}"), "count": n});
                v["function"] = crate::final_name(&scopes, pc, Some(&m.bus.ram));
                v
            }).collect::<Vec<_>>(),
        })
    };
    if !events.is_empty() {
        let path = dir.join("input.jsonl");
        let bytes = jsonl(applied_input);
        if let Err(e) = std::fs::write(&path, bytes.as_bytes()) {
            return Err(Error::io(path, e));
        }
        files.insert("input.jsonl".into(), json!({"path":path,"sha256":sha256(bytes.as_bytes())}));
    }
    let (w, h, rgba) = m.bus.gpu.display_rgba();
    let screen = dir.join("screen.png");
    if write_png(&screen, w, h, &rgba, png::ColorType::Rgba).is_ok() {
        files.insert("screen.png".into(), json!({"path": screen}));
    }
    let vram = dir.join("vram.png");
    if write_png(&vram, 1024, 512, &m.bus.gpu.vram.rgba15(0, 0, 1024, 512), png::ColorType::Rgba).is_ok() {
        files.insert("vram.png".into(), json!({"path": vram}));
    }

    let gpu_report = match &gpu_capture {
        Some(c) => match &c.selected {
            Some(frame) => gpu_frame_outputs(frame, &dir, &mut files)?,
            None => c.missing(termination),
        },
        None => Value::Null,
    };
    let trace_report = match &mut tracer {
        Some(t) => trace_outputs(t, &m, &scopes, &dir, start_frame, &mut files)?,
        None => Value::Null,
    };
    let shown: Vec<Value> = hits
        .iter()
        .take(64)
        .map(|v| {
            let mut v = v.clone();
            if let Some(pc) = v["pc"].as_u64() {
                if v["function"].is_null() && !bound_watches {
                    v["function"] = crate::final_name(&scopes, pc as u32, Some(&m.bus.ram));
                }
            }
            v
        })
        .collect();
    let mut registers = serde_json::Map::new();
    registers.insert("pc".into(), json!(m.cpu.pc));
    for i in 0..32 {
        registers.insert(reg(i as u32).into(), json!(m.cpu.regs[i]));
    }
    registers.insert("hi".into(), json!(m.cpu.hi));
    registers.insert("lo".into(), json!(m.cpu.lo));
    registers.insert("cop0_sr".into(), json!(m.cpu.cop0[12]));
    registers.insert("cop0_cause".into(), json!(m.cpu.cop0[13]));
    registers.insert("cop0_epc".into(), json!(m.cpu.cop0[14]));
    let complete = termination == "frames_reached";
    Ok((
        complete,
        json!({
            "status": if complete { "complete" } else { "partial" },
            "run_dir": dir,
            "termination": termination,
            "warnings": crate::stall::warnings(&m),
            "frames": {"requested": a.frames, "reached": m.bus.frames - start_frame, "unit": "vsync", "start": start_frame, "end": m.bus.frames,
                "numbering": "run frames (--frames, --replay, --poke, --gpu-frame, --trace-from-frame, samples run_frame) count from the run start; `frame` in writes.jsonl, trace.txt, samples.csv and the other logs is absolute (vblanks since power-on) = run_frame + start"},
            "cpu_cycles": m.bus.now,
            "instructions": steps,
            "input":{"application":{"observed":true,"scheduled_events":events.len(),"applied_events":next,
                "pending_events":events.len()-next,"file":(!events.is_empty()).then(||dir.join("input.jsonl")),
                "note":"Actual scheduler applications, including boundary releases; does not prove the game polled the controller."}},
            "registers": registers,
            "screen": {"width": w, "height": h, "bpp": if m.bus.gpu.display.rgb24() {24} else {16}},
            "video": {"region_at_capture":if m.bus.pal(){"PAL"}else{"NTSC"},"mode":m.bus.gpu.display.mode,
                "cycles_per_vblank_at_capture":m.bus.frame_cycles(),"interlaced":m.bus.gpu.display.interlaced_480(),
                "display_start":m.bus.gpu.display.start,"horizontal_range":m.bus.gpu.display.h_range,"vertical_range":m.bus.gpu.display.v_range,
                "note":"GPU mode at capture; raw raster dimensions may change during boot."},
            "watch": {"enabled":!a.watches.is_empty(),"hits": m.bus.obs.hit_count, "recorded": m.bus.obs.hits.len(), "shown": shown.len(), "first": shown,
                "emitted":hits.len(),"complete":m.bus.obs.hit_count==m.bus.obs.hits.len() as u64,
                "truncated":m.bus.obs.hit_count>m.bus.obs.hits.len() as u64,
                "file":(!a.watches.is_empty()).then(||dir.join("writes.jsonl")),"changes_only":obs.watch_changes_only},
            "event_logs": event_logs,
            "trace": trace_report,
            "gpu_frame": gpu_report,
            "samples": samples_report,
            "read_watch": reads_report,
            "spu": spu_report,
            "pokes": if pokes.is_empty() { Value::Null } else { json!(pokes.iter().map(|p| p.report()).collect::<Vec<_>>()) },
            "screens": if obs.snapshot_every.is_some() { json!({"dir": dir.join("screens"), "count": screens.len(), "first": screens.first(), "last": screens.last()}) } else { Value::Null },
            "states": if obs.save_state_at.is_empty() { Value::Null } else { json!(states) },
            "memcards": cards,
            "pad": {"kind": format!("{:?}", m.bus.sio0.pad.kind).to_lowercase(), "analog": m.bus.sio0.pad.analog, "locked": m.bus.sio0.pad.locked,
                "buttons":crate::replay::button_names(m.bus.sio0.buttons),
                "sticks":[m.bus.sio0.pad.sticks[2],m.bus.sio0.pad.sticks[3],m.bus.sio0.pad.sticks[0],m.bus.sio0.pad.sticks[1]],
                "motors": [m.bus.sio0.pad.motors.0, m.bus.sio0.pad.motors.1]},
            "bios_call_counts": m.bus.obs.bios_counts,
            "files": files,
            "identity": {
                "backend": "psx-core", "backend_version": env!("CARGO_PKG_VERSION"),
                "timing_model": "flat per-instruction costs (Redux-like), not cycle-accurate",
                "bios": a.bios, "bios_sha256": bios_sha, "media": a.media, "media_identity": media_id,
                "from_state": a.from_state, "state_format": psx_core::state::FORMAT,
            },
            "wall_ms": wall_ms,
        }),
    ))
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

/// Apply the replay events due by run frame `rel`, logging each one.
fn apply_due(m: &mut Machine, events: &[crate::replay::Input], next: &mut usize, rel: u64, applied: &mut Vec<Value>) {
    while *next < events.len() && events[*next].frame <= rel {
        let e = &events[*next];
        crate::replay::apply(m, e);
        applied.push(json!({"event_index":*next,"scheduled_run_frame":e.frame,
            "run_frame":rel,"frame":m.bus.frames,"buttons":crate::replay::button_names(e.buttons),
            "sticks":e.sticks}));
        *next += 1;
    }
}

/// Write watches and the CD/BIOS/SPU event logs the run asked for.
fn attach_observers(m: &mut Machine, a: &RunArgs) -> crate::Result<()> {
    for w in &a.watches {
        let (addr, width) = w.split_once(':').unwrap_or((w, "4"));
        let Some(addr) = psx_media::memmap::parse_hex(addr) else {
            return Err(Error::invalid(format!("bad watch address {addr}")));
        };
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
    v["address_hex"] = json!(format!("0x{:08X}", h.address));
    if let Some(pc) = h.pc {
        v["pc_hex"] = json!(format!("0x{pc:08X}"));
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
fn frame_outputs(
    m: &Machine,
    rel: u64,
    obs: &crate::run::ObserveArgs,
    dir: &Path,
    media_id: &Option<MediaId>,
    screens: &mut Vec<Value>,
    states: &mut Vec<Value>,
) -> crate::Result<()> {
    if let Some(n) = obs.snapshot_every {
        if rel.is_multiple_of(n) {
            let sd = dir.join("screens");
            std::fs::create_dir_all(&sd).map_err(|e| Error::io(&sd, e))?;
            let (w, h, rgba) = m.bus.gpu.display_rgba();
            let p = sd.join(format!("f{rel:06}.png"));
            write_png(&p, w, h, &rgba, png::ColorType::Rgba).map_err(|e| Error::io(&p, e))?;
            screens.push(json!({"run_frame": rel, "frame": m.bus.frames, "path": p}));
        }
    }
    if obs.save_state_at.contains(&rel) && !states.iter().any(|s| s["run_frame"] == rel) {
        let p = dir.join(format!("state_f{rel:06}.bin"));
        let bytes = m.save_state(media_id.clone());
        std::fs::write(&p, &bytes).map_err(|e| Error::io(&p, e))?;
        states.push(json!({"run_frame": rel, "frame": m.bus.frames, "path": p, "sha256": sha256(&bytes)}));
    }
    Ok(())
}

/// Hex address (optionally 0x-prefixed) or a function name from --symbols:
/// `(start, end of the function if known)`.
fn resolve(spec: &str, scopes: &[crate::ScopedSymbols]) -> crate::Result<(u32, Option<u32>)> {
    if let Some(v) = psx_media::memmap::parse_hex(spec) {
        return Ok((v, None));
    }
    scopes
        .iter()
        .find_map(|s| s.table.find(spec))
        .ok_or_else(|| Error::invalid(format!("{spec:?} is neither a hex address nor a function in --symbols")))
}

fn make_tracer(
    a: &RunArgs,
    scopes: &std::rc::Rc<Vec<crate::ScopedSymbols>>,
    start_frame: u64,
    dir: &Path,
) -> crate::Result<Tracer> {
    let t = &a.trace;
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
    let start_pc = t.trace_start.as_deref().map(|s| resolve(s, scopes).map(|r| r.0)).transpose()?;
    let cfg = TraceConfig {
        from_frame: start_frame + t.trace_from_frame,
        start_pc,
        frames: t.trace_frames,
        ranges,
        max_lines: a.max_events as u64,
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
    let mut calls: Vec<(u32, &psx_core::trace::CallStat)> = t.calls.iter().map(|(k, v)| (*k, v)).collect();
    calls.sort_by_key(|(addr, c)| (std::cmp::Reverse(c.count), *addr));
    let calls_json: Vec<Value> = calls
        .iter()
        .map(|(addr, c)| {
            let mut callers: Vec<(&u32, &u64)> = c.callers.iter().collect();
            callers.sort_by_key(|(site, n)| (std::cmp::Reverse(**n), **site));
            json!({
                "address": addr, "function": name(*addr), "count": c.count,
                "address_hex": format!("0x{addr:08X}"),
                "first_frame": c.first_frame, "last_frame": c.last_frame,
                "first_run_frame": c.first_frame as i64 - start_frame as i64, "last_run_frame": c.last_frame as i64 - start_frame as i64,
                "callers": callers.iter().take(16).map(|(site, n)| json!({"pc": site, "pc_hex": format!("0x{site:08X}"), "count": n})).collect::<Vec<_>>(),
            })
        })
        .collect();
    for (file, bytes) in
        [("coverage.txt", text.into_bytes()), ("calls.json", serde_json::to_vec_pretty(&calls_json).unwrap())]
    {
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
    Ok(json!({
        "started": window(t.started), "stopped": window(t.stopped),
        "instructions": t.instructions,
        "lines": t.lines, "lines_dropped": t.lines_dropped, "max_lines": t.cfg.max_lines,
        "interrupts": t.interrupts, "exceptions": exceptions,
        "coverage": {"runs": coverage.len(), "unique_instructions": coverage.iter().map(|c| c.2 as u64).sum::<u64>()},
        "top_calls": calls_json.iter().take(20).cloned().collect::<Vec<_>>(),
        "distinct_call_targets": calls_json.len(),
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
                let start = e["start"].as_u64().unwrap_or(0);
                let hash = &e["sample_sha256"];
                let pc = format!("0x{:08X}", e["pc"].as_u64().unwrap_or(0));
                if let Some(s) = samples.iter_mut().find(|s| s["sample_sha256"] == *hash) {
                    s["key_ons"] = json!(s["key_ons"].as_u64().unwrap() + 1);
                    s["last_run_frame"] = e["run_frame"].clone();
                    for (key, value) in [("voices", e["voice"].clone()), ("pcs", json!(pc)), ("starts", json!(start))] {
                        let list = s[key].as_array_mut().unwrap();
                        if !list.contains(&value) && list.len() < 8 {
                            list.push(value);
                        }
                    }
                    continue;
                }
                // The latest recorded upload covering the start address.
                let upload = uploads.iter().rev().find(|u| {
                    let (at, len) = (u["address"].as_u64().unwrap_or(0), u["bytes"].as_u64().unwrap_or(0));
                    (at..at + len).contains(&start)
                });
                let source = upload.map(|u| {
                    let offset = start - u["address"].as_u64().unwrap_or(0);
                    let mut v = json!({"source": u["source"], "run_frame": u["run_frame"], "upload_address": u["address"], "offset": offset});
                    if let Some(ram) = u["ram"].as_u64() {
                        v["ram_hex"] = json!(format!("0x{:08X}", ram + offset));
                    }
                    v
                });
                samples.push(json!({
                    "sample_sha256": hash, "bytes": e["sample_bytes"], "looped": e["looped"],
                    "key_ons": 1, "first_run_frame": e["run_frame"], "last_run_frame": e["run_frame"],
                    "voices": [e["voice"]], "pcs": [pc], "starts": [start], "rate_hz": e["rate_hz"],
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
        "matching": "sample_sha256 equals assets_extract adpcm_sha256 for the VAB/VAG waveform with the same ADPCM bytes; upload.ram_hex is the sample's main RAM source for loads/trace_cd provenance",
        "timing": "frame/run_frame as in other logs; cycle is the system clock; end events are detected per device step",
    })
}
