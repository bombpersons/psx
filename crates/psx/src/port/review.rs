//! The review: `review.md`, findings on the port's code as a whole by
//! someone who did not write it (docs/port-design.md, The review). It asks
//! whether the code is what one would write for a new game, not whether it
//! does what the original does (the recordings check that). TOML front
//! matter between `+++` lines names the commit reviewed and whether the
//! goal's work recordings all passed then (`matching`) and its work was
//! finished (`finished`); each finding is a `## ` section, answered by a
//! `**Answer:**` line. The review holds while the code is unchanged since
//! its commit and every finding has an answer. A review comes when the
//! goal comes to match its recordings, again when it is finished (its
//! branches resolved, no question open, all its code cited), and once
//! enough of the code changed in between.

use std::path::Path;

use serde::Deserialize;

use super::split_front;

pub const REVIEW: &str = "review.md";

/// The port's code: what the review reads and what makes it stale.
pub const CODE: [&str; 2] = ["game", "tools"];

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Front {
    commit: String,
    #[serde(default)]
    matching: bool,
    #[serde(default)]
    finished: Flag,
    /// What reviews wrote while there were features: the ones matching.
    #[serde(default)]
    features: Vec<String>,
}

/// `finished`: true or false, or as reviews wrote it while there were
/// features, a list of those finished (some finished: true).
#[derive(Debug, Default, Deserialize)]
#[serde(untagged)]
enum Flag {
    #[default]
    No,
    Is(bool),
    Listed(Vec<String>),
}

impl Front {
    fn matching(&self) -> bool {
        self.matching || !self.features.is_empty()
    }

    fn finished(&self) -> bool {
        match &self.finished {
            Flag::No => false,
            Flag::Is(b) => *b,
            Flag::Listed(l) => !l.is_empty(),
        }
    }
}

