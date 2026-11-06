//! The port's workspace (`templates/base`): written by `psx port init`,
//! tracked by `port init --existing`, brought up to date by `psx port
//! update`.
//!
//! The template holds only what connects the port to psx: a game library
//! implementing `psx_scenario::Port`, its scenarios test and `--replay`,
//! the importers and the glossary. `vendor/` is tracked: `update` replaces
//! the files the project hasn't changed and writes `<file>.psx-new` beside
//! the ones it has. Everything else (`game/`, `tools/`, `glossary/`, root
//! manifests) is seeded once and belongs to the project.
//! `.psx-template.toml` records the version and, per file, the template
//! version the project's copy is based on, so `update` can tell untouched
//! files from edited ones.
//!
//! Workspaces made from the Bevy template (`template = "psx-bevy"`) keep
//! their `psx-bevy/` as their own code: `update` stops tracking it and
//! brings only `vendor/` up to date.

mod addon;
pub use addon::PortAddArgs;

use crate::Error;
use std::collections::BTreeMap;
use std::path::Path;

use crate::commands::{Ctx, Op, Outcome};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

#[derive(clap::Args, Deserialize, JsonSchema)]
pub struct TemplateUpdateArgs {
    /// Report changes without writing.
    #[arg(long)]
    #[serde(default)]
    pub dry_run: bool,
}

impl Op for TemplateUpdateArgs {
    /// Updates the project at the project root (`--root`, or the MCP project).
    fn exec(self, ctx: &Ctx) -> Outcome {
        Outcome::result(update(&ctx.root, self.dry_run))
    }
}

macro_rules! files {
    ($($path:literal),* $(,)?) => {
        &[$(($path, include_str!(concat!("../../../templates/base/", $path)))),*]
    };
}

pub(crate) const FILES: &[(&str, &str)] = files![
    ".gitignore",
    "AGENTS.md",
    "Cargo.lock",
    "Cargo.toml",
    "README.md",
    "psx-port.toml",
    "rustfmt.toml",
    "game/Cargo.toml",
    "game/build.rs",
    "game/src/lib.rs",
    "game/src/main.rs",
    "game/tests/scenarios.rs",
    "glossary/Cargo.toml",
    "glossary/src/lib.rs",
    "tools/import/Cargo.toml",
    "tools/import/src/lib.rs",
    "tools/import/src/main.rs",
    "vendor/LICENSE-APACHE",
    "vendor/LICENSE-MIT",
    "vendor/README.md",
    "vendor/psx-assets/Cargo.toml",
    "vendor/psx-assets/src/adpcm.rs",
    "vendor/psx-assets/src/archive.rs",
    "vendor/psx-assets/src/gltf.rs",
    "vendor/psx-assets/src/le.rs",
    "vendor/psx-assets/src/lib.rs",
    "vendor/psx-assets/src/model_document.rs",
    "vendor/psx-assets/src/scan.rs",
    "vendor/psx-assets/src/tim.rs",
    "vendor/psx-assets/src/tmd.rs",
    "vendor/psx-assets/src/upload.rs",
    "vendor/psx-assets/src/vab.rs",
    "vendor/psx-capture/Cargo.toml",
    "vendor/psx-capture/src/abi.rs",
    "vendor/psx-capture/src/lib.rs",
    "vendor/psx-fixed/Cargo.toml",
    "vendor/psx-fixed/src/angle.rs",
    "vendor/psx-fixed/src/lib.rs",
    "vendor/psx-fixed/src/mode.rs",
    "vendor/psx-fixed/src/num.rs",
    "vendor/psx-fixed/src/tests.rs",
    "vendor/psx-fixed/src/vec.rs",
    "vendor/psx-fixed/src/word.rs",
    "vendor/psx-gte/Cargo.toml",
    "vendor/psx-gte/src/gte.rs",
    "vendor/psx-gte/src/lib.rs",
    "vendor/psx-gte/src/math.rs",
    "vendor/psx-gte/src/tests.rs",
    "vendor/psx-gte/src/types.rs",
    "vendor/psx-gte/src/unr.rs",
    "vendor/psx-media/Cargo.toml",
    "vendor/psx-media/src/chd.rs",
    "vendor/psx-media/src/cue.rs",
    "vendor/psx-media/src/disc.rs",
    "vendor/psx-media/src/error.rs",
    "vendor/psx-media/src/exe.rs",
    "vendor/psx-media/src/iso9660.rs",
    "vendor/psx-media/src/le.rs",
    "vendor/psx-media/src/lib.rs",
    "vendor/psx-media/src/memmap.rs",
    "vendor/psx-media/src/source.rs",
    "vendor/psx-media/src/str_video.rs",
    "vendor/psx-media/src/subq.rs",
    "vendor/psx-media/src/system_cnf.rs",
    "vendor/psx-scenario/Cargo.toml",
    "vendor/psx-scenario/src/lib.rs",
    "vendor/psx-scenario/src/protocol.rs",
    "vendor/psx-scenario/src/release.rs",
    "vendor/psx-scenario/src/replay.rs",
    "vendor/psx-scenario/src/runner.rs",
    "vendor/psx-scenario/src/rust.rs",
    "vendor/psx-scenario/src/scenario.rs",
    "vendor/psx-scenario/src/value.rs",
];

