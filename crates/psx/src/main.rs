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
mod binding;
mod bios;
mod bound_symbols;
mod code;
mod commands;
mod context;
mod coverage;
mod error;
mod front_matter;
mod ghidra;
mod guide;
mod handoff;
mod inventory;
mod mcp;
mod media_cmd;
mod moved;
mod native;
mod page;
mod parser;
mod port;
mod provenance;
mod ram;
mod replay;
mod run;
mod run_native;
mod scenario;
mod stall;
mod stamp;
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
    /// Discover build, project defaults, BIOS candidates and capabilities.
    Context(context::ArgsContext),
    /// List local BIOSes or explicitly install/download a verified image.
    Bios {
        #[command(subcommand)]
        action: bios::Action,
    },
    /// Read built-in investigation workflows and tool-usage guidance.
    Guide(guide::GuideArgs),
    /// Inspect artifacts: directory inventory, paginated JSON/logs/CSV, text, hex or PNG.
    Read(artifact::ReadArgs),
    /// A disc image: its tracks and boot executable, files, sectors and executables.
    Disc {
        #[command(subcommand)]
        action: DiscAction,
    },
    /// The game's data: find and convert TIM, TMD and VAB/VAG, VRAM textures,
    /// VRAM comparison and memory cards.
    Assets {
        #[command(subcommand)]
        action: assets_cmd::AssetsAction,
    },
    /// The game's code: disassembly, references, Ghidra decompilation, library
    /// signatures, the function inventory and what runs covered.
    Code {
        #[command(subcommand)]
        action: CodeAction,
    },
    /// RAM snapshot analysis across runs: diff, cheat-finder search, side-by-side view.
    Ram {
        #[command(subcommand)]
        action: RamAction,
    },
    /// Run a disc, EXE or state for N frames on the native core and capture RAM, VRAM,
    /// screen and registers, with watches, traces, GPU captures, samples and states.
    /// Input can be inline or replayed.
    Run(run::RunCmdArgs),
    /// Named start states, each with its chain of runs from power-on.
    States {
        #[command(subcommand)]
        action: StatesAction,
    },
    /// Scenarios: record the original's quantities in them, check they are
    /// deterministic, and have a person play.
    Scenario {
        #[command(subcommand)]
        action: ScenarioAction,
    },
    /// A port: its workspace, goal and catalogue.
    Port {
        #[command(subcommand)]
        action: PortAction,
    },
    /// Ghidra's installation check, analysis, export.
    Ghidra {
        #[command(subcommand)]
        action: ghidra::Action,
    },
    /// List a file's XA-ADPCM audio channels (STR/XA streams), or decode one to a 44.1 kHz WAV.
    Xa(xa_cmd::XaArgs),
    /// Decode an STR movie (MDEC BS v1/v2/v3) into PNG frames, audio.wav and summary.json.
    Str(str_cmd::StrArgs),
    /// Decode a `--trace-cd` run into disc → RAM load runs, mapped to ISO9660 files
    /// and verified against the run's final RAM.
    Loads(provenance::LoadsArgs),
    /// Emulator diagnostics: run the native core from power-on for N frames and report
    /// how far it got and which hardware it is missing. For game RE use `run`.
    Native(native::NativeArgs),
    /// Serve every command above as an MCP tool over stdin/stdout (start it in the project root).
    Mcp(mcp::McpArgs),
}

#[derive(Subcommand)]
enum DiscAction {
    /// Tracks, volume, SYSTEM.CNF and boot executable of a disc image.
    Info(media_cmd::DiscArgs),
    /// List ISO9660 entries with LBA, size, CD-XA attributes, filtering and pagination.
    Ls(media_cmd::LsArgs),
    /// Extract one file, or every file with --all, into a directory.
    /// Form 2 / interleaved files are written as raw 2352-byte sectors with a `.raw` suffix.
    Extract(media_cmd::ExtractArgs),
    /// Describe sectors: form, XA subheader, owning file and a hex preview.
    Sector(media_cmd::SectorArgs),
    /// Parse a PS-X EXE from a file, or from a disc with `image.cue::/PATH`.
    Exe(media_cmd::ExeArgs),
}

#[derive(Subcommand)]
#[allow(clippy::large_enum_variant)] // parsed once per process
enum CodeAction {
    /// Disassemble MIPS code from a PS-X EXE (file or `image.cue::/PATH`) or a run's RAM:
    /// an address range, or a whole function; JSON lines or `--text`.
    Disasm(code::DisasmArgs),
    /// Cross-references to an address or range: jal/j/branch targets, lui+addiu/ori/load/store
    /// pairs, gp-relative accesses and pointer words, in an EXE or a run's RAM.
    Xrefs(code::XrefsArgs),
    /// A function's pseudocode and callers from Ghidra, importing and analysing its source
    /// on first use.
    Decompile(ghidra::DecompileArgs),
    /// Ghidra's functions, with optional name filtering and pagination.
    Functions(ghidra::FunctionsArgs),
    /// Apply function renames and comments from a JSON patch to a Ghidra analysis.
    Annotate(ghidra::AnnotateArgs),
    /// Name library functions in a PS-X EXE by PsyQ signature matching.
    /// Writes <out>.symbols.json, <out>.ghidra.txt (ImportSymbolsScript.py) and <out>.sym.
    Symbols(symbols_io::SymbolsArgs),
    /// Every function of every executable on the disc and of the overlays in RAM snapshots,
    /// with instruction counts and basic blocks, and the disc's file list.
    Inventory(inventory::InventoryArgs),
    /// What traced runs executed, per function of the inventory: blocks executed and not,
    /// and the runs reaching each.
    Coverage(coverage::CoverageArgs),
}

