//! `psx run`: bounded execution of a disc, EXE or state on the native core.
//! Arguments and shared helpers live here; the run itself is `run_native`.

use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use crate::Error;
use crate::commands::{Ctx, Op, Outcome};
use schemars::JsonSchema;
use serde::Deserialize;

fn d_frames() -> u32 {
    60
}
fn d_run_timeout() -> u64 {
    120
}
fn d_max_events() -> u32 {
    200_000
}

#[derive(clap::Args, Deserialize, JsonSchema)]
pub struct RunCmdArgs {
    /// Disc image (.cue/.bin/.iso/.chd) or PS-X EXE; omit to boot the BIOS or continue a state.
    pub media: Option<PathBuf>,
    /// BIOS override; otherwise exact checkpoint identity, project defaults or regional local discovery.
    #[arg(long)]
    pub bios: Option<PathBuf>,
    /// Frames (vsyncs) to run: from power-on, or from the restored state with --from-state.
    #[arg(long, default_value_t = d_frames())]
    #[serde(default = "d_frames")]
    pub frames: u32,
    /// Path to a JSON file containing complete pad states. Frames are relative to this run's start,
    /// including after --from-state. Use --input for an inline JSON array instead.
    #[arg(long)]
    pub replay: Option<PathBuf>,
    /// Inline JSON array of complete pad states, e.g. '[{"frame":0,"buttons":["CROSS"],"until":5}]'.
    /// Run-relative frame boundaries; until releases at its exclusive end; [] releases all buttons.
    #[arg(long, conflicts_with = "replay")]
    pub input: Option<crate::replay::Events>,
    /// Write watch `ADDR[:WIDTH]` in hex, repeatable. Native: every CPU/DMA store into
    /// the range goes to writes.jsonl with pc, value, old value,
    /// `frame` (absolute, from power-on) and `run_frame` (from the run start).
    #[arg(long = "watch")]
    #[serde(default, rename = "watch")]
    pub watches: Vec<String>,
    /// Continue from a state saved by an earlier run (state.bin) instead of power-on.
    #[arg(long)]
    pub from_state: Option<PathBuf>,
    /// Write state.bin into the run directory at the end.
    #[arg(long)]
    #[serde(default)]
    pub save_state: bool,
    /// Output directory (default .psx/runs/<id>).
    #[arg(long)]
    pub out: Option<PathBuf>,
    /// Wall-clock limit in seconds; the run stops as "partial" when it is hit.
    #[arg(long, default_value_t = d_run_timeout())]
    #[serde(default = "d_run_timeout")]
    pub timeout: u64,
    /// Log CD-ROM controller and CD DMA register writes (cd.jsonl).
    #[arg(long)]
    #[serde(default)]
    pub trace_cd: bool,
    /// Log BIOS A0/B0/C0 function calls (bios.jsonl).
    #[arg(long)]
    #[serde(default)]
    pub trace_bios: bool,
    /// Native: log sound voice key on/off, sample ends and sound RAM uploads to spu.jsonl.
    /// Key ons carry settings, the storing PC and a sample hash; the report groups them by sample.
    #[arg(long)]
    #[serde(default)]
    pub trace_spu: bool,
    /// Native: record the console's mixed sound output to audio.wav, 44.1 kHz stereo
    /// 16-bit, from the run start.
    #[arg(long)]
    #[serde(default)]
    pub audio: bool,
    /// Native: profile the CPU's cycles (Mednafen's timing) by function into
    /// profile.json: functions are call targets, named from --symbols.
    #[arg(long)]
    #[serde(default)]
    pub profile: bool,
    /// `.symbols.json` (verified against RAM) or `.sym` files used to name PCs; repeatable.
    #[arg(long = "symbols")]
    #[serde(default)]
    pub symbols: Vec<PathBuf>,
    /// Cap on recorded events per trace log.
    #[arg(long, default_value_t = d_max_events())]
    #[serde(default = "d_max_events")]
    pub max_events: u32,
    #[command(flatten)]
    #[serde(flatten)]
    pub trace: TraceArgs,
    #[command(flatten)]
    #[serde(flatten)]
    pub cards: CardArgs,
    #[command(flatten)]
    #[serde(flatten)]
    pub observe: ObserveArgs,
    /// Print a short summary (status, frames, files, samples, pokes); the full
    /// report is always written to <run_dir>/report.json.
    #[arg(long)]
    #[serde(default)]
    pub brief: bool,
    /// Print only status, frames, the report/screen paths and counts for the requested
    /// observations, without artifact descriptors. Overrides --brief.
    #[arg(long)]
    #[serde(default)]
    pub summary: bool,
}

