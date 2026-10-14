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
//!   background; it cannot be killed from here.
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

#[derive(clap::Args)]
pub struct McpArgs {
    /// Concurrent heavy jobs (run, native, lockstep, symbols, loads); 1..=8.
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
    Ok(a.exec(ctx))
}

fn schema_of<A: Op>() -> Value {
    let mut s = serde_json::to_value(schemars::schema_for!(A)).unwrap();
    if let Some(o) = s.as_object_mut() {
        o.remove("title");
        o.remove("description");
        o.entry("properties").or_insert_with(|| json!({}));
    }
    portable_schema(&mut s);
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
        if self.screen {
            s["properties"]["return_screen"] = json!({
                "type": "boolean", "default": false,
                "description": "MCP only: also return the final screen PNG as image content (a few tens of KB).",
            });
        }
        if self.default_timeout {
            if let Some(t) = s["properties"].get_mut("timeout") {
                t["description"] = json!(format!("Wall-clock limit in seconds; the job stops with status \"timeout\". Default over MCP: {DEFAULT_TIMEOUT_S}."));
            }
        }
        s
    }
}

/// Every tool. Descriptions are written for an agent doing PS1 reverse engineering.
pub fn tools() -> Vec<ToolDef> {
    use commands::*;
    vec![
        def::<DiscArgs>("disc", "First look at a PlayStation disc image (.cue preferred; raw .bin or .iso also work): tracks, volume descriptor, file/dir counts, SYSTEM.CNF (BOOT line, stack, TCB/EVENT), boot executable header (entry pc, gp, load range, region marker) and LibCrypt subchannel status. LibCrypt-protected discs need the redump .sbi/.lsd next to the .cue (same base name); `subchannel.source` reports whether it was found and the native core honours it. hash=true streams SHA-256 over the image files (slow for full CDs).").read_only(),
        def::<LsArgs>("ls", "List every ISO9660 entry: path, LBA, size, sector count, directory flag, CD-XA attributes and `streamed` (Form 2 / interleaved XA or STR data that is read raw, not via the file system). Use it to map a disc's layout before extract/sector/xa, or to identify which file an LBA belongs to. Large discs give large lists; results above 256 KiB are written to a file and the path returned.").read_only(),
        def::<ExtractArgs>("extract", "Extract one file (`path`, e.g. `/DATA/LEVEL1.BIN` or `cdrom:\\SLUS_000.79;1`) or every file (`all: true`) into `out` (default ./extracted). Form 1 files are written as user data; Form 2 / interleaved files (XA audio, STR video) are written as raw 2352-byte sectors with a `.raw` suffix. Returns per-file destination, size and SHA-256."),
        def::<ExeArgs>("exe", "Parse a PS-X EXE header: entry pc, gp, text/data/bss ranges, initial stack, region marker string and hashes. `source` is a file path or `image.cue::/PATH` to read an executable straight from a disc (e.g. the BOOT path from `disc`). Use the load range to know where code lives in RAM (KSEG0 0x80000000+).").read_only(),
        def::<SectorArgs>("sector", "Describe raw disc sectors starting at `lba` (up to 256 with `count`): track, owning ISO9660 file, mode/form, XA subheader (file, channel, submode, coding) and a hex preview of the user data. Use for streamed data that the file system cannot read, or to check what a CD-ROM trace (`loads`) pointed at.").read_only(),
        def::<AddrArgs>("addr", "Classify a CPU address (hex, e.g. 0x80010000 or 1F801810): KUSEG/KSEG0/KSEG1/KSEG2 segment, physical address, region (main RAM with its mirrors folded, scratchpad, I/O, BIOS, expansion areas, cache control) and the offset into that region. Cheap; use it whenever an address from a trace or watch needs context.").read_only(),
        def::<XaArgs>("xa", "List the XA-ADPCM audio channels interleaved in a streamed file (STR movies, .XA music banks): per file/channel first LBA, sector count, duration, sample rate, bit depth and stereo flag. With `channel` and `out` it decodes that channel to a 44.1 kHz WAV (`file` also filters on the subheader file number)."),
        def::<StrArgs>("str", "Decode a PlayStation FMV (STR/MDEC bitstream v1/v2/v3, incl. FF7-style prefixed frames) from a disc file into `out`: frame_NNNNN.png (cropped to the movie size), audio.wav (interleaved XA, same as `xa`), contact.png, frames.json and summary.json (dimensions, frame count, fps estimate, BS versions, XA channels). `source` is `image.cue::/PATH`. `frames` limits the PNGs (A..B, 1-based). Unsupported variants (polygon streams, iki, v0, encrypted) are refused with the variant named."),
        def::<MemcardArgs>("memcard", "List the saves on a raw 128 KiB memory card image (.mcd/.mcr, e.g. one written by `run` with memcard1/memcard2): product code, blocks used, Shift-JIS title and icon frames, plus free blocks.").read_only(),
        def::<TexturesArgs>("textures", "Decode VRAM texture pages through their CLUTs into PNGs. Given a native run directory captured with `gpu_frame`, it decodes every texture page/CLUT pair that frame's primitives used (full page and cropped to the UV bounds actually sampled) and writes textures/scene.psxscene for the psx-bevy template. With `page` (X,Y,DEPTH in VRAM halfword units, 4/8/15 bpp) and `clut` (X,Y) it decodes one page by hand from any run's vram.bin."),
        def::<RunCmdArgs>("run", "Bounded emulation of a disc, PS-X EXE or BIOS boot for `frames` vsyncs, capturing ram.bin, scratch.bin, vram.bin, screen.png, vram.png, registers and optional state.bin into a run directory (default .psx/runs/<id>; returned as run_dir with every file path). Read files from there rather than expecting data inline.\n\nBackends: `redux` (default) is an isolated PCSX-Redux worker, the reference oracle; it needs external/pcsx-redux built. `native` is the in-tree psx-core: deterministic, gives exact observations (write watches with writer PC, `trace` instruction traces with coverage/calls, `gpu_frame` GPU command capture, memory cards, dualshock), and its state.bin is a native state.\n\nCaveats: frames count from power-on (BIOS boot included, ~ 3-5 s before a disc's EXE runs), or from the restored state with `from_state` (states only load on the backend that wrote them, with the same BIOS and media). `replay` is a JSON file of [{\"frame\": n, \"buttons\": [\"CROSS\", ...], \"sticks\": [lx, ly, rx, ry]}] relative to the run start; sticks need backend native + pad dualshock (Redux gets an analog pad with pad dualshock too). `watch` takes [\"80012345:4\", ...] (hex ADDR[:WIDTH]). memcard1/memcard2 take a .mcd path or \"new\"; your card file is never modified, written cards go to the run directory. `trace` with trace_start/trace_range/trace_frames keeps trace.txt bounded (max_events). `fast` uses the Redux dynarec without observation. status \"partial\" with termination \"timeout\" means `timeout` (seconds, default 120) was hit. Pass return_screen=true to get the final screen as an image.").heavy().screen(),
        def::<NativeArgs>("native", "Run the native psx-core alone from power-on for `frames` frames and report where it got: final PC/instruction, CD-ROM command history, SPU state, unhandled I/O reads, a profile of the last two frames (hot PCs, polled I/O), and speed. Diagnostic tool for the emulator itself and quick boot checks; for captures prefer `run` with backend native. `break` (hex PC) stops there with an instruction tail and registers; `watch` (hex RAM word) lists the first 32 changes with the writing instruction (from `watch_after`); `dump` writes <prefix>.screen.png/.vram.png; `wav` captures SPU audio; `pad` digital|dualshock; `replay` frames count from power-on. Optional `disc`; without it the BIOS shell boots. Pass return_screen=true to get the final screen as an image.").heavy().default_timeout().screen(),
        def::<LoadsArgs>("loads", "Turn a `run` recorded with trace_cd=true into disc → RAM provenance: which ISO9660 file and offset each CD DMA transfer copied to which RAM range, merged into spans and runs and verified against the run's final ram.bin. Answers 'which file is this RAM code/data from'. `limit` caps the individual runs listed (spans are always complete; default 0). Pass `symbols` to name the PCs that issued the reads.").heavy(),
        def::<SymbolsArgs>("symbols", "Name PsyQ library functions in a PS-X EXE (file or `image.cue::/PATH`) by signature matching; needs a local signature checkout (default external/psx_psyq_signatures). Writes <out>.symbols.json (feed it to run/loads/disasm `symbols`; names are checked against RAM residency), <out>.ghidra.txt for ImportSymbolsScript.py and <out>.sym. Returns counts by library, chosen SDK version and coverage.").heavy(),
        def::<DisasmArgs>("disasm", "Disassemble MIPS R3000 code from a PS-X EXE (file or `image.cue::/PATH`) at `addr` (hex RAM address, default entry pc) for `count` instructions, with labels from `symbols` (.sym or .symbols.json). Static: only covers the EXE's own load range; for overlays loaded at run time, use a run's ram.bin.").read_only(),
        def::<LockstepArgs>("lockstep", "Emulator validation: run the native core and PCSX-Redux side by side from BIOS reset and stop at the first instruction where they disagree, with recent history. Default mode streams Redux's trace (fast, compares PC, instruction and source operands); `exact` single-steps over GDB comparing all registers (~20 steps/s, keep `steps` small). Needs external/pcsx-redux. Not needed for game RE.").heavy().default_timeout(),
        def::<TemplateCreateArgs>("template_create", "Create a standalone Rust/Bevy game project with the PS1 look (vertex snapping, affine textures, dithering, low-res view, DualShock input, scene loading from `textures` output) in `dest` (new or empty directory). Pass an absolute path or one relative to the project root."),
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
        return Err(error_value("invalid_arguments", format!("unknown argument {k:?} for {}; expected one of {}", tool.name, names.join(", "))));
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
        "run" => &v["files"]["screen.png"]["path"],
        "native" => &v["dump"]["screen"],
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
        "note": "The full result exceeded the MCP inline limit and was written to result_file (pretty JSON, identical to the CLI output). Large fields are summarised below.",
        "summary": summary,
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
            let tool = Tool::new(d.name, d.description, Arc::new(schema.as_object().unwrap().clone())).with_annotations(
                ToolAnnotations::from_raw(None, Some(d.read_only), Some(false), None, Some(false)),
            );
            tools.push(tool);
            defs.insert(d.name, d);
        }
        PsxServer { root, tools: Arc::new(tools), defs: Arc::new(defs), slots: Arc::new(Semaphore::new(jobs)) }
    }

    async fn call(&self, name: &str, args: Map<String, Value>, cancel: tokio_util::sync::CancellationToken) -> CallToolResponse {
        let tool = &self.defs[name];
        if serde_json::to_vec(&args).map(|v| v.len()).unwrap_or(0) > ARGS_LIMIT {
            return respond(error_value("limit_exceeded", "tool arguments exceed 1 MiB"), None);
        }
        let ctx = Ctx { root: self.root.clone() };
        let (args, want_screen) = match prepare(tool, args, &ctx) {
            Ok(r) => r,
            Err(e) => return respond(e, None),
        };
        let cap = args.get("timeout").and_then(Value::as_u64).map(|s| Duration::from_secs(s) + GRACE).unwrap_or(DEFAULT_CAP);
        let permit = if tool.heavy {
            tokio::select! {
                p = self.slots.clone().acquire_owned() => match p {
                    Ok(p) => Some(p),
                    Err(_) => return respond(error_value("internal", "execution queue closed"), None),
                },
                _ = cancel.cancelled() => return respond(error_value("cancelled", "cancelled while waiting for an execution slot"), None),
            }
        } else {
            None
        };
        let call = tool.call;
        let tool_name = tool.name;
        let worker = tokio::task::spawn_blocking(move || {
            let _permit = permit;
            let out = match call(args, &ctx) {
                Ok(o) => o.value,
                Err(e) => error_value("invalid_arguments", e),
            };
            let shot = want_screen.then(|| {
                screen_path(tool_name, &out).and_then(|p| {
                    let ok = std::fs::metadata(&p).is_ok_and(|m| m.len() <= IMAGE_LIMIT);
                    ok.then(|| std::fs::read(&p).ok()).flatten()
                })
            });
            (spill(tool_name, out, &ctx), shot)
        });
        tokio::select! {
            r = tokio::time::timeout(cap, worker) => match r {
                Ok(Ok((value, shot))) => respond(value, shot),
                Ok(Err(e)) => respond(error_value("internal", format!("{name} failed: {e}")), None),
                Err(_) => respond(error_value("timeout", format!("{name} exceeded {} s; it keeps running in the background and may still write its outputs", cap.as_secs())), None),
            },
            _ = cancel.cancelled() => respond(error_value("cancelled", format!("{name} was cancelled; the job keeps running in the background until its own timeout")), None),
        }
    }
}

