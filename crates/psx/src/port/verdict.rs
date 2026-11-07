//! The verdict: what `port check` found for the recordings, and the
//! commit it judged. A verdict counts only while that commit is the
//! workspace's HEAD and the check ran with no uncommitted changes, where
//! notes and the review do not count (they change nothing a check runs). psx
//! writes it; nobody else does. It lives in `.psx/verdicts/goal/scenario.json`,
//! outside the commit, so recording it does not make it stale.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// `.psx/verdicts/goal/scenario.json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Verdict {
    /// [`super::GOAL_ID`].
    pub goal: String,
    /// The HEAD the check ran at.
    pub commit: String,
    /// Whether the workspace had no uncommitted changes then.
    pub clean: bool,
    pub at: String,
    /// Why no recording could run (the test did not build).
    #[serde(default)]
    pub failures: Vec<String>,
    /// Each recording checked (`<scenario>/<name>`), with the quantities
    /// that differ, or why it did not run; empty when it passed.
    pub recordings: BTreeMap<String, Vec<String>>,
    /// Where each failing work recording first differs, for the order
    /// port next gives them in; never shown.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub differs: BTreeMap<String, Differs>,
    /// The failing recording Fix gives: the one it gave by the verdict
    /// before while it still fails, so a fix is not left for another.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fixing: Option<String>,
    /// Where in the original to look, by failing work recording (the
    /// pointer).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub pointers: BTreeMap<String, serde_json::Value>,
}

/// Where a recording first differs: the tick, and how many quantities
/// differ.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Differs {
    pub tick: u64,
    pub quantities: usize,
}

impl Verdict {
    /// Those of `work` that fail, in the order Fix gives them: the one
    /// being fixed, then the earliest difference, the fewest quantities,
    /// by name.
    pub fn failing(&self, work: &[String]) -> Vec<String> {
        let mut out: Vec<&String> = work.iter().filter(|r| !self.of(r).is_empty()).collect();
        let key = |r: &&String| {
            let d = self.differs.get(*r);
            (
                self.fixing.as_ref() != Some(*r),
                d.map_or(u64::MAX, |d| d.tick),
                d.map_or(usize::MAX, |d| d.quantities),
                (*r).clone(),
            )
        };
        out.sort_by_key(key);
        out.into_iter().cloned().collect()
    }

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

pub fn path(root: &Path) -> PathBuf {
    root.join(".psx/verdicts").join(super::GOAL_ID).join("scenario.json")
}

/// The verdict at `head` (None: no commit yet). One written by an older psx
/// reads as missing.
pub fn state(root: &Path, head: Option<&str>) -> State {
    let Some(v) = std::fs::read(path(root))
        .ok()
        .and_then(|b| serde_json::from_slice::<Verdict>(&b).ok())
        .filter(|v| v.goal == super::GOAL_ID)
    else {
        return State::Missing;
    };
    if !v.clean || !judges(root, &v.commit, head) { State::Stale { commit: v.commit } } else { State::Current(v) }
}

/// Paths a check does not read: the agent's notes and the review.
pub fn inert(path: &str) -> bool {
    path.starts_with("notes/") || path == super::review::REVIEW
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
    let p = path(root);
    if let Some(d) = p.parent() {
        std::fs::create_dir_all(d).map_err(|e| crate::Error::io(d, e))?;
    }
    crate::write_atomic(&p, &serde_json::to_vec_pretty(v).unwrap_or_default()).map_err(|e| crate::Error::io(&p, e))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn notes_and_the_review_leave_a_check_judging_head() {
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
        let reviewed = commit("review.md");
        assert!(judges(root, &checked, Some(&reviewed)));
        let code = commit("game/src/jump.rs");
        assert!(!judges(root, &checked, Some(&code)));
        assert!(!judges(root, &checked, None));
    }
}
