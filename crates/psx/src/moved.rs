//! Paths kept in a project's files (saved defaults, state chains, case
//! recipes, run and handoff reports) after the project folder moves. psx
//! writes a path inside the project relative to its root; a path saved
//! absolute that no longer exists is found again under the root by its
//! trailing components.

use std::path::{Component, Path, PathBuf};

/// `p` as a project file keeps it: relative to `root` when it lies inside
/// (also when one of them is reached through a symlink), else absolute.
pub fn store(root: &Path, p: &Path) -> PathBuf {
    let abs = if p.is_absolute() { p.to_path_buf() } else { root.join(p) };
    if let Ok(rel) = abs.strip_prefix(root) {
        return rel.to_path_buf();
    }
    let canon = |q: &Path| q.canonicalize().ok();
    if let (Some(a), Some(r)) = (canon(&abs), canon(root)) {
        if let Ok(rel) = a.strip_prefix(&r) {
            return rel.to_path_buf();
        }
    }
    abs
}

/// A path a project file kept, now: a relative one under `root`; an absolute
/// one as it is while it exists, else the file with its longest trailing part
/// under `root` (samples.toml, .psx/states/a.bin), then, two components at
/// least (rom/game.cue), under the folder holding `root`: a disc kept beside
/// the project. Else as kept.
pub fn find(root: &Path, p: &Path) -> PathBuf {
    if !p.is_absolute() {
        return root.join(p);
    }
    if p.exists() {
        return p.to_path_buf();
    }
    let parts: Vec<Component> = p.components().filter(|c| matches!(c, Component::Normal(_))).collect();
    for (base, least) in [(Some(root), 1), (root.parent(), 2)] {
        let Some(base) = base else { continue };
        for skip in 0..(parts.len() + 1).saturating_sub(least) {
            let q = parts[skip..].iter().fold(base.to_path_buf(), |q, c| q.join(c));
            if q.exists() {
                return q;
            }
        }
    }
    p.to_path_buf()
}

/// Every absolute path in `v` (a report read back) that no longer exists,
/// through [`find`]: what a report from before the move names, where it is now.
pub fn relocate(root: &Path, v: &mut serde_json::Value) {
    match v {
        serde_json::Value::String(s) if Path::new(s.as_str()).is_absolute() => {
            *s = find(root, Path::new(s.as_str())).to_string_lossy().into_owned();
        }
        serde_json::Value::Array(a) => a.iter_mut().for_each(|x| relocate(root, x)),
        serde_json::Value::Object(m) => m.values_mut().for_each(|x| relocate(root, x)),
        _ => {}
    }
}

/// `p` from `base`: `../clean` for a sibling, with `..` as far up as they
/// share a folder; absolute when they share none but the filesystem's root.
pub fn relative(base: &Path, p: &Path) -> PathBuf {
    let (b, q): (Vec<Component>, Vec<Component>) = (base.components().collect(), p.components().collect());
    let shared = b.iter().zip(&q).take_while(|(x, y)| x == y).count();
    if shared <= 1 || !p.is_absolute() {
        return p.to_path_buf();
    }
    let mut out: PathBuf = b[shared..].iter().map(|_| Component::ParentDir).collect();
    out.extend(&q[shared..]);
    if out.as_os_str().is_empty() { PathBuf::from(".") } else { out }
}

/// A repository or folder kept beside `base` (a port's other repositories,
/// a legacy port): as kept while it exists, else the folder of the same
/// name beside `base` (the port moved as a whole), else as kept.
pub fn sibling(base: &Path, p: &Path) -> PathBuf {
    if p.exists() {
        return p.to_path_buf();
    }
    match (base.parent(), p.file_name()) {
        (Some(d), Some(n)) if d.join(n).is_dir() => d.join(n),
        _ => p.to_path_buf(),
    }
}

#[cfg(test)]
mod tests {
    use super::{find, relative, relocate, sibling, store};
    use std::path::Path;