impl ServerHandler for PsxServer {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new("psx", env!("CARGO_PKG_VERSION")))
            .with_instructions(format!(
                "PlayStation (PS1) reverse-engineering toolkit. Each tool is one `psx` CLI command and returns exactly the JSON the CLI prints. \
                 Project root: {root} (bios/, media/, external/ and .psx/ outputs live here). Relative paths resolve against it; absolute paths also work. \
                 Typical flow: disc -> ls / exe (`image.cue::/PATH`) -> symbols -> run (frames from power-on; trace_cd, watch, trace, gpu_frame) -> loads / textures / disasm. \
                 Emulation outputs (RAM, VRAM, traces, PNGs) are files in the returned run_dir; read them from disk instead of expecting them inline. \
                 Backend native gives exact observations and native states; redux is the PCSX-Redux oracle. Results over 256 KiB come back as a result_file path.",
                root = self.root.display()
            ))
    }

    fn get_tool(&self, name: &str) -> Option<Tool> {
        self.tools.iter().find(|t| t.name == name).cloned()
    }

    async fn list_tools(&self, request: Option<PaginatedRequestParams>, _: RequestContext<RoleServer>) -> Result<ListToolsResult, McpError> {
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
    fn poll_read(self: std::pin::Pin<&mut Self>, cx: &mut std::task::Context<'_>, buffer: &mut tokio::io::ReadBuf<'_>) -> std::task::Poll<std::io::Result<()>> {
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
