//! `psx mcp`: every CLI command as an MCP tool over stdin/stdout.
//!
//! Tools call the same [`Op`] implementations as the CLI, so a tool's
//! `structuredContent` is exactly the JSON document the CLI prints. stdout
//! carries only the protocol; diagnostics go to stderr.
//!
//! Differences from the CLI, all MCP-only:
//! - `return_screen: true` on `run`/`native` attaches the final screen as an
//!   image content block (`native` then picks a `dump` prefix if none was given).
//! - `native` and `lockstep` get a default `timeout` (seconds) when none is given.
//! - Every call has a hard wall-clock cap (its `timeout` + 60 s, or 15 minutes).
//!   A job that overruns it is reported as `timeout` and left to finish in the
//!   background, except supervised Ghidra workers, which are stopped.
//! - Results over 256 KiB are written to `.psx/mcp/` and replaced by a summary
//!   with the file path.
//! - Heavy tools (emulation, signature matching) share `--jobs` execution slots.

use std::collections::BTreeMap;
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

use base64::Engine;
use rmcp::model::{
    CallToolRequestParams, CallToolResponse, CallToolResult, ContentBlock, ErrorCode, Implementation, ListToolsResult,
    PaginatedRequestParams, ServerCapabilities, ServerConfig, Tool, ToolAnnotations,
};
use rmcp::service::{RequestContext, RoleServer};
use rmcp::{ErrorData as McpError, ServerHandler, ServiceExt};
use serde_json::{json, Map, Value};
use tokio::sync::Semaphore;

use crate::commands::{self, Ctx, Op, Outcome};
pub(crate) mod project;

#[derive(clap::Args)]
pub struct McpArgs {
    /// Concurrent heavy jobs (emulation, symbols, loads, Ghidra queries); 1..=8.
    #[arg(long, default_value_t = 2)]
    pub jobs: usize,
}

/// Inline result limit; larger results are spilled to a file.
const INLINE_LIMIT: usize = 256 * 1024;
/// Request argument limit.
const ARGS_LIMIT: usize = 1024 * 1024;
/// Hard cap for a call without a `timeout` argument.
const DEFAULT_CAP: Duration = Duration::from_secs(15 * 60);
/// Extra time past a tool's own `timeout` before the call is abandoned.
const GRACE: Duration = Duration::from_secs(60);
/// Default `timeout` for tools whose CLI default is unlimited.
const DEFAULT_TIMEOUT_S: u64 = 300;
/// Largest screen PNG returned inline.
const IMAGE_LIMIT: u64 = 4 * 1024 * 1024;

pub struct ToolDef {
    pub name: &'static str,
    pub description: &'static str,
    schema: fn() -> Value,
    call: fn(Value, &Ctx) -> Result<Outcome, String>,
    /// Takes an execution slot.
    heavy: bool,
    /// Only reads its inputs (writes nothing).
    read_only: bool,
    /// Gets [`DEFAULT_TIMEOUT_S`] when `timeout` is omitted.
    default_timeout: bool,
    /// Accepts `return_screen`.
    screen: bool,
}

fn call_op<A: Op>(args: Value, ctx: &Ctx) -> Result<Outcome, String> {
    let a: A = serde_json::from_value(args).map_err(|e| e.to_string())?;
    Ok(a.exec(ctx).with_path_context(ctx))
}

fn schema_of<A: Op>() -> Value {
    let mut s = serde_json::to_value(schemars::schema_for!(A)).unwrap();
    if let Some(o) = s.as_object_mut() {
        o.remove("title");
        o.remove("description");
        o.entry("properties").or_insert_with(|| json!({}));
    }
    portable_schema(&mut s);
    s["additionalProperties"] = json!(false);
    s
}

const fn def<A: Op>(name: &'static str, description: &'static str) -> ToolDef {
    ToolDef {
        name,
        description,
        schema: schema_of::<A>,
        call: call_op::<A>,
        heavy: false,
        read_only: false,
        default_timeout: false,
        screen: false,
    }
}

impl ToolDef {
    const fn heavy(mut self) -> Self {
        self.heavy = true;
        self
    }
    const fn read_only(mut self) -> Self {
        self.read_only = true;
        self
    }
    const fn default_timeout(mut self) -> Self {
        self.default_timeout = true;
        self
    }
    const fn screen(mut self) -> Self {
        self.screen = true;
        self
    }

    /// The input schema as published (with the MCP-only arguments).
    pub fn input_schema(&self) -> Value {
        let mut s = (self.schema)();
        s["properties"]["project_root"] = json!({"type":"string",
            "description":"MCP only: run this call in an existing project directory, with isolated path resolution, defaults and outputs. Absolute, or relative to the server's default root. Does not change other calls or the server default."});
        if self.screen {
            s["properties"]["return_screen"] = json!({
                "type": "boolean", "default": false,
                "description": "MCP only: also return the final screen PNG as image content (a few tens of KB).",
            });
        }
        if self.default_timeout {
            if let Some(t) = s["properties"].get_mut("timeout") {
                t["description"] = json!(format!(
                    "Wall-clock limit in seconds; the job stops with status \"timeout\". Default over MCP: {DEFAULT_TIMEOUT_S}."
                ));
            }
        }
        s
    }
}