    #[test]
    fn a_moved_project_finds_its_files_again() {
        let tmp = std::env::temp_dir().join(format!("psx-moved-{}", std::process::id()));
        let root = tmp.join("games/tr2/investigate");
        std::fs::create_dir_all(root.join(".psx/states")).unwrap();
        std::fs::create_dir_all(tmp.join("games/tr2/rom")).unwrap();
        std::fs::write(root.join(".psx/states/a.bin"), b"s").unwrap();
        std::fs::write(tmp.join("games/tr2/rom/game.cue"), b"c").unwrap();
        // Inside the root: relative. Outside: absolute.
        assert_eq!(store(&root, &root.join(".psx/states/a.bin")), Path::new(".psx/states/a.bin"));
        assert_eq!(store(&root, Path::new("bios/x.bin")), Path::new("bios/x.bin"));
        assert_eq!(store(&root, &tmp.join("games/tr2/rom/game.cue")), tmp.join("games/tr2/rom/game.cue"));
        #[cfg(unix)]
        {
            let link = tmp.join("link");
            std::os::unix::fs::symlink(&root, &link).unwrap();
            assert_eq!(store(&root, &link.join(".psx/states/a.bin")), Path::new(".psx/states/a.bin"));
        }
        // Saved before the move, from /old/place/investigate.
        let old = Path::new("/old/place/investigate/.psx/states/a.bin");
        assert_eq!(find(&root, old), root.join(".psx/states/a.bin"));
        assert_eq!(find(&root, Path::new("/old/place/rom/game.cue")), tmp.join("games/tr2/rom/game.cue"));
        assert_eq!(find(&root, Path::new(".psx/states/a.bin")), root.join(".psx/states/a.bin"));
        let mut report = serde_json::json!({"run": {"from_state": old, "frames": 3}, "note": "/old/a.bin"});
        relocate(&root, &mut report);
        assert_eq!(report["run"]["from_state"], serde_json::json!(root.join(".psx/states/a.bin")));
        assert_eq!(report["note"], "/old/a.bin");
        // A file at the root: one component is enough there, not beside it.
        std::fs::write(root.join("samples.toml"), b"m").unwrap();
        assert_eq!(find(&root, Path::new("/old/place/investigate/samples.toml")), root.join("samples.toml"));
        std::fs::write(tmp.join("games/tr2/b.bin"), b"b").unwrap();
        assert_eq!(find(&root, Path::new("/old/b.bin")), Path::new("/old/b.bin"));
        // A missing file stays as kept.
        assert_eq!(find(&root, Path::new("/old/a.bin")), Path::new("/old/a.bin"));
        assert_eq!(find(&root, Path::new("/old/place/nothing/here.bin")), Path::new("/old/place/nothing/here.bin"));
        std::fs::remove_dir_all(&tmp).unwrap();
    }

    #[test]
    fn repositories_beside_each_other_are_kept_relative() {
        let p = |s: &str| Path::new(s).to_path_buf();
        assert_eq!(relative(&p("/a/tr2/inv"), &p("/a/tr2/clean")), p("../clean"));
        assert_eq!(relative(&p("/a/tr2/inv"), &p("/a/rom/x.cue")), p("../../rom/x.cue"));
        assert_eq!(relative(&p("/a/tr2/inv"), &p("/a/tr2/inv/rom/x.cue")), p("rom/x.cue"));
        assert_eq!(relative(&p("/a/tr2/inv"), &p("/a/tr2/inv")), p("."));
        assert_eq!(relative(&p("/a/inv"), &p("/b/clean")), p("/b/clean"), "only the root in common");
        let tmp = std::env::temp_dir().join(format!("psx-sibling-{}", std::process::id()));
        std::fs::create_dir_all(tmp.join("tr2/clean")).unwrap();
        let inv = tmp.join("tr2/inv");
        assert_eq!(sibling(&inv, &p("/old/clean")), tmp.join("tr2/clean"));
        assert_eq!(sibling(&inv, &p("/old/gone")), p("/old/gone"));
        std::fs::remove_dir_all(&tmp).unwrap();
    }
}
