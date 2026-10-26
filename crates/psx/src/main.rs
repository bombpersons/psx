//! `psx`: PlayStation reverse-engineering toolkit.
//!
//! Every command prints one JSON document on stdout. Failures print
//! `{"error": {"code", "message"}}` and exit non-zero.

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};

use commands::{Ctx, Op};

mod artifact;
mod assets_cmd;
mod bios;
mod bound_symbols;
mod case;
mod code;
mod commands;
mod context;
mod error;
mod fieldmap;
mod ghidra;
mod guide;
mod handoff;
mod leak;
mod mcp;
mod media_cmd;
mod native;
mod page;
mod parser;
mod parser_template;
mod provenance;
mod publish;
mod ram;
mod replay;
mod run;
mod run_native;
mod stall;
mod states;
mod str_cmd;
mod symbols_io;
mod template;
mod textures;
mod tickfind;
mod until;
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
    #[command(name = "__handoff-child", hide = true)]
    HandoffChild { run_dir: PathBuf },
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
    Disc(media_cmd::DiscArgs),
    /// List ISO9660 entries with LBA, size, CD-XA attributes, filtering and pagination.
    Ls(media_cmd::LsArgs),
    /// Extract one file, or every file with --all, into a directory.
    /// Form 2 / interleaved files are written as raw 2352-byte sectors with a `.raw` suffix.
    Extract(media_cmd::ExtractArgs),
    /// Parse a PS-X EXE from a file, or from a disc with `image.cue::/PATH`.
    Exe(media_cmd::ExeArgs),
    /// List a file's XA-ADPCM audio channels (STR/XA streams), or decode one to a 44.1 kHz WAV.
    Xa(xa_cmd::XaArgs),
    /// Find and convert TIM textures, TMD models and VAB/VAG sound banks in disc files, archives or RAM dumps.
    Assets {
        #[command(subcommand)]
        action: assets_cmd::AssetsAction,
    },
    /// Decode an STR movie (MDEC BS v1/v2/v3) into PNG frames, audio.wav and summary.json.
    Str(str_cmd::StrArgs),
    /// List the saves on a raw 128 KiB memory card image (.mcd/.mcr).
    Memcard(media_cmd::MemcardArgs),
    /// Decode VRAM texture pages through their CLUTs into PNGs: every page/CLUT
    /// pair a `--gpu-frame` capture used (with the UV bounds sampled), or one page.
    Textures(textures::TexturesArgs),
    /// Describe sectors: form, XA subheader, owning file and a hex preview.
    Sector(media_cmd::SectorArgs),
    /// Create a native Bevy game starter or an external parser starter.
    Template {
        #[command(subcommand)]
        action: TemplateAction,
    },
    /// Run a disc, EXE or state for N frames on the native core and capture RAM, VRAM,
    /// screen and registers, with watches, traces, GPU captures, samples and states.
    /// Input can be inline or replayed.
    Run(run::RunCmdArgs),
    /// Ask a person to play: open a window showing the game and REQUEST, let them play with a
    /// keyboard or gamepad, and on F12 write state.bin, their input as a replay, a screenshot
    /// and their note; F6 marks moments on the way (state, RAM and screen each). --detach returns at once; poll with handoff-status.
    Handoff(handoff::HandoffArgs),
    /// Progress or result of a handoff: waiting, done, declined, closed or abandoned.
    HandoffStatus(handoff::HandoffStatusArgs),
    /// Record a case (start state, input and a field map subject in; the case's JSON,
    /// observation CSV with the tick column, and a recipe out), or check cases by
    /// recording them again.
    Case {
        #[command(subcommand)]
        action: CaseAction,
    },
    /// The barrier between investigator and implementer: check text for leaks.
    Barrier {
        #[command(subcommand)]
        action: BarrierAction,
    },
    /// Carry specs and cases to the implementer, only as the reviewer approved them.
    Publish {
        #[command(subcommand)]
        action: PublishAction,
    },
    /// Named start states, each with its chain of runs from power-on.
    States {
        #[command(subcommand)]
        action: StatesAction,
    },
    /// Decode a `--trace-cd` run into disc → RAM load runs, mapped to ISO9660 files
    /// and verified against the run's final RAM.
    Loads(provenance::LoadsArgs),
    /// Name library functions in a PS-X EXE by PsyQ signature matching.
    /// Writes <out>.symbols.json, <out>.ghidra.txt (ImportSymbolsScript.py) and <out>.sym.
    Symbols(symbols_io::SymbolsArgs),
    /// Emulator diagnostics: run the native core from power-on for N frames and report
    /// how far it got and which hardware it is missing. For game RE use `run`.
    Native(native::NativeArgs),
    /// Disassemble MIPS code from a PS-X EXE (file or `image.cue::/PATH`) or a run's RAM:
    /// an address range, or a whole function; JSON lines or `--text`.
    Disasm(code::DisasmArgs),
    /// Cross-references to an address or range: jal/j/branch targets, lui+addiu/ori/load/store
    /// pairs, gp-relative accesses and pointer words, in an EXE or a run's RAM.
    Xrefs(code::XrefsArgs),
    /// RAM snapshot analysis across runs: diff, cheat-finder search, side-by-side view.
    Ram {
        #[command(subcommand)]
        action: RamAction,
    },
    /// Replay a recorded GP0 stream (`[n, words..]` u32 records, e.g. tools/probes/gpu/gp0_stream.bin)
    /// into a fresh psx-gpu and write the resulting VRAM.
    GpuReplay(vram_diff::GpuReplayArgs),
    /// Compare two raw VRAM dumps (vram.bin) pixel by pixel.
    VramDiff(vram_diff::VramDiffArgs),
    /// Classify a CPU address (segment, region, folded offset).
    Addr(media_cmd::AddrArgs),
    /// Serve every command above as an MCP tool over stdin/stdout (start it in the project root).
    Mcp(mcp::McpArgs),
}

