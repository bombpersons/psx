//! The editor's document model without a window: open, edit, undo, save.
use bevy::prelude::*;
use editor::document::{self, Document, History, Selection};
use std::path::PathBuf;

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("editor-test-{}-{name}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir.join("level.ron")
}

const LEVEL: &str = "// A comment that survives an untouched save.
(
    objects: [
        (type_id: (1), position: (0.0, 0.5, 0.0)),
        (type_id: (2), position: (-3.0, 1.0, -2.0), params: [1200, 0]),
    ],
    triggers: [],
)
";

#[test]
fn untouched_save_writes_the_file_back_byte_for_byte() {
    let path = scratch("untouched");
    std::fs::write(&path, LEVEL).unwrap();
    let mut doc = Document::open(&path).unwrap();
    assert!(!doc.is_dirty());
    doc.save(None).unwrap();
    assert_eq!(std::fs::read_to_string(&path).unwrap(), LEVEL);
}

#[test]
fn edits_save_and_reopen_equal() {
    let path = scratch("edited");
    std::fs::write(&path, LEVEL).unwrap();
    let mut doc = Document::open(&path).unwrap();
    doc.level.objects[1].position.x = 4.25;
    doc.level.triggers.push(document::new_trigger());
    assert!(doc.is_dirty());
    doc.save(None).unwrap();
    assert!(!doc.is_dirty());
    assert_eq!(Document::open(&path).unwrap().level, doc.level);
}

#[test]
fn a_gesture_is_one_undo_step_and_redo_restores_it() {
    let mut doc = Document::new_level();
    let mut history = History::default();
    let mut selection = Selection::None;

    // Add an object: one step.
    let before = doc.level.clone();
    doc.level.objects.push(engine::prelude::Placement {
        type_id: engine::prelude::ObjectTypeId(7),
        position: Vec3::ZERO,
        yaw: 0.0,
        params: vec![],
    });
    history.record(before, false);

    // Drag it over three frames: one more step.
    for _ in 0..3 {
        let before = doc.level.clone();
        doc.level.objects[0].position.x += 1.0;
        history.record(before, true);
    }
    history.end_gesture();
    assert_eq!(doc.level.objects[0].position.x, 3.0);

    assert!(history.undo(&mut doc.level));
    assert_eq!(doc.level.objects[0].position.x, 0.0);
    assert!(history.undo(&mut doc.level));
    assert!(doc.level.objects.is_empty());
    assert!(!history.can_undo());
    selection = selection.valid(&doc.level);
    assert_eq!(selection, Selection::None);

    assert!(history.redo(&mut doc.level));
    assert!(history.redo(&mut doc.level));
    assert_eq!(doc.level.objects[0].position.x, 3.0);
}

#[test]
fn duplicate_and_delete_follow_the_selection() {
    let mut level = ron::from_str::<engine::prelude::LevelData>(LEVEL).unwrap();
    let copy = document::duplicate(&mut level, Selection::Object(1));
    assert_eq!(copy, Selection::Object(2));
    assert_eq!(level.objects[2].params, level.objects[1].params);
    assert!(document::delete(&mut level, Selection::Object(0)));
    assert_eq!(level.objects.len(), 2);
    assert!(!document::delete(&mut level, Selection::Trigger(0)));
}

#[test]
fn opens_the_template_levels() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..");
    Document::open(&root.join("game/assets/demo/level.ron")).unwrap();
}
