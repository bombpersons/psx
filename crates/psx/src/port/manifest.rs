//! `psx-port.toml`: the commands that build the port, run it in protocol
//! mode, import the disc and test the importers. Any program can be a port;
//! a workspace without the file is a cargo one, and gets cargo's commands.
//!
//! ```toml
//! [port]
//! build = ["make", "-C", "game"]            # optional
//! run = ["game/build/game", "--psx"]         # speaks the port protocol
//! import = ["game/build/import"]             # optional; psx appends DISC OUT
//! import_test = ["make", "-C", "game", "test-import"]  # optional
//! ```
//!
//! Commands run in the workspace root. `import` is run as `import DISC OUT`
//! and `import index DISC ARCHIVE...`; `import_test` with
//! `PSX_IMPORT_REPORT` set to the file it marks checked disc files in.

use std::path::Path;

/// The manifest's name, in the workspace root.
pub const FILE: &str = "psx-port.toml";

#[derive(Clone, Debug, PartialEq, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Commands {
    /// Builds the port; nothing to build when absent.
    #[serde(default)]
    pub build: Option<Vec<String>>,
    /// Runs the port speaking the protocol on stdin and stdout.
    pub run: Vec<String>,
    /// The importer, given `DISC OUT` or `index DISC ARCHIVE...`.
    #[serde(default)]
    pub import: Option<Vec<String>>,
    /// The importers' tests.
    #[serde(default)]
    pub import_test: Option<Vec<String>>,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Manifest {
    port: Commands,
}

fn words(w: &[&str]) -> Vec<String> {
    w.iter().map(|s| s.to_string()).collect()
}

impl Commands {
    /// The commands of a cargo workspace on the base template.
    pub fn cargo() -> Commands {
        Commands {
            build: Some(words(&["cargo", "build", "--locked", "-q", "--manifest-path", "game/Cargo.toml"])),
            run: words(&["cargo", "run", "--locked", "-q", "--manifest-path", "game/Cargo.toml", "--", "--psx"]),
            import: Some(words(&["cargo", "run", "--locked", "-q", "-p", "import", "--"])),
            import_test: Some(words(&["cargo", "test", "--locked", "--no-fail-fast", "-q", "-p", "import"])),
        }
    }

    /// The workspace's commands: its `psx-port.toml`, or cargo's when it has
    /// none. The error says what is wrong with the file.
    pub fn read(root: &Path) -> Result<Commands, String> {
        let path = root.join(FILE);
        let text = match std::fs::read_to_string(&path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Commands::cargo()),
            Err(e) => return Err(format!("{FILE}: {e}")),
        };
        let m: Manifest = toml::from_str(&text).map_err(|e| format!("{FILE}: {}", e.message()))?;
        let empty = |c: &Option<Vec<String>>| c.as_ref().is_some_and(Vec::is_empty);
        if m.port.run.is_empty() || empty(&m.port.build) || empty(&m.port.import) || empty(&m.port.import_test) {
            return Err(format!("{FILE}: a command is a program and its arguments, not empty"));
        }
        let mut c = m.port;
        // A program named by a path is in the workspace, whichever directory
        // psx runs in.
        for cmd in [Some(&mut c.run), c.build.as_mut(), c.import.as_mut(), c.import_test.as_mut()].into_iter().flatten()
        {
            if cmd[0].contains('/') && Path::new(&cmd[0]).is_relative() {
                cmd[0] = root.join(&cmd[0]).to_string_lossy().into_owned();
            }
        }
        Ok(c)
    }
}

/// `command` with `args` after it, run in `root`, as a process ready to
/// spawn.
pub fn command(root: &Path, command: &[String], args: &[&str]) -> std::process::Command {
    let mut c = std::process::Command::new(&command[0]);
    c.args(&command[1..]).args(args).current_dir(root).stdin(std::process::Stdio::null());
    c
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_workspace_without_a_manifest_is_a_cargo_one() {
        let dir = crate::port::tests::tempdir::Dir::new();
        assert_eq!(Commands::read(dir.0.as_path()).unwrap(), Commands::cargo());
    }

    #[test]
    fn the_manifest_names_the_commands() {
        let dir = crate::port::tests::tempdir::Dir::new();
        std::fs::write(dir.0.as_path().join(FILE), "[port]\nrun = [\"game/game\", \"--psx\"]\nbuild = [\"make\"]\n")
            .unwrap();
        let c = Commands::read(dir.0.as_path()).unwrap();
        assert_eq!(c.run, [dir.0.join("game/game").to_string_lossy().as_ref(), "--psx"]);
        assert_eq!(c.build.unwrap(), ["make"]);
        assert_eq!((c.import, c.import_test), (None, None));
        std::fs::write(dir.0.as_path().join(FILE), "[port]\nrun = []\n").unwrap();
        assert!(Commands::read(dir.0.as_path()).unwrap_err().contains("not empty"));
        std::fs::write(dir.0.as_path().join(FILE), "[port]\nrun = [\"a\"]\ntest = [\"b\"]\n").unwrap();
        assert!(Commands::read(dir.0.as_path()).unwrap_err().contains("unknown field"));
    }
}