/// Every tool. Descriptions are written for an agent doing PS1 reverse engineering.
pub fn tools() -> Vec<ToolDef> {
    use commands::*;
    vec![
        def::<crate::guide::GuideArgs>("guide", "Read built-in psx workflow guidance without a skill loader or filesystem access. Omit topic for an overview and topic navigation; choose setup, input, code, assets, bevy or troubleshooting for focused guidance. Explains project-root paths, checkpoint/input semantics, bounded evidence, code/asset investigation and rebuilding in Bevy. Read-only documentation; use context to inspect the actual installation and resolved defaults.").read_only(),
        def::<crate::context::ArgsContext>("context", "Discover the running build, workspace, saved project defaults, local BIOS candidates and backend capabilities. Optional image or from_state explains region/exact checkpoint identity. Discovery never downloads. save=true persists resolved BIOS/media/backend for later calls; ordinary run calls use the same defaults. Includes next-call examples for template creation and investigation."),
        def::<crate::artifact::ReadArgs>("artifact_read", "Inspect a returned artifact path or run directory without filesystem scripts. Directories list artifact descriptors. JSON supports RFC6901 pointer plus array offset/limit; JSONL/CSV/text stream within byte/row budgets with page.next_cursor. Filter JSONL/CSV by inclusive frame_from/frame_to, frame_field=frame|run_frame and hex pc; contains also filters text; columns projects record fields. Cursor binds source metadata and query; reuse filters and omit offset. Hex pages raw bytes; PNG returns dimensions and image content automatically. Totals remain null for partially scanned streams; coverage states exactly what was inspected. Does not write files.").read_only(),
        def::<crate::bios::ListArgs>("bios_list", "List BIOS images in this project's bios/ and the shared psx BIOS directory, with hashes, regional hints, recognized images and an explicit download catalog. Use context to see the selected BIOS and why. Does not download or install anything.").read_only(),
        def::<crate::bios::InstallArgs>("bios_install", "One-time BIOS setup: import from a local file, or explicitly set download=true and region=pal|ntsc-u|ntsc-j to fetch one pinned Internet Archive image. Downloaded size/SHA-256 must match before atomic installation. Default shared storage is PSX_BIOS_DIR or XDG_DATA_HOME/psx/bios; dir overrides it. Existing images are preserved. Runs discover installed compatible BIOSes without further setup.").heavy(),
        def::<crate::ghidra::AnnotateArgs>("ghidra_annotate", "Apply a JSON file of function renames/comments to an existing analysis. Patches use functions:[{addr,name,comment}]. User-name collisions fail explicitly; generated names yield to user names. Publishes a new revision and invalidates cached pseudocode while preserving the previous project.").heavy(),
        def::<crate::ghidra::ExportArgs>("ghidra_export_symbols", "Export function names from an analysis as a .symbols.json document bound to the copied input hash and all function body bytes. Feed the symbols path to run, loads, disasm or xrefs. Different code at the same address does not receive these names.").heavy(),
        def::<crate::ghidra::DoctorArgs>("ghidra_doctor", "Check optional Ghidra installation discovery and Java. Normally unnecessary before queries; ghidra_decompile automatically imports and analyses an EXE. Set PSX_GHIDRA_HOME or ghidra_home if discovery fails."),
        def::<crate::ghidra::Input>("ghidra_analyze", "Optionally prewarm a reusable Ghidra analysis from a PS-X EXE source (file or disc.cue::/PATH) or ram snapshot (requires ranges and entries; optional gp). Query tools analyse automatically; no setup call or custom script is required. Returns an analysis manifest handle and project path. Optional symbols seeds PsyQ/user names.").heavy(),
        def::<crate::ghidra::DecompileArgs>("ghidra_decompile", "Get pseudocode and callers for the function containing addr (hex, e.g. a watched writer PC). Supply source (PS-X EXE file or disc.cue::/PATH), ram (run directory or 2 MiB dump, with ranges and entries), or an analysis manifest from an earlier call. Automatically extracts, imports, analyses and caches on first use; no Ghidra GUI or scripts needed. Returns inline pseudocode, function body ranges, callers/callees, warnings, artifact and analysis paths. First call can take minutes; timeout defaults to 300 seconds. Requires a local Ghidra installation; inspect warnings for PS1 COP2/load-delay limitations. Same-analysis requests wait within timeout and support cancellation; foreign/GUI locks remain errors.").heavy(),
        def::<crate::ghidra::FunctionsArgs>("ghidra_functions", "List Ghidra's analysed functions from source (automatically imports/analyzes an EXE) or ram (with ranges and entries) or an analysis manifest. filter matches function names case-insensitively; offset/limit paginate. Returns entries, names, signatures and body ranges without decompiling every function.").heavy(),
        def::<crate::ghidra::XrefsArgs>("ghidra_xrefs", "Query Ghidra analysis references to/from addr (hex code or data address) using source, ram (with ranges and entries), or an analysis manifest. Automatically imports/analyzes on first use. offset/limit paginate each direction; references identify their type, call status and containing function. Unresolved indirect references are not a complete call graph.").heavy(),
        def::<DiscArgs>("disc", "First look at a PlayStation disc image (.cue preferred; raw .bin or .iso also work): tracks, volume descriptor, file/dir counts, SYSTEM.CNF (BOOT line, stack, TCB/EVENT), boot executable header (entry pc, gp, load range, region marker) and LibCrypt subchannel status. LibCrypt-protected discs need the redump .sbi/.lsd next to the .cue (same base name); `subchannel.source` reports whether it was found and the native core honours it. hash=true streams SHA-256 over the image files (slow for full CDs).").read_only(),
        def::<LsArgs>("ls", "List ISO9660 entries with filter/offset/limit (default 100): path, LBA, size, sector count, directory flag, CD-XA attributes and `streamed` (Form 2 / interleaved XA or STR data that is read raw, not via the file system). Use it to map a disc's layout before extract/sector/xa, or to identify which file an LBA belongs to. page reports total/returned/truncated/next_offset; coverage describes the full filesystem index.").read_only(),
        def::<ExtractArgs>("extract", "Extract one file (`path`, e.g. `/DATA/LEVEL1.BIN` or `cdrom:\\SLUS_000.79;1`) or every file (`all: true`) into `out` (default ./extracted). Form 1 files are written as user data; Form 2 / interleaved files (XA audio, STR video) are written as raw 2352-byte sectors with a `.raw` suffix. Returns per-file destination, size and SHA-256."),
        def::<ExeArgs>("exe", "Parse a PS-X EXE header: entry pc, gp, text/data/bss ranges, initial stack, region marker string and hashes. `source` is a file path or `image.cue::/PATH` to read an executable straight from a disc (e.g. the BOOT path from `disc`). Use the load range to know where code lives in RAM (KSEG0 0x80000000+).").read_only(),
        def::<SectorArgs>("sector", "Describe raw disc sectors starting at `lba` (up to 256 with `count`): track, owning ISO9660 file, mode/form, XA subheader (file, channel, submode, coding) and a hex preview of the user data. Use for streamed data that the file system cannot read, or to check what a CD-ROM trace (`loads`) pointed at.").read_only(),
        def::<AddrArgs>("addr", "Classify a CPU address (hex, e.g. 0x80010000 or 1F801810): KUSEG/KSEG0/KSEG1/KSEG2 segment, physical address, region (main RAM with its mirrors folded, scratchpad, I/O, BIOS, expansion areas, cache control) and the offset into that region. Cheap; use it whenever an address from a trace or watch needs context.").read_only(),
        def::<XaArgs>("xa", "List the XA-ADPCM audio channels interleaved in a streamed file (STR movies, .XA music banks): per file/channel first LBA, sector count, duration, sample rate, bit depth and stereo flag. With `channel` and `out` it decodes that channel to a 44.1 kHz WAV (`file` also filters on the subheader file number)."),
        def::<StrArgs>("str", "Decode a PlayStation FMV (STR/MDEC bitstream v1/v2/v3, incl. FF7-style prefixed frames) from a disc file into `out`: frame_NNNNN.png (cropped to the movie size), audio.wav (interleaved XA, same as `xa`), contact.png, frames.json and summary.json (dimensions, frame count, fps estimate, BS versions, XA channels). `source` is `image.cue::/PATH`. `frames` limits the PNGs (A..B, 1-based). Use frame_parser plus frame_parser_args for a custom per-frame wrapper returning a version-1 base64 BS payload. parser_timeout bounds each invocation. Other unsupported variants are refused with the variant named.").heavy(),
        def::<AssetsScanArgs>("assets_scan", "Find PlayStation TIM/TMD/VAB/VAG assets by signature + validation in disc files or host files/RAM dumps. source accepts image.cue::/PATH, a directory ending in /, a whole disc image, or a host path. json=true returns paginated structured hits; otherwise compact text plus summary/coverage. filter matches source names; offset/limit page files, item_offset/limit page nested hits/upload streams/archive entries. Totals cover the selected scan scope, not just the page. No hit does not prove custom assets are absent. Automatically recognizes validated DOT-T sector archives and Croc sibling .DIR indexes. files[].archive lists every zero-based entry, including empty entries and archives without recognized assets. Hits retain parent-file offset plus entry index, format, archive_offset and relative offset. Scanning is bounded to each entry. archive=dot-t forces validation, archive=none disables indexing. ram_base overrides RAM address mapping; kinds filters tim/tmd/vab/vag. Use archive_parser for a version-1 JSON index, model_parser for JSON geometry, or asset_parser for a base64 standard payload or ordered raw VRAM upload document; *_parser_args are literal argument arrays, parser_timeout bounds each invocation. Generate working starters with template_parser_create.").heavy(),
        def::<AssetsExtractArgs>("assets_extract", "Convert assets found by assets_scan into out. brief=true returns counts only; otherwise limit/report_offset bound returned records and nested output lists. The full content-bound manifest and next_call support artifact_read navigation without rerunning extraction. TIM → PNG, TMD → glTF (+ texture-page PNGs from this file or textures sources), VAB/VAG → WAV and programs JSON; retain original bytes and entry provenance. offset (hex) selects an asset by parent-file offset. Alternatively, entry (zero-based integer) extracts one complete raw archive entry, including padding or an empty entry, without interpreting its payload; requires one source file and cannot combine with offset, kinds or textures. DOT-T and Croc .DIR indexes are automatic; archive=dot-t forces validated parsing and archive=none disables indexing. External archive_parser, model_parser and asset_parser hooks accept version-1 JSON; *_parser_args are literal arrays, parser_timeout bounds each invocation. Model and wrapper conversions retain original bytes, parser JSON and hashes. An asset parser may return version-1 uploads with source spans, VRAM rectangles and base64 words; scan reports upload_streams separately from standard hits. texture_parser plus texture_parser_args adapts textures sources before model page/CLUT decoding (requires textures). Raw uploads retain their words and manifests. Generate starters with template_parser_create; see docs/external-parsers.md.").heavy(),
        def::<MemcardArgs>("memcard", "List the saves on a raw 128 KiB memory card image (.mcd/.mcr, e.g. one written by `run` with memcard1/memcard2): product code, blocks used, Shift-JIS title and icon frames, plus free blocks.").read_only(),
        def::<TexturesArgs>("textures", "Decode VRAM texture pages through their CLUTs into PNGs. Given a native run directory captured with `gpu_frame`, it decodes every texture page/CLUT pair that frame's primitives used (full page and cropped to the UV bounds actually sampled) and writes textures/scene.psxscene for the psx-bevy template. With `page` (X,Y,DEPTH in VRAM halfword units, 4/8/15 bpp) and `clut` (X,Y) it decodes one page by hand from any run's vram.bin. Frame mode decodes gpu_frame_vram.bin (VRAM at the end of the captured frame; games that stream sprite frames into VRAM every tick need it) when present, else vram.bin; `vram` overrides. Mirrored quads become flipped sprites with the PS1's texel offset."),
        def::<RunCmdArgs>("run", "Run a disc/EXE, BIOS boot or checkpoint for bounded frames. Native is the default investigation backend; redux is the separately installed reference oracle. context explains BIOS/media defaults; from_state resolves exact native identity. Never downloads. Returns run_dir, report and artifact descriptors for RAM, VRAM, PNGs and optional states; use artifact_read to inspect them. brief=true summarizes; return_screen=true attaches the final PNG even with brief.\n\nInput: input is an inline array of complete pad states, e.g. [{\"frame\":0,\"buttons\":[\"CROSS\"],\"until\":5}]. until releases at the exclusive boundary; buttons=[] releases all; omitted until holds until the next state. Strictly increasing frames in 0..=frames, no overlapping until intervals. replay accepts the same array from a file; cannot combine with input. Noncentral sticks [lx,ly,rx,ry] require native + dualshock (checkpoint pad settings are restored). Events, pokes, GPU selection and trace windows count from this run's start; frame in logs is absolute, run_frame is relative.\n\nNative: sample records per-frame values to samples.csv; watch records stores with writer PC to writes.jsonl; watch_reads records loads to reads.jsonl; poke tests memory changes; trace records bounded instruction evidence. gpu_frame selects one frame; gpu_next_draw searches for drawing within gpu_search_frames and the existing frames budget. gpu_draw_filter=primitives skips fill-only frames; textured requires textured polygons/rectangles. gpu_frame.content classifies empty/fill/primitive content separately from capture completion. Native input.application and input.jsonl record actual scheduler applications, not proof of game polling. snapshot_every and save_state_at capture intermediate evidence. memcard1/2 accept a path or new and preserve originals. States require the same BIOS/media/backend. PCSX-Redux fast uses its dynarec without observation.\n\nThe full report is always saved. status=partial with termination=timeout means the wall-clock budget was reached; inspect frames.reached. Watch metadata distinguishes disabled, empty and capped observations.").heavy().screen(),
        def::<NativeArgs>("native", "Run the native psx-core alone from power-on for `frames` frames and report where it got: final PC/instruction, CD-ROM command history, SPU state, unhandled I/O reads, a profile of the last two frames (hot PCs, polled I/O), and speed. Diagnostic tool for the emulator itself and quick boot checks; for game RE use `run` with backend native, which also has snapshot_every, save states, samples and pokes. `break` (hex PC) stops there with an instruction tail and registers; `watch` (hex RAM word) lists the first 32 changes with the writing instruction (from `watch_after`); `dump` writes <prefix>.screen.png/.vram.png; `wav` captures SPU audio; `pad` digital|dualshock; `replay` frames count from power-on. Optional `disc`; without it the BIOS shell boots. Pass return_screen=true to get the final screen as an image.").heavy().default_timeout().screen(),
        def::<LoadsArgs>("loads", "Turn a `run` recorded with trace_cd=true into disc → RAM provenance: which ISO9660 file and offset each CD DMA transfer copied to which RAM range, merged into spans and runs and verified against the run's final ram.bin. Answers 'which file is this RAM code/data from'. offset/limit page the individual runs (default 100); page exposes totals and continuation. Spans describe the full result. Pass `symbols` to name the PCs that issued the reads.").heavy(),
        def::<SymbolsArgs>("symbols", "Name PsyQ library functions in a PS-X EXE (file or `image.cue::/PATH`) by signature matching; needs a local signature checkout (default external/psx_psyq_signatures). Writes <out>.symbols.json (feed it to run/loads/disasm `symbols`; names are checked against RAM residency), <out>.ghidra.txt for ImportSymbolsScript.py and <out>.sym. Returns counts by library, chosen signature-set SDK version and coverage. Coverage is library-code scope, not all game functions; low coverage alone is not failure. Ambiguous matches are skipped. Use Ghidra/runtime evidence for game-specific code.").heavy(),
        def::<DisasmArgs>("disasm", "Disassemble MIPS R3000 code at `addr` (hex, default entry pc; count defaults to 32), or the function containing `function` (hex). Function bounds are heuristic: jal targets, stack prologues, returns and tail jumps. function_budget bounds the search (default 4096 instructions); function.truncated, start_evidence, stop_reason and warnings expose incomplete discovery. Function mode emits the discovered extent unless count caps the listing; listing.next_addr supports continuation. Code comes from a PS-X EXE (`source`: file or `image.cue::/PATH`) or a run's RAM (`ram`: run_dir or ram.bin; use for overlays). With both, warnings report RAM/EXE differences. Lines carry refs resolved from lui+addiu/ori/load/store pairs and gp-relative accesses (`gp` hex, default EXE gp). text=true returns a compact assembler listing with labels, resolved addresses and warnings. Labels from symbols (.sym or .symbols.json).").read_only(),
        def::<XrefsArgs>("xrefs", "Find every reference to `target` (hex address, or LO-HI hex range for a whole struct, HI exclusive) in an EXE (`source`) or a run's RAM (`ram`): jal calls, j jumps, branches, addresses built by lui+addiu/ori and lui+load/store pairs, gp-relative loads/stores (`gp` hex, default the EXE's gp; give it for RAM-only scans), and aligned data words equal to the address (pointer and jump tables; `no_data` to skip). Each ref has the referencing pc, kind, how it was formed and the enclosing function start; `functions` groups them. Answers 'who calls this' and 'which code touches this variable' statically; pair with run `watch` for runtime writers. text=true for a compact listing.").read_only(),
        def::<RamDiffArgs>("ram_diff", "Compare two RAM snapshots (`a`, `b`: run directories or ram.bin) unit by unit (`width` 1/2/4 bytes, aligned; `range` LO-HI hex): count of differing units, the first `limit` with both values, hex and signed delta, and clusters of nearby changes (objects that changed). `min_delta` drops small changes; `signed` shows signed values. Use after two runs that differ in one input to find candidate variables.").read_only(),
        def::<RamSearchArgs>("ram_search", "Cheat-finder search over ordered RAM snapshots (`snapshots`: run directories or ram.bin, e.g. runs from the same state with LEFT, nothing, RIGHT held): keeps addresses whose values satisfy `pred` between consecutive snapshots (changed, unchanged, increased, decreased, delta=N, any; one for all pairs or one per pair), exact `values` per snapshot (\"441,*,376\"), and/or `between` LO..HI. `width` 1/2/4 (default 2), `signed` for signed comparison, `range` LO-HI hex. `save` writes the matches to a JSON file and `candidates` restricts a later search to them, so a search can be narrowed over several rounds. Matches show values per snapshot (and fx16.16 for width 4).").read_only(),
        def::<RamViewArgs>("ram_view", "Show a RAM window (`addr` hex, `len` bytes) from one or more snapshots side by side, decoded `as` u8/i8/x8/u16/i16/x16/u32/i32/x32/fxI.F (fx16.16, fx4.12), marking units that differ between snapshots. Use it to read a struct (an object's fields across runs). text=true gives a table in `text`.").read_only(),
        def::<LockstepArgs>("lockstep", "Emulator validation: run the native core and PCSX-Redux side by side from BIOS reset and stop at the first instruction where they disagree, with recent history. Default mode streams Redux's trace (fast, compares PC, instruction and source operands); `exact` single-steps over GDB comparing all registers (~20 steps/s, keep `steps` small). Needs external/pcsx-redux. Not needed for game RE.").heavy().default_timeout(),
        def::<ParserTemplateArgs>("template_parser_create", "Create an executable Python 3 external parser starter, authored sample.bin and protocol documentation in dest (new or empty). kind is archive, model, asset or video. Use with assets_scan/assets_extract or str; no toolkit rebuild required."),
        def::<TemplateCreateArgs>("template_create", "Create a standalone Rust/Bevy game project with the PS1 look (vertex snapping, affine textures, dithering, low-res view, DualShock input, scene loading from `textures` output) in `dest` (new or empty directory). Pass an absolute path or one relative to the project root. `region` (ntsc/pal) and `fields_per_tick` set REGION and FIELDS_PER_TICK in src/lib.rs, i.e. the display and logic tick rate (Croc PAL: pal + 2 = 25 Hz; Mega Man 8 PAL: pal + 1 = 50 Hz); take them from the original's region and how many vblanks its main loop waits."),
        def::<GpuReplayArgs>("gpu_replay", "Replay a recorded GP0 command stream (u32 records `[n, words..]`, e.g. tools/probes/gpu/gp0_stream.bin) into a fresh software GPU and write the resulting VRAM to `out` (raw 1024x512 halfwords) and optionally `png`. Pair with vram_diff to compare against a VRAM dump from a run."),
        def::<VramDiffArgs>("vram_diff", "Compare two raw VRAM dumps (vram.bin from runs or gpu_replay) pixel by pixel: differing pixel count, per-cell counts, channel deltas, sample pixels, optional diff PNG (differing pixels red). `region` X,Y,W,H restricts the comparison (e.g. the display area)."),
    ]
}

