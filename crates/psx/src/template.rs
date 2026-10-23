//! `psx template create|update|adopt`: the Bevy game workspace.
//!
//! The template is a workspace on the psx-bevy library, a git dependency
//! (<https://github.com/bombpersons/psx-bevy>). `vendor/` is owned by the
//! template and replaced by `update`; everything else (`engine/`, `game/`, `tools/`,
//! docs, root manifests) is seeded once and belongs to the project, except the
//! root manifest's psx-bevy line, which `update` moves to this build's tag.
//! `.psx-template.toml` records the version and a hash of every file as the
//! template wrote it, so `update` can tell untouched files from edited ones.
//!
//! Projects from before the split have psx-bevy as a `psx-bevy/` member;
//! `update` removes its untouched files and switches the manifest to git.

use std::collections::BTreeMap;
use std::path::Path;

use psx_media::MediaError;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

macro_rules! files {
    ($($path:literal),* $(,)?) => {
        &[$(($path, include_str!(concat!("../../../templates/psx-bevy/", $path)))),*]
    };
}

const FILES: &[(&str, &str)] = files![
    ".gitignore",
    "AGENTS.md",
    "Cargo.lock",
    "Cargo.toml",
    "README.md",
    "rustfmt.toml",
    "docs/engine-model.md",
    "docs/evidence.md",
    "docs/template.md",
    "editor/AGENTS.md",
    "editor/Cargo.toml",
    "editor/src/document.rs",
    "editor/src/files.rs",
    "editor/src/lib.rs",
    "editor/src/main.rs",
    "editor/src/panels.rs",
    "editor/tests/document.rs",
    "engine/AGENTS.md",
    "engine/Cargo.toml",
    "engine/assets/sandbox/room.ron",
    "engine/assets/sandbox/walker.ron",
    "engine/examples/sandbox.rs",
    "engine/src/config.rs",
    "engine/src/level.rs",
    "engine/src/lib.rs",
    "engine/src/objects.rs",
    "engine/src/schedule.rs",
    "engine/src/triggers.rs",
    "game/AGENTS.md",
    "game/Cargo.toml",
    "game/assets/demo/level.ron",
    "game/assets/demo/player.ron",
    "game/src/demo/mod.rs",
    "game/src/demo/player.rs",
    "game/src/disc.rs",
    "game/src/lib.rs",
    "game/src/main.rs",
    "game/tests/dev_mcp.rs",
    "game/tests/disc.rs",
    "game/tests/player.rs",
    "mods/example/Cargo.toml",
    "mods/example/assets/demo/player.ron",
    "mods/example/assets/example_mod/level.ron",
    "mods/example/src/lib.rs",
    "mods/example/src/main.rs",
    "mods/example/tests/dev_mcp.rs",
    "mods/example/tests/rules.rs",
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
    "vendor/psx-assets/src/lib.rs",
    "vendor/psx-assets/src/model_document.rs",
    "vendor/psx-assets/src/scan.rs",
    "vendor/psx-assets/src/tim.rs",
    "vendor/psx-assets/src/tmd.rs",
    "vendor/psx-assets/src/upload.rs",
    "vendor/psx-assets/src/vab.rs",
    "vendor/psx-media/Cargo.toml",
    "vendor/psx-media/src/chd.rs",
    "vendor/psx-media/src/cue.rs",
    "vendor/psx-media/src/disc.rs",
    "vendor/psx-media/src/error.rs",
    "vendor/psx-media/src/exe.rs",
    "vendor/psx-media/src/iso9660.rs",
    "vendor/psx-media/src/lib.rs",
    "vendor/psx-media/src/memmap.rs",
    "vendor/psx-media/src/source.rs",
    "vendor/psx-media/src/str_video.rs",
    "vendor/psx-media/src/subq.rs",
    "vendor/psx-media/src/system_cnf.rs",
];

