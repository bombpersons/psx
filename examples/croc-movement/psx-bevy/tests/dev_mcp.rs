//! Real offscreen GPU and stdio MCP verification, requiring a software adapter.
//! The tests drive the `depth_cue` and `depth_bias` examples, which use the
//! same launcher as a game: build them first with
//! `cargo build -p psx-bevy --features dev-mcp --examples`.
#![cfg(feature = "dev-mcp")]

use psx_bevy::testing::mcp::Client;
use serde_json::{Value, json};
use std::{path::PathBuf, process::Command};

/// An example beside this test's target/debug/deps directory, so a
/// generated project or custom CARGO_TARGET_DIR uses its own build artifacts.
fn example(name: &str) -> PathBuf {
    let binary = std::env::current_exe().unwrap().parent().unwrap().parent().unwrap().join("examples").join(name);
    assert!(binary.exists(), "build the {name} example with --features dev-mcp first");
    binary
}

fn depth_cue_example() -> PathBuf {
    example("depth_cue")
}

#[test]
#[ignore = "requires offscreen GPU and cargo build --features dev-mcp --example depth_bias"]
fn depth_bias_loses_to_coplanar_copy_on_a_custom_screen() {
    let output = std::env::temp_dir().join(format!("psx-depth-bias-{}", std::process::id()));
    std::fs::create_dir_all(&output).unwrap();
    println!("Depth bias GPU evidence: {}", output.display());
    let mut client = Client::launch(Command::new(example("depth_bias")), &output.join("game.log"));
    // Every pixel along the slanted wall, near (left) to far (right), shows
    // the unbiased copy, whichever copy has the bias.
    let winner = |image: &image::RgbaImage| -> Vec<[u8; 3]> {
        (24..344).step_by(8).map(|x| [0, 1, 2].map(|c| image.get_pixel(x, 120)[c])).collect()
    };
    client.call("game_step", json!({"frames":1}));
    let red_behind = client.screenshot(output.join("red-behind.png"));
    assert_eq!(red_behind.dimensions(), (368, 240), "Game::screen sizes the screen target");
    assert!(winner(&red_behind).iter().all(|p| *p == [0, 255, 0]), "{:?}", winner(&red_behind));
    client.call(
        "game_set_component",
        json!({"name":"DepthBiasControls","component":"DepthBiasControls","fields":{"red_behind":false}}),
    );
    let green_behind = client.screenshot(output.join("green-behind.png"));
    assert!(winner(&green_behind).iter().all(|p| *p == [255, 0, 0]), "{:?}", winner(&green_behind));
    let log = std::fs::read_to_string(output.join("game.log")).unwrap();
    assert!(!log.contains("Shader compilation error") && !log.contains("B0001"), "{log}");
}

#[test]
#[ignore = "requires offscreen GPU and cargo build --features dev-mcp --example depth_cue"]
fn reciprocal_depth_cue_rendering() {
    let output = std::env::temp_dir().join(format!("psx-depth-cue-{}", std::process::id()));
    std::fs::create_dir_all(&output).unwrap();
    println!("Depth cue GPU evidence: {}", output.display());
    let mut client = Client::launch(Command::new(depth_cue_example()), &output.join("game.log"));
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
    assert_eq!(baseline.dimensions(), (320, 240));
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
#[test]
#[ignore = "requires offscreen GPU and cargo build --features dev-mcp --example depth_cue"]
fn controls_menu_renders_and_rejects_gameplay_steps_while_capturing() {
    let output = std::env::temp_dir().join(format!("psx-controls-gpu-{}", std::process::id()));
    std::fs::create_dir_all(&output).unwrap();
    println!("Controls GPU evidence: {}", output.display());
    let path = output.join("controls.json");
    let mut command = Command::new(depth_cue_example());
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
