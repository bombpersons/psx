//! `psx mcp`: every CLI command as an MCP tool over stdin/stdout.
//!
//! Tools call the same [`Op`] implementations as the CLI, so a tool's
//! `structuredContent` is exactly the JSON document the CLI prints. stdout
//! carries only the protocol; diagnostics go to stderr.
//!
//! Differences from the CLI, all MCP-only:
//! - `return_screen: true` on `run`/`native` attaches the final screen as an
//!   image content block (`native` then picks a `dump` prefix if none was given).
//! - `native` gets a default `timeout` (seconds) when none is given.
//! - Every call has a hard wall-clock cap (its `timeout` + 60 s, or 15 minutes).
//!   A job that overruns it is reported as `timeout` and left to finish in the
//!   background, except supervised Ghidra workers, which are stopped.
//! - Results over 256 KiB are written to `.psx/mcp/` and replaced by a summary
//!   with the file path.
//! - Heavy tools (emulation, signature matching) share `--jobs` execution slots.
//! - The default project root is `--root`/`PSX_ROOT` when given, else the
//!   client's single MCP root, else the server's working directory. A call whose
//!   root differs from the working directory runs in an isolated worker.
//!   `--require-project-root` rejects calls that omit `project_root`.

use std::collections::BTreeMap;
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

use base64::Engine;
use rmcp::model::{
    CallToolRequestParams, CallToolResponse, CallToolResult, ContentBlock, ErrorCode, Implementation, ListToolsResult,
    PaginatedRequestParams, ServerCapabilities, ServerConfig, Tool, ToolAnnotations,
};
use rmcp::service::{NotificationContext, Peer, RequestContext, RoleServer};
use rmcp::{ErrorData as McpError, ServerHandler, ServiceExt};
use serde_json::{Map, Value, json};
use tokio::sync::Semaphore;

use crate::Error;
use crate::commands::{Ctx, Op, Outcome};
pub(crate) mod project;

