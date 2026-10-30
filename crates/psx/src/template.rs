//! `psx template create|update|adopt`: the Bevy game workspace.
//!
//! The template is a workspace whose game is built on `psx-bevy/`, a copy
//! of the PS1 look, ticks, input, menus and launcher that the project is
//! expected to change. `psx-bevy/` and `vendor/` are tracked: `update`
//! replaces the files the project hasn't changed and writes `<file>.psx-new`
//! beside the ones it has. Everything else (`game/`, `tools/`,
//! docs, root manifests) is seeded once and belongs to the project.
//! `.psx-template.toml` records the version and, per file, the template
//! version the project's copy is based on, so `update` can tell untouched
//! files from edited ones.

use crate::Error;
use std::collections::BTreeMap;
use std::path::Path;

use crate::commands::{Ctx, Op, Outcome};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::path::PathBuf;

#[derive(clap::Args, Deserialize, JsonSchema)]
pub struct TemplateCreateArgs {
    /// Destination: new, empty or the game folder; never overwrites files.
    pub dest: PathBuf,
    /// Game package name (default: the folder's name).
    #[arg(long)]
    pub name: Option<String>,
    /// The original's video region: display and tick rate. Default ntsc.
    #[arg(long, value_enum)]
    pub region: Option<TemplateRegion>,
    /// Video fields per logic tick: 2 when the original waits two vblanks per frame (Croc PAL). Default 1.
    #[arg(long)]
    pub fields_per_tick: Option<u32>,
}

impl Op for TemplateCreateArgs {
    fn exec(self, _: &Ctx) -> Outcome {
        let options = TemplateOptions {
            region: self.region.unwrap_or_default(),
            fields_per_tick: self.fields_per_tick.unwrap_or(1),
        };
        Outcome::result(create(&self.dest, self.name.as_deref(), options))
    }
}

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

#[derive(clap::Args, Deserialize, JsonSchema)]
pub struct TemplateAdoptArgs {
    /// Existing template workspace (root Cargo.toml) with no .psx-template.toml.
    pub dest: PathBuf,
    /// The game's Cargo package name (defaults to the directory name).
    #[arg(long)]
    pub name: Option<String>,
    /// The project's region, as in game/src/lib.rs. Default ntsc.
    #[arg(long, value_enum)]
    pub region: Option<TemplateRegion>,
    /// The project's fields per tick, as in game/src/lib.rs. Default 1.
    #[arg(long)]
    pub fields_per_tick: Option<u32>,
}

impl Op for TemplateAdoptArgs {
    fn exec(self, _: &Ctx) -> Outcome {
        let options = TemplateOptions {
            region: self.region.unwrap_or_default(),
            fields_per_tick: self.fields_per_tick.unwrap_or(1),
        };
        Outcome::result(adopt(&self.dest, self.name.as_deref(), options))
    }
}

macro_rules! files {
    ($($path:literal),* $(,)?) => {
        &[$(($path, include_str!(concat!("../../../templates/psx-bevy/", $path)))),*]
    };
}

