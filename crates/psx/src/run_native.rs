//! `psx run --backend native`: the same bounded run as the Redux backend, on
//! psx-core, with observations taken from the emulated hardware itself.

use std::path::Path;
use std::time::Instant;

use psx_core::disasm::reg;
use psx_core::state::{disc_fingerprint, MediaId, StateError};
use psx_core::trace::{Namer, TraceConfig, Tracer};
use psx_core::Machine;
use serde_json::{json, Value};

use crate::run::{write_png, RunArgs};

fn err(code: &str, message: impl std::fmt::Display) -> (bool, Value) {
    (false, json!({"error": {"code": code, "message": message.to_string()}}))
}

mod gpu_capture;
use gpu_capture::{CapturedFrame, GpuCapture};

fn sha256(bytes: &[u8]) -> String {
    psx_redux::sha256_bytes(bytes)
}

pub fn run(root: &Path, a: RunArgs) -> (bool, Value) {
    let bios = match std::fs::read(&a.bios) {
        Ok(b) => b,
        Err(e) => return err("io_error", format!("{}: {e}", a.bios.display())),
    };
    let bios_sha = sha256(&bios);
    // Open the medium first: a restored state needs it to check identity.
    let mut exe: Option<Vec<u8>> = None;
    let mut disc: Option<psx_media::Disc> = None;
    let mut media_id: Option<MediaId> = None;
    if let Some(p) = &a.media {
        let ext = p.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
        if ["exe", "ps-exe", "psx"].contains(&ext.as_str()) {
            let bytes = match std::fs::read(p) {
                Ok(b) => b,
                Err(e) => return err("io_error", format!("{}: {e}", p.display())),
            };
            media_id = Some(MediaId { kind: "exe".into(), sha256: sha256(&bytes) });
            exe = Some(bytes);
        } else {
            match psx_media::Disc::open(p) {
                Ok(d) => {
                    media_id = Some(MediaId { kind: "disc".into(), sha256: disc_fingerprint(&d) });
                    disc = Some(d);
                }
                Err(e) => return err(e.code(), e),
            }
        }
    }
    let mut m = match &a.from_state {
        Some(p) => {
            let bytes = match std::fs::read(p) {
                Ok(b) => b,
                Err(e) => return err("io_error", format!("{}: {e}", p.display())),
            };
            let (header, mut m) = match Machine::load_state(&bytes, bios) {
                Ok(r) => r,
                Err(e @ StateError::BiosMismatch { .. }) => return err("identity_mismatch", e),
                Err(e) => return err("invalid_state", e),
            };
            if header.media != media_id {
                return err(
                    "identity_mismatch",
                    format!(
                        "state was saved with media {}, but --media is {}",
                        serde_json::to_string(&header.media).unwrap_or_default(),
                        serde_json::to_string(&media_id).unwrap_or_default()
                    ),
                );
            }
            // Reinsert without resetting the drive: its state came from the file.
            m.bus.cdrom.disc = disc;
            if a.cards.given() {
                return err("invalid_arguments", "memory cards and the pad are part of the save state; drop --memcard1/--memcard2/--pad with --from-state");
            }
            m
        }
        None => {
            let mut m = Machine::new(bios);
            match a.cards.load() {
                Ok(cards) => m.bus.sio0.cards = cards,
                Err(e) => return err("invalid_arguments", e),
            }
            match a.cards.pad_kind() {
                Ok(k) => m.bus.sio0.pad.kind = k,
                Err(e) => return err("invalid_arguments", e),
            }
            if let Some(bytes) = &exe {
                if let Err(e) = m.sideload_exe(bytes, 500_000_000) {
                    return err("backend_failed", e);
                }
            } else if disc.is_some() {
                m.insert_disc(disc);
            }
            m
        }
    };
    // Replay events: pad state from each frame (frames counted from the run start).
    let events = match a.replay.as_deref().map(crate::replay::load).transpose() {
        Ok(e) => e.unwrap_or_default(),
        Err(e) => return err("invalid_arguments", e),
    };
    for w in &a.watches {
        let (addr, width) = w.split_once(':').unwrap_or((w, "4"));
        let Ok(addr) = u32::from_str_radix(addr.trim_start_matches("0x").trim_start_matches("0X"), 16) else {
            return err("invalid_arguments", format!("bad watch address {addr}"));
        };
        // Any byte length: the native watch covers [ADDR, ADDR+WIDTH).
        let width: u32 = match width.parse() {
            Ok(w) if w >= 1 => w,
            _ => return err("invalid_arguments", format!("bad watch width {width:?} (bytes, e.g. 4)")),
        };
        m.bus.obs.watch(addr, width);
    }
    let obs = &a.observe;
    let mut sampler = match obs.samples.iter().map(|s| crate::ram::parse_sample(s)).collect::<Result<Vec<_>, _>>() {
        Ok(cols) if cols.is_empty() => None,
        Ok(cols) => Some(crate::ram::Sampler::new(cols, obs.sample_every.unwrap_or(1))),
        Err(e) => return err("invalid_arguments", e),
    };
    let mut pokes = match obs.pokes.iter().map(|s| crate::ram::parse_poke(s)).collect::<Result<Vec<_>, _>>() {
        Ok(p) => p,
        Err(e) => return err("invalid_arguments", e),
    };
    let read_ranges = match obs.watch_reads.iter().map(|s| crate::run::parse_range_spec(s)).collect::<Result<Vec<_>, _>>() {
        Ok(r) => r,
        Err(e) => return err("invalid_arguments", format!("--watch-reads: {e}")),
    };
    let mut reads: Vec<Value> = Vec::new();
    let mut read_hits: u64 = 0;
    let mut readers: std::collections::HashMap<u32, u64> = Default::default();
    if obs.snapshot_every == Some(0) || obs.sample_every == Some(0) {
        return err("invalid_arguments", "--snapshot-every and --sample-every take N >= 1");
    }
    if a.trace_cd {
        m.bus.obs.cd = Some(Vec::new());
    }
    if a.trace_bios {
        m.bus.obs.bios = Some(Vec::new());
    }

    let scopes = match crate::load_symbols(&a.symbols) {
        Ok(t) => std::rc::Rc::new(t),
        Err(e) => return err(e.code(), e),
    };
    let dir = a.out.clone().unwrap_or_else(|| {
        let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis()).unwrap_or(0);
        root.join(".psx/runs").join(format!("{stamp}-{}-native{}", std::process::id(), crate::instance_suffix()))
    });
    if let Err(e) = std::fs::create_dir_all(&dir) {
        return err("io_error", format!("{}: {e}", dir.display()));
    }
    let mut tracer = if a.trace.trace {
        match make_tracer(&a, &scopes, m.bus.frames, &dir) {
            Ok(t) => Some(t),
            Err(e) => return err("invalid_arguments", e),
        }
    } else {
        None
    };

    let started = Instant::now();
    // Frames count from power-on (like the Redux backend), including any BIOS
    // boot spent before an EXE was side-loaded; after --from-state they count
    // from the restored state (Redux counts from SaveStateLoaded).
    let start_frame = if a.from_state.is_some() { m.bus.frames } else { 0 };
    let mut gpu_capture = GpuCapture::new(&a.trace);
    let Some(target) = start_frame.checked_add(a.frames as u64) else {
        return err("invalid_arguments", "run frame range overflows u64");
    };
    let deadline = std::time::Duration::from_secs(a.timeout_s);
    let mut next = 0;
    let mut termination = "frames_reached";
    let mut steps: u64 = 0;
    let mut last_rel = u64::MAX;
    let mut screens: Vec<Value> = Vec::new();
    let mut states: Vec<Value> = Vec::new();
    let mut boundary_err: Option<String> = None;
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
        while next < events.len() && events[next].frame <= rel {
            crate::replay::apply(&mut m, &events[next]);
            next += 1;
        }
        // Everything above acts only when the frame counter changes, so without
        // per-instruction observers (tracer, read watches) the machine can run the
        // rest of the frame in one call (stopping at the next timeout check).
        if tracer.is_some() || !read_ranges.is_empty() {
            let info = m.step();
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
        return err("io_error", e);
    }
    if termination == "frames_reached" {
        // The final boundary: the state the run's files capture.
        let rel = m.bus.frames - start_frame;
        if let Some(s) = &mut sampler {
            s.row(rel, m.bus.frames, &m.bus.ram, &m.bus.scratch, true);
        }
        if let Err(e) = frame_outputs(&m, rel, obs, &dir, &media_id, &mut screens, &mut states) {
            return err("io_error", e);
        }
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
        .filter(|h| !(obs.watch_changes_only && h.old.is_some() && h.old == h.value))
        .map(|h| with_run_frame(annotate_hit(h)))
        .collect();
    put("writes.jsonl", jsonl(hits.clone()).as_bytes());
    let mut event_logs = serde_json::Map::new();
    if let Some(cd) = &m.bus.obs.cd {
        put("cd_native.jsonl", jsonl(cd.iter().map(|t| with_run_frame(serde_json::to_value(t).unwrap())).collect()).as_bytes());
        event_logs.insert("cd_native".into(), json!({"events": cd.len(), "recorded": cd.len()}));
    }
    if let Some(b) = &m.bus.obs.bios {
        put("bios.jsonl", jsonl(b.iter().map(|t| with_run_frame(serde_json::to_value(t).unwrap())).collect()).as_bytes());
        event_logs.insert("bios".into(), json!({"events": b.len(), "recorded": b.len()}));
    }
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
            "hits": read_hits, "recorded": read_hits.min(a.max_events as u64), "file": dir.join("reads.jsonl"),
            "readers": top.iter().take(16).map(|&(pc, n)| {
                let mut v = json!({"pc_hex": format!("0x{pc:08X}"), "count": n});
                if let Some((name, check, exe)) = crate::name_pc(&scopes, pc, Some(&m.bus.ram)) {
                    v["function"] = json!({"name": name, "exe": exe, "check": check.as_str()});
                }
                v
            }).collect::<Vec<_>>(),
        })
    };
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
            Some(frame) => match gpu_frame_outputs(frame, &dir, &mut files) {
                Ok(v) => v,
                Err(e) => return err("io_error", e),
            },
            None => c.missing(termination),
        },
        None => Value::Null,
    };
    let trace_report = match &mut tracer {
        Some(t) => match trace_outputs(t, &m, &scopes, &dir, start_frame, &mut files) {
            Ok(v) => v,
            Err(e) => return err("io_error", e),
        },
        None => Value::Null,
    };
    let shown: Vec<Value> = hits
        .iter()
        .take(64)
        .map(|v| {
            let mut v = v.clone();
            if let Some(pc) = v["pc"].as_u64() {
                if let Some((name, check, exe)) = crate::name_pc(&scopes, pc as u32, Some(&m.bus.ram)) {
                    v["function"] = json!({"name": name, "exe": exe, "check": check.as_str()});
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
    (
        complete,
        json!({
            "status": if complete { "complete" } else { "partial" },
            "run_dir": dir,
            "termination": termination,
            "frames": {"requested": a.frames, "reached": m.bus.frames - start_frame, "unit": "vsync", "start": start_frame, "end": m.bus.frames,
                "numbering": "run frames (--frames, --replay, --poke, --gpu-frame, --trace-from-frame, samples run_frame) count from the run start; `frame` in writes.jsonl, trace.txt, samples.csv and the other logs is absolute (vblanks since power-on) = run_frame + start"},
            "cpu_cycles": m.bus.now,
            "instructions": steps,
            "registers": registers,
            "screen": {"width": w, "height": h, "bpp": 16},
            "watch": {"hits": m.bus.obs.hit_count, "recorded": m.bus.obs.hits.len(), "shown": shown.len(), "first": shown},
            "event_logs": event_logs,
            "trace": trace_report,
            "gpu_frame": gpu_report,
            "samples": samples_report,
            "read_watch": reads_report,
            "pokes": if pokes.is_empty() { Value::Null } else { json!(pokes.iter().map(|p| p.report()).collect::<Vec<_>>()) },
            "screens": if obs.snapshot_every.is_some() { json!({"dir": dir.join("screens"), "count": screens.len(), "first": screens.first(), "last": screens.last()}) } else { Value::Null },
            "states": if obs.save_state_at.is_empty() { Value::Null } else { json!(states) },
            "memcards": cards,
            "pad": {"kind": format!("{:?}", m.bus.sio0.pad.kind).to_lowercase(), "analog": m.bus.sio0.pad.analog, "locked": m.bus.sio0.pad.locked, "motors": [m.bus.sio0.pad.motors.0, m.bus.sio0.pad.motors.1]},
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
    )
}

/// A write hit as JSON with hex strings next to the numbers and signed views
/// of the stored value (by store width) for CPU writes.
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
) -> Result<(), String> {
    if let Some(n) = obs.snapshot_every {
        if rel.is_multiple_of(n) {
            let sd = dir.join("screens");
            std::fs::create_dir_all(&sd).map_err(|e| format!("{}: {e}", sd.display()))?;
            let (w, h, rgba) = m.bus.gpu.display_rgba();
            let p = sd.join(format!("f{rel:06}.png"));
            write_png(&p, w, h, &rgba, png::ColorType::Rgba).map_err(|e| format!("{}: {e}", p.display()))?;
            screens.push(json!({"run_frame": rel, "frame": m.bus.frames, "path": p}));
        }
    }
    if obs.save_state_at.contains(&rel) && !states.iter().any(|s| s["run_frame"] == rel) {
        let p = dir.join(format!("state_f{rel:06}.bin"));
        let bytes = m.save_state(media_id.clone());
        std::fs::write(&p, &bytes).map_err(|e| format!("{}: {e}", p.display()))?;
        states.push(json!({"run_frame": rel, "frame": m.bus.frames, "path": p, "sha256": sha256(&bytes)}));
    }
    Ok(())
}

/// Hex address (optionally 0x-prefixed) or a function name from --symbols:
/// `(start, end of the function if known)`.
fn resolve(spec: &str, scopes: &[crate::ScopedSymbols]) -> Result<(u32, Option<u32>), String> {
    let hex = spec.trim_start_matches("0x").trim_start_matches("0X");
    if let Ok(v) = u32::from_str_radix(hex, 16) {
        return Ok((v, None));
    }
    scopes
        .iter()
        .find_map(|s| s.table.find(spec))
        .ok_or_else(|| format!("{spec:?} is neither a hex address nor a function in --symbols"))
}

fn make_tracer(a: &RunArgs, scopes: &std::rc::Rc<Vec<crate::ScopedSymbols>>, start_frame: u64, dir: &Path) -> Result<Tracer, String> {
    let t = &a.trace;
    let mut ranges = Vec::new();
    for r in &t.trace_ranges {
        let (lo, hi) = match r.split_once('-') {
            Some((lo, hi)) => (resolve(lo, scopes)?.0, resolve(hi, scopes)?.0),
            None => {
                let (lo, end) = resolve(r, scopes)?;
                (lo, end.ok_or_else(|| format!("{r:?}: no following symbol to bound it; use LO-HI"))?)
            }
        };
        if hi <= lo {
            return Err(format!("empty trace range {r:?}"));
        }
        ranges.push((lo & 0x1FFF_FFFF, (lo & 0x1FFF_FFFF) + (hi - lo)));
    }
    let start_pc = t.trace_start.as_deref().map(|s| resolve(s, scopes).map(|r| r.0)).transpose()?;
    let cfg = TraceConfig { from_frame: start_frame + t.trace_from_frame, start_pc, frames: t.trace_frames, ranges, max_lines: a.max_events as u64 };
    let path = dir.join("trace.txt");
    let mut file = std::fs::File::create(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    {
        use std::io::Write;
        writeln!(
            file,
            "# frame cycle pc word disassembly ; effects. `frame` counts vblanks from power-on; run frame = frame - {start_frame}"
        )
        .map_err(|e| format!("{}: {e}", path.display()))?;
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
) -> Result<Value, String> {
    t.finish(&m.bus).map_err(|e| e.to_string())?;
    let name = |pc: u32| crate::name_pc(scopes, pc, Some(&m.bus.ram)).map(|(n, check, exe)| json!({"name": n, "exe": exe, "check": check.as_str()}));
    let coverage = t.coverage();
    let mut text = String::from("# start end instructions executions [function at start]\n");
    for &(s, e, n, total) in &coverage {
        let f = name(s).map(|v| format!(" {}", v["name"].as_str().unwrap_or_default())).unwrap_or_default();
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
    for (file, bytes) in [("coverage.txt", text.into_bytes()), ("calls.json", serde_json::to_vec_pretty(&calls_json).unwrap())] {
        let p = dir.join(file);
        std::fs::write(&p, &bytes).map_err(|e| format!("{}: {e}", p.display()))?;
        files.insert(file.into(), json!({"path": p, "sha256": sha256(&bytes)}));
    }
    files.insert("trace.txt".into(), json!({"path": dir.join("trace.txt")}));
    let exceptions: serde_json::Map<String, Value> =
        t.exceptions.iter().map(|(code, n)| (psx_core::trace::exception_name(*code).to_string(), json!(n))).collect();
    let window = |w: Option<(u64, u64)>| w.map(|(f, c)| json!({"frame": f, "run_frame": f as i64 - start_frame as i64, "cycle": c}));
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
) -> Result<Value, String> {
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
        let cands: Vec<&Vec<usize>> = match c.vertices.iter().map(|v| by_xy.get(&(v.x - ox, v.y - oy))).collect::<Option<Vec<_>>>() {
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
    let bytes = serde_json::to_vec_pretty(&doc).map_err(|e| e.to_string())?;
    std::fs::write(&p, &bytes).map_err(|e| format!("{}: {e}", p.display()))?;
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
    std::fs::write(&p, &vram_bytes).map_err(|e| format!("{}: {e}", p.display()))?;
    files.insert("gpu_frame_vram.bin".into(), json!({"path": p, "sha256": sha256(&vram_bytes)}));
    let mut after = end_vram.rgba15(0, 0, 1024, 512);
    png("gpu_frame_after.png", &after, files);
    // The captured frame as drawn: its drawing area at the end of the frame.
    let (x1, y1, x2, y2) = area;
    let (aw, ah) = ((x2 - x1 + 1).clamp(0, 1024), (y2 - y1 + 1).clamp(0, 512));
    if aw > 0 && ah > 0 && x1 >= 0 && y1 >= 0 && x1 + aw <= 1024 && y1 + ah <= 512 {
        let p = dir.join("gpu_frame_screen.png");
        if write_png(&p, aw as u32, ah as u32, &end_vram.rgba15(x1 as _, y1 as _, aw as _, ah as _), png::ColorType::Rgba).is_ok() {
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