#[derive(clap::Args)]
pub struct McpArgs {
    /// Concurrent heavy jobs (emulation, symbols, loads, Ghidra queries); 1..=8.
    #[arg(long, default_value_t = 2)]
    pub jobs: usize,
    /// Reject calls without project_root instead of using the default root.
    #[arg(long)]
    pub require_project_root: bool,
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

/// A handoff over MCP always detaches: the person may take longer than any call.
fn call_handoff(args: Value, ctx: &Ctx) -> Result<Outcome, String> {
    let mut a: crate::handoff::HandoffArgs = serde_json::from_value(args).map_err(|e| e.to_string())?;
    a.detach = true;
    Ok(a.exec(ctx).with_path_context(ctx))
}

/// `detach` is forced over MCP, so it is not offered.
fn handoff_schema() -> Value {
    let mut s = schema_of::<crate::handoff::HandoffArgs>();
    if let Some(p) = s["properties"].as_object_mut() {
        p.remove("detach");
    }
    s
}

fn schema_of<A: Op>() -> Value {
    let mut s = serde_json::to_value(schemars::schema_for!(A)).unwrap();
    if let Some(o) = s.as_object_mut() {
        o.remove("$schema");
        o.remove("title");
        o.remove("description");
        o.entry("properties").or_insert_with(|| json!({}));
    }
    portable_schema(&mut s);
    brief_descriptions(&mut s);
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
            "description":"MCP only: project directory for this call."});
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
    vec![
        def::<crate::guide::GuideArgs>("guide", "Built-in workflow guidance. Omit topic for an overview. Tool descriptions are brief: argument rules and output details live in its topics.").read_only(),
        def::<crate::context::ArgsContext>("context", "Start here: the running build, project root, saved defaults (media, BIOS), local BIOS candidates and capabilities. image or from_state explains region or checkpoint identity; save=true stores the resolved defaults. Never downloads."),
        def::<crate::artifact::ReadArgs>("artifact_read", "Read a returned artifact or run directory in bounded pages: JSON by pointer, JSONL/CSV/text by rows with frame/pc/contains filters and column projection, hex bytes, PNG as an image. Continue with page.next_cursor.").read_only(),
        def::<crate::bios::ListArgs>("bios_list", "List BIOS images in the project's bios/ and the shared BIOS store, with hashes, regions and the download catalog. Read-only; context shows which one is selected and why.").read_only(),
        def::<crate::bios::InstallArgs>("bios_install", "One-time BIOS setup: import a local file (from), or download=true with region pal|ntsc-u|ntsc-j for a pinned, hash-checked image. Installs into the shared store (PSX_BIOS_DIR); never replaces existing images.").heavy(),
        def::<crate::ghidra::AnnotateArgs>("ghidra_annotate", "Apply function renames and comments from a JSON file (functions:[{addr,name,comment}]) to a Ghidra analysis as a new revision. User-name collisions fail; generated names yield.").heavy(),
        def::<crate::ghidra::ExportArgs>("ghidra_export_symbols", "Export a Ghidra analysis's function names as .symbols.json, bound to the input hash and function bytes, for the symbols argument of run, loads, disasm or xrefs.").heavy(),
        def::<crate::ghidra::DoctorArgs>("ghidra_doctor", "Check that Ghidra and Java are discoverable. Usually unnecessary: query tools import and analyse automatically. Set PSX_GHIDRA_HOME or ghidra_home if discovery fails."),
        def::<crate::ghidra::Input>("ghidra_analyze", "Optional: prewarm a reusable Ghidra analysis from source (EXE or disc.cue::/PATH) or a RAM snapshot (with ranges and entries). Returns an analysis handle; query tools analyse automatically anyway.").heavy(),
        def::<crate::ghidra::DecompileArgs>("ghidra_decompile", "Pseudocode, body ranges and callers/callees for the function containing addr (hex, e.g. a writer PC). Give source (EXE or disc.cue::/PATH), ram with ranges and entries, or an analysis handle. The first call imports and analyses (minutes); needs local Ghidra. See guide topic code.").heavy(),
        def::<crate::ghidra::FunctionsArgs>("ghidra_functions", "List a Ghidra analysis's functions (entries, names, signatures, body ranges) from source, ram or an analysis handle; filter by name, page with offset/limit.").heavy(),
        def::<crate::ghidra::XrefsArgs>("ghidra_xrefs", "References to and from addr (hex code or data) in a Ghidra analysis, with type, call status and containing function; paged. Indirect references are incomplete.").heavy(),
        def::<crate::media_cmd::DiscArgs>("disc", "First look at a disc image (.cue preferred): tracks, volume, SYSTEM.CNF boot line, boot EXE header and LibCrypt subchannel status. hash=true hashes the image files (slow).").read_only(),
        def::<crate::media_cmd::LsArgs>("ls", "List ISO9660 entries (path, LBA, size, XA attributes, streamed flag) with filter/offset/limit, to map a disc's layout or find which file an LBA belongs to.").read_only(),
        def::<crate::media_cmd::ExtractArgs>("extract", "Extract one disc file (path) or every file (all=true) into out. Form 2/streamed files (XA, STR) are written as raw 2352-byte sectors (.raw). offset/length (hex) read just a byte window of one file, returned inline as hex when small, with its LBA and raw-image byte offset. Returns sizes and SHA-256."),
        def::<crate::media_cmd::ExeArgs>("exe", "Parse a PS-X EXE header (entry pc, gp, load ranges, stack, region marker, hashes) from a file or image.cue::/PATH, e.g. the BOOT path from disc.").read_only(),
        def::<crate::media_cmd::SectorArgs>("sector", "Describe raw disc sectors from lba (count up to 256): track, owning file, mode/form, XA subheader and a hex preview, for streamed data the filesystem cannot read.").read_only(),
        def::<crate::media_cmd::AddrArgs>("addr", "Classify a CPU address (hex): segment, physical address, region (main RAM with mirrors, scratchpad, I/O, BIOS, expansion) and offset within it.").read_only(),
        def::<crate::xa_cmd::XaArgs>("xa", "List the XA-ADPCM channels in a streamed file (rate, stereo, duration, sectors); with channel and out, decode one channel to a 44.1 kHz WAV."),
        def::<crate::str_cmd::StrArgs>("str", "Decode an STR/MDEC movie from image.cue::/PATH into out: frame PNGs, audio.wav, a contact sheet and summary. frames limits the PNGs; frame_parser handles custom frame wrappers.").heavy(),
        def::<crate::assets_cmd::AssetsScanArgs>("assets_scan", "Find TIM/TMD/VAB/VAG assets by validated signatures in disc files (image.cue::/PATH, /DIR/), disc images, host files or directories, and RAM dumps; archives and external parsers extend it. Paged; no hit does not prove absence. See guide topic assets.").heavy(),
        def::<crate::assets_cmd::AssetsExtractArgs>("assets_extract", "Convert scanned assets into out: TIM to PNG, TMD to glTF, VAB/VAG to WAV with adpcm_sha256 per waveform, keeping original bytes and provenance. entry extracts one raw archive entry; brief=true returns counts. See guide topic assets.").heavy(),
        def::<crate::media_cmd::MemcardArgs>("memcard", "List the saves on a raw 128 KiB memory card image (.mcd/.mcr): product code, blocks used, title, icon frames and free blocks.").read_only(),
        def::<crate::textures::TexturesArgs>("textures", "Decode VRAM texture pages to PNGs: every page/CLUT a gpu_frame run used (plus textures/scene.psxscene for the Bevy template), or one page and clut by hand from any vram.bin."),
        def::<crate::run::RunCmdArgs>("run", "Run a disc, EXE, BIOS boot or checkpoint (from_state) for bounded frames on the native core. Drive it with input/replay; observe with sample, watch, watch_reads, poke, trace, trace_cd, trace_spu or gpu_frame; save_state makes checkpoints. Returns run_dir and the report; brief or summary shorten it. See guide topics run and input.").heavy().screen(),
        ToolDef { call: call_handoff, schema: handoff_schema, ..def::<crate::handoff::HandoffArgs>("handoff", "Ask the person to play to a point you cannot easily reach: opens a window on this machine with the game and your request, and returns a ticket at once. Poll handoff_status; the result has state.bin to continue from, input.json (their input as a replay), their note, and the moments they marked with F6 (state, RAM and screen each, with RAM diffs between them).") },
        def::<crate::case::CaseRecordArgs>("case_record", "Record a case for a spec from a start state, input and a field map subject: the case JSON, its CSV with the tick column, and a recipe. See guide topic cases.").heavy(),
        def::<crate::case::CaseCheckArgs>("case_check", "Record cases again from their recipes and compare them with their CSVs, cell by cell. See guide topic cases.").heavy(),
        def::<crate::states::StatesMakeArgs>("states_make", "Run from a named start state (or power-on) and name the state it ends in: .psx/states/NAME.bin, and its chain of runs from power-on in notes/states/NAME.json. See guide topic states.").heavy(),
        def::<crate::states::StatesSaveArgs>("states_save", "Name a state an earlier run or handoff made; its chain from power-on comes from the runs' reports. See guide topic states."),
        def::<crate::inventory::InventoryArgs>("inventory", "Every function of the disc's executables and of overlays in ram snapshots, with instructions and blocks, and the disc's files. See guide topic code.").heavy(),
        def::<crate::coverage::CoverageArgs>("coverage", "Traced runs' executed blocks per inventory function, and the blocks never executed. See guide topic code.").heavy(),
        def::<crate::states::StatesListArgs>("states_list", "The named start states: description, steps from power-on, and whether each is present.").read_only(),
        def::<crate::states::StatesRebuildArgs>("states_rebuild", "Replay start states' chains from power-on on this psx and compare: which step first differs, and named values old and new. replace puts rebuilt states in place. See guide topic states.").heavy(),
        def::<crate::handoff::HandoffStatusArgs>("handoff_status", "A handoff's progress or result: waiting, done, declined (read the note), closed or abandoned.").read_only().screen(),
        def::<crate::native::NativeArgs>("native", "Emulator diagnostic: boot the native core from power-on and report where it got (PC, CD history, SPU state, unhandled I/O, hot PCs, speed). For game investigation use run.").heavy().default_timeout().screen(),
        def::<crate::provenance::LoadsArgs>("loads", "Turn a run recorded with trace_cd into disc-to-RAM provenance: which file and offset each CD transfer copied where, verified against the run's final RAM. Paged; symbols names the reading code.").heavy(),
        def::<crate::symbols_io::SymbolsArgs>("symbols", "Name PsyQ library functions in an EXE by signature matching (signatures from sigs or PSX_SIGS_DIR). Writes .symbols.json for run, loads, disasm and xrefs, plus Ghidra and .sym files. Library code only.").heavy(),
        def::<crate::code::DisasmArgs>("disasm", "Disassemble MIPS from an EXE (source) or run RAM (ram) at addr, or the heuristic function containing function. Resolves lui pairs and gp-relative references; text=true gives a listing. See guide topic code.").read_only(),
        def::<crate::code::XrefsArgs>("xrefs", "Static references to a hex address or LO-HI range in an EXE or run RAM: calls, jumps, branches, lui pairs, gp-relative accesses and data pointers, grouped by function. text=true gives a listing.").read_only(),
        def::<crate::ram::RamDiffArgs>("ram_diff", "Compare two RAM snapshots (run directories or ram.bin) by 1/2/4-byte unit within a range: differing units, deltas and clusters of nearby changes.").read_only(),
        def::<crate::ram::RamSearchArgs>("ram_search", "Cheat-finder over ordered RAM snapshots: keep addresses matching pred (changed, increased, delta=N...), exact values or a between range; save and candidates narrow the search over rounds.").read_only(),
        def::<crate::ram::RamViewArgs>("ram_view", "Show a RAM window (addr, len) from one or more snapshots side by side, decoded as u8..x32 or fxI.F, marking units that differ.").read_only(),
        def::<crate::parser_template::ParserTemplateArgs>("template_parser_create", "Create an executable Python 3 external parser starter (kind archive, model, asset or video) with a sample input and protocol notes in dest, for assets_scan, assets_extract or str."),
        def::<crate::template::TemplateCreateArgs>("template_create", "Create a Bevy port workspace (psx-bevy, engine, game, importers) with the PS1 look in dest, e.g. the game folder. Set region and fields_per_tick from the original. See guide topics port, bevy."),
        def::<crate::port::PortInitArgs>("port_init", "Write a port's workspace in dest: the Bevy template, goal.toml, the catalogue, the disc and BIOS linked in, and this MCP server for the agent working there. See guide topic port."),
        def::<crate::port::PortStatusArgs>("port_status", "In a port's workspace: the goal's features by area and stage, their verdicts, problems in the goal or catalogue, and the item due now. plain=true: text for the person. See guide topic port.").read_only(),
        def::<crate::port::PortNextArgs>("port_next", "In a port's workspace: the one item due now (what, which feature, why, and the fact that shows it done). Start every session with it, and call it again after each item. See guide topic port."),
        def::<crate::port::PortCheckArgs>("port_check", "In a port's workspace, at a clean commit: run the features' scenarios against the port and write their scenario verdicts. See guide topic port.").heavy(),
        def::<crate::scenario::ScenarioRecordArgs>("scenario_record", "In a port's workspace: run from the scenario's state with input for frames and write scenarios/<id>/<name>.jsonl, the compared glossary quantities at each tick's end. See docs/scenarios.md.").heavy(),
        def::<crate::scenario::ScenarioCheckArgs>("scenario_check", "In a port's workspace: the determinism check. Reach each scenario again with idle fields before its chain's last run, record its recordings again, compare per tick.").heavy(),
        def::<crate::mechanic::MechanicCheckArgs>("mechanic_check", "In a port's workspace: check interfaces/ and a mechanic's (or every mechanic's) crate, binding and traces. See docs/mechanics.md.").read_only(),
        def::<crate::mechanic::MechanicCaptureArgs>("mechanic_capture", "In a port's workspace: run a scenario recording again and capture each mechanic in ids (default: every bound one) in that run, to mechanics/<id>/traces/<scenario>/<recording>.trace.jsonl. Reports per mechanic the read audit, external changes, coverage and refusals.").heavy(),
        def::<crate::template::TemplateUpdateArgs>("template_update", "Update the project's psx-bevy/ and vendor/ files to this build; edited files get a .psx-new copy. See guide topic port."),
        def::<crate::vram_diff::GpuReplayArgs>("gpu_replay", "Replay a recorded GP0 command stream into a fresh software GPU and write the resulting VRAM to out (and optionally a PNG). Pair with vram_diff."),
        def::<crate::vram_diff::VramDiffArgs>("vram_diff", "Compare two raw VRAM dumps pixel by pixel, optionally within a region: differing pixel counts, channel deltas, sample pixels and an optional diff PNG."),
    ]
}

