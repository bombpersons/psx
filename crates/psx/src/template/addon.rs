//! `psx port add NAME`: ready-made crates (`templates/addons/NAME`) written
//! into a template workspace as `addons/NAME/`, a workspace member the game
//! depends on. They are seeded files: the project's code once added.
//! `update` offers newer versions beside them as `<file>.psx-new`.
//!
//! `full` is the set a playable port wants (shell, renderer, disc-import,
//! menus) plus the game's `src/main.rs` wiring them and `src/demo.rs`.

use super::{MANIFEST, Manifest, sha256, write_file};
use crate::Error;
use crate::commands::{Ctx, Op, Outcome};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{Value, json};
use std::path::Path;

#[derive(clap::Args, Deserialize, JsonSchema)]
pub struct PortAddArgs {
    /// The add-on: full (shell, renderer, disc-import and menus, wired in
    /// game/src/main.rs), shell, renderer, disc-import or menus. The add-ons
    /// it needs come with it.
    pub name: String,
}

impl Op for PortAddArgs {
    /// Adds to the project at the project root (`--root`, or the MCP project).
    fn exec(self, ctx: &Ctx) -> Outcome {
        Outcome::result(add(&ctx.root, &self.name))
    }
}

macro_rules! files {
    ($dir:literal: $($path:literal),* $(,)?) => {
        &[$(($path, include_str!(concat!("../../../../templates/addons/", $dir, "/", $path)))),*]
    };
}

pub(crate) struct Addon {
    pub name: &'static str,
    /// One line for AGENTS.md.
    about: &'static str,
    /// Added first.
    needs: &'static [&'static str],
    /// A crate written to `addons/NAME/`, a member the game depends on;
    /// otherwise its files are written at their paths in the workspace.
    member: bool,
    pub files: &'static [(&'static str, &'static str)],
}

pub(crate) const ADDONS: &[Addon] = &[
    Addon {
        name: "shell",
        about: "the window (windowed or borderless fullscreen, any size), the fixed-step loop at the \
                port's tick rate, and the pad from keyboard and gamepads with psx play's bindings",
        needs: &[],
        member: true,
        files: files!["shell": "Cargo.toml", "src/bindings.rs", "src/lib.rs", "src/settings.rs"],
    },
    Addon {
        name: "renderer",
        about: "wgpu drawing in a PS1 look and a modern look: glTF models, textures, a camera, lights, \
                fog and sprites (`cargo run -p renderer --example view -- MODEL.gltf`)",
        needs: &["shell"],
        member: true,
        files: files![
            "renderer": "Cargo.toml",
            "examples/view.rs",
            "src/blit.wgsl",
            "src/font.rs",
            "src/gltf_load.rs",
            "src/glyphs.rs",
            "src/lib.rs",
            "src/shader.wgsl",
            "tests/capture.rs",
        ],
    },
    Addon {
        name: "disc-import",
        about: "the player's disc imported by tools/import on first start, with a screen that asks for it",
        needs: &["renderer"],
        member: true,
        files: files!["disc-import": "Cargo.toml", "src/lib.rs"],
    },
    Addon {
        name: "menus",
        about: "F1 controls and F2 display menus over the game, saved to the player's settings",
        needs: &["shell", "renderer"],
        member: true,
        files: files!["menus": "Cargo.toml", "src/lib.rs"],
    },
    Addon {
        name: "full",
        about: "the four above wired in game/src/main.rs; game/src/demo.rs stands in for the game's drawing",
        needs: &["shell", "renderer", "disc-import", "menus"],
        member: false,
        files: files!["full": "game/src/main.rs", "game/src/demo.rs"],
    },
];

fn find(name: &str) -> crate::Result<&'static Addon> {
    ADDONS.iter().find(|a| a.name == name).ok_or_else(|| {
        Error::invalid(format!(
            "no add-on {name:?}; there are {}",
            ADDONS.iter().map(|a| a.name).collect::<Vec<_>>().join(", ")
        ))
    })
}

impl Addon {
    fn dest(&self, rel: &str) -> String {
        if self.member { format!("addons/{}/{rel}", self.name) } else { rel.to_string() }
    }
}

/// `name` and what it needs, dependencies first.
fn closure(name: &str, out: &mut Vec<&'static Addon>) -> crate::Result<()> {
    let addon = find(name)?;
    for need in addon.needs {
        closure(need, out)?;
    }
    if !out.iter().any(|a| a.name == addon.name) {
        out.push(addon);
    }
    Ok(())
}

/// The files of the recorded add-ons, at their workspace paths.
pub(crate) fn files(addons: &[String]) -> Vec<(&'static str, String, &'static str)> {
    ADDONS
        .iter()
        .filter(|a| addons.iter().any(|n| n == a.name))
        .flat_map(|a| a.files.iter().map(move |(rel, c)| (a.name, a.dest(rel), *c)))
        .collect()
}