pub(crate) const FILES: &[(&str, &str)] = files![
    ".cargo/config.toml",
    ".gitignore",
    "AGENTS.md",
    "Cargo.lock",
    "Cargo.toml",
    "README.md",
    "rustfmt.toml",
    "docs/template.md",
    "game/AGENTS.md",
    "game/Cargo.toml",
    "game/assets/demo/level.ron",
    "game/assets/demo/player.ron",
    "game/src/config.rs",
    "game/src/demo/mod.rs",
    "game/src/demo/player.rs",
    "game/src/demo/spinner.rs",
    "game/src/disc.rs",
    "game/src/lib.rs",
    "game/src/main.rs",
    "game/tests/cases.rs",
    "game/tests/cases/demo_hop_walk.csv",
    "game/tests/cases/demo_hop_walk.json",
    "game/tests/dev_mcp.rs",
    "game/tests/mechanics.rs",
    "game/tests/scenarios.rs",
    "game/tests/disc.rs",
    "game/tests/player.rs",
    "psx-bevy/AGENTS.md",
    "psx-bevy/Cargo.toml",
    "psx-bevy/LICENSE-APACHE",
    "psx-bevy/LICENSE-MIT",
    "psx-bevy/README.md",
    "psx-bevy/examples/corridor.rs",
    "psx-bevy/examples/depth_bias.rs",
    "psx-bevy/examples/depth_cue.rs",
    "psx-bevy/examples/screen_size.rs",
    "psx-bevy/src/assets.rs",
    "psx-bevy/src/controller.rs",
    "psx-bevy/src/controller/bindings.rs",
    "psx-bevy/src/controls/mod.rs",
    "psx-bevy/src/controls/tests.rs",
    "psx-bevy/src/controls/view.rs",
    "psx-bevy/src/dev_mcp.rs",
    "psx-bevy/src/display/mod.rs",
    "psx-bevy/src/display/tests.rs",
    "psx-bevy/src/display/view.rs",
    "psx-bevy/src/gte.rs",
    "psx-bevy/src/launcher.rs",
    "psx-bevy/src/lib.rs",
    "psx-bevy/src/pointer.rs",
    "psx-bevy/src/psx/font.rs",
    "psx-bevy/src/psx/material.rs",
    "psx-bevy/src/psx/material2d.rs",
    "psx-bevy/src/psx/mod.rs",
    "psx-bevy/src/psx/scene.rs",
    "psx-bevy/src/psx/shaders.rs",
    "psx-bevy/src/psx/shaders/psx_material.wesl",
    "psx-bevy/src/psx/shaders/psx_material2d.wesl",
    "psx-bevy/src/psx/shaders/psx_screen.wesl",
    "psx-bevy/src/psx/shaders/psx_sprite.wesl",
    "psx-bevy/src/replay.rs",
    "psx-bevy/src/scenario.rs",
    "psx-bevy/src/settings.rs",
    "psx-bevy/src/shell.rs",
    "psx-bevy/src/testing.rs",
    "psx-bevy/src/testing/case.rs",
    "psx-bevy/src/testing/mcp.rs",
    "psx-bevy/src/testing/trajectory.rs",
    "psx-bevy/src/viewer.rs",
    "psx-bevy/tests/dev_mcp.rs",
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
    "vendor/psx-capture/Cargo.toml",
    "vendor/psx-capture/src/abi.rs",
    "vendor/psx-capture/src/lib.rs",
    "vendor/psx-mechanics/Cargo.toml",
    "vendor/psx-mechanics/src/binding.rs",
    "vendor/psx-mechanics/src/interface.rs",
    "vendor/psx-mechanics/src/lib.rs",
    "vendor/psx-mechanics/src/mechanic.rs",
    "vendor/psx-mechanics/src/rust.rs",
    "vendor/psx-mechanics/src/scenario.rs",
    "vendor/psx-mechanics/src/trace.rs",
    "vendor/psx-mechanics/src/value.rs",
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
];

/// Files under these prefixes are tracked; `update` replaces the untouched ones.
/// `vendor/` holds copies of the toolkit's psx-media, psx-assets, psx-fixed,
/// psx-gte, psx-mechanics and psx-capture (not to be edited); `psx-bevy/` is the base the game builds on
/// (edited freely).
const VENDOR_PREFIX: &str = "vendor/";
const PSX_BEVY_PREFIX: &str = "psx-bevy/";
/// Seeded files `create` skips (and reports) when the destination already has them.
const KEEP_EXISTING: &[&str] = &["AGENTS.md", "README.md", ".gitignore"];
const MANIFEST: &str = ".psx-template.toml";
/// Where `update` writes new versions of seeded files for the project to merge.
const SEEDED_UPDATES: &str = ".psx/template-update";

const TEMPLATE_PACKAGE: &str = "psx-game";
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
}

impl Manifest {
    fn options(&self) -> TemplateOptions {
        TemplateOptions { region: self.region, fields_per_tick: self.fields_per_tick }
    }

    fn write(&self, dest: &Path) -> crate::Result<()> {
        let header = "# Written by `psx template create|update|adopt`. `update` compares these hashes\n\
                      # with the files to tell untouched template files from edited ones.\n";
        let body = toml_like(self);
        let path = dest.join(MANIFEST);
        std::fs::write(&path, format!("{header}{body}")).map_err(|e| Error::io(&path, e))
    }

