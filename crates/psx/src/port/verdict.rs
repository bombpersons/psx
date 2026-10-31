//! Verdicts: what `port check` found for a feature's recordings, and the
//! commit it judged. A verdict counts only while that commit is the
//! workspace's HEAD and the check ran with no uncommitted changes, where
//! notes and reviews do not count (they change nothing a check runs). psx
//! writes them; nobody else does. They live in
//! `.psx/verdicts/F-NNNN/scenario.json`, outside the commit, so recording
//! one does not make it stale.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// `.psx/verdicts/F-NNNN/scenario.json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Verdict {
    pub feature: String,
    /// The HEAD the check ran at.
    pub commit: String,
    /// Whether the workspace had no uncommitted changes then.
    pub clean: bool,
    pub at: String,
    /// Why no recording could run (the test did not build).
    #[serde(default)]
    pub failures: Vec<String>,
    /// Each recording the entry names (`<scenario>/<name>`), with the
    /// first tick each quantity differs at, or why it did not run; empty
    /// when it passed.
    pub recordings: BTreeMap<String, Vec<String>>,
}

impl Verdict {
    /// The recording's failures; one saying so when the check did not run it.
    pub fn of(&self, recording: &str) -> Vec<String> {
        if !self.failures.is_empty() {
            return self.failures.clone();
        }
        self.recordings.get(recording).cloned().unwrap_or_else(|| vec!["not run by the last psx port check".into()])
    }
}

/// A verdict as it stands at the workspace's HEAD.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum State {
    Missing,
    /// Made at another commit, or with uncommitted changes.
    Stale {
        commit: String,
    },
    Current(Verdict),
}

impl State {
    pub fn name(&self) -> &'static str {
        match self {
            State::Missing => "missing",
            State::Stale { .. } => "stale",
            State::Current(_) => "current",
        }
    }
}

pub fn path(root: &Path, feature: &str) -> PathBuf {
    root.join(".psx/verdicts").join(feature).join("scenario.json")
}

/// The feature's verdict at `head` (None: no commit yet). One written by an
/// older psx reads as missing.
pub fn state(root: &Path, feature: &str, head: Option<&str>) -> State {
    let Some(v) = std::fs::read(path(root, feature))
        .ok()
        .and_then(|b| serde_json::from_slice::<Verdict>(&b).ok())
        .filter(|v| v.feature == feature)
    else {
        return State::Missing;
    };
    if !v.clean || !judges(root, &v.commit, head) { State::Stale { commit: v.commit } } else { State::Current(v) }
}

/// Paths a check does not read: the agent's notes and the reviews.
pub fn inert(path: &str) -> bool {
    path.starts_with("notes/") || path.starts_with("reviews/")
}

/// Whether a check at `commit` judges `head`: the same commit, or one that
/// changed only paths a check does not read.
pub fn judges(root: &Path, commit: &str, head: Option<&str>) -> bool {
    let Some(head) = head else { return false };
    if commit == head {
        return true;
    }
    let out = std::process::Command::new("git").args(["diff", "--name-only", commit, head]).current_dir(root).output();
    out.ok().filter(|o| o.status.success()).is_some_and(|o| String::from_utf8_lossy(&o.stdout).lines().all(inert))
}

pub fn write(root: &Path, v: &Verdict) -> crate::Result<()> {
    let p = path(root, &v.feature);
    if let Some(d) = p.parent() {
        std::fs::create_dir_all(d).map_err(|e| crate::Error::io(d, e))?;
    }
    crate::write_atomic(&p, &serde_json::to_vec_pretty(v).unwrap_or_default()).map_err(|e| crate::Error::io(&p, e))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn notes_and_reviews_leave_a_check_judging_head() {
        let d = super::super::tests::tempdir::Dir::new();
        let root = d.0.as_path();
        let git = |args: &[&str]| {
            let o = std::process::Command::new("git").args(args).current_dir(root).output().unwrap();
            assert!(o.status.success(), "git {args:?}");
            String::from_utf8(o.stdout).unwrap().trim().to_string()
        };
        git(&["init", "-q"]);
        git(&["config", "user.email", "t@t"]);
        git(&["config", "user.name", "t"]);
        let commit = |path: &str| {
            let p = root.join(path);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(&p, path).unwrap();
            git(&["add", "."]);
            git(&["commit", "-q", "-m", path]);
            git(&["rev-parse", "HEAD"])
        };
        let checked = commit("game/src/lib.rs");
        let notes = commit("notes/feedback.md");
        assert!(judges(root, &checked, Some(&checked)));
        assert!(judges(root, &checked, Some(&notes)));
        let reviewed = commit("reviews/F-0001.md");
        assert!(judges(root, &checked, Some(&reviewed)));
        let code = commit("game/src/jump.rs");
        assert!(!judges(root, &checked, Some(&code)));
        assert!(!judges(root, &checked, None));
    }
}
