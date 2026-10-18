//! `psx`: PlayStation reverse-engineering toolkit.
//!
//! Every command prints one JSON document on stdout. Failures print
//! `{"error": {"code", "message"}}` and exit non-zero.

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};

use commands::{Ctx, Op};

mod assets_cmd;
mod bios;
mod context;
mod artifact;
mod page;
mod bound_symbols;
mod code;
mod commands;
mod ghidra;
mod guide;
mod mcp;
mod media_cmd;
mod native;
mod parser;
mod parser_template;
mod provenance;
mod ram;
mod replay;
mod run;
mod run_native;
mod str_cmd;
mod symbols_io;
mod template;
mod textures;
mod vram_diff;
mod xa_cmd;

#[derive(Parser)]
#[command(name = "psx", version = concat!(env!("CARGO_PKG_VERSION"),"+",env!("PSX_BUILD_ID")), about = "PlayStation reverse-engineering toolkit")]
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
    #[command(name = "__mcp-call", hide = true)]
    McpCall,
    /// Read built-in investigation workflows and tool-usage guidance.
    Guide(guide::GuideArgs),
    /// Discover build, project defaults, BIOS candidates and capabilities.
    Context(context::ArgsContext),
    /// Inspect artifacts: directory inventory, paginated JSON/logs/CSV, text, hex or PNG.
    ArtifactRead(artifact::ReadArgs),
    /// List local BIOSes or explicitly install/download a verified image.
    Bios {
        #[command(subcommand)]
        action: bios::Action,
    },
    /// Ghidra analysis and decompilation; queries automatically import their source.
    Ghidra {
        #[command(subcommand)]
        action: ghidra::Action,
    },
    /// Tracks, volume, SYSTEM.CNF and boot executable of a disc image.
    Disc(commands::DiscArgs),
    /// List ISO9660 entries with LBA, size, CD-XA attributes, filtering and pagination.
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
    /// Create a native Bevy game starter or an external parser starter.
    Template {
        #[command(subcommand)]
        action: TemplateAction,
    },
    /// Run a disc, EXE or state for N frames on the native core and capture RAM, VRAM,
    /// screen and registers, with watches, traces, GPU captures, samples and states.
    /// Input can be inline or replayed.
    Run(commands::RunCmdArgs),
    /// Decode a `--trace-cd` run into disc → RAM load runs, mapped to ISO9660 files
    /// and verified against the run's final RAM.
    Loads(commands::LoadsArgs),
    /// Name library functions in a PS-X EXE by PsyQ signature matching.
    /// Writes <out>.symbols.json, <out>.ghidra.txt (ImportSymbolsScript.py) and <out>.sym.
    Symbols(commands::SymbolsArgs),
    /// Emulator diagnostics: run the native core from power-on for N frames and report
    /// how far it got and which hardware it is missing. For game RE use `run`.
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
    /// Write the Bevy game workspace (psx-bevy, engine, game, tools) into a new or game directory.
    Create(commands::TemplateCreateArgs),
    /// Replace untouched psx-bevy/ files with this build's template; report edits and seeded changes.
    Update(commands::TemplateUpdateArgs),
    /// Start tracking an existing project's psx-bevy/ crate (writes .psx-template.toml).
    Adopt(commands::TemplateAdoptArgs),
    /// Write a working external parser, authored fixture and protocol docs.
    Parser(commands::ParserTemplateArgs),
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
    let pinned_root = project_root(cli.root);
    let pinned = pinned_root.is_some();
    if let Some(root) = pinned_root {
        if let Err(e) = std::env::set_current_dir(&root) {
            let abs = std::path::absolute(&root).unwrap_or(root);
            println!("{}", serde_json::json!({"error": {"code": "invalid_arguments", "message": format!("project root {} (--root / PSX_ROOT): {e}", abs.display())}}));
            return ExitCode::from(1);
        }
    }
    let ctx = Ctx { root: std::env::current_dir().unwrap_or_default(), cancel: None };
    let out = match cli.command {
        Command::McpCall => mcp::project::worker(&ctx),
        Command::Guide(a) => a.exec(&ctx),
        Command::Context(a) => a.exec(&ctx),
        Command::ArtifactRead(a) => a.exec(&ctx),
        Command::Bios { action } => action.exec(&ctx),
        Command::Ghidra { action } => action.exec(&ctx),
        Command::Mcp(a) => return mcp::main(ctx, a, pinned),
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
        Command::Template { action: TemplateAction::Update(a) } => a.exec(&ctx),
        Command::Template { action: TemplateAction::Adopt(a) } => a.exec(&ctx),
        Command::Template { action: TemplateAction::Parser(a) } => a.exec(&ctx),
        Command::Run(a) => a.exec(&ctx),
        Command::Loads(a) => a.exec(&ctx),
        Command::Symbols(a) => a.exec(&ctx),
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
    let out = out.with_path_context(&ctx);
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

pub(crate) use symbols_io::{final_name, load_symbols, name_pc, ScopedSymbols, SymbolCheck};