/// Strip non-standard integer `format`s, turning them into explicit bounds
/// (adapted from mdx): some clients reject `uint32` and friends.
fn portable_schema(value: &mut Value) {
    match value {
        Value::Object(object) => {
            let bounds = match object.get("format").and_then(Value::as_str) {
                Some("uint" | "uint64") => Some((json!(0), None)),
                Some("uint8") => Some((json!(0), Some(json!(u8::MAX)))),
                Some("uint16") => Some((json!(0), Some(json!(u16::MAX)))),
                Some("uint32") => Some((json!(0), Some(json!(u32::MAX)))),
                Some("int" | "int64" | "int32" | "int16" | "int8") => Some((Value::Null, None)),
                Some("float" | "double") => Some((Value::Null, None)),
                _ => None,
            };
            if let Some((min, max)) = bounds {
                object.remove("format");
                if !min.is_null() {
                    object.entry("minimum").or_insert(min);
                }
                if let Some(max) = max {
                    object.entry("maximum").or_insert(max);
                }
            }
            for child in object.values_mut() {
                portable_schema(child);
            }
        }
        Value::Array(values) => values.iter_mut().for_each(portable_schema),
        _ => {}
    }
}

fn error_value(code: &str, message: impl std::fmt::Display) -> Value {
    json!({"error": {"code": code, "message": message.to_string()}})
}

