//! Verdicts: what psx's checks found for a feature, and the commit they
//! judged. A verdict counts only while that commit is the workspace's HEAD
//! and the check ran with no uncommitted changes. psx writes them; nobody
//! else does. They live in `.psx/verdicts/F-NNNN/<kind>.json`, outside the
//! commit, so recording one does not make it stale.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// What a verdict judged.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Kind {
    /// The feature's scenarios compared with the original's.
    Scenario,
    /// The feature's sandbox, run headless on hand-made assets.
    Sandbox,
}

impl Kind {
    pub const ALL: [Kind; 2] = [Kind::Scenario, Kind::Sandbox];

    pub fn name(self) -> &'static str {
        match self {
            Kind::Scenario => "scenario",
            Kind::Sandbox => "sandbox",
        }
    }
}

/// `.psx/verdicts/F-NNNN/<kind>.json`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Verdict {
    pub feature: String,
    pub kind: String,
    /// The HEAD the check ran at.
    pub commit: String,
    /// Whether the workspace had no uncommitted changes then.
    pub clean: bool,
    pub at: String,
    pub passed: bool,
    /// One line per failure: the scenario, tick and quantity.
    #[serde(default)]
    pub failures: Vec<String>,
}

/// A verdict as it stands at the workspace's HEAD.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum State {
    Missing,
    /// Made at another commit, or with uncommitted changes.
    Stale {
        commit: String,
    },
    Pass,
    Fail {
        failures: Vec<String>,
    },
}

impl State {
    pub fn name(&self) -> &'static str {
        match self {
            State::Missing => "missing",
            State::Stale { .. } => "stale",
            State::Pass => "pass",
            State::Fail { .. } => "fail",
        }
    }
}

pub fn path(root: &Path, feature: &str, kind: Kind) -> PathBuf {
    root.join(".psx/verdicts").join(feature).join(format!("{}.json", kind.name()))
}

/// The feature's verdict of `kind` at `head` (None: no commit yet).
pub fn state(root: &Path, feature: &str, kind: Kind, head: Option<&str>) -> State {
    let Some(v) = std::fs::read(path(root, feature, kind))
        .ok()
        .and_then(|b| serde_json::from_slice::<Verdict>(&b).ok())
        .filter(|v| v.feature == feature && v.kind == kind.name())
    else {
        return State::Missing;
    };
    if !v.clean || Some(v.commit.as_str()) != head {
        State::Stale { commit: v.commit }
    } else if v.passed {
        State::Pass
    } else {
        State::Fail { failures: v.failures }
    }
}

#[cfg(test)]
pub fn write(root: &Path, v: &Verdict) {
    let kind = Kind::ALL.into_iter().find(|k| k.name() == v.kind).unwrap();
    let p = path(root, &v.feature, kind);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, serde_json::to_vec_pretty(v).unwrap()).unwrap();
}
