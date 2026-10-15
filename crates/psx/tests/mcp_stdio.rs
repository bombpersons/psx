//! `psx mcp` over real stdio with a plain JSON-RPC client: every tool's
//! structured result must equal what the CLI prints for the same arguments.

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;

use serde_json::{json, Value};

const BIN: &str = env!("CARGO_BIN_EXE_psx");

/// The binary, working in its current directory (a PSX_ROOT in the test's environment would override it).
fn psx() -> Command {
    let mut c = Command::new(BIN);
    c.env_remove("PSX_ROOT");
    c
}

struct Rpc {
    child: Child,
    input: Option<ChildStdin>,
    lines: mpsc::Receiver<String>,
    next_id: u64,
    response_timeout: Duration,
}

impl Rpc {
    fn start(root: &Path) -> Self {
        let mut child = psx()
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
        let mut rpc = Rpc { child, input, lines, next_id: 1, response_timeout: Duration::from_secs(60) };
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
            let line = self.lines.recv_timeout(self.response_timeout).expect("response before deadline");
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
    let out = psx().args(args).current_dir(root).output().unwrap();
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
fn next_draw_captures_the_same_frame_through_cli_and_mcp() {
    let root = std::env::temp_dir().join(format!("psx-next-draw-{}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    // Authored BIOS: poll RAM[0], then fill 16x1 pixels with that RGB colour.
    // Pokes choose the drawing frames without copyrighted firmware or media.
    let code: [u32; 16] = [
        0x3c09_1f80, 0x3529_1810, // t1 = GP0
        0x8c08_0000, 0, 0x1100_fffd, 0, // poll RAM[0] (including load/branch delays)
        0xac00_0000, 0x3c0a_0200, 0x0148_5025, // clear flag, t2 = fill opcode | colour
        0xad2a_0000, 0xad20_0000, // fill colour, destination (0,0)
        0x3c0a_0001, 0x354a_0010, 0xad2a_0000, // size 16x1
        0x0bf0_0002, 0, // loop at BIOS + 8
    ];
    let bios: Vec<u8> = code.iter().flat_map(|w| w.to_le_bytes()).collect();
    std::fs::write(root.join("bios.bin"), &bios).unwrap();
    let mut m = psx_core::Machine::new(bios);
    m.bus.frames = 37;
    std::fs::write(root.join("start.bin"), m.save_state(None)).unwrap();
    let mut rpc = Rpc::start(&root);
    let common = ["run", "--bios", "bios.bin", "--backend", "native", "--from-state", "start.bin", "--frames", "4",
        "--poke", "0=255:4@2", "--poke", "0=65280:4@3", "--brief"];
    let mut args = common.to_vec();
    // Read watches select the per-instruction loop; MCP below uses frame batching.
    args.extend(["--gpu-next-draw", "--gpu-search-frames", "3", "--watch-reads", "0:4", "--max-events", "0", "--out", "cli"]);
    let (cli_report, ok) = cli(&root, &args);
    assert!(ok, "{cli_report}");
    let base = json!({"bios":"bios.bin", "backend":"native", "from_state":"start.bin", "frames":4,
        "poke":["0=255:4@2", "0=65280:4@3"], "brief":true, "gpu_next_draw":true, "gpu_search_frames":3, "out":"mcp"});
    let (mcp_report, error, _) = rpc.tool("run", base.clone());
    assert!(!error, "{mcp_report}");
    assert_eq!(cli_report["gpu_frame"], mcp_report["gpu_frame"]);
    let capture = &mcp_report["gpu_frame"];
    assert_eq!(capture["frame"], 2);
    assert_eq!(capture["run_frame"], 2);
    assert_eq!(capture["absolute_frame"], 39);
    assert_eq!(capture["complete"], true);
    assert_eq!(capture["draw_commands"], 1);
    assert_eq!(capture["search"]["frames_inspected"], 3);
    assert_eq!(mcp_report["frames"]["reached"], 4);
    for name in ["gpu_frame.json", "gpu_frame_vram.bin", "gpu_frame_before.png", "gpu_frame_after.png", "gpu_frame_screen.png"] {
        assert_eq!(std::fs::read(root.join("cli").join(name)).unwrap(), std::fs::read(root.join("mcp").join(name)).unwrap(), "{name}");
    }
    let selected = std::fs::read(root.join("mcp/gpu_frame_vram.bin")).unwrap();
    let final_vram = std::fs::read(root.join("mcp/vram.bin")).unwrap();
    assert_eq!(&selected[..2], &31u16.to_le_bytes()); // red, selected frame
    assert_eq!(&final_vram[..2], &992u16.to_le_bytes()); // green, later frame
    let full: Value = serde_json::from_slice(&std::fs::read(root.join("mcp/report.json")).unwrap()).unwrap();
    let decoded: Value = serde_json::from_slice(&std::fs::read(root.join("mcp/gpu_frame.json")).unwrap()).unwrap();
    assert_eq!(full["gpu_frame"], decoded["summary"]);

    // Search exhaustion is explicit; the bounded run still completes normally.
    let mut miss = base.clone();
    miss["gpu_search_frames"] = json!(2);
    miss["out"] = json!("miss");
    let (report, error, _) = rpc.tool("run", miss);
    assert!(!error, "{report}");
    assert_eq!(report["status"], "complete");
    assert_eq!(report["gpu_frame"]["status"], "not_found");
    assert_eq!(report["gpu_frame"]["reason"], "search_limit");
    assert_eq!(report["gpu_frame"]["search"]["frames_inspected"], 2);
    assert!(!root.join("miss/gpu_frame.json").exists());

    // An explicit start is inclusive and the final completed frame is eligible.
    let mut last = base;
    last["gpu_frame"] = json!(3);
    last["gpu_search_frames"] = json!(1);
    last["out"] = json!("last");
    let (report, error, _) = rpc.tool("run", last);
    assert!(!error, "{report}");
    assert_eq!(report["gpu_frame"]["run_frame"], 3);
    assert_eq!(report["gpu_frame"]["absolute_frame"], 40);
    assert_eq!(report["gpu_frame"]["complete"], true);
    drop(rpc);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn mcp_tools_match_cli() {
    let root = scratch();
    std::fs::write(root.join("tiny.ps-exe"), tiny_exe()).unwrap();
    std::fs::write(root.join("blank.mcd"), vec![0u8; 128 * 1024]).unwrap();
    // Two RAM snapshots: a 16.16 value that grows, and the tiny EXE's code resident at 0x80010000.
    let mut ram = vec![0u8; 2 * 1024 * 1024];
    ram[0x1000..0x1004].copy_from_slice(&0x0001_8000u32.to_le_bytes());
    ram[0x10000..0x10008].copy_from_slice(&tiny_exe()[0x800..0x808]);
    std::fs::write(root.join("ram_a.bin"), &ram).unwrap();
    ram[0x1000..0x1004].copy_from_slice(&0x0003_0000u32.to_le_bytes());
    std::fs::write(root.join("ram_b.bin"), &ram).unwrap();
    let stream = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tools/probes/gpu/gp0_stream.bin");
    let stream = stream.to_str().unwrap();

    let mut rpc = Rpc::start(&root);

    let list = rpc.request("tools/list", json!({}));
    let tools = list["result"]["tools"].as_array().unwrap();
    let names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
    for want in [
        "disc",
        "ls",
        "extract",
        "exe",
        "sector",
        "addr",
        "xa",
        "memcard",
        "textures",
        "run",
        "loads",
        "symbols",
        "disasm",
        "lockstep",
        "native",
        "template_create",
        "gpu_replay",
        "vram_diff",
        "xrefs",
        "ram_diff",
        "ram_search",
        "ram_view",
        "ghidra_doctor",
        "ghidra_analyze",
        "ghidra_decompile",
        "ghidra_functions",
        "ghidra_xrefs",
    ] {
        assert!(names.contains(&want), "missing tool {want}: {names:?}");
    }
    for t in tools {
        assert_eq!(t["inputSchema"]["type"], "object", "{}", t["name"]);
        assert!(t["description"].as_str().unwrap().len() > 80, "{} needs a real description", t["name"]);
    }
    let run = tools.iter().find(|t| t["name"] == "run").unwrap();
    for field in [
        "bios", "frames", "backend", "watch", "trace", "gpu_frame", "memcard1", "pad", "from_state", "return_screen", "sample", "sample_every",
        "poke", "snapshot_every", "save_state_at", "watch_changes_only", "watch_reads", "brief", "gpu_next_draw", "gpu_search_frames",
    ] {
        assert!(run["inputSchema"]["properties"].get(field).is_some(), "run schema lacks {field}");
    }

    // (tool, MCP arguments, equivalent CLI arguments)
    let cases: Vec<(&str, Value, Vec<&str>)> = vec![
        (
            "ghidra_decompile",
            json!({"source":"tiny.ps-exe","addr":"bad-address"}),
            vec!["ghidra", "decompile", "--source", "tiny.ps-exe", "--addr", "bad-address"],
        ),
        (
            "ghidra_decompile",
            json!({"source":"tiny.ps-exe","addr":"80010000","ghidra_home":"no-ghidra"}),
            vec!["ghidra", "decompile", "--source", "tiny.ps-exe", "--addr", "80010000", "--ghidra-home", "no-ghidra"],
        ),
        ("ghidra_doctor", json!({"ghidra_home":"no-ghidra"}), vec!["ghidra", "doctor", "--ghidra-home", "no-ghidra"]),
        (
            "ghidra_functions",
            json!({"source":"tiny.ps-exe","limit":0}),
            vec!["ghidra", "functions", "--source", "tiny.ps-exe", "--limit", "0"],
        ),
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
        (
            "run", json!({"bios":"b.bin", "gpu_next_draw":true}),
            vec!["run", "--bios", "b.bin", "--gpu-next-draw"],
        ),
        (
            "run", json!({"bios":"b.bin", "backend":"native", "gpu_search_frames":2}),
            vec!["run", "--bios", "b.bin", "--backend", "native", "--gpu-search-frames", "2"],
        ),
        (
            "run", json!({"bios":"b.bin", "backend":"native", "gpu_next_draw":true, "gpu_search_frames":0}),
            vec!["run", "--bios", "b.bin", "--backend", "native", "--gpu-next-draw", "--gpu-search-frames", "0"],
        ),
        (
            "run", json!({"bios":"b.bin", "backend":"native", "gpu_next_draw":true, "gpu_search_frames":601}),
            vec!["run", "--bios", "b.bin", "--backend", "native", "--gpu-next-draw", "--gpu-search-frames", "601"],
        ),
        (
            "run", json!({"bios":"b.bin", "backend":"native", "gpu_next_draw":true, "gpu_frame":u64::MAX}),
            vec!["run", "--bios", "b.bin", "--backend", "native", "--gpu-next-draw", "--gpu-frame", "18446744073709551615"],
        ),
        (
            "run",
            json!({"bios": "no-such-bios.bin", "backend": "redux", "sample": ["80010000:u32"]}),
            vec!["run", "--bios", "no-such-bios.bin", "--sample", "80010000:u32"],
        ),
        ("disasm", json!({"source": "tiny.ps-exe", "function": "80010004"}), vec!["disasm", "tiny.ps-exe", "--function", "80010004"]),
        (
            "disasm",
            json!({"ram": "ram_a.bin", "addr": "80010000", "count": 2}),
            vec!["disasm", "--ram", "ram_a.bin", "--addr", "80010000", "--count", "2"],
        ),
        ("xrefs", json!({"target": "80010004", "source": "tiny.ps-exe"}), vec!["xrefs", "80010004", "--source", "tiny.ps-exe"]),
        ("xrefs", json!({"target": "zz", "source": "tiny.ps-exe"}), vec!["xrefs", "zz", "--source", "tiny.ps-exe"]),
        ("ram_diff", json!({"a": "ram_a.bin", "b": "ram_b.bin", "signed": true}), vec!["ram", "diff", "ram_a.bin", "ram_b.bin", "--signed"]),
        (
            "ram_search",
            json!({"snapshots": ["ram_a.bin", "ram_b.bin"], "width": 4, "pred": ["increased"]}),
            vec!["ram", "search", "ram_a.bin", "ram_b.bin", "--width", "4", "--pred", "increased"],
        ),
        ("ram_view", json!({"snapshots": ["ram_a.bin", "ram_b.bin"], "addr": "80001000", "len": 8, "as": "fx16.16"}), vec!["ram", "view", "ram_a.bin", "ram_b.bin", "--addr", "80001000", "--len", "8", "--as", "fx16.16"]),
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

    // Results that tests elsewhere rely on.
    let (v, _, _) = rpc.tool("ram_search", json!({"snapshots": ["ram_a.bin", "ram_b.bin"], "width": 4, "pred": ["increased"]}));
    assert_eq!(v["candidates"], 1, "{v}");
    assert_eq!(v["matches"][0]["fx16_16"], json!([1.5, 3.0]), "{v}");
    let (v, _, _) = rpc.tool("disasm", json!({"source": "tiny.ps-exe", "function": "80010004"}));
    assert_eq!((v["function"]["start"].as_str(), v["function"]["end"].as_str()), (Some("0x80010000"), Some("0x8001000C")), "{v}");

    // Text mode: the CLI prints the `text` field as is; MCP returns the whole JSON.
    let (v, is_error, _) = rpc.tool("disasm", json!({"source": "tiny.ps-exe", "count": 2, "text": true}));
    assert!(!is_error, "{v}");
    let out = psx().args(["disasm", "tiny.ps-exe", "--count", "2", "--text"]).current_dir(&root).output().unwrap();
    let text = String::from_utf8(out.stdout).unwrap();
    assert_eq!(text.trim_end(), v["text"].as_str().unwrap().trim_end());
    assert!(text.contains("80010000  24080001  li      t0, 1"), "{text}");

    // --root: the same command from another directory.
    let elsewhere = std::env::temp_dir();
    let out = psx().args(["--root", root.to_str().unwrap(), "exe", "tiny.ps-exe"]).current_dir(&elsewhere).output().unwrap();
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v, cli(&root, &["exe", "tiny.ps-exe"]).0);
    let out = psx().args(["exe", "tiny.ps-exe"]).env("PSX_ROOT", &root).current_dir(&elsewhere).output().unwrap();
    assert!(out.status.success(), "PSX_ROOT");

    // MCP-only argument checks.
    let (v, is_error, _) = rpc.tool("addr", json!({"address": "0", "adress": "1"}));
    assert!(is_error);
    assert_eq!(v["error"]["code"], "invalid_arguments", "{v}");
    let (v, is_error, _) = rpc.tool("addr", json!({"address": "0", "return_screen": true}));
    assert!(is_error, "{v}");
    let (v, is_error, _) = rpc.tool("addr", json!({}));
    assert!(is_error && v["error"]["message"].as_str().unwrap().contains("address"), "{v}");
    let (v, is_error, _) = rpc.tool("ghidra_decompile", json!({"addr":"80010000","source":"tiny.ps-exe","analysis":"analysis.json"}));
    assert!(is_error && v["error"]["code"] == "invalid_arguments", "{v}");

    let unknown = rpc.request("tools/call", json!({"name": "no_such_tool", "arguments": {}}));
    assert!(unknown.get("error").is_some(), "{unknown}");

    drop(rpc);
    let _ = std::fs::remove_dir_all(&root);
}

/// Authored MIPS, with a call delay slot, GP-relative data, BSS and COP2.
/// No original game bytes are needed for this integration test.
fn ghidra_probe() -> Vec<u8> {
    let mut b = tiny_exe();
    b[0x14..0x18].copy_from_slice(&0x8001_00C0u32.to_le_bytes());
    b[0x28..0x2C].copy_from_slice(&0x8001_1000u32.to_le_bytes());
    b[0x2C..0x30].copy_from_slice(&256u32.to_le_bytes());
    let routines: &[(usize, &[u32])] = &[
        (0, &[0x27BD_FFE8, 0xAFBF_0014, 0x0C00_4010, 0x2404_0007, 0x0C00_4020, 0, 0x8FBF_0014, 0, 0x03E0_0008, 0x27BD_0018]),
        (0x40, &[0x8F82_0000, 0, 0x2442_0003, 0x03E0_0008, 0x0044_1021]),
        (0x80, &[0x4802_0000, 0, 0x03E0_0008, 0]),
        (0xC0, &[11]),
    ];
    for (offset, words) in routines {
        for (i, word) in words.iter().enumerate() {
            b[0x800 + offset + i * 4..0x804 + offset + i * 4].copy_from_slice(&word.to_le_bytes());
        }
    }
    b
}

#[cfg(target_os = "linux")]
#[test]
fn ghidra_mcp_cancellation_stops_worker() {
    use std::os::unix::fs::PermissionsExt;
    let root = std::env::temp_dir().join(format!("psx-ghidra-cancel-{}", std::process::id()));
    let install = root.join("fake-ghidra");
    std::fs::create_dir_all(install.join("support")).unwrap();
    std::fs::create_dir_all(install.join("Ghidra")).unwrap();
    std::fs::write(install.join("Ghidra/application.properties"), "application.version=test\n").unwrap();
    let launcher = install.join("support/analyzeHeadless");
    std::fs::write(&launcher, "#!/bin/sh\nsleep 60 &\necho $! > \"$(dirname \"$0\")/child.pid\"\nwait\n").unwrap();
    std::fs::set_permissions(&launcher, std::fs::Permissions::from_mode(0o755)).unwrap();
    std::fs::write(root.join("probe.ps-exe"), ghidra_probe()).unwrap();
    let mut rpc = Rpc::start(&root);
    let id = rpc.next_id;
    rpc.next_id += 1;
    rpc.send(json!({"jsonrpc":"2.0","id":id,"method":"tools/call","params":{"name":"ghidra_decompile","arguments":{"source":"probe.ps-exe","addr":"80010048","ghidra_home":install}}}));
    let pid_file = install.join("support/child.pid");
    for _ in 0..300 {
        if pid_file.exists() {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(pid_file.exists(), "fake worker should start");
    rpc.send(json!({"jsonrpc":"2.0","method":"notifications/cancelled","params":{"requestId":id,"reason":"test cancellation"}}));
    let pid = std::fs::read_to_string(pid_file).unwrap();
    let running = || std::fs::read_to_string(format!("/proc/{}/stat", pid.trim())).is_ok_and(|s| s.split_whitespace().nth(2) != Some("Z"));
    for _ in 0..300 {
        if !running() {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(!running(), "cancelled MCP worker descendant must stop");
    drop(rpc);
    assert!(
        !std::fs::read_dir(root.join(".psx/ghidra")).unwrap().flatten().any(|entry| entry.path().join("analysis.json").exists()),
        "interrupted analyses must not be cached"
    );
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
#[ignore = "requires installed Ghidra/Java; no game media needed"]
fn ghidra_single_call_and_cached_cli_mcp_parity() {
    let root = std::env::temp_dir().join(format!("psx ghidra integration {}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("probe.ps-exe"), ghidra_probe()).unwrap();
    let mut rpc = Rpc::start(&root);
    rpc.response_timeout = Duration::from_secs(310);
    // Start cold through real MCP, rather than preparing a project in the test.
    let (first, error, _) = rpc.tool("ghidra_decompile", json!({"source":"probe.ps-exe","addr":"80010048"}));
    assert!(!error, "{first}");
    assert_eq!(first["status"], "complete", "{first}");
    assert_eq!(first["function"]["entry"], "0x80010040", "{first}");
    let c = first["pseudocode"].as_str().unwrap();
    assert!(c.contains("800100c0") && c.contains("3") && c.contains("param_1"), "{c}");
    assert_eq!(first["callers"]["items"][0]["from"], "0x80010008", "{first}");
    let manifest = first["analysis"].as_str().unwrap();
    let artifact = first["artifact"].as_str().unwrap();
    let before = std::fs::metadata(artifact).unwrap().modified().unwrap();
    let (second, ok) = cli(&root, &["ghidra", "decompile", "--source", "probe.ps-exe", "--addr", "80010048"]);
    assert!(ok, "{second}");
    assert_eq!(first, second, "cached CLI/MCP result");
    assert_eq!(before, std::fs::metadata(artifact).unwrap().modified().unwrap(), "cache must not re-decompile");
    let (functions, error, _) = rpc.tool("ghidra_functions", json!({"analysis":manifest,"limit":1}));
    assert!(!error && functions["functions"]["total"] == 3, "{functions}");
    assert_eq!(functions["functions"]["next_offset"], 1);
    let (refs, error, _) = rpc.tool("ghidra_xrefs", json!({"analysis":manifest,"addr":"800100c0"}));
    assert!(!error && refs["references_to"]["items"][0]["from"] == "0x80010040", "{refs}");
    // The next function requires processing the saved program in a new JVM.
    let (cop2, error, _) = rpc.tool("ghidra_decompile", json!({"analysis":manifest,"addr":"A0010080"}));
    assert!(!error && cop2["function"]["entry"] == "0x80010080", "{cop2}");
    assert!(cop2["warnings"].to_string().contains("COP2"), "{cop2}");
    let (again, error, _) = rpc.tool("ghidra_decompile", json!({"analysis":manifest,"addr":"80010048"}));
    assert!(!error && again["pseudocode"] == first["pseudocode"], "{again}");
    let (missing, error, _) = rpc.tool("ghidra_decompile", json!({"analysis":manifest,"addr":"800100c0"}));
    assert!(!error && missing["status"] == "partial" && missing["decompile_status"] == "function_not_found", "{missing}");
    assert_eq!(missing["analysis"], manifest);
    let (exe, ok) = cli(&root, &["exe", "probe.ps-exe"]);
    assert!(ok);
    std::fs::write(root.join("probe.symbols.json"),serde_json::to_vec(&json!({
        "source":root.join("probe.ps-exe"),"exe_sha256":exe["sha256"],
        "report":{"symbols":[{"address":0x80010040u32,"name":"measured_helper","kind":"function"},{"address":0x80010048u32,"name":"inside_helper","kind":"local"}]}
    })).unwrap()).unwrap();
    let (named, error, _) =
        rpc.tool("ghidra_decompile", json!({"source":"probe.ps-exe","addr":"80010048","symbols":["probe.symbols.json"]}));
    assert!(!error && named["function"]["name"] == "measured_helper", "{named}");
    // Respect both our project lock and externally modified Ghidra databases.
    let lock = Path::new(manifest).parent().unwrap().join("psx.lock");
    std::fs::write(&lock, "other owner").unwrap();
    let (busy, error, _) = rpc.tool("ghidra_functions", json!({"analysis":manifest}));
    assert!(error && busy["error"]["code"] == "analysis_busy", "{busy}");
    std::fs::remove_file(lock).unwrap();
    let project = Path::new(first["project"].as_str().unwrap()).parent().unwrap();
    std::fs::write(project.join("external-edit"), "changed by GUI").unwrap();
    let (stale, error, _) = rpc.tool("ghidra_functions", json!({"analysis":manifest}));
    assert!(error && stale["error"]["code"] == "stale_analysis", "{stale}");
    let (fresh, error, _) = rpc.tool("ghidra_decompile", json!({"source":"probe.ps-exe","addr":"80010048","reanalyze":true}));
    assert!(!error && fresh["status"] == "complete" && fresh["project"] != first["project"], "{fresh}");
    assert!(project.join("external-edit").exists(), "reanalyze must preserve edited projects");
    drop(rpc);
    std::fs::remove_dir_all(root).unwrap();
}
