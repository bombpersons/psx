//! The clean-room roles: `template create --role` and `port init`.
//!
//! A port under the protocol has three repositories side by side, each its
//! own agent's: the investigator's (the disc, psx, specs and notes), the
//! implementer's (psx's Bevy template with its own instructions, the disc's
//! data files without the executable, access rules that keep everything
//! else out, and the message check) and the reviewer's (verdicts and
//! audits). `template create --role` writes one of them; `port init` writes
//! all three with the real paths, and the investigator's `protocol.toml`
//! that `publish` and the barrier read.

use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use crate::Error;
use crate::commands::{Ctx, Op, Outcome};
use crate::template::{TemplateOptions, TemplateRegion};

/// A role in the clean room.
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, clap::ValueEnum, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    /// Reads the original; writes specs and cases.
    Investigator,
    /// Builds the port from what crosses (the Bevy template).
    Implementer,
    /// Approves what crosses; audits the port.
    Reviewer,
}

impl Role {
    pub fn name(self) -> &'static str {
        match self {
            Role::Investigator => "investigator",
            Role::Implementer => "implementer",
            Role::Reviewer => "reviewer",
        }
    }
}

macro_rules! role_files {
    ($dir:literal: $($path:literal),* $(,)?) => {
        &[$(($path, include_str!(concat!("../../../templates/roles/", $dir, "/", $path)))),*]
    };
}

const INVESTIGATOR: &[(&str, &str)] = role_files!["investigator":
    "AGENTS.md", ".gitignore", "samples.toml", "specs/README.md", "specs/glossary.md", "specs/INDEX.md",
    "notes/provenance.json", "notes/LOOP.md", "notes/recordings/README.md", "notes/explore/README.md",
    "tools/README.md",
];

const REVIEWER: &[(&str, &str)] = role_files!["reviewer": "AGENTS.md", "reviews/README.md", "audits/README.md"];

/// Files the implementer's workspace adds to the Bevy template's.
const IMPLEMENTER_EXTRA: &[(&str, &str)] = role_files!["implementer":
    "questions/README.md", "questions/CHOICES.md", "questions/LOOP.md", "notes/README.md",
];

const IMPLEMENTER_AGENTS: &str = include_str!("../../../templates/roles/implementer/AGENTS.md");
const IMPLEMENTER_EVIDENCE: &str = include_str!("../../../templates/roles/implementer/docs/evidence.md");
const IMPLEMENTER_ENGINE_MODEL: &str = include_str!("../../../templates/roles/implementer/docs/engine-model.md");
const IMPLEMENTER_TESTING: &str = include_str!("../../../templates/roles/implementer/game/testing.md");
const IMPLEMENTER_CODE: &str = include_str!("../../../templates/roles/implementer/game/code.md");
const IMPLEMENTER_CUT: &str = include_str!("../../../templates/roles/implementer/engine/cut.md");

/// `{{GAME}}` and the repositories' paths in a role's text.
fn fill(text: &str, game: &str, repos: Option<&Repos>) -> String {
    let mut t = text.replace("{{GAME}}", game);
    if let Some(r) = repos {
        t = t
            .replace("{{INVESTIGATOR}}", &r.investigator.display().to_string())
            .replace("{{IMPLEMENTER}}", &r.implementer.display().to_string())
            .replace("{{REVIEWER}}", &r.reviewer.display().to_string());
    }
    t
}

/// `text` with the section from the heading `from` up to (not including)
/// the heading `to` replaced by `with`.
fn replace_section(text: &str, from: &str, to: &str, with: &str) -> crate::Result<String> {
    let start = text.find(&format!("\n{from}\n")).map(|i| i + 1);
    let end = start.and_then(|s| text[s..].find(&format!("\n{to}\n")).map(|i| s + i + 1));
    match (start, end) {
        (Some(s), Some(e)) => Ok(format!("{}{with}{}", &text[..s], &text[e..])),
        _ => Err(Error::new("internal", format!("the template no longer has the section {from:?} before {to:?}"))),
    }
}

fn replace_once(text: &str, from: &str, to: &str) -> crate::Result<String> {
    if !text.contains(from) {
        return Err(Error::new("internal", format!("the template no longer has {from:?}")));
    }
    Ok(text.replacen(from, to, 1))
}