fn stamp() -> String {
    let ms = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis()).unwrap_or(0);
    format!("{ms}-{}{}", std::process::id(), crate::instance_suffix())
}

/// Validate and complete a call's arguments. Returns the arguments for the
/// command and whether a screen image was requested.
fn prepare(tool: &ToolDef, args: Map<String, Value>, ctx: &Ctx) -> Result<(Value, bool), Value> {
    let mut args = args;
    let screen = match args.remove("return_screen") {
        None => false,
        Some(Value::Bool(b)) if tool.screen => b,
        Some(_) if tool.screen => return Err(error_value("invalid_arguments", "return_screen must be a boolean")),
        Some(_) => return Err(error_value("invalid_arguments", format!("{} does not take return_screen", tool.name))),
    };
    let schema = (tool.schema)();
    let known: Vec<&String> = schema["properties"].as_object().map(|p| p.keys().collect()).unwrap_or_default();
    if let Some(k) = args.keys().find(|k| !known.contains(k)) {
        let names: Vec<&str> = known.iter().map(|s| s.as_str()).collect();
        return Err(error_value(
            "invalid_arguments",
            format!("unknown argument {k:?} for {}; expected one of {}", tool.name, names.join(", ")),
        ));
    }
    if tool.default_timeout && args.get("timeout").is_none_or(Value::is_null) {
        args.insert("timeout".into(), json!(DEFAULT_TIMEOUT_S));
    }
    if tool.name == "native" && screen && args.get("dump").is_none_or(Value::is_null) {
        let dir = ctx.root.join(".psx/native").join(stamp());
        args.insert("dump".into(), json!(dir.join("native")));
    }
    Ok((Value::Object(args), screen))
}

