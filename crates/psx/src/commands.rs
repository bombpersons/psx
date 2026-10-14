//! One argument type and one implementation per command, shared by the CLI
//! (clap) and the MCP server (serde + JSON Schema).
//!
//! Field names are the CLI long flags with `_` for `-` (`--trace-cd` is
//! `trace_cd`); repeatable flags are arrays. Positional CLI arguments are plain
//! fields.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use psx_media::iso9660::FileSystem;
use psx_media::{memmap, Disc, MediaError, PsxExe};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{json, Value};

/// Where a command runs: `root` is the project directory (bios/, media/,
/// external/, .psx/). Relative paths in arguments resolve against the process
/// working directory, which for both the CLI and `psx mcp` is that root
/// (`--root`/`PSX_ROOT` change into it before any command runs).
pub struct Ctx {
    pub root: PathBuf,
}

/// A command's JSON result and how the CLI prints it.
pub struct Outcome {
    pub value: Value,
    /// Process exit code (0 success; 1 failure, partial run or divergence; 2 unsupported media).
    pub exit: u8,
    /// Pretty-printed (results) or compact (media errors), as the CLI always did.
    pub pretty: bool,
    /// Plain-text mode (`--text`): the CLI prints `value["text"]` as is; MCP
    /// still returns the whole JSON value (with the text in its `text` field).
    pub text: bool,
}

impl Outcome {
    fn media(r: psx_media::Result<Value>) -> Self {
        match r {
            Ok(value) => Outcome { value, exit: 0, pretty: true, text: false },
            Err(e) => Outcome {
                value: json!({"error": {"code": e.code(), "message": e.to_string()}}),
                exit: if matches!(e, MediaError::Unsupported(_)) { 2 } else { 1 },
                pretty: false,
                text: false,
            },
        }
    }

    fn pair((ok, value): (bool, Value)) -> Self {
        Outcome { value, exit: if ok { 0 } else { 1 }, pretty: true, text: false }
    }

    /// A media result printed as plain text (its `text` field) when `text` is set.
    fn media_text(r: psx_media::Result<Value>, text: bool) -> Self {
        let mut o = Self::media(r);
        o.text = text && o.exit == 0;
        o
    }

    /// The CLI's stdout text (without the trailing newline).
    pub fn render(&self) -> String {
        if self.text {
            return self.value["text"].as_str().unwrap_or_default().trim_end().to_string();
        }
        if self.pretty {
            serde_json::to_string_pretty(&self.value).unwrap()
        } else {
            self.value.to_string()
        }
    }
}

/// A command usable from both front ends.
pub trait Op: serde::de::DeserializeOwned + JsonSchema {
    fn exec(self, ctx: &Ctx) -> Outcome;
}

fn hex_u32(s: &str) -> Option<u32> {
    u32::from_str_radix(s.trim_start_matches("0x").trim_start_matches("0X"), 16).ok()
}

fn deadline(timeout_s: Option<u64>) -> Option<Instant> {
    timeout_s.map(|s| Instant::now() + Duration::from_secs(s))
}

// Defaults shared by clap and serde.
fn d_extracted() -> PathBuf {
    PathBuf::from("extracted")
}
fn d_one() -> u32 {
    1
}
fn d_preview() -> usize {
    64
}
fn d_frames() -> u32 {
    60
}
fn d_run_timeout() -> u64 {
    120
}
fn d_max_events() -> u32 {
    200_000
}
fn d_backend() -> String {
    "redux".into()
}
fn d_sigs() -> PathBuf {
    PathBuf::from("external/psx_psyq_signatures")
}
fn d_min_concrete() -> usize {
    24
}
fn d_lockstep_steps() -> u64 {
    100_000
}
fn d_context() -> usize {
    12
}
fn d_native_frames() -> u64 {
    300
}
fn d_max_steps() -> u64 {
    20_000_000_000
}
fn d_tail() -> usize {
    48
}
fn d_pad() -> String {
    "digital".into()
}
fn d_count() -> u32 {
    32
}
fn d_cell() -> usize {
    128
}
fn d_samples() -> usize {
    16
}

#[derive(clap::Args, Deserialize, JsonSchema)]
pub struct DiscArgs {
    /// Disc image: .cue, raw .bin (single data track) or cooked .iso.
    pub image: PathBuf,
    /// Also stream SHA-256 over every image file (slow for full discs).
    #[arg(long)]
    #[serde(default)]
    pub hash: bool,
}

impl Op for DiscArgs {
    fn exec(self, _: &Ctx) -> Outcome {
        Outcome::media(crate::disc_summary(&self.image, self.hash))
    }
}

#[derive(clap::Args, Deserialize, JsonSchema)]
pub struct LsArgs {
    /// Disc image: .cue, raw .bin or .iso.
    pub image: PathBuf,
}

