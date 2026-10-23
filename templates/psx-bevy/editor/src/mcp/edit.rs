//! The MCP's editing tools: what the menus do (open, new, save, undo) and the
//! level's contents, changed directly. Each change goes through `crate::edit`,
//! so it is one undo step like a menu command, and the status bar says what an
//! agent did ("agent: ..."). Edits are refused while the person at the editor
//! is part way through a gesture or a dialog, so the two never tangle.
//!
//! A new format gets its tools here: add, edit and delete for what it holds,
//! with the same argument style (indices as `editor_level` lists them, fields
//! left out keep their values).
use crate::Status;
use crate::document::{Document, History, Selection};
use bevy::prelude::Vec3;
use engine::level::TriggerPlacement;
use engine::prelude::*;
use serde::Deserialize;
use serde_json::{Value, json};
use std::path::PathBuf;

/// What an editing tool asks for.
#[derive(Debug)]
pub(super) enum Edit {
    /// Open a file, or a new level when there is no path.
    Open {
        path: Option<PathBuf>,
        discard: bool,
    },
    Save {
        path: Option<PathBuf>,
    },
    Undo {
        redo: bool,
        steps: u32,
    },
    AddObject(ObjectFields),
    ChangeObject(usize, ObjectFields),
    DeleteObject(usize),
    AddTrigger(TriggerFields),
    ChangeTrigger(usize, TriggerFields),
    DeleteTrigger(usize),
}

/// An object's fields; on an edit, those left out keep their values.
#[derive(Deserialize, Debug, Default)]
#[serde(deny_unknown_fields)]
pub(super) struct ObjectFields {
    type_id: Option<u32>,
    position: Option<[f32; 3]>,
    yaw: Option<f32>,
    params: Option<Vec<i32>>,
}

/// A trigger's fields; on an edit, those left out keep their values.
#[derive(Deserialize, Debug, Default)]
#[serde(deny_unknown_fields)]
pub(super) struct TriggerFields {
    position: Option<[f32; 3]>,
    half_extents: Option<[f32; 3]>,
    action: Option<u32>,
    args: Option<Vec<i32>>,
    once: Option<bool>,
}

/// What the editing tools change.
pub(super) struct Target<'a> {
    pub document: &'a mut Document,
    pub history: &'a mut History,
    pub selection: &'a mut Selection,
    pub status: &'a mut Status,
}

pub(super) fn tools() -> Vec<(&'static str, &'static str, Value)> {
    let index = json!({"type": "integer", "minimum": 0, "description": "As editor_level lists them"});
    let triple = json!({"type": "array", "items": {"type": "number"}, "minItems": 3, "maxItems": 3});
    let ints = json!({"type": "array", "items": {"type": "integer"}});
    let object = |required: &[&str], with_index: bool| {
        let mut properties = json!({
            "type_id": {"type": "integer", "minimum": 0},
            "position": triple.clone(),
            "yaw": {"type": "number", "description": "Radians about +Y"},
            "params": ints.clone(),
        });
        if with_index {
            properties["index"] = index.clone();
        }
        json!({"type": "object", "properties": properties, "required": required, "additionalProperties": false})
    };
    let trigger = |required: &[&str], with_index: bool| {
        let mut properties = json!({
            "position": triple.clone(),
            "half_extents": triple.clone(),
            "action": {"type": "integer", "minimum": 0},
            "args": ints.clone(),
            "once": {"type": "boolean"},
        });
        if with_index {
            properties["index"] = index.clone();
        }
        json!({"type": "object", "properties": properties, "required": required, "additionalProperties": false})
    };
    let by_index =
        json!({"type": "object", "properties": {"index": index}, "required": ["index"], "additionalProperties": false});
    let path =
        json!({"type": "string", "description": "Relative to the editor's working directory (the project root)"});
    vec![
        (
            "level_add_object",
            "Place an object: a type id and a position in metres, with optional yaw and params. Selects it and returns its index.",
            object(&["type_id", "position"], false),
        ),
        (
            "level_edit_object",
            "Change fields of the object at index; fields left out keep their values.",
            object(&["index"], true),
        ),
        ("level_delete_object", "Delete the object at index. Later objects' indices drop by one.", by_index.clone()),
        (
            "level_add_trigger",
            "Add a trigger box: centre and half extents in metres and the action it calls, with optional args and once. Selects it and returns its index.",
            trigger(&["position", "half_extents", "action"], false),
        ),
        (
            "level_edit_trigger",
            "Change fields of the trigger at index; fields left out keep their values.",
            trigger(&["index"], true),
        ),
        ("level_delete_trigger", "Delete the trigger at index. Later triggers' indices drop by one.", by_index),
        (
            "editor_undo",
            "Undo (or redo) the last steps, whoever made them: the person's and agents' share one history.",
            json!({"type": "object", "properties": {"redo": {"type": "boolean", "default": false}, "steps": {"type": "integer", "minimum": 1, "maximum": 100, "default": 1}}, "additionalProperties": false}),
        ),
        (
            "editor_open",
            "Open a level file, or a new empty level without path. Refused over unsaved changes unless discard is true.",
            json!({"type": "object", "properties": {"path": path.clone(), "discard": {"type": "boolean", "default": false}}, "additionalProperties": false}),
        ),
        (
            "editor_save",
            "Save the level, to path if given (Save As) or where it came from. An untouched file is written back byte for byte.",
            json!({"type": "object", "properties": {"path": path}, "additionalProperties": false}),
        ),
    ]
}

