//! `psx`: PlayStation reverse-engineering toolkit.
//!
//! Every command prints one JSON document on stdout. Failures print
//! `{"error": {"code", "message"}}` and exit non-zero.

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{CommandFactory, FromArgMatches, Parser, Subcommand};

use commands::{Ctx, Op};

mod adjust;
mod artifact;
mod assets_cmd;
mod binding;
mod bios;
mod calls;
mod code;
mod commands;
mod context;

mod coverage;
mod flow;
mod decomp;
mod error;
mod ghidra;
mod guide;
mod handoff;
mod inventory;
mod live;
mod mcp;
mod media_cmd;
mod messages;
mod moved;
mod names_bind;
mod names_cmd;
mod native;
mod pad_reads;
mod page;
mod parser;
mod play;
mod port;
mod provenance;
mod ram;
mod replay;
mod reports;
mod run;
mod run_native;
mod scenario;
mod script_cover;
mod stall;
mod stamp;
mod states;
mod stats;
mod str_cmd;
mod symbols_io;
mod disc_names;
mod template;
mod textures;
mod tickfind;
mod until;
mod vram_diff;
mod web;
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
    #[command(name = "__web-replay", hide = true)]
    WebReplay {
        id: String,
        name: String,
        dir: PathBuf,
        #[arg(long)]
        decomp: bool,
    },
    /// Discover build, project defaults, BIOS candidates and downloads, Ghidra and capabilities.
    Context(context::ArgsContext),
    /// Install a BIOS: a local image, or an explicit download of a verified one.
    Bios {
        #[command(subcommand)]
        action: bios::Action,
    },
    /// Read built-in investigation workflows and tool-usage guidance.
    Guide(guide::GuideArgs),
    /// Messages with the person, who reads and writes them in psx web: send one
    /// (a new thread, or a reply), or read the latest threads.
    Message(messages::MessageArgs),
    /// A reviewer's report for the person alone, shown in psx web.
    Report(reports::ReportArgs),
    /// Inspect artifacts: directory inventory, paginated JSON/logs/CSV, text, hex or PNG.
    Read(artifact::ReadArgs),
    /// A disc image: its tracks and boot executable, files, sectors and executables.
    Disc {
        #[command(subcommand)]
        action: DiscAction,
    },
    /// The game's data: find and convert TIM, TMD, VAB/VAG, XA audio and STR movies,
    /// VRAM textures, VRAM comparison and memory cards.
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
    /// Serve every command above as an MCP tool over stdin/stdout (start it in the project root).
    Mcp(mcp::McpArgs),
    /// Serve a read-only page on localhost: what needs a person, each port's
    /// progress over time, every psx call, and recordings replayed with their quantities.
    Web(web::WebArgs),
    /// For the person: play the agents' play requests (scenario play) in a window, oldest
    /// first, across the projects psx web shows; --list lists them, --decline answers one
    /// with a note for the agent instead.
    Play(play::PlayArgs),
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
    /// Name library functions in a PS-X EXE by PsyQ signature matching.
    /// Writes <out>.symbols.json, <out>.ghidra.txt (ImportSymbolsScript.py) and <out>.sym.
    Symbols(symbols_io::SymbolsArgs),
    /// Every function of every executable on the disc and of the overlays in RAM snapshots,
    /// with instruction counts and basic blocks, and the disc's file list.
    Inventory(inventory::InventoryArgs),
    /// What traced runs executed, per function of the inventory: blocks executed and not,
    /// and the runs reaching each. `coverage diff`: what some runs reach that others do not.
    Coverage(CoverageCmd),
    /// Check the agent's C for functions (decomp/) against the original: build it with the
    /// names' header, replay the recordings that reach each (or run from a state), and compare
    /// each call's effects, also with its arguments varied.
    Check(decomp::CodeCheckArgs),
    /// Where a name (function, global, type, field, local) or address is defined in the names
    /// files and what it resolves to; with no query, the files' problems and what the functions
    /// the recordings run most leave unnamed.
    Names(names_cmd::CodeNamesArgs),
}