impl Op for LsArgs {
    fn exec(self, _: &Ctx) -> Outcome {
        Outcome::media((|| {
            let disc = Disc::open(&self.image)?;
            let fs = FileSystem::read(&disc)?;
            Ok(json!({"volume": fs.volume, "entries": fs.entries.iter().map(crate::entry_json).collect::<Vec<_>>(), "warnings": fs.warnings}))
        })())
    }
}

#[derive(clap::Args, Deserialize, JsonSchema)]
pub struct ExtractArgs {
    /// Disc image: .cue, raw .bin or .iso.
    pub image: PathBuf,
    /// ISO path, e.g. `/DATA/LEVEL1.BIN` or `cdrom:\SLUS_000.79;1`.
    pub path: Option<String>,
    /// Extract every file instead of one path.
    #[arg(long)]
    #[serde(default)]
    pub all: bool,
    /// Output directory.
    #[arg(short, long, default_value_os_t = d_extracted())]
    #[serde(default = "d_extracted")]
    pub out: PathBuf,
}

impl Op for ExtractArgs {
    fn exec(self, _: &Ctx) -> Outcome {
        Outcome::media(crate::extract(&self.image, self.path.as_deref(), self.all, &self.out))
    }
}

#[derive(clap::Args, Deserialize, JsonSchema)]
pub struct ExeArgs {
    /// PS-X EXE file path, or `image.cue::/PATH` for an executable on a disc.
    pub source: String,
}

impl Op for ExeArgs {
    fn exec(self, _: &Ctx) -> Outcome {
        Outcome::media(crate::exe_cmd(&self.source))
    }
}

#[derive(clap::Args, Deserialize, JsonSchema)]
pub struct XaArgs {
    /// Disc image: .cue, raw .bin or .iso.
    pub image: PathBuf,
    /// ISO path of the stream, e.g. `/MUSIC.XA`.
    pub path: String,
    /// Channel to decode (needs -o).
    #[arg(long)]
    pub channel: Option<u8>,
    /// Also require this subheader file number.
    #[arg(long)]
    pub file: Option<u8>,
    /// WAV output path for the decoded channel.
    #[arg(short, long)]
    pub out: Option<PathBuf>,
}

impl Op for XaArgs {
    fn exec(self, _: &Ctx) -> Outcome {
        Outcome::media(crate::xa_cmd::run(&self.image, &self.path, self.file, self.channel, self.out.as_deref()))
    }
}

#[derive(clap::Args, Deserialize, JsonSchema)]
pub struct StrArgs {
    /// `image.cue::/PATH`, or the image with the ISO path in `path`.
    pub source: String,
    pub path: Option<String>,
    /// Frames to write, 1-based inclusive: `A..B`, `A..`, `..B` or `N`.
    #[arg(long)]
    pub frames: Option<String>,
    /// XA channel for audio.wav (default: the one with the most sectors).
    #[arg(long)]
    pub channel: Option<u8>,
    #[arg(long)]
    #[serde(default)]
    pub no_audio: bool,
    /// Also write an uncompressed 4:4:4 movie.y4m.
    #[arg(long)]
    #[serde(default)]
    pub y4m: bool,
    /// Output directory for frames, audio.wav, contact.png, frames.json, summary.json.
    #[arg(short, long)]
    pub out: PathBuf,
}

impl Op for StrArgs {
    fn exec(self, _: &Ctx) -> Outcome {
        Outcome::media(crate::str_cmd::split_source(&self.source, self.path.as_deref()).and_then(|(image, path)| {
            crate::str_cmd::run(crate::str_cmd::Args {
                image: &image,
                path: &path,
                out: &self.out,
                frames: self.frames.as_deref(),
                channel: self.channel,
                no_audio: self.no_audio,
                y4m: self.y4m,
            })
        }))
    }
}

#[derive(clap::Args, Deserialize, JsonSchema)]
pub struct MemcardArgs {
    /// Raw 128 KiB memory card image (.mcd/.mcr).
    pub image: PathBuf,
}

impl Op for MemcardArgs {
    fn exec(self, _: &Ctx) -> Outcome {
        let image = self.image;
        Outcome::media((|| {
            let bytes = std::fs::read(&image).map_err(|e| MediaError::io(&image, e))?;
            if bytes.len() != psx_core::memcard::SIZE {
                return Err(MediaError::Malformed(format!("{} bytes; a raw memory card image is 131072", bytes.len())));
            }
            let card = psx_core::memcard::MemCard::from_image(bytes);
            let saves = card.saves();
            let used: usize = saves.iter().map(|s| s.blocks.len()).sum();
            let formatted = &card.sector(0)[..2] == b"MC";
            Ok(json!({"image": image, "formatted": formatted, "blocks_used": used, "blocks_free": 15 - used.min(15), "saves": saves}))
        })())
    }
}