/// The editing tool `name` asks for, if it is one.
pub(super) fn parse(name: &str, value: Value) -> Option<Result<Edit, String>> {
    fn de<T: serde::de::DeserializeOwned>(v: Value) -> Result<T, String> {
        serde_json::from_value(v).map_err(|e| e.to_string())
    }
    /// Splits `index` off the fields.
    fn indexed<T: serde::de::DeserializeOwned>(mut v: Value) -> Result<(usize, T), String> {
        let index = v.as_object_mut().and_then(|o| o.remove("index")).ok_or("index is required")?;
        Ok((de(index)?, de(v)?))
    }
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Index {
        index: usize,
    }
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct UndoArgs {
        #[serde(default)]
        redo: bool,
        steps: Option<u32>,
    }
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct OpenArgs {
        path: Option<PathBuf>,
        #[serde(default)]
        discard: bool,
    }
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct SaveArgs {
        path: Option<PathBuf>,
    }
    Some(match name {
        "level_add_object" => de::<ObjectFields>(value).and_then(|f| {
            if f.type_id.is_none() || f.position.is_none() {
                return Err("type_id and position are required".into());
            }
            Ok(Edit::AddObject(f))
        }),
        "level_edit_object" => indexed(value).map(|(i, f)| Edit::ChangeObject(i, f)),
        "level_delete_object" => de::<Index>(value).map(|a| Edit::DeleteObject(a.index)),
        "level_add_trigger" => de::<TriggerFields>(value).and_then(|f| {
            if f.position.is_none() || f.half_extents.is_none() || f.action.is_none() {
                return Err("position, half_extents and action are required".into());
            }
            Ok(Edit::AddTrigger(f))
        }),
        "level_edit_trigger" => indexed(value).map(|(i, f)| Edit::ChangeTrigger(i, f)),
        "level_delete_trigger" => de::<Index>(value).map(|a| Edit::DeleteTrigger(a.index)),
        "editor_undo" => de::<UndoArgs>(value).and_then(|a| match a.steps.unwrap_or(1) {
            steps @ 1..=100 => Ok(Edit::Undo { redo: a.redo, steps }),
            _ => Err("steps must be 1..100".into()),
        }),
        "editor_open" => de::<OpenArgs>(value).map(|a| Edit::Open { path: a.path, discard: a.discard }),
        "editor_save" => de::<SaveArgs>(value).map(|a| Edit::Save { path: a.path }),
        _ => return None,
    })
}

fn vec3(v: [f32; 3], name: &str) -> Result<Vec3, String> {
    let v = Vec3::from_array(v);
    if v.is_finite() { Ok(v) } else { Err(format!("{name} must be finite")) }
}

fn finite(v: f32, name: &str) -> Result<f32, String> {
    if v.is_finite() { Ok(v) } else { Err(format!("{name} must be finite")) }
}

impl ObjectFields {
    fn apply(self, o: &mut Placement) -> Result<(), String> {
        if let Some(t) = self.type_id {
            o.type_id = ObjectTypeId(t);
        }
        if let Some(p) = self.position {
            o.position = vec3(p, "position")?;
        }
        if let Some(y) = self.yaw {
            o.yaw = finite(y, "yaw")?;
        }
        if let Some(p) = self.params {
            o.params = p;
        }
        Ok(())
    }
}