/// The Bevy template's files as the implementer gets them: its own
/// instructions and architecture section, guidance written for working
/// from specs and cases rather than the original, `data/` ignored, and the
/// questions folder.
pub fn implementer_files(files: &mut Vec<(&'static str, String)>, game: &str) -> crate::Result<()> {
    for (rel, text) in files.iter_mut() {
        *text = match *rel {
            "AGENTS.md" => fill(IMPLEMENTER_AGENTS, game, None),
            "docs/evidence.md" => IMPLEMENTER_EVIDENCE.to_string(),
            "docs/engine-model.md" => IMPLEMENTER_ENGINE_MODEL.to_string(),
            ".gitignore" => {
                format!("{text}\n# The game's data files, from the player's disc (never commit them).\ndata/\n")
            }
            "engine/AGENTS.md" => {
                let t = replace_section(text, "## Cut it where the original cuts", "## Placing code", IMPLEMENTER_CUT)?;
                replace_once(&t, " (see the psx `guide` topic `port`)", "")?
            }
            "game/AGENTS.md" => {
                let mut t = replace_section(
                    text,
                    "## Testing gameplay",
                    "## From evidence to idiomatic code",
                    IMPLEMENTER_TESTING,
                )?;
                t = replace_section(&t, "## From evidence to idiomatic code", "### Verify", IMPLEMENTER_CODE)?;
                t = replace_once(
                    &t,
                    " `psx textures` and\n  `psx assets extract` convert TIM/TMD to PNG/glTF for a quick look.",
                    " The `FMT` specs describe them.",
                )?;
                t = replace_once(
                    &t,
                    "Evidence-recording scripts and importers live in the workspace's `tools/`.",
                    "Importers live in the workspace's `tools/`. Here they are developed against `data/`, the\n\
                     game's data files with the disc's paths; the shipped game reads them from the player's disc.",
                )?;
                t = replace_once(
                    &t,
                    "- Set `import::SERIALS` to every disc revision the importers support (the\n  boot executable in `SYSTEM.CNF`) and `import::GAME` to the original's name.",
                    "- Set `import::GAME` to the game's name. Leave `import::SERIALS` empty (any\n  disc) unless a spec names the disc revisions the importers support.",
                )?;
                t = replace_once(
                    &t,
                    "  Set both from evidence: the original's main loop and how many vblanks it waits\n  per game frame.",
                    "  Set both from the specs: the case files give `fields_per_tick`.",
                )?;
                replace_once(
                    &t,
                    " Read it\n  from the original's display mode and range (some show 368x240 or 512x240).",
                    " The specs give it.",
                )?
            }
            _ => continue,
        };
    }
    for (rel, text) in IMPLEMENTER_EXTRA {
        files.push((rel, text.to_string()));
    }
    Ok(())
}

/// The three repositories, as absolute paths.
#[derive(Clone, Debug)]
pub struct Repos {
    pub investigator: PathBuf,
    pub implementer: PathBuf,
    pub reviewer: PathBuf,
}

/// What the access rules and settings need besides the repositories.
#[derive(Clone, Debug, Default)]
pub struct Setting {
    /// The disc image the investigator works from.
    pub media: Option<PathBuf>,
    /// The executables' names, refused at the barrier.
    pub executables: Vec<String>,
    /// psx's own project folder, where it keeps its analyses.
    pub psx_root: Option<PathBuf>,
}

fn absolute(p: &Path) -> PathBuf {
    let p = std::path::absolute(p).unwrap_or_else(|_| p.to_path_buf());
    p.canonicalize().unwrap_or(p)
}

/// The folder name an agent host keeps a project's sessions under
/// (Claude Code: every character but letters and digits becomes -).
fn host_key(p: &Path) -> String {
    p.to_string_lossy().chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '-' }).collect()
}

fn uid() -> Option<u32> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        std::fs::metadata("/proc/self").ok().map(|m| m.uid())
    }
    #[cfg(not(unix))]
    {
        None
    }
}

fn psx_exe() -> String {
    std::env::current_exe().map(|p| p.display().to_string()).unwrap_or_else(|_| "psx".into())
}

