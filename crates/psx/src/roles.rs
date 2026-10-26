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
    /// Ports written before the protocol by someone who read the original's
    /// code: the implementer never reads them, and their notes are the
    /// investigator's.
    pub legacy: Vec<PathBuf>,
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
            for l in &s.legacy {
                deny_repo(&mut deny, l, true);
            }
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
                    // Its servers: clean mode only.
                    "Edit(.mcp.json)",
                    "Write(.mcp.json)",
                    "Bash(*.mcp.json*)",
                ]
                .map(String::from),
            );
            json!({
                "permissions": {"allow": ["mcp__clean"], "deny": deny},
                "enabledMcpjsonServers": ["clean"],
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

/// psx-clean, beside psx.
fn clean_exe() -> PathBuf {
    let psx = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("psx"));
    psx.with_file_name(format!("psx-clean{}", std::env::consts::EXE_SUFFIX))
}

/// Clean mode for the implementer, on the investigator's project: the only
/// part of psx it reaches.
fn clean_mcp_json(repos: &Repos) -> Value {
    json!({"mcpServers": {"clean": {
        "command": clean_exe(),
        "args": ["--investigator", repos.investigator, "mcp"],
    }}})
}

/// The implementer's message check settings.
fn barrier_json(repos: &Repos, s: &Setting) -> Value {
    let mut v = json!({
        "investigator": repos.investigator, "names": s.executables,
        "published": ["specs", "questions"], "log": "notes/guidance.jsonl", "confirm_prefix": "[cross]",
    });
    if !s.legacy.is_empty() {
        v["notes"] = json!(barrier_notes(repos, s));
    }
    v
}

/// The text a message must not repeat: the investigator's notes, and a
/// legacy port's notes and docs.
fn barrier_notes(repos: &Repos, s: &Setting) -> Vec<PathBuf> {
    let mut notes = vec![repos.investigator.join("notes")];
    for l in &s.legacy {
        notes.extend(["notes", "docs"].map(|d| l.join(d)).into_iter().filter(|p| p.is_dir()));
    }
    notes
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
    if !s.legacy.is_empty() {
        t += &legacy_toml(&s.legacy);
    }
    t
}

fn legacy_toml(legacy: &[PathBuf]) -> String {
    let q = |p: &PathBuf| serde_json::to_string(&p.display().to_string()).unwrap_or_default();
    format!(
        "\n# Ports written before the protocol, which the implementer never reads.\n[legacy]\nports = [{}]\n",
        legacy.iter().map(q).collect::<Vec<_>>().join(", ")
    )
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
            for p in [".claude/settings.json", ".claude/barrier.json", ".mcp.json"] {
                if dest.join(p).exists() {
                    return Err(Error::invalid(format!("{} already exists", dest.join(p).display())));
                }
            }
            out["template"] = crate::template::create_for(dest, name, options, Some(game))?;
            write_new(dest, ".claude/barrier.json", &pretty(&barrier_json(repos, s)), &mut written)?;
            write_new(dest, ".mcp.json", &pretty(&clean_mcp_json(repos)), &mut written)?;
            if !clean_exe().is_file() {
                out["warnings"] = json!([format!(
                    "{} is not built: cargo build --release -p psx-clean, or clean mode will not start",
                    clean_exe().display()
                )]);
            }
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

/// What a disc gives a port: its executables, kept from the implementer,
/// and every other file as its data.
struct DiscFiles {
    /// The executables' names, refused at the barrier.
    executables: Vec<String>,
    /// The executables' and SYSTEM.CNF's paths.
    excluded: Vec<String>,
    /// (path, read raw, bytes)
    data: Vec<(String, bool, Vec<u8>)>,
    failed: Vec<Value>,
    /// The image's own files (the cue and its tracks).
    sources: Vec<PathBuf>,
}

fn disc_files(media: &Path) -> crate::Result<DiscFiles> {
    use psx_media::disc::Disc;
    use psx_media::iso9660::FileSystem;
    let disc = Disc::open(media)?;
    let fs = FileSystem::read(&disc)?;
    // The executables: SYSTEM.CNF's boot file, and every PS-X EXE on the disc.
    let cnf = fs.find("/SYSTEM.CNF").and_then(|e| fs.read_file(&disc, e).ok());
    let boot = cnf
        .as_deref()
        .map(psx_media::system_cnf::SystemCnf::parse)
        .and_then(|c| c.boot)
        .unwrap_or_else(|| psx_media::system_cnf::DEFAULT_BOOT.into());
    let boot = boot_path(&boot);
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
    let mut sources = vec![disc.source.clone()];
    sources.extend(disc.files.iter().map(|f| f.path.clone()));
    sources.sort();
    sources.dedup();
    Ok(DiscFiles { executables, excluded, data, failed, sources })
}

/// The data files into the implementer's data/, with the disc's paths;
/// their bytes.
fn write_data(implementer: &Path, data: &[(String, bool, Vec<u8>)]) -> crate::Result<usize> {
    let mut bytes = 0usize;
    for (path, raw, b) in data {
        let rel = path.trim_start_matches('/');
        let rel = if *raw { format!("{rel}.raw") } else { rel.to_string() };
        let to = implementer.join("data").join(rel);
        if let Some(d) = to.parent() {
            std::fs::create_dir_all(d).map_err(|e| Error::io(d, e))?;
        }
        std::fs::write(&to, b).map_err(|e| Error::io(&to, e))?;
        bytes += b.len();
    }
    Ok(bytes)
}

impl PortInitArgs {
    fn run(&self, ctx: &Ctx) -> crate::Result<Value> {
        let media = absolute(&ctx.root.join(&self.media));
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
        let DiscFiles { executables, excluded, data, failed, sources: linked } = disc_files(&media)?;
        let s = Setting {
            media: Some(media.clone()),
            executables: executables.clone(),
            psx_root: Some(absolute(&ctx.root)),
            legacy: Vec::new(),
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
        let bytes = write_data(&repos.implementer, &data)?;
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
                            "data_raw": data.iter().filter(|d| d.1).map(|d| &d.0).collect::<Vec<_>>(),
                            "warnings": imp["warnings"]},
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

/// `port adopt`: the protocol's files added to repositories that already
/// exist, as for a port begun before psx had them, or one being moved under
/// the protocol. Nothing is overwritten: a missing file is written, a JSON
/// setting gains what it lacks (deny rules, servers, the message check), and
/// whatever only a person can settle is listed.
#[derive(clap::Args, Clone, Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct PortAdoptArgs {
    /// The investigator's repository.
    #[arg(long)]
    pub investigator: PathBuf,
    /// The implementer's repository (default: the one protocol.toml names).
    #[arg(long)]
    #[serde(default)]
    pub implementer: Option<PathBuf>,
    /// The reviewer's repository, created if it does not exist (default: the one protocol.toml names).
    #[arg(long)]
    #[serde(default)]
    pub reviewer: Option<PathBuf>,
    /// A port written before the protocol by someone who read the original's code; repeatable.
    /// The implementer is denied it, and the message check refuses text from its notes and docs.
    #[arg(long)]
    #[serde(default)]
    pub legacy: Vec<PathBuf>,
    /// The disc image (default: protocol.toml's).
    #[arg(long)]
    #[serde(default)]
    pub media: Option<PathBuf>,
    /// The game's name in the roles' instructions (default: protocol.toml's, else the disc image's name).
    #[arg(long)]
    #[serde(default)]
    pub game: Option<String>,
    /// Say what would change, and change nothing.
    #[arg(long)]
    #[serde(default)]
    pub dry_run: bool,
}

/// What adopting changed, or would.
#[derive(Default)]
struct Adoption {
    dry_run: bool,
    written: Vec<String>,
    merged: Vec<String>,
    /// Files kept as they are though psx would write them differently.
    differing: Vec<String>,
    todo: Vec<String>,
}

impl Adoption {
    fn put(&self, p: &Path, text: &str) -> crate::Result<()> {
        if self.dry_run {
            return Ok(());
        }
        if let Some(d) = p.parent() {
            std::fs::create_dir_all(d).map_err(|e| Error::io(d, e))?;
        }
        std::fs::write(p, text).map_err(|e| Error::io(p, e))
    }

    /// `text` at `rel` unless a file is there already.
    fn file(&mut self, repo: &Path, rel: &str, text: &str) -> crate::Result<()> {
        let p = repo.join(rel);
        match std::fs::read_to_string(&p) {
            Ok(have) => {
                if have != text {
                    self.differing.push(p.display().to_string());
                }
                Ok(())
            }
            Err(_) => {
                self.put(&p, text)?;
                self.written.push(p.display().to_string());
                Ok(())
            }
        }
    }

    /// A JSON file: `ours` if there is none, else what it has with
    /// `merge` applied.
    fn json(
        &mut self,
        repo: &Path,
        rel: &str,
        ours: &Value,
        merge: impl FnOnce(&mut Value, &Value, &mut Vec<String>),
    ) -> crate::Result<()> {
        let p = repo.join(rel);
        let Ok(bytes) = std::fs::read(&p) else {
            self.put(&p, &pretty(ours))?;
            self.written.push(p.display().to_string());
            return Ok(());
        };
        let mut have: Value =
            serde_json::from_slice(&bytes).map_err(|e| Error::malformed(format!("{}: {e}", p.display())))?;
        if !have.is_object() {
            return Err(Error::malformed(format!("{}: not a JSON object", p.display())));
        }
        let before = have.clone();
        let mut notes = Vec::new();
        merge(&mut have, ours, &mut notes);
        self.todo.extend(notes.into_iter().map(|n| format!("{}: {n}", p.display())));
        if have != before {
            self.put(&p, &pretty(&have))?;
            self.merged.push(p.display().to_string());
        }
        Ok(())
    }
}

/// The strings of `ours` that `have` lacks, appended to it.
fn union(have: &mut Value, ours: &Value) {
    let Some(ours) = ours.as_array() else { return };
    if !have.is_array() {
        *have = json!([]);
    }
    let list = have.as_array_mut().expect("an array");
    for v in ours {
        if !list.contains(v) {
            list.push(v.clone());
        }
    }
}

/// An agent host's settings: our permissions and servers added to theirs,
/// our message check unless it has its own.
fn merge_settings(have: &mut Value, ours: &Value, notes: &mut Vec<String>) {
    for k in ["deny", "allow"] {
        if ours["permissions"].get(k).is_some() {
            if !have["permissions"].is_object() {
                have["permissions"] = json!({});
            }
            union(&mut have["permissions"][k], &ours["permissions"][k]);
        }
    }
    if let Some(servers) = ours.get("enabledMcpjsonServers") {
        let extra: Vec<String> = have["enabledMcpjsonServers"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|s| !servers.as_array().is_some_and(|o| o.contains(s)))
            .filter_map(|s| s.as_str().map(str::to_string))
            .collect();
        if !extra.is_empty() {
            notes.push(format!("enables {} besides clean mode: take them out", extra.join(", ")));
        }
        union(&mut have["enabledMcpjsonServers"], servers);
    }
    let Some(ours_hook) = ours["hooks"]["UserPromptSubmit"].as_array() else { return };
    let commands: Vec<String> = have["hooks"]["UserPromptSubmit"]
        .as_array()
        .into_iter()
        .flatten()
        .flat_map(|h| h["hooks"].as_array().cloned().unwrap_or_default())
        .filter_map(|h| h["command"].as_str().map(str::to_string))
        .collect();
    if commands.iter().any(|c| c.contains("barrier hook")) {
        return;
    }
    if commands.is_empty() {
        if !have["hooks"].is_object() {
            have["hooks"] = json!({});
        }
        have["hooks"]["UserPromptSubmit"] = json!(ours_hook);
    } else {
        let psx = ours_hook[0]["hooks"][0]["command"].as_str().unwrap_or_default();
        notes.push(format!(
            "its own message check is kept ({}); psx's is `{psx}`, which logs to the same notes/guidance.jsonl: put it in place of yours once you have checked it does all yours does",
            commands.join("; ")
        ));
    }
}

/// MCP servers: ours added; any other the role should not have, listed.
fn merge_mcp(have: &mut Value, ours: &Value, only_ours: bool, notes: &mut Vec<String>) {
    if !have["mcpServers"].is_object() {
        have["mcpServers"] = json!({});
    }
    for (name, server) in ours["mcpServers"].as_object().into_iter().flatten() {
        match have["mcpServers"].get(name) {
            None => have["mcpServers"][name] = server.clone(),
            Some(s) if s != server => {
                notes.push(format!("server {name} is kept as it is; psx would write {server}"));
            }
            Some(_) => {}
        }
    }
    if only_ours {
        let extra: Vec<&String> =
            have["mcpServers"].as_object().into_iter().flatten().map(|(k, _)| k).filter(|k| *k != "clean").collect();
        if !extra.is_empty() {
            notes.push(format!(
                "servers besides clean mode ({}): take them out",
                extra.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(", ")
            ));
        }
    }
}

/// The message check's settings: what it lacks, and the legacy notes in
/// whichever key it reads them from.
fn merge_barrier(have: &mut Value, ours: &Value, legacy: &[PathBuf]) {
    // The names the first message check used for notes and published.
    let alias = |k: &str| match k {
        "published" => Some("published_paths"),
        _ => None,
    };
    for (k, v) in ours.as_object().into_iter().flatten() {
        if k == "notes" || alias(k).is_some_and(|a| have.get(a).is_some()) {
            continue;
        }
        if have.get(k).is_none() {
            have[k] = v.clone();
        } else if k == "names" {
            union(&mut have[k], v);
        }
    }
    if legacy.is_empty() {
        return;
    }
    let key = if have.get("notes").is_none() && have.get("dirty_paths").is_some() { "dirty_paths" } else { "notes" };
    union(&mut have[key], &ours["notes"]);
}

fn read_toml(p: &Path) -> Option<toml::Value> {
    toml::from_str(&std::fs::read_to_string(p).ok()?).ok()
}

impl PortAdoptArgs {
    fn run(&self, ctx: &Ctx) -> crate::Result<Value> {
        let at = |p: &Path| absolute(&ctx.root.join(p));
        let investigator = at(&self.investigator);
        if !investigator.is_dir() {
            return Err(Error::invalid(format!("{} is not a folder", investigator.display())));
        }
        let toml_path = investigator.join("protocol.toml");
        let have_toml = read_toml(&toml_path);
        if have_toml.is_none() && toml_path.exists() {
            return Err(Error::malformed(format!("{} does not parse", toml_path.display())));
        }
        let named = |k: &str| -> Option<PathBuf> {
            let p = PathBuf::from(have_toml.as_ref()?.get("repositories")?.get(k)?.as_str()?);
            Some(if p.is_absolute() { p } else { investigator.join(p) })
        };
        let need = |given: &Option<PathBuf>, k: &str| -> crate::Result<PathBuf> {
            given
                .as_deref()
                .map(at)
                .or_else(|| named(k).map(|p| absolute(&p)))
                .ok_or_else(|| Error::invalid(format!("give --{k}: the investigator's protocol.toml names no {k}")))
        };
        let repos = Repos {
            investigator: investigator.clone(),
            implementer: need(&self.implementer, "implementer")?,
            reviewer: need(&self.reviewer, "reviewer")?,
        };
        if !repos.implementer.is_dir() {
            return Err(Error::invalid(format!(
                "{} is not a folder: a new port is port init's",
                repos.implementer.display()
            )));
        }
        let legacy: Vec<PathBuf> = self.legacy.iter().map(|p| at(p)).collect();
        for l in &legacy {
            if !l.is_dir() {
                return Err(Error::invalid(format!("{} is not a folder", l.display())));
            }
            if [&repos.investigator, &repos.implementer, &repos.reviewer].contains(&l) {
                return Err(Error::invalid(format!(
                    "{} is one of the port's repositories; a legacy port is apart from them",
                    l.display()
                )));
            }
        }
        let toml_str =
            |t: &str, k: &str| -> Option<String> { Some(have_toml.as_ref()?.get(t)?.get(k)?.as_str()?.to_string()) };
        let media = self
            .media
            .as_deref()
            .map(at)
            .or_else(|| toml_str("disc", "media").map(|m| absolute(&investigator.join(m))));
        let disc = media.as_deref().map(disc_files).transpose()?;
        let game = self
            .game
            .clone()
            .or_else(|| have_toml.as_ref().and_then(|t| t.get("game")?.as_str().map(str::to_string)))
            .or_else(|| media.as_deref().map(game_name))
            .unwrap_or_else(|| "the game".into());
        let mut executables = disc.as_ref().map(|d| d.executables.clone()).unwrap_or_default();
        if executables.is_empty() {
            executables = have_toml
                .as_ref()
                .and_then(|t| t.get("disc")?.get("executables")?.as_array().cloned())
                .into_iter()
                .flatten()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect();
        }
        // psx's own folder (its analyses in .psx/, discs in media/), unless
        // it is one of the port's repositories.
        let root = absolute(&ctx.root);
        let psx_root = (![&repos.investigator, &repos.implementer, &repos.reviewer].contains(&&root)
            && !legacy.contains(&root)
            && (root.join(".psx").is_dir() || root.join("media").is_dir()))
        .then_some(root);
        let s = Setting { media: media.clone(), executables, psx_root, legacy: legacy.clone() };
        let mut a = Adoption { dry_run: self.dry_run, ..Adoption::default() };

        // The investigator's.
        let inv = &repos.investigator;
        for (rel, text) in INVESTIGATOR {
            a.file(inv, rel, &fill(text, &game, Some(&repos)))?;
        }
        if let Some(t) = &have_toml {
            let mut add = String::new();
            for (k, want) in [("implementer", &repos.implementer), ("reviewer", &repos.reviewer)] {
                match named(k).map(|p| absolute(&p)) {
                    Some(p) if p == *want => {}
                    Some(p) => a.todo.push(format!(
                        "{}: its {k} is {}, not {}; change it if that is wrong",
                        toml_path.display(),
                        p.display(),
                        want.display()
                    )),
                    None => a.todo.push(format!(
                        "{}: add {k} = {:?} under [repositories]",
                        toml_path.display(),
                        want.display().to_string()
                    )),
                }
            }
            if t.get("disc").is_none() && (s.media.is_some() || !s.executables.is_empty()) {
                let q = |v: &str| serde_json::to_string(v).unwrap_or_default();
                add += "\n[disc]\n";
                if let Some(m) = &s.media {
                    add += &format!("media = {}\n", q(&m.display().to_string()));
                }
                let names: Vec<String> = s.executables.iter().map(|e| q(e)).collect();
                add += &format!("executables = [{}]\n", names.join(", "));
            }
            let listed: Vec<PathBuf> = t
                .get("legacy")
                .and_then(|l| l.get("ports")?.as_array().cloned())
                .into_iter()
                .flatten()
                .filter_map(|v| v.as_str().map(|p| absolute(Path::new(p))))
                .collect();
            if t.get("legacy").is_none() && !legacy.is_empty() {
                add += &legacy_toml(&legacy);
            } else if let Some(l) = legacy.iter().find(|l| !listed.contains(l)) {
                a.todo.push(format!("{}: add {:?} to [legacy] ports", toml_path.display(), l.display().to_string()));
            }
            if !add.is_empty() {
                let text = std::fs::read_to_string(&toml_path).map_err(|e| Error::io(&toml_path, e))?;
                a.put(&toml_path, &format!("{}{add}", text.trim_end_matches('\n').to_string() + "\n"))?;
                a.merged.push(toml_path.display().to_string());
            }
        } else {
            a.put(&toml_path, &protocol_toml(&game, &repos, &s))?;
            a.written.push(toml_path.display().to_string());
        }
        a.json(inv, ".mcp.json", &mcp_json(&repos), |h, o, n| merge_mcp(h, o, false, n))?;
        a.json(inv, ".claude/settings.json", &settings(Role::Investigator, &repos, &s), merge_settings)?;

        // The reviewer's: a new repository, or what it lacks.
        let rev = &repos.reviewer;
        let mut git = Vec::new();
        if std::fs::read_dir(rev).map(|mut d| d.next().is_none()).unwrap_or(true) {
            if !self.dry_run {
                std::fs::create_dir_all(rev).map_err(|e| Error::io(rev, e))?;
                let made = create(rev, Role::Reviewer, &game, &repos, &s, None, TemplateOptions::default())?;
                for w in made["written"].as_array().into_iter().flatten().filter_map(Value::as_str) {
                    a.written.push(rev.join(w).display().to_string());
                }
                let ok = std::process::Command::new("git")
                    .args(["init", "-q"])
                    .current_dir(rev)
                    .status()
                    .is_ok_and(|s| s.success());
                git.push(json!({"repository": rev, "git_init": ok}));
            } else {
                a.written.push(format!("{} (a new reviewer's repository)", rev.display()));
            }
        } else {
            for (rel, text) in REVIEWER {
                a.file(rev, rel, &fill(text, &game, Some(&repos)))?;
            }
            a.json(rev, ".mcp.json", &mcp_json(&repos), |h, o, n| merge_mcp(h, o, false, n))?;
            a.json(rev, ".claude/settings.json", &settings(Role::Reviewer, &repos, &s), merge_settings)?;
        }

        // The implementer's.
        let imp = &repos.implementer;
        for (rel, text) in IMPLEMENTER_EXTRA {
            a.file(imp, rel, text)?;
        }
        a.file(imp, "AGENTS.md", &fill(IMPLEMENTER_AGENTS, &game, None))?;
        let barrier = barrier_json(&repos, &s);
        a.json(imp, ".claude/barrier.json", &barrier, |h, o, _| merge_barrier(h, o, &legacy))?;
        a.json(imp, ".mcp.json", &clean_mcp_json(&repos), |h, o, n| merge_mcp(h, o, true, n))?;
        a.json(imp, ".claude/settings.json", &settings(Role::Implementer, &repos, &s), merge_settings)?;
        let ignore = imp.join(".gitignore");
        let ignored = std::fs::read_to_string(&ignore).unwrap_or_default();
        if !ignored.lines().any(|l| matches!(l.trim(), "data/" | "/data/" | "data" | "/data")) {
            let text = format!(
                "{}\n# The game's data files, from the player's disc (never commit them).\ndata/\n",
                ignored.trim_end_matches('\n')
            );
            a.put(&ignore, text.trim_start_matches('\n'))?;
            a.merged.push(ignore.display().to_string());
        }
        let mut data_files = 0;
        match &disc {
            Some(d) if !imp.join("data").exists() => {
                if !self.dry_run {
                    write_data(imp, &d.data)?;
                }
                data_files = d.data.len();
                a.written.push(format!("{} ({} files)", imp.join("data").display(), d.data.len()));
            }
            Some(_) => {}
            None => a.todo.push(
                "give --media: without the disc no executable names reach the message check and protocol.toml, and data/ is not filled".into(),
            ),
        }

        // What only a person settles.
        if a.differing.iter().any(|f| f.ends_with("AGENTS.md")) {
            a.todo.push("AGENTS.md is kept where a role already had one (see differing): bring in what the role's current instructions add (psx template create --role ROLE into an empty folder writes them)".into());
        }
        if !imp.join(".psx-template.toml").is_file() {
            a.todo.push(format!(
                "{} is not from psx's template: clean mode replays cases with the port's own runner, so it needs a case test reading PSX_CASES and writing verdicts to PSX_RESULTS (the template's game/tests/cases.rs)",
                imp.display()
            ));
        } else if !crate::publish::files_under(&imp.join("game/tests"))
            .iter()
            .any(|p| std::fs::read_to_string(p).is_ok_and(|t| t.contains("cases_from_folder")))
        {
            a.todo.push("the implementer's game/tests has no cases_from_folder test: copy it from psx's template (game/tests/cases.rs) so clean mode's heldout can run the port's cases".into());
        }
        let has_specs = crate::publish::files_under(&imp.join("specs")).iter().any(|p| {
            p.extension().is_some_and(|e| e == "md")
                && p.file_name().is_some_and(|n| n != "README.md" && n != "INDEX.md" && n != "glossary.md")
        });
        if has_specs && !imp.join("notes/published.jsonl").is_file() {
            a.todo.push("the implementer has specs but no publishing log (notes/published.jsonl): they crossed by hand, so the ledger counts none; stage and cross them with psx publish once the reviewer approves".into());
        }
        let states = crate::publish::files_under(&imp.join("specs/states"));
        if !states.iter().any(|p| p.extension().is_some_and(|e| e == "json")) {
            a.todo.push("no start state is published: the investigator publishes the states clean mode records from (specs/states/, each with a description in game terms)".into());
        }
        let samples = inv.join("samples.toml");
        let template_samples = INVESTIGATOR.iter().find(|(r, _)| *r == "samples.toml").map(|f| f.1);
        // A dry run has not written it yet.
        if std::fs::read_to_string(&samples).map_or(self.dry_run, |t| Some(t.as_str()) == template_samples) {
            a.todo.push(format!(
                "{}: the field map is the template's; the investigator fills in the game's fields",
                samples.display()
            ));
        }
        if std::fs::read_dir(inv.join("bios")).map(|mut d| d.next().is_none()).unwrap_or(true) {
            a.todo.push("the investigator's bios/ is empty: psx bios install there".into());
        }
        if !clean_exe().is_file() {
            a.todo.push(format!(
                "{} is not built: cargo build --release -p psx-clean, or clean mode will not start",
                clean_exe().display()
            ));
        }
        if !legacy.is_empty() {
            a.todo.push("sort each legacy port's modules into keep, audit or rewrite, with the reviewer (guide topic cleanroom, Migrating a port that predates the protocol); copy into the implementer's repository only what the reviewer clears".into());
        }
        Ok(json!({
            "dry_run": self.dry_run, "game": game,
            "repositories": {"investigator": repos.investigator, "implementer": repos.implementer, "reviewer": repos.reviewer, "legacy": legacy},
            "written": a.written, "merged": a.merged, "differing": a.differing,
            "data_files": data_files, "executables": s.executables, "git": git,
            "todo": a.todo, "cautions": CAUTIONS,
        }))
    }
}

impl Op for PortAdoptArgs {
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

    #[test]
    fn the_implementer_reaches_clean_mode_and_nothing_else_of_psx() {
        let repos = Repos {
            investigator: PathBuf::from("/w/investigate"),
            implementer: PathBuf::from("/w/clean"),
            reviewer: PathBuf::from("/w/review"),
        };
        let s = settings(Role::Implementer, &repos, &Setting::default());
        let has = |k: &str, v: &str| s["permissions"][k].as_array().unwrap().iter().any(|x| x == v);
        assert!(has("allow", "mcp__clean") && has("deny", "mcp__psx"));
        assert!(has("deny", "Edit(.mcp.json)") && has("deny", "Edit(notes/**)"));
        assert_eq!(s["enabledMcpjsonServers"], json!(["clean"]));
        let m = clean_mcp_json(&repos);
        let server = &m["mcpServers"]["clean"];
        assert!(server["command"].as_str().unwrap().ends_with(&format!("psx-clean{}", std::env::consts::EXE_SUFFIX)));
        assert_eq!(server["args"], json!(["--investigator", "/w/investigate", "mcp"]));
        assert_eq!(m["mcpServers"].as_object().unwrap().len(), 1);
    }

    /// An existing port with its own access rules and message check: adopt
    /// adds to them and overwrites nothing, the legacy port is denied, and
    /// a second run changes nothing.
    #[test]
    fn adopt_extends_what_is_there() {
        let w = std::env::temp_dir().join(format!("psx-adopt-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&w);
        let put = |rel: &str, text: &str| {
            let p = w.join(rel);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, text).unwrap();
        };
        put("inv/notes/states.md", "how the states were made\n");
        put("old/notes/player.md", "the old port's notes\n");
        put("imp/AGENTS.md", "# Our own instructions\n");
        put(".psx/keep", "");
        put(
            "imp/.claude/settings.json",
            r#"{"permissions": {"deny": ["WebFetch", "Edit(notes/guidance.jsonl)"]},
                "hooks": {"UserPromptSubmit": [{"hooks": [{"type": "command", "command": "python3 check.py"}]}]}}"#,
        );
        put("imp/.claude/barrier.json", r#"{"dirty_paths": ["/elsewhere/notes"], "published_paths": ["specs"]}"#);
        put("imp/.gitignore", "target/\n/data/\n");
        let ctx = Ctx { root: w.clone(), cancel: None };
        let args = PortAdoptArgs {
            investigator: "inv".into(),
            implementer: Some("imp".into()),
            reviewer: Some("rev".into()),
            legacy: vec!["old".into()],
            media: None,
            game: Some("Some Game".into()),
            dry_run: false,
        };
        let dry = PortAdoptArgs { dry_run: true, ..args.clone() }.run(&ctx).unwrap();
        assert!(!w.join("inv/protocol.toml").exists() && !w.join("rev").exists(), "{dry}");
        let v = args.run(&ctx).unwrap();
        let (inv, imp, old) = (absolute(&w.join("inv")), absolute(&w.join("imp")), absolute(&w.join("old")));

        let settings: Value =
            serde_json::from_str(&std::fs::read_to_string(imp.join(".claude/settings.json")).unwrap()).unwrap();
        let deny: Vec<&str> =
            settings["permissions"]["deny"].as_array().unwrap().iter().filter_map(|d| d.as_str()).collect();
        assert_eq!(&deny[..2], ["WebFetch", "Edit(notes/guidance.jsonl)"]);
        assert_eq!(deny.iter().filter(|d| **d == "WebFetch").count(), 1);
        assert!(deny.contains(&format!("Read(/{}/**)", old.display()).as_str()));
        assert!(deny.contains(&format!("Read(/{}/**)", inv.display()).as_str()) && deny.contains(&"mcp__psx"));
        assert_eq!(settings["permissions"]["allow"], json!(["mcp__clean"]));
        // Their message check stays; psx's waits on the person.
        let hooks = settings["hooks"]["UserPromptSubmit"].as_array().unwrap();
        assert_eq!(hooks.len(), 1);
        assert_eq!(hooks[0]["hooks"][0]["command"], "python3 check.py");
        let todo = v["todo"].as_array().unwrap();
        let says = |w: &str| todo.iter().any(|t| t.as_str().unwrap().contains(w));
        assert!(says("its own message check is kept") && says("legacy port") && says("give --media"), "{v}");
        assert!(says("no start state") && says("AGENTS.md is kept"));
        let barrier: Value =
            serde_json::from_str(&std::fs::read_to_string(imp.join(".claude/barrier.json")).unwrap()).unwrap();
        assert_eq!(barrier["investigator"], json!(inv));
        assert_eq!(barrier["dirty_paths"], json!(["/elsewhere/notes", inv.join("notes"), old.join("notes")]));
        assert!(barrier.get("notes").is_none());
        assert_eq!(std::fs::read_to_string(imp.join("AGENTS.md")).unwrap(), "# Our own instructions\n");
        assert_eq!(std::fs::read_to_string(imp.join(".gitignore")).unwrap(), "target/\n/data/\n");
        assert!(imp.join(".mcp.json").is_file() && imp.join("questions/README.md").is_file());
        let toml = std::fs::read_to_string(inv.join("protocol.toml")).unwrap();
        assert!(toml.contains("[legacy]") && toml.contains(&old.display().to_string()), "{toml}");
        assert!(w.join("rev/AGENTS.md").is_file() && w.join("rev/.claude/settings.json").is_file());

        // Nothing left to add.
        let again = PortAdoptArgs { implementer: None, reviewer: None, ..args }.run(&ctx).unwrap();
        assert_eq!(again["written"], json!([]), "{again}");
        assert_eq!(again["merged"], json!([]), "{again}");
        let _ = std::fs::remove_dir_all(&w);
    }
}
