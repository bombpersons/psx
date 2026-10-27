//! psx-clean: clean mode, the implementer's view of the original.
//!
//! It runs the original from what the investigator published and answers
//! only in published names: a start state by the name `specs/states/` gives
//! it, samples by the names the glossary and the crossed cases use. It has
//! no command that shows code, memory, addresses, traces or states, so the
//! implementer's access rules can allow this server and deny psx. It calls
//! psx itself (beside this binary) on the investigator's repository, keeps
//! its working files there, and never passes psx's output or errors on.
//! Every request goes into the implementer's `notes/oracle.jsonl`, each entry
//! carrying the sha256 of the one before, which `psx audit` checks.

mod clean;
mod mcp;

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "psx-clean", version, about = "Clean mode: the original's behaviour in published names only")]
struct Cli {
    /// The investigator's repository (its protocol.toml names the implementer's).
    #[arg(long, global = true, value_name = "DIR", env = "PSX_CLEAN_INVESTIGATOR")]
    investigator: Option<PathBuf>,
    /// The psx binary (default: the one beside this binary).
    #[arg(long, global = true, value_name = "FILE")]
    psx: Option<PathBuf>,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Record an exploratory case from a published start: notes/oracle/NAME.json and .csv.
    Record(clean::RecordArgs),
    /// Compare the port's run of a case with the original's: the first row and column that differ.
    Compare(clean::CompareArgs),
    /// Run the port's case runner on the cases kept back, in a sandbox: pass or fail per case.
    Heldout(clean::HeldoutArgs),
    /// The original's pictures from a published start, beside the port's.
    Screens(clean::ScreensArgs),
    /// What the implementer does now (notes/NEXT.md, brought up to date).
    Next,
    /// Update this repository's psx-bevy template code to this psx (psx template update).
    TemplateUpdate(clean::TemplateUpdateArgs),
    /// Serve these as MCP tools over stdio.
    Mcp,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    if matches!(cli.command, Command::Mcp) {
        if let Some(code) = mcp::bridge() {
            return code;
        }
    }
    let clean = match clean::Clean::open(cli.investigator, cli.psx) {
        Ok(c) => c,
        Err(e) => {
            println!("{}", serde_json::json!({"error": e}));
            return ExitCode::FAILURE;
        }
    };
    let result = match cli.command {
        Command::Mcp => return mcp::serve(&clean),
        Command::Record(a) => clean.call("record", serde_json::to_value(a).unwrap_or_default()),
        Command::Compare(a) => clean.call("compare", serde_json::to_value(a).unwrap_or_default()),
        Command::Heldout(a) => clean.call("heldout", serde_json::to_value(a).unwrap_or_default()),
        Command::Screens(a) => clean.call("screens", serde_json::to_value(a).unwrap_or_default()),
        Command::Next => clean.call("next", serde_json::json!({})),
        Command::TemplateUpdate(a) => clean.call("template_update", serde_json::to_value(a).unwrap_or_default()),
    };
    match result {
        Ok(v) => {
            println!("{}", serde_json::to_string_pretty(&v).unwrap_or_default());
            ExitCode::SUCCESS
        }
        Err(e) => {
            println!("{}", serde_json::json!({"error": e}));
            ExitCode::FAILURE
        }
    }
}