/// Files under this prefix belong to the template; `update` replaces them.
/// `vendor/` holds copies of the toolkit's psx-media and psx-assets.
const VENDOR_PREFIX: &str = "vendor/";
/// Where projects from before psx-bevy became a git dependency keep its copy.
const LEGACY_CRATE: &str = "psx-bevy/";
/// The psx-bevy dependency line in the workspace manifest starts with this.
const PSX_BEVY_DEP: &str = "psx-bevy = ";
/// psx-bevy's repository; `update` moves only dependency lines that use it.
const PSX_BEVY_GIT: &str = "https://github.com/bombpersons/psx-bevy";
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

    fn write(&self, dest: &Path) -> psx_media::Result<()> {
        let header = "# Written by `psx template create|update|adopt`. `update` compares these hashes\n\
                      # with the files to tell untouched template files from edited ones.\n";
        let body = toml_like(self);
        let path = dest.join(MANIFEST);
        std::fs::write(&path, format!("{header}{body}")).map_err(|e| MediaError::io(&path, e))
    }

    fn read(dest: &Path) -> psx_media::Result<Self> {
        let path = dest.join(MANIFEST);
        let text = std::fs::read_to_string(&path).map_err(|e| {
            MediaError::Malformed(format!(
                "{}: {e}; projects created before the workspace template need `psx template adopt`",
                path.display()
            ))
        })?;
        parse_toml_like(&text).map_err(|e| MediaError::Malformed(format!("{}: {e}", path.display())))
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
    rel.starts_with(VENDOR_PREFIX)
}

/// The template's psx-bevy dependency line (root `Cargo.toml`), tag included.
fn psx_bevy_dep() -> &'static str {
    let manifest = FILES.iter().find(|(rel, _)| *rel == "Cargo.toml").expect("template has a Cargo.toml").1;
    manifest.lines().find(|line| line.starts_with(PSX_BEVY_DEP)).expect("template Cargo.toml has a psx-bevy line")
}

/// The project's root manifest with psx-bevy at `dep`: a git line on a tag
/// moved, or a pre-split `psx-bevy/` member and path dependency replaced.
/// `None` when the project names psx-bevy another way (a fork, a branch, a
/// local checkout), which `update` leaves alone.
fn move_psx_bevy(manifest: &str, dep: &str) -> Option<String> {
    let mut found = false;
    let mut out = String::with_capacity(manifest.len());
    for line in manifest.split_inclusive('\n') {
        let body = line.trim_end_matches(['\n', '\r']);
        let ending = &line[body.len()..];
        if body.starts_with(PSX_BEVY_DEP) {
            let tagged = body.contains(&format!("git = \"{PSX_BEVY_GIT}\"")) && body.contains("tag = ");
            if !tagged && body != r#"psx-bevy = { path = "psx-bevy" }"# {
                return None;
            }
            found = true;
            out.push_str(dep);
            out.push_str(ending);
        } else if body.trim_start().starts_with("members = [") {
            out.push_str(&body.replace("\"psx-bevy\", ", "").replace(", \"psx-bevy\"", ""));
            out.push_str(ending);
        } else {
            out.push_str(line);
        }
    }
    found.then_some(out)
}

/// Every file under `dest/dir` (build output excluded), relative to `dest`.
fn files_under(dest: &Path, dir: &str) -> Vec<String> {
    let mut found = Vec::new();
    let mut stack = vec![dest.join(dir)];
    while let Some(path) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&path) else { continue };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.file_name().is_some_and(|n| n == "target") {
                continue;
            }
            if path.is_dir() {
                stack.push(path);
            } else if let Ok(rel) = path.strip_prefix(dest) {
                found.push(rel.to_string_lossy().replace('\\', "/"));
            }
        }
    }
    found.sort();
    found
}

/// Removes the empty folders under `dir`, then `dir` itself if it is empty.
fn remove_empty_dirs(dir: &Path) {
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            if entry.path().is_dir() {
                remove_empty_dirs(&entry.path());
            }
        }
    }
    let _ = std::fs::remove_dir(dir);
}