#[derive(Subcommand)]
enum TemplateAction {
    /// Write the Bevy game workspace (psx-bevy, engine, game, tools) into a new or game directory.
    Create(template::TemplateCreateArgs),
    /// Replace untouched psx-bevy/ and vendor/ files with this build's; report edits and seeded changes.
    Update(template::TemplateUpdateArgs),
    /// Start tracking an existing template workspace (writes .psx-template.toml).
    Adopt(template::TemplateAdoptArgs),
    /// Write a working external parser, authored fixture and protocol docs.
    Parser(parser_template::ParserTemplateArgs),
}

#[derive(Subcommand)]
enum StatesAction {
    /// Run from a named state (or power-on) and name the state it ends in.
    Make(Box<states::StatesMakeArgs>),
    /// Name a state an earlier run or handoff made; its chain comes from the runs' reports.
    Save(states::StatesSaveArgs),
    /// The named states, and whether each is present.
    List(states::StatesListArgs),
    /// Replay chains from power-on on this psx and compare the states they make.
    Rebuild(states::StatesRebuildArgs),
}

#[derive(clap::Subcommand)]
enum BarrierAction {
    /// Check files or text for what would carry the original's code across: addresses,
    /// hex, code-shaped text, tool and symbol names, text copied from the notes.
    Check(leak::BarrierCheckArgs),
    /// The check on each message to the implementer, as its UserPromptSubmit hook: the event
    /// on stdin, settings in .claude/barrier.json, every message logged to notes/guidance.jsonl.
    Hook(leak::BarrierHookArgs),
}

#[derive(clap::Subcommand)]
enum PublishAction {
    /// Check what changed in specs since the last crossing and write it as a bundle with its hash.
    Stage(publish::PublishStageArgs),
    /// Copy a bundle into the implementer's specs, only if reviews/HASH.json approves it.
    Cross(publish::PublishCrossArgs),
    /// Check that the implementer's specs are exactly the approved bundles that crossed.
    Check(publish::PublishCheckArgs),
}

#[derive(clap::Subcommand)]
enum CaseAction {
    /// Record a case: the subject's fields every field from a start state with input.
    Record(Box<case::CaseRecordArgs>),
    /// Record cases again from their recipes and compare with the published CSVs.
    Check(case::CaseCheckArgs),
}

#[derive(Subcommand)]
enum RamAction {
    /// Values that differ between two ram.bin snapshots (run directories or files).
    Diff(ram::RamDiffArgs),
    /// Cheat-finder search: addresses whose values across ordered snapshots satisfy
    /// predicates (changed/unchanged/increased/decreased/delta=N, exact values, bounds).
    Search(ram::RamSearchArgs),
    /// A memory window from one or more snapshots side by side, as u8..u32/i16/fx16.16.
    View(ram::RamViewArgs),
}

