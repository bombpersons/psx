//! `psx run`: bounded execution of a disc, EXE or state, in a PCSX-Redux
//! worker process (`--backend redux`, the oracle) or on the native core
//! (`--backend native`, see `run_native`).

use std::path::{Path, PathBuf};
use std::time::Duration;

use psx_redux::{Backend, Job, Media, ReplayEvent, Watch};
use serde_json::{json, Value};

pub struct RunArgs {
    pub media: Option<PathBuf>,
    pub bios: PathBuf,
    pub frames: u32,
    pub replay: Option<PathBuf>,
    pub input: Option<crate::replay::Events>,
    pub watches: Vec<String>,
    pub from_state: Option<PathBuf>,
    pub save_state: bool,
    pub out: Option<PathBuf>,
    pub timeout_s: u64,
    /// Dynarec without debugger: faster, no watches.
    pub fast: bool,
    pub trace_cd: bool,
    pub trace_bios: bool,
    pub symbols: Vec<PathBuf>,
    pub max_events: u32,
    /// "redux" or "native".
    pub backend: String,
    pub trace: TraceArgs,
    pub cards: CardArgs,
    pub observe: ObserveArgs,
}

/// Per-frame observation and intervention (native backend).
#[derive(clap::Args, Clone, Debug, Default, serde::Deserialize, schemars::JsonSchema)]
pub struct ObserveArgs {
    /// Sample a RAM/scratchpad value at every frame boundary (vblank) into samples.csv:
    /// `ADDR:FMT[:NAME]`, FMT one of u8 i8 x8 u16 i16 x16 u32 i32 x32 fxI.F (fx16.16, fx4.12);
    /// little-endian, read byte-exact from emulated memory; repeatable.
    #[arg(long = "sample")]
    #[serde(default, rename = "sample")]
    pub samples: Vec<String>,
    /// Sample every Nth frame only (the last frame is always sampled).
    #[arg(long)]
    pub sample_every: Option<u64>,
    /// Write memory at the start of a run frame: `ADDR=VALUE[:WIDTH|:FMT][@FRAME|@A..B|@A..]`
    /// (VALUE decimal, -decimal or 0xHEX, or a fraction for fx formats; WIDTH 1/2/4, default 4;
    /// default frame 0; a range re-applies it every frame). Reported under `pokes`; repeatable.
    #[arg(long = "poke")]
    #[serde(default, rename = "poke")]
    pub pokes: Vec<String>,
    /// Save the screen every N frames as screens/fNNNNNN.png (run frames).
    #[arg(long)]
    pub snapshot_every: Option<u64>,
    /// Also save a state at the start of these run frames (state_fNNNNNN.bin); repeatable.
    #[arg(long = "save-state-at")]
    #[serde(default)]
    pub save_state_at: Vec<u64>,
    /// Leave stores that do not change the watched value (old == new) out of writes.jsonl.
    #[arg(long)]
    #[serde(default)]
    pub watch_changes_only: bool,
    /// Read watch: log every CPU load from `LO-HI` (hex, HI exclusive) or `ADDR[:WIDTH]` to
    /// reads.jsonl with pc and value (capped by --max-events); repeatable. Shows which code
    /// reads a variable, or which script bytes a bytecode interpreter consumes.
    #[arg(long = "watch-reads")]
    #[serde(default)]
    pub watch_reads: Vec<String>,
}

impl ObserveArgs {
    pub fn given(&self) -> bool {
        !self.samples.is_empty()
            || !self.pokes.is_empty()
            || self.snapshot_every.is_some()
            || !self.save_state_at.is_empty()
            || self.watch_changes_only
            || !self.watch_reads.is_empty()
    }
}

/// Physical address with main RAM mirrors folded (what read watches compare).
pub fn fold(addr: u32) -> u32 {
    let p = addr & 0x1FFF_FFFF;
    if p < 0x0080_0000 {
        p & 0x1F_FFFF
    } else {
        p
    }
}

/// `LO-HI` or `ADDR[:WIDTH]` (hex) as a folded `[lo, hi)` range.
pub fn parse_range_spec(s: &str) -> Result<(u32, u32), String> {
    let h = |x: &str| u32::from_str_radix(x.trim().trim_start_matches("0x").trim_start_matches("0X"), 16).map_err(|_| format!("bad hex address {x:?} in {s:?}"));
    let (lo, len) = match s.split_once('-') {
        Some((a, b)) => {
            let (a, b) = (h(a)?, h(b)?);
            (a, b.checked_sub(a).filter(|&n| n > 0).ok_or_else(|| format!("empty range {s:?}"))?)
        }
        None => match s.split_once(':') {
            Some((a, w)) => (h(a)?, w.parse::<u32>().ok().filter(|&n| n > 0).ok_or_else(|| format!("bad width in {s:?}"))?),
            None => (h(s)?, 4),
        },
    };
    let lo = fold(lo);
    Ok((lo, lo + len))
}