#[derive(clap::Args, Deserialize, JsonSchema)]
pub struct TexturesArgs {
    /// A native run directory (vram.bin, and gpu_frame.json unless --page).
    pub run_dir: PathBuf,
    /// One page as X,Y,DEPTH (VRAM halfword x, line y, 4/8/15 bpp).
    #[arg(long)]
    pub page: Option<String>,
    /// CLUT position X,Y for 4/8-bit pages.
    #[arg(long)]
    pub clut: Option<String>,
    /// Output directory (default <run_dir>/textures).
    #[arg(short, long)]
    pub out: Option<PathBuf>,
    /// VRAM dump to decode. Default: gpu_frame_vram.bin (the VRAM at the end of the
    /// captured frame) when present and no --page is given, else vram.bin (end of the run).
    #[arg(long)]
    pub vram: Option<PathBuf>,
}

impl Op for TexturesArgs {
    fn exec(self, _: &Ctx) -> Outcome {
        Outcome::media(crate::textures::run(&self.run_dir, self.page.as_deref(), self.clut.as_deref(), self.out.as_deref(), self.vram.as_deref()))
    }
}

#[derive(clap::Args, Deserialize, JsonSchema)]
pub struct SectorArgs {
    /// Disc image: .cue, raw .bin or .iso.
    pub image: PathBuf,
    /// First logical block address (sector 16 is the ISO9660 volume descriptor).
    pub lba: u32,
    /// Number of sectors (at most 256).
    #[arg(long, default_value_t = d_one())]
    #[serde(default = "d_one")]
    pub count: u32,
    /// Hex bytes shown per sector.
    #[arg(long, default_value_t = d_preview())]
    #[serde(default = "d_preview")]
    pub preview: usize,
}

impl Op for SectorArgs {
    fn exec(self, _: &Ctx) -> Outcome {
        Outcome::media(crate::sectors(&self.image, self.lba, self.count, self.preview))
    }
}

#[derive(clap::Args, Deserialize, JsonSchema)]
pub struct TemplateCreateArgs {
    /// Destination directory: new or empty.
    pub dest: PathBuf,
    /// Cargo package name (defaults to the directory name).
    #[arg(long)]
    pub name: Option<String>,
}

impl Op for TemplateCreateArgs {
    fn exec(self, _: &Ctx) -> Outcome {
        Outcome::media(crate::template::create(&self.dest, self.name.as_deref()))
    }
}

#[derive(clap::Args, Deserialize, JsonSchema)]
pub struct RunCmdArgs {
    /// Disc image (.cue/.bin/.iso) or PS-X EXE; omit to boot the BIOS or continue a state.
    pub media: Option<PathBuf>,
    /// BIOS image, e.g. bios/scph7502.bin.
    #[arg(long)]
    pub bios: PathBuf,
    /// Frames (vsyncs) to run: from power-on, or from the restored state with --from-state.
    #[arg(long, default_value_t = d_frames())]
    #[serde(default = "d_frames")]
    pub frames: u32,
    /// JSON array of {"frame": n, "buttons": ["CROSS", ...]}; frames count from the run start.
    #[arg(long)]
    pub replay: Option<PathBuf>,
    /// Write watch `ADDR[:WIDTH]` in hex, repeatable. Native: every CPU/DMA store into
    /// the range goes to writes.jsonl with pc, value, old value (hex and signed views too),
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
    /// Use the dynarec without the debugger: faster, but no --watch or traces.
    #[arg(long)]
    #[serde(default)]
    pub fast: bool,
    /// Log CD-ROM controller and CD DMA register writes (cd.jsonl).
    #[arg(long)]
    #[serde(default)]
    pub trace_cd: bool,
    /// Log BIOS A0/B0/C0 function calls (bios.jsonl).
    #[arg(long)]
    #[serde(default)]
    pub trace_bios: bool,
    /// `.symbols.json` (verified against RAM) or `.sym` files used to name PCs; repeatable.
    #[arg(long = "symbols")]
    #[serde(default)]
    pub symbols: Vec<PathBuf>,
    /// Cap on recorded events per trace log.
    #[arg(long, default_value_t = d_max_events())]
    #[serde(default = "d_max_events")]
    pub max_events: u32,
    /// Emulator: `redux` (PCSX-Redux worker process, default; needs external/pcsx-redux) or
    /// `native` (the in-tree psx-core: exact watches, --trace, --gpu-frame, --sample, --poke,
    /// --snapshot-every, memory cards, native states).
    #[arg(long, default_value_t = d_backend())]
    #[serde(default = "d_backend")]
    pub backend: String,
    #[command(flatten)]
    #[serde(flatten)]
    pub trace: crate::run::TraceArgs,
    #[command(flatten)]
    #[serde(flatten)]
    pub cards: crate::run::CardArgs,
    #[command(flatten)]
    #[serde(flatten)]
    pub observe: crate::run::ObserveArgs,
    /// Print a short summary (status, frames, files, samples, pokes); the full
    /// report is always written to <run_dir>/report.json.
    #[arg(long)]
    #[serde(default)]
    pub brief: bool,
}