/// Files under this prefix are tracked; `update` replaces the untouched ones.
/// `vendor/` holds copies of the toolkit's psx-media, psx-assets, psx-fixed,
/// psx-gte, psx-scenario and psx-capture (not to be edited).
const VENDOR_PREFIX: &str = "vendor/";
/// The Bevy template's tracked base, which psx no longer ships: a manifest
/// may still list it, and `update` lets it go.
const PSX_BEVY_PREFIX: &str = "psx-bevy/";
/// The template's name in the manifest.
const TEMPLATE: &str = "base";
/// The Bevy template's name in older workspaces' manifests.
const BEVY_TEMPLATE: &str = "psx-bevy";
/// Seeded files `create` skips (and reports) when the destination already has them.
const KEEP_EXISTING: &[&str] = &["AGENTS.md", "README.md", ".gitignore"];
pub const MANIFEST: &str = ".psx-template.toml";
/// Where `update` writes new versions of seeded files for the project to merge.
const SEEDED_UPDATES: &str = ".psx/template-update";

/// The executable's name in the template (`game/Cargo.toml`), which `create`
/// replaces with the game's; the package itself is always `game`.
const TEMPLATE_BIN: &str = "psx-game";
/// The lines of the template's `game/src/lib.rs` that `create` rewrites.
const REGION_LINE: &str = "pub const REGION: Region = Region::Ntsc;";
const FIELDS_PER_TICK_LINE: &str = "pub const FIELDS_PER_TICK: u32 = 1;";

/// Video region (`REGION` in game/src/lib.rs).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, clap::ValueEnum, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum TemplateRegion {
    /// 60 Hz display.
    #[default]
    Ntsc,
    /// 50 Hz display.
    Pal,
}

impl TemplateRegion {
    fn name(self) -> &'static str {
        match self {
            TemplateRegion::Ntsc => "ntsc",
            TemplateRegion::Pal => "pal",
        }
    }

    fn rust(self) -> &'static str {
        match self {
            TemplateRegion::Ntsc => "Region::Ntsc",
            TemplateRegion::Pal => "Region::Pal",
        }
    }

    fn hz(self) -> f64 {
        match self {
            TemplateRegion::Ntsc => 60.0,
            TemplateRegion::Pal => 50.0,
        }
    }
}

/// Settings written into the generated project.
#[derive(Clone, Copy, Debug)]
pub struct TemplateOptions {
    pub region: TemplateRegion,
    /// Video fields (vblanks) per logic tick, 1 to 60.
    pub fields_per_tick: u32,
}

impl Default for TemplateOptions {
    fn default() -> Self {
        Self { region: TemplateRegion::Ntsc, fields_per_tick: 1 }
    }
}

/// `.psx-template.toml`.
#[derive(Serialize, Deserialize, Debug, PartialEq)]
struct Manifest {
    template: String,
    /// The psx build that last wrote the template's files.
    psx: String,
    package: String,
    region: TemplateRegion,
    fields_per_tick: u32,
    /// Template-owned files and the sha256 of the content the template wrote.
    owned: BTreeMap<String, String>,
    /// Seeded files and the sha256 of the template's version of them.
    seeded: BTreeMap<String, String>,
    /// Add-ons `psx port add` wrote, in the order added.
    addons: Vec<String>,
}

impl Manifest {
    fn options(&self) -> TemplateOptions {
        TemplateOptions { region: self.region, fields_per_tick: self.fields_per_tick }
    }

    fn write(&self, dest: &Path) -> crate::Result<()> {
        let header = "# Written by `psx port init` and `psx port update`. `update` compares these hashes\n\
                      # with the files to tell untouched template files from edited ones.\n";
        let body = toml_like(self);
        let path = dest.join(MANIFEST);
        std::fs::write(&path, format!("{header}{body}")).map_err(|e| Error::io(&path, e))
    }

    fn read(dest: &Path) -> crate::Result<Self> {
        let path = dest.join(MANIFEST);
        let text = std::fs::read_to_string(&path).map_err(|e| {
            Error::malformed(format!(
                "{}: {e}; projects created before the workspace template need `psx port init --existing`",
                path.display()
            ))
        })?;
        parse_toml_like(&text).map_err(|e| Error::malformed(format!("{}: {e}", path.display())))
    }
}

/// The manifest as a small, fixed TOML subset (string values, two tables),
/// so psx needs no TOML dependency.
fn toml_like(m: &Manifest) -> String {
    let mut out = format!(
        "template = {:?}\npsx = {:?}\npackage = {:?}\nregion = {:?}\nfields_per_tick = {}\n",
        m.template,
        m.psx,
        m.package,
        m.region.name(),
        m.fields_per_tick
    );
    if !m.addons.is_empty() {
        out.push_str(&format!("addons = {:?}\n", m.addons.join(",")));
    }
    for (table, entries) in [("owned", &m.owned), ("seeded", &m.seeded)] {
        out.push_str(&format!("\n[{table}]\n"));
        for (path, hash) in entries {
            out.push_str(&format!("{path:?} = {hash:?}\n"));
        }
    }
    out
}

