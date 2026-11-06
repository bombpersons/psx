//! The port as a program: built, then started speaking the port protocol
//! (`--psx`, psx_scenario::protocol), for `port check` to run the scenarios
//! against and psx web to draw replays with. The runner and the comparison
//! stay psx's.

use std::path::Path;

use psx_scenario::protocol::Process;

/// Builds the port.
const BUILD: &[&str] = &["cargo", "build", "--locked", "-q", "--manifest-path", "game/Cargo.toml"];

/// Runs the port in protocol mode.
const RUN: &[&str] = &["cargo", "run", "--locked", "-q", "--manifest-path", "game/Cargo.toml", "--", "--psx"];

/// Builds the port in `root` and starts it, greeted. The error says why it
/// could not: the build's error lines, the glossary's problems, or what the
/// program said.
pub fn start(root: &Path) -> Result<Process, String> {
    let out = std::process::Command::new(BUILD[0])
        .args(&BUILD[1..])
        .current_dir(root)
        .stdin(std::process::Stdio::null())
        .output()
        .map_err(|e| format!("{}: {e}", BUILD[0]))?;
    if !out.status.success() {
        let text = String::from_utf8_lossy(&out.stderr);
        let lines: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
        let errors: Vec<&str> = lines.iter().copied().filter(|l| l.starts_with("error")).take(5).collect();
        let why = if errors.is_empty() {
            lines.last().copied().unwrap_or("no output").to_string()
        } else {
            errors.join("; ")
        };
        return Err(format!("the port did not build: {why}"));
    }
    let glossary = crate::scenario::glossary(root).map_err(|p| format!("the glossary: {}", p.join("; ")))?;
    let run: Vec<String> = RUN.iter().map(|a| a.to_string()).collect();
    Process::start(&run, root, &glossary).map_err(|e| format!("the port did not start: {e}"))
}