/// Deny reading, writing and shell commands naming `p`, and with
/// `sessions` the host's session folders for an agent working there.
fn deny_repo(deny: &mut Vec<String>, p: &Path, sessions: bool) {
    let s = p.display();
    deny.push(format!("Read(/{s}/**)"));
    deny.push(format!("Edit(/{s}/**)"));
    deny.push(format!("Write(/{s}/**)"));
    deny.push(format!("Bash(*{s}*)"));
    if !sessions {
        return;
    }
    if let Ok(home) = std::env::var("HOME") {
        deny.push(format!("Read(/{home}/.claude/projects/{}/**)", host_key(p)));
    }
    if let Some(u) = uid() {
        deny.push(format!("Read(//tmp/claude-{u}/{}/**)", host_key(p)));
    }
}

fn web(deny: &mut Vec<String>) {
    deny.extend(["WebFetch", "WebSearch"].map(String::from));
}

/// The host's settings for a role (Claude Code's `.claude/settings.json`).
fn settings(role: Role, repos: &Repos, s: &Setting) -> Value {
    let mut deny = Vec::new();
    web(&mut deny);
    match role {
        Role::Investigator => {
            // It reads the implementer's questions, and never writes there or approves.
            for p in [&repos.implementer, &repos.reviewer] {
                deny.push(format!("Edit(/{}/**)", p.display()));
                deny.push(format!("Write(/{}/**)", p.display()));
                deny.push(format!("Bash(*{}*)", p.display()));
            }
            json!({"permissions": {"deny": deny}})
        }
        Role::Reviewer => {
            for p in [&repos.investigator, &repos.implementer] {
                deny.push(format!("Edit(/{}/**)", p.display()));
                deny.push(format!("Write(/{}/**)", p.display()));
                deny.push(format!("Bash(*{}*)", p.display()));
            }
            deny.extend(["mcp__psx__publish_stage", "mcp__psx__publish_cross"].map(String::from));
            json!({"permissions": {"deny": deny}})
        }
        Role::Implementer => {
            deny.extend(["mcp__psx", "Bash(psx:*)", "Bash(psx *)"].map(String::from));
            deny.push(format!("Bash(*{}*)", psx_exe()));
            deny_repo(&mut deny, &repos.investigator, true);
            deny_repo(&mut deny, &repos.reviewer, true);
            if let Some(root) = &s.psx_root {
                // psx's own folder keeps its analyses (and the disc) in .psx/ and media/.
                if repos.implementer.starts_with(root) {
                    for d in [".psx", "media", "bios"] {
                        deny_repo(&mut deny, &root.join(d), false);
                    }
                } else {
                    deny_repo(&mut deny, root, true);
                }
            }
            if let Some(dir) = s.media.as_ref().and_then(|m| m.parent()) {
                deny_repo(&mut deny, dir, false);
            }
            if let Ok(home) = std::env::var("HOME") {
                deny.push(format!("Read(/{home}/.config/Claude/**)"));
            }
            deny.extend(
                [
                    "Bash(*.config/Claude*)",
                    "Bash(*side-session-notes*)",
                    "Edit(.claude/**)",
                    "Write(.claude/**)",
                    "Bash(*.claude/*)",
                    "Edit(notes/**)",
                    "Write(notes/**)",
                    "Bash(*guidance.jsonl*)",
                    "Bash(*published.jsonl*)",
                    "Edit(specs/**)",
                    "Write(specs/**)",
                ]
                .map(String::from),
            );
            json!({
                "permissions": {"deny": deny},
                "hooks": {"UserPromptSubmit": [{"hooks": [{"type": "command", "command": format!("{} barrier hook", psx_exe())}]}]},
            })
        }
    }
}

/// The psx MCP server for the investigator and the reviewer, on the
/// investigator's project.
fn mcp_json(repos: &Repos) -> Value {
    json!({"mcpServers": {"psx": {
        "command": psx_exe(),
        "args": ["--root", repos.investigator, "mcp"],
    }}})
}

/// The implementer's message check settings.
fn barrier_json(repos: &Repos, s: &Setting) -> Value {
    json!({
        "investigator": repos.investigator, "names": s.executables,
        "published": ["specs", "questions"], "log": "notes/guidance.jsonl", "confirm_prefix": "[cross]",
    })
}

/// The investigator's `protocol.toml`.
fn protocol_toml(game: &str, repos: &Repos, s: &Setting) -> String {
    let q = |v: &str| serde_json::to_string(v).unwrap_or_default();
    let mut t = format!(
        "# Written by psx port init: the port's repositories and its disc. psx publish and the\n\
         # barrier read it.\ngame = {}\n\n[repositories]\ninvestigator = \".\"\nimplementer = {}\nreviewer = {}\n",
        q(game),
        q(&repos.implementer.display().to_string()),
        q(&repos.reviewer.display().to_string())
    );
    if s.media.is_some() || !s.executables.is_empty() {
        t += "\n[disc]\n";
        if let Some(m) = &s.media {
            t += &format!("media = {}\n", q(&m.display().to_string()));
        }
        let names: Vec<String> = s.executables.iter().map(|e| q(e)).collect();
        t += &format!("executables = [{}]\n", names.join(", "));
    }
    t
}