/// The `--brief` view of a run report.
fn brief(v: &Value) -> Value {
    if v.get("error").is_some() {
        return v.clone();
    }
    let mut b = json!({
        "status": v["status"], "run_dir": v["run_dir"], "termination": v["termination"], "frames": v["frames"],
        "report": v["report"],
        "files": v["files"].as_object().map(|f| f.keys().cloned().collect::<Vec<_>>()),
        "watch": {"hits": v["watch"]["hits"], "recorded": v["watch"]["recorded"]},
    });
    for k in ["samples", "pokes", "screens", "states", "read_watch", "wall_ms"] {
        if !v[k].is_null() {
            b[k] = v[k].clone();
        }
    }
    if v["gpu_frame"].is_object() {
        let g = &v["gpu_frame"];
        b["gpu_frame"] = match g.get("error") {
            Some(e) => json!({"error": e}),
            None => json!({"frame": g["frame"], "commands": g["commands"]}),
        };
    }
    if v["trace"].is_object() {
        let t = &v["trace"];
        b["trace"] = json!({"instructions": t["instructions"], "lines": t["lines"], "lines_dropped": t["lines_dropped"]});
    }
    b
}

impl Op for RunCmdArgs {
    fn exec(self, ctx: &Ctx) -> Outcome {
        let a = self;
        let short = a.brief;
        let mut out = Outcome::pair(crate::run::run(
            &ctx.root,
            crate::run::RunArgs {
                media: a.media,
                bios: a.bios,
                frames: a.frames,
                replay: a.replay,
                watches: a.watches,
                from_state: a.from_state,
                save_state: a.save_state,
                out: a.out,
                timeout_s: a.timeout,
                fast: a.fast,
                trace_cd: a.trace_cd,
                trace_bios: a.trace_bios,
                symbols: a.symbols,
                max_events: a.max_events,
                backend: a.backend,
                trace: a.trace,
                cards: a.cards,
                observe: a.observe,
            },
        ));
        // The full report always goes to the run directory.
        if let Some(dir) = out.value["run_dir"].as_str().map(PathBuf::from) {
            let p = dir.join("report.json");
            out.value["report"] = json!(p);
            let _ = std::fs::write(&p, serde_json::to_vec_pretty(&out.value).unwrap_or_default());
        }
        if short {
            out.value = brief(&out.value);
        }
        out
    }
}

#[derive(clap::Args, Deserialize, JsonSchema)]
pub struct LoadsArgs {
    /// A run directory recorded with --trace-cd.
    pub run_dir: PathBuf,
    /// The disc image the run used.
    #[arg(long)]
    pub disc: PathBuf,
    /// Include at most N individual runs (spans are always complete).
    #[arg(long, default_value_t = 0)]
    #[serde(default)]
    pub limit: usize,
    /// `.symbols.json` (verified against RAM) or `.sym` files from `psx symbols`; repeatable.
    #[arg(long = "symbols")]
    #[serde(default)]
    pub symbols: Vec<PathBuf>,
}

impl Op for LoadsArgs {
    fn exec(self, _: &Ctx) -> Outcome {
        Outcome::media((|| {
            let scopes = crate::load_symbols(&self.symbols)?;
            let mut v = crate::provenance::report(&self.run_dir, &self.disc, &scopes).map_err(MediaError::Malformed)?;
            if let Some(runs) = v["runs"].as_array_mut() {
                let total = runs.len();
                runs.truncate(self.limit);
                v["runs_total"] = json!(total);
            }
            Ok(v)
        })())
    }
}

#[derive(clap::Args, Deserialize, JsonSchema)]
pub struct SymbolsArgs {
    /// PS-X EXE file, or `image.cue::/PATH`.
    pub source: String,
    /// Local signature directory (`<version>/<LIB>.json` layout).
    #[arg(long, default_value_os_t = d_sigs())]
    #[serde(default = "d_sigs")]
    pub sigs: PathBuf,
    /// Minimum non-wildcard bytes for a signature to be used.
    #[arg(long, default_value_t = d_min_concrete())]
    #[serde(default = "d_min_concrete")]
    pub min_concrete: usize,
    /// Output path prefix (default .psx/symbols/<exe name>).
    #[arg(long)]
    pub out: Option<PathBuf>,
}

impl Op for SymbolsArgs {
    fn exec(self, _: &Ctx) -> Outcome {
        Outcome::media(crate::symbols_cmd(&self.source, &self.sigs, self.min_concrete, self.out))
    }
}

