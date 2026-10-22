//! The open file, its edit history and what is selected: plain data, no UI,
//! so tests can drive it without a window.
//!
//! The template edits one format, the engine's [`LevelData`]. Add a document
//! kind per engine format as the engine grows (see `AGENTS.md`).
use bevy::prelude::*;
use engine::level::TriggerPlacement;
use engine::prelude::*;
use std::path::{Path, PathBuf};

/// A level open in the editor.
#[derive(Resource, Default)]
pub struct Document {
    /// Where it is saved; `None` until the first Save As.
    pub path: Option<PathBuf>,
    pub level: LevelData,
    /// The text read from disk, written back unchanged when nothing was
    /// edited, so opening and saving never reformats a file or drops its
    /// comments.
    original: Option<String>,
    /// The level as last saved or opened; unsaved when `level` differs.
    saved: LevelData,
}

impl Document {
    pub fn new_level() -> Self {
        Self::default()
    }

    pub fn open(path: &Path) -> Result<Self, String> {
        let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
        let level: LevelData = ron::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?;
        Ok(Self { path: Some(path.to_path_buf()), saved: level.clone(), level, original: Some(text) })
    }

    pub fn is_dirty(&self) -> bool {
        self.level != self.saved
    }

    /// The text `save` would write.
    pub fn to_text(&self) -> Result<String, String> {
        match &self.original {
            Some(text) if !self.is_dirty() => Ok(text.clone()),
            _ => ron::ser::to_string_pretty(&self.level, ron::ser::PrettyConfig::default())
                .map(|text| text + "\n")
                .map_err(|e| e.to_string()),
        }
    }

    /// Writes to `path` (or the current path) and makes it the current path.
    pub fn save(&mut self, path: Option<&Path>) -> Result<PathBuf, String> {
        let path = path.map(Path::to_path_buf).or_else(|| self.path.clone()).ok_or("no file name yet: use Save As")?;
        let text = self.to_text()?;
        if let Some(dir) = path.parent().filter(|dir| !dir.as_os_str().is_empty()) {
            std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        }
        std::fs::write(&path, &text).map_err(|e| format!("{}: {e}", path.display()))?;
        self.original = Some(text);
        self.saved = self.level.clone();
        self.path = Some(path.clone());
        Ok(path)
    }

    /// The window title: file name, with `*` while unsaved.
    pub fn title(&self) -> String {
        let name = self.path.as_deref().and_then(Path::file_name).map_or("Untitled".into(), |n| n.to_string_lossy());
        format!("{}{} - Editor", name, if self.is_dirty() { " *" } else { "" })
    }
}

/// Undo and redo as whole-level snapshots: simple, and exact for any edit.
/// Swap in finer-grained commands if levels grow large.
#[derive(Resource, Default)]
pub struct History {
    undo: Vec<LevelData>,
    redo: Vec<LevelData>,
    /// True while one gesture (a drag, typing in a field) is under way, so it
    /// becomes one undo step rather than one per frame.
    coalescing: bool,
}

impl History {
    /// Records `before` as an undo step for an edit that changed the level.
    /// While `ongoing` stays true, later changes join the same step.
    pub fn record(&mut self, before: LevelData, ongoing: bool) {
        if !self.coalescing {
            self.undo.push(before);
            self.redo.clear();
        }
        self.coalescing = ongoing;
    }

    /// Call when no edit happened this frame: the next one starts a new step.
    pub fn end_gesture(&mut self) {
        self.coalescing = false;
    }

    pub fn undo(&mut self, level: &mut LevelData) -> bool {
        self.coalescing = false;
        let Some(previous) = self.undo.pop() else { return false };
        self.redo.push(std::mem::replace(level, previous));
        true
    }

    pub fn redo(&mut self, level: &mut LevelData) -> bool {
        self.coalescing = false;
        let Some(next) = self.redo.pop() else { return false };
        self.undo.push(std::mem::replace(level, next));
        true
    }

    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    pub fn clear(&mut self) {
        *self = Self::default();
    }
}

/// What the outline, view and inspector act on.
#[derive(Resource, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Selection {
    #[default]
    None,
    Object(usize),
    Trigger(usize),
}

impl Selection {
    /// `None` when the index no longer exists (after undo or a delete).
    pub fn valid(self, level: &LevelData) -> Self {
        match self {
            Self::Object(i) if i < level.objects.len() => self,
            Self::Trigger(i) if i < level.triggers.len() => self,
            _ => Self::None,
        }
    }
}

/// Removes the selected item; returns whether anything was removed.
pub fn delete(level: &mut LevelData, selection: Selection) -> bool {
    match selection.valid(level) {
        Selection::Object(i) => drop(level.objects.remove(i)),
        Selection::Trigger(i) => drop(level.triggers.remove(i)),
        Selection::None => return false,
    }
    true
}

/// Copies the selected item next to the original; returns the copy's selection.
pub fn duplicate(level: &mut LevelData, selection: Selection) -> Selection {
    match selection.valid(level) {
        Selection::Object(i) => {
            let mut copy = level.objects[i].clone();
            copy.position.x += 1.0;
            level.objects.push(copy);
            Selection::Object(level.objects.len() - 1)
        }
        Selection::Trigger(i) => {
            let mut copy = level.triggers[i].clone();
            copy.position.x += 1.0;
            level.triggers.push(copy);
            Selection::Trigger(level.triggers.len() - 1)
        }
        Selection::None => Selection::None,
    }
}

/// A trigger for Edit > Add trigger: a 1 m cube at the origin calling action 1.
pub fn new_trigger() -> TriggerPlacement {
    TriggerPlacement {
        position: Vec3::ZERO,
        half_extents: Vec3::splat(0.5),
        call: ActionCall { action: ActionId(1), args: vec![], source: None },
        once: false,
    }
}