/// The investigator's protocol settings, if it has them: (implementer,
/// reviewer, executables).
pub fn protocol(root: &Path) -> Option<(PathBuf, PathBuf, Vec<String>)> {
    let text = std::fs::read_to_string(root.join("protocol.toml")).ok()?;
    let v: toml::Value = toml::from_str(&text).ok()?;
    let at = |k: &str| {
        let p = PathBuf::from(v.get("repositories")?.get(k)?.as_str()?);
        Some(if p.is_absolute() { p } else { root.join(p) })
    };
    let exes = v
        .get("disc")
        .and_then(|d| d.get("executables"))
        .and_then(|e| e.as_array())
        .map(|a| a.iter().filter_map(|x| x.as_str().map(str::to_string)).collect())
        .unwrap_or_default();
    Some((at("implementer")?, at("reviewer")?, exes))
}

fn write_new(dest: &Path, rel: &str, text: &str, written: &mut Vec<String>) -> crate::Result<()> {
    let p = dest.join(rel);
    if p.exists() {
        return Err(Error::invalid(format!(
            "{} already exists; a role is only written into a new folder",
            p.display()
        )));
    }
    if let Some(d) = p.parent() {
        std::fs::create_dir_all(d).map_err(|e| Error::io(d, e))?;
    }
    std::fs::write(&p, text).map_err(|e| Error::io(&p, e))?;
    written.push(rel.to_string());
    Ok(())
}

fn pretty(v: &Value) -> String {
    format!("{}\n", serde_json::to_string_pretty(v).unwrap_or_default())
}

/// One role's repository.
pub fn create(
    dest: &Path,
    role: Role,
    game: &str,
    repos: &Repos,
    s: &Setting,
    name: Option<&str>,
    options: TemplateOptions,
) -> crate::Result<Value> {
    let mut written = Vec::new();
    let mut out = json!({"role": role.name(), "dest": dest});
    match role {
        Role::Investigator => {
            for (rel, text) in INVESTIGATOR {
                write_new(dest, rel, &fill(text, game, Some(repos)), &mut written)?;
            }
            write_new(dest, "protocol.toml", &protocol_toml(game, repos, s), &mut written)?;
            write_new(dest, ".mcp.json", &pretty(&mcp_json(repos)), &mut written)?;
        }
        Role::Reviewer => {
            for (rel, text) in REVIEWER {
                write_new(dest, rel, &fill(text, game, Some(repos)), &mut written)?;
            }
            write_new(dest, ".mcp.json", &pretty(&mcp_json(repos)), &mut written)?;
        }
        Role::Implementer => {
            for p in [".claude/settings.json", ".claude/barrier.json"] {
                if dest.join(p).exists() {
                    return Err(Error::invalid(format!("{} already exists", dest.join(p).display())));
                }
            }
            out["template"] = crate::template::create_for(dest, name, options, Some(game))?;
            write_new(dest, ".claude/barrier.json", &pretty(&barrier_json(repos, s)), &mut written)?;
        }
    }
    write_new(dest, ".claude/settings.json", &pretty(&settings(role, repos, s)), &mut written)?;
    out["written"] = json!(written);
    Ok(out)
}

/// The repositories next to `dest` for a role created alone: the other
/// two are siblings named as `port init` names them.
fn sibling_repos(dest: &Path, role: Role, given: [&Option<PathBuf>; 3]) -> Repos {
    let dest = absolute(dest);
    let parent = dest.parent().map(Path::to_path_buf).unwrap_or_default();
    let pick = |r: Role, g: &Option<PathBuf>, default: &str| match g {
        Some(p) => absolute(p),
        None if r == role => dest.clone(),
        None => parent.join(default),
    };
    Repos {
        investigator: pick(Role::Investigator, given[0], "investigate"),
        implementer: pick(Role::Implementer, given[1], "clean"),
        reviewer: pick(Role::Reviewer, given[2], "review"),
    }
}