fn parse_toml_like(text: &str) -> Result<Manifest, String> {
    let mut top = BTreeMap::new();
    let (mut owned, mut seeded) = (BTreeMap::new(), BTreeMap::new());
    let mut table = None;
    for (n, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some(name) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            table = Some(name.to_string());
            continue;
        }
        let (key, value) = line.split_once(" = ").ok_or_else(|| format!("line {}: expected key = value", n + 1))?;
        let unquote = |s: &str| -> Result<String, String> {
            serde_json::from_str::<String>(s).or_else(|_| Ok::<_, String>(s.to_string()))
        };
        let (key, value) = (unquote(key)?, unquote(value)?);
        // File keys become paths that update writes and deletes: a manifest
        // from a cloned workspace must not reach outside it.
        if table.is_some() && !tracked_path_ok(&key) {
            return Err(format!("line {}: {key:?} is not a relative path inside the workspace", n + 1));
        }
        if table.as_deref() == Some("owned") && !is_owned(&key) && !key.starts_with(PSX_BEVY_PREFIX) {
            return Err(format!("line {}: {key:?} is not a template-owned path", n + 1));
        }
        match table.as_deref() {
            None => drop(top.insert(key, value)),
            Some("owned") => drop(owned.insert(key, value)),
            Some("seeded") => drop(seeded.insert(key, value)),
            Some(other) => return Err(format!("unknown table [{other}]")),
        }
    }
    if top.contains_key("role") {
        return Err("a clean-room role's workspace: use the last psx release with the clean room".into());
    }
    let get = |k: &str| top.get(k).cloned().ok_or_else(|| format!("missing {k}"));
    let region = match get("region")?.as_str() {
        "ntsc" => TemplateRegion::Ntsc,
        "pal" => TemplateRegion::Pal,
        other => return Err(format!("region {other:?} must be ntsc or pal")),
    };
    Ok(Manifest {
        template: get("template")?,
        psx: get("psx")?,
        package: get("package")?,
        region,
        fields_per_tick: get("fields_per_tick")?.parse().map_err(|e| format!("fields_per_tick: {e}"))?,
        owned,
        seeded,
        addons: top.get("addons").map(|a| a.split(',').map(str::to_string).collect()).unwrap_or_default(),
    })
}

fn psx_version() -> String {
    format!("{}+{}", env!("CARGO_PKG_VERSION"), env!("PSX_BUILD_ID"))
}

fn sha256(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    format!("sha256:{}", digest.iter().map(|b| format!("{b:02x}")).collect::<String>())
}

/// A manifest path: relative, `/`-separated, with no `.`, `..`, empty or
/// drive-like component.
fn tracked_path_ok(rel: &str) -> bool {
    !rel.is_empty() && rel.split('/').all(|c| !c.is_empty() && c != "." && c != ".." && !c.contains(['\\', ':', '\0']))
}

fn is_owned(rel: &str) -> bool {
    rel.starts_with(VENDOR_PREFIX)
}

/// Removes `rel` under `dest`, then the folders it leaves empty.
fn remove_tracked(dest: &Path, rel: &str) -> crate::Result<()> {
    let path = dest.join(rel);
    std::fs::remove_file(&path).map_err(|e| Error::io(&path, e))?;
    let mut dir = path.parent();
    while let Some(d) = dir.filter(|d| *d != dest) {
        if std::fs::remove_dir(d).is_err() {
            break;
        }
        dir = d.parent();
    }
    Ok(())
}

/// The template's `game/src/lib.rs` with `REGION` and `FIELDS_PER_TICK` set.
fn configure_lib(contents: &str, options: TemplateOptions) -> crate::Result<String> {
    if !contents.contains(REGION_LINE) || !contents.contains(FIELDS_PER_TICK_LINE) {
        return Err(Error::new(
            "internal",
            "template game/src/lib.rs no longer has the REGION / FIELDS_PER_TICK lines psx port init rewrites",
        ));
    }
    Ok(contents.replacen(REGION_LINE, &format!("pub const REGION: Region = {};", options.region.rust()), 1).replacen(
        FIELDS_PER_TICK_LINE,
        &format!("pub const FIELDS_PER_TICK: u32 = {};", options.fields_per_tick),
        1,
    ))
}

/// Every template file as this project gets it: package name, region and
/// tick rate filled in.
fn render(name: &str, options: TemplateOptions) -> crate::Result<Vec<(&'static str, String)>> {
    let files: Vec<(&'static str, String)> = FILES
        .iter()
        .map(|(rel, contents)| {
            let contents = match *rel {
                "game/Cargo.toml" => {
                    contents.replacen(&format!("name = \"{TEMPLATE_BIN}\""), &format!("name = \"{name}\""), 1)
                }
                "game/src/lib.rs" => configure_lib(contents, options)?,
                _ => contents.to_string(),
            };
            Ok((*rel, contents))
        })
        .collect::<crate::Result<_>>()?;
    Ok(files)
}

fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.chars().next().is_some_and(|c| c.is_ascii_lowercase())
        && name.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_')
        && !matches!(name, "glossary" | "import")
}

fn check_options(name: &str, options: TemplateOptions) -> crate::Result<()> {
    if !valid_name(name) {
        return Err(Error::invalid(format!(
            "package name {name:?} must start with a-z, contain only a-z, 0-9, '-' or '_', and not be \
             glossary or import; pass --name"
        )));
    }
    if !(1..=60).contains(&options.fields_per_tick) {
        return Err(Error::invalid(format!(
            "fields_per_tick {} must be between 1 and 60 (vblanks the original waits per game frame)",
            options.fields_per_tick
        )));
    }
    Ok(())
}

fn write_file(dest: &Path, rel: &str, contents: &[u8]) -> crate::Result<()> {
    let path = dest.join(rel);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| Error::io(parent, e))?;
    }
    std::fs::write(&path, contents).map_err(|e| Error::io(&path, e))
}