/// Path of the final screen PNG in a `run` or `native` result.
fn screen_path(tool: &str, v: &Value) -> Option<String> {
    let p = match tool {
        "run" => {
            if let Some(path) = v["files"]["screen.png"]["path"].as_str() { return Some(path.into()); }
            // Compact reports retain the run directory, but list artifact names.
            // Only attach a screenshot when the report says it was written.
            let listed = v["files"].as_array()?.iter().any(|f| f == "screen.png");
            let dir = v["run_dir"].as_str()?;
            return listed.then(|| std::path::Path::new(dir).join("screen.png").to_string_lossy().into_owned());
        },
        "native" => &v["dump"]["screen"],
        "artifact_read" => &v["image"]["path"],
        _ => return None,
    };
    p.as_str().map(str::to_string)
}

fn spill(tool: &str, value: Value, ctx: &Ctx) -> Value {
    let text = serde_json::to_string_pretty(&value).unwrap();
    if text.len() <= INLINE_LIMIT {
        return value;
    }
    let dir = ctx.root.join(".psx/mcp");
    let path = dir.join(format!("{tool}-{}.json", stamp()));
    if let Err(e) = std::fs::create_dir_all(&dir).and_then(|_| std::fs::write(&path, &text)) {
        return error_value("io_error", format!("result of {} bytes could not be written to {}: {e}", text.len(), path.display()));
    }
    // Keep small top-level values inline; describe the rest.
    let mut summary = Map::new();
    if let Value::Object(o) = &value {
        for (k, v) in o {
            let small = serde_json::to_string(v).map(|s| s.len() <= 512).unwrap_or(false);
            summary.insert(
                k.clone(),
                if small {
                    v.clone()
                } else {
                    match v {
                        Value::Array(a) => json!({"omitted": "array", "len": a.len()}),
                        Value::Object(m) => json!({"omitted": "object", "keys": m.len()}),
                        _ => json!({"omitted": "value"}),
                    }
                },
            );
        }
    }
    json!({
        "result_file": path,
        "result_bytes": text.len(),
        "note": "The full structured result exceeded the MCP inline limit and was written to result_file as pretty JSON. Use artifact_read with a JSON pointer and pagination. Large fields are summarised below.",
        "summary": summary,
        "artifact": crate::artifact::descriptor(&path).ok(),
        "next_call": {"tool":"artifact_read","arguments":{"path":path,"project_root":ctx.root}},
    })
}