/// `template create --role`.
pub fn create_role(
    ctx: &Ctx,
    dest: &Path,
    role: Role,
    game: Option<&str>,
    given: [&Option<PathBuf>; 3],
    name: Option<&str>,
    options: TemplateOptions,
) -> crate::Result<Value> {
    let repos = sibling_repos(dest, role, given);
    let s = Setting { psx_root: Some(absolute(&ctx.root)), ..Setting::default() };
    let game = game.map(str::to_string).unwrap_or_else(|| {
        absolute(dest).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "the game".into())
    });
    std::fs::create_dir_all(dest).map_err(|e| Error::io(dest, e))?;
    let mut v = create(dest, role, &game, &repos, &s, name, options)?;
    v["repositories"] =
        json!({"investigator": repos.investigator, "implementer": repos.implementer, "reviewer": repos.reviewer});
    Ok(v)
}

#[derive(clap::Args, Clone, Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct PortInitArgs {
    /// Folder for the three repositories: investigate/, clean/ and review/.
    pub dest: PathBuf,
    /// The disc image (.cue, .chd, .iso or .bin).
    #[arg(long)]
    pub media: PathBuf,
    /// The game's name in the roles' instructions (default: the disc image's name).
    #[arg(long)]
    #[serde(default)]
    pub game: Option<String>,
    /// The port's package name (default from the game's name).
    #[arg(long)]
    #[serde(default)]
    pub name: Option<String>,
    /// The original's video region. Default ntsc.
    #[arg(long, value_enum)]
    #[serde(default)]
    pub region: Option<TemplateRegion>,
    /// Video fields per logic tick. Default 1.
    #[arg(long)]
    #[serde(default)]
    pub fields_per_tick: Option<u32>,
}

const CAUTIONS: [&str; 2] = [
    "This is not legal advice, and a clean room lowers a risk rather than removing it.",
    "An LLM implementer may have seen other reimplementations of the game in training, which no tool restriction prevents.",
];

fn is_executable(bytes: &[u8]) -> bool {
    bytes.starts_with(b"PS-X EXE")
}

/// The boot path in SYSTEM.CNF as an ISO path (`cdrom:\SLUS_005.67;1` is `/SLUS_005.67`).
pub(crate) fn boot_path(boot: &str) -> String {
    let p = boot.trim_start_matches("cdrom:").trim_start_matches("cdrom0:");
    let p = p.split(';').next().unwrap_or(p).replace('\\', "/");
    format!("/{}", p.trim_start_matches('/')).to_ascii_uppercase()
}

/// The game's name from its disc image's file name, without the tags
/// after it: `Tomb Raider II - Starring Lara Croft (USA) (Rev 3).cue` is
/// `Tomb Raider II - Starring Lara Croft`. (The volume id is often the
/// disc's serial, which the barrier refuses.)
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

