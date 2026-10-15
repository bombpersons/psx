//! `psx`: PlayStation reverse-engineering toolkit.
//!
//! Every command prints one JSON document on stdout. Failures print
//! `{"error": {"code", "message"}}` and exit non-zero.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use psx_media::disc::{sha256_bytes, sha256_file};
use psx_media::iso9660::{Entry, FileSystem};
use psx_media::system_cnf::{SystemCnf, DEFAULT_BOOT};
use psx_media::{Disc, MediaError, PsxExe};
use serde_json::{json, Value};

use commands::{Ctx, Op};

mod assets_cmd;
mod code;
mod commands;
mod ghidra;
mod lockstep;
mod mcp;
mod native;
mod provenance;
mod ram;
mod replay;
mod run;
mod run_native;
mod str_cmd;
mod template;
mod textures;
mod vram_diff;
mod xa_cmd;

#[derive(Parser)]
#[command(name = "psx", version, about = "PlayStation reverse-engineering toolkit")]
struct Cli {
    /// Project root holding bios/, media/, external/ and .psx/ (default: $PSX_ROOT, else the
    /// current directory). psx changes into it first, so relative paths in every argument
    /// resolve against it and outputs go to its .psx/.
    #[arg(long, global = true, value_name = "DIR")]
    root: Option<PathBuf>,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
#[allow(clippy::large_enum_variant)] // parsed once per process
enum Command {
    /// Ghidra analysis and decompilation; queries automatically import their source.
    Ghidra {
        #[command(subcommand)]
        action: ghidra::Action,
    },
    /// Tracks, volume, SYSTEM.CNF and boot executable of a disc image.
    Disc(commands::DiscArgs),
    /// List every ISO9660 entry with LBA, size and CD-XA attributes.
    Ls(commands::LsArgs),
    /// Extract one file, or every file with --all, into a directory.
    /// Form 2 / interleaved files are written as raw 2352-byte sectors with a `.raw` suffix.
    Extract(commands::ExtractArgs),
    /// Parse a PS-X EXE from a file, or from a disc with `image.cue::/PATH`.
    Exe(commands::ExeArgs),
    /// List a file's XA-ADPCM audio channels (STR/XA streams), or decode one to a 44.1 kHz WAV.
    Xa(commands::XaArgs),
    /// Find and convert TIM textures, TMD models and VAB/VAG sound banks in disc files, archives or RAM dumps.
    Assets {
        #[command(subcommand)]
        action: assets_cmd::AssetsAction,
    },
    /// Decode an STR movie (MDEC BS v1/v2/v3) into PNG frames, audio.wav and summary.json.
    Str(commands::StrArgs),
    /// List the saves on a raw 128 KiB memory card image (.mcd/.mcr).
    Memcard(commands::MemcardArgs),
    /// Decode VRAM texture pages through their CLUTs into PNGs: every page/CLUT
    /// pair a `--gpu-frame` capture used (with the UV bounds sampled), or one page.
    Textures(commands::TexturesArgs),
    /// Describe sectors: form, XA subheader, owning file and a hex preview.
    Sector(commands::SectorArgs),
    /// Create a standalone native Bevy game project with the PS1 look.
    Template {
        #[command(subcommand)]
        action: TemplateAction,
    },
    /// Run a disc, EXE or state for N frames and capture RAM, VRAM, screen and registers:
    /// in a PCSX-Redux worker (default) or on the native core (`--backend native`: write
    /// watches, traces, GPU frame capture, per-frame samples, pokes, screens, save states).
    Run(commands::RunCmdArgs),
    /// Decode a `--trace-cd` run into disc → RAM load runs, mapped to ISO9660 files
    /// and verified against the run's final RAM.
    Loads(commands::LoadsArgs),
    /// Name library functions in a PS-X EXE by PsyQ signature matching.
    /// Writes <out>.symbols.json, <out>.ghidra.txt (ImportSymbolsScript.py) and <out>.sym.
    Symbols(commands::SymbolsArgs),
    /// Run the native core and PCSX-Redux in lockstep from BIOS reset,
    /// comparing PC and registers after every instruction.
    Lockstep(commands::LockstepArgs),
    /// Emulator diagnostics: run the native core from power-on for N frames and report
    /// how far it got and which hardware it is missing. For game RE use `run --backend native`.
    Native(commands::NativeArgs),
    /// Disassemble MIPS code from a PS-X EXE (file or `image.cue::/PATH`) or a run's RAM:
    /// an address range, or a whole function; JSON lines or `--text`.
    Disasm(commands::DisasmArgs),
    /// Cross-references to an address or range: jal/j/branch targets, lui+addiu/ori/load/store
    /// pairs, gp-relative accesses and pointer words, in an EXE or a run's RAM.
    Xrefs(commands::XrefsArgs),
    /// RAM snapshot analysis across runs: diff, cheat-finder search, side-by-side view.
    Ram {
        #[command(subcommand)]
        action: RamAction,
    },
    /// Replay a recorded GP0 stream (`[n, words..]` u32 records, e.g. tools/probes/gpu/gp0_stream.bin)
    /// into a fresh psx-gpu and write the resulting VRAM.
    GpuReplay(commands::GpuReplayArgs),
    /// Compare two raw VRAM dumps (vram.bin) pixel by pixel.
    VramDiff(commands::VramDiffArgs),
    /// Classify a CPU address (segment, region, folded offset).
    Addr(commands::AddrArgs),
    /// Serve every command above as an MCP tool over stdin/stdout (start it in the project root).
    Mcp(mcp::McpArgs),
}

#[derive(Subcommand)]
enum TemplateAction {
    /// Write the psx-bevy starter into an empty or new directory.
    Create(commands::TemplateCreateArgs),
}

#[derive(Subcommand)]
enum RamAction {
    /// Values that differ between two ram.bin snapshots (run directories or files).
    Diff(commands::RamDiffArgs),
    /// Cheat-finder search: addresses whose values across ordered snapshots satisfy
    /// predicates (changed/unchanged/increased/decreased/delta=N, exact values, bounds).
    Search(commands::RamSearchArgs),
    /// A memory window from one or more snapshots side by side, as u8..u32/i16/fx16.16.
    View(commands::RamViewArgs),
}

/// `--root`, else `PSX_ROOT`: the directory to work in.
fn project_root(flag: Option<PathBuf>) -> Option<PathBuf> {
    flag.or_else(|| std::env::var_os("PSX_ROOT").filter(|v| !v.is_empty()).map(PathBuf::from))
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    if let Some(root) = project_root(cli.root) {
        if let Err(e) = std::env::set_current_dir(&root) {
            let abs = std::path::absolute(&root).unwrap_or(root);
            println!("{}", serde_json::json!({"error": {"code": "invalid_arguments", "message": format!("project root {} (--root / PSX_ROOT): {e}", abs.display())}}));
            return ExitCode::from(1);
        }
    }
    let ctx = Ctx { root: std::env::current_dir().unwrap_or_default(), cancel: None };
    let out = match cli.command {
        Command::Ghidra { action } => action.exec(&ctx),
        Command::Mcp(a) => return mcp::main(ctx, a),
        Command::Disc(a) => a.exec(&ctx),
        Command::Ls(a) => a.exec(&ctx),
        Command::Extract(a) => a.exec(&ctx),
        Command::Exe(a) => a.exec(&ctx),
        Command::Xa(a) => a.exec(&ctx),
        Command::Assets { action: assets_cmd::AssetsAction::Scan(a) } => a.exec(&ctx),
        Command::Assets { action: assets_cmd::AssetsAction::Extract(a) } => a.exec(&ctx),
        Command::Str(a) => a.exec(&ctx),
        Command::Memcard(a) => a.exec(&ctx),
        Command::Textures(a) => a.exec(&ctx),
        Command::Sector(a) => a.exec(&ctx),
        Command::Template { action: TemplateAction::Create(a) } => a.exec(&ctx),
        Command::Run(a) => a.exec(&ctx),
        Command::Loads(a) => a.exec(&ctx),
        Command::Symbols(a) => a.exec(&ctx),
        Command::Lockstep(a) => a.exec(&ctx),
        Command::Native(a) => a.exec(&ctx),
        Command::Disasm(a) => a.exec(&ctx),
        Command::Xrefs(a) => a.exec(&ctx),
        Command::Ram { action: RamAction::Diff(a) } => a.exec(&ctx),
        Command::Ram { action: RamAction::Search(a) } => a.exec(&ctx),
        Command::Ram { action: RamAction::View(a) } => a.exec(&ctx),
        Command::GpuReplay(a) => a.exec(&ctx),
        Command::VramDiff(a) => a.exec(&ctx),
        Command::Addr(a) => a.exec(&ctx),
    };
    println!("{}", out.render());
    ExitCode::from(out.exit)
}

/// A suffix that keeps per-process output directories distinct when one
/// process (the MCP server) runs several jobs: empty the first time, then `-1`, `-2`, ...
pub(crate) fn instance_suffix() -> String {
    static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    match N.fetch_add(1, std::sync::atomic::Ordering::Relaxed) {
        0 => String::new(),
        n => format!("-{n}"),
    }
}

type R = psx_media::Result<Value>;

fn entry_json(e: &Entry) -> Value {
    json!({
        "path": e.path,
        "lba": e.lba,
        "size": e.size,
        "sectors": e.sectors(),
        "dir": e.is_dir,
        "streamed": e.is_streamed(),
        "xa_attributes": e.xa_attributes.map(|a| format!("0x{a:04X}")),
        "recorded": e.recorded,
    })
}

fn exe_json(exe: &PsxExe, bytes: &[u8]) -> Value {
    let h = |v: u32| format!("0x{v:08X}");
    json!({
        "pc": h(exe.pc),
        "gp": h(exe.gp),
        "text": {"addr": h(exe.text_addr), "size": h(exe.text_size), "end": h(exe.text_range().end), "file_offset": "0x800"},
        "data": {"addr": h(exe.data_addr), "size": h(exe.data_size)},
        "bss": {"addr": h(exe.bss_addr), "size": h(exe.bss_size)},
        "initial_sp": exe.initial_sp().map(h),
        "marker": exe.marker,
        "region_hint": exe.region_hint(),
        "file_size": exe.file_size,
        "sha256": sha256_bytes(bytes),
        "payload_sha256": sha256_bytes(exe.payload(bytes)),
        "warnings": exe.warnings,
    })
}

fn disc_summary(image: &Path, hash: bool) -> R {
    let disc = Disc::open(image)?;
    let files: Vec<Value> = disc
        .files
        .iter()
        .map(|f| {
            let mut v = json!({"name": f.name, "bytes": f.bytes});
            if hash {
                v["sha256"] = json!(sha256_file(&f.path).unwrap_or_else(|e| format!("error: {e}")));
            }
            v
        })
        .collect();
    let audio = disc.tracks.iter().filter(|t| !t.mode.is_data()).count();
    let mut out = json!({
        "source": disc.source,
        "files": files,
        "tracks": disc.tracks,
        "track_summary": {"total": disc.tracks.len(), "data": disc.tracks.len() - audio, "audio": audio},
        "end_lba": disc.end_lba(),
        // LibCrypt subchannel data: rips lack it; supply the redump .sbi/.lsd beside the image.
        "subchannel": {
            "source": disc.subq_source,
            "overridden_sectors": disc.subq.len(),
            "first_lbas": disc.subq.keys().take(4).collect::<Vec<_>>(),
        },
    });
    let fs = match FileSystem::read(&disc) {
        Ok(fs) => fs,
        Err(e) => {
            out["filesystem_error"] = json!(e.to_string());
            return Ok(out);
        }
    };
    let files_only = fs.entries.iter().filter(|e| !e.is_dir);
    out["volume"] = json!(fs.volume);
    out["entries"] = json!({
        "files": files_only.clone().count(),
        "dirs": fs.entries.iter().filter(|e| e.is_dir).count(),
        "streamed_files": files_only.clone().filter(|e| e.is_streamed()).count(),
        "bytes": files_only.map(|e| e.size as u64).sum::<u64>(),
    });
    out["fs_warnings"] = json!(fs.warnings);

    let cnf = fs.find("/SYSTEM.CNF").map(|e| fs.read_file(&disc, e)).transpose()?.map(|b| SystemCnf::parse(&b));
    let boot = cnf.as_ref().and_then(|c| c.boot.clone()).unwrap_or_else(|| DEFAULT_BOOT.to_string());
    out["system_cnf"] = json!(cnf);
    out["boot_path"] = json!(boot);
    match fs.find(&boot) {
        Some(e) => {
            let bytes = fs.read_file(&disc, e)?;
            out["boot_exe"] = match PsxExe::parse(&bytes) {
                Ok(exe) => json!({"entry": entry_json(e), "header": exe_json(&exe, &bytes)}),
                Err(err) => json!({"entry": entry_json(e), "error": err.to_string()}),
            };
        }
        None => out["boot_exe"] = json!({"error": format!("{boot} not found in file system")}),
    }
    Ok(out)
}

fn extract(image: &Path, path: Option<&str>, all: bool, out: &Path) -> R {
    let disc = Disc::open(image)?;
    let fs = FileSystem::read(&disc)?;
    let selected: Vec<&Entry> = match (path, all) {
        (_, true) => fs.entries.iter().filter(|e| !e.is_dir).collect(),
        (Some(p), false) => vec![fs.find(p).ok_or_else(|| MediaError::NotFound(p.to_string()))?],
        (None, false) => return Err(MediaError::Malformed("give a path or --all".into())),
    };
    let mut written = Vec::new();
    let mut failed = Vec::new();
    for e in selected {
        let (bytes, raw) = match fs.read_file(&disc, e) {
            Ok(b) => (b, false),
            Err(MediaError::Unsupported(_)) => match fs.read_file_raw(&disc, e) {
                Ok(b) => (b, true),
                Err(err) => {
                    failed.push(json!({"path": e.path, "error": err.to_string()}));
                    continue;
                }
            },
            Err(err) => {
                failed.push(json!({"path": e.path, "error": err.to_string()}));
                continue;
            }
        };
        let rel = e.path.trim_start_matches('/');
        let dest = out.join(if raw { format!("{rel}.raw") } else { rel.to_string() });
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent).map_err(|err| MediaError::io(parent, err))?;
        }
        std::fs::write(&dest, &bytes).map_err(|err| MediaError::io(&dest, err))?;
        written.push(json!({
            "path": e.path, "lba": e.lba, "dest": dest, "bytes": bytes.len(),
            "format": if raw { "raw_2352" } else { "form1_user_data" },
            "sha256": sha256_bytes(&bytes),
        }));
    }
    Ok(json!({"written": written, "failed": failed}))
}