/// `name` from `--name`, else the destination folder's name.
pub fn package_name(dest: &Path, name: Option<&str>) -> String {
    match name {
        Some(n) => n.to_string(),
        None => dest
            .canonicalize()
            .ok()
            .as_deref()
            .unwrap_or(dest)
            .file_name()
            .map(|n| n.to_string_lossy().to_ascii_lowercase().replace([' ', '.'], "-"))
            .unwrap_or_else(|| TEMPLATE_BIN.to_string()),
    }
}

pub fn create(dest: &Path, name: Option<&str>, options: TemplateOptions) -> crate::Result<Value> {
    let name = package_name(dest, name);
    check_options(&name, options)?;
    let files = render(&name, options)?;
    // The destination may hold the disc and the user's own AGENTS.md: refuse
    // only when a file the template would write is already there.
    let mut skipped = Vec::new();
    let mut clashes = Vec::new();
    for (rel, _) in &files {
        if dest.join(rel).exists() {
            if KEEP_EXISTING.contains(rel) {
                skipped.push(*rel);
            } else {
                clashes.push(*rel);
            }
        }
    }
    if dest.join(MANIFEST).exists() {
        clashes.push(MANIFEST);
    }
    if !clashes.is_empty() {
        return Err(Error::invalid(format!(
            "{} already has {}; port init only adds files (psx port init --existing tracks an existing project)",
            dest.display(),
            clashes.join(", ")
        )));
    }
    let mut manifest = Manifest {
        template: TEMPLATE.into(),
        psx: psx_version(),
        package: name.clone(),
        region: options.region,
        fields_per_tick: options.fields_per_tick,
        owned: BTreeMap::new(),
        seeded: BTreeMap::new(),
        addons: Vec::new(),
    };
    let (mut owned, mut seeded) = (0, 0);
    for (rel, contents) in &files {
        let hash = sha256(contents.as_bytes());
        if is_owned(rel) {
            manifest.owned.insert(rel.to_string(), hash);
            owned += 1;
        } else {
            manifest.seeded.insert(rel.to_string(), hash);
            seeded += 1;
        }
        if !skipped.contains(rel) {
            write_file(dest, rel, contents.as_bytes())?;
        }
    }
    manifest.write(dest)?;
    let mut next = vec!["cargo test --workspace".to_string()];
    if skipped.contains(&"AGENTS.md") {
        next.push(format!(
            "your AGENTS.md was kept: the template's is copied to {SEEDED_UPDATES}/AGENTS.md; bring in what psx relies on"
        ));
        let agents = files.iter().find(|(rel, _)| *rel == "AGENTS.md").map_or("", |(_, c)| c.as_str());
        write_file(dest, &format!("{SEEDED_UPDATES}/AGENTS.md"), agents.as_bytes())?;
    } else {
        next.push("read AGENTS.md".into());
    }
    Ok(json!({
        "template": TEMPLATE,
        "package": name,
        "dest": dest,
        "region": options.region.name(),
        "fields_per_tick": options.fields_per_tick,
        "tick_hz": options.region.hz() / f64::from(options.fields_per_tick),
        "owned_files": owned,
        "seeded_files": seeded,
        "skipped": skipped,
        "manifest": MANIFEST,
        "next": next,
    }))
}

/// What the manifest of a tracked workspace records, as `create` reports it.
pub fn tracked(dest: &Path) -> crate::Result<Value> {
    let m = Manifest::read(dest)?;
    Ok(json!({
        "template": m.template,
        "package": m.package,
        "dest": dest,
        "region": m.region.name(),
        "fields_per_tick": m.fields_per_tick,
        "tick_hz": m.region.hz() / f64::from(m.fields_per_tick),
        "owned_files": m.owned.len(),
        "seeded_files": m.seeded.len(),
        "manifest": MANIFEST,
    }))
}