#[derive(clap::Args, Deserialize, JsonSchema)]
pub struct LockstepArgs {
    /// BIOS image, e.g. bios/scph7502.bin.
    #[arg(long)]
    pub bios: PathBuf,
    /// Instructions to compare from reset.
    #[arg(long, default_value_t = d_lockstep_steps())]
    #[serde(default = "d_lockstep_steps")]
    pub steps: u64,
    /// Instructions of history shown at a divergence.
    #[arg(long, default_value_t = d_context())]
    #[serde(default = "d_context")]
    pub context: usize,
    /// GDB single-step with full register comparison (about 20 steps/s).
    #[arg(long)]
    #[serde(default)]
    pub exact: bool,
    /// Write the core's screen and VRAM as <PREFIX>.screen.png / .vram.png at the end.
    #[arg(long)]
    pub dump: Option<PathBuf>,
    /// Wall-clock limit in seconds; stops with status "timeout" (default: none).
    #[arg(long)]
    pub timeout: Option<u64>,
}

impl Op for LockstepArgs {
    fn exec(self, ctx: &Ctx) -> Outcome {
        let d = deadline(self.timeout);
        Outcome::pair(if self.exact {
            crate::lockstep::run(&ctx.root, &self.bios, self.steps, self.context, d)
        } else {
            crate::lockstep::run_trace(&ctx.root, &self.bios, self.steps, self.context, self.dump.as_deref(), d)
        })
    }
}

#[derive(clap::Args, Deserialize, JsonSchema)]
pub struct NativeArgs {
    /// BIOS image, e.g. bios/scph7502.bin.
    #[arg(long)]
    pub bios: PathBuf,
    /// Disc image to insert (CUE/BIN/ISO); omit for an empty drive.
    #[arg(long)]
    pub disc: Option<PathBuf>,
    /// Frames to run from power-on.
    #[arg(long, default_value_t = d_native_frames())]
    #[serde(default = "d_native_frames")]
    pub frames: u64,
    /// Instruction budget.
    #[arg(long, default_value_t = d_max_steps())]
    #[serde(default = "d_max_steps")]
    pub max_steps: u64,
    /// Write <PREFIX>.screen.png and <PREFIX>.vram.png at the end.
    #[arg(long)]
    pub dump: Option<PathBuf>,
    /// Stop when the PC reaches this hex address and report the tail.
    #[arg(long = "break")]
    #[serde(rename = "break")]
    pub brk: Option<String>,
    /// Instructions of history kept for --break.
    #[arg(long, default_value_t = d_tail())]
    #[serde(default = "d_tail")]
    pub tail: usize,
    /// Report every change of this RAM word (hex address) with the responsible instruction.
    #[arg(long)]
    pub watch: Option<String>,
    /// Start recording --watch changes at this frame.
    #[arg(long, default_value_t = 0)]
    #[serde(default)]
    pub watch_after: u64,
    /// JSON `[{"frame": n, "buttons": ["CROSS"]}]` (frames from power-on).
    #[arg(long)]
    pub replay: Option<PathBuf>,
    /// With --dump: also save the screen every N frames.
    #[arg(long)]
    pub snapshot_every: Option<u64>,
    /// Capture the SPU output to a WAV file.
    #[arg(long)]
    pub wav: Option<PathBuf>,
    /// Controller in port 1: `digital` (default) or `dualshock`.
    #[arg(long, default_value_t = d_pad())]
    #[serde(default = "d_pad")]
    pub pad: String,
    /// Wall-clock limit in seconds; stops with status "timeout" (default: none).
    #[arg(long)]
    pub timeout: Option<u64>,
}

impl Op for NativeArgs {
    fn exec(self, _: &Ctx) -> Outcome {
        let hex = |b: &Option<String>| b.as_deref().and_then(hex_u32);
        Outcome::pair(crate::native::run(crate::native::Options {
            bios: &self.bios,
            disc: self.disc.as_deref(),
            frames: self.frames,
            max_steps: self.max_steps,
            dump: self.dump.as_deref(),
            brk: hex(&self.brk),
            tail: self.tail,
            watch: hex(&self.watch),
            watch_after: self.watch_after,
            replay: self.replay.as_deref(),
            snapshot_every: self.snapshot_every,
            wav: self.wav.as_deref(),
            pad: &self.pad,
            deadline: deadline(self.timeout),
        }))
    }
}

#[derive(clap::Args, Deserialize, JsonSchema)]
pub struct DisasmArgs {
    /// PS-X EXE file, or `image.cue::/PATH`. Optional with --ram.
    pub source: Option<String>,
    /// Start address (hex); defaults to the entry point.
    #[arg(long)]
    pub addr: Option<String>,
    /// Instructions to decode (with --function: the maximum function length).
    #[arg(long, default_value_t = d_count())]
    #[serde(default = "d_count")]
    pub count: u32,
    /// `.sym` or `.symbols.json` files for labels; repeatable.
    #[arg(long = "symbols")]
    #[serde(default)]
    pub symbols: Vec<PathBuf>,
    /// Plain-text listing (`ADDR WORD asm ; notes`, with labels for symbols and
    /// branch targets) instead of JSON lines.
    #[arg(long)]
    #[serde(default)]
    pub text: bool,
    /// Disassemble the whole function containing this hex address: back to the
    /// nearest jal target or `addiu sp,sp,-N` prologue, forward to its final `jr ra`.
    #[arg(long)]
    pub function: Option<String>,
    /// Read code from a run's RAM (run directory or ram.bin) instead of the EXE:
    /// overlays and code loaded at run time. With an EXE source too, warns where they differ.
    #[arg(long)]
    pub ram: Option<PathBuf>,
    /// gp value (hex) for resolving gp-relative loads/stores (default: the EXE header's gp).
    #[arg(long)]
    pub gp: Option<String>,
}