impl Op for RunCmdArgs {
    fn exec(self, ctx: &Ctx) -> Outcome {
        Outcome::report(self.run(ctx))
    }
}

impl RunCmdArgs {
    pub(crate) fn run(self, ctx: &Ctx) -> crate::Result<(bool, Value)> {
        let a = self;
        let resolved = crate::context::resolve(&ctx.root, a.media, a.bios, a.from_state.as_deref(), false)?;
        if let Some(e) = resolved.setup_error() {
            return Err(e);
        }
        let bios = resolved.bios.unwrap();
        let (short, summarise) = (a.brief, a.summary);
        let (complete, mut value) = run(
            &ctx.root,
            RunArgs {
                media: resolved.media,
                bios,
                frames: a.frames,
                replay: a.replay,
                input: a.input,
                watches: a.watches,
                from_state: a.from_state,
                save_state: a.save_state,
                out: a.out,
                timeout_s: a.timeout,
                trace_cd: a.trace_cd,
                trace_bios: a.trace_bios,
                trace_spu: a.trace_spu,
                audio: a.audio,
                profile: a.profile,
                symbols: a.symbols,
                max_events: a.max_events,
                trace: a.trace,
                cards: a.cards,
                observe: a.observe,
            },
        )?;
        value["build"] = crate::context::build();
        value["resolved"] = json!({"bios_selection":resolved.bios_reason});
        value["artifacts"] = json!(
            value["files"]
                .as_object()
                .map(|files| files
                    .values()
                    .filter_map(|f| f["path"].as_str())
                    .filter_map(|p| crate::artifact::descriptor(std::path::Path::new(p)).ok())
                    .collect::<Vec<_>>())
                .unwrap_or_default()
        );
        // The full report always goes to the run directory.
        if let Some(dir) = value["run_dir"].as_str().map(PathBuf::from) {
            let p = dir.join("report.json");
            value["report"] = json!(p);
            let _ = std::fs::write(&p, serde_json::to_vec_pretty(&value).unwrap_or_default());
        }
        if summarise {
            value = summary(&value);
        } else if short {
            value = brief(&value);
        }
        Ok((complete, value))
    }
}

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
    pub trace_cd: bool,
    pub trace_bios: bool,
    pub trace_spu: bool,
    pub audio: bool,
    pub profile: bool,
    pub symbols: Vec<PathBuf>,
    pub max_events: u32,
    pub trace: TraceArgs,
    pub cards: CardArgs,
    pub observe: ObserveArgs,
}

