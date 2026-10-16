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
        Self::launch(Command::new(env!(concat!("CARGO_BIN_EXE_", env!("CARGO_PKG_NAME")))), log)
    }
    fn launch(mut command: Command, log: &std::path::Path) -> Self {
        let mut child = command
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

#[test]
#[ignore = "requires offscreen GPU and cargo build --features dev-mcp --example depth_cue"]
fn reciprocal_depth_cue_rendering() {
    let output = std::env::temp_dir().join(format!("psx-depth-cue-{}", std::process::id()));
    std::fs::create_dir_all(&output).unwrap();
    println!("Depth cue GPU evidence: {}", output.display());
    // Locate the example beside this test's target/debug/deps directory, so a
    // generated project or custom CARGO_TARGET_DIR uses its own build artifacts.
    let binary = std::env::current_exe().unwrap().parent().unwrap().parent().unwrap().join("examples/depth_cue");
    assert!(binary.exists(), "build the depth_cue example with --features dev-mcp first");
    let mut client = Client::launch(Command::new(binary), &output.join("game.log"));
    let edit = |client: &mut Client, fields: Value| {
        client.call(
            "game_set_component",
            json!({"name":"DepthCueControls","component":"DepthCueControls","fields":fields}),
        );
    };
    let sample = |image: &image::RgbaImage, x: u32, y: u32| -> [u8; 3] {
        let p = image.get_pixel(x, y);
        [p[0], p[1], p[2]]
    };
    let close = |actual: [u8; 3], expected: [u8; 3]| {
        assert!(actual.iter().zip(expected).all(|(a, b)| a.abs_diff(b) <= 1), "{actual:?} != {expected:?}");
    };
    // Mesh shaders quantize each channel to five bits, then expand it to 8.
    let encoded = |v: f32| {
        let q = ((v.clamp(0.0, 1.0) * 255.0).round() as u32) / 8;
        (q * 8 + q / 4) as u8
    };
    client.call("game_step", json!({"frames":1}));
    let baseline = client.screenshot(output.join("reciprocal.png"));
    for (x, expected) in [(61, 255), (160, 99), (259, 0)] {
        close(sample(&baseline, x, 60), [expected; 3]);
    }
    // Every point on the slanted wall must interpolate endpoint colours in
    // screen space. Applying a reciprocal to interpolated depth fails here.
    for x in [40, 80, 120, 160, 200, 240, 280] {
        let ndc = (x as f32 + 0.5) / 160.0 - 1.0;
        let t = (ndc + 0.87) / 1.74;
        close(sample(&baseline, x, 180), [encoded(1.0 - t); 3]);
    }
    // Equal-depth panels stay uniform despite different radial distances.
    close(sample(&baseline, 130, 28), [99; 3]);
    close(sample(&baseline, 190, 92), [99; 3]);
    edit(&mut client, json!({"camera_fog":true}));
    let camera_fog = client.screenshot(output.join("cue-with-camera-fog.png"));
    assert_eq!(baseline, camera_fog, "cue overrides DistanceFog instead of being applied twice");
    edit(&mut client, json!({"enabled":false}));
    let legacy = client.screenshot(output.join("ordinary-fog.png"));
    for x in [61, 160, 259] {
        close(sample(&legacy, x, 60), [0; 3]);
    }
    edit(&mut client, json!({"fog":false}));
    let disabled = client.screenshot(output.join("disabled.png"));
    for x in [61, 160, 259] {
        close(sample(&disabled, x, 60), [255; 3]);
    }
    edit(&mut client, json!({"enabled":true,"surface_color":[1,0,0],"far_color":[0,0,1]}));
    let colored = client.screenshot(output.join("colored.png"));
    close(sample(&colored, 160, 60), [99, 0, 156]);
    edit(&mut client, json!({"surface_color":[1,1,1],"far_color":[1,0,0],"textured":true}));
    let textured = client.screenshot(output.join("textured.png"));
    close(sample(&textured, 160, 60), [132, 49, 49]);
    edit(&mut client, json!({"far_color":[0,0,0],"textured":false,"orthographic":true}));
    let ortho = client.screenshot(output.join("orthographic.png"));
    for (x, expected) in [(61, 255), (160, 99), (259, 0)] {
        close(sample(&ortho, x, 60), [expected; 3]);
    }
    // The whole authored pattern stays the same under this projection change;
    // relying on clip.w instead of camera depth would darken every panel alike.
    assert_eq!(baseline, ortho);
    // An actual FDAT[48] lighting-preset setting from King's Field PAL:
    // 16000 PS1 units / 1024 scene units. Its default camera H=200 gives
    // DQA=-12800, DQB=0x01400000; the renderer uses the equivalent float curve.
    edit(&mut client, json!({"scale":-0.625*16000.0/1024.0,"orthographic":false}));
    let kings_field = client.screenshot(output.join("kings-field-setting.png"));
    close(sample(&kings_field, 61, 60), [57; 3]);
    for x in [160, 259] {
        close(sample(&kings_field, x, 60), [0; 3]);
    }
    let log = std::fs::read_to_string(output.join("game.log")).unwrap();
    assert!(!log.contains("Shader compilation error") && !log.contains("Error matching ShaderStages"), "{log}");
}
impl Drop for Client {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[test]
#[ignore = "requires a working offscreen GPU/software adapter; no display needed"]
fn controls_menu_renders_and_rejects_gameplay_steps_while_capturing() {
    let output = std::env::temp_dir().join(format!("psx-controls-gpu-{}", std::process::id()));
    std::fs::create_dir_all(&output).unwrap();
    println!("Controls GPU evidence: {}", output.display());
    let path = output.join("controls.json");
    let mut command = Command::new(env!(concat!("CARGO_BIN_EXE_", env!("CARGO_PKG_NAME"))));
    command.arg("--controls").arg("--bindings").arg(&path);
    let mut client = Client::launch(command, &output.join("game.log"));
    let menu = client.screenshot(output.join("controls.png"));
    let light = menu.pixels().filter(|p| p[0] > 150 && p[1] > 150).count();
    assert!(light > 300, "menu labels should be visible, not a blank blue target");
    assert!(
        (0..menu.width()).all(|x| {
            let p = menu.get_pixel(x, menu.height() - 1);
            p[0] < 120 && p[1] > 150 && p[2] > 200
        }),
        "the bottom border must fit the screen; overflowing menu content used to clip the help text"
    );
    let step = client.request("tools/call", json!({"name":"game_step","arguments":{"frames":1,"buttons":["cross"]}}));
    assert!(step["error"]["message"].as_str().unwrap().contains("Controls menu"), "{step}");
    let input = client.request("tools/call", json!({"name":"game_set_input","arguments":{"buttons":["cross"]}}));
    assert!(input["error"]["message"].as_str().unwrap().contains("Controls menu"), "{input}");
    let status = client.call("game_status", json!({}))["structuredContent"].clone();
    assert_eq!(status["frame"], 0);
    assert_eq!(status["controller_buttons"], json!([]));
    assert!(!path.exists(), "opening a menu must not create or replace settings");
    let log = std::fs::read_to_string(output.join("game.log")).unwrap();
    assert!(!log.contains("Shader compilation error") && !log.contains("B0001"), "{log}");
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