/// Memory cards (native backend).
#[derive(clap::Args, Clone, Debug, Default, serde::Deserialize, schemars::JsonSchema)]
pub struct CardArgs {
    /// Memory card in slot 1: a raw 128 KiB image (.mcd/.mcr) or `new` for a formatted empty card.
    /// The image file is never modified; a written card is saved as memcard1.mcd in the run directory.
    #[arg(long)]
    pub memcard1: Option<String>,
    /// Memory card in slot 2 (as --memcard1).
    #[arg(long)]
    pub memcard2: Option<String>,
    /// Controller in port 1: `digital` (default) or `dualshock` (starts digital;
    /// games switch it to analog; replay `sticks` drive it).
    #[arg(long)]
    pub pad: Option<String>,
}

impl CardArgs {
    pub fn given(&self) -> bool {
        self.memcard1.is_some() || self.memcard2.is_some() || self.pad.is_some()
    }

    pub fn pad_kind(&self) -> Result<psx_core::pad::PadKind, String> {
        match &self.pad {
            None => Ok(psx_core::pad::PadKind::Digital),
            Some(p) => psx_core::pad::PadKind::from_name(p).ok_or_else(|| format!("unknown pad {p:?} (digital | dualshock)")),
        }
    }

    /// Load the cards for slots 1 and 2.
    pub fn load(&self) -> Result<[Option<psx_core::memcard::MemCard>; 2], String> {
        let one = |spec: &Option<String>| -> Result<Option<psx_core::memcard::MemCard>, String> {
            Ok(match spec.as_deref() {
                None => None,
                Some("new") => Some(psx_core::memcard::MemCard::formatted()),
                Some(p) => {
                    let bytes = std::fs::read(p).map_err(|e| format!("{p}: {e}"))?;
                    if bytes.len() != psx_core::memcard::SIZE {
                        return Err(format!("{p}: {} bytes; a raw memory card image is 131072", bytes.len()));
                    }
                    Some(psx_core::memcard::MemCard::from_image(bytes))
                }
            })
        };
        Ok([one(&self.memcard1)?, one(&self.memcard2)?])
    }
}

#[derive(Clone, Copy, Debug, Default, clap::ValueEnum, serde::Deserialize, serde::Serialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum GpuDrawFilter {
    #[default]
    Any,
    Primitives,
    Textured,
}

/// Instruction tracing and GPU capture (native backend).
#[derive(clap::Args, Clone, Debug, Default, serde::Deserialize, schemars::JsonSchema)]
pub struct TraceArgs {
    /// Trace instructions: trace.txt (capped by --max-events; its `frame` column is
    /// absolute, see its header), coverage.txt and calls.json.
    #[arg(long)]
    #[serde(default)]
    pub trace: bool,
    /// Start tracing at this frame of the run (default: the start).
    #[arg(long, default_value_t = 0)]
    #[serde(default)]
    pub trace_from_frame: u64,
    /// Start tracing at the first execution of ADDR (hex) or a --symbols function.
    #[arg(long)]
    pub trace_start: Option<String>,
    /// Stop tracing after N frames.
    #[arg(long)]
    pub trace_frames: Option<u64>,
    /// Only write lines for LO-HI (hex) or a --symbols function; repeatable.
    #[arg(long = "trace-range")]
    #[serde(default, rename = "trace_range")]
    pub trace_ranges: Vec<String>,
    /// Capture every GPU command of run frame N: gpu_frame.json (decoded primitives,
    /// texture pages, CLUTs), gpu_frame_vram.bin (VRAM at the end of that frame, which
    /// `psx textures` decodes), gpu_frame_screen.png (its drawing area) and VRAM
    /// before/after/wireframe PNGs.
    #[arg(long)]
    pub gpu_frame: Option<u64>,
    /// Capture the first completed frame with GP0 drawing commands, starting at
    /// --gpu-frame (default 0). Never extends --frames. Native backend only.
    #[arg(long)]
    #[serde(default)]
    pub gpu_next_draw: bool,
    /// Search at most N frames for --gpu-next-draw (default 60, range 1..=600).
    #[arg(long)]
    pub gpu_search_frames: Option<u32>,
    /// Required drawing content for --gpu-next-draw: any (includes fills), primitives
    /// (excludes fill-only frames), or textured (textured polygons/rectangles).
    #[arg(long, value_enum, requires = "gpu_next_draw")]
    pub gpu_draw_filter: Option<GpuDrawFilter>,
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

pub(crate) fn write_png(path: &Path, w: u32, h: u32, rgb: &[u8], color: png::ColorType) -> std::io::Result<()> {
    let file = std::fs::File::create(path)?;
    let mut enc = png::Encoder::new(std::io::BufWriter::new(file), w, h);
    enc.set_color(color);
    enc.set_depth(png::BitDepth::Eight);
    enc.write_header()?.write_image_data(rgb).map_err(std::io::Error::other)
}

fn next_run_dir(root: &Path) -> PathBuf {
    let runs = root.join(".psx/runs");
    let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis()).unwrap_or(0);
    runs.join(format!("{stamp}-{}{}", std::process::id(), crate::instance_suffix()))
}