impl TriggerFields {
    fn apply(self, t: &mut TriggerPlacement) -> Result<(), String> {
        if let Some(p) = self.position {
            t.position = vec3(p, "position")?;
        }
        if let Some(h) = self.half_extents {
            t.half_extents = vec3(h, "half_extents")?;
            if t.half_extents.min_element() < 0.0 {
                return Err("half_extents can't be negative".into());
            }
        }
        if let Some(a) = self.action {
            t.call.action = ActionId(a);
        }
        if let Some(a) = self.args {
            t.call.args = a;
        }
        if let Some(o) = self.once {
            t.once = o;
        }
        Ok(())
    }
}

/// Applies an edit unless `busy` says why not. Returns what it did, with
/// enough to carry on (an index, the undo depth) without another read.
pub(super) fn apply(edit: Edit, busy: Option<&str>, mut t: Target) -> Result<Value, String> {
    if let Some(why) = busy {
        return Err(why.into());
    }
    let said = match edit {
        Edit::Open { path, discard } => {
            if t.document.is_dirty() && !discard {
                return Err("the level has unsaved changes: save first, or pass discard: true".into());
            }
            crate::load(path.as_deref(), t.document, t.history, t.selection)?
        }
        Edit::Save { path } => {
            let path = t.document.save(path.as_deref())?;
            format!("saved {}", path.display())
        }
        Edit::Undo { redo, steps } => {
            let done = (0..steps)
                .take_while(|_| {
                    if redo { t.history.redo(&mut t.document.level) } else { t.history.undo(&mut t.document.level) }
                })
                .count();
            *t.selection = t.selection.valid(&t.document.level);
            format!("{} {done} step(s)", if redo { "redid" } else { "undid" })
        }
        Edit::AddObject(fields) => {
            let mut object = Placement { type_id: ObjectTypeId(0), position: Vec3::ZERO, yaw: 0.0, params: vec![] };
            fields.apply(&mut object)?;
            change(&mut t, |level| {
                level.objects.push(object);
                Ok(Selection::Object(level.objects.len() - 1))
            })?;
            format!("added object {}", t.document.level.objects.len() - 1)
        }
        Edit::ChangeObject(i, fields) => {
            let mut object = t.document.level.objects.get(i).ok_or(format!("no object {i}"))?.clone();
            fields.apply(&mut object)?;
            change(&mut t, |level| {
                level.objects[i] = object;
                Ok(Selection::Object(i))
            })?;
            format!("edited object {i}")
        }
        Edit::DeleteObject(i) => {
            change(&mut t, |level| {
                level.objects.get(i).ok_or(format!("no object {i}"))?;
                level.objects.remove(i);
                Ok(Selection::None)
            })?;
            format!("deleted object {i}")
        }
        Edit::AddTrigger(fields) => {
            let mut trigger = crate::document::new_trigger();
            fields.apply(&mut trigger)?;
            change(&mut t, |level| {
                level.triggers.push(trigger);
                Ok(Selection::Trigger(level.triggers.len() - 1))
            })?;
            format!("added trigger {}", t.document.level.triggers.len() - 1)
        }
        Edit::ChangeTrigger(i, fields) => {
            let mut trigger = t.document.level.triggers.get(i).ok_or(format!("no trigger {i}"))?.clone();
            fields.apply(&mut trigger)?;
            change(&mut t, |level| {
                level.triggers[i] = trigger;
                Ok(Selection::Trigger(i))
            })?;
            format!("edited trigger {i}")
        }
        Edit::DeleteTrigger(i) => {
            change(&mut t, |level| {
                level.triggers.get(i).ok_or(format!("no trigger {i}"))?;
                level.triggers.remove(i);
                Ok(Selection::None)
            })?;
            format!("deleted trigger {i}")
        }
    };
    t.status.0 = format!("agent: {said}");
    Ok(json!({
        "done": said,
        "selection": super::selection_json(*t.selection),
        "dirty": t.document.is_dirty(),
        "can_undo": t.history.can_undo(),
        "counts": {"objects": t.document.level.objects.len(), "triggers": t.document.level.triggers.len()},
    }))
}

