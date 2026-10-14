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
        let width: u32 = width.parse().unwrap_or(4);
        m.bus.obs.watch(addr, width);
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
        root.join(".psx/runs").join(format!("{stamp}-{}-native", std::process::id()))
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
    // GPU frame capture: (draw state and VRAM at the frame start, commands).
    let gpu_at = a.trace.gpu_frame.map(|f| start_frame + f);
    let mut gpu_start: Option<(psx_gpu::DrawState, psx_gpu::vram::Vram)> = None;
    let mut gpu_cmds: Option<Vec<Vec<u32>>> = None;

    let target = start_frame + a.frames as u64;
    let deadline = std::time::Duration::from_secs(a.timeout_s);
    let mut next = 0;
    let mut termination = "frames_reached";
    let mut steps: u64 = 0;
    while m.bus.frames < target {
        let rel = m.bus.frames - start_frame;
        while next < events.len() && events[next].frame <= rel {
            crate::replay::apply(&mut m, &events[next]);
            next += 1;
        }
        if let Some(at) = gpu_at {
            if gpu_start.is_none() && m.bus.frames == at {
                gpu_start = Some((m.bus.gpu.state, m.bus.gpu.vram.clone()));
                m.bus.gpu.log = Some(Vec::new());
            } else if gpu_cmds.is_none() && m.bus.frames > at && gpu_start.is_some() {
                gpu_cmds = m.bus.gpu.log.take();
            }
        }
        let info = m.step();
        if let Some(t) = &mut tracer {
            t.after_step(&info, &m.cpu, &m.bus);
        }
        steps += 1;
        if steps.is_multiple_of(1 << 20) && started.elapsed() > deadline {
            termination = "timeout";
            break;
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
    let hits: Vec<Value> = m.bus.obs.hits.iter().map(|h| serde_json::to_value(h).unwrap()).collect();
    put("writes.jsonl", jsonl(hits.clone()).as_bytes());
    let mut event_logs = serde_json::Map::new();
    if let Some(cd) = &m.bus.obs.cd {
        put("cd_native.jsonl", jsonl(cd.iter().map(|t| serde_json::to_value(t).unwrap()).collect()).as_bytes());
        event_logs.insert("cd_native".into(), json!({"events": cd.len(), "recorded": cd.len()}));
    }
    if let Some(b) = &m.bus.obs.bios {
        put("bios.jsonl", jsonl(b.iter().map(|t| serde_json::to_value(t).unwrap()).collect()).as_bytes());
        event_logs.insert("bios".into(), json!({"events": b.len(), "recorded": b.len()}));
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

    if gpu_cmds.is_none() {
        gpu_cmds = m.bus.gpu.log.take(); // the run ended inside the captured frame
    }
    let gpu_report = match (&gpu_start, &gpu_cmds) {
        (Some((state, before)), Some(cmds)) => match gpu_frame_outputs(state, before, cmds, &m, &dir, gpu_at.unwrap_or(0) - start_frame, &mut files) {
            Ok(v) => v,
            Err(e) => return err("io_error", e),
        },
        _ if gpu_at.is_some() => json!({"error": "the run ended before the requested frame"}),
        _ => Value::Null,
    };
    let trace_report = match &mut tracer {
        Some(t) => match trace_outputs(t, &m, &scopes, &dir, &mut files) {
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
            "frames": {"requested": a.frames, "reached": m.bus.frames - start_frame, "unit": "vsync"},
            "cpu_cycles": m.bus.now,
            "instructions": steps,
            "registers": registers,
            "screen": {"width": w, "height": h, "bpp": 16},
            "watch": {"hits": m.bus.obs.hit_count, "recorded": m.bus.obs.hits.len(), "shown": shown.len(), "first": shown},
            "event_logs": event_logs,
            "trace": trace_report,
            "gpu_frame": gpu_report,
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
    let file = std::fs::File::create(&path).map_err(|e| format!("{}: {e}", path.display()))?;
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
                "first_frame": c.first_frame, "last_frame": c.last_frame,
                "callers": callers.iter().take(16).map(|(site, n)| json!({"pc": site, "count": n})).collect::<Vec<_>>(),
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
    let window = |w: Option<(u64, u64)>| w.map(|(f, c)| json!({"frame": f, "cycle": c}));
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

/// Write the decoded GPU commands of one frame, with VRAM before/after and a
/// wireframe of every primitive drawn over the after image.
fn gpu_frame_outputs(
    start: &psx_gpu::DrawState,
    before: &psx_gpu::vram::Vram,
    cmds: &[Vec<u32>],
    m: &Machine,
    dir: &Path,
    frame: u64,
    files: &mut serde_json::Map<String, Value>,
) -> Result<Value, String> {
    use psx_gpu::capture::decode;
    use std::collections::BTreeMap;
    let decoded = decode(start, cmds);
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
    let summary = json!({
        "frame": frame,
        "commands": decoded.len(),
        "by_kind": kinds,
        "textured": textured,
        "semi_transparent": semi,
        "texture_pages": pages.iter().map(|((x, y, d), n)| json!({"x": x, "y": y, "depth": d, "uses": n})).collect::<Vec<_>>(),
        "cluts": cluts.iter().map(|((x, y), n)| json!({"x": x, "y": y, "uses": n})).collect::<Vec<_>>(),
        "draw_state_at_start": {"offset": start.offset, "area": start.area, "texpage": start.texpage},
    });
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
    let mut after = m.bus.gpu.vram.rgba15(0, 0, 1024, 512);
    png("gpu_frame_after.png", &after, files);
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
