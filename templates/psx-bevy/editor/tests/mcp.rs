//! The editor through its MCP, as an agent drives it: the real binary,
//! headless, with a software adapter. Extend it as the editor grows: a new
//! format's tools get a check here that edits through the MCP and through the
//! UI (widgets and input), and sees the result on screen.
#![cfg(feature = "dev-mcp")]

use base64::Engine;
use engine::testing::mcp::Client;
use serde_json::{Value, json};
use std::process::Command;

fn screenshot(client: &mut Client, args: Value, path: std::path::PathBuf) -> image::RgbaImage {
    let reply = client.call("editor_screenshot", args);
    let png =
        reply["content"].as_array().unwrap().iter().find(|c| c["type"] == "image").unwrap()["data"].as_str().unwrap();
    let bytes = base64::engine::general_purpose::STANDARD.decode(png).unwrap();
    std::fs::write(path, &bytes).unwrap();
    image::load_from_memory(&bytes).unwrap().to_rgba8()
}

/// The centre of the one widget whose label is exactly `label`.
fn widget(client: &mut Client, label: &str) -> Value {
    let found = client.call("editor_widgets", json!({"text": label}))["structuredContent"]["widgets"].clone();
    let exact: Vec<_> = found.as_array().unwrap().iter().filter(|w| w["label"] == label).collect();
    assert_eq!(exact.len(), 1, "one widget labelled {label:?}: {found}");
    exact[0]["centre"].clone()
}

#[test]
#[ignore = "requires a working offscreen GPU/software adapter; no display needed"]
fn agent_edits_and_ui_input_reach_the_level_and_the_screen() {
    let output = std::env::temp_dir().join(format!("editor-mcp-{}", std::process::id()));
    std::fs::create_dir_all(&output).unwrap();
    println!("GPU evidence: {}", output.display());
    let mut command = Command::new(env!("CARGO_BIN_EXE_editor"));
    // Paths in the tools are relative to the project root, as in `cargo run -p editor`.
    command.current_dir(concat!(env!("CARGO_MANIFEST_DIR"), "/..")).args([
        "game/assets/demo/level.ron",
        "--size",
        "1280x800",
    ]);
    let mut client = Client::launch(command, &output.join("editor.log"));

    let tools = client.request("tools/list", json!({}));
    let names: Vec<_> = tools["result"]["tools"].as_array().unwrap().iter().map(|t| t["name"].clone()).collect();
    for name in ["editor_status", "level_add_object", "editor_undo", "editor_input", "editor_widgets"] {
        assert!(names.contains(&json!(name)), "{name} in {names:?}");
    }
    let status = client.call("editor_status", json!({}))["structuredContent"].clone();
    assert_eq!((status["headless"].clone(), status["window"].clone()), (json!(true), json!([1280.0, 800.0])));
    assert_eq!(status["counts"], json!({"objects": 3, "triggers": 1}));
    let before = screenshot(&mut client, json!({"panel": "view"}), output.join("before.png"));

    // An agent's edit: one undo step, drawn in the view.
    let added =
        client.call("level_add_object", json!({"type_id": 9, "position": [4, 0, 3]}))["structuredContent"].clone();
    assert_eq!(added["selection"], json!({"object": 3}));
    let at = client.call("editor_locate", json!({"object": 3}))["structuredContent"].clone();
    assert_eq!(at["visible"], true, "{at}");
    let after = screenshot(&mut client, json!({"panel": "view"}), output.join("after-add.png"));
    assert_eq!(before.dimensions(), after.dimensions());
    assert!(before.pixels().zip(after.pixels()).filter(|(a, b)| a != b).count() > 50, "the new object is drawn");

    // A person's gesture, played in: drag the object 2 m right in the view.
    let scale = status["view"]["pixels_per_metre"].as_f64().unwrap();
    let (x, y) = (at["at"][0].as_f64().unwrap(), at["at"][1].as_f64().unwrap());
    client.call("editor_input", json!({"steps": [{"do": "drag", "from": [x, y], "to": [x + 2.0 * scale, y]}]}));
    let objects = client.call("editor_level", json!({"part": "objects"}))["structuredContent"]["objects"].clone();
    let moved = objects[3]["position"][0].as_f64().unwrap();
    assert!((moved - 6.0).abs() < 0.01, "dragged to x = 6: {objects}");

    // The menus by label: Edit > Undo twice (the drag, then the agent's add).
    for _ in 0..2 {
        let edit = widget(&mut client, "Edit");
        client.call("editor_input", json!({"steps": [{"do": "click", "at": edit}]}));
        let undo =
            client.call("editor_widgets", json!({"text": "Undo", "panel": "pop-up"}))["structuredContent"]["widgets"]
                [0]["centre"]
                .clone();
        client.call("editor_input", json!({"steps": [{"do": "click", "at": undo}]}));
    }
    let status = client.call("editor_status", json!({}))["structuredContent"].clone();
    assert_eq!((status["counts"]["objects"].clone(), status["dirty"].clone()), (json!(3), json!(false)), "{status}");

    // Keyboard shortcuts reach the editor too: Ctrl+Y redoes the add.
    client.call("editor_input", json!({"steps": [{"do": "key", "key": "Ctrl+Y"}]}));
    assert_eq!(client.call("editor_status", json!({}))["structuredContent"]["counts"]["objects"], 4);
    screenshot(&mut client, json!({}), output.join("window.png"));

    // Unsaved work is the person's to discard, not an agent's.
    let refused = client.request("tools/call", json!({"name": "editor_open", "arguments": {}}));
    assert_eq!(refused["result"]["isError"], true, "{refused}");
}
