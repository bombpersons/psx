//! A stdio client for the developer MCP, for GPU tests that drive a game
//! binary: `--mcp --headless --software` with no display. Set
//! `BEVY_ASSET_ROOT` on the command when the binary is not run by cargo.
//!
//! ```ignore
//! # use psx_bevy::testing::mcp::Client;
//! # use serde_json::json;
//! let mut command = std::process::Command::new(env!("CARGO_BIN_EXE_my-game"));
//! command.env("BEVY_ASSET_ROOT", env!("CARGO_MANIFEST_DIR"));
//! let mut client = Client::launch(command, std::path::Path::new("game.log"));
//! client.call("game_step", json!({"frames": 12, "buttons": ["left"]}));
//! ```
use base64::Engine;
use serde_json::{Value, json};
use std::{
    io::{BufRead, BufReader, Write},
    path::PathBuf,
    process::{Child, ChildStdin, Command, Stdio},
    sync::mpsc,
    time::Duration,
};

pub struct Client {
    pub child: Child,
    input: ChildStdin,
    output: mpsc::Receiver<String>,
    id: u64,
}
impl Client {
    pub fn launch(mut command: Command, log: &std::path::Path) -> Self {
        // An empty settings folder beside the log, so a player's saved
        // controls.json or display.json cannot change what the test sees.
        let config = log.with_extension("config");
        let _ = std::fs::remove_dir_all(&config);
        std::fs::create_dir_all(&config).unwrap();
        let mut child = command
            .args(["--mcp", "--headless", "--software"])
            .env_remove("DISPLAY")
            .env_remove("WAYLAND_DISPLAY")
            .env("XDG_CONFIG_HOME", &config)
            .env("APPDATA", &config)
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
    pub fn send(&mut self, value: Value) {
        writeln!(self.input, "{value}").unwrap();
        self.input.flush().unwrap();
    }
    pub fn request(&mut self, method: &str, params: Value) -> Value {
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
    pub fn call(&mut self, name: &str, args: Value) -> Value {
        let message = self.request("tools/call", json!({"name":name,"arguments":args}));
        assert!(message.get("error").is_none(), "{message}");
        assert_ne!(message["result"]["isError"], true, "{message}");
        message["result"].clone()
    }
    pub fn screenshot(&mut self, path: PathBuf) -> image::RgbaImage {
        let reply = self.call("game_screenshot", json!({"target":"screen"}));
        let encoded = reply["content"].as_array().unwrap().iter().find(|v| v["type"] == "image").unwrap()["data"]
            .as_str()
            .unwrap();
        let bytes = base64::engine::general_purpose::STANDARD.decode(encoded).unwrap();
        std::fs::write(path, &bytes).unwrap();
        image::load_from_memory(&bytes).unwrap().to_rgba8()
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