#[derive(Clone)]
struct PsxServer {
    root: std::path::PathBuf,
    tools: Arc<Vec<Tool>>,
    defs: Arc<BTreeMap<&'static str, ToolDef>>,
    slots: Arc<Semaphore>,
}

/// `image`: `None` when no screen was requested, `Some(None)` when it was but
/// there is none (error, display off, PNG too large).
fn respond(value: Value, image: Option<Option<Vec<u8>>>) -> CallToolResponse {
    let failed = value.get("error").is_some();
    let mut r = if failed { CallToolResult::structured_error(value) } else { CallToolResult::structured(value) };
    match image {
        Some(Some(png)) => r.content.push(ContentBlock::image(base64::engine::general_purpose::STANDARD.encode(png), "image/png")),
        Some(None) if !failed => r.content.push(ContentBlock::text(
            "return_screen: no screen image (the display was off or 0x0 at the end of the run, or the PNG exceeds 4 MiB); vram.png in the run directory still shows VRAM.",
        )),
        _ => {}
    }
    r.into()
}

impl PsxServer {
    fn new(root: std::path::PathBuf, jobs: usize) -> Self {
        let mut tools = Vec::new();
        let mut defs = BTreeMap::new();
        for d in self::tools() {
            let schema = d.input_schema();
            let tool = Tool::new(d.name, d.description, Arc::new(schema.as_object().unwrap().clone()))
                .with_annotations(ToolAnnotations::from_raw(None, Some(d.read_only), Some(false), None, Some(false)));
            tools.push(tool);
            defs.insert(d.name, d);
        }
        PsxServer { root, tools: Arc::new(tools), defs: Arc::new(defs), slots: Arc::new(Semaphore::new(jobs)) }
    }

