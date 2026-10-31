//! The game through the real binary: offscreen GPU and the developer MCP,
//! with a software adapter.
#![cfg(feature = "dev-mcp")]

use psx_bevy::testing::mcp::Client;
use serde_json::{Value, json};
use std::process::Command;

fn croc(client: &mut Client) -> Value {
    client.call("game_query", json!({"name":"Croc","components":["Position","Facing"]}))["structuredContent"]["entities"][0]
        .clone()
}

#[test]
#[ignore = "requires a working offscreen GPU/software adapter; no display needed"]
fn headless_render_step_and_query() {
    let output = std::env::temp_dir().join(format!("croc-headless-{}", std::process::id()));
    std::fs::create_dir_all(&output).unwrap();
    println!("GPU evidence: {}", output.display());
    let mut command = Command::new(env!("CARGO_BIN_EXE_croc-legend-of-the-gobbos"));
    command.env("BEVY_ASSET_ROOT", env!("CARGO_MANIFEST_DIR"));
    // The stand-in world needs no disc import (no disc on test machines).
    command.arg("--skip-import");
    let mut client = Client::launch(command, &output.join("game.log"));
    let status = client.call("game_status", json!({}))["structuredContent"].clone();
    assert_eq!(status["paused"], true);
    assert_eq!(status["headless"], true);
    let before = client.screenshot(output.join("before.png"));
    assert_eq!(before.dimensions(), (game::SCREEN.x, game::SCREEN.y));
    let colors: std::collections::HashSet<_> = before.pixels().map(|p| p.0).collect();
    assert!(colors.len() > 8, "offscreen frame should render the scene, not an empty target");
    let level = game::world::Level::start().unwrap();
    let placed =
        level.placements.iter().find(|p| p.kind == game::croc::PLACEMENT).expect("the start places Croc").position;
    let start = croc(&mut client);
    assert_eq!(start["components"]["Position"], json!([placed.x, placed.y, placed.z]));
    // Forward for 20 fields: ten ticks of running along Croc's facing (+z).
    client.call("game_step", json!({"frames":20,"buttons":["up"]}));
    let moved = croc(&mut client);
    let z = moved["components"]["Position"][2].as_i64().unwrap();
    assert!(z > placed.z as i64 + 500, "Croc should have run forward: {moved}");
    let after = client.screenshot(output.join("after-step.png"));
    assert!(before.pixels().zip(after.pixels()).filter(|(a, b)| a != b).count() > 100, "the camera follows Croc");
    std::fs::write(output.join("status.json"), serde_json::to_vec_pretty(&status).unwrap()).unwrap();
}