/// The template's `game/src/lib.rs` with `REGION` and `FIELDS_PER_TICK` set.
fn configure_lib(contents: &str, options: TemplateOptions) -> psx_media::Result<String> {
    if !contents.contains(REGION_LINE) || !contents.contains(FIELDS_PER_TICK_LINE) {
        return Err(MediaError::Malformed(
            "template game/src/lib.rs no longer has the REGION / FIELDS_PER_TICK lines template create rewrites".into(),
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
fn render(name: &str, options: TemplateOptions) -> psx_media::Result<Vec<(&'static str, String)>> {
    FILES
        .iter()
        .map(|(rel, contents)| {
            let contents = match *rel {
                "game/Cargo.toml" => {
                    contents.replacen(&format!("name = \"{TEMPLATE_PACKAGE}\""), &format!("name = \"{name}\""), 1)
                }
                // The package's own entry and every dependency on it (mods).
                "Cargo.lock" => contents.replace(&format!("\"{TEMPLATE_PACKAGE}\""), &format!("\"{name}\"")),
                // The workspace's `game` dependency names the package.
                "Cargo.toml" => {
                    contents.replacen(&format!("package = \"{TEMPLATE_PACKAGE}\""), &format!("package = \"{name}\""), 1)
                }
                "game/src/lib.rs" => configure_lib(contents, options)?,
                _ => contents.to_string(),
            };
            Ok((*rel, contents))
        })
        .collect()
}

fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.chars().next().is_some_and(|c| c.is_ascii_lowercase())
        && name.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_')
        && !matches!(name, "engine" | "psx-bevy" | "import" | "example-mod")
}

fn check_options(name: &str, options: TemplateOptions) -> psx_media::Result<()> {
    if !valid_name(name) {
        return Err(MediaError::Malformed(format!(
            "package name {name:?} must start with a-z, contain only a-z, 0-9, '-' or '_', and not be engine, \
             psx-bevy, import or example-mod; pass --name"
        )));
    }
    if !(1..=60).contains(&options.fields_per_tick) {
        return Err(MediaError::Malformed(format!(
            "fields_per_tick {} must be between 1 and 60 (vblanks the original waits per game frame)",
            options.fields_per_tick
        )));
    }
    Ok(())
}

fn write_file(dest: &Path, rel: &str, contents: &[u8]) -> psx_media::Result<()> {
    let path = dest.join(rel);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| MediaError::io(parent, e))?;
    }
    std::fs::write(&path, contents).map_err(|e| MediaError::io(&path, e))
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

pub fn create(dest: &Path, name: Option<&str>, options: TemplateOptions) -> psx_media::Result<Value> {
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
        return Err(MediaError::Malformed(format!(
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
        "cargo run -p engine --example sandbox".to_string(),
        "cargo test --workspace --all-features".to_string(),
        "cargo run --features dev-mcp -- --mcp --headless --software".to_string(),
    ];
    if skipped.contains(&"AGENTS.md") {
        next.push("your AGENTS.md was kept: read engine/AGENTS.md and game/AGENTS.md, and point it at them".into());
    } else {
        next.push("read AGENTS.md, engine/AGENTS.md and game/AGENTS.md".into());
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

/// Replaces untouched template-owned files with this psx build's versions,
/// and reports seeded files whose template version changed.
pub fn update(dest: &Path, dry_run: bool) -> psx_media::Result<Value> {
    let mut manifest = Manifest::read(dest)?;
    let files = render(&manifest.package, manifest.options())?;
    let new_owned: BTreeMap<&str, &String> =
        files.iter().filter(|(rel, _)| is_owned(rel)).map(|(rel, c)| (*rel, c)).collect();
    let current = |rel: &str| std::fs::read(dest.join(rel)).ok().map(|b| sha256(&b));

    let (mut updated, mut added, mut removed, mut conflicts, mut unchanged) = (vec![], vec![], vec![], vec![], 0);
    for (rel, contents) in &new_owned {
        let new_hash = sha256(contents.as_bytes());
        match (manifest.owned.get(*rel), current(rel)) {
            // Untouched since the template wrote it (or deleted): take the new version.
            (Some(base), Some(now)) if *base == now => {
                if now == new_hash {
                    unchanged += 1;
                } else {
                    if !dry_run {
                        write_file(dest, rel, contents.as_bytes())?;
                        manifest.owned.insert(rel.to_string(), new_hash);
                    }
                    updated.push(rel.to_string());
                }
            }
            (_, None) => {
                if !dry_run {
                    write_file(dest, rel, contents.as_bytes())?;
                    manifest.owned.insert(rel.to_string(), new_hash);
                }
                added.push(rel.to_string());
            }
            // Edited locally, or a new template file whose path is taken.
            (_, Some(now)) => {
                if now == new_hash {
                    // The edit matches the new version exactly: adopt it.
                    if !dry_run {
                        manifest.owned.insert(rel.to_string(), new_hash);
                    }
                    unchanged += 1;
                } else {
                    let beside = format!("{rel}.psx-new");
                    if !dry_run {
                        write_file(dest, &beside, contents.as_bytes())?;
                    }
                    conflicts.push(json!({"path": rel, "new_version": beside}));
                }
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
                    let path = dest.join(&rel);
                    std::fs::remove_file(&path).map_err(|e| MediaError::io(&path, e))?;
                    manifest.owned.remove(&rel);
                }
                removed.push(rel);
            }
            // An edited file of the old in-project psx-bevy: the change now
            // belongs in a psx-bevy checkout (`[patch]`) or upstream.
            Some(_) if rel.starts_with(LEGACY_CRATE) => {
                conflicts.push(json!({"path": rel, "removed_upstream": true, "moved_to": PSX_BEVY_GIT}))
            }
            Some(_) => conflicts.push(json!({"path": rel, "removed_upstream": true})),
            None => {
                if !dry_run {
                    manifest.owned.remove(&rel);
                }
            }
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

    if !dry_run {
        remove_empty_dirs(&dest.join(LEGACY_CRATE));
    }

    // The root manifest is the project's, but its psx-bevy line follows this build.
    let dep = psx_bevy_dep();
    let root_manifest = dest.join("Cargo.toml");
    let text = std::fs::read_to_string(&root_manifest).map_err(|e| MediaError::io(&root_manifest, e))?;
    let mut psx_bevy = match move_psx_bevy(&text, dep) {
        Some(new) if new == text => json!({"status": "unchanged", "dependency": dep}),
        Some(new) => {
            if !dry_run {
                write_file(dest, "Cargo.toml", new.as_bytes())?;
            }
            json!({"status": "moved", "dependency": dep})
        }
        None => json!({
            "status": "custom",
            "dependency": dep,
            "note": "Cargo.toml names psx-bevy another way (a fork, branch or local path) and was left as is",
        }),
    };
    // Tracked files of the old copy are already removed or listed as conflicts.
    let leftover: Vec<String> =
        files_under(dest, LEGACY_CRATE).into_iter().filter(|rel| !manifest.owned.contains_key(rel)).collect();
    if !leftover.is_empty() && psx_bevy["status"] != "custom" {
        psx_bevy["leftover"] = json!({
            "files": leftover,
            "note": "psx-bevy/ is no longer built: move any edits to engine/ or a psx-bevy checkout used through [patch], then delete the folder",
        });
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
        "unchanged": unchanged,
        "conflicts": conflicts,
        "seeded_changes": seeded_changes,
        "psx_bevy": psx_bevy,
        "next": if conflicts.is_empty() && seeded_changes.is_empty() {
            json!(["cargo test --workspace --all-features"])
        } else {
            json!([
                "merge each conflict's new_version into its path (reapply the change listed in docs/template.md, or move it to engine/), then delete the .psx-new file",
                "a conflict with moved_to is an edit to the old psx-bevy/ copy: move it to engine/, or to a psx-bevy checkout used through [patch] (psx-bevy's AGENTS.md), then delete the file",
                "compare each seeded change's template_version with your file and apply what applies",
                "cargo test --workspace --all-features",
            ])
        },
    }))
}

/// Starts tracking an existing workspace: records this build's seeded files
/// as already merged and its `vendor/` files as the template's, so the next
/// `update` replaces them. A pre-split `psx-bevy/` copy is not tracked (its
/// edits are unknown): `update` switches the manifest to git and reports it.
pub fn adopt(dest: &Path, name: Option<&str>, options: TemplateOptions) -> psx_media::Result<Value> {
    if dest.join(MANIFEST).exists() {
        return Err(MediaError::Malformed(format!("{} already has {MANIFEST}; use template update", dest.display())));
    }
    if !dest.join("Cargo.toml").is_file() {
        return Err(MediaError::Malformed(format!(
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
        if is_owned(rel) {
            match std::fs::read(dest.join(rel)) {
                Ok(bytes) => drop(manifest.owned.insert(rel.to_string(), sha256(&bytes))),
                Err(_) => missing.push(*rel),
            }
        } else {
            manifest.seeded.insert(rel.to_string(), sha256(contents.as_bytes()));
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
        assert!(workspace.contains("package = \"my-game\""));
        assert!(workspace.contains(psx_bevy_dep()) && psx_bevy_dep().contains(PSX_BEVY_GIT));
        assert!(!game.join("psx-bevy").exists());
        assert!(game.join("engine/examples/sandbox.rs").exists());
        let lib = std::fs::read_to_string(game.join("game/src/lib.rs")).unwrap();
        assert!(lib.contains(REGION_LINE) && lib.contains(FIELDS_PER_TICK_LINE));
        let manifest = Manifest::read(&game).unwrap();
        assert_eq!(manifest.package, "my-game");
        assert!(manifest.owned.keys().all(|k| is_owned(k)));
        assert!(manifest.seeded.contains_key("engine/src/objects.rs"));
        assert_eq!(manifest.owned.len() + manifest.seeded.len(), FILES.len());
        assert!(create(&game, None, TemplateOptions::default()).is_err());
        assert!(create(&dir.join("Bad Name!"), None, TemplateOptions::default()).is_err());
        assert!(create(&dir.join("engine"), None, TemplateOptions::default()).is_err());
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
        assert!(dir.join("engine/AGENTS.md").exists() && dir.join("Game.cue").exists());
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
        assert_eq!(fresh["psx_bevy"]["status"], "unchanged");

        // Pretend the project came from an older template: the recorded
        // versions of two owned files and one seeded file differ from now.
        let mut manifest = Manifest::read(&dir).unwrap();
        let old = b"// older template version\n";
        let (cue, tim) = ("vendor/psx-media/src/cue.rs", "vendor/psx-assets/src/tim.rs");
        for rel in [cue, tim] {
            std::fs::write(dir.join(rel), old).unwrap();
            manifest.owned.insert(rel.into(), sha256(old));
        }
        manifest.seeded.insert("engine/src/objects.rs".into(), sha256(old));
        manifest.owned.insert("vendor/psx-media/src/retired.rs".into(), sha256(old));
        std::fs::write(dir.join("vendor/psx-media/src/retired.rs"), old).unwrap();
        manifest.write(&dir).unwrap();
        // ...and the project edited one of them, plus deleted another owned file.
        std::fs::write(dir.join(tim), "// local edit\n").unwrap();
        std::fs::remove_file(dir.join("vendor/README.md")).unwrap();
        std::fs::write(dir.join("engine/src/objects.rs"), "// the project's own engine\n").unwrap();

        let dry = update(&dir, true).unwrap();
        assert_eq!(dry["updated"], json!([cue]));
        assert_eq!(std::fs::read(dir.join(cue)).unwrap(), old, "dry run writes nothing");

        let out = update(&dir, false).unwrap();
        assert_eq!(out["updated"], json!([cue]));
        assert_eq!(out["added"], json!(["vendor/README.md"]));
        assert_eq!(out["removed"], json!(["vendor/psx-media/src/retired.rs"]));
        assert_eq!(out["conflicts"][0]["path"], tim);
        assert_eq!(out["seeded_changes"][0]["path"], "engine/src/objects.rs");
        assert_eq!(std::fs::read_to_string(dir.join(tim)).unwrap(), "// local edit\n");
        assert!(dir.join(format!("{tim}.psx-new")).exists());
        assert_eq!(
            std::fs::read_to_string(dir.join("engine/src/objects.rs")).unwrap(),
            "// the project's own engine\n",
            "seeded files are never touched"
        );
        assert!(dir.join(".psx/template-update/engine/src/objects.rs").exists());
        assert!(!dir.join("vendor/psx-media/src/retired.rs").exists());

        // The edit stays a conflict until resolved; everything else is settled.
        let again = update(&dir, false).unwrap();
        assert_eq!(again["updated"], json!([]));
        assert_eq!(again["seeded_changes"], json!([]));
        assert_eq!(again["conflicts"].as_array().unwrap().len(), 1);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn update_moves_the_psx_bevy_tag_and_leaves_custom_sources() {
        let dir = scratch("tag");
        create(&dir, Some("port"), TemplateOptions::default()).unwrap();
        let path = dir.join("Cargo.toml");
        let current = std::fs::read_to_string(&path).unwrap();
        let older = format!("{PSX_BEVY_DEP}{{ git = \"{PSX_BEVY_GIT}\", tag = \"v0.0.1\" }}");
        std::fs::write(&path, current.replace(psx_bevy_dep(), &older)).unwrap();
        let dry = update(&dir, true).unwrap();
        assert_eq!(dry["psx_bevy"]["status"], "moved");
        assert!(std::fs::read_to_string(&path).unwrap().contains(&older), "dry run writes nothing");
        assert_eq!(update(&dir, false).unwrap()["psx_bevy"]["status"], "moved");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), current);

        // A local checkout (or a fork, or a branch) is the project's choice.
        let local = current.replace(psx_bevy_dep(), r#"psx-bevy = { path = "../psx-bevy" }"#);
        std::fs::write(&path, &local).unwrap();
        assert_eq!(update(&dir, false).unwrap()["psx_bevy"]["status"], "custom");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), local);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// A project from before the split: psx-bevy/ is a member with a path
    /// dependency, its files tracked as template-owned.
    fn legacy_project(tag: &str) -> std::path::PathBuf {
        let dir = scratch(tag);
        create(&dir, Some("port"), TemplateOptions::default()).unwrap();
        let path = dir.join("Cargo.toml");
        let legacy = std::fs::read_to_string(&path)
            .unwrap()
            .replace(psx_bevy_dep(), r#"psx-bevy = { path = "psx-bevy" }"#)
            .replace("members = [\"engine\",", "members = [\"psx-bevy\", \"engine\",");
        std::fs::write(&path, legacy).unwrap();
        let mut manifest = Manifest::read(&dir).unwrap();
        for rel in ["psx-bevy/Cargo.toml", "psx-bevy/src/lib.rs", "psx-bevy/src/shell.rs"] {
            let old = format!("// {rel} from an older template\n");
            write_file(&dir, rel, old.as_bytes()).unwrap();
            manifest.owned.insert(rel.into(), sha256(old.as_bytes()));
        }
        manifest.write(&dir).unwrap();
        dir
    }

    #[test]
    fn update_moves_a_pre_split_project_to_the_git_dependency() {
        let dir = legacy_project("legacy");
        std::fs::write(dir.join("psx-bevy/src/shell.rs"), "// local edit\n").unwrap();
        let out = update(&dir, false).unwrap();
        assert_eq!(out["removed"], json!(["psx-bevy/Cargo.toml", "psx-bevy/src/lib.rs"]));
        assert_eq!(
            out["conflicts"],
            json!([{"path": "psx-bevy/src/shell.rs", "removed_upstream": true, "moved_to": PSX_BEVY_GIT}])
        );
        assert_eq!(out["psx_bevy"]["status"], "moved");
        assert!(out["psx_bevy"].get("leftover").is_none(), "the edited file is already a conflict");
        let workspace = std::fs::read_to_string(dir.join("Cargo.toml")).unwrap();
        let template = FILES.iter().find(|(rel, _)| *rel == "Cargo.toml").unwrap().1;
        assert_eq!(workspace, template.replacen("package = \"psx-game\"", "package = \"port\"", 1));
        assert!(dir.join("psx-bevy/src/shell.rs").exists());
        assert!(!dir.join("psx-bevy/Cargo.toml").exists());

        std::fs::remove_file(dir.join("psx-bevy/src/shell.rs")).unwrap();
        let settled = update(&dir, false).unwrap();
        assert_eq!(settled["conflicts"], json!([]));
        assert!(!dir.join("psx-bevy").exists(), "empty folders are removed");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn adopt_tracks_an_existing_workspace_and_reports_an_old_psx_bevy_copy() {
        let dir = legacy_project("adopt");
        std::fs::remove_file(dir.join(MANIFEST)).unwrap();
        assert!(update(&dir, true).is_err());
        std::fs::write(dir.join("vendor/psx-media/src/cue.rs"), "// copied from an older template\n").unwrap();
        let out = adopt(&dir, Some("port"), TemplateOptions::default()).unwrap();
        assert_eq!(out["owned_missing"], json!([]));
        let up = update(&dir, false).unwrap();
        assert_eq!(up["updated"], json!(["vendor/psx-media/src/cue.rs"]), "adopted files count as untouched");
        assert_eq!(up["removed"], json!([]), "an untracked psx-bevy/ copy is never deleted");
        assert_eq!(up["psx_bevy"]["status"], "moved");
        assert_eq!(up["psx_bevy"]["leftover"]["files"].as_array().unwrap().len(), 3);
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

    /// The template's vendored psx-media and psx-assets must be exact copies
    /// of crates/, at this psx version.
    #[test]
    fn vendored_crates_match_crates() {
        let repo = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        for krate in ["psx-media", "psx-assets"] {
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

    /// The engine and the editor must never depend on the game.
    #[test]
    fn engine_and_editor_do_not_depend_on_the_game() {
        for (rel, contents) in FILES {
            if matches!(*rel, "engine/Cargo.toml" | "editor/Cargo.toml") {
                assert!(!contents.contains("psx-game") && !contents.contains("game."), "{rel} depends on the game");
            }
            if (rel.starts_with("engine/") || rel.starts_with("editor/")) && rel.ends_with(".rs") {
                assert!(!contents.contains("game::"), "{rel} uses the game crate");
            }
        }
    }
}
