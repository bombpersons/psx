//! Real offscreen GPU and stdio MCP verification, requiring a software adapter.
#![cfg(feature = "dev-mcp")]

use base64::Engine;
use serde_json::{Value, json};
use std::{
    io::{BufRead, BufReader, Write},
    path::PathBuf,
    process::{Child, ChildStdin, Command, Stdio},
    sync::mpsc,
    time::Duration,
};

struct Client {
    child: Child,
    input: ChildStdin,
    output: mpsc::Receiver<String>,
    id: u64,
}
impl Client {
    fn start(log: &std::path::Path) -> Self {
        let mut child = Command::new(env!(concat!("CARGO_BIN_EXE_", env!("CARGO_PKG_NAME"))))
            .args(["--mcp", "--headless", "--software"])
            .env_remove("DISPLAY")
            .env_remove("WAYLAND_DISPLAY")
            .env("BEVY_ASSET_ROOT", env!("CARGO_MANIFEST_DIR"))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(std::fs::File::create(log).unwrap())
            .spawn()
            .unwrap();
        let input = child.stdin.take().unwrap();
        let stdout = child.stdout.take().unwrap();
        let (send, output) = mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                if send.send(line.unwrap()).is_err() {
                    break;
                }
            }
        });
        let mut client = Self { child, input, output, id: 0 };
        let init = client.request("initialize", json!({"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"template-headless-test","version":"1"}}));
        assert!(init.get("result").is_some(), "{init}");
        client.send(json!({"jsonrpc":"2.0","method":"notifications/initialized"}));
        client
    }
    fn send(&mut self, value: Value) {
        writeln!(self.input, "{value}").unwrap();
        self.input.flush().unwrap();
    }
    fn request(&mut self, method: &str, params: Value) -> Value {
        self.id += 1;
        self.send(json!({"jsonrpc":"2.0","id":self.id,"method":method,"params":params}));
        loop {
            let line = self
                .output
                .recv_timeout(Duration::from_secs(120))
                .expect("game reply before deadline; inspect game.log");
            let message: Value = serde_json::from_str(&line).expect("stdout must contain JSON-RPC only");
            if message["id"] == self.id {
                return message;
            }
        }
    }
    fn call(&mut self, name: &str, args: Value) -> Value {
        let message = self.request("tools/call", json!({"name":name,"arguments":args}));
        assert!(message.get("error").is_none(), "{message}");
        assert_ne!(message["result"]["isError"], true, "{message}");
        message["result"].clone()
    }
    fn state(&mut self) -> Value {
        self.call("game_query", json!({"name":"Player","components":["Transform"]}))["structuredContent"]["entities"][0]
            .clone()
    }
    fn screenshot(&mut self, path: PathBuf) -> image::RgbaImage {
        let reply = self.call("game_screenshot", json!({"target":"screen"}));
        let encoded = reply["content"].as_array().unwrap().iter().find(|v| v["type"] == "image").unwrap()["data"]
            .as_str()
            .unwrap();
        let bytes = base64::engine::general_purpose::STANDARD.decode(encoded).unwrap();
        std::fs::write(path, &bytes).unwrap();
        let image = image::load_from_memory(&bytes).unwrap().to_rgba8();
        assert_eq!(image.dimensions(), (320, 240));
        image
    }
}
impl Drop for Client {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[test]
#[ignore = "requires a working offscreen GPU/software adapter; no display needed"]
fn headless_render_edit_step_and_query() {
    let output = std::env::temp_dir().join(format!("psx-template-headless-{}", std::process::id()));
    std::fs::create_dir_all(&output).unwrap();
    println!("GPU evidence: {}", output.display());
    let mut client = Client::start(&output.join("game.log"));
    let tools = client.request("tools/list", json!({}));
    assert!(tools["result"]["tools"].as_array().unwrap().iter().any(|t| t["name"] == "game_set_component"));
    let status = client.call("game_status", json!({}))["structuredContent"].clone();
    assert_eq!(status["paused"], true);
    assert_eq!(status["headless"], true);
    assert_eq!(status["frame"], 0);
    let window = client.request("tools/call", json!({"name":"game_screenshot","arguments":{"target":"window"}}));
    assert!(window["error"]["message"].as_str().unwrap().contains("No primary window"), "{window}");
    let before = client.screenshot(output.join("before.png"));
    let colors: std::collections::HashSet<_> = before.pixels().map(|p| p.0).collect();
    assert!(colors.len() > 32, "offscreen frame should render the scene, not an empty target");
    let state = client.state();
    assert_eq!(state["components"]["Transform"]["translation"], json!([0.0, 0.5, 0.0]));
    let edit = client.call(
        "game_set_component",
        json!({"entity":state["entity"],"component":"Transform","fields":{"translation":[2.0,0.5,0.0]}}),
    )["structuredContent"]
        .clone();
    assert_eq!(edit["frame"], 0);
    assert_eq!(edit["after"]["translation"], json!([2.0, 0.5, 0.0]));
    assert_eq!(client.state()["components"]["Transform"]["translation"], json!([2.0, 0.5, 0.0]));
    let invalid = client.request("tools/call",json!({"name":"game_set_component","arguments":{"name":"Player","component":"Transform","fields":{"translation":[99,0.5,0],"zzz":1}}}));
    assert!(invalid.get("error").is_some(), "{invalid}");
    assert_eq!(client.state()["components"]["Transform"]["translation"], json!([2.0, 0.5, 0.0]));
    let after = client.screenshot(output.join("after-edit.png"));
    assert!(
        before.pixels().zip(after.pixels()).filter(|(a, b)| a != b).count() > 100,
        "component edit should affect the rendered scene"
    );
    let stepped = client.call("game_step", json!({"frames":12,"buttons":["left"]}))["structuredContent"].clone();
    assert_eq!(stepped["frame"], 12);
    let state = client.state();
    let x = state["components"]["Transform"]["translation"][0].as_f64().unwrap();
    let expected = (2.0 - 12.0 * f64::from(game::demo::player::WALK_SPEED) / game::tick_rate(game::REGION).hz)
        .clamp(-f64::from(game::demo::player::ARENA_HALF_SIZE), f64::from(game::demo::player::ARENA_HALF_SIZE));
    assert!((x - expected).abs() < 0.0001, "exactly twelve movement ticks: {state}");
    client.screenshot(output.join("after-step.png"));
    client.call("game_pause", json!({"paused":false}));
    let running = client.request("tools/call",json!({"name":"game_set_component","arguments":{"name":"Player","component":"Transform","fields":{"translation":[99,0.5,0]}}}));
    assert!(running["error"]["message"].as_str().unwrap().contains("pause"), "{running}");
    client.call("game_pause", json!({"paused":true}));
    std::fs::write(output.join("status.json"), serde_json::to_vec_pretty(&status).unwrap()).unwrap();
}