    async fn call(&self, name: &str, mut args: Map<String, Value>, cancel: tokio_util::sync::CancellationToken) -> CallToolResponse {
        let tool = &self.defs[name];
        if serde_json::to_vec(&args).map(|v| v.len()).unwrap_or(0) > ARGS_LIMIT {
            return respond(error_value("limit_exceeded", "tool arguments exceed 1 MiB"), None);
        }
        let scoped = args.remove("project_root");
        let root = match project::resolve(&self.root, scoped.as_ref()) {
            Ok(root) => root,
            Err(error) => return respond(error, None),
        };
        let worker_cancel = cancel.child_token();
        let ctx = Ctx { root, cancel: Some(worker_cancel.clone()) };
        let (args, want_screen) = match prepare(tool, args, &ctx) {
            Ok(r) => r,
            Err(e) => return respond(e, None),
        };
        let cap = args.get("timeout").and_then(Value::as_u64).map(|s| Duration::from_secs(s) + GRACE).unwrap_or(DEFAULT_CAP);
        let queued_at=std::time::Instant::now();
        let permit = if tool.heavy {
            tokio::select! {
                p = tokio::time::timeout(cap,self.slots.clone().acquire_owned()) => match p {
                    Ok(Ok(p)) => Some(p),
                    Ok(Err(_)) => return respond(error_value("internal", "execution queue closed"), None),
                    Err(_) => return respond(error_value("timeout","Deadline reached while waiting for an execution slot"),None),
                },
                _ = cancel.cancelled() => return respond(error_value("cancelled", "cancelled while waiting for an execution slot"), None),
            }
        } else {
            None
        };
        let call = tool.call;
        let tool_name = tool.name;
        let supervised = scoped.is_some() || name.starts_with("ghidra_") || name == "bios_install"
            || ["archive_parser", "model_parser", "asset_parser", "texture_parser", "frame_parser"]
                .iter()
                .any(|key| args.get(*key).is_some_and(Value::is_string));
        let mut worker = tokio::task::spawn_blocking(move || {
            let _permit = permit;
            let out = if scoped.is_some() {
                project::call(tool_name, args, &ctx).unwrap_or_else(|e| error_value("worker_failed", e))
            } else {
                match call(args, &ctx) {
                    Ok(o) => o.value,
                    Err(e) => error_value("invalid_arguments", e),
                }
            };
            let attach = want_screen || (tool_name == "artifact_read" && out["image"]["path"].is_string());
            let shot = attach.then(|| {
                screen_path(tool_name, &out).and_then(|p| {
                    let path = ctx.root.join(p);
                    let ok = std::fs::metadata(&path).is_ok_and(|m| m.len() <= IMAGE_LIMIT);
                    ok.then(|| std::fs::read(&path).ok()).flatten()
                })
            });
            (spill(tool_name, out, &ctx), shot)
        });
        // Also stop a supervised child if the request future itself is dropped.
        struct CancelOnDrop(tokio_util::sync::CancellationToken);
        impl Drop for CancelOnDrop {
            fn drop(&mut self) {
                self.0.cancel();
            }
        }
        let _cancel_on_drop = CancelOnDrop(worker_cancel.clone());
        tokio::select! {
            r = tokio::time::timeout(cap.saturating_sub(queued_at.elapsed()), &mut worker) => match r {
                Ok(Ok((value, shot))) => respond(value, shot),
                Ok(Err(e)) => respond(error_value("internal", format!("{name} failed: {e}")), None),
                Err(_) => {
                    worker_cancel.cancel();
                    if supervised { let _ = tokio::time::timeout(Duration::from_secs(3), &mut worker).await; }
                    respond(error_value("timeout", if supervised { format!("{name} exceeded {} s; worker termination requested", cap.as_secs()) } else { format!("{name} exceeded {} s; it keeps running in the background and may still write its outputs", cap.as_secs()) }), None)
                },
            },
            _ = cancel.cancelled() => {
                worker_cancel.cancel();
                if supervised { let _ = tokio::time::timeout(Duration::from_secs(3), &mut worker).await; }
                respond(error_value("cancelled", if supervised { format!("{name} was cancelled; worker termination requested") } else { format!("{name} was cancelled; the job keeps running in the background until its own timeout") }), None)
            },
        }
    }
}

