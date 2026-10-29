//! `port init`: a port's workspace.

use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use super::{GOAL, GOAL_TOML};
use crate::Error;
use crate::commands::{Ctx, Op, Outcome};
use crate::template::{TemplateOptions, TemplateRegion};

const CATALOGUE_README: &str = include_str!("../../../../templates/port/catalogue/README.md");

#[derive(clap::Args, Clone, Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct PortInitArgs {
    /// The port's workspace: a new folder, or one holding nothing the
    /// template writes.
    pub dest: PathBuf,
    /// The disc image (.cue, .chd, .iso or .bin), linked into the
    /// workspace's media/.
    #[arg(long)]
    pub media: PathBuf,
    /// The port's package name (default from the disc image's name).
    #[arg(long)]
    #[serde(default)]
    pub name: Option<String>,
    /// The original's video region. Default: the boot executable's region, else ntsc.
    #[arg(long, value_enum)]
    #[serde(default)]
    pub region: Option<TemplateRegion>,
    /// Video fields per logic tick. Default 1.
    #[arg(long)]
    #[serde(default)]
    pub fields_per_tick: Option<u32>,
}

/// A package name from a game's name: lower case, words joined by `-`.
fn package_from(game: &str) -> String {
    let n: String =
        game.to_ascii_lowercase().chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '-' }).collect();
    let mut n = n.trim_matches('-').to_string();
    while n.contains("--") {
        n = n.replace("--", "-");
    }
    if n.starts_with(|c: char| c.is_ascii_lowercase()) { n } else { format!("game-{n}") }
}

fn write_new(dest: &Path, rel: &str, text: &str, written: &mut Vec<String>) -> crate::Result<()> {
    let p = dest.join(rel);
    if let Some(d) = p.parent() {
        std::fs::create_dir_all(d).map_err(|e| Error::io(d, e))?;
    }
    std::fs::write(&p, text).map_err(|e| Error::io(&p, e))?;
    written.push(rel.to_string());
    Ok(())
}