pub fn run(root: &Path, a: RunArgs) -> (bool, Value) {
    let events=match crate::replay::resolve(a.replay.as_deref(),a.input.as_ref(),a.frames as u64) {
        Ok(e)=>e,Err(e)=>return (false,err("invalid_arguments",e)),
    };
    let input_report=crate::replay::report(&events);
    let (ok,mut value)=run_resolved(root,a,events);
    if value.get("error").is_none(){
        let application = value["input"]["application"].take();
        value["input"] = input_report;
        value["input"]["application"] = if application.is_null() {
            json!({"observed":false,"note":"This backend reports the requested schedule, not applied-event observations."})
        } else { application };
    }
    (ok,value)
}

fn run_resolved(root: &Path, a: RunArgs, events: Vec<crate::replay::Input>) -> (bool, Value) {
    if a.trace.gpu_draw_filter.is_some() && !a.trace.gpu_next_draw {
        return (false, err("invalid_arguments", "--gpu-draw-filter requires --gpu-next-draw"));
    }
    if a.trace.gpu_search_frames.is_some() && !a.trace.gpu_next_draw {
        return (false, err("invalid_arguments", "--gpu-search-frames requires --gpu-next-draw"));
    }
    if a.trace.gpu_search_frames.is_some_and(|n| !(1..=600).contains(&n)) {
        return (false, err("invalid_arguments", "--gpu-search-frames must be in 1..=600"));
    }
    if a.trace.gpu_next_draw && a.trace.gpu_frame.unwrap_or(0).checked_add(a.trace.gpu_search_frames.unwrap_or(60) as u64).is_none() {
        return (false, err("invalid_arguments", "GPU search frame range overflows u64"));
    }
    match a.backend.as_str() {
        "native" => return crate::run_native::run(root, a, events),
        "redux" => {}
        other => return (false, err("invalid_arguments", format!("unknown backend {other:?} (redux | native)"))),
    }
    if a.trace.trace || a.trace.gpu_frame.is_some() || a.trace.gpu_next_draw {
        return (false, err("unsupported", "--trace, --gpu-frame and --gpu-next-draw need --backend native"));
    }
    if a.observe.given() {
        return (false, err("unsupported", "--sample, --poke, --snapshot-every, --save-state-at, --watch-reads and --watch-changes-only need --backend native"));
    }
    if a.cards.memcard1.is_some() || a.cards.memcard2.is_some() {
        return (false, err("unsupported", "--memcard1/--memcard2 need --backend native"));
    }
    let analog_pad = match a.cards.pad_kind() {
        Ok(k) => k == psx_core::pad::PadKind::DualShock,
        Err(e) => return (false, err("invalid_arguments", e)),
    };
    if crate::replay::uses_sticks(&events) {
        return (false, err("unsupported", "input sticks need --backend native with a dualshock pad"));
    }
    if let Some(p) = &a.from_state {
        if std::fs::read(p).is_ok_and(|b| b.starts_with(psx_core::state::MAGIC)) {
            return (false, err("invalid_state", format!("{} is a psx-core state: use --backend native", p.display())));
        }
    }
    let mut backend = match Backend::from_project(root) {
        Ok(b) => b,
        Err(e) => return (false, err(e.code(), e)),
    };
    backend.interpreter = !a.fast;
    let watches = match a.watches.iter().map(|w| parse_watch(w)).collect::<Result<Vec<_>, _>>() {
        Ok(w) => w,
        Err(e) => return (false, err("invalid_arguments", e)),
    };
    let replay: Vec<ReplayEvent> = events.iter().map(|e|ReplayEvent{frame:e.frame as u32,buttons:crate::replay::button_names(e.buttons)}).collect();
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
        max_events: a.max_events,
        analog_pad,
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
    let scopes = match crate::load_symbols(&a.symbols) {
        Ok(t) => t,
        Err(e) => return (false, err(e.code(), e)),
    };
    let ram = out.ram().ok();
    let shown_hits: Vec<Value> = out
        .hits
        .iter()
        .take(64)
        .map(|h| {
            let mut v = serde_json::to_value(h).unwrap();
            if let Some(pc) = h.pc {
                v["function"] = crate::final_name(&scopes, pc, ram.as_deref());
            }
            v
        })
        .collect();
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