impl ServerHandler for PsxServer {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new("psx", crate::context::version()))
            .with_instructions(format!(
                "PlayStation (PS1) reverse-engineering toolkit. Each tool is one `psx` CLI command and returns exactly the JSON the CLI prints. \
                 Default project root: {root} (bios/, media/, external/ and .psx/ outputs live here). Relative paths resolve against it; absolute paths also work. Every tool accepts project_root for one isolated call; use it consistently for sibling game studies. It never changes the server default. \
                 Use guide for workflow instructions and topic-specific recipes without a skill loader. Use artifact_read for returned artifact paths; run accepts typed inline input with explicit until releases. Same-analysis Ghidra calls queue within deadlines. Start with context to discover the exact build, configured BIOS/media, workspace and capabilities; bios_list and explicit bios_install handle one-time setup. run defaults to native and resolves installed BIOSes; no automatic downloads. template_create creates the Bevy project. \
                 Typical flow: disc -> ls / exe (`image.cue::/PATH`) -> symbols -> run (backend native; save_state then from_state; trace_cd, watch, trace, gpu_frame) -> loads / textures / disasm. \
                 Gameplay RE: short runs from a state with different inputs -> ram_search / ram_diff to find variables -> run `sample` for per-frame values and `watch` for writers -> disasm (text, function) / xrefs -> run `poke` to test. \
                 For pseudocode: ghidra_decompile with source (EXE or disc.cue::/PATH) and addr (writer PC); import/analysis/cache are automatic. Use its analysis handle for ghidra_functions / ghidra_xrefs or more decompiles. For runtime overlays use ram with ranges and entries. Rename/comment via ghidra_annotate, then ghidra_export_symbols to return byte-bound names to native traces. No separate Ghidra setup call is needed. \
                 Emulation outputs (RAM, VRAM, traces, PNGs) are files in the returned run_dir; read them from disk instead of expecting them inline. \
                 Backend native gives exact observations and native states; redux is the PCSX-Redux oracle. Results over 256 KiB come back as a result_file path.",
                root = self.root.display()
            ))
    }

    fn get_tool(&self, name: &str) -> Option<Tool> {
        self.tools.iter().find(|t| t.name == name).cloned()
    }

    async fn list_tools(
        &self,
        request: Option<PaginatedRequestParams>,
        _: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, McpError> {
        if request.is_some_and(|r| r.cursor.is_some()) {
            return Err(McpError::invalid_params("tool list is complete and has no pagination cursor", None));
        }
        Ok(ListToolsResult::with_all_items(self.tools.as_ref().clone()))
    }

    async fn call_tool(&self, request: CallToolRequestParams, context: RequestContext<RoleServer>) -> Result<CallToolResponse, McpError> {
        if !self.defs.contains_key(request.name.as_ref()) {
            return Err(McpError::new(ErrorCode::METHOD_NOT_FOUND, format!("unknown psx tool {:?}", request.name), None));
        }
        Ok(self.call(request.name.as_ref(), request.arguments.unwrap_or_default(), context.ct.clone()).await)
    }
}

// A dedicated stdin pump (from mdx) observes EOF independently of RMCP's
// handler draining and avoids Tokio's non-cancellable blocking stdin reads.
struct StdinPipe {
    receiver: tokio::sync::mpsc::Receiver<Vec<u8>>,
    current: Vec<u8>,
    position: usize,
}

impl tokio::io::AsyncRead for StdinPipe {
    fn poll_read(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buffer: &mut tokio::io::ReadBuf<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        use std::task::Poll;
        let this = self.get_mut();
        if buffer.remaining() == 0 {
            return Poll::Ready(Ok(()));
        }
        loop {
            if this.position < this.current.len() {
                let count = (this.current.len() - this.position).min(buffer.remaining());
                buffer.put_slice(&this.current[this.position..this.position + count]);
                this.position += count;
                return Poll::Ready(Ok(()));
            }
            match this.receiver.poll_recv(cx) {
                Poll::Ready(Some(bytes)) => {
                    this.current = bytes;
                    this.position = 0;
                }
                Poll::Ready(None) => return Poll::Ready(Ok(())),
                Poll::Pending => return Poll::Pending,
            }
        }
    }
}

fn stdin_pipe(shutdown: tokio_util::sync::CancellationToken) -> StdinPipe {
    let (sender, receiver) = tokio::sync::mpsc::channel(8);
    std::thread::spawn(move || {
        use std::io::Read;
        let mut input = std::io::stdin().lock();
        let mut buffer = [0u8; 8192];
        let mut frame_bytes = 0usize;
        loop {
            let count = match input.read(&mut buffer) {
                Ok(0) | Err(_) => break,
                Ok(n) => n,
            };
            for byte in &buffer[..count] {
                frame_bytes += 1;
                if frame_bytes > 2 * ARGS_LIMIT {
                    eprintln!("psx mcp: inbound JSON-RPC message exceeds 2 MiB; closing");
                    shutdown.cancel();
                    return;
                }
                if *byte == b'\n' {
                    frame_bytes = 0;
                }
            }
            if sender.blocking_send(buffer[..count].to_vec()).is_err() {
                break;
            }
        }
        shutdown.cancel();
    });
    StdinPipe { receiver, current: Vec::new(), position: 0 }
}

async fn serve(root: std::path::PathBuf, jobs: usize) -> Result<(), Box<dyn std::error::Error>> {
    let shutdown = tokio_util::sync::CancellationToken::new();
    let server = PsxServer::new(root, jobs);
    let input = stdin_pipe(shutdown.clone());
    let running = server.serve_with_ct((input, tokio::io::stdout()), shutdown.clone()).await?;
    let result = running.waiting().await;
    shutdown.cancel();
    result?;
    Ok(())
}

pub fn main(ctx: Ctx, a: McpArgs) -> ExitCode {
    if !(1..=8).contains(&a.jobs) {
        println!("{}", error_value("invalid_arguments", "--jobs must be 1..=8"));
        return ExitCode::from(1);
    }
    eprintln!("psx mcp: serving {} tools on stdio, project root {}", tools().len(), ctx.root.display());
    let rt = match tokio::runtime::Builder::new_multi_thread().enable_all().build() {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("psx mcp: {e}");
            return ExitCode::from(1);
        }
    };
    let r = rt.block_on(serve(ctx.root, a.jobs));
    // Jobs still running in the background are abandoned with the process.
    rt.shutdown_timeout(Duration::from_millis(200));
    match r {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("psx mcp: {e}");
            ExitCode::from(1)
        }
    }
}