/// Symbols for one executable, with its code bytes when known so that a name
/// is only applied where that code is actually resident.
pub(crate) struct ScopedSymbols {
    pub name: String,
    pub table: psx_symbols::SymbolTable,
    /// `(text_addr, payload)` from a `.symbols.json` report; `None` for bare `.sym` files.
    pub code: Option<(u32, Vec<u8>)>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum SymbolCheck {
    /// The executable's bytes around the PC match the RAM given.
    RamMatch,
    /// Bare `.sym` file: no code to compare against.
    Unverified,
}

impl SymbolCheck {
    pub fn as_str(self) -> &'static str {
        match self {
            SymbolCheck::RamMatch => "ram_match",
            SymbolCheck::Unverified => "unverified",
        }
    }
}

/// Load `.sym` or `.symbols.json` files. Names describe one executable's code;
/// overlays and loaders can reuse the same addresses, so prefer `.symbols.json`.
pub(crate) fn load_symbols(paths: &[PathBuf]) -> psx_media::Result<Vec<ScopedSymbols>> {
    let mut out = Vec::new();
    for p in paths {
        let text = std::fs::read_to_string(p).map_err(|e| MediaError::io(p, e))?;
        let name = p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        if name.ends_with(".symbols.json") {
            let v: Value = serde_json::from_str(&text).map_err(|e| MediaError::Malformed(format!("{name}: {e}")))?;
            let source = v["source"].as_str().unwrap_or_default().to_string();
            let report: psx_symbols::MatchReportIn = serde_json::from_value(v["report"].clone())
                .map_err(|e| MediaError::Malformed(format!("{name}: {e}")))?;
            let bytes = read_source(&source)?;
            let exe = PsxExe::parse(&bytes)?;
            out.push(ScopedSymbols {
                name: source.rsplit(['/', '\\', ':']).next().unwrap_or(&source).trim_end_matches(";1").to_string(),
                table: psx_symbols::SymbolTable::from_symbols(&report.symbols),
                code: Some((exe.text_addr, exe.payload(&bytes).to_vec())),
            });
        } else {
            out.push(ScopedSymbols {
                name: name.trim_end_matches(".sym").to_string(),
                table: psx_symbols::SymbolTable::from_sym_text(&text),
                code: None,
            });
        }
    }
    Ok(out)
}

