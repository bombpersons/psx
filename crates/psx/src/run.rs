//! `psx run`: bounded execution in a PCSX-Redux worker process.

use std::path::{Path, PathBuf};
use std::time::Duration;

use psx_redux::{Backend, Job, Media, ReplayEvent, Watch};
use serde_json::{json, Value};

pub struct RunArgs {
    pub media: Option<PathBuf>,
    pub bios: PathBuf,
    pub frames: u32,
    pub replay: Option<PathBuf>,
    pub watches: Vec<String>,
    pub from_state: Option<PathBuf>,
    pub save_state: bool,
    pub out: Option<PathBuf>,
    pub timeout_s: u64,
    /// Dynarec without debugger: faster, no watches.
    pub fast: bool,
    pub trace_cd: bool,
    pub trace_bios: bool,
}

fn err(code: &str, message: impl std::fmt::Display) -> Value {
    json!({"error": {"code": code, "message": message.to_string()}})
}

fn parse_watch(s: &str) -> Result<Watch, String> {
    let (addr, width) = s.split_once(':').unwrap_or((s, "4"));
    let addr = u32::from_str_radix(addr.trim_start_matches("0x").trim_start_matches("0X"), 16)
        .map_err(|_| format!("bad watch address {addr:?}"))?;
    let width: u32 = width.parse().map_err(|_| format!("bad watch width {width:?}"))?;
    if ![1, 2, 4].contains(&width) {
        return Err("watch width must be 1, 2 or 4".into());
    }
    Ok(Watch { addr, width })
}

fn write_png(path: &Path, w: u32, h: u32, rgb: &[u8], color: png::ColorType) -> std::io::Result<()> {
    let file = std::fs::File::create(path)?;
    let mut enc = png::Encoder::new(std::io::BufWriter::new(file), w, h);
    enc.set_color(color);
    enc.set_depth(png::BitDepth::Eight);
    enc.write_header()?.write_image_data(rgb).map_err(std::io::Error::other)
}

fn next_run_dir(root: &Path) -> PathBuf {
    let runs = root.join(".psx/runs");
    let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis()).unwrap_or(0);
    runs.join(format!("{stamp}-{}", std::process::id()))
}

pub fn run(root: &Path, a: RunArgs) -> (bool, Value) {
    let mut backend = match Backend::from_project(root) {
        Ok(b) => b,
        Err(e) => return (false, err(e.code(), e)),
    };
    backend.interpreter = !a.fast;
    let watches = match a.watches.iter().map(|w| parse_watch(w)).collect::<Result<Vec<_>, _>>() {
        Ok(w) => w,
        Err(e) => return (false, err("invalid_arguments", e)),
    };
    let replay: Vec<ReplayEvent> = match &a.replay {
        Some(p) => match std::fs::read_to_string(p).map_err(|e| e.to_string()).and_then(|t| serde_json::from_str(&t).map_err(|e| e.to_string())) {
            Ok(r) => r,
            Err(e) => return (false, err("invalid_arguments", format!("replay {}: {e}", p.display()))),
        },
        None => Vec::new(),
    };
    let media = match &a.media {
        None => Media::None,
        Some(p) => {
            let ext = p.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
            if ["exe", "ps-exe", "psx"].contains(&ext.as_str()) {
                Media::Exe(p.clone())
            } else {
                Media::Disc(p.clone())
            }
        }
    };
    let job = Job {
        bios: a.bios.clone(),
        media,
        frames: a.frames,
        replay,
        watches,
        max_watch_hits: 10_000,
        load_state: a.from_state.clone(),
        save_state: a.save_state,
        timeout: Duration::from_secs(a.timeout_s),
        realtime: false,
        trace_cd: a.trace_cd,
        trace_bios: a.trace_bios,
    };
    let dir = a.out.clone().unwrap_or_else(|| next_run_dir(root));
    let out = match psx_redux::run(&backend, &job, &dir) {
        Ok(o) => o,
        Err(e) => return (false, err(e.code(), e)),
    };

    let mut files = serde_json::Map::new();
    let mut hash = |name: &str| {
        let p = out.file(name);
        if let Ok(h) = psx_redux::sha256_file(&p) {
            files.insert(name.into(), json!({"path": p, "sha256": h}));
        }
    };
    for name in ["ram.bin", "scratch.bin", "vram.bin", "screen.bin", "state.bin", "writes.jsonl", "cd.jsonl", "bios.jsonl"] {
        hash(name);
    }
    if let Ok((w, h, rgb)) = out.screen_rgb() {
        let p = out.file("screen.png");
        if write_png(&p, w, h, &rgb, png::ColorType::Rgb).is_ok() {
            files.insert("screen.png".into(), json!({"path": p}));
        }
    }
    if let Ok(bytes) = std::fs::read(out.file("vram.bin")) {
        let mut vram = psx_gpu::Vram::default();
        vram.load_bytes(&bytes);
        let p = out.file("vram.png");
        if write_png(&p, 1024, 512, &vram.rgba15(0, 0, 1024, 512), png::ColorType::Rgba).is_ok() {
            files.insert("vram.png".into(), json!({"path": p}));
        }
    }
    let complete = out.report.termination == "frames_reached" && out.report.reached_frames >= a.frames;
    let shown_hits: Vec<_> = out.hits.iter().take(64).collect();
    (
        complete,
        json!({
            "status": if complete { "complete" } else { "partial" },
            "run_dir": out.dir,
            "termination": out.report.termination,
            "frames": {"requested": out.report.requested_frames, "reached": out.report.reached_frames, "unit": "vsync"},
            "cpu_cycles": out.report.cpu_cycles,
            "registers": out.report.registers,
            "screen": out.report.screen,
            "watch": {"hits": out.report.watch_hits, "recorded": out.report.watch_hits_recorded, "shown": shown_hits.len(), "first": shown_hits},
            "event_logs": out.report.event_logs,
            "bios_call_counts": out.report.bios_call_counts,
            "files": files,
            "identity": {
                "backend": backend.binary, "backend_sha256": backend.identity().ok(),
                "bios": a.bios, "bios_sha256": psx_redux::sha256_file(&a.bios).ok(),
                "media": a.media, "from_state": a.from_state,
                "interpreter": backend.interpreter,
            },
            "wall_ms": out.wall_ms,
        }),
    )
}