/// The review as it stands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Review {
    Missing,
    /// The file is not a review: why.
    Invalid(String),
    /// The code changed since its commit: a follow-up review of the changes
    /// is due. `lines`: lines added and removed in the commits since.
    Stale {
        commit: String,
        changed: Vec<String>,
        lines: usize,
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

/// Whether the review says the goal matched its recordings when it was
/// written, and whether it was finished; neither without a readable review.
pub fn says(root: &Path) -> (bool, bool) {
    std::fs::read_to_string(root.join(REVIEW))
        .ok()
        .and_then(|t| split_front(&t).ok())
        .and_then(|(front, _)| toml::from_str::<Front>(&front).ok())
        .map(|f| (f.matching(), f.finished()))
        .unwrap_or_default()
}

/// The code's files git doesn't track yet.
fn untracked(root: &Path) -> Vec<String> {
    let mut args = vec!["ls-files", "--others", "--exclude-standard", "--"];
    args.extend(CODE);
    git(root, &args)
        .map(|o| String::from_utf8_lossy(&o.stdout).lines().map(str::to_string).collect())
        .unwrap_or_default()
}

/// Lines added and removed in the code since `commit`, committed or not,
/// new files included: an agent that doesn't commit still gets reviewed.
fn lines_since(root: &Path, commit: &str) -> usize {
    let mut args = vec!["diff", "--numstat", commit, "--"];
    args.extend(CODE);
    let new: usize =
        untracked(root).iter().map(|f| std::fs::read_to_string(root.join(f)).map_or(0, |t| t.lines().count())).sum();
    new + git(root, &args)
        .map(|o| {
            String::from_utf8_lossy(&o.stdout)
                .lines()
                .flat_map(|l| l.split('\t').take(2).map(|n| n.parse::<usize>().unwrap_or(0)).collect::<Vec<_>>())
                .sum()
        })
        .unwrap_or(0)
}

/// Lines of the code written since the workspace's first commit, for a
/// port with no review yet.
pub fn lines_written(root: &Path) -> usize {
    git(root, &["rev-list", "--max-parents=0", "HEAD"])
        .and_then(|o| String::from_utf8_lossy(&o.stdout).lines().last().map(|c| lines_since(root, c)))
        .unwrap_or(0)
}

/// The review in the workspace `root`.
pub fn read(root: &Path) -> Review {
    let Ok(text) = std::fs::read_to_string(root.join(REVIEW)) else {
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
    let known = git(root, &["rev-parse", "--verify", "-q", &format!("{}^{{commit}}", f.commit)])
        .is_some_and(|o| o.status.success());
    if !known {
        return Review::Invalid(format!("commit {:?}: not a commit of this repository", f.commit));
    }
    // Against the working tree: uncommitted changes count as changes.
    let mut args = vec!["diff", "--name-only", f.commit.as_str(), "--"];
    args.extend(CODE);
    let mut changed: Vec<String> = git(root, &args)
        .map(|o| String::from_utf8_lossy(&o.stdout).lines().map(str::to_string).collect())
        .unwrap_or_default();
    changed.extend(untracked(root));
    if !changed.is_empty() {
        return Review::Stale { lines: lines_since(root, &f.commit), commit: f.commit, changed };
    }
    let all = findings(&body);
    let unanswered: Vec<String> = all.iter().filter(|(_, a)| !a).map(|(h, _)| h.clone()).collect();
    if !unanswered.is_empty() {
        return Review::Open { unanswered };
    }
    Review::Holds { commit: f.commit, findings: all.len() }
}

/// A finding as `psx web` shows it: heading, text and answer (Markdown).
fn finding_texts(body: &str) -> Vec<serde_json::Value> {
    let mut out: Vec<(String, String, Option<String>)> = Vec::new();
    for line in body.lines() {
        if let Some(h) = line.strip_prefix("## ") {
            out.push((h.trim().to_string(), String::new(), None));
        } else if let Some(last) = out.last_mut() {
            match (&mut last.2, line.trim_start().strip_prefix("**Answer:**")) {
                (None, Some(a)) => last.2 = Some(a.trim().to_string()),
                (Some(a), _) => {
                    a.push('\n');
                    a.push_str(line);
                }
                (None, None) => {
                    last.1.push_str(line);
                    last.1.push('\n');
                }
            }
        }
    }
    out.into_iter()
        .map(|(h, t, a)| {
            let a = a.map(|a| a.trim().to_string()).filter(|a| !a.is_empty());
            serde_json::json!({"heading": h, "text": t.trim(), "answer": a})
        })
        .collect()
}

/// Every review round in the history, for `psx web`: the review as it stood
/// at each commit that changed `review.md` (and uncommitted), grouped by
/// the commit it reviewed, newest last. A finding a round adds is `new`.
pub fn rounds(root: &Path) -> serde_json::Value {
    use serde_json::json;
    let text = |o: Option<std::process::Output>| {
        o.filter(|o| o.status.success()).map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
    };
    let log = text(git(root, &["log", "--reverse", "--format=%H%x1f%ct%x1f%s", "--", REVIEW])).unwrap_or_default();
    let mut versions: Vec<(Option<String>, u64, String, String)> = log
        .lines()
        .filter_map(|l| {
            let mut f = l.split('\x1f');
            let (h, t, s) = (f.next()?, f.next()?, f.next().unwrap_or(""));
            let body = text(git(root, &["show", &format!("{h}:{REVIEW}")]))?;
            Some((Some(h.to_string()), t.parse::<u64>().ok()? * 1000, s.to_string(), body))
        })
        .collect();
    if let Ok(now) = std::fs::read_to_string(root.join(REVIEW)) {
        if versions.last().is_none_or(|v| v.3 != now) {
            let t = std::fs::metadata(root.join(REVIEW))
                .and_then(|m| m.modified())
                .ok()
                .and_then(|m| m.duration_since(std::time::UNIX_EPOCH).ok())
                .map_or(0, |d| d.as_millis() as u64);
            versions.push((None, t, "uncommitted".into(), now));
        }
    }
    let mut rounds: Vec<serde_json::Value> = Vec::new();
    for (commit, t, subject, body) in versions {
        let Ok((front, body)) = split_front(&body) else { continue };
        let Ok(f) = toml::from_str::<Front>(&front) else { continue };
        let findings = finding_texts(&body);
        let version = json!({"commit": commit, "t": t, "subject": subject});
        match rounds.last_mut().filter(|r| r["reviewed"] == json!(f.commit)) {
            // The same round, answered further.
            Some(r) => {
                r["findings"] = json!(findings);
                r["versions"].as_array_mut().unwrap().push(version);
            }
            None => rounds.push(json!({"reviewed": f.commit, "matching": f.matching(), "finished": f.finished(),
                "findings": findings, "versions": [version]})),
        }
    }
    // What each round added to the one before.
    let mut seen: std::collections::HashSet<String> = Default::default();
    for r in &mut rounds {
        for f in r["findings"].as_array_mut().unwrap() {
            let h = f["heading"].as_str().unwrap_or("").to_string();
            f["new"] = json!(seen.insert(h));
        }
    }
    let status = read(root);
    let mut s = json!({"state": status.name()});
    match &status {
        Review::Invalid(why) => s["why"] = json!(why),
        Review::Stale { commit, changed, lines } => {
            s["commit"] = json!(commit);
            s["changed"] = json!(changed.len());
            s["lines"] = json!(lines);
        }
        Review::Open { unanswered } => s["unanswered"] = json!(unanswered),
        Review::Holds { commit, findings } => {
            s["commit"] = json!(commit);
            s["findings"] = json!(findings);
        }
        Review::Missing => {}
    }
    json!({"status": s, "rounds": rounds})
}

/// The code's changes from `from` to `to` (the commits two rounds
/// reviewed), as `git diff` prints them, or None for a commit that is not
/// one or a diff too large to show.
pub fn diff(root: &Path, from: &str, to: &str) -> Option<String> {
    let ok = |c: &str| c.len() >= 7 && c.len() <= 40 && c.bytes().all(|b| b.is_ascii_hexdigit());
    if !ok(from) || !ok(to) {
        return None;
    }
    let mut args = vec!["diff", "--stat=120", "--patch", from, to, "--"];
    args.extend(CODE);
    let out = git(root, &args).filter(|o| o.status.success())?;
    (out.stdout.len() <= 2 << 20).then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lines_written_counts_uncommitted_and_new_files_in_the_code_only() {
        let d = super::super::tests::tempdir::Dir::new();
        let run = |args: &[&str]| {
            let base = ["-c", "user.name=t", "-c", "user.email=t@t"];
            assert!(git(&d.0, &[&base[..], args].concat()).unwrap().status.success(), "{args:?}");
        };
        run(&["init", "-q"]);
        std::fs::create_dir_all(d.0.join("game")).unwrap();
        std::fs::write(d.0.join("game/a.rs"), "one\ntwo\n").unwrap();
        run(&["add", "."]);
        run(&["commit", "-q", "-m", "first"]);
        assert_eq!(lines_written(&d.0), 0);
        std::fs::write(d.0.join("game/a.rs"), "one\ntwo\nthree\n").unwrap();
        std::fs::write(d.0.join("game/b.rs"), "a\nb\nc\n").unwrap();
        std::fs::write(d.0.join("notes.md"), "not code\n").unwrap();
        assert_eq!(lines_written(&d.0), 4);
    }

    #[test]
    fn finding_texts_keep_the_text_and_answer() {
        let body = "## Stomp is a bool\n`stomping` is...\n\n**Answer:** fixed in 7c01d2e\nand tested\n## One system\ntext\n**Answer:**\n";
        let f = finding_texts(body);
        assert_eq!(f[0]["heading"], "Stomp is a bool");
        assert_eq!(f[0]["text"], "`stomping` is...");
        assert_eq!(f[0]["answer"], "fixed in 7c01d2e\nand tested");
        assert_eq!(f[1]["answer"], serde_json::Value::Null);
    }

    #[test]
    fn findings_need_an_answer_each() {
        let body = "## Stomp is a bool\n`stomping`...\n**Answer:** fixed in 7c01d2e\n\n## One system per routine\ntext\n**Answer:**\n";
        assert_eq!(
            findings(body),
            [("Stomp is a bool".to_string(), true), ("One system per routine".to_string(), false)]
        );
    }

    #[test]
    fn the_review_holds_while_the_code_is_unchanged_and_answered() {
        let d = super::super::tests::tempdir::Dir::new();
        let root = d.0.as_path();
        let run = |args: &[&str]| assert!(git(root, args).unwrap().status.success(), "git {args:?}");
        run(&["init", "-q"]);
        run(&["config", "user.email", "t@t"]);
        run(&["config", "user.name", "t"]);
        std::fs::create_dir_all(root.join("game/src")).unwrap();
        std::fs::write(root.join("game/src/jump.rs"), "fn jump() {}\n").unwrap();
        run(&["add", "."]);
        run(&["commit", "-q", "-m", "jump"]);
        assert_eq!(read(root), Review::Missing);
        let head = String::from_utf8(git(root, &["rev-parse", "HEAD"]).unwrap().stdout).unwrap().trim().to_string();
        let write =
            |body: &str| std::fs::write(root.join(REVIEW), format!("+++\ncommit = \"{head}\"\n+++\n{body}")).unwrap();
        write("## A finding\nwhat and why\n");
        assert_eq!(read(root), Review::Open { unanswered: vec!["A finding".into()] });
        write("## A finding\nwhat and why\n**Answer:** done in the next commit\n");
        assert_eq!(read(root), Review::Holds { commit: head.clone(), findings: 1 });
        assert_eq!(says(root), (false, false));
        std::fs::write(root.join(REVIEW), format!("+++\ncommit = \"{head}\"\nmatching = true\n+++\n")).unwrap();
        assert_eq!((read(root).holds(), says(root)), (true, (true, false)));
        // As reviews wrote it while there were features.
        std::fs::write(
            root.join(REVIEW),
            format!("+++\ncommit = \"{head}\"\nfeatures = [\"F-0001\"]\nfinished = [\"F-0001\"]\n+++\n"),
        )
        .unwrap();
        assert_eq!((read(root).holds(), says(root)), (true, (true, true)));
        // What is not code leaves it holding.
        std::fs::create_dir_all(root.join("catalogue")).unwrap();
        std::fs::write(root.join("catalogue/questions.md"), "- why\n").unwrap();
        assert!(read(root).holds());
        std::fs::write(root.join("game/src/jump.rs"), "fn jump() { let _ = 1; }\n").unwrap();
        // Uncommitted: stale, its lines counted.
        let stale = |lines| Review::Stale { commit: head.clone(), changed: vec!["game/src/jump.rs".into()], lines };
        assert_eq!(read(root), stale(2));
        run(&["commit", "-qam", "jump"]);
        assert_eq!(read(root), stale(2));
        std::fs::write(root.join(REVIEW), "---\ncommit: x\n---\n").unwrap();
        assert!(matches!(read(root), Review::Invalid(_)));
    }
}