#[derive(Subcommand)]
enum StatesAction {
    /// Name a state an earlier run or handoff made; its chain comes from the runs' reports.
    Save(states::StatesSaveArgs),
    /// The named states, and whether each is present.
    List(states::StatesListArgs),
    /// Replay chains from power-on on this psx and compare the states they make.
    Rebuild(states::StatesRebuildArgs),
}

#[derive(clap::Subcommand)]
enum PortAction {
    /// Write a port's workspace: the Bevy template, the goal, the catalogue, the disc and
    /// BIOS linked in, and psx's MCP server for the agent working there. --existing adds
    /// them to a template workspace made earlier.
    Init(port::PortInitArgs),
    /// Replace untouched psx-bevy/ and vendor/ files with this build's; report edits and
    /// seeded changes.
    Update(template::TemplateUpdateArgs),
    /// Show the goal's features, their closure and any problems; with --yes, record the
    /// person's confirmation of the list in goal.toml.
    Confirm(port::PortConfirmArgs),
    /// The goal's features by stage and area, their verdicts, problems, and the item due now.
    Status(port::PortStatusArgs),
    /// The one item due now: what, for which feature, why, and the fact that will show it done.
    /// An agent item handed out three times with nothing changed is reported as stuck instead.
    Next(port::PortNextArgs),
    /// Run the features' scenarios against the port (the game's tests/scenarios.rs) and
    /// write their scenario verdicts for HEAD, which must have no uncommitted changes.
    Check(port::PortCheckArgs),
}

#[derive(clap::Subcommand)]
#[allow(clippy::large_enum_variant)] // parsed once per process
enum ScenarioAction {
    /// Run the game from the scenario's state with input and write scenarios/<ID>/<NAME>.jsonl:
    /// the compared glossary quantities at the end of every tick.
    Record(Box<scenario::ScenarioRecordArgs>),
    /// The determinism check: reach each scenario again with idle fields added before its
    /// chain's last run, record its recordings again from there, and compare.
    Check(scenario::ScenarioCheckArgs),
    /// Ask a person to play: open a window showing the game and REQUEST, let them play with a
    /// keyboard or gamepad, and on F12 write state.bin, their input as a replay, a screenshot
    /// and their note; F6 marks moments on the way (state, RAM and screen each). --detach
    /// returns at once; --status RUN_DIR reports a play's progress or result.
    Play(handoff::HandoffArgs),
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

impl DiscAction {
    fn exec(self, ctx: &Ctx) -> commands::Outcome {
        match self {
            Self::Info(a) => a.exec(ctx),
            Self::Ls(a) => a.exec(ctx),
            Self::Extract(a) => a.exec(ctx),
            Self::Sector(a) => a.exec(ctx),
            Self::Exe(a) => a.exec(ctx),
        }
    }
}

impl CodeAction {
    fn exec(self, ctx: &Ctx) -> commands::Outcome {
        match self {
            Self::Disasm(a) => a.exec(ctx),
            Self::Xrefs(a) => a.exec(ctx),
            Self::Decompile(a) => a.exec(ctx),
            Self::Functions(a) => a.exec(ctx),
            Self::Annotate(a) => a.exec(ctx),
            Self::Symbols(a) => a.exec(ctx),
            Self::Inventory(a) => a.exec(ctx),
            Self::Coverage(a) => a.exec(ctx),
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
    let out = match cli.command {
        Command::McpCall => mcp::project::worker(&ctx),
        Command::HandoffChild { run_dir } => handoff::child(&run_dir),
        Command::Guide(a) => a.exec(&ctx),
        Command::Context(a) => a.exec(&ctx),
        Command::Read(a) => a.exec(&ctx),
        Command::Bios { action } => action.exec(&ctx),
        Command::Ghidra { action } => action.exec(&ctx),
        Command::Mcp(a) => return mcp::main(ctx, a, pinned),
        Command::Disc { action } => action.exec(&ctx),
        Command::Assets { action } => action.exec(&ctx),
        Command::Code { action } => action.exec(&ctx),
        Command::Xa(a) => a.exec(&ctx),
        Command::Str(a) => a.exec(&ctx),
        Command::Run(a) => a.exec(&ctx),
        Command::Loads(a) => a.exec(&ctx),
        Command::Native(a) => a.exec(&ctx),
        Command::Ram { action } => action.exec(&ctx),
        Command::Scenario { action } => match action {
            ScenarioAction::Record(a) => a.exec(&ctx),
            ScenarioAction::Check(a) => a.exec(&ctx),
            ScenarioAction::Play(a) => a.exec(&ctx),
        },
        Command::Port { action } => match action {
            PortAction::Init(a) => a.exec(&ctx),
            PortAction::Update(a) => a.exec(&ctx),
            PortAction::Confirm(a) => a.exec(&ctx),
            PortAction::Status(a) => a.exec(&ctx),
            PortAction::Next(a) => a.exec(&ctx),
            PortAction::Check(a) => a.exec(&ctx),
        },
        Command::States { action } => match action {
            StatesAction::Save(a) => a.exec(&ctx),
            StatesAction::List(a) => a.exec(&ctx),
            StatesAction::Rebuild(a) => a.exec(&ctx),
        },
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