impl PortInitArgs {
    fn run(&self, ctx: &Ctx) -> crate::Result<Value> {
        let media = absolute(&ctx.root.join(&self.media));
        let dest = absolute(&ctx.root.join(&self.dest));
        let ours = [GOAL, "catalogue/README.md", ".mcp.json", ".psx/project.json"];
        let clashes: Vec<&str> = ours.into_iter().filter(|r| dest.join(r).exists()).collect();
        if !clashes.is_empty() {
            return Err(Error::invalid(format!(
                "{} already has {}; port init writes a new workspace",
                dest.display(),
                clashes.join(", ")
            )));
        }
        // The disc first, so one that cannot be read writes nothing.
        let disc = psx_media::disc::Disc::open(&media)?;
        let mut sources = vec![disc.source.clone()];
        sources.extend(disc.files.iter().map(|f| f.path.clone()));
        sources.sort();
        sources.dedup();
        let game = game_name(&media);
        let name = self.name.clone().unwrap_or_else(|| package_from(&game));
        let (region, region_from) = match self.region {
            Some(r) => (r, "argument"),
            None => match crate::context::region(Some(&media))? {
                Some(crate::bios::Region::Pal) => (TemplateRegion::Pal, "disc"),
                Some(_) => (TemplateRegion::Ntsc, "disc"),
                None => (TemplateRegion::Ntsc, "default"),
            },
        };
        let options = TemplateOptions { region, fields_per_tick: self.fields_per_tick.unwrap_or(1) };
        std::fs::create_dir_all(&dest).map_err(|e| Error::io(&dest, e))?;
        let template = crate::template::create(&dest, Some(&name), options)?;
        let mut written = Vec::new();
        write_new(&dest, GOAL, GOAL_TOML, &mut written)?;
        write_new(&dest, "catalogue/README.md", CATALOGUE_README, &mut written)?;
        let mcp = json!({"mcpServers": {"psx": {"command": psx_exe(), "args": ["--root", dest, "mcp"]}}});
        write_new(
            &dest,
            ".mcp.json",
            &format!("{}\n", serde_json::to_string_pretty(&mcp).unwrap_or_default()),
            &mut written,
        )?;
        // The disc and the BIOS, linked from where they are and never committed.
        let ignore = "# Linked by psx port init from where they are: never commit them.\n*\n";
        write_new(&dest, "media/.gitignore", ignore, &mut written)?;
        write_new(&dest, "bios/.gitignore", ignore, &mut written)?;
        let mut linked = Vec::new();
        for f in &sources {
            let to = dest.join("media").join(f.file_name().unwrap_or_default());
            if !to.exists() {
                link_or_copy(&absolute(f), &to)?;
                linked.push(format!("media/{}", f.file_name().unwrap_or_default().to_string_lossy()));
            }
        }
        let mut bios = Vec::new();
        for e in std::fs::read_dir(ctx.root.join("bios")).into_iter().flatten().flatten() {
            let to = dest.join("bios").join(e.file_name());
            if e.path().is_file() && e.file_name() != ".gitignore" && !to.exists() {
                link_or_copy(&absolute(&e.path()), &to)?;
                bios.push(format!("bios/{}", e.file_name().to_string_lossy()));
            }
        }
        // The disc as the workspace's default media, for every psx command run in it.
        let disc_rel = format!("media/{}", media.file_name().unwrap_or_default().to_string_lossy());
        let project = json!({"format": 1, "media": disc_rel});
        write_new(
            &dest,
            ".psx/project.json",
            &serde_json::to_string_pretty(&project).unwrap_or_default(),
            &mut written,
        )?;
        let git = std::process::Command::new("git")
            .args(["init", "-q"])
            .current_dir(&dest)
            .status()
            .is_ok_and(|s| s.success());
        let mut next = vec![
            format!("the person starts a session (Claude Code or Codex) in {}", dest.display()),
            "the person states the goal; the agent catalogues the features it needs and lists them in goal.toml".into(),
            "the person reads the list (psx port confirm) and confirms it (psx port confirm --yes)".into(),
        ];
        if bios.is_empty() {
            next.push("no BIOS was linked: psx bios install in the workspace".into());
        }
        Ok(json!({
            "game": game, "package": name, "workspace": dest,
            "template": {"owned_files": template["owned_files"], "seeded_files": template["seeded_files"],
                         "region": template["region"], "region_from": region_from, "tick_hz": template["tick_hz"]},
            "written": written, "media": linked, "bios": bios, "git_init": git,
            "next": next,
        }))
    }
}

impl Op for PortInitArgs {
    fn exec(self, ctx: &Ctx) -> Outcome {
        Outcome::result(self.run(ctx))
    }
}

fn absolute(p: &Path) -> PathBuf {
    let p = std::path::absolute(p).unwrap_or_else(|_| p.to_path_buf());
    p.canonicalize().unwrap_or(p)
}

fn psx_exe() -> String {
    std::env::current_exe().map(|p| p.display().to_string()).unwrap_or_else(|_| "psx".into())
}

/// The game's name from its disc image's file name, without the tags
/// after it: `Tomb Raider II - Starring Lara Croft (USA) (Rev 3).cue` is
/// `Tomb Raider II - Starring Lara Croft`.
fn game_name(media: &Path) -> String {
    let stem = media.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let cut = stem.find([' ', '_']).and_then(|_| stem.find(" (").or_else(|| stem.find(" ["))).unwrap_or(stem.len());
    let name = stem[..cut].trim();
    if name.is_empty() { "the game".into() } else { name.to_string() }
}

fn link_or_copy(from: &Path, to: &Path) -> crate::Result<()> {
    if let Some(d) = to.parent() {
        std::fs::create_dir_all(d).map_err(|e| Error::io(d, e))?;
    }
    #[cfg(unix)]
    let r = std::os::unix::fs::symlink(from, to);
    #[cfg(not(unix))]
    let r = std::fs::copy(from, to).map(|_| ());
    r.map_err(|e| Error::io(to, e))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn package_names_come_from_the_game() {
        assert_eq!(package_from("Croc - Legend of the Gobbos"), "croc-legend-of-the-gobbos");
        assert_eq!(package_from("1943"), "game-1943");
    }
}
