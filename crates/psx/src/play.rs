//! `psx play`: the person answers the agents' play requests ([`crate::handoff::queue`])
//! across the projects `psx web` remembers, in the native window. Not an MCP
//! tool: agents ask with `scenario play`, and only a person answers.

use std::io::{BufRead, IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use crate::commands::Ctx;
use crate::handoff::queue::{self, Request, State};

#[derive(clap::Args, Debug)]
pub struct PlayArgs {
    /// A request to play: its number in the list, or its id (default: the oldest waiting).
    pub request: Option<String>,
    /// List the waiting requests and play none.
    #[arg(long, conflicts_with = "decline")]
    pub list: bool,
    /// Answer REQUEST without playing it: NOTE goes to the agent (why, or what to ask instead).
    #[arg(long, value_name = "NOTE", requires = "request")]
    pub decline: Option<String>,
    /// Also this project root (remembered for psx web and psx play from then on).
    #[arg(long, value_name = "DIR")]
    pub project: Vec<PathBuf>,
}

/// The waiting requests of every project, oldest first, with their roots.
fn gather(roots: &[PathBuf]) -> Vec<(PathBuf, Request)> {
    let mut all: Vec<(PathBuf, Request)> =
        roots.iter().flat_map(|root| queue::waiting(root).into_iter().map(move |r| (root.clone(), r))).collect();
    all.sort_by_key(|(_, r)| r.queued_unix_ms);
    all
}

fn project_name(root: &Path) -> String {
    root.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| root.display().to_string())
}

fn ago(unix_ms: u64) -> String {
    let s = (crate::now_ms().saturating_sub(unix_ms as u128) / 1000) as u64;
    match s {
        0..60 => format!("{s}s ago"),
        60..3600 => format!("{}m ago", s / 60),
        3600..86400 => format!("{}h ago", s / 3600),
        _ => format!("{}d ago", s / 86400),
    }
}

fn print_list(all: &[(PathBuf, Request)]) {
    for (i, (root, r)) in all.iter().enumerate() {
        let state = match &r.state {
            State::Playing(_) => " (being played)",
            _ => "",
        };
        let what = r.recording().map(|n| format!(" → {n}")).unwrap_or_default();
        println!("{:>3}  {}{what}  {}{state}  [{}]", i + 1, project_name(root), ago(r.queued_unix_ms), r.id());
        for line in r.text().lines() {
            println!("       {line}");
        }
    }
}

pub fn main(a: PlayArgs, here: &Path) -> ExitCode {
    let mut roots = crate::web::remember(&a.project, &[]);
    let here = std::fs::canonicalize(here).unwrap_or(here.to_path_buf());
    if here.join(queue::DIR).is_dir() && !roots.contains(&here) {
        roots.push(here);
    }
    let all = gather(&roots);
    if a.list || (all.is_empty() && a.request.is_none()) {
        if all.is_empty() {
            println!(
                "No play requests wait ({} projects: psx web DIR or psx play --project DIR adds one).",
                roots.len()
            );
        } else {
            print_list(&all);
        }
        return ExitCode::SUCCESS;
    }
    let pick = |all: &[(PathBuf, Request)], key: &Option<String>| -> Result<(PathBuf, Request), String> {
        match key {
            None => all
                .iter()
                .find(|(_, r)| r.state == State::Queued)
                .cloned()
                .ok_or_else(|| "every waiting request is being played".to_string()),
            Some(k) => {
                let by_number = k.parse::<usize>().ok().and_then(|n| n.checked_sub(1)).and_then(|i| all.get(i));
                by_number
                    .or_else(|| all.iter().find(|(_, r)| &r.id() == k))
                    .cloned()
                    .ok_or_else(|| format!("no waiting request {k} (psx play --list)"))
            }
        }
    };
    let (root, r) = match pick(&all, &a.request) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("{e}");
            return ExitCode::from(1);
        }
    };
    if let Some(note) = a.decline {
        return match queue::decline(&r.dir, &note) {
            Ok(_) => {
                println!("Declined; the note goes to the agent.");
                ExitCode::SUCCESS
            }
            Err(e) => {
                eprintln!("{}", e.message);
                ExitCode::from(1)
            }
        };
    }
    let mut next = Some((root, r));
    while let Some((root, r)) = next.take() {
        println!("Playing {} [{}]:\n  {}", project_name(&root), r.id(), r.text().replace('\n', "\n  "));
        let ctx = Ctx { root: root.clone(), cancel: None };
        match crate::handoff::answer(&ctx, &r.dir, "psx play") {
            Ok((_, v)) => match v["status"].as_str() {
                Some("queued") => println!("Closed without an answer: the request is back in the queue."),
                Some(s) if v.get("error").is_none() => {
                    let rec =
                        v["recording"]["status"].as_str().map(|r| format!(" (recording {r})")).unwrap_or_default();
                    println!("Answered: {s}{rec}.");
                }
                _ => println!("The play failed: {}", v["error"]["message"].as_str().unwrap_or("")),
            },
            Err(e) => {
                eprintln!("{}", e.message);
                return ExitCode::from(1);
            }
        }
        // Requests in a row, while the person wants more.
        let rest: Vec<_> = gather(&roots).into_iter().filter(|(_, r)| r.state == State::Queued).collect();
        if rest.is_empty() || !std::io::stdin().is_terminal() {
            break;
        }
        print!("{} more waiting. Enter plays the next, q stops: ", rest.len());
        let _ = std::io::stdout().flush();
        let mut line = String::new();
        if std::io::stdin().lock().read_line(&mut line).is_err() || line.trim().eq_ignore_ascii_case("q") {
            break;
        }
        next = rest.into_iter().next();
    }
    ExitCode::SUCCESS
}