/// Adds `"addons/NAME"` to the root manifest's `members`.
fn add_member(text: &str, name: &str) -> Option<String> {
    let member = format!("\"addons/{name}\"");
    let start = text.find("\nmembers = [")? + "\nmembers = [".len();
    let end = start + text[start..].find(']')?;
    if text[start..end].contains(&member) {
        return Some(text.to_string());
    }
    let sep = if text[start..end].trim().is_empty() { "" } else { ", " };
    Some(format!("{}{sep}{member}{}", text[..end].trim_end_matches([' ', ',']), &text[end..]))
}

const DEPENDENCIES_NOTE: &str = "# Crates psx port add wrote (addons/).\n";

/// Adds `NAME = { path = "../addons/NAME" }` to game/Cargo.toml's
/// dependencies, after the add-ons already there.
fn add_dependency(text: &str, name: &str) -> Option<String> {
    let line = format!("{name} = {{ path = \"../addons/{name}\" }}\n");
    if text.contains(&line) {
        return Some(text.to_string());
    }
    let section = text.find("[dependencies]\n")? + "[dependencies]\n".len();
    let Some(note) = text[section..].find(DEPENDENCIES_NOTE).map(|at| section + at + DEPENDENCIES_NOTE.len()) else {
        return Some(format!("{}{DEPENDENCIES_NOTE}{line}{}", &text[..section], &text[section..]));
    };
    let mut at = note;
    while text[at..].split_inclusive('\n').next().is_some_and(|l| l.contains("path = \"../addons/")) {
        at += text[at..].find('\n').map_or(text.len() - at, |n| n + 1);
    }
    Some(format!("{}{line}{}", &text[..at], &text[at..]))
}

pub fn add(dest: &Path, name: &str) -> crate::Result<Value> {
    let mut manifest = Manifest::read(dest)?;
    let mut wanted = Vec::new();
    closure(name, &mut wanted)?;
    let (mut added, mut already, mut written, mut conflicts, mut next) = (vec![], vec![], vec![], vec![], vec![]);
    // Refuse before writing anything when a new add-on's folder is taken.
    for addon in wanted.iter().filter(|a| a.member && !manifest.addons.iter().any(|n| n == a.name)) {
        let dir = dest.join("addons").join(addon.name);
        if dir.exists() {
            return Err(Error::invalid(format!(
                "{} already exists but {MANIFEST} doesn't list {}; move it aside to add it",
                dir.display(),
                addon.name
            )));
        }
    }
    for addon in wanted {
        if manifest.addons.iter().any(|n| n == addon.name) {
            already.push(addon.name);
            continue;
        }
        for (rel, contents) in addon.files {
            let rel = addon.dest(rel);
            let new_hash = sha256(contents.as_bytes());
            let current = std::fs::read(dest.join(&rel)).ok().map(|b| sha256(&b));
            // A file of the base template the project hasn't changed is
            // replaced; any other file already there gets the add-on's
            // version beside it.
            match current {
                Some(now) if now != new_hash && manifest.seeded.get(&rel) != Some(&now) => {
                    let beside = format!("{rel}.psx-new");
                    write_file(dest, &beside, contents.as_bytes())?;
                    conflicts.push(json!({"path": rel, "new_version": beside}));
                }
                _ => {
                    write_file(dest, &rel, contents.as_bytes())?;
                    written.push(rel.clone());
                }
            }
            manifest.seeded.insert(rel, new_hash);
        }
        if addon.member {
            for (rel, edit) in
                [("Cargo.toml", add_member as fn(&str, &str) -> Option<String>), ("game/Cargo.toml", add_dependency)]
            {
                let path = dest.join(rel);
                let text = std::fs::read_to_string(&path).map_err(|e| Error::io(&path, e))?;
                match edit(&text, addon.name) {
                    Some(new) if new != text => std::fs::write(&path, new).map_err(|e| Error::io(&path, e))?,
                    Some(_) => {}
                    None if rel == "Cargo.toml" => {
                        next.push(format!("add \"addons/{}\" to the workspace members in Cargo.toml", addon.name))
                    }
                    None => next.push(format!(
                        "add {0} = {{ path = \"../addons/{0}\" }} to game/Cargo.toml's dependencies",
                        addon.name
                    )),
                }
            }
        }
        note_in_agents(dest, addon)?;
        manifest.addons.push(addon.name.to_string());
        added.push(addon.name);
    }
    manifest.write(dest)?;
    if !conflicts.is_empty() {
        next.push("merge each conflict's new_version into its path, then delete the .psx-new file".into());
    }
    if added.contains(&"full") {
        next.push(
            "cargo run (asks for the disc, imports it, then plays the demo; --disc PATH imports that one)".into(),
        );
    }
    next.push("cargo test --workspace".into());
    Ok(json!({
        "added": added,
        "already_added": already,
        "written": written,
        "conflicts": conflicts,
        "next": next,
    }))
}