fn opt_hex(s: &Option<String>, what: &str) -> psx_media::Result<Option<u32>> {
    s.as_deref().map(|a| hex_u32(a).ok_or_else(|| MediaError::Malformed(format!("bad {what} {a:?} (hex)")))).transpose()
}

impl Op for DisasmArgs {
    fn exec(self, _: &Ctx) -> Outcome {
        let DisasmArgs { source, addr, count, symbols, text, function, ram, gp } = self;
        Outcome::media_text(
            (|| {
                let mut code = crate::code::Code::open(source.as_deref(), ram.as_deref())?;
                if let Some(g) = opt_hex(&gp, "gp")? {
                    code.gp = Some(g);
                }
                let scopes = crate::load_symbols(&symbols)?;
                let entry = match &source {
                    Some(s) => Some(PsxExe::parse(&crate::read_source(s)?)?.pc),
                    None => None,
                };
                let (start, end, func) = if let Some(f) = opt_hex(&function, "function address")? {
                    let jal = code.jal_targets();
                    let (s, e) = crate::code::function_bounds(&code, f, &jal, count.max(16))
                        .ok_or_else(|| MediaError::Malformed(format!("0x{f:08X} is outside the code ({})", code.origin)))?;
                    let callers = crate::code::xrefs(&code, s, s + 4, false).iter().filter(|(_, r)| r.kind == "call").count();
                    (s, e, Some(json!({"start": format!("0x{s:08X}"), "end": format!("0x{e:08X}"), "instructions": (e - s) / 4, "jal_callers": callers})))
                } else {
                    let start = match opt_hex(&addr, "address")? {
                        Some(a) => a,
                        None => entry.ok_or_else(|| MediaError::Malformed("give --addr or --function (there is no EXE entry point with --ram alone)".into()))?,
                    } & !3;
                    (start, start.wrapping_add(4 * count), None)
                };
                let (lines, listing) = crate::code::listing(&code, start, end, &crate::code::ListOpts { scopes: &scopes, text });
                let mut warnings = Vec::new();
                if let Some((checked, differ)) = code.exe_mismatch(start, end) {
                    if differ > 0 {
                        warnings.push(format!("RAM differs from the EXE in {differ} of {checked} words here: code patched or overlaid at run time (the listing shows RAM)"));
                    } else if checked == 0 {
                        warnings.push("this range is outside the EXE's load range: RAM only".to_string());
                    }
                }
                if lines.is_empty() {
                    warnings.push(format!("0x{start:08X} is outside the code ({}: 0x{:08X}..0x{:08X})", code.origin, code.base, code.end()));
                }
                let mut v = json!({"source": source.clone().unwrap_or_else(|| code.origin.clone()), "lines": lines});
                if ram.is_some() {
                    v["ram"] = json!(code.origin);
                }
                if let Some(f) = func {
                    v["function"] = f;
                }
                if !warnings.is_empty() {
                    v["warnings"] = json!(warnings);
                }
                if text {
                    let mut t = String::new();
                    if let Some(f) = v.get("function") {
                        t += &format!(
                            "; function {}..{} ({} instructions, {} jal callers) in {}\n",
                            f["start"].as_str().unwrap_or(""),
                            f["end"].as_str().unwrap_or(""),
                            f["instructions"],
                            f["jal_callers"],
                            code.origin
                        );
                    }
                    for w in &warnings {
                        t += &format!("; warning: {w}\n");
                    }
                    v["text"] = json!(t + &listing);
                }
                Ok(v)
            })(),
            text,
        )
    }
}

#[derive(clap::Args, Deserialize, JsonSchema)]
pub struct XrefsArgs {
    /// Target: a hex address, or a hex range LO-HI (HI exclusive) such as a whole struct.
    pub target: String,
    /// PS-X EXE file, or `image.cue::/PATH` (scans its load range). Optional with --ram.
    #[arg(long)]
    pub source: Option<String>,
    /// Scan a run's RAM (run directory or ram.bin) instead of the EXE: all 2 MiB.
    #[arg(long)]
    pub ram: Option<PathBuf>,
    /// gp value (hex) for gp-relative accesses (default: the EXE header's gp, when an EXE is given).
    #[arg(long)]
    pub gp: Option<String>,
    /// Skip data words equal to the target (pointer and jump tables).
    #[arg(long)]
    #[serde(default)]
    pub no_data: bool,
    /// `.sym` or `.symbols.json` files to name the referencing functions; repeatable.
    #[arg(long = "symbols")]
    #[serde(default)]
    pub symbols: Vec<PathBuf>,
    /// References listed individually (counts are always complete).
    #[arg(long, default_value_t = d_xref_limit())]
    #[serde(default = "d_xref_limit")]
    pub limit: usize,
    /// Plain-text listing instead of JSON.
    #[arg(long)]
    #[serde(default)]
    pub text: bool,
}

