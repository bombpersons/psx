//! `psx mcp`: every CLI command as an MCP tool over stdin/stdout.
//!
//! Tools call the same [`Op`] implementations as the CLI, so a tool's
//! `structuredContent` is exactly the JSON document the CLI prints. stdout
//! carries only the protocol; diagnostics go to stderr.
//!
//! Differences from the CLI, all MCP-only:
//! - `return_screen: true` on `run` (and a few others) attaches the final
//!   screen as an image content block.
//! - Every call has a hard wall-clock cap (its `timeout` + 60 s, or 15 minutes).
//!   A job that overruns it is reported as `timeout` and left to finish in the
//!   background, except supervised Ghidra workers, which are stopped.
//! - Results over 32 KiB are written to `.psx/mcp/` and replaced by a summary
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
/// Results larger than this go to a file, with a summary inline: every
/// result stays in the agent's context for the rest of its session.
const INLINE_LIMIT: usize = 32 * 1024;
/// How much of a long text field (a listing, pseudocode) a spilled
/// result's summary keeps.
const PREVIEW: usize = 8 * 1024;
/// Request argument limit.
const ARGS_LIMIT: usize = 1024 * 1024;
/// Hard cap for a call without a `timeout` argument.
const DEFAULT_CAP: Duration = Duration::from_secs(15 * 60);
/// Extra time past a tool's own `timeout` before the call is abandoned.
const GRACE: Duration = Duration::from_secs(60);
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
        s
    }
}