/// Brings tracked files (`vendor/`) to this psx build: untouched ones are
/// replaced, edited ones get the new version beside them as
/// `<file>.psx-new`. Seeded files whose template version changed are copied
/// under `.psx/template-update/` and reported. A Bevy template workspace's
/// `psx-bevy/` stops being tracked, and its seeded files, the Bevy
/// template's, are left alone.
pub fn update(dest: &Path, dry_run: bool) -> crate::Result<Value> {
    let mut manifest = Manifest::read(dest)?;
    let bevy = manifest.template == BEVY_TEMPLATE;
    let released: Vec<String> = manifest.owned.keys().filter(|r| r.starts_with(PSX_BEVY_PREFIX)).cloned().collect();
    for rel in &released {
        manifest.owned.remove(rel);
    }
    let files = render(&manifest.package, manifest.options())?;
    let new_owned: BTreeMap<&str, &String> =
        files.iter().filter(|(rel, _)| is_owned(rel)).map(|(rel, c)| (*rel, c)).collect();
    let current = |rel: &str| std::fs::read(dest.join(rel)).ok().map(|b| sha256(&b));

    let (mut updated, mut added, mut removed, mut edited, mut conflicts, mut unchanged) =
        (vec![], vec![], vec![], vec![], vec![], 0);
    for (rel, contents) in &new_owned {
        let new_hash = sha256(contents.as_bytes());
        let beside = format!("{rel}.psx-new");
        // The template version the project's file is based on: the one the
        // template wrote, or the last `.psx-new` it was given to merge.
        let base = manifest.owned.get(*rel);
        match current(rel) {
            None => {
                if !dry_run {
                    write_file(dest, rel, contents.as_bytes())?;
                    manifest.owned.insert(rel.to_string(), new_hash);
                }
                added.push(rel.to_string());
            }
            // The file already matches this build (untouched, or an edit the
            // template now makes too).
            Some(now) if now == new_hash => {
                if !dry_run {
                    manifest.owned.insert(rel.to_string(), new_hash);
                }
                unchanged += 1;
            }
            // Untouched since the template wrote it: take the new version.
            Some(now) if base == Some(&now) => {
                if !dry_run {
                    write_file(dest, rel, contents.as_bytes())?;
                    manifest.owned.insert(rel.to_string(), new_hash);
                }
                updated.push(rel.to_string());
            }
            // Edited, on top of this build's version: nothing to merge, unless
            // an earlier update's `.psx-new` is still waiting.
            Some(_) if base == Some(&new_hash) => {
                if dest.join(&beside).exists() {
                    conflicts.push(json!({"path": rel, "new_version": beside}));
                } else {
                    edited.push(rel.to_string());
                }
            }
            // Edited, and the template changed too (or a new template file
            // whose path is taken): the project merges. Deleting the
            // `.psx-new` marks it merged, so it is based on the new version.
            Some(_) => {
                if !dry_run {
                    write_file(dest, &beside, contents.as_bytes())?;
                    manifest.owned.insert(rel.to_string(), new_hash);
                }
                conflicts.push(json!({"path": rel, "new_version": beside}));
            }
        }
    }
    let gone: Vec<String> =
        manifest.owned.keys().filter(|rel| !new_owned.contains_key(rel.as_str())).cloned().collect();
    for rel in gone {
        let base = manifest.owned.get(&rel).cloned();
        match current(&rel) {
            Some(now) if Some(&now) == base.as_ref() => {
                if !dry_run {
                    remove_tracked(dest, &rel)?;
                }
                removed.push(rel.clone());
            }
            // An edited file the template no longer has: it is the project's now.
            Some(_) => conflicts.push(json!({"path": rel, "removed_upstream": true})),
            None => {}
        }
        if !dry_run {
            manifest.owned.remove(&rel);
        }
    }

    // Add-ons' files: newer versions beside the project's copies.
    let addon_paths: Vec<String> = addon::files(&manifest.addons).into_iter().map(|(_, rel, _)| rel).collect();
    let (addon_added, addon_conflicts) = addon::update(dest, &mut manifest, dry_run)?;
    added.extend(addon_added);
    conflicts.extend(addon_conflicts);

    // Seeded files belong to the project: only report template changes.
    let mut seeded_changes = Vec::new();
    for (rel, contents) in
        files.iter().filter(|(rel, _)| !is_owned(rel) && !bevy && !addon_paths.iter().any(|p| p == rel))
    {
        let new_hash = sha256(contents.as_bytes());
        if manifest.seeded.get(*rel) != Some(&new_hash) {
            let copy = format!("{SEEDED_UPDATES}/{rel}");
            if !dry_run {
                write_file(dest, &copy, contents.as_bytes())?;
                manifest.seeded.insert(rel.to_string(), new_hash);
            }
            seeded_changes.push(json!({"path": rel, "template_version": copy}));
        }
    }
    // Seeded files the template no longer has: the project decides.
    let dropped: Vec<String> = manifest
        .seeded
        .keys()
        .filter(|rel| !bevy && !files.iter().any(|(r, _)| r == rel) && !addon_paths.contains(rel))
        .cloned()
        .collect();
    for rel in dropped {
        if dest.join(&rel).exists() {
            seeded_changes.push(json!({"path": rel, "removed_upstream": true}));
        }
        if !dry_run {
            manifest.seeded.remove(&rel);
        }
    }

    let from = std::mem::replace(&mut manifest.psx, psx_version());
    if !dry_run {
        manifest.write(dest)?;
    }
    Ok(json!({
        "dry_run": dry_run,
        "from": from,
        "to": psx_version(),
        "updated": updated,
        "added": added,
        "removed": removed,
        "edited": edited,
        "unchanged": unchanged,
        "conflicts": conflicts,
        "seeded_changes": seeded_changes,
        "released": released.len(),
        "next": if conflicts.is_empty() && seeded_changes.is_empty() {
            json!(["cargo test --workspace --all-features"])
        } else {
            json!([
                "merge each conflict's new_version into its path, keeping the project's changes, then delete the .psx-new file",
                "a removed_upstream conflict is a file the template dropped: keep it as the project's or delete it",
                "compare each seeded change's template_version with your file and apply what applies; a seeded change with removed_upstream is a file the template no longer seeds: delete it unless the project uses it",
                "cargo test --workspace --all-features",
            ])
        },
    }))
}