/// Name `pc` from the first executable whose code is resident there in `ram`
/// (2 MiB main RAM image). Without RAM, or for bare `.sym` files, the name is unverified.
pub(crate) fn name_pc(scopes: &[ScopedSymbols], pc: u32, ram: Option<&[u8]>) -> Option<(String, SymbolCheck, String)> {
    for s in scopes {
        let Some(n) = s.table.describe(pc, 0x4000) else { continue };
        match (&s.code, ram) {
            (Some((base, code)), Some(ram)) => {
                let off = pc.wrapping_sub(*base) as usize & !3;
                let lo = off.saturating_sub(16);
                let hi = (off + 16).min(code.len());
                let r = (*base as usize & 0x1F_FFFF) + lo;
                if lo < hi && ram.get(r..r + (hi - lo)) == Some(&code[lo..hi]) {
                    return Some((n, SymbolCheck::RamMatch, s.name.clone()));
                }
            }
            (Some(_), None) | (None, _) => return Some((n, SymbolCheck::Unverified, s.name.clone())),
        }
    }
    None
}

fn read_source(source: &str) -> psx_media::Result<Vec<u8>> {
    Ok(match source.split_once("::") {
        Some((image, path)) => {
            let disc = Disc::open(image)?;
            let fs = FileSystem::read(&disc)?;
            let e = fs.find(path).ok_or_else(|| MediaError::NotFound(path.to_string()))?;
            fs.read_file(&disc, e)?
        }
        None => std::fs::read(source).map_err(|e| MediaError::io(source, e))?,
    })
}