const AGENTS_HEADING: &str = "## Add-ons";

/// A line for the add-on under AGENTS.md's add-ons heading (added at the end
/// when missing).
fn note_in_agents(dest: &Path, addon: &Addon) -> crate::Result<()> {
    let path = dest.join("AGENTS.md");
    let Ok(mut text) = std::fs::read_to_string(&path) else { return Ok(()) };
    let place = if addon.member { format!("`addons/{}/`", addon.name) } else { format!("`{}`", addon.name) };
    let line = format!("- {place} (`psx port add {}`): {}\n", addon.name, addon.about);
    if text.contains(&line) {
        return Ok(());
    }
    if !text.contains(AGENTS_HEADING) {
        if !text.ends_with('\n') {
            text.push('\n');
        }
        text.push_str(&format!(
            "\n{AGENTS_HEADING}\n\nCrates `psx port add` wrote. They are this project's code: use them as \
             they are, change them, or move what you want into game/ and delete the rest. `psx port \
             update` offers newer versions beside them as `.psx-new` files.\n\n"
        ));
    }
    if !text.ends_with('\n') {
        text.push('\n');
    }
    text.push_str(&line);
    std::fs::write(&path, text).map_err(|e| Error::io(&path, e))
}

/// `update`'s part for the add-ons' files: a newer version is written beside
/// the project's copy as `<file>.psx-new` (or as the file, when it is new to
/// an add-on still in the workspace). Returns (added, conflicts).
pub(crate) fn update(dest: &Path, manifest: &mut Manifest, dry_run: bool) -> crate::Result<(Vec<String>, Vec<Value>)> {
    let (mut added, mut conflicts) = (vec![], vec![]);
    for (name, rel, contents) in files(&manifest.addons) {
        let new_hash = sha256(contents.as_bytes());
        let recorded = manifest.seeded.get(&rel).cloned();
        let beside = format!("{rel}.psx-new");
        match std::fs::read(dest.join(&rel)).ok().map(|b| sha256(&b)) {
            _ if recorded.as_ref() == Some(&new_hash) => {
                if dest.join(&beside).exists() {
                    conflicts.push(json!({"path": rel, "new_version": beside}));
                }
                continue;
            }
            Some(now) if now == new_hash => {}
            Some(_) => {
                if !dry_run {
                    write_file(dest, &beside, contents.as_bytes())?;
                }
                conflicts.push(json!({"path": rel, "new_version": beside}));
            }
            // Gone: deleted by the project, unless the add-on gained it.
            None => {
                let root = find(name).map(|a| a.member).unwrap_or(false);
                let present = !root || dest.join("addons").join(name).is_dir();
                if recorded.is_none() && present {
                    if !dry_run {
                        write_file(dest, &rel, contents.as_bytes())?;
                    }
                    added.push(rel.clone());
                }
            }
        }
        if !dry_run {
            manifest.seeded.insert(rel, new_hash);
        }
    }
    Ok((added, conflicts))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::template::{TemplateOptions, create};

    #[test]
    fn full_adds_its_crates_wired_into_the_game() {
        let dir = std::env::temp_dir().join(format!("psx-addon-full-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        create(&dir, Some("port"), TemplateOptions::default()).unwrap();
        let out = add(&dir, "full").unwrap();
        assert_eq!(out["added"], json!(["shell", "renderer", "disc-import", "menus", "full"]));
        assert_eq!(out["conflicts"], json!([]));
        let root = std::fs::read_to_string(dir.join("Cargo.toml")).unwrap();
        assert!(root.contains(
            r#"members = ["game", "tools/import", "glossary", "addons/shell", "addons/renderer", "addons/disc-import", "addons/menus"]"#
        ));
        let game = std::fs::read_to_string(dir.join("game/Cargo.toml")).unwrap();
        assert!(game.contains(
            "[dependencies]\n# Crates psx port add wrote (addons/).\nshell = { path = \"../addons/shell\" }\n\
             renderer = { path = \"../addons/renderer\" }\ndisc-import = { path = \"../addons/disc-import\" }\n\
             menus = { path = \"../addons/menus\" }\n# The glossary"
        ));
        let main = std::fs::read_to_string(dir.join("game/src/main.rs")).unwrap();
        assert!(main.contains("mod demo;"), "an untouched main.rs is replaced");
        let agents = std::fs::read_to_string(dir.join("AGENTS.md")).unwrap();
        assert!(agents.contains("- `addons/renderer/` (`psx port add renderer`): ") && agents.contains("- `full` ("));
        let manifest = Manifest::read(&dir).unwrap();
        assert_eq!(manifest.addons.len(), 5);
        assert!(manifest.seeded.contains_key("addons/shell/src/lib.rs"));

        // Again: nothing to do. The base's update leaves the add-ons alone.
        assert_eq!(add(&dir, "menus").unwrap()["already_added"], json!(["shell", "renderer", "menus"]));
        let up = super::super::update(&dir, false).unwrap();
        assert_eq!((up["seeded_changes"].clone(), up["conflicts"].clone()), (json!([]), json!([])));

        // A newer add-on version goes beside the project's edited copy.
        let lib = "addons/shell/src/lib.rs";
        let mut manifest = Manifest::read(&dir).unwrap();
        manifest.seeded.insert(lib.into(), sha256(b"// older\n"));
        manifest.write(&dir).unwrap();
        std::fs::write(dir.join(lib), "// the port's shell\n").unwrap();
        let up = super::super::update(&dir, false).unwrap();
        assert_eq!(up["conflicts"], json!([{"path": lib, "new_version": format!("{lib}.psx-new")}]));
        assert_eq!(std::fs::read_to_string(dir.join(lib)).unwrap(), "// the port's shell\n");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn an_edited_main_gets_full_beside_it() {
        let dir = std::env::temp_dir().join(format!("psx-addon-main-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        create(&dir, Some("port"), TemplateOptions::default()).unwrap();
        std::fs::write(dir.join("game/src/main.rs"), "fn main() {}\n").unwrap();
        let out = add(&dir, "full").unwrap();
        assert_eq!(out["conflicts"][0]["path"], "game/src/main.rs");
        assert_eq!(std::fs::read_to_string(dir.join("game/src/main.rs")).unwrap(), "fn main() {}\n");
        assert!(dir.join("game/src/main.rs.psx-new").exists() && dir.join("game/src/demo.rs").exists());
        assert!(add(&dir, "nope").is_err());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn edits_manifests() {
        let root = "[workspace]\nresolver = \"3\"\nmembers = [\"game\", \"glossary\"]\n";
        let once = add_member(root, "shell").unwrap();
        assert!(once.contains("members = [\"game\", \"glossary\", \"addons/shell\"]\n"));
        assert_eq!(add_member(&once, "shell").unwrap(), once);
        assert!(add_member("[workspace]\n", "shell").is_none());
        let game = "[dependencies]\nglossary = { path = \"../glossary\" }\n";
        let one = add_dependency(game, "shell").unwrap();
        assert!(one.starts_with("[dependencies]\n# Crates psx port add wrote (addons/).\nshell = { path"));
        let two = add_dependency(&one, "menus").unwrap();
        assert!(two.contains("\"../addons/shell\" }\nmenus = { path = \"../addons/menus\" }\nglossary"));
        assert_eq!(add_dependency(&two, "menus").unwrap(), two);
    }

    /// CI: the base template plus `full` written to $PSX_ADDONS_WORKSPACE,
    /// which CI then formats and tests.
    #[test]
    #[ignore = "writes the workspace CI tests: PSX_ADDONS_WORKSPACE=DIR"]
    fn write_full_workspace() {
        let dir = std::path::PathBuf::from(std::env::var_os("PSX_ADDONS_WORKSPACE").expect("PSX_ADDONS_WORKSPACE"));
        create(&dir, Some("port"), TemplateOptions::default()).unwrap();
        assert_eq!(add(&dir, "full").unwrap()["conflicts"], json!([]));
    }

    /// Every file under templates/addons must be embedded, in an add-on of
    /// its folder's name.
    #[test]
    fn embeds_every_addon_file() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../templates/addons");
        let mut found = Vec::new();
        let mut stack = vec![root.clone()];
        while let Some(dir) = stack.pop() {
            for entry in std::fs::read_dir(&dir).unwrap() {
                let path = entry.unwrap().path();
                let rel = path.strip_prefix(&root).unwrap().to_string_lossy().replace('\\', "/");
                if rel.ends_with("/target") || rel == "target" || rel.ends_with(".psx-new") {
                    continue;
                }
                if path.is_dir() {
                    stack.push(path);
                } else {
                    found.push(rel);
                }
            }
        }
        let embedded: Vec<String> =
            ADDONS.iter().flat_map(|a| a.files.iter().map(move |(p, _)| format!("{}/{p}", a.name))).collect();
        let missing: Vec<&String> = found.iter().filter(|f| !embedded.contains(f)).collect();
        assert!(missing.is_empty(), "add-on files not in ADDONS: {missing:?}");
    }
}