/// One undo step through the menus' `crate::edit`, or nothing when `f` fails.
fn change(t: &mut Target, f: impl FnOnce(&mut LevelData) -> Result<Selection, String>) -> Result<(), String> {
    let mut result = Ok(());
    crate::edit(t.document, t.history, t.selection, |level, current| match f(level) {
        Ok(selected) => selected,
        Err(e) => {
            result = Err(e);
            current
        }
    });
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Editor {
        document: Document,
        history: History,
        selection: Selection,
        status: Status,
    }

    impl Editor {
        fn new() -> Self {
            Self {
                document: Document::new_level(),
                history: History::default(),
                selection: Selection::None,
                status: Status::default(),
            }
        }

        fn call(&mut self, name: &str, args: Value) -> Result<Value, String> {
            self.call_busy(name, args, None)
        }

        fn call_busy(&mut self, name: &str, args: Value, busy: Option<&str>) -> Result<Value, String> {
            let edit = parse(name, args).expect("an editing tool")?;
            let target = Target {
                document: &mut self.document,
                history: &mut self.history,
                selection: &mut self.selection,
                status: &mut self.status,
            };
            apply(edit, busy, target)
        }
    }

    #[test]
    fn every_edit_is_one_undo_step() {
        let mut e = Editor::new();
        let added = e.call("level_add_object", json!({"type_id": 3, "position": [1, 0, 2], "params": [7]})).unwrap();
        assert_eq!(added["selection"], json!({"object": 0}));
        assert_eq!(e.status.0, "agent: added object 0");
        e.call("level_edit_object", json!({"index": 0, "yaw": 1.5})).unwrap();
        let object = &e.document.level.objects[0];
        // Fields left out keep their values.
        assert_eq!(
            (object.type_id.0, object.position, object.yaw, &object.params),
            (3, Vec3::new(1.0, 0.0, 2.0), 1.5, &vec![7])
        );
        e.call("level_add_trigger", json!({"position": [0, 0, 0], "half_extents": [1, 1, 1], "action": 4})).unwrap();
        e.call("level_delete_object", json!({"index": 0})).unwrap();
        assert!(e.document.level.objects.is_empty());

        let undone = e.call("editor_undo", json!({"steps": 2})).unwrap();
        assert_eq!(undone["done"], "undid 2 step(s)");
        assert_eq!(e.document.level.objects[0].yaw, 1.5);
        assert!(e.document.level.triggers.is_empty());
        e.call("editor_undo", json!({"steps": 5})).unwrap();
        assert_eq!(e.document.level, LevelData::default());
        assert!(!e.document.is_dirty());
        e.call("editor_undo", json!({"redo": true})).unwrap();
        assert_eq!(e.document.level.objects.len(), 1);
    }

    #[test]
    fn bad_edits_change_nothing() {
        let mut e = Editor::new();
        e.call("level_add_trigger", json!({"position": [0, 0, 0], "half_extents": [1, 1, 1], "action": 1})).unwrap();
        let before = e.document.level.clone();
        for (name, args) in [
            ("level_edit_trigger", json!({"index": 0, "half_extents": [1, -1, 1]})),
            ("level_edit_trigger", json!({"index": 3, "once": true})),
            ("level_delete_object", json!({"index": 0})),
            ("level_add_object", json!({"position": [0, 0, 0]})),
            ("level_edit_object", json!({"index": 0, "colour": 2})),
            ("editor_undo", json!({"steps": 0})),
        ] {
            assert!(e.call(name, args.clone()).is_err(), "{name} {args}");
        }
        assert_eq!(e.document.level, before);
        assert!(e.history.can_undo());
        e.history.undo(&mut e.document.level);
        assert!(!e.history.can_undo(), "refused edits leave no undo steps");
    }

    #[test]
    fn edits_wait_for_the_person_and_unsaved_work_is_kept() {
        let mut e = Editor::new();
        let busy = e.call_busy("level_add_object", json!({"type_id": 1, "position": [0, 0, 0]}), Some("dragging"));
        assert_eq!(busy.unwrap_err(), "dragging");
        assert!(e.document.level.objects.is_empty());

        e.call("level_add_object", json!({"type_id": 1, "position": [0, 0, 0]})).unwrap();
        let refused = e.call("editor_open", json!({})).unwrap_err();
        assert!(refused.contains("unsaved"), "{refused}");
        assert_eq!(e.document.level.objects.len(), 1);
        e.call("editor_open", json!({"discard": true})).unwrap();
        assert!(e.document.level.objects.is_empty() && !e.history.can_undo());

        let path = std::env::temp_dir().join(format!("editor-mcp-{}/level.ron", std::process::id()));
        e.call("level_add_object", json!({"type_id": 2, "position": [1, 2, 3]})).unwrap();
        e.call("editor_save", json!({"path": path})).unwrap();
        assert!(!e.document.is_dirty());
        e.call("editor_open", json!({"path": path})).unwrap();
        assert_eq!(e.document.level.objects[0].position, Vec3::new(1.0, 2.0, 3.0));
    }
}
