//! git, as psx reads a workspace's history and commits the person's files.

use std::path::Path;
use std::process::{Command, Output};

/// git `args` in `root`; None when git could not start.
pub fn run(root: &Path, args: &[&str]) -> Option<Output> {
    Command::new("git").args(args).current_dir(root).output().ok()
}

/// Whether git `args` in `root` succeeded.
pub fn ok(root: &Path, args: &[&str]) -> bool {
    run(root, args).is_some_and(|o| o.status.success())
}

/// What git `args` in `root` printed, when it succeeded.
pub fn text(root: &Path, args: &[&str]) -> Option<String> {
    run(root, args).filter(|o| o.status.success()).map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
}

/// The commit at HEAD: None before the first commit, or outside a
/// repository.
pub fn head(root: &Path) -> Option<String> {
    text(root, &["rev-parse", "--verify", "-q", "HEAD"]).map(|s| s.trim().to_string())
}

/// Commits `paths` (under `root`) alone, whatever else is changed or
/// staged: the person's files psx writes for them (a play, goal.toml's
/// held_out), so the agent finds them committed rather than in its way.
/// The new commit, none outside a repository or with nothing to commit,
/// or why git refused.
pub fn commit_alone(root: &Path, paths: &[&str], message: &str) -> Result<Option<String>, String> {
    let paths: Vec<&str> = paths.iter().copied().filter(|p| root.join(p).exists()).collect();
    if paths.is_empty() || !ok(root, &["rev-parse", "--is-inside-work-tree"]) {
        return Ok(None);
    }
    let run = |args: &[&str]| -> Result<Output, String> {
        // Another git (the agent's) may hold the index a moment.
        for _ in 0..10 {
            let out = Command::new("git").args(args).current_dir(root).output().map_err(|e| format!("git: {e}"))?;
            if out.status.success() || !String::from_utf8_lossy(&out.stderr).contains("index.lock") {
                return Ok(out);
            }
            std::thread::sleep(std::time::Duration::from_millis(300));
        }
        Err("git: the index stayed locked".into())
    };
    let refused = |out: &Output| String::from_utf8_lossy(&out.stderr).trim().to_string();
    let out = run(&[&["add", "--"][..], &paths].concat())?;
    if !out.status.success() {
        return Err(refused(&out));
    }
    let same = run(&[&["diff", "--cached", "--quiet", "--"][..], &paths].concat())?;
    if same.status.success() {
        return Ok(None);
    }
    let out = run(&[&["commit", "-q", "-m", message, "--only", "--"][..], &paths].concat())?;
    if !out.status.success() {
        return Err(refused(&out));
    }
    Ok(head(root))
}