/// MCP parameter descriptions keep their first sentence; the CLI's `--help`
/// and the guide topics carry the rest.
fn brief_descriptions(value: &mut Value) {
    match value {
        Value::Object(object) => {
            if let Some(Value::String(text)) = object.get_mut("description") {
                *text = first_sentence(text);
            }
            object.values_mut().for_each(brief_descriptions);
        }
        Value::Array(values) => values.iter_mut().for_each(brief_descriptions),
        _ => {}
    }
}

/// Whitespace-normalised text up to the first full stop that ends a sentence
/// (not one inside "e.g." or "i.e.").
fn first_sentence(text: &str) -> String {
    let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let end = text
        .match_indices(". ")
        .map(|(i, _)| i)
        .find(|&i| !["e.g", "i.e", "etc", "vs"].iter().any(|a| text[..i].ends_with(a)));
    match end {
        Some(i) => text[..=i].to_string(),
        None => text,
    }
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
                // The full u32 range adds nothing a client needs to know.
                Some("uint32") => Some((json!(0), None)),
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

/// Validate and complete a call's arguments. Returns the arguments for the
/// command and whether a screen image was requested.
fn prepare(tool: &ToolDef, args: Map<String, Value>, ctx: &Ctx) -> crate::Result<(Value, bool)> {
    let mut args = args;
    let screen = match args.remove("return_screen") {
        None => false,
        Some(Value::Bool(b)) if tool.screen => b,
        Some(_) if tool.screen => return Err(Error::invalid("return_screen must be a boolean")),
        Some(_) => return Err(Error::invalid(format!("{} does not take return_screen", tool.name))),
    };
    let schema = (tool.schema)();
    let known: Vec<&String> = schema["properties"].as_object().map(|p| p.keys().collect()).unwrap_or_default();
    if let Some(k) = args.keys().find(|k| !known.contains(k)) {
        let names: Vec<&str> = known.iter().map(|s| s.as_str()).collect();
        return Err(Error::new(
            "invalid_arguments",
            format!("unknown argument {k:?} for {}; expected one of {}", tool.name, names.join(", ")),
        ));
    }
    if tool.default_timeout && args.get("timeout").is_none_or(Value::is_null) {
        args.insert("timeout".into(), json!(DEFAULT_TIMEOUT_S));
    }
    if tool.name == "native" && screen && args.get("dump").is_none_or(Value::is_null) {
        let dir = ctx.root.join(".psx/native").join(crate::stamp());
        args.insert("dump".into(), json!(dir.join("native")));
    }
    Ok((Value::Object(args), screen))
}

/// Path of the final screen PNG in a `run` or `native` result.
fn screen_path(tool: &str, v: &Value) -> Option<String> {
    let p = match tool {
        "run" => {
            if let Some(path) = v["files"]["screen.png"]["path"].as_str().or(v["screen"].as_str()) {
                return Some(path.into());
            }
            // Compact reports retain the run directory, but list artifact names.
            // Only attach a screenshot when the report says it was written.
            let listed = v["files"].as_array()?.iter().any(|f| f == "screen.png");
            let dir = v["run_dir"].as_str()?;
            return listed.then(|| std::path::Path::new(dir).join("screen.png").to_string_lossy().into_owned());
        }
        "handoff_status" => &v["files"]["screen.png"]["path"],
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
    let path = dir.join(format!("{tool}-{}.json", crate::stamp()));
    if let Err(e) = std::fs::create_dir_all(&dir).and_then(|_| std::fs::write(&path, &text)) {
        return Error::new(
            "io_error",
            format!("result of {} bytes could not be written to {}: {e}", text.len(), path.display()),
        )
        .to_value();
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
    /// The process working directory: in-process calls resolve paths against it.
    base: std::path::PathBuf,
    /// `--root`/`PSX_ROOT` fixed the default; client roots are ignored.
    pinned: bool,
    require_root: bool,
    /// The default root from client roots; `None` until first needed and after
    /// the client reports a roots change.
    client_root: Arc<tokio::sync::Mutex<Option<std::path::PathBuf>>>,
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
    fn new(base: std::path::PathBuf, pinned: bool, require_root: bool, jobs: usize) -> Self {
        let mut tools = Vec::new();
        let mut defs = BTreeMap::new();
        for d in self::tools() {
            let schema = d.input_schema();
            let tool = Tool::new(d.name, d.description, Arc::new(schema.as_object().unwrap().clone()))
                .with_annotations(ToolAnnotations::from_raw(None, Some(d.read_only), Some(false), None, Some(false)));
            tools.push(tool);
            defs.insert(d.name, d);
        }
        PsxServer {
            base,
            pinned,
            require_root,
            client_root: Arc::default(),
            tools: Arc::new(tools),
            defs: Arc::new(defs),
            slots: Arc::new(Semaphore::new(jobs)),
        }
    }

    /// The default project root for a call without `project_root`. The lock is
    /// held across the roots query so concurrent first calls ask only once.
    async fn default_root(&self, peer: &Peer<RoleServer>) -> std::path::PathBuf {
        if self.pinned {
            return self.base.clone();
        }
        let mut cached = self.client_root.lock().await;
        cached.get_or_insert(project::client_root(peer).await.unwrap_or_else(|| self.base.clone())).clone()
    }

    async fn call(
        &self,
        name: &str,
        mut args: Map<String, Value>,
        peer: &Peer<RoleServer>,
        cancel: tokio_util::sync::CancellationToken,
    ) -> CallToolResponse {
        let tool = &self.defs[name];
        if serde_json::to_vec(&args).map(|v| v.len()).unwrap_or(0) > ARGS_LIMIT {
            return respond(Error::new("limit_exceeded", "tool arguments exceed 1 MiB").to_value(), None);
        }
        let scoped = args.remove("project_root");
        if scoped.is_none() && self.require_root {
            return respond(
                Error::new(
                    "invalid_arguments",
                    "this server was started with --require-project-root; pass project_root (the game study directory) on every call",
                ).to_value(),
                None,
            );
        }
        let default = self.default_root(peer).await;
        let root = match project::resolve(&default, scoped.as_ref()) {
            Ok(root) => root,
            Err(e) => return respond(e.to_value(), None),
        };
        // Relative paths in-process resolve against the working directory, so
        // any other root needs a worker running there.
        let isolated = scoped.is_some() || root != self.base;
        let from_client = scoped.is_none() && isolated;
        let worker_cancel = cancel.child_token();
        let ctx = Ctx { root, cancel: Some(worker_cancel.clone()) };
        let (args, want_screen) = match prepare(tool, args, &ctx) {
            Ok(r) => r,
            Err(e) => return respond(e.to_value(), None),
        };
        let cap =
            args.get("timeout").and_then(Value::as_u64).map(|s| Duration::from_secs(s) + GRACE).unwrap_or(DEFAULT_CAP);
        let queued_at = std::time::Instant::now();
        let permit = if tool.heavy {
            tokio::select! {
                p = tokio::time::timeout(cap,self.slots.clone().acquire_owned()) => match p {
                    Ok(Ok(p)) => Some(p),
                    Ok(Err(_)) => return respond(Error::new("internal", "execution queue closed").to_value(), None),
                    Err(_) => return respond(Error::new("timeout","Deadline reached while waiting for an execution slot").to_value(),None),
                },
                _ = cancel.cancelled() => return respond(Error::new("cancelled", "cancelled while waiting for an execution slot").to_value(), None),
            }
        } else {
            None
        };
        let call = tool.call;
        let tool_name = tool.name;
        let supervised = isolated
            || name.starts_with("ghidra_")
            || name == "bios_install"
            || ["archive_parser", "model_parser", "asset_parser", "texture_parser", "frame_parser"]
                .iter()
                .any(|key| args.get(*key).is_some_and(Value::is_string));
        let mut worker = tokio::task::spawn_blocking(move || {
            let _permit = permit;
            let mut out = if isolated {
                project::call(tool_name, args, &ctx).unwrap_or_else(|e| Error::new("worker_failed", e).to_value())
            } else {
                match call(args, &ctx) {
                    Ok(o) => o.value,
                    Err(e) => Error::invalid(e).to_value(),
                }
            };
            if from_client && tool_name == "context" && out.is_object() {
                out["project_root_source"] = json!("client_roots");
            }
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
                Ok(Err(e)) => respond(Error::new("internal", format!("{name} failed: {e}")).to_value(), None),
                Err(_) => {
                    worker_cancel.cancel();
                    if supervised { let _ = tokio::time::timeout(Duration::from_secs(3), &mut worker).await; }
                    respond(Error::new("timeout", if supervised { format!("{name} exceeded {} s; worker termination requested", cap.as_secs()) } else { format!("{name} exceeded {} s; it keeps running in the background and may still write its outputs", cap.as_secs()) }).to_value(), None)
                },
            },
            _ = cancel.cancelled() => {
                worker_cancel.cancel();
                if supervised { let _ = tokio::time::timeout(Duration::from_secs(3), &mut worker).await; }
                respond(Error::new("cancelled", if supervised { format!("{name} was cancelled; worker termination requested") } else { format!("{name} was cancelled; the job keeps running in the background until its own timeout") }).to_value(), None)
            },
        }
    }
}

impl PsxServer {
    fn default_note(&self) -> String {
        let required = if self.require_root { "This server requires project_root on every call. " } else { "" };
        if self.pinned {
            format!("{required}Default project root: {} (fixed by --root/PSX_ROOT).", self.base.display())
        } else {
            format!(
                "{required}Default project root: the client's workspace root when it reports exactly one, otherwise {}. context reports the root in use.",
                self.base.display()
            )
        }
    }
}

impl ServerHandler for PsxServer {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new("psx", crate::context::version()))
            .with_instructions(format!(
                "PlayStation (PS1) reverse-engineering toolkit. Each tool is one `psx` CLI command and returns the JSON the CLI prints. \
                 {default} bios/, media/, external/ and .psx/ outputs live in the project root; relative paths resolve against it. project_root on any call selects another existing project for that call only. \
                 Start with context, then guide (overview, or a topic: setup, input, run, code, assets, bevy, port, troubleshooting): tool descriptions are brief and the guide holds argument rules and recipes. \
                 Emulation outputs are files in the returned run_dir; read them with artifact_read. run summary=true returns only paths and counts. Results over 256 KiB come back as a result_file path.",
                default = self.default_note()
            ))
    }

    async fn on_roots_list_changed(&self, _: NotificationContext<RoleServer>) {
        *self.client_root.lock().await = None;
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

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, McpError> {
        if !self.defs.contains_key(request.name.as_ref()) {
            return Err(McpError::new(
                ErrorCode::METHOD_NOT_FOUND,
                format!("unknown psx tool {:?}", request.name),
                None,
            ));
        }
        Ok(self
            .call(request.name.as_ref(), request.arguments.unwrap_or_default(), &context.peer, context.ct.clone())
            .await)
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

async fn serve(root: std::path::PathBuf, pinned: bool, a: McpArgs) -> Result<(), Box<dyn std::error::Error>> {
    let shutdown = tokio_util::sync::CancellationToken::new();
    let server = PsxServer::new(root, pinned, a.require_project_root, a.jobs);
    let input = stdin_pipe(shutdown.clone());
    let running = server.serve_with_ct((input, tokio::io::stdout()), shutdown.clone()).await?;
    let result = running.waiting().await;
    shutdown.cancel();
    result?;
    Ok(())
}

pub fn main(ctx: Ctx, a: McpArgs, pinned: bool) -> ExitCode {
    if !(1..=8).contains(&a.jobs) {
        println!("{}", Error::invalid("--jobs must be 1..=8").to_value());
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
    let r = rt.block_on(serve(ctx.root, pinned, a));
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn descriptions_keep_their_first_sentence() {
        assert_eq!(first_sentence("Pick one, e.g. `new`. Later detail.\nMore."), "Pick one, e.g. `new`.");
        assert_eq!(first_sentence("No full stop\n  here"), "No full stop here");
        assert_eq!(first_sentence("Ends here."), "Ends here.");
    }

    /// The tool list is loaded into every agent's context: keep it bounded.
    #[test]
    fn published_tool_definitions_stay_within_budget() {
        let tools = tools();
        let size: usize = tools
            .iter()
            .map(|t| t.name.len() + t.description.len() + serde_json::to_string(&t.input_schema()).unwrap().len())
            .sum();
        // 50K on 2026-10-17, grown with each tool since; 65.7K from 2026-10-29, after the
        // clean-room tools went and port_status and port_next came; 68.5K from
        // 2026-10-29 with mechanic_check and mechanic_capture; 69K with port_check;
        // 71.6K with scenario_record and scenario_check (the guide pass trims later).
        assert!(size <= 71_600, "tool definitions grew to {size} bytes; was 75K before 2026-10-17");
        for t in &tools {
            assert!(
                t.description.len() <= 400,
                "{} description is {} bytes; move detail into a guide topic",
                t.name,
                t.description.len()
            );
        }
    }
}