impl PortInitArgs {
    fn run(&self, ctx: &Ctx) -> crate::Result<Value> {
        use psx_media::disc::Disc;
        use psx_media::iso9660::FileSystem;
        let media = absolute(&ctx.root.join(&self.media));
        let disc = Disc::open(&media)?;
        let fs = FileSystem::read(&disc)?;
        // The executables: SYSTEM.CNF's boot file, and every PS-X EXE on the disc.
        let cnf = fs.find("/SYSTEM.CNF").and_then(|e| fs.read_file(&disc, e).ok());
        let boot = cnf
            .as_deref()
            .map(psx_media::system_cnf::SystemCnf::parse)
            .and_then(|c| c.boot)
            .unwrap_or_else(|| psx_media::system_cnf::DEFAULT_BOOT.into());
        let boot = boot_path(&boot);
        let game = self.game.clone().unwrap_or_else(|| game_name(&media));
        let dest = absolute(&ctx.root.join(&self.dest));
        let repos = Repos {
            investigator: dest.join("investigate"),
            implementer: dest.join("clean"),
            reviewer: dest.join("review"),
        };
        for p in [&repos.investigator, &repos.implementer, &repos.reviewer] {
            if std::fs::read_dir(p).map(|mut d| d.next().is_some()).unwrap_or(false) {
                return Err(Error::invalid(format!("{} is not empty; port init writes new repositories", p.display())));
            }
        }
        // The data files, read first so a disc that cannot be read writes nothing.
        let (mut data, mut excluded, mut failed) = (Vec::new(), Vec::new(), Vec::new());
        for e in fs.entries.iter().filter(|e| !e.is_dir) {
            let (bytes, raw) = match fs.read_file(&disc, e) {
                Ok(b) => (b, false),
                Err(psx_media::MediaError::Unsupported(_)) => match fs.read_file_raw(&disc, e) {
                    Ok(b) => (b, true),
                    Err(err) => {
                        failed.push(json!({"path": e.path, "error": err.to_string()}));
                        continue;
                    }
                },
                Err(err) => {
                    failed.push(json!({"path": e.path, "error": err.to_string()}));
                    continue;
                }
            };
            let upper = e.path.to_ascii_uppercase();
            if upper == boot || upper == "/SYSTEM.CNF" || is_executable(&bytes) {
                excluded.push(e.path.clone());
                continue;
            }
            data.push((e.path.clone(), raw, bytes));
        }
        let mut executables: Vec<String> = excluded
            .iter()
            .filter(|p| !p.eq_ignore_ascii_case("/SYSTEM.CNF"))
            .map(|p| p.trim_start_matches('/').rsplit('/').next().unwrap_or(p).to_string())
            .collect();
        executables.dedup();
        let s = Setting {
            media: Some(media.clone()),
            executables: executables.clone(),
            psx_root: Some(absolute(&ctx.root)),
        };
        let options = TemplateOptions {
            region: self.region.unwrap_or_default(),
            fields_per_tick: self.fields_per_tick.unwrap_or(1),
        };
        let name = self.name.clone().unwrap_or_else(|| {
            let n: String =
                game.to_ascii_lowercase().chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '-' }).collect();
            let n = n.trim_matches('-').to_string();
            if n.starts_with(|c: char| c.is_ascii_lowercase()) { n } else { format!("game-{n}") }
        });
        for p in [&repos.investigator, &repos.implementer, &repos.reviewer] {
            std::fs::create_dir_all(p).map_err(|e| Error::io(p, e))?;
        }
        let inv = create(&repos.investigator, Role::Investigator, &game, &repos, &s, None, options)?;
        let imp = create(&repos.implementer, Role::Implementer, &game, &repos, &s, Some(&name), options)?;
        let rev = create(&repos.reviewer, Role::Reviewer, &game, &repos, &s, None, options)?;
        // The investigator's disc and BIOS, linked from where they are.
        let mut linked = vec![disc.source.clone()];
        linked.extend(disc.files.iter().map(|f| f.path.clone()));
        linked.sort();
        linked.dedup();
        for f in &linked {
            let to = repos.investigator.join("media").join(f.file_name().unwrap_or_default());
            if !to.exists() {
                link_or_copy(&absolute(f), &to)?;
            }
        }
        let mut bios = Vec::new();
        for e in std::fs::read_dir(ctx.root.join("bios")).into_iter().flatten().flatten() {
            if e.path().is_file() {
                link_or_copy(&absolute(&e.path()), &repos.investigator.join("bios").join(e.file_name()))?;
                bios.push(e.file_name().to_string_lossy().into_owned());
            }
        }
        // The implementer's data files.
        let mut bytes = 0usize;
        for (path, raw, b) in &data {
            let rel = path.trim_start_matches('/');
            let rel = if *raw { format!("{rel}.raw") } else { rel.to_string() };
            let to = repos.implementer.join("data").join(rel);
            if let Some(d) = to.parent() {
                std::fs::create_dir_all(d).map_err(|e| Error::io(d, e))?;
            }
            std::fs::write(&to, b).map_err(|e| Error::io(&to, e))?;
            bytes += b.len();
        }
        let mut git = Vec::new();
        for p in [&repos.investigator, &repos.implementer, &repos.reviewer] {
            let ok = std::process::Command::new("git")
                .args(["init", "-q"])
                .current_dir(p)
                .status()
                .is_ok_and(|s| s.success());
            git.push(json!({"repository": p, "git_init": ok}));
        }
        let mcp = |v: &Value| v["written"].as_array().map(|w| w.len()).unwrap_or(0);
        Ok(json!({
            "game": game, "package": name,
            "repositories": {"investigator": repos.investigator, "implementer": repos.implementer, "reviewer": repos.reviewer},
            "investigator": {"written": mcp(&inv), "media_files": linked.len(), "bios_files": bios.len()},
            "implementer": {"written": mcp(&imp), "template": imp["template"]["owned_files"], "data_files": data.len(), "data_bytes": bytes,
                            "data_raw": data.iter().filter(|d| d.1).map(|d| &d.0).collect::<Vec<_>>()},
            "reviewer": {"written": mcp(&rev)},
            "left_out": excluded, "unreadable": failed, "git": git,
            "cautions": CAUTIONS,
            "note": "data/ holds every disc file but the executables; a data file may still hold code (overlays), which the implementer only reads as far as an FMT spec describes it",
            "next": [
                format!("the person starts the investigator's session in {}", repos.investigator.display()),
                "the investigator never starts the implementer's or the reviewer's session: the person starts each in its own folder".to_string(),
                "BIOS: psx bios install in the investigator's repository if bios/ is empty".to_string(),
            ],
        }))
    }
}