/// Starts tracking an existing workspace: records this build's seeded files
/// as already merged and its `vendor/` files as the template's, so the next
/// `update` replaces them.
pub fn adopt(dest: &Path, name: Option<&str>, options: TemplateOptions) -> crate::Result<Value> {
    if dest.join(MANIFEST).exists() {
        return Err(Error::invalid(format!("{} already has {MANIFEST}; use psx port update", dest.display())));
    }
    if !dest.join("Cargo.toml").is_file() {
        return Err(Error::invalid(format!(
            "{} has no Cargo.toml: adopt tracks an existing template workspace (psx port init makes a new one)",
            dest.display()
        )));
    }
    let name = package_name(dest, name);
    check_options(&name, options)?;
    let files = render(&name, options)?;
    let mut manifest = Manifest {
        template: TEMPLATE.into(),
        psx: psx_version(),
        package: name,
        region: options.region,
        fields_per_tick: options.fields_per_tick,
        owned: BTreeMap::new(),
        seeded: BTreeMap::new(),
        addons: Vec::new(),
    };
    let mut missing = Vec::new();
    for (rel, contents) in &files {
        let template = sha256(contents.as_bytes());
        if is_owned(rel) {
            match std::fs::read(dest.join(rel)).map(|b| sha256(&b)) {
                Ok(now) => drop(manifest.owned.insert(rel.to_string(), now)),
                Err(_) => missing.push(*rel),
            }
        } else {
            manifest.seeded.insert(rel.to_string(), template);
        }
    }
    manifest.write(dest)?;
    Ok(json!({
        "manifest": MANIFEST,
        "package": manifest.package,
        "owned_tracked": manifest.owned.len(),
        "owned_missing": missing,
        "next": ["psx port update --dry-run", "psx port update", "cargo test --workspace --all-features"],
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("psx-template-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn writes_renamed_workspace_and_refuses_clashes() {
        let dir = scratch("create");
        let out = create(&dir.join("my-game"), None, TemplateOptions::default()).unwrap();
        assert_eq!(out["package"], "my-game");
        assert_eq!(out["region"], "ntsc");
        let game = dir.join("my-game");
        let toml = std::fs::read_to_string(game.join("game/Cargo.toml")).unwrap();
        assert!(toml.contains("[package]\nname = \"game\"") && toml.contains("[[bin]]\nname = \"my-game\""));
        assert!(!toml.contains(TEMPLATE_BIN));
        let workspace = std::fs::read_to_string(game.join("Cargo.toml")).unwrap();
        assert!(workspace.contains(r#"members = ["game", "tools/import", "glossary"]"#));
        assert!(!workspace.contains("bevy"));
        let lib = std::fs::read_to_string(game.join("game/src/lib.rs")).unwrap();
        assert!(lib.contains(REGION_LINE) && lib.contains(FIELDS_PER_TICK_LINE));
        let manifest = Manifest::read(&game).unwrap();
        assert_eq!(manifest.package, "my-game");
        assert!(manifest.owned.keys().all(|k| is_owned(k)));
        assert_eq!(manifest.template, TEMPLATE);
        assert!(manifest.seeded.contains_key("game/tests/scenarios.rs"));
        assert_eq!(manifest.owned.len() + manifest.seeded.len(), FILES.len());
        assert!(create(&game, None, TemplateOptions::default()).is_err());
        assert!(create(&dir.join("Bad Name!"), None, TemplateOptions::default()).is_err());
        assert!(create(&dir.join("import"), None, TemplateOptions::default()).is_err());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn creates_beside_a_disc_and_keeps_the_users_agents_md() {
        let dir = scratch("beside");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("AGENTS.md"), "my prompt").unwrap();
        std::fs::write(dir.join("Game.cue"), "FILE").unwrap();
        let out = create(&dir, Some("port"), TemplateOptions::default()).unwrap();
        assert_eq!(out["skipped"], json!(["AGENTS.md"]));
        assert_eq!(std::fs::read_to_string(dir.join("AGENTS.md")).unwrap(), "my prompt");
        assert!(dir.join(".psx/template-update/AGENTS.md").exists() && dir.join("Game.cue").exists());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn writes_region_and_fields_per_tick() {
        let dir = scratch("region");
        let options = TemplateOptions { region: TemplateRegion::Pal, fields_per_tick: 2 };
        let out = create(&dir.join("croc"), None, options).unwrap();
        assert_eq!(out["region"], "pal");
        assert_eq!(out["tick_hz"], 25.0);
        let lib = std::fs::read_to_string(dir.join("croc/game/src/lib.rs")).unwrap();
        assert!(lib.contains("pub const REGION: Region = Region::Pal;"));
        assert!(lib.contains("pub const FIELDS_PER_TICK: u32 = 2;"));
        let manifest = Manifest::read(&dir.join("croc")).unwrap();
        assert_eq!((manifest.region, manifest.fields_per_tick), (TemplateRegion::Pal, 2));
        let bad = TemplateOptions { fields_per_tick: 0, ..options };
        assert!(create(&dir.join("zero"), None, bad).is_err());
        assert!(!dir.join("zero").exists());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn update_replaces_untouched_files_and_reports_edits() {
        let dir = scratch("update");
        create(&dir, Some("port"), TemplateOptions::default()).unwrap();
        let fresh = update(&dir, false).unwrap();
        assert_eq!(fresh["updated"], json!([]));
        assert_eq!(fresh["conflicts"], json!([]));
        assert_eq!(fresh["seeded_changes"], json!([]));

        // Pretend the project came from an older template: the recorded
        // versions of two owned files and one seeded file differ from now.
        let mut manifest = Manifest::read(&dir).unwrap();
        let old = b"// older template version\n";
        let (cue, tim) = ("vendor/psx-media/src/cue.rs", "vendor/psx-assets/src/tim.rs");
        for rel in [cue, tim] {
            std::fs::write(dir.join(rel), old).unwrap();
            manifest.owned.insert(rel.into(), sha256(old));
        }
        manifest.seeded.insert("game/src/main.rs".into(), sha256(old));
        manifest.seeded.insert("game/tests/retired.rs".into(), sha256(old));
        std::fs::write(dir.join("game/tests/retired.rs"), old).unwrap();
        manifest.owned.insert("vendor/psx-media/src/retired.rs".into(), sha256(old));
        std::fs::write(dir.join("vendor/psx-media/src/retired.rs"), old).unwrap();
        manifest.write(&dir).unwrap();
        // ...and the project edited one of them, plus deleted another owned file.
        std::fs::write(dir.join(tim), "// local edit\n").unwrap();
        std::fs::remove_file(dir.join("vendor/README.md")).unwrap();
        std::fs::write(dir.join("game/src/main.rs"), "// the project's own main\n").unwrap();

        let dry = update(&dir, true).unwrap();
        assert_eq!(dry["updated"], json!([cue]));
        assert_eq!(std::fs::read(dir.join(cue)).unwrap(), old, "dry run writes nothing");

        let out = update(&dir, false).unwrap();
        assert_eq!(out["updated"], json!([cue]));
        assert_eq!(out["added"], json!(["vendor/README.md"]));
        assert_eq!(out["removed"], json!(["vendor/psx-media/src/retired.rs"]));
        assert_eq!(out["conflicts"][0]["path"], tim);
        assert_eq!(out["seeded_changes"][0]["path"], "game/src/main.rs");
        assert_eq!(out["seeded_changes"][1], json!({"path": "game/tests/retired.rs", "removed_upstream": true}));
        assert!(dir.join("game/tests/retired.rs").exists(), "a seeded file is the project's to delete");
        assert_eq!(std::fs::read_to_string(dir.join(tim)).unwrap(), "// local edit\n");
        assert!(dir.join(format!("{tim}.psx-new")).exists());
        assert_eq!(
            std::fs::read_to_string(dir.join("game/src/main.rs")).unwrap(),
            "// the project's own main\n",
            "seeded files are never touched"
        );
        assert!(dir.join(".psx/template-update/game/src/main.rs").exists());
        assert!(!dir.join("vendor/psx-media/src/retired.rs").exists());

        // The edit stays a conflict until resolved; everything else is settled.
        let again = update(&dir, false).unwrap();
        assert_eq!(again["updated"], json!([]));
        assert_eq!(again["seeded_changes"], json!([]));
        assert_eq!(again["conflicts"].as_array().unwrap().len(), 1);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn edited_vendor_files_are_merged_once() {
        let dir = scratch("edits");
        create(&dir, Some("port"), TemplateOptions::default()).unwrap();
        let cue = "vendor/psx-media/src/cue.rs";
        let (older, edit, merged) = ("// older template\n", "// the game's change\n", "// merged\n");
        // The project changed a file the template has since changed too.
        let mut manifest = Manifest::read(&dir).unwrap();
        manifest.owned.insert(cue.into(), sha256(older.as_bytes()));
        manifest.write(&dir).unwrap();
        std::fs::write(dir.join(cue), edit).unwrap();

        let out = update(&dir, false).unwrap();
        let beside = format!("{cue}.psx-new");
        assert_eq!(out["conflicts"], json!([{"path": cue, "new_version": beside}]));
        assert_eq!(std::fs::read_to_string(dir.join(cue)).unwrap(), edit, "edits are never overwritten");
        assert!(dir.join(&beside).exists());
        // Until the .psx-new is deleted the merge is still pending.
        assert_eq!(update(&dir, false).unwrap()["conflicts"].as_array().unwrap().len(), 1);

        std::fs::write(dir.join(cue), merged).unwrap();
        std::fs::remove_file(dir.join(&beside)).unwrap();
        let settled = update(&dir, false).unwrap();
        assert_eq!(settled["conflicts"], json!([]));
        assert_eq!(settled["edited"], json!([cue]));
        assert_eq!(std::fs::read_to_string(dir.join(cue)).unwrap(), merged);
        assert!(!dir.join(&beside).exists());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn adopt_tracks_an_existing_workspace() {
        let dir = scratch("adopt");
        create(&dir, Some("port"), TemplateOptions::default()).unwrap();
        std::fs::remove_file(dir.join(MANIFEST)).unwrap();
        assert!(update(&dir, true).is_err());
        let cue = "vendor/psx-media/src/cue.rs";
        std::fs::write(dir.join(cue), "// copied from an older template\n").unwrap();
        let out = adopt(&dir, Some("port"), TemplateOptions::default()).unwrap();
        assert_eq!(out["owned_missing"], json!([]));
        let up = update(&dir, false).unwrap();
        assert_eq!(up["updated"], json!([cue]), "adopted vendor/ files count as untouched");
        assert_eq!(up["conflicts"], json!([]));
        assert!(adopt(&dir, None, TemplateOptions::default()).is_err());
        assert!(adopt(&dir.join("missing"), None, TemplateOptions::default()).is_err());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// A workspace from the Bevy template keeps its psx-bevy/ and seeded
    /// files; only vendor/ is brought up to date.
    #[test]
    fn a_bevy_workspace_keeps_psx_bevy_as_its_own() {
        let dir = scratch("bevy");
        create(&dir, Some("port"), TemplateOptions::default()).unwrap();
        let shell = "psx-bevy/src/shell.rs";
        std::fs::create_dir_all(dir.join("psx-bevy/src")).unwrap();
        std::fs::write(dir.join(shell), "// the Bevy template's shell\n").unwrap();
        let mut manifest = Manifest::read(&dir).unwrap();
        manifest.template = BEVY_TEMPLATE.into();
        manifest.owned.insert(shell.into(), sha256(b"// the Bevy template's shell\n"));
        manifest.seeded = BTreeMap::from([("game/src/demo/mod.rs".into(), "sha256:00".into())]);
        manifest.write(&dir).unwrap();
        let cue = "vendor/psx-media/src/cue.rs";
        std::fs::write(dir.join(cue), "// older\n").unwrap();
        manifest.owned.insert(cue.into(), sha256(b"// older\n"));
        manifest.write(&dir).unwrap();

        let out = update(&dir, false).unwrap();
        assert_eq!((out["updated"].clone(), out["released"].clone()), (json!([cue]), json!(1)));
        assert_eq!((out["removed"].clone(), out["seeded_changes"].clone()), (json!([]), json!([])));
        assert!(dir.join(shell).exists(), "psx-bevy/ is the project's");
        let after = Manifest::read(&dir).unwrap();
        assert_eq!(after.template, BEVY_TEMPLATE);
        assert!(after.owned.keys().all(|k| k.starts_with(VENDOR_PREFIX)));
        assert_eq!(after.seeded.len(), 1);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn manifest_round_trips() {
        let m = Manifest {
            template: TEMPLATE.into(),
            psx: "0.1.0+abc".into(),
            package: "my-game".into(),
            region: TemplateRegion::Pal,
            fields_per_tick: 2,
            owned: BTreeMap::from([("vendor/README.md".into(), "sha256:00".into())]),
            seeded: BTreeMap::from([("game/src/lib.rs".into(), "sha256:11".into())]),
            addons: vec!["shell".into(), "renderer".into()],
        };
        assert_eq!(parse_toml_like(&toml_like(&m)).unwrap(), m);
    }

    #[test]
    fn manifest_paths_stay_inside_the_workspace() {
        let base = |m: &mut Manifest| m.owned.clear();
        let mut m = Manifest {
            template: TEMPLATE.into(),
            psx: "0.1.0+abc".into(),
            package: "my-game".into(),
            region: TemplateRegion::Pal,
            fields_per_tick: 2,
            owned: BTreeMap::new(),
            seeded: BTreeMap::new(),
            addons: Vec::new(),
        };
        for bad in ["vendor/../../.ssh/config", "/etc/passwd", "../x", "game/src/lib.rs", "vendor//x", "vendor\\..\\x"]
        {
            base(&mut m);
            m.owned.insert(bad.into(), "sha256:00".into());
            assert!(parse_toml_like(&toml_like(&m)).is_err(), "{bad}");
        }
        base(&mut m);
        m.seeded.insert("../outside".into(), "sha256:00".into());
        assert!(parse_toml_like(&toml_like(&m)).is_err());
    }

    /// Every file under templates/base (except build output) must be
    /// embedded, or `psx port init` silently drops it.
    #[test]
    fn embeds_every_template_file() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../templates/base");
        let mut found = Vec::new();
        let mut stack = vec![root.clone()];
        while let Some(dir) = stack.pop() {
            for entry in std::fs::read_dir(&dir).unwrap() {
                let path = entry.unwrap().path();
                let rel = path.strip_prefix(&root).unwrap().to_string_lossy().replace('\\', "/");
                if rel == "target" || rel.ends_with("/target") || rel.ends_with(".psx-new") {
                    continue;
                }
                if path.is_dir() {
                    stack.push(path);
                } else {
                    found.push(rel);
                }
            }
        }
        let embedded: Vec<&str> = FILES.iter().map(|(p, _)| *p).collect();
        let missing: Vec<&String> = found.iter().filter(|f| !embedded.contains(&f.as_str())).collect();
        assert!(missing.is_empty(), "template files not in FILES: {missing:?}");
    }

    /// The template's vendored crates must be exact copies
    /// of crates/, at this psx version.
    #[test]
    fn vendored_crates_match_crates() {
        let repo = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        for krate in ["psx-media", "psx-assets", "psx-fixed", "psx-gte", "psx-scenario", "psx-capture"] {
            let src = repo.join("crates").join(krate).join("src");
            let mut names: Vec<String> = std::fs::read_dir(&src)
                .unwrap()
                .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
                .collect();
            names.sort();
            let prefix = format!("{VENDOR_PREFIX}{krate}/src/");
            let mut vendored: Vec<String> =
                FILES.iter().filter_map(|(p, _)| p.strip_prefix(prefix.as_str())).map(str::to_string).collect();
            vendored.sort();
            assert_eq!(vendored, names, "templates/base/{prefix} must hold exactly crates/{krate}/src's files");
            for name in names {
                let ours = std::fs::read_to_string(src.join(&name)).unwrap();
                let theirs = FILES.iter().find(|(p, _)| *p == format!("{prefix}{name}")).unwrap().1;
                assert!(
                    ours == theirs,
                    "templates/base/{prefix}{name} differs from crates/{krate}/src/{name}; copy it over"
                );
            }
            let manifest = FILES.iter().find(|(p, _)| *p == format!("{VENDOR_PREFIX}{krate}/Cargo.toml")).unwrap().1;
            assert!(
                manifest.contains(&format!("version = \"{}\"", env!("CARGO_PKG_VERSION"))),
                "templates/base/{VENDOR_PREFIX}{krate}/Cargo.toml: set version to {}",
                env!("CARGO_PKG_VERSION")
            );
        }
    }
}
