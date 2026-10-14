//! `psx mcp` over real stdio with a plain JSON-RPC client: every tool's
//! structured result must equal what the CLI prints for the same arguments.

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;

use serde_json::{json, Value};

const BIN: &str = env!("CARGO_BIN_EXE_psx");

struct Rpc {
    child: Child,
    input: Option<ChildStdin>,
    lines: mpsc::Receiver<String>,
    next_id: u64,
}

impl Rpc {
    fn start(root: &Path) -> Self {
        let mut child = Command::new(BIN)
            .arg("mcp")
            .current_dir(root)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let input = child.stdin.take();
        let out = child.stdout.take().unwrap();
        let (tx, lines) = mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(out).lines() {
                let Ok(line) = line else { break };
                if tx.send(line).is_err() {
                    break;
                }
            }
        });
        let mut rpc = Rpc { child, input, lines, next_id: 1 };
        let init = rpc.request(
            "initialize",
            json!({"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "psx-test", "version": "1"}}),
        );
        assert_eq!(init["result"]["serverInfo"]["name"], "psx", "{init}");
        rpc.send(json!({"jsonrpc": "2.0", "method": "notifications/initialized"}));
        rpc
    }

    fn send(&mut self, v: Value) {
        let input = self.input.as_mut().unwrap();
        writeln!(input, "{v}").unwrap();
        input.flush().unwrap();
    }

    fn request(&mut self, method: &str, params: Value) -> Value {
        let id = self.next_id;
        self.next_id += 1;
        self.send(json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}));
        loop {
            let line = self.lines.recv_timeout(Duration::from_secs(60)).expect("response within 60 s");
            // Everything on stdout must be protocol.
            let v: Value = serde_json::from_str(&line).unwrap_or_else(|e| panic!("non-JSON stdout {line:?}: {e}"));
            if v["id"] == id {
                return v;
            }
        }
    }

    /// Call a tool; returns (structured result, isError, content blocks).
    fn tool(&mut self, name: &str, args: Value) -> (Value, bool, Vec<Value>) {
        let r = self.request("tools/call", json!({"name": name, "arguments": args}));
        assert!(r.get("error").is_none(), "protocol error: {r}");
        let result = &r["result"];
        let structured = result["structuredContent"].clone();
        let content = result["content"].as_array().cloned().unwrap_or_default();
        let text: Value = serde_json::from_str(content[0]["text"].as_str().unwrap()).unwrap();
        assert_eq!(text, structured, "text content mirrors structuredContent");
        (structured, result["isError"].as_bool().unwrap_or(false), content)
    }
}

impl Drop for Rpc {
    fn drop(&mut self) {
        self.input.take();
        let _ = self.child.wait();
    }
}

fn cli(root: &Path, args: &[&str]) -> (Value, bool) {
    let out = Command::new(BIN).args(args).current_dir(root).output().unwrap();
    let text = String::from_utf8(out.stdout).unwrap();
    (serde_json::from_str(&text).unwrap_or_else(|e| panic!("CLI {args:?} printed {text:?}: {e}")), out.status.success())
}