    fn read(dest: &Path) -> crate::Result<Self> {
        let path = dest.join(MANIFEST);
        let text = std::fs::read_to_string(&path).map_err(|e| {
            Error::malformed(format!(
                "{}: {e}; projects created before the workspace template need `psx template adopt`",
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
    })
}

fn psx_version() -> String {
    format!("{}+{}", env!("CARGO_PKG_VERSION"), env!("PSX_BUILD_ID"))
}

fn sha256(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    format!("sha256:{}", digest.iter().map(|b| format!("{b:02x}")).collect::<String>())
}

fn is_owned(rel: &str) -> bool {
    rel.starts_with(VENDOR_PREFIX) || rel.starts_with(PSX_BEVY_PREFIX)
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
            "template game/src/lib.rs no longer has the REGION / FIELDS_PER_TICK lines template create rewrites",
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
                    contents.replacen(&format!("name = \"{TEMPLATE_PACKAGE}\""), &format!("name = \"{name}\""), 1)
                }
                // The package's own entry and every dependency on it.
                "Cargo.lock" => contents.replace(&format!("\"{TEMPLATE_PACKAGE}\""), &format!("\"{name}\"")),
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
        && !matches!(name, "psx-bevy" | "import")
}

fn check_options(name: &str, options: TemplateOptions) -> crate::Result<()> {
    if !valid_name(name) {
        return Err(Error::invalid(format!(
            "package name {name:?} must start with a-z, contain only a-z, 0-9, '-' or '_', and not be \
             psx-bevy or import; pass --name"
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
fn package_name(dest: &Path, name: Option<&str>) -> String {
    match name {
        Some(n) => n.to_string(),
        None => dest
            .canonicalize()
            .ok()
            .as_deref()
            .unwrap_or(dest)
            .file_name()
            .map(|n| n.to_string_lossy().to_ascii_lowercase().replace([' ', '.'], "-"))
            .unwrap_or_else(|| TEMPLATE_PACKAGE.to_string()),
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
            "{} already has {}; template create only adds files (use template update for an existing project)",
            dest.display(),
            clashes.join(", ")
        )));
    }
    let mut manifest = Manifest {
        template: "psx-bevy".into(),
        psx: psx_version(),
        package: name.clone(),
        region: options.region,
        fields_per_tick: options.fields_per_tick,
        owned: BTreeMap::new(),
        seeded: BTreeMap::new(),
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
    let mut next = vec![
        "cargo run".to_string(),
        "cargo test --workspace --all-features".to_string(),
        "cargo run --features dev-mcp -- --mcp --headless --software".to_string(),
    ];
    if skipped.contains(&"AGENTS.md") {
        next.push("your AGENTS.md was kept: read game/AGENTS.md and point it at it".into());
    } else {
        next.push("read AGENTS.md and game/AGENTS.md".into());
    }
    Ok(json!({
        "template": "psx-bevy",
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

/// Brings tracked files (`psx-bevy/`, `vendor/`) to this psx build: untouched
/// ones are replaced, edited ones get the new version beside them as
/// `<file>.psx-new`. Seeded files whose template version changed are copied
/// under `.psx/template-update/` and reported.
pub fn update(dest: &Path, dry_run: bool) -> crate::Result<Value> {
    let mut manifest = Manifest::read(dest)?;
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

    // Seeded files belong to the project: only report template changes.
    let mut seeded_changes = Vec::new();
    for (rel, contents) in files.iter().filter(|(rel, _)| !is_owned(rel)) {
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
        "next": if conflicts.is_empty() && seeded_changes.is_empty() {
            json!(["cargo test --workspace --all-features"])
        } else {
            json!([
                "merge each conflict's new_version into its path, keeping the project's changes (docs/template.md lists them), then delete the .psx-new file",
                "a removed_upstream conflict is a file the template dropped: keep it as the project's or delete it",
                "compare each seeded change's template_version with your file and apply what applies",
                "cargo test --workspace --all-features",
            ])
        },
    }))
}

/// Starts tracking an existing workspace: records this build's seeded files
/// as already merged and its `vendor/` files as the template's, so the next
/// `update` replaces them. `psx-bevy/` files count as untouched only when
/// they match this build; any other is treated as edited, so `update` puts
/// the new version beside it instead of replacing it.
pub fn adopt(dest: &Path, name: Option<&str>, options: TemplateOptions) -> crate::Result<Value> {
    if dest.join(MANIFEST).exists() {
        return Err(Error::invalid(format!("{} already has {MANIFEST}; use template update", dest.display())));
    }
    if !dest.join("Cargo.toml").is_file() {
        return Err(Error::invalid(format!(
            "{} has no Cargo.toml: adopt tracks an existing template workspace (template create makes a new one)",
            dest.display()
        )));
    }
    let name = package_name(dest, name);
    check_options(&name, options)?;
    let files = render(&name, options)?;
    let mut manifest = Manifest {
        template: "psx-bevy".into(),
        psx: psx_version(),
        package: name,
        region: options.region,
        fields_per_tick: options.fields_per_tick,
        owned: BTreeMap::new(),
        seeded: BTreeMap::new(),
    };
    let mut missing = Vec::new();
    for (rel, contents) in &files {
        let template = sha256(contents.as_bytes());
        if is_owned(rel) {
            match std::fs::read(dest.join(rel)).map(|b| sha256(&b)) {
                Ok(now) if rel.starts_with(PSX_BEVY_PREFIX) && now != template => {}
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
        "next": ["psx template update --dry-run", "psx template update", "cargo test --workspace --all-features"],
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
        assert!(toml.contains("name = \"my-game\""));
        let lock = std::fs::read_to_string(game.join("Cargo.lock")).unwrap();
        assert!(lock.contains("name = \"my-game\"") && !lock.contains(TEMPLATE_PACKAGE));
        let workspace = std::fs::read_to_string(game.join("Cargo.toml")).unwrap();
        assert!(workspace.contains(r#"psx-bevy = { path = "psx-bevy", default-features = false }"#));
        assert!(game.join("psx-bevy/src/lib.rs").exists());
        let lib = std::fs::read_to_string(game.join("game/src/lib.rs")).unwrap();
        assert!(lib.contains(REGION_LINE) && lib.contains(FIELDS_PER_TICK_LINE));
        let manifest = Manifest::read(&game).unwrap();
        assert_eq!(manifest.package, "my-game");
        assert!(manifest.owned.keys().all(|k| is_owned(k)));
        assert!(manifest.seeded.contains_key("game/src/demo/player.rs"));
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
        assert!(dir.join("game/AGENTS.md").exists() && dir.join("Game.cue").exists());
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
        manifest.seeded.insert("game/src/demo/spinner.rs".into(), sha256(old));
        manifest.owned.insert("vendor/psx-media/src/retired.rs".into(), sha256(old));
        std::fs::write(dir.join("vendor/psx-media/src/retired.rs"), old).unwrap();
        manifest.write(&dir).unwrap();
        // ...and the project edited one of them, plus deleted another owned file.
        std::fs::write(dir.join(tim), "// local edit\n").unwrap();
        std::fs::remove_file(dir.join("vendor/README.md")).unwrap();
        std::fs::write(dir.join("game/src/demo/spinner.rs"), "// the project's own spinner\n").unwrap();

        let dry = update(&dir, true).unwrap();
        assert_eq!(dry["updated"], json!([cue]));
        assert_eq!(std::fs::read(dir.join(cue)).unwrap(), old, "dry run writes nothing");

        let out = update(&dir, false).unwrap();
        assert_eq!(out["updated"], json!([cue]));
        assert_eq!(out["added"], json!(["vendor/README.md"]));
        assert_eq!(out["removed"], json!(["vendor/psx-media/src/retired.rs"]));
        assert_eq!(out["conflicts"][0]["path"], tim);
        assert_eq!(out["seeded_changes"][0]["path"], "game/src/demo/spinner.rs");
        assert_eq!(std::fs::read_to_string(dir.join(tim)).unwrap(), "// local edit\n");
        assert!(dir.join(format!("{tim}.psx-new")).exists());
        assert_eq!(
            std::fs::read_to_string(dir.join("game/src/demo/spinner.rs")).unwrap(),
            "// the project's own spinner\n",
            "seeded files are never touched"
        );
        assert!(dir.join(".psx/template-update/game/src/demo/spinner.rs").exists());
        assert!(!dir.join("vendor/psx-media/src/retired.rs").exists());

        // The edit stays a conflict until resolved; everything else is settled.
        let again = update(&dir, false).unwrap();
        assert_eq!(again["updated"], json!([]));
        assert_eq!(again["seeded_changes"], json!([]));
        assert_eq!(again["conflicts"].as_array().unwrap().len(), 1);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn edited_psx_bevy_files_are_merged_once() {
        let dir = scratch("edits");
        create(&dir, Some("port"), TemplateOptions::default()).unwrap();
        let shell = "psx-bevy/src/shell.rs";
        let (older, edit, merged) = ("// older template\n", "// the game's change\n", "// merged\n");
        // The project changed a file the template has since changed too.
        let mut manifest = Manifest::read(&dir).unwrap();
        manifest.owned.insert(shell.into(), sha256(older.as_bytes()));
        manifest.write(&dir).unwrap();
        std::fs::write(dir.join(shell), edit).unwrap();

        let out = update(&dir, false).unwrap();
        let beside = format!("{shell}.psx-new");
        assert_eq!(out["conflicts"], json!([{"path": shell, "new_version": beside}]));
        assert_eq!(std::fs::read_to_string(dir.join(shell)).unwrap(), edit, "edits are never overwritten");
        assert!(dir.join(&beside).exists());
        // Until the .psx-new is deleted the merge is still pending.
        assert_eq!(update(&dir, false).unwrap()["conflicts"].as_array().unwrap().len(), 1);

        std::fs::write(dir.join(shell), merged).unwrap();
        std::fs::remove_file(dir.join(&beside)).unwrap();
        let settled = update(&dir, false).unwrap();
        assert_eq!(settled["conflicts"], json!([]));
        assert_eq!(settled["edited"], json!([shell]));
        assert_eq!(std::fs::read_to_string(dir.join(shell)).unwrap(), merged);
        assert!(!dir.join(&beside).exists());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn adopt_tracks_an_existing_workspace_and_keeps_psx_bevy_edits() {
        let dir = scratch("adopt");
        create(&dir, Some("port"), TemplateOptions::default()).unwrap();
        std::fs::remove_file(dir.join(MANIFEST)).unwrap();
        assert!(update(&dir, true).is_err());
        let (cue, shell) = ("vendor/psx-media/src/cue.rs", "psx-bevy/src/shell.rs");
        std::fs::write(dir.join(cue), "// copied from an older template\n").unwrap();
        std::fs::write(dir.join(shell), "// the game's change\n").unwrap();
        let out = adopt(&dir, Some("port"), TemplateOptions::default()).unwrap();
        assert_eq!(out["owned_missing"], json!([]));
        let up = update(&dir, false).unwrap();
        assert_eq!(up["updated"], json!([cue]), "adopted vendor/ files count as untouched");
        assert_eq!(up["conflicts"], json!([{"path": shell, "new_version": format!("{shell}.psx-new")}]));
        assert_eq!(std::fs::read_to_string(dir.join(shell)).unwrap(), "// the game's change\n");
        assert!(adopt(&dir, None, TemplateOptions::default()).is_err());
        assert!(adopt(&dir.join("missing"), None, TemplateOptions::default()).is_err());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn manifest_round_trips() {
        let m = Manifest {
            template: "psx-bevy".into(),
            psx: "0.1.0+abc".into(),
            package: "my-game".into(),
            region: TemplateRegion::Pal,
            fields_per_tick: 2,
            owned: BTreeMap::from([("vendor/README.md".into(), "sha256:00".into())]),
            seeded: BTreeMap::from([("game/src/lib.rs".into(), "sha256:11".into())]),
        };
        assert_eq!(parse_toml_like(&toml_like(&m)).unwrap(), m);
    }

    /// Every file under templates/psx-bevy (except build output) must be
    /// embedded, or `template create` silently drops it.
    #[test]
    fn embeds_every_template_file() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../templates/psx-bevy");
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
        for krate in ["psx-media", "psx-assets", "psx-fixed", "psx-gte", "psx-mechanics", "psx-capture"] {
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
            assert_eq!(vendored, names, "templates/psx-bevy/{prefix} must hold exactly crates/{krate}/src's files");
            for name in names {
                let ours = std::fs::read_to_string(src.join(&name)).unwrap();
                let theirs = FILES.iter().find(|(p, _)| *p == format!("{prefix}{name}")).unwrap().1;
                assert!(
                    ours == theirs,
                    "templates/psx-bevy/{prefix}{name} differs from crates/{krate}/src/{name}; copy it over"
                );
            }
            let manifest = FILES.iter().find(|(p, _)| *p == format!("{VENDOR_PREFIX}{krate}/Cargo.toml")).unwrap().1;
            assert!(
                manifest.contains(&format!("version = \"{}\"", env!("CARGO_PKG_VERSION"))),
                "templates/psx-bevy/{VENDOR_PREFIX}{krate}/Cargo.toml: set version to {}",
                env!("CARGO_PKG_VERSION")
            );
        }
    }
}
