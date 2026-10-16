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
    /// Cooperative cancellation for supervised workers.
    pub cancel: Option<tokio_util::sync::CancellationToken>,
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
    pub(crate) fn with_path_context(mut self, ctx: &Ctx) -> Self {
        if self.value["error"]["code"] == "io_error" {
            self.value["error"]["project_root"] = json!(ctx.root);
            self.value["error"]["path_resolution"] = json!("Relative paths are resolved against project_root, not the client working directory.");
        }
        self
    }

    pub(crate) fn media(r: psx_media::Result<Value>) -> Self {
        match r {
            Ok(value) => Outcome { value, exit: 0, pretty: true, text: false },
            Err(e) => {
                let mut value = json!({"error": {"code": e.code(), "message": e.to_string()}});
                if let MediaError::Io { path, .. } = &e {
                    value["error"]["path"] = json!(std::path::absolute(path).unwrap_or_else(|_| path.clone()));
                }
                Outcome { value, exit: if matches!(e, MediaError::Unsupported(_)) { 2 } else { 1 }, pretty: false, text: false }
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
pub(crate) fn d_sigs() -> PathBuf {
    std::env::var_os("PSX_SIGS_DIR").filter(|p| !p.is_empty()).map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("external/psx_psyq_signatures"))
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
    /// Case-insensitive substring of the ISO path.
    #[arg(long)]
    pub filter: Option<String>,
    #[arg(long, default_value_t = 0)]
    #[serde(default)]
    pub offset: usize,
    #[arg(long, default_value_t=crate::page::default_limit())]
    #[serde(default = "crate::page::default_limit")]
    pub limit: usize,
}

impl Op for LsArgs {
    fn exec(self, _: &Ctx) -> Outcome {
        Outcome::media((|| {
            crate::page::validate(self.limit).map_err(MediaError::Malformed)?;
            let disc = Disc::open(&self.image)?;
            let fs = FileSystem::read(&disc)?;
            let filter = self.filter.unwrap_or_default().to_lowercase();
            let rows: Vec<_> = fs
                .entries
                .iter()
                .map(crate::entry_json)
                .filter(|e| e["path"].as_str().unwrap_or("").to_lowercase().contains(&filter))
                .collect();
            let entries: Vec<_> = rows.iter().skip(self.offset).take(self.limit).collect();
            Ok(json!({"volume": fs.volume, "entries": entries, "warnings": fs.warnings,
                    "page":crate::page::metadata(rows.len(),self.offset,entries.len(),self.limit),
                    "coverage":{"filesystem_entries":fs.entries.len(),"matching_entries":rows.len(),"scope":"ISO9660 directory index","complete":true}}))
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
    /// Executable normalizing each selected frame to a standard BS payload.
    #[arg(long)]
    pub frame_parser: Option<PathBuf>,
    #[arg(long = "frame-parser-arg", allow_hyphen_values = true, requires = "frame_parser")]
    #[serde(default)]
    pub frame_parser_args: Vec<String>,
    #[arg(long, default_value_t = default_parser_timeout(), value_parser = clap::value_parser!(u64).range(1..=300))]
    #[serde(default = "default_parser_timeout")]
    pub parser_timeout: u64,
    /// Output directory for frames, audio.wav, contact.png, frames.json, summary.json.
    #[arg(short, long)]
    pub out: PathBuf,
}

impl Op for StrArgs {
    fn exec(self, ctx: &Ctx) -> Outcome {
        Outcome::media(crate::str_cmd::split_source(&self.source, self.path.as_deref()).and_then(|(image, path)| {
            let frame_parser = crate::parser::Spec::new(self.frame_parser.as_ref(), &self.frame_parser_args, self.parser_timeout)?;
            crate::str_cmd::run(crate::str_cmd::Args {
                image: &image,
                path: &path,
                out: &self.out,
                frames: self.frames.as_deref(),
                channel: self.channel,
                no_audio: self.no_audio,
                y4m: self.y4m,
                frame_parser: frame_parser.as_ref(),
                cancel: ctx.cancel.as_ref(),
            })
        }))
    }
}

#[derive(clap::Args, Deserialize, JsonSchema)]
pub struct AssetParsersArgs {
    /// Executable returning a version-1 JSON model per file/archive entry.
    #[arg(long, conflicts_with = "asset_parser")]
    pub model_parser: Option<PathBuf>,
    #[arg(long = "model-parser-arg", allow_hyphen_values = true, requires = "model_parser")]
    #[serde(default)]
    pub model_parser_args: Vec<String>,
    /// Executable returning a version-1 JSON archive index; replaces built-in detection.
    #[arg(long, conflicts_with = "archive")]
    pub archive_parser: Option<PathBuf>,
    #[arg(long = "archive-parser-arg", allow_hyphen_values = true, requires = "archive_parser")]
    #[serde(default)]
    pub archive_parser_args: Vec<String>,
    /// Executable normalizing each source entry to a standard TIM/TMD/VAB/VAG payload.
    #[arg(long, conflicts_with = "model_parser")]
    pub asset_parser: Option<PathBuf>,
    #[arg(long = "asset-parser-arg", allow_hyphen_values = true, requires = "asset_parser")]
    #[serde(default)]
    pub asset_parser_args: Vec<String>,
    /// Wall-clock seconds per external invocation, 1..=300.
    #[arg(long, default_value_t = 30, value_parser = clap::value_parser!(u64).range(1..=300))]
    #[serde(default = "default_parser_timeout")]
    pub parser_timeout: u64,
}

fn default_parser_timeout() -> u64 {
    30
}

impl AssetParsersArgs {
    pub fn build(&self, ctx: &Ctx, archive: Option<&str>) -> psx_media::Result<crate::parser::Parsers> {
        if self.archive_parser.is_some() && archive.is_some() {
            return Err(MediaError::Malformed("--archive-parser cannot combine with --archive".into()));
        }
        if self.model_parser.is_some() && self.asset_parser.is_some() {
            return Err(MediaError::Malformed("choose --model-parser or --asset-parser".into()));
        }
        Ok(crate::parser::Parsers {
            archive: crate::parser::Spec::new(self.archive_parser.as_ref(), &self.archive_parser_args, self.parser_timeout)?,
            model: crate::parser::Spec::new(self.model_parser.as_ref(), &self.model_parser_args, self.parser_timeout)?,
            asset: crate::parser::Spec::new(self.asset_parser.as_ref(), &self.asset_parser_args, self.parser_timeout)?,
            cancel: ctx.cancel.clone(),
        })
    }
}

#[derive(clap::Args, Deserialize, JsonSchema)]
pub struct AssetsScanArgs {
    /// `image.cue::/PATH` (a file; `/DIR/` or `*` for many), a disc image (every
    /// file), a host file or a directory (recursive, sorted; no symlink traversal).
    /// `ram.bin` dumps resolve relocated TMDs at 0x80000000.
    pub source: String,
    /// Case-insensitive source-file name filter; affects scanning coverage.
    #[arg(long)]
    pub filter: Option<String>,
    /// Offset among files with recognized hits, uploads or archive indexes.
    #[arg(long, default_value_t = 0)]
    #[serde(default)]
    pub offset: usize,
    /// Maximum files and maximum rows within each file's hit/upload/archive lists (1..=1000).
    #[arg(long, default_value_t=crate::page::default_limit())]
    #[serde(default = "crate::page::default_limit")]
    pub limit: usize,
    /// Offset within each file's hits, upload streams and archive entries. Use filter to narrow to one file.
    #[arg(long, default_value_t = 0)]
    #[serde(default)]
    pub item_offset: usize,
    /// One JSON document instead of a text table (always JSON over MCP).
    #[arg(long)]
    #[serde(default)]
    pub json: bool,
    /// Only these kinds (tim, tmd, vab, vag); repeatable.
    #[arg(long = "kind")]
    #[serde(default)]
    pub kinds: Vec<String>,
    /// Address of the file's first byte when it is a RAM dump (hex).
    #[arg(long)]
    pub ram_base: Option<String>,
    /// Force DOT-T validation, or disable archive detection (default: automatic).
    #[arg(long, value_parser = ["dot-t", "none"])]
    pub archive: Option<String>,
    #[command(flatten)]
    #[serde(flatten)]
    pub parsers: AssetParsersArgs,
}

impl Op for AssetsScanArgs {
    fn exec(self, ctx: &Ctx) -> Outcome {
        match crate::assets_cmd::scan_assets(&self, ctx) {
            Ok(Err(table)) => Outcome { value: json!({"text": table}), exit: 0, pretty: true, text: true },
            Ok(Ok(v)) => {
                let mut out = Outcome::media(Ok(v));
                out.text = !self.json;
                out
            }
            Err(e) => Outcome::media(Err(e)),
        }
    }
}

#[derive(clap::Args, Deserialize, JsonSchema)]
pub struct AssetsExtractArgs {
    /// Same syntax as `assets scan`.
    pub source: String,
    /// Output directory.
    #[arg(short, long)]
    pub out: PathBuf,
    /// Return summary counts and a manifest read call, without per-asset results.
    #[arg(long)]
    #[serde(default)]
    pub brief: bool,
    /// Offset in returned conversion records; extraction still processes every selected asset.
    /// Continue by reading the saved manifest rather than repeating extraction.
    #[arg(long, default_value_t = 0)]
    #[serde(default)]
    pub report_offset: usize,
    /// Maximum returned records and nested output rows (1..=1000); full results go to the manifest.
    #[arg(long, default_value_t = crate::page::default_limit())]
    #[serde(default = "crate::page::default_limit")]
    pub limit: usize,
    /// Only these kinds (tim, tmd, vab, vag); repeatable.
    #[arg(long = "kind")]
    #[serde(default)]
    pub kinds: Vec<String>,
    /// Only the asset at this file offset (hex).
    #[arg(long)]
    pub offset: Option<String>,
    /// More TIM or explicitly parsed upload sources (same syntax as `source`) loaded into virtual VRAM
    /// that TMD texture pages are decoded from; repeatable.
    #[arg(long = "textures")]
    #[serde(default)]
    pub textures: Vec<String>,
    /// Executable decoding --textures inputs as standard payloads or raw VRAM upload documents.
    #[arg(long, requires = "textures")]
    pub texture_parser: Option<PathBuf>,
    #[arg(long = "texture-parser-arg", allow_hyphen_values = true, requires = "texture_parser")]
    #[serde(default)]
    pub texture_parser_args: Vec<String>,
    /// Address of the file's first byte when it is a RAM dump (hex).
    #[arg(long)]
    pub ram_base: Option<String>,
    /// Force DOT-T validation, or disable archive detection (default: automatic).
    #[arg(long, value_parser = ["dot-t", "none"])]
    pub archive: Option<String>,
    #[command(flatten)]
    #[serde(flatten)]
    pub parsers: AssetParsersArgs,
    /// Extract this zero-based archive entry as raw bytes, without conversion.
    #[arg(long, conflicts_with_all = ["offset", "kinds", "textures", "model_parser", "asset_parser", "texture_parser"])]
    pub entry: Option<usize>,
}

impl Op for AssetsExtractArgs {
    fn exec(self, ctx: &Ctx) -> Outcome {
        Outcome::media(crate::assets_cmd::extract_assets(&self, ctx))
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
        Outcome::media(crate::textures::run(
            &self.run_dir,
            self.page.as_deref(),
            self.clut.as_deref(),
            self.out.as_deref(),
            self.vram.as_deref(),
        ))
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
    /// The original game's video region (`REGION` in src/lib.rs): display rate
    /// and, with `fields_per_tick`, the logic tick rate. Default ntsc.
    #[arg(long, value_enum)]
    pub region: Option<crate::template::TemplateRegion>,
    /// Video fields per logic tick (`FIELDS_PER_TICK` in src/lib.rs): 1 when
    /// the original's main loop runs every vblank, 2 when it waits for two
    /// (Croc PAL: 25 Hz). Default 1.
    #[arg(long)]
    pub fields_per_tick: Option<u32>,
}

impl Op for TemplateCreateArgs {
    fn exec(self, _: &Ctx) -> Outcome {
        let options = crate::template::TemplateOptions {
            region: self.region.unwrap_or_default(),
            fields_per_tick: self.fields_per_tick.unwrap_or(1),
        };
        Outcome::media(crate::template::create(&self.dest, self.name.as_deref(), options))
    }
}

#[derive(clap::Args, Deserialize, JsonSchema)]
pub struct ParserTemplateArgs {
    /// Destination directory: new or empty.
    pub dest: PathBuf,
    /// Parser contract to scaffold.
    #[arg(long, value_enum)]
    pub kind: crate::parser_template::Kind,
}

impl Op for ParserTemplateArgs {
    fn exec(self, _: &Ctx) -> Outcome {
        Outcome::media(crate::parser_template::create(&self.dest, self.kind))
    }
}

#[derive(clap::Args, Deserialize, JsonSchema)]
pub struct RunCmdArgs {
    /// Disc image (.cue/.bin/.iso) or PS-X EXE; omit to boot the BIOS or continue a state.
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
    /// Native: log SPU voice key on/off (settings, sample hash, storing PC), each voice's
    /// first END block and sound RAM uploads to spu.jsonl; the report groups key ons by sample.
    #[arg(long)]
    #[serde(default)]
    pub trace_spu: bool,
    /// `.symbols.json` (verified against RAM) or `.sym` files used to name PCs; repeatable.
    #[arg(long = "symbols")]
    #[serde(default)]
    pub symbols: Vec<PathBuf>,
    /// Cap on recorded events per trace log.
    #[arg(long, default_value_t = d_max_events())]
    #[serde(default = "d_max_events")]
    pub max_events: u32,
    /// Emulator: `redux` (PCSX-Redux oracle worker; needs external/pcsx-redux) or
    /// `native` (default, the in-tree psx-core: exact watches, --trace, --gpu-frame, --sample, --poke,
    /// --snapshot-every, memory cards, native states).
    #[arg(long)]
    #[schemars(with = "Option<crate::context::Backend>")]
    pub backend: Option<String>,
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
    for k in ["build", "resolved", "input", "pad", "artifacts", "samples", "pokes", "screens", "states", "read_watch", "spu", "wall_ms"] {
        if !v[k].is_null() {
            b[k] = v[k].clone();
        }
    }
    if let Some(events) = v["input"]["events"].as_array() {
        b["input"]["events"] = json!(events.iter().take(8).collect::<Vec<_>>());
        b["input"]["page"] = crate::page::metadata(events.len(), 0, events.len().min(8), 8);
    }
    if v["gpu_frame"].is_object() {
        let g = &v["gpu_frame"];
        let mut summary = serde_json::Map::new();
        for key in
            ["error", "status", "reason", "mode", "search", "frame", "run_frame", "absolute_frame", "complete", "commands", "draw_commands", "draw_filter", "content"]
        {
            if let Some(value) = g.get(key) {
                summary.insert(key.into(), value.clone());
            }
        }
        b["gpu_frame"] = Value::Object(summary);
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
        let backend = match crate::context::backend_arg(a.backend) {
            Ok(b) => b,
            Err(e) => return crate::context::failed(e),
        };
        let resolved = match crate::context::resolve(&ctx.root, a.media, a.bios, backend, a.from_state.as_deref(), false) {
            Ok(r) => r,
            Err(e) => return crate::context::failed(e),
        };
        if let Some(e) = resolved.setup_error() {
            return crate::context::failed(e);
        }
        let bios = resolved.bios.unwrap();
        let short = a.brief;
        let mut out = Outcome::pair(crate::run::run(
            &ctx.root,
            crate::run::RunArgs {
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
                fast: a.fast,
                trace_cd: a.trace_cd,
                trace_bios: a.trace_bios,
                trace_spu: a.trace_spu,
                symbols: a.symbols,
                max_events: a.max_events,
                backend: resolved.backend.name().into(),
                trace: a.trace,
                cards: a.cards,
                observe: a.observe,
            },
        ));
        if out.value.get("error").is_none() {
            out.value["build"] = crate::context::build();
            out.value["resolved"] = json!({"backend":resolved.backend,"bios_selection":resolved.bios_reason});
            out.value["artifacts"] = json!(out.value["files"]
                .as_object()
                .map(|files| files
                    .values()
                    .filter_map(|f| f["path"].as_str())
                    .filter_map(|p| crate::artifact::descriptor(std::path::Path::new(p)).ok())
                    .collect::<Vec<_>>())
                .unwrap_or_default());
        }
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
    /// Include at most N individual runs (1..=1000); spans describe the full result.
    #[arg(long, default_value_t = crate::page::default_limit())]
    #[serde(default = "crate::page::default_limit")]
    pub limit: usize,
    #[arg(long, default_value_t = 0)]
    #[serde(default)]
    pub offset: usize,
    /// `.symbols.json` (verified against RAM) or `.sym` files from `psx symbols`; repeatable.
    #[arg(long = "symbols")]
    #[serde(default)]
    pub symbols: Vec<PathBuf>,
}

impl Op for LoadsArgs {
    fn exec(self, _: &Ctx) -> Outcome {
        Outcome::media((|| {
            crate::page::validate(self.limit).map_err(MediaError::Malformed)?;
            let scopes = crate::load_symbols(&self.symbols)?;
            let mut v = crate::provenance::report(&self.run_dir, &self.disc, &scopes).map_err(MediaError::Malformed)?;
            if let Some(runs) = v["runs"].as_array_mut() {
                let total = runs.len();
                let rows: Vec<_> = runs.iter().skip(self.offset).take(self.limit).cloned().collect();
                *runs = rows;
                v["page"] = crate::page::metadata(total, self.offset, v["runs"].as_array().unwrap().len(), self.limit);
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
    /// Local signature directory (`<version>/<LIB>.json` layout). Defaults to
    /// PSX_SIGS_DIR when set, otherwise external/psx_psyq_signatures in this project.
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
    pub bios: Option<PathBuf>,
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
    fn exec(self, ctx: &Ctx) -> Outcome {
        let resolved = match crate::context::resolve(&ctx.root,self.disc,self.bios,Some(crate::context::Backend::Native),None,false) {
            Ok(r)=>r,Err(e)=>return crate::context::failed(e),
        };
        if let Some(e) = resolved.setup_error() {return crate::context::failed(e);}
        let bios = resolved.bios.unwrap();
        let hex = |b: &Option<String>| b.as_deref().and_then(hex_u32);
        Outcome::pair(crate::native::run(crate::native::Options {
            bios: &bios,
            disc: resolved.media.as_deref(),
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
    /// Maximum instructions to list; defaults to 32, or the discovered function with --function.
    #[arg(long)]
    pub count: Option<u32>,
    /// Instruction budget for finding function boundaries, independent of --count (1..=262144).
    #[arg(long, default_value_t = d_function_budget())]
    #[serde(default = "d_function_budget")]
    pub function_budget: u32,
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

fn d_function_budget() -> u32 { 4096 }

fn opt_hex(s: &Option<String>, what: &str) -> psx_media::Result<Option<u32>> {
    s.as_deref().map(|a| hex_u32(a).ok_or_else(|| MediaError::Malformed(format!("bad {what} {a:?} (hex)")))).transpose()
}

impl Op for DisasmArgs {
    fn exec(self, _: &Ctx) -> Outcome {
        let DisasmArgs { source, addr, count, function_budget, symbols, text, function, ram, gp } = self;
        Outcome::media_text(
            (|| {
                if !(1..=262144).contains(&function_budget) || count.is_some_and(|n| !(1..=262144).contains(&n)) {
                    return Err(MediaError::Malformed("count/function_budget must be 1..=262144".into()));
                }
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
                    let extent = crate::code::function_extent(&code, f, &jal, function_budget)
                        .ok_or_else(|| MediaError::Malformed(format!("0x{f:08X} is outside the code ({})", code.origin)))?;
                    let (s,e) = (extent.start,extent.end);
                    let listed_end = count.map_or(e, |n| s.saturating_add(4*n).min(e));
                    let callers = crate::code::xrefs(&code, s, s + 4, false).iter().filter(|(_, r)| r.kind == "call").count();
                    (
                        s,
                        listed_end,
                        Some(json!({"start": format!("0x{s:08X}"), "end": format!("0x{e:08X}"), "instructions": (e-s)/4,
                            "jal_callers":callers,"boundary_source":"heuristic","start_evidence":extent.start_evidence,
                            "stop_reason":extent.stop_reason,"truncated":extent.truncated(),"search_budget":function_budget,
                            "listing_truncated":listed_end<e})),
                    )
                } else {
                    let start = match opt_hex(&addr, "address")? {
                        Some(a) => a,
                        None => entry.ok_or_else(|| {
                            MediaError::Malformed("give --addr or --function (there is no EXE entry point with --ram alone)".into())
                        })?,
                    } & !3;
                    (start, start.saturating_add(4 * count.unwrap_or(32)), None)
                };
                let (lines, listing) = crate::code::listing(&code, start, end, &crate::code::ListOpts { scopes: &scopes, text });
                let mut warnings = Vec::new();
                if func.as_ref().is_some_and(|f| f["truncated"] == true) {
                    warnings.push("Function boundary search is incomplete; increase --function-budget. Reported bounds are partial.".into());
                }
                if func.as_ref().is_some_and(|f| f["listing_truncated"] == true) {
                    warnings.push("Listing limited by --count; omit it to list the discovered function.".into());
                }
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
                let mut v = json!({"source": source.clone().unwrap_or_else(|| code.origin.clone()), "lines": lines,
                    "listing":{"start":format!("0x{start:08X}"),"end":format!("0x{end:08X}"),
                        "next_addr":func.as_ref().filter(|f| f["truncated"]==true || f["listing_truncated"]==true).map(|_|format!("0x{end:08X}"))}});
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
                    v.as_object_mut().unwrap().remove("lines");
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
    #[arg(long, default_value_t = 0)]
    #[serde(default)]
    pub offset: usize,
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
        let XrefsArgs { target, source, ram, gp, no_data, symbols, limit, offset, text } = self;
        Outcome::media_text(
            (|| {
                crate::page::validate(limit).map_err(MediaError::Malformed)?;
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
                let (mut v, t) = crate::code::xref_report(&code, lo, hi, !no_data, &scopes, offset, limit, text);
                if text {
                    v["text"] = json!(t);
                    v.as_object_mut().unwrap().remove("refs");
                    v.as_object_mut().unwrap().remove("functions");
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
    #[arg(long, default_value_t=0)]
    #[serde(default)]
    pub offset: usize,
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
            crate::ram::DiffOpts {
                width: self.width,
                range: self.range.as_deref(),
                min_delta: self.min_delta,
                signed: self.signed,
                limit: self.limit,
                offset: self.offset,
            },
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
    #[arg(long, default_value_t=0)]
    #[serde(default)]
    pub offset: usize,
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
                offset: self.offset,
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