/// Every tool. Descriptions are written for an agent doing PS1 reverse engineering.
pub fn tools() -> Vec<ToolDef> {
    vec![
        def::<crate::guide::GuideArgs>("guide", "The method: omit topic for the porting guide, read once; topics run, code, assets and setup hold the details. Tool descriptions are brief: argument rules and output details live here.").read_only(),
        def::<crate::messages::MessageArgs>("message", "Talk with the person, who reads psx web: text sends (reply_to: a message's id, to answer in its thread); without text, the latest threads. Their messages arrive in any psx result as messages: reply at once, in a line or two, then go on with the work."),
        def::<crate::reports::ReportArgs>("report", "A reviewer's report for the person alone (Markdown text), shown in psx web: the review brief says when."),
        def::<crate::context::ArgsContext>("context", "Start here: the running build, project root, saved defaults (media, BIOS), local BIOS candidates and the download catalog, whether Ghidra is found, and capabilities. image or from_state explains region or checkpoint identity; save=true stores the resolved defaults. Never downloads."),
        def::<crate::artifact::ReadArgs>("read", "Read a returned artifact or run directory in bounded pages: JSON by pointer, JSONL/CSV/text by rows with frame/pc/contains filters and column projection, hex bytes, PNG as an image. Continue with page.next_cursor.").read_only(),
        def::<crate::bios::InstallArgs>("bios_install", "One-time BIOS setup: import a local file (from), or download=true with region pal|ntsc-u|ntsc-j for a pinned, hash-checked image. Installs into the shared store (PSX_BIOS_DIR); never replaces existing images.").heavy(),
        def::<crate::media_cmd::DiscArgs>("disc_info", "First look at a disc image (.cue preferred): tracks, volume, SYSTEM.CNF boot line, boot EXE header and LibCrypt subchannel status. hash=true hashes the image files (slow).").read_only(),
        def::<crate::media_cmd::LsArgs>("disc_ls", "List ISO9660 entries (path, LBA, size, XA attributes, streamed flag) with filter/offset/limit, to map a disc's layout or find which file an LBA belongs to.").read_only(),
        def::<crate::media_cmd::ExtractArgs>("disc_extract", "Extract one disc file (path) or every file (all=true) into out. Form 2/streamed files (XA, STR) are written as raw 2352-byte sectors (.raw). offset/length (hex) read just a byte window of one file, returned inline as hex when small, with its LBA and raw-image byte offset. Returns sizes and SHA-256."),
        def::<crate::media_cmd::SectorArgs>("disc_sector", "Describe raw disc sectors from lba (count up to 256): track, owning file, mode/form, XA subheader and a hex preview, for streamed data the filesystem cannot read.").read_only(),
        def::<crate::media_cmd::ExeArgs>("disc_exe", "Parse a PS-X EXE header (entry pc, gp, load ranges, stack, region marker, hashes) from a file or image.cue::/PATH, e.g. the BOOT path from disc.").read_only(),
        def::<crate::assets_cmd::AssetsScanArgs>("assets_scan", "Find TIM/TMD/VAB/VAG assets by validated signatures in disc files (image.cue::/PATH, /DIR/), disc images, host files or directories, and RAM dumps; archives and external parsers extend it. Paged; no hit does not prove absence. See guide topic assets.").heavy(),
        def::<crate::assets_cmd::AssetsExtractArgs>("assets_extract", "Convert scanned assets into out: TIM to PNG, TMD to glTF, VAB/VAG to WAV with adpcm_sha256 per waveform, keeping original bytes and provenance. entry extracts one raw archive entry; brief=true returns counts. See guide topic assets.").heavy(),
        def::<crate::textures::TexturesArgs>("assets_textures", "Decode VRAM texture pages to PNGs: every page/CLUT a captured field used (run_dir a debugger stop's dir from debug_show gpu; plus textures/scene.psxscene, its layout), or one page and clut by hand from any vram.bin."),
        def::<crate::vram_diff::VramDiffArgs>("assets_diff", "Compare two raw VRAM dumps pixel by pixel, optionally within a region: differing pixel counts, channel deltas, sample pixels and an optional diff PNG."),
        def::<crate::media_cmd::MemcardArgs>("assets_memcard", "List the saves on a raw 128 KiB memory card image (.mcd/.mcr): product code, blocks used, title, icon frames and free blocks.").read_only(),
        def::<crate::code::DisasmArgs>("code_disasm", "Disassemble MIPS from an EXE (source) or run RAM (ram) at addr, or the heuristic function containing function. Resolves lui pairs and gp-relative references; text=true gives a listing. See guide topic code.").read_only(),
        def::<crate::decomp::CodeCheckArgs>("code_check", "Check the agent's C for a function (decomp/, names/) against the original: built for the R3000, the recordings reaching it replayed (or a state run), each call's effects compared, also with its arguments varied; with no function, every function whose C or names changed. See guide topic code.").heavy(),
        def::<crate::names_cmd::CodeNamesArgs>("code_names", "Look up a name (function, global, type, Type.field, function.local) or hex address in the names files: file and line, declaration, what it resolves to. With no query and a source or analysis: the files' problems and what the functions the captured recordings run most leave unnamed (callees, DAT_ globals, struct offsets in no field), weighted by runs. See guide topic code.").heavy(),
        def::<crate::code::XrefsArgs>("code_xrefs", "Static references to a hex address or LO-HI range in an EXE or run RAM: calls, jumps, branches, lui pairs, gp-relative accesses and data pointers, grouped by function. text=true gives a listing.").read_only(),
        def::<crate::ghidra::DecompileArgs>("code_decompile", "Pseudocode, body ranges and callers/callees for the function containing addr (hex, e.g. a writer PC). Give source (EXE or disc.cue::/PATH), ram with ranges and entries, or an analysis handle. The first call imports and analyses (minutes); needs local Ghidra. See guide topic code.").heavy(),
        def::<crate::ghidra::FunctionsArgs>("code_functions", "List a Ghidra analysis's functions (entries, names, signatures, body ranges) from source, ram or an analysis handle; filter by name, page with offset/limit.").heavy(),
        def::<crate::symbols_io::SymbolsArgs>("code_symbols", "Name PsyQ library functions in an EXE by signature matching (signatures from sigs or PSX_SIGS_DIR). Writes .symbols.json for run, loads, disasm and xrefs, plus Ghidra and .sym files, and .psx/names/. Given a disc, also searches it for debug files left by the build and imports those that describe its executables.").heavy(),
        def::<crate::inventory::InventoryArgs>("code_inventory", "Every function of the disc's executables and of overlays in ram snapshots, with instructions and blocks, and the disc's files. See guide topic code.").heavy(),
        def::<crate::coverage::CoverageArgs>("code_coverage", "Traced runs' executed blocks and branch ways per inventory function, and those no run reached. See guide topic code.").heavy(),
        def::<crate::coverage::CoverageDiffArgs>("code_coverage_diff", "What traced runs reach that the against runs do not: functions only they executed, and the blocks and branch ways only they took in functions both reached. Finds the code behind a behaviour. See guide topic code.").heavy(),
        def::<crate::ram::RamDiffArgs>("ram_diff", "Compare two RAM snapshots (run directories or ram.bin) by 1/2/4-byte unit within a range: differing units, deltas and clusters of nearby changes.").read_only(),
        def::<crate::ram::RamSearchArgs>("ram_search", "Cheat-finder over ordered RAM snapshots: keep addresses matching pred (changed, increased, delta=N...), exact values or a between range; save and candidates narrow the search over rounds.").read_only(),
        def::<crate::ram::RamViewArgs>("ram_view", "Show a RAM window (addr, len) from one or more snapshots side by side, decoded as u8..x32 or fxI.F, marking units that differ.").read_only(),
        def::<crate::run::RunCmdArgs>("run", "Run a disc, EXE, BIOS boot or checkpoint (from_state) for bounded frames on the native core. Drive it with input/replay; observe with sample, watch, watch_reads, trace, trace_cd or trace_spu; save_state makes checkpoints. Returns run_dir and the report; summary shortens it. See guide topic run.").heavy().screen(),
        def::<crate::states::StatesSaveArgs>("states_save", "Name a state an earlier run or handoff made; its chain from power-on comes from the runs' reports. See guide topic setup."),
        def::<crate::debug::DebugStartArgs>("debug_start", "A run that stops between calls: starts where run would (media, from_state or recording, the port's adjustments), before its first field. Returns its id (other debug calls default to the latest). See guide topic debug.").heavy(),
        def::<crate::debug::DebugGoArgs>("debug_go", "Go on to a new stop: pc, write, read, until, tick, draw, fields, ticks or instructions; history, trace, gpu, sample, watch and watch_reads keep what happens on the way. Returns why and where, the PC and its function.").heavy(),
        def::<crate::debug::DebugShowArgs>("debug_show", "Look at the debugging session's stop: registers (default), values, bytes, history, screen, stops, gpu (the latest capture), hardware.").screen(),
        def::<crate::debug::DebugSetArgs>("debug_set", "Poke memory, set a register or hold a pad state at the stop; a new stop, which debug_back undoes."),
        def::<crate::debug::DebugBackArgs>("debug_back", "Go back to the stop this one came from, or to=N; every stop is kept until debug_end."),
        def::<crate::debug::DebugSaveArgs>("debug_save", "Write the stop's machine state to a file, to continue from with run from_state (no name: save_as names states)."),
        def::<crate::debug::DebugEndArgs>("debug_end", "End a debugging session and remove its files (its stops and checkpoints in .psx/debug/)."),
        def::<crate::states::StatesListArgs>("states_list", "The named start states: description, steps from power-on, and whether each is present.").read_only(),
        def::<crate::states::StatesRebuildArgs>("states_rebuild", "Replay start states' chains from power-on on this psx and compare: which step first differs, and named values old and new. replace puts rebuilt states in place. See guide topic setup.").heavy(),
        def::<crate::scenario::ScenarioRecordArgs>("scenario_record", "In a port's workspace: run from the scenario's state with input for frames and write scenarios/<id>/<name>.jsonl, the compared glossary quantities at each tick's end, and <name>.coverage.json beside it, what the original executed as the inventory's code (code_coverage takes the .jsonl; take code_inventory first). See the guide's Recordings.").heavy(),
        def::<crate::scenario::ScenarioAdjustCheckArgs>("scenario_adjust_check", "Measure psx's adjustments to the original: record each scenario's recordings again, stock and adjusted, from the same ticks; each quantity's first differing tick.").heavy(),
        def::<crate::scenario::ScenarioCheckArgs>("scenario_check", "In a port's workspace: the determinism check. Reach each scenario again with idle fields before its chain's last run, record its recordings again, compare per tick.").heavy(),
        def::<crate::scenario::ScenarioShowArgs>("scenario_show", "In a port's workspace: a recording's quantities per tick (0: the initial moment), each numeric one's change since the tick before; fixed as exact decimals unless raw. quantities, ticks (A..B) narrow it; text=true: a table; screens=true: both games' screens.").read_only(),
        def::<crate::handoff::HandoffArgs>("scenario_play", "Ask the person to play: queues your request and returns a ticket at once; go on with other work: port_status lists it when they answer (recorded, or declined with a note). With id and name, play starts at the scenario's state (or where from starts) and becomes that recording. Without, from media or from_state (state.bin, input.json, note). See guide topic run.").screen(),
        def::<crate::port::PortInitArgs>("port_init", "Write a port's workspace in dest: the template, goal.toml, the catalogue, the disc and BIOS linked in, and this MCP server for the agent working there. See the guide."),
        def::<crate::template::TemplateUpdateArgs>("port_update", "Update the project's vendor/ files to this build (edited files and newer add-ons get a .psx-new copy) and move an older workspace to the current form. See guide topic setup."),
        def::<crate::template::PortAddArgs>("port_add", "Add a ready-made crate to the port's workspace under addons/: full (a window, a renderer with a PS1 and a modern look, the disc import and menus, wired into game/src/main.rs), shell, renderer, disc-import or menus. None is required. See guide topic setup."),
        def::<crate::port::PortStatusArgs>("port_status", "In a port's workspace, information only: the recordings by state, the code they ran and the port cites, branches, assets, plays, problems, and whether a review is due. plain=true: text for the person. See the guide.").read_only(),
        def::<crate::port::vectors::PortVectorsArgs>("port_vectors", "Write test vectors (JSON lines) for the original's integer arithmetic and the GTE, for the port's own tests. See guide topic arithmetic."),
        def::<crate::port::PortCheckArgs>("port_check", "In a port's workspace, at a clean commit: replay the recordings against the port and write the goal's verdict (held-out results are counted, not shown). See the guide.").heavy(),
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
fn prepare(tool: &ToolDef, args: Map<String, Value>) -> crate::Result<(Value, bool)> {
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
        "scenario_play" => &v["files"]["screen.png"]["path"],
        "debug_show" => &v["screen"],
        "read" => &v["image"]["path"],
        _ => return None,
    };
    p.as_str().map(str::to_string)
}

/// A tool's input schema without what goes without saying, since every
/// tool's schema is in the agent's context on every call: defaults of
/// null, false, 0-length lists or empty text; and null as an alternative
/// type (an optional field may be left out).
fn slim(v: &mut Value) {
    match v {
        Value::Object(o) => {
            let trivial = |d: &Value| match d {
                Value::Null | Value::Bool(false) => true,
                Value::Array(a) => a.is_empty(),
                Value::String(s) => s.is_empty(),
                _ => false,
            };
            if o.get("default").is_some_and(trivial) {
                o.remove("default");
            }
            if let Some(Value::Array(t)) = o.get_mut("type") {
                t.retain(|t| t != "null");
                if t.len() == 1 {
                    let only = t.remove(0);
                    o.insert("type".into(), only);
                }
            }
            if let Some(Value::Array(any)) = o.get("anyOf") {
                let rest: Vec<Value> = any.iter().filter(|a| a.get("type") != Some(&json!("null"))).cloned().collect();
                if rest.len() == 1 {
                    o.remove("anyOf");
                    if let Value::Object(only) = &rest[0] {
                        for (k, v) in only {
                            o.entry(k.clone()).or_insert_with(|| v.clone());
                        }
                    }
                }
            }
            o.values_mut().for_each(slim);
        }
        Value::Array(a) => a.iter_mut().for_each(slim),
        _ => {}
    }
}

/// A result asked for as `text`: the text, without the structured data it
/// shows (rows, listings), keeping small fields.
fn text_only(value: Value) -> Value {
    let Value::Object(o) = value else { return value };
    if !o.get("text").is_some_and(Value::is_string) || o.contains_key("error") {
        return Value::Object(o);
    }
    let small = |k: &str, v: &Value| k == "text" || serde_json::to_string(v).is_ok_and(|s| s.len() <= 200);
    Value::Object(o.into_iter().filter(|(k, v)| small(k, v)).collect())
}

fn spill(tool: &str, value: Value, ctx: &Ctx) -> Value {
    // As sent: compact. read pages through spilled results itself, so it is
    // never spilled.
    if serde_json::to_string(&value).map_or(0, |t| t.len()) <= INLINE_LIMIT || tool == "read" {
        return value;
    }
    let text = serde_json::to_string_pretty(&value).unwrap();
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
                        Value::String(t) => {
                            let mut end = PREVIEW.min(t.len());
                            while !t.is_char_boundary(end) {
                                end -= 1;
                            }
                            let end = t[..end].rfind('\n').map_or(end, |n| n + 1);
                            json!({"preview": &t[..end], "chars": t.len()})
                        }
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
        "note": "The result exceeded the MCP inline limit (32 KiB) and was written to result_file as pretty JSON. Large fields are summarised below (a long text's start as preview); read with a JSON pointer and pagination gives the rest. A narrower call (fewer instructions, a smaller range) often answers without it.",
        "summary": summary,
        "artifact": crate::artifact::descriptor(&path).ok(),
        "next_call": {"tool":"read","arguments":{"path":path,"project_root":ctx.root}},
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
            let mut schema = d.input_schema();
            slim(&mut schema);
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
        // any other root needs a worker running there; naming this one does not.
        let isolated = root != self.base;
        let from_client = scoped.is_none() && isolated;
        let worker_cancel = cancel.child_token();
        let client = peer.peer_info().map(|i| format!("{} {}", i.client_info.name, i.client_info.version));
        let log = crate::calls::start(&root, name, Value::Object(args.clone()), crate::calls::caller("mcp", client));
        let live = log.as_ref().map(|l| l.live_path());
        let inbox = root.clone();
        let respond = move |mut value: Value, shot: Option<Option<Vec<u8>>>| {
            // The person's messages ride on whatever the agent called.
            if value.is_object() && name != "message" {
                if let Some(m) = crate::messages::deliver(&inbox) {
                    value["messages"] = m;
                }
            }
            if let Some(log) = log {
                log.end(&value, u8::from(value.get("error").is_some()));
            }
            respond(value, shot)
        };
        // The person's held-out plays are judged, never shown to the agent.
        if let Some(why) =
            Some(name).filter(|n| *n != "message").and_then(|n| crate::port::held_out::refused(&root, n, &args))
        {
            return respond(Error::new("held_out", why).to_value(), None);
        }
        let ctx = Ctx { root, cancel: Some(worker_cancel.clone()) };
        let (args, want_screen) = match prepare(tool, args) {
            Ok(r) => r,
            Err(e) => return respond(e.to_value(), None),
        };
        let cap = args
            .get("timeout")
            .and_then(Value::as_u64)
            .map(|s| Duration::from_secs(s).saturating_add(GRACE))
            .unwrap_or(DEFAULT_CAP);
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
        let as_text = args.get("text") == Some(&Value::Bool(true));
        let supervised = isolated
            || name.starts_with("ghidra_")
            || name == "bios_install"
            || ["archive_parser", "model_parser", "asset_parser", "texture_parser", "frame_parser"]
                .iter()
                .any(|key| args.get(*key).is_some_and(Value::is_string));
        let mut worker = tokio::task::spawn_blocking(move || {
            let _permit = permit;
            let _live = live.map(crate::live::publish);
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
            let attach = want_screen || (tool_name == "read" && out["image"]["path"].is_string());
            let shot = attach.then(|| {
                screen_path(tool_name, &out).and_then(|p| {
                    let path = ctx.root.join(p);
                    let ok = std::fs::metadata(&path).is_ok_and(|m| m.len() <= IMAGE_LIMIT);
                    ok.then(|| std::fs::read(&path).ok()).flatten()
                })
            });
            if as_text {
                out = text_only(out);
            }
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
                 In a port's workspace port_check is the test and port_status shows where it stands; elsewhere start with context. guide is the method, read once (topics run, code, assets and setup hold the details): tool descriptions are brief and the guide holds argument rules and recipes. \
                 Emulation outputs are files in the returned run_dir; read them with read. run summary=true returns only paths and counts. Results over 32 KiB come back as a result_file path with a summary.",
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
    fn results_and_schemas_carry_nothing_twice() {
        // Text asked for: the table without the rows it shows.
        let rows: Vec<Value> = (0..50).map(|i| json!({"address": i, "values": [i, i]})).collect();
        let v = text_only(json!({"text": "a table", "format": "i32", "rows": rows}));
        assert_eq!(v, json!({"text": "a table", "format": "i32"}));
        // Defaults that go without saying, and null as an alternative type.
        let mut schema = json!({"properties": {
            "a": {"type": ["string", "null"], "default": null},
            "b": {"type": "boolean", "default": false},
            "c": {"type": "integer", "default": 60},
            "d": {"anyOf": [{"$ref": "#/$defs/X"}, {"type": "null"}], "description": "x"},
        }});
        slim(&mut schema);
        assert_eq!(
            schema,
            json!({"properties": {
                "a": {"type": "string"}, "b": {"type": "boolean"}, "c": {"type": "integer", "default": 60},
                "d": {"$ref": "#/$defs/X", "description": "x"},
            }})
        );
        // A spilled result keeps the start of a long text.
        let dir = std::env::temp_dir().join(format!("psx-spill-{}", std::process::id()));
        let ctx = Ctx { root: dir.clone(), cancel: None };
        let listing: String = (0..4000).map(|i| format!("  {i:08X}  nop\n")).collect();
        let v = spill("code_disasm", json!({"text": listing}), &ctx);
        let preview = v["summary"]["text"]["preview"].as_str().unwrap();
        assert!(preview.len() <= PREVIEW && preview.ends_with('\n') && v["result_file"].is_string(), "{v}");
        assert_eq!(spill("read", json!({"text": listing}), &ctx)["text"].as_str().unwrap().len(), listing.len());
        let _ = std::fs::remove_dir_all(dir);
    }

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
        // 71.6K with scenario_record and scenario_check (the guide pass trims later);
        // mechanic_check and mechanic_capture went on 2026-10-30; 71.7K from 2026-11-11 with
        // scenario_play's held_out.
        assert!(size <= 71_700, "tool definitions grew to {size} bytes; was 75K before 2026-10-17");
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
