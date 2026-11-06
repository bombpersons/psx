//! The port as a program: built, then started speaking the port protocol
//! (psx_scenario::protocol), for `port check` to run the scenarios against
//! and psx web to draw replays with, by the commands its `psx-port.toml`
//! names ([`super::manifest`]). The runner and the comparison stay psx's.

use std::path::Path;

use psx_scenario::protocol::Process;

use super::manifest::{self, Commands};

/// The last lines of a failed command's output that say why: its error
/// lines when it printed any, else its last line.
pub fn why(out: &std::process::Output) -> String {
    let text = String::from_utf8_lossy(&out.stderr);
    let lines: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
    let errors: Vec<&str> = lines
        .iter()
        .copied()
        .filter(|l| l.starts_with("error") || l.contains(" error:") || l.contains("panicked"))
        .take(5)
        .collect();
    if errors.is_empty() { lines.last().copied().unwrap_or("no output").to_string() } else { errors.join("; ") }
}

/// Builds the port in `root` and starts it, greeted. The error says why it
/// could not: the manifest's problem, the build's error lines, the
/// glossary's problems, or what the program said.
pub fn start(root: &Path) -> Result<Process, String> {
    let commands = Commands::read(root)?;
    if let Some(build) = &commands.build {
        let out = manifest::command(root, build, &[]).output().map_err(|e| format!("{}: {e}", build[0]))?;
        if !out.status.success() {
            return Err(format!("the port did not build: {}", why(&out)));
        }
    }
    let glossary = crate::scenario::quantities(root).map_err(|p| format!("the glossary: {}", p.join("; ")))?;
    Process::start(&commands.run, root, &glossary).map_err(|e| format!("the port did not start: {e}"))
}