fn scratch() -> PathBuf {
    let dir = std::env::temp_dir().join(format!("psx-mcp-test-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// A minimal PS-X EXE: header plus one 2 KiB text page at 0x80010000.
fn tiny_exe() -> Vec<u8> {
    let mut b = vec![0u8; 0x1000];
    b[..8].copy_from_slice(b"PS-X EXE");
    let put = |b: &mut [u8], off: usize, v: u32| b[off..off + 4].copy_from_slice(&v.to_le_bytes());
    put(&mut b, 0x10, 0x8001_0000); // pc
    put(&mut b, 0x18, 0x8001_0000); // text addr
    put(&mut b, 0x1C, 0x800); // text size
    put(&mut b, 0x30, 0x801F_FFF0); // stack
    b[0x4C..0x4C + 12].copy_from_slice(b"Test EXE  NA");
    put(&mut b, 0x800, 0x2408_0001); // addiu t0, zero, 1
    put(&mut b, 0x804, 0x03E0_0008); // jr ra
    b
}

#[test]
fn mcp_tools_match_cli() {
    let root = scratch();
    std::fs::write(root.join("tiny.ps-exe"), tiny_exe()).unwrap();
    std::fs::write(root.join("blank.mcd"), vec![0u8; 128 * 1024]).unwrap();
    let stream = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tools/probes/gpu/gp0_stream.bin");
    let stream = stream.to_str().unwrap();

    let mut rpc = Rpc::start(&root);

    let list = rpc.request("tools/list", json!({}));
    let tools = list["result"]["tools"].as_array().unwrap();
    let names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
    for want in [
        "disc", "ls", "extract", "exe", "sector", "addr", "xa", "memcard", "textures", "run", "loads", "symbols", "disasm",
        "lockstep", "native", "template_create", "gpu_replay", "vram_diff",
    ] {
        assert!(names.contains(&want), "missing tool {want}: {names:?}");
    }
    for t in tools {
        assert_eq!(t["inputSchema"]["type"], "object", "{}", t["name"]);
        assert!(t["description"].as_str().unwrap().len() > 80, "{} needs a real description", t["name"]);
    }
    let run = tools.iter().find(|t| t["name"] == "run").unwrap();
    for field in ["bios", "frames", "backend", "watch", "trace", "gpu_frame", "memcard1", "pad", "from_state", "return_screen"] {
        assert!(run["inputSchema"]["properties"].get(field).is_some(), "run schema lacks {field}");
    }

    // (tool, MCP arguments, equivalent CLI arguments)
    let cases: Vec<(&str, Value, Vec<&str>)> = vec![
        ("addr", json!({"address": "0x80010000"}), vec!["addr", "0x80010000"]),
        ("addr", json!({"address": "1F801810"}), vec!["addr", "1F801810"]),
        ("addr", json!({"address": "zz"}), vec!["addr", "zz"]),
        ("disc", json!({"image": "missing.cue"}), vec!["disc", "missing.cue"]),
        ("ls", json!({"image": "missing.cue"}), vec!["ls", "missing.cue"]),
        ("exe", json!({"source": "tiny.ps-exe"}), vec!["exe", "tiny.ps-exe"]),
        ("disasm", json!({"source": "tiny.ps-exe", "count": 4}), vec!["disasm", "tiny.ps-exe", "--count", "4"]),
        ("memcard", json!({"image": "blank.mcd"}), vec!["memcard", "blank.mcd"]),
        ("gpu_replay", json!({"stream": stream, "out": "vram.bin"}), vec!["gpu-replay", stream, "-o", "vram.bin"]),
        ("vram_diff", json!({"a": "vram.bin", "b": "vram.bin", "samples": 2}), vec!["vram-diff", "vram.bin", "vram.bin", "--samples", "2"]),
        ("native", json!({"bios": "no-such-bios.bin", "frames": 1}), vec!["native", "--bios", "no-such-bios.bin", "--frames", "1"]),
        (
            "run",
            json!({"bios": "no-such-bios.bin", "backend": "native", "frames": 1}),
            vec!["run", "--bios", "no-such-bios.bin", "--backend", "native", "--frames", "1"],
        ),
        ("run", json!({"bios": "b.bin", "backend": "bogus"}), vec!["run", "--bios", "b.bin", "--backend", "bogus"]),
    ];
    for (tool, args, argv) in cases {
        let (mcp, is_error) = {
            let (v, e, _) = rpc.tool(tool, args.clone());
            (v, e)
        };
        let (cli, ok) = cli(&root, &argv);
        assert_eq!(mcp, cli, "{tool} {args}: MCP and CLI differ");
        assert_eq!(is_error, cli.get("error").is_some(), "{tool} {args}: isError");
        if cli.get("error").is_some() {
            assert!(!ok, "{argv:?} should exit non-zero");
        }
    }

    // MCP-only argument checks.
    let (v, is_error, _) = rpc.tool("addr", json!({"address": "0", "adress": "1"}));
    assert!(is_error);
    assert_eq!(v["error"]["code"], "invalid_arguments", "{v}");
    let (v, is_error, _) = rpc.tool("addr", json!({"address": "0", "return_screen": true}));
    assert!(is_error, "{v}");
    let (v, is_error, _) = rpc.tool("addr", json!({}));
    assert!(is_error && v["error"]["message"].as_str().unwrap().contains("address"), "{v}");

    let unknown = rpc.request("tools/call", json!({"name": "no_such_tool", "arguments": {}}));
    assert!(unknown.get("error").is_some(), "{unknown}");

    drop(rpc);
    let _ = std::fs::remove_dir_all(&root);
}