fn d_xref_limit() -> usize {
    200
}

impl Op for XrefsArgs {
    fn exec(self, _: &Ctx) -> Outcome {
        let XrefsArgs { target, source, ram, gp, no_data, symbols, limit, text } = self;
        Outcome::media_text(
            (|| {
                let mut code = crate::code::Code::open(source.as_deref(), ram.as_deref())?;
                if let Some(g) = opt_hex(&gp, "gp")? {
                    code.gp = Some(g);
                }
                let addr = |s: &str| hex_u32(s).ok_or_else(|| MediaError::Malformed(format!("bad hex address {s:?}")));
                let (lo, hi) = match target.split_once('-') {
                    Some((l, h)) => (addr(l)?, addr(h)?),
                    None => {
                        let a = addr(&target)?;
                        (a, a.wrapping_add(1))
                    }
                };
                if hi <= lo {
                    return Err(MediaError::Malformed(format!("empty range {target:?}")));
                }
                let scopes = crate::load_symbols(&symbols)?;
                let (mut v, t) = crate::code::xref_report(&code, lo, hi, !no_data, &scopes, limit, text);
                if text {
                    v["text"] = json!(t);
                }
                Ok(v)
            })(),
            text,
        )
    }
}

/// `psx ram diff`.
#[derive(clap::Args, Deserialize, JsonSchema)]
pub struct RamDiffArgs {
    /// First snapshot: a run directory or a ram.bin.
    pub a: PathBuf,
    /// Second snapshot.
    pub b: PathBuf,
    /// Unit width in bytes (aligned): 1, 2 or 4.
    #[arg(long, default_value_t = d_width4())]
    #[serde(default = "d_width4")]
    pub width: u8,
    /// Hex address range LO-HI (HI exclusive), e.g. 80100000-80200000.
    #[arg(long)]
    pub range: Option<String>,
    /// Only units whose (signed, wrapping) difference is at least this large.
    #[arg(long)]
    pub min_delta: Option<u64>,
    /// Show values as signed.
    #[arg(long)]
    #[serde(default)]
    pub signed: bool,
    /// Changes listed individually (counts are always complete).
    #[arg(long, default_value_t = d_ram_limit())]
    #[serde(default = "d_ram_limit")]
    pub limit: usize,
}

fn d_width4() -> u8 {
    4
}
fn d_width2() -> u8 {
    2
}
fn d_ram_limit() -> usize {
    100
}

impl Op for RamDiffArgs {
    fn exec(self, _: &Ctx) -> Outcome {
        Outcome::media(crate::ram::diff(
            &self.a,
            &self.b,
            crate::ram::DiffOpts { width: self.width, range: self.range.as_deref(), min_delta: self.min_delta, signed: self.signed, limit: self.limit },
        ))
    }
}

/// `psx ram search`.
#[derive(clap::Args, Deserialize, JsonSchema)]
pub struct RamSearchArgs {
    /// Snapshots in order: run directories or ram.bin files (e.g. runs with LEFT, idle, RIGHT held).
    #[arg(required = true)]
    pub snapshots: Vec<PathBuf>,
    /// Unit width in bytes: 1, 2 or 4.
    #[arg(long, default_value_t = d_width2())]
    #[serde(default = "d_width2")]
    pub width: u8,
    /// Compare values as signed (for increased/decreased, --values and --between).
    #[arg(long)]
    #[serde(default)]
    pub signed: bool,
    /// Predicate between consecutive snapshots: changed, unchanged, increased,
    /// decreased, delta=N or any. One applies to every pair; or give one per pair
    /// (comma-separated or repeated).
    #[arg(long = "pred")]
    #[serde(default, rename = "pred")]
    pub preds: Vec<String>,
    /// Exact value per snapshot, comma-separated, `*` for any: `441,*,376`.
    #[arg(long)]
    pub values: Option<String>,
    /// Every snapshot's value within LO..HI (inclusive).
    #[arg(long)]
    pub between: Option<String>,
    /// Hex address range LO-HI (HI exclusive).
    #[arg(long)]
    pub range: Option<String>,
    /// Address alignment in bytes (default: the width).
    #[arg(long)]
    pub align: Option<usize>,
    /// Only consider the addresses an earlier search saved with --save (narrowing session).
    #[arg(long)]
    pub candidates: Option<PathBuf>,
    /// Save the matching addresses as JSON for a later --candidates.
    #[arg(long)]
    pub save: Option<PathBuf>,
    /// Matches listed individually (the count is always complete).
    #[arg(long, default_value_t = d_ram_limit())]
    #[serde(default = "d_ram_limit")]
    pub limit: usize,
}