fn symbols_cmd(source: &str, sigs_dir: &Path, min_concrete: usize, out: Option<PathBuf>) -> R {
    let bytes = read_source(source)?;
    let exe = PsxExe::parse(&bytes)?;
    let sigs = psx_symbols::load_dir(sigs_dir).map_err(|e| {
        MediaError::NotFound(format!("signatures: {e} (supply a local psx_psyq_signatures checkout with --sigs)"))
    })?;
    let report = psx_symbols::match_code(exe.payload(&bytes), exe.text_addr, &sigs, min_concrete);
    let stem = source.rsplit(['/', '\\', ':']).next().unwrap_or("exe").trim_end_matches(";1").to_string();
    let prefix = out.unwrap_or_else(|| PathBuf::from(".psx/symbols").join(&stem));
    if let Some(parent) = prefix.parent() {
        std::fs::create_dir_all(parent).map_err(|e| MediaError::io(parent, e))?;
    }
    let with_ext = |ext: &str| PathBuf::from(format!("{}.{ext}", prefix.display()));
    let write = |p: PathBuf, text: String| std::fs::write(&p, text).map_err(|e| MediaError::io(&p, e)).map(|_| p);
    let doc = json!({
        "source": source,
        "text_addr": exe.text_addr,
        "exe_sha256": psx_media::disc::sha256_bytes(&bytes),
        "report": report,
    });
    let json_path = write(with_ext("symbols.json"), serde_json::to_string_pretty(&doc).unwrap())?;
    let ghidra = write(with_ext("ghidra.txt"), psx_symbols::to_ghidra(&report.symbols))?;
    let sym = write(with_ext("sym"), psx_symbols::to_sym(&report.symbols))?;
    let functions = report.symbols.iter().filter(|s| s.kind == psx_symbols::Kind::Function).count();
    let mut libs: std::collections::BTreeMap<String, usize> = Default::default();
    for o in &report.objects {
        let lib = o.source.split('/').nth(1).unwrap_or("?").to_string();
        *libs.entry(lib).or_default() += 1;
    }
    let matched: u64 = report.objects.iter().map(|o| o.size as u64).sum();
    Ok(json!({
        "exe": source,
        "text": {"addr": format!("0x{:08X}", exe.text_addr), "bytes": exe.text_size},
        "signatures_loaded": sigs.len(),
        "versions": report.versions.iter().take(5).collect::<Vec<_>>(),
        "chosen_version": report.chosen_version,
        "objects_matched": report.objects.len(),
        "objects_by_library": libs,
        "functions_named": functions,
        "code_covered_bytes": matched,
        "code_covered_pct": (matched as f64 * 1000.0 / exe.text_size.max(1) as f64).round() / 10.0,
        "ambiguous": report.ambiguous.len(),
        "files": {"report": json_path, "ghidra": ghidra, "sym": sym},
    }))
}