#[derive(clap::Args)]
#[command(args_conflicts_with_subcommands = true, subcommand_negates_reqs = true)]
struct CoverageCmd {
    #[command(subcommand)]
    action: Option<CoverageAction>,
    #[command(flatten)]
    args: coverage::CoverageArgs,
}

#[derive(Subcommand)]
enum CoverageAction {
    /// What traced runs reach that other traced runs (--against) do not: functions only
    /// they executed, and in functions both reached, the blocks and branch ways only they took.
    Diff(coverage::CoverageDiffArgs),
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
    /// Write a port's workspace: the template, the goal, the catalogue, the disc and
    /// BIOS linked in, and psx's MCP server for the agent working there. --existing adds
    /// them to a template workspace made earlier.
    Init(port::PortInitArgs),
    /// Replace untouched vendor/ files with this build's; report edits and
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
    /// the compared glossary quantities at the end of every tick; and beside it <NAME>.coverage.json,
    /// what the original executed as the inventory's code, which code coverage reads.
    Record(Box<scenario::ScenarioRecordArgs>),
    /// The determinism check: reach each scenario again with idle fields added before its
    /// chain's last run, record its recordings again from there, and compare.
    Check(scenario::ScenarioCheckArgs),
    /// Measure the adjustments psx makes to the original (docs/master-plan.md): record each
    /// scenario's recordings again from its state, stock and adjusted, driven by the same
    /// ticks, and report each quantity's first tick where they differ.
    AdjustCheck(scenario::ScenarioAdjustCheckArgs),
    /// A recording's quantities per tick, with each numeric one's change since the tick before
    /// (a fixed as its exact decimal unless --raw); --text for an aligned table.
    Show(scenario::ScenarioShowArgs),
    /// Ask a person to play: queue REQUEST for them and return a ticket at once. They answer
    /// from psx web or psx play: a window shows the game and REQUEST, they play with a keyboard
    /// or gamepad, and on F12 it writes state.bin, their input as a replay, a screenshot and
    /// their note (F10 declines, with a note); F6 marks moments on the way. With a scenario ID,
    /// play starts at its state (or the end of --from, one of its recordings) and is written as
    /// its recording NAME, as scenario record writes one. --status RUN_DIR reports where a request
    /// stands and the answer.
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
            Self::Check(a) => a.exec(ctx),
            Self::Names(a) => a.exec(ctx),
            Self::Decompile(a) => a.exec(ctx),
            Self::Functions(a) => a.exec(ctx),
            Self::Symbols(a) => a.exec(ctx),
            Self::Inventory(a) => a.exec(ctx),
            Self::Coverage(CoverageCmd { action: Some(CoverageAction::Diff(a)), .. }) => a.exec(ctx),
            Self::Coverage(c) => c.args.exec(ctx),
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
    let matches = Cli::command().get_matches();
    let cli = Cli::from_arg_matches(&matches).unwrap_or_else(|e| e.exit());
    // The subcommands as MCP names the tool: `scenario record` is scenario_record.
    let mut tool = Vec::new();
    let mut m = &matches;
    while let Some((name, sub)) = m.subcommand() {
        tool.push(name.replace('-', "_"));
        m = sub;
    }
    let tool = tool.join("_");
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
    // An MCP server logs its own calls (its project workers' included); a
    // handoff's child is part of the play that started it; web only reads.
    let logged = !matches!(
        cli.command,
        Command::McpCall
            | Command::Mcp(_)
            | Command::HandoffChild { .. }
            | Command::Web(_)
            | Command::WebReplay { .. }
            | Command::Play(_)
    );
    let call = logged
        .then(|| {
            calls::start(
                &ctx.root,
                &tool,
                serde_json::json!(std::env::args().skip(1).collect::<Vec<_>>()),
                calls::caller("cli", None),
            )
        })
        .flatten();
    // The screen of a game this call runs, for psx web (a worker's from its MCP server).
    let _live =
        call.as_ref().map(|c| c.live_path()).or_else(|| std::env::var_os(live::ENV).map(Into::into)).map(live::publish);
    let out = match cli.command {
        Command::McpCall => mcp::project::worker(&ctx),
        Command::HandoffChild { run_dir } => handoff::child(&ctx, &run_dir),
        Command::WebReplay { id, name, dir, decomp } => web::replay_child(&ctx, &id, &name, &dir, decomp),
        Command::Guide(a) => a.exec(&ctx),
        Command::Message(a) => a.exec(&ctx),
        Command::Report(a) => a.exec(&ctx),
        Command::Context(a) => a.exec(&ctx),
        Command::Read(a) => a.exec(&ctx),
        Command::Bios { action } => action.exec(&ctx),
        Command::Mcp(a) => return mcp::main(ctx, a, pinned),
        Command::Web(a) => return web::main(a),
        Command::Play(a) => return play::main(a, &ctx.root),
        Command::Disc { action } => action.exec(&ctx),
        Command::Assets { action } => action.exec(&ctx),
        Command::Code { action } => action.exec(&ctx),
        Command::Run(a) => a.exec(&ctx),
        Command::Ram { action } => action.exec(&ctx),
        Command::Scenario { action } => match action {
            ScenarioAction::Record(a) => a.exec(&ctx),
            ScenarioAction::Check(a) => a.exec(&ctx),
            ScenarioAction::AdjustCheck(a) => a.exec(&ctx),
            ScenarioAction::Show(a) => a.exec(&ctx),
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
    let mut out = out.with_path_context(&ctx);
    // The person's messages ride on whatever the agent called, as over MCP.
    if logged && tool != "message" && out.value.is_object() {
        if let Some(m) = messages::deliver(&ctx.root) {
            if out.text {
                let mut t = out.value["text"].as_str().unwrap_or_default().trim_end().to_string();
                t.push_str("\n\nFrom the person (in psx web; reply now in a line or two with psx message --text ... --reply-to ID, then go on):");
                for x in m["from_person"].as_array().into_iter().flatten() {
                    t.push_str(&format!(
                        "\n  [{}] {}",
                        x["id"].as_str().unwrap_or(""),
                        x["text"].as_str().unwrap_or("")
                    ));
                }
                out.value["text"] = serde_json::json!(t);
            }
            out.value["messages"] = m;
        }
    }
    if let Some(call) = call {
        call.end(&out.value, out.exit);
    }
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

/// `secs` from now. A timeout argument can be any u64, and `Instant + Duration`
/// panics past what the clock can hold: such a deadline is a century away.
pub(crate) fn deadline_in(secs: u64) -> std::time::Instant {
    let now = std::time::Instant::now();
    let far = std::time::Duration::from_secs(100 * 365 * 24 * 3600);
    now.checked_add(std::time::Duration::from_secs(secs).min(far)).unwrap_or(now)
}

#[cfg(test)]
#[test]
fn any_timeout_gives_a_deadline() {
    assert!(deadline_in(u64::MAX) > std::time::Instant::now());
}

/// Milliseconds since the Unix epoch (0 if the clock is set before it).
pub(crate) fn now_ms() -> u128 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis()).unwrap_or(0)
}

/// A file or directory name unique to this moment and job: `<unix ms>-<pid><instance suffix>`.
pub(crate) fn stamp() -> String {
    format!("{}-{}{}", now_ms(), std::process::id(), instance_suffix())
}

/// This running psx, for the processes it starts (project workers, the
/// handoff window). On Linux, /proc/self/exe: after a rebuild, current_exe
/// names a deleted file, which /proc/self/exe still opens.
pub(crate) fn self_exe() -> std::io::Result<std::path::PathBuf> {
    if cfg!(target_os = "linux") { Ok("/proc/self/exe".into()) } else { std::env::current_exe() }
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