impl Op for RamSearchArgs {
    fn exec(self, _: &Ctx) -> Outcome {
        Outcome::media(crate::ram::search(
            &self.snapshots,
            crate::ram::SearchOpts {
                width: self.width,
                signed: self.signed,
                preds: &self.preds,
                values: self.values.as_deref(),
                between: self.between.as_deref(),
                range: self.range.as_deref(),
                align: self.align,
                candidates: self.candidates.as_deref(),
                save: self.save.as_deref(),
                limit: self.limit,
            },
        ))
    }
}

/// `psx ram view`.
#[derive(clap::Args, Deserialize, JsonSchema)]
pub struct RamViewArgs {
    /// One or more snapshots (run directories or ram.bin), shown side by side.
    #[arg(required = true)]
    pub snapshots: Vec<PathBuf>,
    /// Start address (hex).
    #[arg(long)]
    pub addr: String,
    /// Bytes to show (at most 64 KiB).
    #[arg(long, default_value_t = d_view_len())]
    #[serde(default = "d_view_len")]
    pub len: usize,
    /// Value format: u8 i8 x8 u16 i16 x16 u32 i32 x32 or fxI.F (e.g. fx16.16).
    #[arg(long = "as", default_value_t = d_view_fmt())]
    #[serde(default = "d_view_fmt", rename = "as")]
    pub fmt: String,
    /// Plain-text table (rows that differ between snapshots marked `*`).
    #[arg(long)]
    #[serde(default)]
    pub text: bool,
}

fn d_view_len() -> usize {
    64
}
fn d_view_fmt() -> String {
    "x32".into()
}

impl Op for RamViewArgs {
    fn exec(self, _: &Ctx) -> Outcome {
        let text = self.text;
        Outcome::media_text(
            crate::ram::view(&self.snapshots, &self.addr, self.len, &self.fmt).map(|(mut v, t)| {
                if text {
                    v["text"] = json!(t);
                }
                v
            }),
            text,
        )
    }
}

#[derive(clap::Args, Deserialize, JsonSchema)]
pub struct GpuReplayArgs {
    /// Recorded GP0 stream of `[n, words..]` u32 records.
    pub stream: PathBuf,
    /// Raw VRAM output (1024x512 little-endian halfwords).
    #[arg(long, short)]
    pub out: PathBuf,
    /// Also write the VRAM as a PNG.
    #[arg(long)]
    pub png: Option<PathBuf>,
}

impl Op for GpuReplayArgs {
    fn exec(self, _: &Ctx) -> Outcome {
        Outcome::media(crate::vram_diff::replay(&self.stream, &self.out, self.png.as_deref()))
    }
}

#[derive(clap::Args, Deserialize, JsonSchema)]
pub struct VramDiffArgs {
    /// First raw VRAM dump (vram.bin).
    pub a: PathBuf,
    /// Second raw VRAM dump.
    pub b: PathBuf,
    /// Cell size for per-block counts.
    #[arg(long, default_value_t = d_cell())]
    #[serde(default = "d_cell")]
    pub cell: usize,
    /// Region `X,Y,W,H` to compare (default: all of VRAM).
    #[arg(long)]
    pub region: Option<String>,
    /// Write a diff image (differing pixels red).
    #[arg(long)]
    pub png: Option<PathBuf>,
    /// Differing pixels listed individually.
    #[arg(long, default_value_t = d_samples())]
    #[serde(default = "d_samples")]
    pub samples: usize,
}

impl Op for VramDiffArgs {
    fn exec(self, _: &Ctx) -> Outcome {
        let VramDiffArgs { a, b, cell, region, png, samples } = self;
        Outcome::media((|| {
            let region = match region {
                Some(r) => {
                    let v: Vec<usize> = r.split(',').filter_map(|s| s.trim().parse().ok()).collect();
                    if v.len() != 4 {
                        return Err(MediaError::Malformed(format!("bad region {r}, expected X,Y,W,H")));
                    }
                    Some((v[0], v[1], v[2], v[3]))
                }
                None => None,
            };
            crate::vram_diff::diff(crate::vram_diff::DiffArgs { a: &a, b: &b, cell, region, png: png.as_deref(), samples })
        })())
    }
}

#[derive(clap::Args, Deserialize, JsonSchema)]
pub struct AddrArgs {
    /// Hex address, e.g. 0x80010000.
    pub address: String,
}

impl Op for AddrArgs {
    fn exec(self, _: &Ctx) -> Outcome {
        let address = self.address;
        Outcome::media(
            hex_u32(&address)
                .ok_or_else(|| MediaError::Malformed(format!("bad hex address {address}")))
                .map(|a| serde_json::to_value(memmap::resolve(a)).unwrap()),
        )
    }
}
