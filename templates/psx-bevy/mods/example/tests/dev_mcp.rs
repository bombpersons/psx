//! The mod through its real binary: offscreen GPU and the developer MCP,
//! with a software adapter. Checks that the mod's scene renders and the
//! ported controller drives the mod's character.
#![cfg(feature = "dev-mcp")]

use engine::testing::mcp::Client;
use serde_json::json;
use std::process::Command;

#[test]
#[ignore = "requires a working offscreen GPU/software adapter; no display needed"]
fn the_mod_renders_and_its_player_walks() {
    let output = std::env::temp_dir().join(format!("psx-example-mod-{}", std::process::id()));
    std::fs::create_dir_all(&output).unwrap();
    println!("GPU evidence: {}", output.display());
    let mut command = Command::new(env!("CARGO_BIN_EXE_example-mod"));
    command.env("BEVY_ASSET_ROOT", env!("CARGO_MANIFEST_DIR"));
    let mut client = Client::launch(command, &output.join("mod.log"));
    client.call("game_step", json!({"frames":2}));
    let before = client.screenshot(output.join("before.png"));
    let colors: std::collections::HashSet<_> = before.pixels().map(|p| p.0).collect();
    assert!(colors.len() > 16, "the mod's scene should render");
    let x = |client: &mut Client| {
        client.call("game_query", json!({"name":"Player","components":["Transform"]}))["structuredContent"]["entities"]
            [0]["components"]["Transform"]["translation"][0]
            .as_f64()
            .unwrap()
    };
    let start = x(&mut client);
    client.call("game_step", json!({"frames":30,"buttons":["right"]}));
    assert!(x(&mut client) > start + 0.5, "the game's controller should move the mod's player");
    client.screenshot(output.join("after-walk.png"));
}