impl Op for PortInitArgs {
    fn exec(self, ctx: &Ctx) -> Outcome {
        Outcome::result(self.run(ctx))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn implementer_guidance_has_no_reverse_engineering() {
        let mut files: Vec<(&'static str, String)> =
            crate::template::FILES.iter().map(|(r, t)| (*r, t.to_string())).collect();
        implementer_files(&mut files, "Some Game").unwrap();
        for (rel, text) in &files {
            // The root AGENTS.md names what the implementer must never use.
            let skip = ["README.md", "docs/template.md", "AGENTS.md"];
            if rel.ends_with(".md")
                && !rel.starts_with("vendor/")
                && !rel.starts_with("psx-bevy/")
                && !skip.contains(rel)
            {
                for word in ["MIPS", "psx run", "psx textures", "RAM address", "disassembl"] {
                    assert!(!text.contains(word), "{rel} still says {word:?}");
                }
                let addresses: Vec<_> = crate::leak::Checker::default()
                    .check(text, crate::leak::Form::Prose)
                    .into_iter()
                    .filter(|f| f.kind == "address")
                    .map(|f| f.text)
                    .collect();
                assert!(addresses.is_empty(), "{rel} still names {addresses:?}");
            }
        }
        let agents = &files.iter().find(|f| f.0 == "AGENTS.md").unwrap().1;
        assert!(agents.contains("Some Game") && agents.contains("States are enums"));
        assert!(files.iter().any(|f| f.0 == "questions/README.md"));
        assert!(files.iter().find(|f| f.0 == ".gitignore").unwrap().1.contains("\ndata/\n"));
    }

    /// Every file under templates/roles must be embedded, or the role
    /// silently lacks it.
    #[test]
    fn embeds_every_role_file() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../templates/roles");
        let replacing = [
            "AGENTS.md",
            "docs/evidence.md",
            "docs/engine-model.md",
            "game/testing.md",
            "game/code.md",
            "engine/cut.md",
        ];
        for (dir, list) in [("investigator", INVESTIGATOR), ("reviewer", REVIEWER), ("implementer", IMPLEMENTER_EXTRA)]
        {
            let base = root.join(dir);
            for p in crate::publish::files_under(&base) {
                let rel = p.strip_prefix(&base).unwrap().to_string_lossy().replace('\\', "/");
                let known =
                    list.iter().any(|(r, _)| *r == rel) || (dir == "implementer" && replacing.contains(&rel.as_str()));
                assert!(known, "templates/roles/{dir}/{rel} is not embedded");
            }
        }
    }

    #[test]
    fn boot_paths() {
        assert_eq!(boot_path("cdrom:\\SLUS_005.67;1"), "/SLUS_005.67");
        assert_eq!(boot_path("cdrom:PSX.EXE;1"), "/PSX.EXE");
        assert_eq!(boot_path("cdrom0:\\EXE\\MAIN.EXE;1"), "/EXE/MAIN.EXE");
    }

    #[test]
    fn game_names() {
        assert_eq!(
            game_name(Path::new("/m/Tomb Raider II - Starring Lara Croft (USA) (Rev 3).cue")),
            "Tomb Raider II - Starring Lara Croft"
        );
        assert_eq!(game_name(Path::new("/m/Crash Bandicoot.cue")), "Crash Bandicoot");
        assert_eq!(game_name(Path::new("/m/Metal Gear Solid [Disc 1].chd")), "Metal Gear Solid");
    }

    #[test]
    fn host_keys() {
        assert_eq!(host_key(Path::new("/home/a/projects/re_x/psx")), "-home-a-projects-re-x-psx");
    }
}
