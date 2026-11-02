//! Reports for the person: what a reviewer thinks of the port, written for
//! the person alone and shown in `psx web`, never in a psx result. Kept
//! outside the project, in psx's data directory
//! (`reports/<key>/<id>.json`, the key naming the project's root), so
//! nothing in the workspace leads the writer to them.

use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use crate::Error;
use crate::commands::{Ctx, Op, Outcome};

/// Longest report kept.
const TEXT_LIMIT: usize = 32 * 1024;

#[derive(clap::Args, Clone, Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct ReportArgs {
    /// The report, for the person (Markdown).
    #[arg(long)]
    pub text: String,
}

/// Where `root`'s reports are kept.
pub fn dir(root: &Path) -> Option<PathBuf> {
    let root = std::fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
    let sha = psx_media::disc::sha256_bytes(root.to_string_lossy().as_bytes());
    crate::bios::data_dir().map(|d| d.join("reports").join(&sha[..16]))
}

/// Every report on `root`, oldest first.
pub fn all(root: &Path) -> Vec<Value> {
    let Some(dir) = dir(root) else { return Vec::new() };
    let mut reports: Vec<Value> = std::fs::read_dir(&dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.path().extension().is_some_and(|x| x == "json"))
        .filter_map(|e| std::fs::read(e.path()).ok())
        .filter_map(|b| serde_json::from_slice(&b).ok())
        .collect();
    reports.sort_by_key(|r| r["t"].as_u64().unwrap_or(0));
    reports
}

impl Op for ReportArgs {
    fn exec(self, ctx: &Ctx) -> Outcome {
        let text = self.text.trim();
        if text.is_empty() {
            return Outcome::from(Error::invalid("an empty report"));
        }
        if text.len() > TEXT_LIMIT {
            return Outcome::from(Error::invalid(format!("the report is {} bytes; at most {TEXT_LIMIT}", text.len())));
        }
        let Some(dir) = dir(&ctx.root) else {
            return Outcome::from(Error::invalid("no data directory for psx (HOME is not set)"));
        };
        let head = std::process::Command::new("git")
            .args(["rev-parse", "--verify", "-q", "HEAD"])
            .current_dir(&ctx.root)
            .output()
            .ok()
            .filter(|o| o.status.success())
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string());
        let id = format!("r{}-{}", crate::now_ms(), std::process::id());
        let report = json!({"id": id, "t": crate::now_ms() as u64, "commit": head, "text": text});
        let path = dir.join(format!("{id}.json"));
        if let Err(e) = std::fs::create_dir_all(&dir).and_then(|_| std::fs::write(&path, report.to_string())) {
            return Outcome::from(Error::io(&path, e));
        }
        // Which root the key names, for whoever lists the folder.
        let _ = std::fs::write(dir.join("root.txt"), ctx.root.to_string_lossy().as_bytes());
        Outcome::ok(json!({"status": "filed", "id": id, "note": "The person reads it in psx web."}))
    }
}
