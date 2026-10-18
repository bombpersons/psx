//! The demo through the real binary: offscreen GPU and the developer MCP,
//! with a software adapter. Replace with checks of your own game.
#![cfg(feature = "dev-mcp")]

use engine::testing::mcp::Client;
use game::demo::player::PlayerTuning;
use serde_json::{Value, json};
use std::process::Command;

fn player(client: &mut Client) -> Value {
    client.call("game_query", json!({"name":"Player","components":["Transform"]}))["structuredContent"]["entities"][0]
        .clone()
}

#[test]
#[ignore = "requires a working offscreen GPU/software adapter; no display needed"]
fn headless_render_edit_step_and_query() {
    let output = std::env::temp_dir().join(format!("psx-template-headless-{}", std::process::id()));
    std::fs::create_dir_all(&output).unwrap();
    println!("GPU evidence: {}", output.display());
    let mut command = Command::new(env!(concat!("CARGO_BIN_EXE_", env!("CARGO_PKG_NAME"))));
    command.env("BEVY_ASSET_ROOT", env!("CARGO_MANIFEST_DIR"));
    let mut client = Client::launch(command, &output.join("game.log"));
    let tools = client.request("tools/list", json!({}));
    assert!(tools["result"]["tools"].as_array().unwrap().iter().any(|t| t["name"] == "game_set_component"));
    let status = client.call("game_status", json!({}))["structuredContent"].clone();
    assert_eq!(status["paused"], true);
    assert_eq!(status["headless"], true);
    assert_eq!(status["frame"], 0);
    let window = client.request("tools/call", json!({"name":"game_screenshot","arguments":{"target":"window"}}));
    assert!(window["error"]["message"].as_str().unwrap().contains("No primary window"), "{window}");
    let before = client.screenshot(output.join("before.png"));
    assert_eq!(before.dimensions(), (game::SCREEN.x, game::SCREEN.y));
    let colors: std::collections::HashSet<_> = before.pixels().map(|p| p.0).collect();
    assert!(colors.len() > 32, "offscreen frame should render the scene, not an empty target");
    let state = player(&mut client);
    assert_eq!(state["components"]["Transform"]["translation"], json!([0.0, 0.5, 0.0]));
    let edit = client.call(
        "game_set_component",
        json!({"entity":state["entity"],"component":"Transform","fields":{"translation":[2.0,0.5,0.0]}}),
    )["structuredContent"]
        .clone();
    assert_eq!(edit["frame"], 0);
    assert_eq!(edit["after"]["translation"], json!([2.0, 0.5, 0.0]));
    assert_eq!(player(&mut client)["components"]["Transform"]["translation"], json!([2.0, 0.5, 0.0]));
    let invalid = client.request("tools/call",json!({"name":"game_set_component","arguments":{"name":"Player","component":"Transform","fields":{"translation":[99,0.5,0],"zzz":1}}}));
    assert!(invalid.get("error").is_some(), "{invalid}");
    assert_eq!(player(&mut client)["components"]["Transform"]["translation"], json!([2.0, 0.5, 0.0]));
    let after = client.screenshot(output.join("after-edit.png"));
    assert!(
        before.pixels().zip(after.pixels()).filter(|(a, b)| a != b).count() > 100,
        "component edit should affect the rendered scene"
    );
    let stepped = client.call("game_step", json!({"frames":12,"buttons":["left"]}))["structuredContent"].clone();
    assert_eq!(stepped["frame"], 12);
    let state = player(&mut client);
    let x = state["components"]["Transform"]["translation"][0].as_f64().unwrap();
    let tuning: PlayerTuning = engine::config::load_ron(PlayerTuning::PATH).unwrap();
    let expected = (2.0 - 12.0 * f64::from(tuning.walk_speed) / game::tick_rate(game::REGION).hz)
        .clamp(-f64::from(game::demo::player::ARENA_HALF_SIZE), f64::from(game::demo::player::ARENA_HALF_SIZE));
    assert!((x - expected).abs() < 0.0001, "exactly twelve movement ticks: {state}");
    client.screenshot(output.join("after-step.png"));
    client.call("game_pause", json!({"paused":false}));
    let running = client.request("tools/call",json!({"name":"game_set_component","arguments":{"name":"Player","component":"Transform","fields":{"translation":[99,0.5,0]}}}));
    assert!(running["error"]["message"].as_str().unwrap().contains("pause"), "{running}");
    client.call("game_pause", json!({"paused":true}));
    std::fs::write(output.join("status.json"), serde_json::to_vec_pretty(&status).unwrap()).unwrap();
}