impl TemplateAction {
    fn exec(self, ctx: &Ctx) -> commands::Outcome {
        match self {
            Self::Create(a) => a.exec(ctx),
            Self::Update(a) => a.exec(ctx),
            Self::Adopt(a) => a.exec(ctx),
            Self::Parser(a) => a.exec(ctx),
        }
    }
}

impl RamAction {
    fn exec(self, ctx: &Ctx) -> commands::Outcome {
        match self {
            Self::Diff(a) => a.exec(ctx),
            Self::Search(a) => a.exec(ctx),
            Self::View(a) => a.exec(ctx),
        }
    }
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
            let message = format!("project root {} (--root / PSX_ROOT): {e}", abs.display());
            println!("{}", Error::invalid(message).to_value());
            return ExitCode::from(1);
        }
    }
    let ctx = Ctx { root: std::env::current_dir().unwrap_or_default(), cancel: None };
    if let Command::Barrier { action: BarrierAction::Hook(a) } = &cli.command {
        let mut stdin = String::new();
        let _ = std::io::Read::read_to_string(&mut std::io::stdin(), &mut stdin);
        let out = a.run(&stdin);
        print!("{}", out.stdout);
        eprint!("{}", out.stderr);
        return ExitCode::from(out.exit);
    }
    let out = match cli.command {
        Command::McpCall => mcp::project::worker(&ctx),
        Command::HandoffChild { run_dir } => handoff::child(&run_dir),
        Command::Handoff(a) => a.exec(&ctx),
        Command::HandoffStatus(a) => a.exec(&ctx),
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
        Command::Assets { action } => action.exec(&ctx),
        Command::Str(a) => a.exec(&ctx),
        Command::Memcard(a) => a.exec(&ctx),
        Command::Textures(a) => a.exec(&ctx),
        Command::Sector(a) => a.exec(&ctx),
        Command::Template { action } => action.exec(&ctx),
        Command::Run(a) => a.exec(&ctx),
        Command::Loads(a) => a.exec(&ctx),
        Command::Symbols(a) => a.exec(&ctx),
        Command::Native(a) => a.exec(&ctx),
        Command::Disasm(a) => a.exec(&ctx),
        Command::Xrefs(a) => a.exec(&ctx),
        Command::Ram { action } => action.exec(&ctx),
        Command::Case { action } => match action {
            CaseAction::Record(a) => a.exec(&ctx),
            CaseAction::Check(a) => a.exec(&ctx),
        },
        Command::Barrier { action } => match action {
            BarrierAction::Check(a) => a.exec(&ctx),
            BarrierAction::Hook(_) => unreachable!("handled above"),
        },
        Command::Publish { action } => match action {
            PublishAction::Stage(a) => a.exec(&ctx),
            PublishAction::Cross(a) => a.exec(&ctx),
            PublishAction::Check(a) => a.exec(&ctx),
        },
        Command::States { action } => match action {
            StatesAction::Make(a) => a.exec(&ctx),
            StatesAction::Save(a) => a.exec(&ctx),
            StatesAction::List(a) => a.exec(&ctx),
            StatesAction::Rebuild(a) => a.exec(&ctx),
        },
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

/// Milliseconds since the Unix epoch (0 if the clock is set before it).
pub(crate) fn now_ms() -> u128 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis()).unwrap_or(0)
}

/// A file or directory name unique to this moment and job: `<unix ms>-<pid><instance suffix>`.
pub(crate) fn stamp() -> String {
    format!("{}-{}{}", now_ms(), std::process::id(), instance_suffix())
}

/// Write `bytes` to `path` through a temporary file beside it and a rename,
/// so readers never see a partly written file.
pub(crate) fn write_atomic(path: &std::path::Path, bytes: &[u8]) -> std::io::Result<()> {
    let tmp = path.with_extension(format!("tmp-{}{}", std::process::id(), instance_suffix()));
    std::fs::write(&tmp, bytes).and_then(|()| std::fs::rename(&tmp, path)).inspect_err(|_| {
        let _ = std::fs::remove_file(&tmp);
    })
}

pub(crate) use error::{Error, Result};
pub(crate) use symbols_io::{ScopedSymbols, SymbolCheck, final_name, load_symbols, name_pc};