fn exe_cmd(source: &str) -> R {
    let bytes = match source.split_once("::") {
        Some((image, path)) => {
            let disc = Disc::open(image)?;
            let fs = FileSystem::read(&disc)?;
            let e = fs.find(path).ok_or_else(|| MediaError::NotFound(path.to_string()))?;
            fs.read_file(&disc, e)?
        }
        None => std::fs::read(source).map_err(|e| MediaError::io(source, e))?,
    };
    let exe = PsxExe::parse(&bytes)?;
    Ok(exe_json(&exe, &bytes))
}

fn sectors(image: &Path, lba: u32, count: u32, preview: usize) -> R {
    let disc = Disc::open(image)?;
    let fs = FileSystem::read(&disc).ok();
    let mut out = Vec::new();
    for l in lba..lba.saturating_add(count.min(256)) {
        let track = disc.track_for_lba(l).map(|t| t.number);
        let owner = fs.as_ref().and_then(|fs| fs.owner_of(l)).map(|e| e.path.clone());
        let v = match disc.read_data(l) {
            Ok(s) => json!({
                "lba": l, "track": track, "owner": owner, "form": s.form,
                "subheader": s.subheader.map(|h| json!({"file": h[0], "channel": h[1], "submode": format!("0x{:02X}", h[2]), "coding": format!("0x{:02X}", h[3])})),
                "preview": s.data.iter().take(preview).map(|b| format!("{b:02x}")).collect::<String>(),
            }),
            Err(e) => json!({"lba": l, "track": track, "owner": owner, "error": e.to_string()}),
        };
        out.push(v);
    }
    Ok(json!({"sectors": out}))
}
