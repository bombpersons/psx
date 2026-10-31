//! Reviews: `reviews/<id>.md`, findings on a feature's code by someone who
//! did not write it (docs/port-design.md, The review). TOML front matter
//! between `+++` lines names the commit reviewed and its files; each
//! finding is a `## ` section, answered by a `**Answer:**` line. A review
//! holds while its files are unchanged since its commit and every finding
//! has an answer.

use std::path::Path;

use serde::Deserialize;

use super::split_front;

pub const REVIEWS: &str = "reviews";

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Front {
    commit: String,
    files: Vec<String>,
}

/// A feature's review as it stands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Review {
    Missing,
    /// The file is not a review: why.
    Invalid(String),
    /// Its files changed since its commit: another review is due.
    Stale {
        commit: String,
        changed: Vec<String>,
    },
    /// Findings without an answer, by heading.
    Open {
        unanswered: Vec<String>,
    },
    Holds {
        commit: String,
        findings: usize,
    },
}

impl Review {
    pub fn name(&self) -> &'static str {
        match self {
            Review::Missing => "missing",
            Review::Invalid(_) => "invalid",
            Review::Stale { .. } => "stale",
            Review::Open { .. } => "open",
            Review::Holds { .. } => "holds",
        }
    }

    pub fn holds(&self) -> bool {
        matches!(self, Review::Holds { .. })
    }
}

/// The findings' headings, each with whether it has an answer.
fn findings(body: &str) -> Vec<(String, bool)> {
    let mut out: Vec<(String, bool)> = Vec::new();
    for line in body.lines() {
        if let Some(h) = line.strip_prefix("## ") {
            out.push((h.trim().to_string(), false));
        } else if let (Some(last), Some(a)) = (out.last_mut(), line.trim_start().strip_prefix("**Answer:**")) {
            last.1 |= !a.trim().is_empty();
        }
    }
    out
}

fn git(root: &Path, args: &[&str]) -> Option<std::process::Output> {
    std::process::Command::new("git").args(args).current_dir(root).output().ok()
}

/// The review of feature `id` in the workspace `root`.
pub fn read(root: &Path, id: &str) -> Review {
    let Ok(text) = std::fs::read_to_string(root.join(REVIEWS).join(format!("{id}.md"))) else {
        return Review::Missing;
    };
    let (front, body) = match split_front(&text) {
        Ok(f) => f,
        Err(e) => return Review::Invalid(e),
    };
    let f: Front = match toml::from_str(&front) {
        Ok(f) => f,
        Err(e) => return Review::Invalid(format!("front matter: {}", e.message())),
    };
    if f.files.is_empty() {
        return Review::Invalid("files: the files reviewed, none listed".into());
    }
    let known = git(root, &["rev-parse", "--verify", "-q", &format!("{}^{{commit}}", f.commit)])
        .is_some_and(|o| o.status.success());
    if !known {
        return Review::Invalid(format!("commit {:?}: not a commit of this repository", f.commit));
    }
    // Against the working tree: uncommitted changes count as changes.
    let mut args = vec!["diff", "--name-only", f.commit.as_str(), "--"];
    args.extend(f.files.iter().map(String::as_str));
    let changed: Vec<String> = git(root, &args)
        .map(|o| String::from_utf8_lossy(&o.stdout).lines().map(str::to_string).collect())
        .unwrap_or_default();
    if !changed.is_empty() {
        return Review::Stale { commit: f.commit, changed };
    }
    let all = findings(&body);
    let unanswered: Vec<String> = all.iter().filter(|(_, a)| !a).map(|(h, _)| h.clone()).collect();
    if !unanswered.is_empty() {
        return Review::Open { unanswered };
    }
    Review::Holds { commit: f.commit, findings: all.len() }
}

/// The files the feature's commits touched (commits naming its id), that
/// still exist.
pub fn touched(root: &Path, id: &str) -> Vec<String> {
    let Some(o) = git(root, &["log", "--format=", "--name-only", &format!("--grep={id}"), "-F"]) else {
        return Vec::new();
    };
    let mut out: Vec<String> = Vec::new();
    for f in String::from_utf8_lossy(&o.stdout).lines().filter(|l| !l.is_empty()) {
        if !out.iter().any(|o| o == f) && root.join(f).is_file() {
            out.push(f.to_string());
        }
    }
    out.sort();
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn findings_need_an_answer_each() {
        let body = "## Stomp is a bool\n`stomping`...\n**Answer:** fixed in 7c01d2e\n\n## One system per routine\ntext\n**Answer:**\n";
        assert_eq!(
            findings(body),
            [("Stomp is a bool".to_string(), true), ("One system per routine".to_string(), false)]
        );
    }

    #[test]
    fn a_review_holds_while_its_files_are_unchanged_and_answered() {
        let d = super::super::tests::tempdir::Dir::new();
        let root = d.0.as_path();
        let run = |args: &[&str]| assert!(git(root, args).unwrap().status.success(), "git {args:?}");
        run(&["init", "-q"]);
        run(&["config", "user.email", "t@t"]);
        run(&["config", "user.name", "t"]);
        std::fs::write(root.join("jump.rs"), "fn jump() {}\n").unwrap();
        run(&["add", "."]);
        run(&["commit", "-q", "-m", "F-0001: jump"]);
        assert_eq!(read(root, "F-0001"), Review::Missing);
        assert_eq!(touched(root, "F-0001"), ["jump.rs"]);
        let head = String::from_utf8(git(root, &["rev-parse", "HEAD"]).unwrap().stdout).unwrap().trim().to_string();
        std::fs::create_dir_all(root.join(REVIEWS)).unwrap();
        let write = |body: &str| {
            std::fs::write(
                root.join("reviews/F-0001.md"),
                format!("+++\ncommit = \"{head}\"\nfiles = [\"jump.rs\"]\n+++\n{body}"),
            )
            .unwrap()
        };
        write("## A finding\nwhat and why\n");
        assert_eq!(read(root, "F-0001"), Review::Open { unanswered: vec!["A finding".into()] });
        write("## A finding\nwhat and why\n**Answer:** kept: the original does it once per field\n");
        assert_eq!(read(root, "F-0001"), Review::Holds { commit: head.clone(), findings: 1 });
        std::fs::write(root.join("jump.rs"), "fn jump() { let _ = 1; }\n").unwrap();
        assert_eq!(read(root, "F-0001"), Review::Stale { commit: head, changed: vec!["jump.rs".into()] });
        std::fs::write(root.join("reviews/F-0001.md"), "---\ncommit: x\n---\n").unwrap();
        assert!(matches!(read(root, "F-0001"), Review::Invalid(_)));
    }
}