/// Per-frame observation and intervention.
#[derive(clap::Args, Clone, Debug, Default, serde::Deserialize, schemars::JsonSchema)]
pub struct ObserveArgs {
    /// Sample a RAM/scratchpad value at every frame boundary (vblank) into samples.csv:
    /// `ADDR:FMT[:NAME]`, FMT one of u8 i8 x8 u16 i16 x16 u32 i32 x32 fxI.F (fx16.16, fx4.12),
    /// a byte string strN (str32) or a width 1/2/4 (unsigned); MCP also takes {"addr","format","name"}. Repeatable.
    #[arg(long = "sample")]
    #[serde(default, rename = "sample")]
    pub samples: Vec<crate::ram::SampleSpec>,
    /// Group every CPU store by the fields it fell in, to find the game's tick (guide
    /// topic fields). Writes tick_finder.json; run a few hundred frames of play.
    #[arg(long)]
    #[serde(default)]
    pub find_tick: bool,
    /// Stop at the first vblank where `ADDR:FMT=VALUE` holds (or != < <= > >=; guide topic
    /// fields). Repeatable: all must hold.
    #[arg(long)]
    #[serde(default)]
    pub until: Vec<String>,
    /// Sample every Nth frame only (the last frame is always sampled).
    #[arg(long)]
    pub sample_every: Option<u64>,
    /// Write memory at the start of a run frame: `ADDR=VALUE[:WIDTH|:FMT][@FRAME|@A..B|@A..]`
    /// (VALUE decimal, 0xHEX or a fraction for fx; WIDTH 1/2/4, default 4;
    /// default frame 0; a range re-applies it every frame).
    /// Reported under `pokes`; repeatable.
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

/// Physical address with main RAM mirrors folded (what read watches compare).
pub fn fold(addr: u32) -> u32 {
    let p = addr & 0x1FFF_FFFF;
    if p < 0x0080_0000 { p & 0x1F_FFFF } else { p }
}

/// `LO-HI` or `ADDR[:WIDTH]` (hex) as a folded `[lo, hi)` range.
pub fn parse_range_spec(s: &str) -> Result<(u32, u32), String> {
    let h = |x: &str| psx_media::memmap::parse_hex(x).ok_or_else(|| format!("bad hex address {x:?} in {s:?}"));
    let (lo, len) = match s.split_once('-') {
        Some((a, b)) => {
            let (a, b) = (h(a)?, h(b)?);
            (a, b.checked_sub(a).filter(|&n| n > 0).ok_or_else(|| format!("empty range {s:?}"))?)
        }
        None => match s.split_once(':') {
            Some((a, w)) => {
                (h(a)?, w.parse::<u32>().ok().filter(|&n| n > 0).ok_or_else(|| format!("bad width in {s:?}"))?)
            }
            None => (h(s)?, 4),
        },
    };
    let lo = fold(lo);
    Ok((lo, lo + len))
}

/// Memory cards.
#[derive(clap::Args, Clone, Debug, Default, serde::Deserialize, serde::Serialize, schemars::JsonSchema)]
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
            Some(p) => {
                psx_core::pad::PadKind::from_name(p).ok_or_else(|| format!("unknown pad {p:?} (digital | dualshock)"))
            }
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

/// Instruction tracing and GPU capture.
#[derive(clap::Args, Clone, Debug, Default, serde::Deserialize, schemars::JsonSchema)]
pub struct TraceArgs {
    /// Trace instructions: trace.txt (capped by --max-events; its `frame` column is
    /// absolute, see its header), coverage.txt, calls.json, owners.json (the words run inside each call), executed.bin (for coverage) and code.bin (each code page's history, for inventory).
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
    /// Capture every GPU command of run frame N. Writes gpu_frame.json (decoded
    /// primitives, texture pages, CLUTs), gpu_frame_vram.bin (VRAM at the end of that
    /// frame, which `psx assets textures` decodes), gpu_frame_screen.png (its drawing area) and
    /// VRAM before/after/wireframe PNGs.
    #[arg(long)]
    pub gpu_frame: Option<u64>,
    /// Capture the first completed frame with GP0 drawing commands, starting at
    /// --gpu-frame (default 0). Never extends --frames.
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

pub(crate) fn write_png(path: &Path, w: u32, h: u32, rgb: &[u8], color: png::ColorType) -> std::io::Result<()> {
    let file = std::fs::File::create(path)?;
    let mut enc = png::Encoder::new(std::io::BufWriter::new(file), w, h);
    enc.set_color(color);
    enc.set_depth(png::BitDepth::Eight);
    enc.write_header()?.write_image_data(rgb).map_err(std::io::Error::other)
}

pub fn run(root: &Path, a: RunArgs) -> crate::Result<(bool, Value)> {
    let events =
        crate::replay::resolve(a.replay.as_deref(), a.input.as_ref(), a.frames as u64).map_err(Error::invalid)?;
    if a.trace.gpu_draw_filter.is_some() && !a.trace.gpu_next_draw {
        return Err(Error::invalid("--gpu-draw-filter requires --gpu-next-draw"));
    }
    if a.trace.gpu_search_frames.is_some() && !a.trace.gpu_next_draw {
        return Err(Error::invalid("--gpu-search-frames requires --gpu-next-draw"));
    }
    if a.trace.gpu_search_frames.is_some_and(|n| !(1..=600).contains(&n)) {
        return Err(Error::invalid("--gpu-search-frames must be in 1..=600"));
    }
    if a.trace.gpu_next_draw
        && a.trace.gpu_frame.unwrap_or(0).checked_add(a.trace.gpu_search_frames.unwrap_or(60) as u64).is_none()
    {
        return Err(Error::invalid("GPU search frame range overflows u64"));
    }
    let input_report = crate::replay::report(&events);
    let (complete, mut value) = crate::run_native::run(root, a, events)?;
    let application = value["input"]["application"].take();
    value["input"] = input_report;
    value["input"]["application"] = application;
    Ok((complete, value))
}

/// The `--brief` view of a run report.
pub(crate) fn brief(v: &Value) -> Value {
    let mut frames = v["frames"].clone();
    if let Some(f) = frames.as_object_mut() {
        f.remove("numbering");
    }
    let mut b = json!({
        "status": v["status"], "run_dir": v["run_dir"], "termination": v["termination"], "frames": frames,
        "report": v["report"],
        "files": v["files"].as_object().map(|f| f.keys().cloned().collect::<Vec<_>>()),
        "watch": {"enabled":v["watch"]["enabled"],"hits": v["watch"]["hits"], "recorded": v["watch"]["recorded"],
            "complete":v["watch"]["complete"],"file":v["watch"]["file"]},
    });
    for k in [
        "build",
        "resolved",
        "input",
        "pad",
        "artifacts",
        "samples",
        "tick_finder",
        "until",
        "pokes",
        "screens",
        "states",
        "read_watch",
        "spu",
        "audio",
        "profile",
        "wall_ms",
    ] {
        if !v[k].is_null() {
            b[k] = v[k].clone();
        }
    }
    if v["warnings"].as_array().is_some_and(|w| !w.is_empty()) {
        b["warnings"] = v["warnings"].clone();
    }
    if let Some(events) = v["input"]["events"].as_array() {
        b["input"]["events"] = json!(events.iter().take(8).collect::<Vec<_>>());
        b["input"]["page"] = crate::page::metadata(events.len(), 0, events.len().min(8), 8);
    }
    if v["gpu_frame"].is_object() {
        let g = &v["gpu_frame"];
        let mut summary = serde_json::Map::new();
        for key in [
            "error",
            "status",
            "reason",
            "mode",
            "search",
            "frame",
            "run_frame",
            "absolute_frame",
            "complete",
            "commands",
            "draw_commands",
            "draw_filter",
            "content",
        ] {
            if let Some(value) = g.get(key) {
                summary.insert(key.into(), value.clone());
            }
        }
        b["gpu_frame"] = Value::Object(summary);
    }
    if v["trace"].is_object() {
        let t = &v["trace"];
        b["trace"] = trace_counts(t);
    }
    b
}

/// trace.txt's lines are capped (max_events); code.bin, executed.bin and
/// calls.json are whole unless the code pages or call frames dropped say not.
fn trace_counts(t: &Value) -> Value {
    json!({"instructions": t["instructions"], "lines": t["lines"], "lines_dropped": t["lines_dropped"],
        "code_pages_dropped": t["code_pages"]["dropped"], "call_frames_dropped": t["owners"]["frames_dropped"]})
}

/// `--summary`: paths and counts only. Each observation appears when it was requested.
pub(crate) fn summary(v: &Value) -> Value {
    let pick = |section: &Value, keys: &[&str]| {
        Value::Object(keys.iter().filter_map(|k| section.get(*k).map(|value| (k.to_string(), value.clone()))).collect())
    };
    let mut s = json!({
        "status": v["status"], "run_dir": v["run_dir"], "termination": v["termination"], "report": v["report"],
        "frames": pick(&v["frames"], &["requested", "reached", "start"]),
        "screen": v["files"]["screen.png"]["path"],
        "next_call": {"tool": "read", "arguments": {"path": v["run_dir"]}},
    });
    if let Some(state) = v["files"]["state.bin"]["path"].as_str() {
        s["state"] = json!(state);
    }
    if v["warnings"].as_array().is_some_and(|w| !w.is_empty()) {
        s["warnings"] = v["warnings"].clone();
    }
    if v["watch"]["enabled"] == true {
        s["watch"] = pick(&v["watch"], &["hits", "recorded", "complete", "file"]);
    }
    if v["trace"].is_object() {
        s["trace"] = trace_counts(&v["trace"]);
    }
    let sections: [(&str, &[&str]); 10] = [
        ("until", &["met", "run_frame", "values"]),
        ("tick_finder", &["file", "fields", "pairs"]),
        ("profile", &["file", "cycles", "functions", "top"]),
        ("read_watch", &["hits", "recorded", "complete", "file"]),
        ("samples", &["file", "rows"]),
        ("spu", &["file", "events", "recorded", "complete"]),
        ("audio", &["file", "seconds", "peak"]),
        ("gpu_frame", &["status", "reason", "frame", "run_frame", "draw_commands"]),
        ("screens", &["dir", "count"]),
        ("states", &[]),
    ];
    for (key, keys) in sections {
        if !v[key].is_null() {
            s[key] = if keys.is_empty() { v[key].clone() } else { pick(&v[key], keys) };
        }
    }
    if v["event_logs"].as_object().is_some_and(|m| !m.is_empty()) {
        s["event_logs"] = v["event_logs"].clone();
    }
    if let Some(pokes) = v["pokes"].as_array() {
        s["pokes"] = json!(pokes.len());
    }
    if v["input"]["application"]["scheduled_events"].as_u64().is_some_and(|n| n > 0) {
        s["input"] = pick(&v["input"]["application"], &["scheduled_events", "applied_events", "pending_events"]);
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_summary_says_whether_the_trace_files_are_whole() {
        let v = json!({"trace": {"instructions": 9, "lines": 5, "lines_dropped": 4,
            "code_pages": {"kept": 3, "dropped": 0}, "owners": {"frames_dropped": 2}}});
        let t = &summary(&v)["trace"];
        assert_eq!(t["lines_dropped"], 4);
        assert_eq!(t["code_pages_dropped"], 0);
        assert_eq!(t["call_frames_dropped"], 2);
    }
}
