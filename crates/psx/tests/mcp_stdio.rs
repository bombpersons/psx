//! `psx mcp` over real stdio with a plain JSON-RPC client: every tool's
//! structured result must equal what the CLI prints for the same arguments.

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;

use serde_json::{json, Value};

const BIN: &str = env!("CARGO_BIN_EXE_psx");

#[test]
fn host_asset_directories_preserve_order_siblings_and_cli_mcp_parity() {
    let root = std::env::temp_dir().join(format!("psx-host-assets-{}", std::process::id()));
    std::fs::create_dir_all(root.join("decoded/nested")).unwrap();
    std::fs::create_dir_all(root.join("empty")).unwrap();
    let tim: Vec<u8> = [
        16u32.to_le_bytes().as_slice(), 2u32.to_le_bytes().as_slice(), 20u32.to_le_bytes().as_slice(),
        &[0, 0, 0, 0, 2, 0, 2, 0], &[31, 0, 0, 0, 0, 0, 0, 0],
    ].concat();
    // Create files out of order; a Croc index must still bind its own sibling.
    std::fs::write(root.join("decoded/z.tim"), &tim).unwrap();
    std::fs::write(root.join("decoded/nested/a.tim"), &tim).unwrap();
    std::fs::write(root.join("decoded/a.1"), &tim).unwrap();
    let mut directory = 1u32.to_le_bytes().to_vec();
    let mut name = [0; 12];
    name[..9].copy_from_slice(b"PLATE.TIM");
    directory.extend_from_slice(&name);
    for word in [tim.len() as u32, 0, 0] { directory.extend_from_slice(&word.to_le_bytes()); }
    std::fs::write(root.join("decoded/a.DIR"), directory).unwrap();
    // A split sound bank exercises .VH/.VB lookup after directory expansion.
    let header_size = 32 + 128 * 16 + 16 * 32 + 512;
    let mut vh = vec![0; header_size];
    vh[..4].copy_from_slice(b"pBAV");
    vh[4..8].copy_from_slice(&7u32.to_le_bytes());
    vh[12..16].copy_from_slice(&((header_size + 16) as u32).to_le_bytes());
    vh[18..20].copy_from_slice(&1u16.to_le_bytes());
    vh[22..24].copy_from_slice(&1u16.to_le_bytes());
    vh[header_size - 510..header_size - 508].copy_from_slice(&2u16.to_le_bytes());
    let mut vb = vec![0x11; 16];
    vb[..2].copy_from_slice(&[0, 1]);
    std::fs::write(root.join("decoded/sound.VH"), vh).unwrap();
    std::fs::write(root.join("decoded/sound.VB"), vb).unwrap();
    #[cfg(unix)]
    std::os::unix::fs::symlink(&root, root.join("decoded/loop")).unwrap();
    let mut rpc = Rpc::start(&root);
    let (scan, ok) = cli(&root, &["assets", "scan", "decoded/", "--json"]);
    let (mcp, error, _) = rpc.tool("assets_scan", json!({"source":"decoded/", "json":true}));
    assert!(ok && !error, "{scan} {mcp}");
    assert_eq!(scan, mcp);
    assert_eq!(scan["coverage"]["source_files"], 6);
    assert_eq!(scan["coverage"]["files_inspected"], 6);
    assert_eq!(scan["coverage"]["complete"], true);
    assert_eq!(scan["files"].as_array().unwrap().iter().map(|f| f["file"].clone()).collect::<Vec<_>>(),
        vec![json!("decoded/a.1"), json!("decoded/nested/a.tim"), json!("decoded/sound.VH"), json!("decoded/z.tim")]);
    assert_eq!(scan["files"][0]["hits"][0]["entry"]["name"], "PLATE.TIM");
    let (filtered, error, _) = rpc.tool("assets_scan", json!({"source":"decoded/", "filter":"NESTED", "json":true}));
    assert!(!error, "{filtered}");
    assert_eq!(filtered["coverage"]["selected_files"], 1);
    assert_eq!(filtered["totals"]["tim"], 1);
    let (extracted, ok) = cli(&root, &["assets", "extract", "decoded/", "-o", "converted"]);
    let (mcp, error, _) = rpc.tool("assets_extract", json!({"source":"decoded/", "out":"converted"}));
    assert!(ok && !error, "{extracted} {mcp}");
    assert_eq!(extracted, mcp);
    assert_eq!(extracted["extracted"], 4);
    assert_eq!(extracted["summary"]["warnings"], 0);
    let sound = extracted["assets"].as_array().unwrap().iter().find(|a| a["kind"] == "vab").unwrap();
    assert!(sound["outputs"].as_array().unwrap().iter().any(|f| f["waveform"] == 1), "{sound}");
    for asset in extracted["assets"].as_array().unwrap() {
        let original = asset["outputs"].as_array().unwrap().iter().find(|o| o["role"] == "original").unwrap();
        let bytes = std::fs::read(root.join(original["path"].as_str().unwrap())).unwrap();
        if asset["kind"] == "tim" { assert_eq!(bytes, tim); }
    }
    let (empty, error, _) = rpc.tool("assets_extract", json!({"source":"empty/", "out":"empty-converted"}));
    assert!(!error, "{empty}");
    assert_eq!(empty["extracted"], 0);
    drop(rpc);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn skipped_tmd_geometry_is_visible_in_full_and_brief_extraction() {
    let root = std::env::temp_dir().join(format!("psx-skipped-tmd-{}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    // One supported triangle followed by an unsupported line packet.
    let words: [u32; 22] = [
        0x41, 0, 1, 28, 3, 52, 0, 52, 2, 0,
        0, 0, 100, 0, 100 << 16, 0,
        u32::from_le_bytes([0, 3, 1, 0x21]), 0x21ff_ffff, 1 << 16, 2,
        u32::from_le_bytes([0, 1, 0, 0x40]), 0,
    ];
    std::fs::write(root.join("partial.tmd"), words.into_iter().flat_map(u32::to_le_bytes).collect::<Vec<_>>()).unwrap();
    let (full, ok) = cli(&root, &["assets", "extract", "partial.tmd", "-o", "converted"]);
    assert!(ok, "{full}");
    assert_eq!(full["extracted"], 1);
    assert_eq!(full["assets"][0]["details"]["polygons"], 1);
    assert_eq!(full["assets"][0]["details"]["skipped_primitives"], 1);
    assert!(full["assets"][0]["details"]["warning"].as_str().unwrap().contains("--model-parser"));
    let mut rpc = Rpc::start(&root);
    let (brief, error, _) = rpc.tool("assets_extract", json!({"source":"partial.tmd", "out":"converted", "brief":true}));
    assert!(!error, "{brief}");
    assert_eq!(brief["summary"], full["summary"]);
    assert_eq!(brief["summary"]["warnings"], 1);
    assert_eq!(brief["summary"]["skipped_primitives"], 1);
    assert_eq!(brief["assets"], json!([]));
    drop(rpc);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn shared_signature_default_is_discoverable_and_explicit_override_wins() {
    let root = std::env::temp_dir().join(format!("psx-shared-sigs-{}", std::process::id()));
    std::fs::create_dir_all(root.join("signatures")).unwrap();
    std::fs::create_dir_all(root.join("project")).unwrap();
    std::fs::write(root.join("project/test.exe"), tiny_exe()).unwrap();
    let run = |args: &[&str]| {
        let out = psx().current_dir(root.join("project")).env("PSX_SIGS_DIR", root.join("signatures"))
            .env("PSX_BIOS_DIR", root.join("bios")).args(args).output().unwrap();
        (serde_json::from_slice::<Value>(&out.stdout).unwrap(), out.status.success())
    };
    let (context, ok) = run(&["context"]);
    assert!(ok, "{context}");
    assert_eq!(context["signatures"]["path"], json!(root.join("signatures")));
    assert_eq!(context["signatures"]["selection"], "PSX_SIGS_DIR");
    assert_eq!(context["signatures"]["available"], true);
    let (symbols, ok) = run(&["symbols", "test.exe"]);
    assert!(ok, "{symbols}"); // An empty installed index is valid, with zero matches.
    assert_eq!(symbols["signatures_loaded"], 0);
    let (missing, ok) = run(&["symbols", "test.exe", "--sigs", "missing"]);
    assert!(!ok, "{missing}");
    assert!(missing["error"]["message"].as_str().unwrap().contains("missing"));
    std::fs::remove_dir_all(root).unwrap();
}

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
    /// Directories answered to the server's roots/list; `None` advertises no roots capability.
    roots: Option<Vec<PathBuf>>,
}

impl Rpc {
    fn start(root: &Path) -> Self {
        Self::launch(root, &[], &[], None)
    }

    /// `global` precedes the `mcp` subcommand (e.g. `--root`); `mcp_args` follow it.
    fn launch(root: &Path, global: &[&str], mcp_args: &[&str], roots: Option<Vec<PathBuf>>) -> Self {
        let mut child = psx().env("PSX_BIOS_DIR",root.join("shared-bios")).args(global).arg("mcp").args(mcp_args)
            .current_dir(root).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null()).spawn().unwrap();
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
        let capabilities = if roots.is_some() { json!({"roots": {"listChanged": true}}) } else { json!({}) };
        let mut rpc = Rpc { child, input, lines, next_id: 1, response_timeout: Duration::from_secs(60), roots };
        let init = rpc.request(
            "initialize",
            json!({"protocolVersion": "2025-06-18", "capabilities": capabilities, "clientInfo": {"name": "psx-test", "version": "1"}}),
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
            if v["method"] == "roots/list" {
                let roots: Vec<Value> = self.roots.iter().flatten()
                    .map(|p| json!({"uri": format!("file://{}", p.display()), "name": "test"})).collect();
                self.send(json!({"jsonrpc": "2.0", "id": v["id"], "result": {"roots": roots}}));
                continue;
            }
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
    let out = psx().env("PSX_BIOS_DIR",root.join("shared-bios")).args(args).current_dir(root).output().unwrap();
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
fn guide_is_self_contained_read_only_and_matches_the_cli_for_every_topic() {
    let root = std::env::temp_dir().join(format!("psx-guide-mcp-{}", std::process::id()));
    std::fs::create_dir_all(root.join(".psx")).unwrap();
    // Help must remain available even when context cannot load the project's config.
    let config = b"deliberately invalid project configuration";
    std::fs::write(root.join(".psx/project.json"), config).unwrap();
    let mut rpc = Rpc::start(&root);
    let catalog = rpc.request("tools/list", json!({}));
    let tools = catalog["result"]["tools"].as_array().unwrap();
    let guide = tools.iter().find(|t| t["name"] == "guide").unwrap();
    assert_eq!(guide["annotations"]["readOnlyHint"], true);
    let (overview, error, _) = rpc.tool("guide", json!({}));
    let (cli_overview, ok) = cli(&root, &["guide"]);
    assert!(!error && ok, "{overview}");
    assert_eq!(overview, cli_overview);
    assert_eq!(overview["topic"], "overview");
    assert_eq!(overview["project_root"], json!(root));
    assert!(overview["build"]["id"].is_string());
    let topics = overview["topics"].as_array().unwrap();
    assert!(topics.len() > 1);
    for topic in topics {
        let call = &topic["next_call"];
        let name = call["arguments"]["topic"].as_str().unwrap();
        let (result, error, _) = rpc.tool(call["tool"].as_str().unwrap(), call["arguments"].clone());
        let (cli_result, ok) = cli(&root, &["guide", "--topic", name]);
        assert!(!error && ok, "{result}");
        assert_eq!(result, cli_result);
        assert_eq!(result["topic"], topic["topic"]);
        assert!(!result["text"].as_str().unwrap().is_empty());
        assert!(result.to_string().len() < 16 * 1024, "guide responses should stay inline and bounded");
        for next in result["next_calls"].as_array().unwrap() {
            assert!(tools.iter().any(|t| t["name"] == next["tool"]), "unknown next tool: {next}");
        }
    }
    let (bad, error, _) = rpc.tool("guide", json!({"topic":"not-a-topic"}));
    assert!(error && bad["error"]["code"] == "invalid_arguments", "{bad}");
    assert!(!psx().args(["guide", "--topic", "not-a-topic"]).current_dir(&root).output().unwrap().status.success());
    assert_eq!(std::fs::read(root.join(".psx/project.json")).unwrap(), config);
    assert_eq!(std::fs::read_dir(&root).unwrap().count(), 1, "guide must not create artifacts");
    assert_eq!(std::fs::read_dir(root.join(".psx")).unwrap().count(), 1);
    drop(rpc);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn client_roots_select_the_default_project_unless_pinned_ambiguous_or_required() {
    let root = std::env::temp_dir().join(format!("psx-client-roots-{}", std::process::id()));
    for name in ["server", "a", "b"] {
        std::fs::create_dir_all(root.join(name)).unwrap();
        std::fs::write(root.join(name).join("note.txt"), name).unwrap();
    }
    let (server, a, b) = (root.join("server"), root.join("a"), root.join("b"));
    let note = |rpc: &mut Rpc, args: Value| {
        let (read, error, _) = rpc.tool("artifact_read", args);
        assert!(!error, "{read}");
        read["items"][0].as_str().unwrap().to_owned()
    };
    let mut rpc = Rpc::launch(&server, &[], &[], Some(vec![a.clone()]));
    assert_eq!(note(&mut rpc, json!({"path":"note.txt"})), "a");
    let (context, error, _) = rpc.tool("context", json!({}));
    assert!(!error, "{context}");
    assert_eq!(context["project_root"], json!(a));
    assert_eq!(context["project_root_source"], "client_roots");
    // An explicit project_root still wins and resolves against the client root.
    assert_eq!(note(&mut rpc, json!({"path":"note.txt","project_root":"../b"})), "b");
    rpc.roots = Some(vec![b.clone()]);
    rpc.send(json!({"jsonrpc":"2.0","method":"notifications/roots/list_changed"}));
    // Notifications are handled asynchronously; the next calls must converge on b.
    let changed = (0..50).any(|_| {
        let seen = note(&mut rpc, json!({"path":"note.txt"}));
        seen == "b" || { std::thread::sleep(Duration::from_millis(20)); false }
    });
    assert!(changed, "roots change was not applied");
    drop(rpc);

    let mut ambiguous = Rpc::launch(&server, &[], &[], Some(vec![a.clone(), b.clone()]));
    assert_eq!(note(&mut ambiguous, json!({"path":"note.txt"})), "server");
    let (context, _, _) = ambiguous.tool("context", json!({}));
    assert!(context.get("project_root_source").is_none(), "{context}");
    drop(ambiguous);

    let mut pinned = Rpc::launch(&server, &["--root", a.to_str().unwrap()], &[], Some(vec![b.clone()]));
    assert_eq!(note(&mut pinned, json!({"path":"note.txt"})), "a");
    drop(pinned);

    let mut strict = Rpc::launch(&server, &[], &["--require-project-root"], Some(vec![a.clone()]));
    let (refused, error, _) = strict.tool("artifact_read", json!({"path":"note.txt"}));
    assert!(error && refused["error"]["message"].as_str().unwrap().contains("--require-project-root"), "{refused}");
    assert_eq!(note(&mut strict, json!({"path":"note.txt","project_root":b})), "b");
    drop(strict);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn scoped_projects_keep_paths_defaults_continuations_and_screens_isolated() {
    let root = std::env::temp_dir().join(format!("psx-project-isolation-{}", std::process::id()));
    for name in ["server", "a", "b"] { std::fs::create_dir_all(root.join(name)).unwrap(); }
    let bios: Vec<u8> = [0x0bf00000u32, 0].iter().flat_map(|w| w.to_le_bytes()).collect();
    for (name, marker) in [("a", 1), ("b", 2)] {
        let mut image = bios.clone(); image.push(marker);
        std::fs::write(root.join(name).join("bios.bin"), image).unwrap();
        std::fs::write(root.join(name).join("note.txt"), name).unwrap();
    }
    let server = root.join("server");
    let mut rpc = Rpc::start(&server);
    let first = rpc.next_id; rpc.next_id += 2;
    for (i, name) in ["a", "b"].into_iter().enumerate() {
        rpc.send(json!({"jsonrpc":"2.0","id":first+i as u64,"method":"tools/call","params":{
            "name":"context","arguments":{"project_root":format!("../{name}"),"bios":"bios.bin","save":true}}}));
    }
    let mut replies = Vec::new();
    while replies.len() < 2 {
        let reply: Value = serde_json::from_str(&rpc.lines.recv_timeout(Duration::from_secs(30)).unwrap()).unwrap();
        if reply.get("id").is_some() { replies.push(reply); }
    }
    for (i, name) in ["a", "b"].into_iter().enumerate() {
        let reply = replies.iter().find(|r| r["id"] == first+i as u64).unwrap();
        assert_ne!(reply["result"]["isError"], true, "{reply}");
        let result = &reply["result"]["structuredContent"];
        let project = root.join(name);
        assert_eq!(result["project_root"], json!(project));
        assert_eq!(result["next_calls"][0]["arguments"]["project_root"], json!(project));
        let config: Value = serde_json::from_slice(&std::fs::read(project.join(".psx/project.json")).unwrap()).unwrap();
        assert_eq!(config["bios"], json!(project.join("bios.bin")));
        let (read, error, _) = rpc.tool("artifact_read", json!({"project_root":project,"path":"note.txt"}));
        assert!(!error && read["items"] == json!([name]), "{read}");
    }
    assert!(!server.join(".psx/project.json").exists());
    let (default, error, _) = rpc.tool("context", json!({}));
    assert!(!error && default["status"] == "needs_setup", "{default}");
    assert_eq!(default["project_root"], json!(server));
    let (run, error, blocks) = rpc.tool("run", json!({"project_root":"../a","frames":1,"out":"capture","save_state":true,"brief":true,"return_screen":true}));
    assert!(!error, "{run}");
    assert!(root.join("a/capture/state.bin").exists());
    assert!(!root.join("b/capture").exists() && !server.join("capture").exists());
    assert!(blocks.iter().any(|b| b["type"] == "image"), "scoped screenshot must be read relative to its project");
    let next = &run["artifacts"][0]["read"];
    assert_eq!(next["arguments"]["project_root"], json!(root.join("a")));
    let mut arguments = next["arguments"].clone(); arguments["limit"] = json!(2);
    let (_, error, _) = rpc.tool(next["tool"].as_str().unwrap(), arguments);
    assert!(!error);
    let (missing, error, _) = rpc.tool("disc", json!({"project_root":"../b","image":"missing.cue"}));
    assert!(error, "{missing}");
    assert_eq!(missing["error"]["path"], json!(root.join("b/missing.cue")));
    assert_eq!(missing["error"]["project_root"], json!(root.join("b")));
    for project in [json!("../missing"), json!(7), json!("../a/note.txt"), json!("")] {
        let (_, error, _) = rpc.tool("guide", json!({"project_root":project})); assert!(error);
    }
    let (scan, error, _) = rpc.tool("assets_scan", json!({"project_root":"../a","source":"bios.bin"}));
    assert!(!error && scan["text"].is_string(), "text-mode scoped calls must still use JSON transport: {scan}");
    drop(rpc); std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn extraction_pages_and_brief_summaries_keep_the_complete_manifest() {
    let root = std::env::temp_dir().join(format!("psx-extraction-pages-{}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    let tim: Vec<u8> = [16u32, 2, 20, 0, 0x00020002, 0x0000001f, 0].iter().flat_map(|w| w.to_le_bytes()).collect();
    let mut source = Vec::new(); for _ in 0..5 { source.extend_from_slice(&tim); }
    std::fs::write(root.join("bank.bin"), source).unwrap();
    let mut rpc = Rpc::start(&root);
    let (page, error, _) = rpc.tool("assets_extract", json!({"source":"bank.bin","out":"converted","limit":2,"report_offset":1}));
    assert!(!error, "{page}");
    assert_eq!(page["extracted"], 5);
    assert_eq!(page["assets_page"]["total"], 5);
    assert_eq!(page["assets"].as_array().unwrap().len(), 2);
    assert_eq!(page["summary"]["output_files"], 10);
    let manifest = PathBuf::from(page["manifest"]["path"].as_str().unwrap());
    let complete: Value = serde_json::from_slice(&std::fs::read(&manifest).unwrap()).unwrap();
    assert_eq!(complete["assets"].as_array().unwrap().len(), 5);
    assert!(complete["assets"].as_array().unwrap().iter().flat_map(|a| a["outputs"].as_array().unwrap()).all(|o| root.join(o["path"].as_str().unwrap()).exists()));
    let next = &page["next_call"];
    let (read, error, _) = rpc.tool(next["tool"].as_str().unwrap(), next["arguments"].clone());
    let expected: Vec<_> = page["assets"].as_array().unwrap().iter().map(|a| {
        let mut a = a.clone(); a.as_object_mut().unwrap().remove("outputs_page"); a
    }).collect();
    assert!(!error && read["items"] == json!(expected), "{read}");
    let (brief, error, _) = rpc.tool("assets_extract", json!({"source":"bank.bin","out":"converted","brief":true,"limit":2}));
    assert!(!error && brief["assets"] == json!([]), "{brief}");
    assert_eq!(brief["manifest"], page["manifest"], "identical results reuse their content-bound manifest");
    assert_eq!(brief["summary"], page["summary"]);
    let (nested, error, _) = rpc.tool("assets_extract", json!({"source":"bank.bin","out":"converted","limit":1}));
    assert!(!error && nested["assets"][0]["outputs"].as_array().unwrap().len() == 1, "{nested}");
    assert_eq!(nested["assets"][0]["outputs_page"]["total"], 2);
    assert_eq!(nested["assets"][0]["outputs_page"]["truncated"], true);
    let old_manifest = std::fs::read(&manifest).unwrap();
    let mut changed = std::fs::read(root.join("bank.bin")).unwrap();
    changed[20] = 1; // Change pixels without changing any header, offset or output path.
    std::fs::write(root.join("bank.bin"), changed).unwrap();
    let (changed, error, _) = rpc.tool("assets_extract", json!({"source":"bank.bin","out":"converted","brief":true}));
    assert!(!error && changed["manifest"]["path"] != page["manifest"]["path"], "changed bytes must produce a distinct manifest: {changed}");
    assert_eq!(std::fs::read(&manifest).unwrap(), old_manifest);
    let (_, error, _) = rpc.tool("assets_extract", json!({"source":"bank.bin","out":"invalid","limit":0}));
    assert!(error && !root.join("invalid").exists());
    drop(rpc); std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn generated_parsers_chain_and_export_through_cli_and_mcp() {
    let root = std::env::temp_dir().join(format!("psx-parser-mcp-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    for kind in ["archive", "model", "asset", "video"] {
        let (_, ok) = cli(&root, &["template", "parser", kind, "--kind", kind]);
        assert!(ok);
    }
    let mut rpc = Rpc::start(&root);
    let (generated, ok) = cli(&root, &["template", "parser", "mcp-model", "--kind", "model"]);
    // Regenerate into an empty directory to compare identical CLI/MCP results.
    std::fs::remove_dir_all(root.join("mcp-model")).unwrap();
    let (mcp, error, _) = rpc.tool("template_parser_create", json!({"dest":"mcp-model","kind":"model"}));
    assert!(ok && !error, "{mcp}");
    assert_eq!(generated, mcp);
    let args = [
        "assets",
        "scan",
        "archive/sample.bin",
        "--archive-parser",
        "./archive/parser.py",
        "--model-parser",
        "./model/parser.py",
        "--json",
    ];
    let (scan, ok) = cli(&root, &args);
    let (mcp, error, _) = rpc.tool(
        "assets_scan",
        json!({"source":"archive/sample.bin","archive_parser":"./archive/parser.py","model_parser":"./model/parser.py","json":true}),
    );
    assert!(ok && !error, "{scan} {mcp}");
    assert_eq!(scan, mcp);
    let hit = &scan["files"][0]["hits"][0];
    assert_eq!(hit["entry"]["index"], 1);
    assert_eq!(hit["offset"], "0x18");
    assert_eq!(hit["vertices"], 3);
    assert_eq!(hit["polygons"], 1);
    assert_eq!(hit["parser"]["protocol_version"], 1);
    assert_eq!(scan["files"][0]["archive"]["entries"][0]["size"], 0);
    let (extract, ok) = cli(
        &root,
        &[
            "assets",
            "extract",
            "archive/sample.bin",
            "--archive-parser",
            "./archive/parser.py",
            "--model-parser",
            "./model/parser.py",
            "-o",
            "converted",
        ],
    );
    let (mcp, error, _) = rpc.tool(
        "assets_extract",
        json!({"source":"archive/sample.bin","archive_parser":"./archive/parser.py","model_parser":"./model/parser.py","out":"converted"}),
    );
    assert!(ok && !error, "{extract} {mcp}");
    assert_eq!(extract, mcp);
    let outputs = extract["assets"][0]["outputs"].as_array().unwrap();
    let role = |name: &str| {
        let path = outputs.iter().find(|o| o["role"] == name).unwrap()["path"].as_str().unwrap();
        std::fs::read(root.join(path)).unwrap()
    };
    assert_eq!(role("original"), std::fs::read(root.join("model/sample.bin")).unwrap());
    let doc: psx_assets::model_document::ModelDocument = serde_json::from_slice(&role("parser_document")).unwrap();
    assert_eq!(doc.into_model().unwrap().polygon_count(), 1);
    let gltf: Value = serde_json::from_slice(&role("gltf")).unwrap();
    assert_eq!(gltf["meshes"].as_array().unwrap().len(), 1);
    // The wrapper starter passes through the real TIM parser and PNG exporter.
    let (converted, ok) = cli(&root, &["assets", "extract", "asset/sample.bin", "--asset-parser", "./asset/parser.py", "-o", "wrapped"]);
    let (mcp, error, _) =
        rpc.tool("assets_extract", json!({"source":"asset/sample.bin","asset_parser":"./asset/parser.py","out":"wrapped"}));
    assert!(ok && !error, "{converted} {mcp}");
    assert_eq!(converted, mcp);
    assert_eq!(converted["assets"][0]["kind"], "tim");
    let (textured,ok)=cli(&root,&["assets","extract","model/sample.bin","--model-parser","./model/parser.py","--textures","asset/sample.bin","--texture-parser","./asset/parser.py","-o","wrapper-textures"]);
    assert!(ok,"{textured}");
    assert_eq!(textured["assets"][0]["details"]["vram_tims"]["from_textures"],1);
    assert_eq!(textured["texture_sources"][0]["hits"][0]["parser"]["protocol_version"],1);
    // A wrapped authored sound bank uses the same adapter and standard WAV
    // exporter. One program slot, one 16-byte ADPCM waveform, END flag set.
    let header_size = 32 + 128 * 16 + 16 * 32 + 512;
    let mut vab = vec![0u8; header_size + 16];
    vab[..4].copy_from_slice(b"pBAV");
    vab[4..8].copy_from_slice(&7u32.to_le_bytes());
    vab[12..16].copy_from_slice(&((header_size + 16) as u32).to_le_bytes());
    vab[18..20].copy_from_slice(&1u16.to_le_bytes());
    vab[22..24].copy_from_slice(&1u16.to_le_bytes());
    vab[header_size - 512 + 2..header_size - 512 + 4].copy_from_slice(&2u16.to_le_bytes());
    vab[header_size + 1] = 1;
    vab[header_size + 2..].fill(0x11);
    let wrapped = [b"WRAP".as_slice(), &(vab.len() as u32).to_le_bytes(), &vab].concat();
    std::fs::write(root.join("sound.dat"), wrapped).unwrap();
    let (sound, ok) = cli(&root, &["assets", "extract", "sound.dat", "--asset-parser", "./asset/parser.py", "-o", "sound"]);
    assert!(ok, "{sound}");
    assert_eq!(sound["assets"][0]["kind"], "vab");
    let outputs = sound["assets"][0]["outputs"].as_array().unwrap();
    let wave = outputs.iter().find(|o| o["waveform"] == 1).unwrap();
    assert_eq!(wave["wav"]["samples"], 28);
    assert_eq!(wave["wav"]["rate"], 44100);
    let wav = std::fs::read(root.join(wave["path"].as_str().unwrap())).unwrap();
    assert_eq!(&wav[..4], b"RIFF");
    // Author a small cooked ISO containing a single wrapped STR frame. The
    // generated video parser must enable the complete STR -> MDEC -> PNG path.
    let frame = std::fs::read(root.join("video/sample.bin")).unwrap();
    let mut iso = vec![0u8; 24 * 2048];
    let record = |name: &[u8], lba: u32, size: u32, flags: u8| {
        let mut record = vec![0u8; (33 + name.len()).next_multiple_of(2)];
        record[0] = record.len() as u8;
        record[2..6].copy_from_slice(&lba.to_le_bytes());
        record[6..10].copy_from_slice(&lba.to_be_bytes());
        record[10..14].copy_from_slice(&size.to_le_bytes());
        record[14..18].copy_from_slice(&size.to_be_bytes());
        record[25] = flags;
        record[28] = 1;
        record[31] = 1;
        record[32] = name.len() as u8;
        record[33..33 + name.len()].copy_from_slice(name);
        record
    };
    let pvd = &mut iso[16 * 2048..17 * 2048];
    pvd[0] = 1;
    pvd[1..6].copy_from_slice(b"CD001");
    pvd[6] = 1;
    pvd[80..84].copy_from_slice(&24u32.to_le_bytes());
    pvd[128..130].copy_from_slice(&2048u16.to_le_bytes());
    pvd[156..190].copy_from_slice(&record(&[0], 20, 2048, 2));
    let movie = record(b"MOVIE.STR;1", 21, 2048, 0);
    iso[20 * 2048..20 * 2048 + movie.len()].copy_from_slice(&movie);
    let sector = &mut iso[21 * 2048..22 * 2048];
    sector[..4].copy_from_slice(&[0x60, 1, 1, 0x80]);
    sector[6..8].copy_from_slice(&1u16.to_le_bytes());
    sector[8..12].copy_from_slice(&42u32.to_le_bytes());
    sector[12..16].copy_from_slice(&(frame.len() as u32).to_le_bytes());
    sector[16..18].copy_from_slice(&16u16.to_le_bytes());
    sector[18..20].copy_from_slice(&16u16.to_le_bytes());
    sector[32..32 + frame.len()].copy_from_slice(&frame);
    std::fs::write(root.join("movie.iso"), iso).unwrap();
    let (_, ok) = cli(&root, &["str", "movie.iso::/MOVIE.STR", "--no-audio", "-o", "unsupported"]);
    assert!(!ok);
    let (movie, ok) = cli(&root, &["str", "movie.iso::/MOVIE.STR", "--no-audio", "--frame-parser", "./video/parser.py", "-o", "movie"]);
    let (mcp, error, _) =
        rpc.tool("str", json!({"source":"movie.iso::/MOVIE.STR","no_audio":true,"frame_parser":"./video/parser.py","out":"movie"}));
    assert!(ok && !error, "{movie} {mcp}");
    assert_eq!(movie, mcp);
    assert_eq!(movie["video"]["decoded"], 1);
    let png = std::fs::read(root.join("movie/frame_00001.png")).unwrap();
    let mut reader = png::Decoder::new(std::io::Cursor::new(png)).read_info().unwrap();
    let mut pixels = vec![0; reader.output_buffer_size().unwrap()];
    reader.next_frame(&mut pixels).unwrap();
    assert!(pixels.iter().all(|&p| p == 128));
    for args in [
        json!({"source":"archive/sample.bin","entry":1,"model_parser":"./model/parser.py","out":"invalid"}),
        json!({"source":"model/sample.bin","model_parser":"./model/parser.py","asset_parser":"./asset/parser.py","out":"invalid"}),
        json!({"source":"model/sample.bin","model_parser_args":["oops"],"out":"invalid"}),
        json!({"source":"model/sample.bin","parser_timeout":0,"out":"invalid"}),
    ] {
        let (_, error, _) = rpc.tool("assets_extract", args);
        assert!(error);
    }
    assert!(!root.join("invalid").exists());
    drop(rpc);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn raw_upload_parsers_preserve_provenance_and_texture_models_through_cli_and_mcp() {
    let root = std::env::temp_dir().join(format!("psx-uploads-mcp-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let parser = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tools/parsers/kings_field_rtim.py").canonicalize().unwrap();
    let parser = parser.to_str().unwrap();
    let mut source = Vec::new();
    let mut append = |rect: [u16; 4], words: &[u16]| {
        for _ in 0..2 {
            for n in rect {
                source.extend_from_slice(&n.to_le_bytes());
            }
        }
        for n in words {
            source.extend_from_slice(&n.to_le_bytes());
        }
    };
    let mut palette = [0; 16];
    palette[1] = 0x801f;
    append([0, 500, 16, 1], &palette);
    append([640, 256, 1, 1], &[0x1111]);
    source.extend_from_slice(&[255; 16]);
    source.extend_from_slice(b"authored nonzero padding");
    std::fs::write(root.join("uploads.dat"), &source).unwrap();
    let mut rpc = Rpc::start(&root);
    let (scan, ok) = cli(&root, &["assets", "scan", "uploads.dat", "--asset-parser", "python3", "--asset-parser-arg", parser, "--json"]);
    let (mcp, error, _) =
        rpc.tool("assets_scan", json!({"source":"uploads.dat","asset_parser":"python3","asset_parser_args":[parser],"json":true}));
    assert!(ok && !error, "{scan} {mcp}");
    assert_eq!(scan, mcp);
    assert_eq!(scan["files"][0]["hits"], json!([]));
    assert_eq!(scan["upload_totals"],json!({"streams":1,"uploads":2,"words":17}));
    let stream = &scan["files"][0]["upload_streams"][0];
    assert_eq!(stream["uploads"][0]["rect"], json!([0, 500, 16, 1]));
    assert_eq!(stream["uploads"][1]["offset"], "0x30");
    let (extracted, ok) =
        cli(&root, &["assets", "extract", "uploads.dat", "--asset-parser", "python3", "--asset-parser-arg", parser, "-o", "uploads"]);
    let (mcp, error, _) =
        rpc.tool("assets_extract", json!({"source":"uploads.dat","asset_parser":"python3","asset_parser_args":[parser],"out":"uploads"}));
    assert!(ok && !error, "{extracted} {mcp}");
    assert_eq!(extracted, mcp);
    let asset = &extracted["assets"][0];
    assert_eq!(asset["kind"], "vram_uploads");
    let outputs = asset["outputs"].as_array().unwrap();
    let original = outputs.iter().find(|o| o["role"] == "original").unwrap()["path"].as_str().unwrap();
    assert_eq!(std::fs::read(root.join(original)).unwrap(), source);
    let words = asset["uploads"][1]["path"].as_str().unwrap();
    assert_eq!(std::fs::read(root.join(words)).unwrap(), [0x11, 0x11]);
    let model = json!({"version":1,"objects":[{"vertices":[[0,0,0],[100,0,0],[0,100,0]],"polygons":[{"vertices":[0,1,2],"uvs":[[0,0],[1,0],[0,1]],"texture":{"page":[640,256],"depth":4,"clut":[0,500]}}]}]});
    std::fs::write(root.join("model.dat"), b"authored geometry source").unwrap();
    std::fs::write(root.join("model.py"), format!("import sys\nsys.stdin.buffer.read()\nprint({:?})\n", model.to_string())).unwrap();
    let (converted, ok) = cli(
        &root,
        &[
            "assets",
            "extract",
            "model.dat",
            "--model-parser",
            "python3",
            "--model-parser-arg",
            "model.py",
            "--textures",
            "uploads.dat",
            "--texture-parser",
            "python3",
            "--texture-parser-arg",
            parser,
            "-o",
            "textured",
        ],
    );
    let (mcp,error,_)=rpc.tool("assets_extract",json!({"source":"model.dat","model_parser":"python3","model_parser_args":["model.py"],"textures":["uploads.dat"],"texture_parser":"python3","texture_parser_args":[parser],"out":"textured"}));
    assert!(ok && !error, "{converted} {mcp}");
    assert_eq!(converted, mcp);
    let details = &converted["assets"][0]["details"];
    assert_eq!(details["vram_uploads"]["from_textures"], 2);
    assert_eq!(converted["texture_sources"][0]["upload_streams"][0]["parser"], stream["parser"]);
    let png = details["texture_pages"][0]["png"].as_str().unwrap();
    let mut reader = png::Decoder::new(std::io::Cursor::new(std::fs::read(root.join(png)).unwrap())).read_info().unwrap();
    let mut rgba = vec![0; reader.output_buffer_size().unwrap()];
    reader.next_frame(&mut rgba).unwrap();
    assert_eq!(&rgba[..16], &[255, 0, 0, 255].repeat(4));
    assert_eq!(&rgba[16..20], &[0; 4]);
    let (_, error, _) = rpc.tool("assets_extract", json!({"source":"model.dat","texture_parser":"python3","out":"invalid"}));
    assert!(error);
    assert!(!root.join("invalid").exists());
    // Valid duplicated dimensions with insufficient data are an error, not a
    // stream terminator; malformed parser output must not create conversions.
    let mut truncated = source[..48].to_vec();
    truncated[4..6].copy_from_slice(&100u16.to_le_bytes());
    truncated[12..14].copy_from_slice(&100u16.to_le_bytes());
    std::fs::write(root.join("bad.dat"), truncated).unwrap();
    let (_, error, _) =
        rpc.tool("assets_extract", json!({"source":"bad.dat","asset_parser":"python3","asset_parser_args":[parser],"out":"invalid"}));
    assert!(error);
    assert!(!root.join("invalid").exists());
    drop(rpc);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn archives_scan_and_extract_match_cli_and_preserve_raw_entry_bytes() {
    let root = std::env::temp_dir().join(format!("psx-archive-mcp-{}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    let mut bytes = vec![0; 3 * 2048];
    bytes[..10].copy_from_slice(&[3, 0, 1, 0, 1, 0, 2, 0, 3, 0]);
    let tim: Vec<u8> = [
        16u32.to_le_bytes().as_slice(),
        2u32.to_le_bytes().as_slice(),
        20u32.to_le_bytes().as_slice(),
        &[0, 0, 0, 0, 2, 0, 2, 0],
        &[31, 0, 0, 0, 0, 0, 0, 0],
    ]
    .concat();
    bytes[2048 + 16..2048 + 16 + tim.len()].copy_from_slice(&tim);
    std::fs::write(root.join("authored.T"), &bytes).unwrap();
    let mut rpc = Rpc::start(&root);
    let (scan, ok) = cli(&root, &["assets", "scan", "authored.T", "--json"]);
    let (mcp, error, _) = rpc.tool("assets_scan", json!({"source":"authored.T", "json":true}));
    assert!(ok && !error, "{scan} {mcp}");
    assert_eq!(scan, mcp);
    assert_eq!(scan["files"][0]["archive"]["entries"][0]["size"], 0);
    assert_eq!(scan["files"][0]["hits"][0]["entry"]["index"], 1);
    assert_eq!(scan["files"][0]["hits"][0]["entry"]["offset"], "0x10");

    // Raw extraction works even for entries without known assets and for empty entries.
    for index in 0..3 {
        let index_string = index.to_string();
        let (raw, ok) = cli(&root, &["assets", "extract", "authored.T", "--entry", &index_string, "-o", "raw"]);
        let (mcp, error, _) = rpc.tool("assets_extract", json!({"source":"authored.T", "entry":index, "out":"raw"}));
        assert!(ok && !error, "{raw} {mcp}");
        assert_eq!(raw, mcp);
        let output = &raw["entries"][0]["outputs"][0]["path"];
        let actual = std::fs::read(root.join(output.as_str().unwrap())).unwrap();
        let (start, end) = if index == 0 { (2048, 2048) } else { (index * 2048, (index + 1) * 2048) };
        assert_eq!(actual, bytes[start..end]);
    }

    let (assets, ok) = cli(&root, &["assets", "extract", "authored.T", "-o", "converted"]);
    let (mcp, error, _) = rpc.tool("assets_extract", json!({"source":"authored.T", "out":"converted"}));
    assert!(ok && !error);
    assert_eq!(assets, mcp);
    assert_eq!(assets["extracted"], 1);
    assert_eq!(assets["assets"][0]["file"], "authored.T");
    assert_eq!(assets["assets"][0]["entry"]["archive_offset"], "0x800");
    let original = assets["assets"][0]["outputs"].as_array().unwrap().iter().find(|o| o["role"] == "original").unwrap();
    assert_eq!(std::fs::read(root.join(original["path"].as_str().unwrap())).unwrap(), tim);

    // Entries are still listed when kind filtering leaves no asset hits.
    let (filtered, error, _) = rpc.tool("assets_scan", json!({"source":"authored.T", "json":true, "kinds":["tmd"]}));
    assert!(!error, "{filtered}");
    assert_eq!(filtered["files"][0]["archive"]["entries"].as_array().unwrap().len(), 3);
    assert!(filtered["files"][0]["hits"].as_array().unwrap().is_empty());

    for args in [
        json!({"source":"authored.T", "entry":3, "out":"invalid"}),
        json!({"source":"authored.T", "entry":1, "kinds":["tim"], "out":"invalid"}),
        json!({"source":"authored.T", "entry":1, "archive":"none", "out":"invalid"}),
    ] {
        let (_, error, _) = rpc.tool("assets_extract", args);
        assert!(error);
    }
    assert!(!root.join("invalid").exists());

    // A host archive can use the same sibling .DIR reader as a disc source.
    let mut directory = vec![0; 28];
    directory[..4].copy_from_slice(&1u32.to_le_bytes());
    directory[4..13].copy_from_slice(b"CHECK.TIM");
    directory[16..20].copy_from_slice(&(tim.len() as u32).to_le_bytes());
    std::fs::write(root.join("named.DIR"), directory).unwrap();
    std::fs::write(root.join("named.bin"), &tim).unwrap();
    let (named, error, _) = rpc.tool("assets_scan", json!({"source":"named.bin", "json":true}));
    assert!(!error, "{named}");
    assert_eq!(named["files"][0]["archive"]["format"], "croc_dir");
    assert_eq!(named["files"][0]["hits"][0]["entry"]["name"], "CHECK.TIM");
    assert_eq!(named["files"][0]["hits"][0]["entry"]["offset"], "0x0");

    let mut malformed = bytes;
    malformed[8..10].copy_from_slice(&4u16.to_le_bytes());
    std::fs::write(root.join("bad.T"), malformed).unwrap();
    let (_, error, _) = rpc.tool("assets_scan", json!({"source":"bad.T", "archive":"dot-t", "json":true}));
    assert!(error);
    drop(rpc);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn spu_trace_links_key_ons_to_sample_hashes_and_uploads_through_cli_and_mcp() {
    let root = std::env::temp_dir().join(format!("psx-spu-trace-{}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    // Authored BIOS: upload one END block to sound RAM 0x1000 through the FIFO,
    // then key on voice 1 at 44.1 kHz and spin.
    let mut code = vec![
        0x3c09_1f80, // lui t1, 0x1F80
        0x3408_0200, 0xa528_1da6, // transfer address 0x1000
        0x3408_0100, 0xa528_1da8, // block header: END flag
    ];
    code.extend([0xa520_1da8u32; 7]); // the rest of the block
    code.extend([
        0x3408_1000, 0xa528_1c14, // voice 1 pitch
        0x3408_0200, 0xa528_1c16, // voice 1 start 0x1000
        0x3408_0002, 0xa528_1d88, // key on voice 1
        0x0bf0_0012, 0, // spin
    ]);
    assert_eq!(code[18], 0x0bf0_0012, "the jump targets itself");
    let bios: Vec<u8> = code.iter().flat_map(|w: &u32| w.to_le_bytes()).collect();
    std::fs::write(root.join("bios.bin"), &bios).unwrap();
    std::fs::write(root.join("start.bin"), psx_core::Machine::new(bios).save_state(None)).unwrap();
    let common = ["run", "--bios", "bios.bin", "--from-state", "start.bin", "--frames", "2", "--trace-spu", "--brief"];
    let (cli_report, ok) = cli(&root, &[common.as_slice(), &["--out", "cli"]].concat());
    assert!(ok, "{cli_report}");
    let mut rpc = Rpc::start(&root);
    let (mcp_report, error, _) = rpc.tool("run", json!({"bios":"bios.bin","from_state":"start.bin",
        "frames":2,"trace_spu":true,"brief":true,"out":"mcp"}));
    assert!(!error, "{mcp_report}");
    let strip = |mut v: Value| { v.as_object_mut().unwrap().remove("file"); v };
    assert_eq!(strip(cli_report["spu"].clone()), strip(mcp_report["spu"].clone()));
    let spu = &mcp_report["spu"];
    assert_eq!(spu["events"], json!({"upload": 1, "key_on": 1, "end": 1}), "{spu}");
    assert_eq!(spu["complete"], true);
    assert_eq!(spu["voices_keyed"], json!([1]));
    let mut block = [0u8; 16];
    block[1] = 1;
    let sample = &spu["samples"][0];
    assert_eq!(sample["sample_sha256"], psx_assets::adpcm::sample_sha256(&block, 0));
    assert_eq!(sample["bytes"], 16);
    assert_eq!(sample["upload"], json!({"source":"cpu","run_frame":0,"upload_address":0x1000,"offset":0}));
    let events: Vec<Value> = std::fs::read_to_string(root.join("mcp/spu.jsonl")).unwrap()
        .lines().map(|l| serde_json::from_str(l).unwrap()).collect();
    assert_eq!(events.iter().map(|e| e["event"].as_str().unwrap()).collect::<Vec<_>>(), ["upload", "key_on", "end"]);
    assert_eq!(events[1]["pc"], 0xbfc0_0000u32 + 4 * 17, "the key-on store");
    assert_eq!(events[1]["rate_hz"], 44_100.0);
    // summary keeps paths and counts only, and still attaches the screen.
    let (summary, error, blocks) = rpc.tool("run", json!({"bios":"bios.bin","from_state":"start.bin",
        "frames":2,"trace_spu":true,"summary":true,"return_screen":true,"out":"mcp-summary"}));
    assert!(!error, "{summary}");
    assert_eq!(summary["spu"]["events"], spu["events"]);
    assert_eq!(summary["next_call"]["tool"], "artifact_read");
    assert!(summary.get("artifacts").is_none() && summary.get("watch").is_none(), "{summary}");
    assert!(summary.to_string().len() < 2048, "{summary}");
    assert!(blocks.iter().any(|b| b["type"] == "image"), "summary must still attach the requested screen");
    drop(rpc);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn next_draw_captures_the_same_frame_through_cli_and_mcp() {
    let root = std::env::temp_dir().join(format!("psx-next-draw-{}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    // Authored BIOS: poll RAM[0], then fill 16x1 pixels with that RGB colour.
    // Pokes choose the drawing frames without copyrighted firmware or media.
    let code: [u32; 16] = [
        0x3c09_1f80,
        0x3529_1810, // t1 = GP0
        0x8c08_0000,
        0,
        0x1100_fffd,
        0, // poll RAM[0] (including load/branch delays)
        0xac00_0000,
        0x3c0a_0200,
        0x0148_5025, // clear flag, t2 = fill opcode | colour
        0xad2a_0000,
        0xad20_0000, // fill colour, destination (0,0)
        0x3c0a_0001,
        0x354a_0010,
        0xad2a_0000, // size 16x1
        0x0bf0_0002,
        0, // loop at BIOS + 8
    ];
    let bios: Vec<u8> = code.iter().flat_map(|w| w.to_le_bytes()).collect();
    std::fs::write(root.join("bios.bin"), &bios).unwrap();
    let mut m = psx_core::Machine::new(bios);
    m.bus.frames = 37;
    std::fs::write(root.join("start.bin"), m.save_state(None)).unwrap();
    let mut rpc = Rpc::start(&root);
    let common = [
        "run",
        "--bios",
        "bios.bin",
        "--from-state",
        "start.bin",
        "--frames",
        "4",
        "--poke",
        "0=255:4@2",
        "--poke",
        "0=65280:4@3",
        "--brief",
    ];
    let mut args = common.to_vec();
    // Read watches select the per-instruction loop; MCP below uses frame batching.
    args.extend(["--gpu-next-draw", "--gpu-search-frames", "3", "--watch-reads", "0:4", "--max-events", "0", "--out", "cli"]);
    let (cli_report, ok) = cli(&root, &args);
    assert!(ok, "{cli_report}");
    let base = json!({"bios":"bios.bin", "from_state":"start.bin", "frames":4,
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
    assert_eq!(capture["search"]["effective_max_frames"], 3);
    assert_eq!(capture["search"]["truncated_by_run_budget"], false);
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

    // The default search window cannot extend a one-field run.
    let mut short = base.clone();
    short["frames"] = json!(1);
    short["poke"] = json!([]);
    short.as_object_mut().unwrap().remove("gpu_search_frames");
    short["out"] = json!("short");
    let (report, error, _) = rpc.tool("run", short);
    assert!(!error, "{report}");
    assert_eq!(report["gpu_frame"]["status"], "not_found");
    assert_eq!(report["gpu_frame"]["reason"], "run_end");
    assert_eq!(report["gpu_frame"]["search"]["max_frames"], 60);
    assert_eq!(report["gpu_frame"]["search"]["run_budget_frames"], 1);
    assert_eq!(report["gpu_frame"]["search"]["effective_max_frames"], 1);
    assert_eq!(report["gpu_frame"]["search"]["truncated_by_run_budget"], true);
    assert_eq!(report["gpu_frame"]["search"]["frames_inspected"], 1);

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
        "guide",
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
        "ghidra_annotate",
        "ghidra_export_symbols",
        "context",
        "bios_list",
        "bios_install",
        "artifact_read",
    ] {
        assert!(names.contains(&want), "missing tool {want}: {names:?}");
    }
    for t in tools {
        assert_eq!(t["inputSchema"]["type"], "object", "{}", t["name"]);
        assert_eq!(t["inputSchema"]["additionalProperties"], false, "{}", t["name"]);
        assert!(t["description"].as_str().unwrap().len() > 80, "{} needs a real description", t["name"]);
    }
    let run = tools.iter().find(|t| t["name"] == "run").unwrap();
    assert!(!run["inputSchema"]["required"].as_array().is_some_and(|v|v.iter().any(|f|f=="bios")));
    for field in [
        "bios",
        "input",
        "frames",
        "watch",
        "trace",
        "gpu_frame",
        "memcard1",
        "pad",
        "from_state",
        "return_screen",
        "sample",
        "sample_every",
        "poke",
        "snapshot_every",
        "save_state_at",
        "watch_changes_only",
        "watch_reads",
        "brief",
        "gpu_next_draw",
        "gpu_search_frames",
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
            json!({"bios": "no-such-bios.bin", "frames": 1}),
            vec!["run", "--bios", "no-such-bios.bin", "--frames", "1"],
        ),
        ("run", json!({"bios":"b.bin", "gpu_next_draw":true}), vec!["run", "--bios", "b.bin", "--gpu-next-draw"]),
        (
            "run",
            json!({"bios":"b.bin", "gpu_search_frames":2}),
            vec!["run", "--bios", "b.bin", "--gpu-search-frames", "2"],
        ),
        (
            "run",
            json!({"bios":"b.bin", "gpu_next_draw":true, "gpu_search_frames":0}),
            vec!["run", "--bios", "b.bin", "--gpu-next-draw", "--gpu-search-frames", "0"],
        ),
        (
            "run",
            json!({"bios":"b.bin", "gpu_next_draw":true, "gpu_search_frames":601}),
            vec!["run", "--bios", "b.bin", "--gpu-next-draw", "--gpu-search-frames", "601"],
        ),
        (
            "run",
            json!({"bios":"b.bin", "gpu_next_draw":true, "gpu_frame":u64::MAX}),
            vec!["run", "--bios", "b.bin", "--gpu-next-draw", "--gpu-frame", "18446744073709551615"],
        ),
        ("disasm", json!({"source": "tiny.ps-exe", "function": "80010004"}), vec!["disasm", "tiny.ps-exe", "--function", "80010004"]),
        (
            "disasm",
            json!({"ram": "ram_a.bin", "addr": "80010000", "count": 2}),
            vec!["disasm", "--ram", "ram_a.bin", "--addr", "80010000", "--count", "2"],
        ),
        ("xrefs", json!({"target": "80010004", "source": "tiny.ps-exe"}), vec!["xrefs", "80010004", "--source", "tiny.ps-exe"]),
        ("xrefs", json!({"target": "zz", "source": "tiny.ps-exe"}), vec!["xrefs", "zz", "--source", "tiny.ps-exe"]),
        (
            "ram_diff",
            json!({"a": "ram_a.bin", "b": "ram_b.bin", "signed": true}),
            vec!["ram", "diff", "ram_a.bin", "ram_b.bin", "--signed"],
        ),
        (
            "ram_search",
            json!({"snapshots": ["ram_a.bin", "ram_b.bin"], "width": 4, "pred": ["increased"]}),
            vec!["ram", "search", "ram_a.bin", "ram_b.bin", "--width", "4", "--pred", "increased"],
        ),
        (
            "ram_view",
            json!({"snapshots": ["ram_a.bin", "ram_b.bin"], "addr": "80001000", "len": 8, "as": "fx16.16"}),
            vec!["ram", "view", "ram_a.bin", "ram_b.bin", "--addr", "80001000", "--len", "8", "--as", "fx16.16"],
        ),
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
fn external_parser_mcp_cancellation_stops_descendants() {
    for scoped in [false, true] {
    let root = std::env::temp_dir().join(format!("psx-parser-cancel-{}-{scoped}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("source.dat"), b"authored data").unwrap();
    std::fs::write(root.join("slow.py"),"import pathlib,subprocess,sys\nsys.stdin.buffer.read()\np=subprocess.Popen(['sleep','30'])\npathlib.Path('child.pid').write_text(str(p.pid))\np.wait()\n").unwrap();
    let mut rpc = Rpc::start(&root);
    let id = rpc.next_id;
    rpc.next_id += 1;
    let mut args = json!({"source":"source.dat","model_parser":"python3","model_parser_args":["slow.py"],"out":"converted"});
    if scoped { args["project_root"] = json!(root); }
    rpc.send(json!({"jsonrpc":"2.0","id":id,"method":"tools/call","params":{"name":"assets_extract","arguments":args}}));
    for _ in 0..300 {
        if root.join("child.pid").exists() {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let pid = std::fs::read_to_string(root.join("child.pid")).expect("parser starts before cancellation");
    rpc.send(json!({"jsonrpc":"2.0","method":"notifications/cancelled","params":{"requestId":id,"reason":"test"}}));
    let running = || std::fs::read_to_string(format!("/proc/{pid}/stat")).is_ok_and(|s| s.split_whitespace().nth(2) != Some("Z"));
    for _ in 0..300 {
        if !running() {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(!running(), "parser descendant must stop after MCP cancellation");
    drop(rpc);
    assert!(!root.join("converted").exists());
    std::fs::remove_dir_all(root).unwrap();
    }
}

#[cfg(target_os = "linux")]
#[test]
fn ghidra_mcp_cancellation_stops_worker() {
    use std::os::unix::fs::PermissionsExt;
    for scoped in [false, true] {
    let root = std::env::temp_dir().join(format!("psx-ghidra-cancel-{}-{scoped}", std::process::id()));
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
    let mut args = json!({"source":"probe.ps-exe","addr":"80010048","ghidra_home":install});
    if scoped { args["project_root"] = json!(root); }
    rpc.send(json!({"jsonrpc":"2.0","id":id,"method":"tools/call","params":{"name":"ghidra_decompile","arguments":args}}));
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
}

#[test]
#[ignore = "requires installed Ghidra/Java; no game media needed"]
fn ghidra_single_call_and_cached_cli_mcp_parity() {
    let root = std::env::temp_dir().join(format!("psx ghidra integration {}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("probe.ps-exe"), ghidra_probe()).unwrap();
    let mut rpc = Rpc::start(&root);
    rpc.response_timeout = Duration::from_secs(310);
    // Two concurrent cold requests for the same analysis must queue and share the import.
    let id=rpc.next_id;rpc.next_id+=2;
    for (i,addr) in [(id,"80010048"),(id+1,"80010000")] {
        rpc.send(json!({"jsonrpc":"2.0","id":i,"method":"tools/call","params":{"name":"ghidra_decompile","arguments":{"source":"probe.ps-exe","addr":addr}}}));
    }
    let mut results=std::collections::BTreeMap::new();
    while results.len()<2 {
        let line=rpc.lines.recv_timeout(rpc.response_timeout).unwrap();let v:Value=serde_json::from_str(&line).unwrap();
        if let Some(i)=v["id"].as_u64() {assert_ne!(v["result"]["isError"],true,"{v}");results.insert(i,v["result"]["structuredContent"].clone());}
    }
    let first=results.remove(&id).unwrap();let queued=results.remove(&(id+1)).unwrap();
    assert_eq!(first["analysis"],queued["analysis"]);assert_eq!(queued["status"],"complete","{queued}");
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

#[test]
#[ignore = "requires installed Ghidra/Java; authored runtime RAM and native state only"]
fn ghidra_runtime_annotations_symbols_and_overlay_residency() {
    let root = std::env::temp_dir().join(format!("psx ghidra runtime {}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    let bios = vec![0; 512 * 1024];
    std::fs::write(root.join("bios.bin"), &bios).unwrap();
    let mut machine = psx_core::Machine::new(bios);
    let exe = ghidra_probe();
    machine.bus.ram[0x10000..0x10800].copy_from_slice(&exe[0x800..]);
    let routines: &[(usize, &[u32])] = &[
        (0x10000, &[0x3c1c8001, 0x379c00c0, 0x0c004010, 0x24040007, 0x08004002, 0]),
        (0x10040, &[0x8f820000, 0, 0x24420003, 0xaf820000, 0x03e00008, 0x00441021]),
    ];
    for (off, words) in routines {
        for (i, w) in words.iter().enumerate() {
            machine.bus.ram[off + i * 4..off + i * 4 + 4].copy_from_slice(&w.to_le_bytes());
        }
    }
    machine.cpu.set_pc(0x80010000);
    machine.cpu.regs[29] = 0x801ffff0;
    std::fs::write(root.join("start.bin"), machine.save_state(None)).unwrap();
    std::fs::write(root.join("ram.bin"), &machine.bus.ram).unwrap();
    let mut rpc = Rpc::start(&root);
    rpc.response_timeout = Duration::from_secs(310);
    let args = json!({"ram":"ram.bin","ranges":["80010000-80010098"],"entries":["a0010000"],"gp":"800100c0","addr":"a061004c"});
    let (first, error, _) = rpc.tool("ghidra_decompile", args.clone());
    assert!(!error && first["status"] == "complete", "{first}");
    assert_eq!(first["addr"], "0x8001004C");
    assert_eq!(first["requested_addr"], "a061004c");
    assert_eq!(first["function"]["entry"], "0x80010040");
    let manifest = first["analysis"].as_str().unwrap();
    let (same, ok) = cli(
        &root,
        &[
            "ghidra",
            "decompile",
            "--ram",
            "ram.bin",
            "--range",
            "80010000-80010098",
            "--entry",
            "a0010000",
            "--gp",
            "800100c0",
            "--addr",
            "a061004c",
        ],
    );
    assert!(ok);
    assert_eq!(first, same);
    std::fs::write(root.join("renamed.bin"), &machine.bus.ram).unwrap();
    let mut alias = args.clone();
    alias["ram"] = json!("renamed.bin");
    let (same, error, _) = rpc.tool("ghidra_decompile", alias);
    assert!(!error);
    assert_eq!(same["analysis"], first["analysis"], "input filenames must not control identity");
    let collision = json!({"functions":[{"addr":"80010000","name":first["function"]["name"]}]});
    std::fs::write(root.join("generated-conflict.json"), serde_json::to_vec(&collision).unwrap()).unwrap();
    let (displaced, error, _) = rpc.tool("ghidra_annotate", json!({"analysis":manifest,"file":"generated-conflict.json"}));
    assert!(!error && displaced["conflicts"].as_array().unwrap().len() == 1, "{displaced}");
    let patch = json!({"functions":[{"addr":"a0010040","name":"studied_writer","comment":"Authored GP-relative counter writer."}]});
    std::fs::write(root.join("patch.json"), serde_json::to_vec(&patch).unwrap()).unwrap();
    let (annotated, error, _) = rpc.tool("ghidra_annotate", json!({"analysis":manifest,"file":"patch.json"}));
    assert!(!error && annotated["revision"] == 2, "{annotated}");
    assert_ne!(annotated["project"], first["project"]);
    assert!(Path::new(first["project"].as_str().unwrap()).exists());
    let (named, error, _) = rpc.tool("ghidra_decompile", json!({"analysis":manifest,"addr":"8001004c"}));
    assert!(!error && named["function"]["name"] == "studied_writer", "{named}");
    assert!(named["pseudocode"].as_str().unwrap().contains("studied_writer"));
    assert_eq!(named["function"]["comment"], patch["functions"][0]["comment"]);
    assert_ne!(first["artifact"], named["artifact"], "annotation revision invalidates cached pseudocode");
    std::fs::write(root.join("user-conflict.json"), br#"{"functions":[{"addr":"80010000","name":"studied_writer"}]}"#).unwrap();
    let (conflict, error, _) = rpc.tool("ghidra_annotate", json!({"analysis":manifest,"file":"user-conflict.json"}));
    assert!(error && conflict["error"]["code"] == "symbol_conflict", "{conflict}");
    let (export, error, _) = rpc.tool("ghidra_export_symbols", json!({"analysis":manifest}));
    assert!(!error, "{export}");
    let symbols = export["symbols"].as_str().unwrap();
    let (cli_export, ok) = cli(&root, &["ghidra", "export-symbols", manifest]);
    assert!(ok, "{cli_export}");
    assert_eq!(export, cli_export);
    let trace_args = [
        "run",
        "--bios",
        "bios.bin",
        "--from-state",
        "start.bin",
        "--frames",
        "1",
        "--trace",
        "--trace-range",
        "studied_writer",
        "--symbols",
        symbols,
        "--max-events",
        "100",
        "--watch",
        "800100c0:4",
        "--out",
        "trace-a",
    ];
    let (run, ok) = cli(&root, &trace_args);
    assert!(ok, "{run}");
    assert!(std::fs::read_to_string(root.join("trace-a/trace.txt")).unwrap().contains("studied_writer"));
    let writes = std::fs::read_to_string(root.join("trace-a/writes.jsonl")).unwrap();
    assert!(writes.contains("studied_writer") && writes.contains("body_match"), "{writes}");
    let (dis, ok) = cli(&root, &["disasm", "--ram", "ram.bin", "--addr", "80010040", "--count", "6", "--symbols", symbols]);
    assert!(ok && dis.to_string().contains("studied_writer"), "{dis}");
    let (refs, ok) = cli(&root, &["xrefs", "800100c0", "--ram", "ram.bin", "--gp", "800100c0", "--symbols", symbols]);
    assert!(ok && refs.to_string().contains("studied_writer"), "{refs}");
    // The same address now contains a different function; no name from snapshot A may label it.
    machine.bus.ram[0x10048..0x1004c].copy_from_slice(&0x24420004u32.to_le_bytes());
    std::fs::write(root.join("ram.bin"), &machine.bus.ram).unwrap();
    std::fs::write(root.join("changed.bin"), machine.save_state(None)).unwrap();
    let (second, error, _) = rpc.tool("ghidra_decompile", args);
    assert!(!error && second["status"] == "complete", "{second}");
    assert_ne!(first["analysis"], second["analysis"]);
    assert_ne!(first["project"], second["project"]);
    let (dis, ok) = cli(&root, &["disasm", "--ram", "ram.bin", "--addr", "80010040", "--count", "6", "--symbols", symbols]);
    assert!(ok && !dis.to_string().contains("studied_writer"), "{dis}");
    let (refs, ok) = cli(&root, &["xrefs", "800100c0", "--ram", "ram.bin", "--gp", "800100c0", "--symbols", symbols]);
    assert!(ok && !refs.to_string().contains("studied_writer"), "{refs}");
    let mut changed_args = trace_args;
    changed_args[6] = "changed.bin";
    changed_args[changed_args.len() - 1] = "trace-b";
    let (run, ok) = cli(&root, &changed_args);
    assert!(ok, "{run}");
    assert!(!std::fs::read_to_string(root.join("trace-b/trace.txt")).unwrap().contains("studied_writer"));
    assert!(!std::fs::read_to_string(root.join("trace-b/writes.jsonl")).unwrap().contains("studied_writer"));
    // Re-importing snapshot A still retains its annotations and cached analysis.
    let (old, error, _) = rpc.tool("ghidra_decompile", json!({"analysis":manifest,"addr":"8001004c"}));
    assert!(!error && old["function"]["name"] == "studied_writer", "{old}");
    drop(rpc);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn first_use_bios_setup_context_and_compact_screens_work_through_mcp() {
    let root = std::env::temp_dir().join(format!("psx-first-use-{}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    let mut rpc = Rpc::start(&root);
    let (context, error, _) = rpc.tool("context", json!({}));
    assert!(!error && context["status"] == "needs_setup", "{context}");
    assert!(context["build"]["id"].as_str().unwrap().starts_with("src-"));
    let (list, error, _) = rpc.tool("bios_list", json!({}));
    assert!(!error && list["installed"].as_array().unwrap().is_empty(), "{list}");
    let (same, ok) = cli(&root, &["bios", "list"]);
    assert!(ok);
    assert_eq!(list, same);
    let (missing, error, _) = rpc.tool("run", json!({"frames":0}));
    assert!(error && missing["error"]["code"] == "bios_required", "{missing}");
    // Authored firmware is supported as a local explicit preference, without any network.
    let bios: Vec<u8> = [0x3c081f80u32, 0x35081800, 0x24090001, 0xa1090001, 0x0bf00004, 0].iter().flat_map(|w| w.to_le_bytes()).collect();
    std::fs::write(root.join("authored.bin"), &bios).unwrap();
    let (installed, error, _) = rpc.tool("bios_install", json!({"from":"authored.bin","dir":"shared-bios"}));
    assert!(!error && installed["status"] == "installed", "{installed}");
    let installed_path = installed["bios"]["path"].as_str().unwrap();
    assert_eq!(std::fs::read(installed_path).unwrap(), bios);
    let (again, ok) = cli(&root, &["bios", "install", "--from", "authored.bin", "--dir", "shared-bios"]);
    assert!(ok);
    assert_eq!(again, installed, "install is idempotent");
    let (configured, error, _) = rpc.tool("context", json!({"bios":installed_path,"save":true}));
    assert!(!error && configured["status"] == "ready" && configured["saved"] == true, "{configured}");
    for compact in [false, true] {
        let out = if compact { "compact" } else { "expanded" };
        let (run, error, content) = rpc.tool("run", json!({"frames":0,"save_state":true,"brief":compact,"return_screen":true,"out":out}));
        assert!(!error && run["status"] == "complete", "{run}");
        let block = content.iter().find(|v| v["type"] == "image").expect("screenshot attached in both output modes");
        use base64::Engine;
        let decoded = base64::engine::general_purpose::STANDARD.decode(block["data"].as_str().unwrap()).unwrap();
        assert_eq!(decoded, std::fs::read(root.join(out).join("screen.png")).unwrap());
        let report: Value = serde_json::from_slice(&std::fs::read(root.join(out).join("report.json")).unwrap()).unwrap();
        assert_eq!(report["identity"]["backend"], "psx-core");
        if compact {
            assert!(run["frames"].get("numbering").is_none());
        }
    }
    // A checkpoint is self-identifying: changing project defaults cannot select another BIOS.
    std::fs::write(root.join("wrong.bin"), [0; 32]).unwrap();
    let (_, error, _) = rpc.tool("context", json!({"bios":"wrong.bin","save":true}));
    assert!(!error);
    let (resumed, error, _) = rpc.tool("run", json!({"from_state":"compact/state.bin","frames":0,"out":"resumed"}));
    assert!(!error && resumed["resolved"]["bios_selection"] == "state_sha256", "{resumed}");
    assert_eq!(resumed["identity"]["backend"], "psx-core");
    let (wrong, error, _) = rpc.tool("run", json!({"bios":"wrong.bin","from_state":"compact/state.bin","frames":0}));
    assert!(error && wrong["error"]["code"] == "identity_mismatch", "{wrong}");
    // An unbound report is never allowed to supply a BIOS or media path.
    let mut report: Value = serde_json::from_slice(&std::fs::read(root.join("compact/report.json")).unwrap()).unwrap();
    report["files"]["state.bin"]["sha256"] = json!("wrong");
    report["identity"]["bios"] = json!("wrong.bin");
    std::fs::write(root.join("compact/report.json"), serde_json::to_vec(&report).unwrap()).unwrap();
    let (resumed, error, _) = rpc.tool("run", json!({"from_state":"compact/state.bin","frames":0,"out":"hash-lookup"}));
    assert!(!error && resumed["identity"]["bios_sha256"] == installed["bios"]["sha256"], "{resumed}");
    drop(rpc);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn disassembly_exposes_search_and_listing_limits_instead_of_claiming_complete_functions() {
    let root = std::env::temp_dir().join(format!("psx-disasm-evidence-{}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    let mut exe = tiny_exe();
    // A 102-instruction leaf: jump target at entry and return beyond the old 32-word cutoff.
    for off in (0x800..0x990).step_by(4) {
        exe[off..off + 4].copy_from_slice(&0x24420001u32.to_le_bytes());
    }
    exe[0x990..0x994].copy_from_slice(&0x03e00008u32.to_le_bytes());
    std::fs::write(root.join("long.exe"), exe).unwrap();
    let mut rpc = Rpc::start(&root);
    let (whole, error, _) = rpc.tool("disasm", json!({"source":"long.exe","function":"80010000"}));
    assert!(!error && whole["lines"].as_array().unwrap().len() == 102, "{whole}");
    assert_eq!(whole["function"]["stop_reason"], "return");
    assert_eq!(whole["function"]["truncated"], false);
    let (cli_whole, ok) = cli(&root, &["disasm", "long.exe", "--function", "80010000"]);
    assert!(ok);
    assert_eq!(whole, cli_whole);
    let (limited, error, _) = rpc.tool("disasm", json!({"source":"long.exe","function":"80010000","count":16}));
    assert!(!error && limited["lines"].as_array().unwrap().len() == 16);
    assert_eq!(limited["function"]["end"], whole["function"]["end"]);
    assert_eq!(limited["function"]["listing_truncated"], true);
    assert_eq!(limited["listing"]["next_addr"], "0x80010040");
    let (partial, error, _) = rpc.tool("disasm", json!({"source":"long.exe","function":"80010000","function_budget":32}));
    assert!(!error && partial["function"]["truncated"] == true, "{partial}");
    assert_eq!(partial["function"]["stop_reason"], "search_limit");
    assert!(!partial["warnings"].as_array().unwrap().is_empty());
    let text = psx()
        .args(["disasm", "long.exe", "--function", "80010000", "--function-budget", "32", "--text"])
        .current_dir(&root)
        .output()
        .unwrap();
    assert!(text.status.success());
    assert!(String::from_utf8(text.stdout).unwrap().contains("boundary search is incomplete"));
    drop(rpc);
    std::fs::remove_dir_all(root).unwrap();
}

#[cfg(unix)]
#[test]
fn bios_download_rejects_unverified_bytes_and_never_replaces_existing_installs() {
    use std::os::unix::fs::PermissionsExt;
    let root = std::env::temp_dir().join(format!("psx-bios-integrity-{}", std::process::id()));
    let bin = root.join("fake-bin");
    std::fs::create_dir_all(&bin).unwrap();
    // A fake transport exercises verification without network access or firmware fixtures.
    let curl = bin.join("curl");
    std::fs::write(&curl,"#!/bin/sh\nwhile [ $# -gt 0 ]; do\n if [ \"$1\" = --output ]; then shift; target=$1; fi\n shift\ndone\nprintf unverified > \"$target\"\n").unwrap();
    std::fs::set_permissions(&curl, std::fs::Permissions::from_mode(0o755)).unwrap();
    let out = psx()
        .env("PATH", &bin)
        .args(["bios", "install", "--download", "--region", "pal", "--dir", "installed"])
        .current_dir(&root)
        .output()
        .unwrap();
    assert!(!out.status.success());
    let report: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert!(report["error"]["message"].as_str().unwrap().contains("pinned SHA-256"), "{report}");
    assert!(std::fs::read_dir(root.join("installed")).unwrap().next().is_none(), "failed downloads leave no staged or published bytes");
    std::fs::write(root.join("source.bin"), [1; 64]).unwrap();
    let (installed, ok) = cli(&root, &["bios", "install", "--from", "source.bin", "--dir", "installed"]);
    assert!(ok, "{installed}");
    let path = installed["bios"]["path"].as_str().unwrap();
    std::fs::write(path, b"preserve these bytes").unwrap();
    let (failed, ok) = cli(&root, &["bios", "install", "--from", "source.bin", "--dir", "installed"]);
    assert!(!ok && failed.get("error").is_some());
    assert_eq!(std::fs::read(path).unwrap(), b"preserve these bytes");
    assert_eq!(std::fs::read_dir(root.join("installed")).unwrap().count(), 1);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn regional_bios_discovery_requires_a_unique_compatible_image() {
    let root = std::env::temp_dir().join(format!("psx-bios-region-{}", std::process::id()));
    std::fs::create_dir_all(root.join("bios")).unwrap();
    let mut exe = tiny_exe();
    exe[0x4c..0x4c + 6].copy_from_slice(b"Europe");
    std::fs::write(root.join("game.exe"), exe).unwrap();
    let mut rpc = Rpc::start(&root);
    let (missing, error, _) = rpc.tool("context", json!({"image":"game.exe"}));
    assert!(!error && missing["status"] == "needs_setup", "{missing}");
    assert_eq!(missing["next_calls"][0]["arguments"]["region"], "pal");
    assert!(missing["next_calls"][0]["arguments"].get("fields_per_tick").is_none());
    assert_eq!(missing["setup"]["next_action"]["arguments"], json!({"download":true,"region":"pal"}));
    let mut pal = vec![0; 524288];
    let version = b"System ROM Version TEST E\0";
    pal[0x7ff32..0x7ff32 + version.len()].copy_from_slice(version);
    std::fs::write(root.join("bios/pal.bin"), &pal).unwrap();
    let (ready, error, _) = rpc.tool("context", json!({"image":"game.exe"}));
    assert!(!error && ready["status"] == "ready", "{ready}");
    assert_eq!(ready["resolved"]["bios_selection"], "unique_regional_candidate");
    // Two filenames with identical bytes still represent a single choice.
    std::fs::write(root.join("bios/duplicate.rom"), &pal).unwrap();
    let (duplicate, error, _) = rpc.tool("context", json!({"image":"game.exe"}));
    assert!(!error && duplicate["status"] == "ready", "{duplicate}");
    pal[0] = 1;
    std::fs::write(root.join("bios/other-pal.bin"), &pal).unwrap();
    let (ambiguous, error, _) = rpc.tool("context", json!({"image":"game.exe"}));
    assert!(!error && ambiguous["status"] == "needs_setup", "{ambiguous}");
    assert_eq!(ambiguous["setup"]["selection"], "ambiguous_regional_candidates");
    let (explicit, error, _) = rpc.tool("context", json!({"image":"game.exe","bios":"bios/pal.bin","save":true}));
    assert!(!error && explicit["status"] == "ready", "{explicit}");
    let (saved, error, _) = rpc.tool("context", json!({}));
    assert!(!error && saved["resolved"]["bios_selection"] == "project_config", "{saved}");
    drop(rpc);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn artifacts_support_bounded_navigation_filters_projection_and_png_delivery() {
    let root = std::env::temp_dir().join(format!("psx-artifact-workflow-{}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("report.json"), br#"{"rows":[{"value":1},{"value":2},{"value":3}]}"#).unwrap();
    std::fs::write(
        root.join("samples.csv"),
        "frame,pc,note\r\n0,0x80010000,zero\r\n1,0x80010000,\"comma, and \"\"quote\"\"\"\r\n2,0x80010000,\"two\nlines\"\r\n",
    )
    .unwrap();
    let logs =
        (0..23).map(|f| json!({"frame":f,"run_frame":f-100i64,"pc":0x80010000u32,"value":f*2}).to_string() + "\n").collect::<String>();
    std::fs::write(root.join("writes.jsonl"), logs).unwrap();
    let mut rpc = Rpc::start(&root);
    let (first, error, _) = rpc.tool("artifact_read", json!({"path":"report.json","pointer":"/rows","limit":1}));
    assert!(!error && first["items"] == json!([{"value":1}]), "{first}");
    let (same, ok) = cli(&root, &["artifact-read", "report.json", "--pointer", "/rows", "--limit", "1"]);
    assert!(ok);
    assert_eq!(same, first);
    assert_eq!(first["page"]["total"], 3);
    assert_eq!(first["page"]["truncated"], true);
    let cursor = first["page"]["next_cursor"].as_str().unwrap();
    let (second, error, _) = rpc.tool("artifact_read", json!({"path":"report.json","pointer":"/rows","limit":1,"cursor":cursor}));
    assert!(!error && second["items"] == json!([{"value":2}]));
    let (changed, error, _) = rpc.tool("artifact_read", json!({"path":"report.json","pointer":"/rows/0","cursor":cursor}));
    assert!(error && changed["error"]["code"] == "stale_cursor", "{changed}");
    std::fs::write(root.join("report.json"), br#"{"rows":[0]}"#).unwrap();
    let (stale, error, _) = rpc.tool("artifact_read", json!({"path":"report.json","pointer":"/rows","cursor":cursor}));
    assert!(error && stale["error"]["code"] == "stale_cursor", "{stale}");
    let args = json!({"path":"samples.csv","frame_from":1,"frame_to":2,"pc":"80010000","columns":["frame","note"],"limit":1});
    let (csv, error, _) = rpc.tool("artifact_read", args.clone());
    assert!(!error && csv["items"] == json!([{"frame":"1","note":"comma, and \"quote\""}]), "{csv}");
    let mut follow = args.clone();
    follow["cursor"] = csv["page"]["next_cursor"].clone();
    let (csv, error, _) = rpc.tool("artifact_read", follow);
    assert!(!error && csv["items"] == json!([{"frame":"2","note":"two\nlines"}]), "{csv}");
    assert_eq!(csv["page"]["truncated"], false);
    let mut args = json!({"path":"writes.jsonl","frame_from":3,"frame_to":19,"pc":"0x80010000","columns":["frame","value"],"limit":4});
    let mut frames = Vec::new();
    loop {
        let (page, error, _) = rpc.tool("artifact_read", args.clone());
        assert!(!error, "{page}");
        frames.extend(page["items"].as_array().unwrap().iter().map(|v| v["frame"].as_i64().unwrap()));
        if page["page"]["truncated"] != true {
            break;
        }
        assert!(page["page"]["total"].is_null());
        args["cursor"] = page["page"]["next_cursor"].clone();
    }
    assert_eq!(frames, (3..=19).collect::<Vec<_>>(), "continuations must neither omit nor duplicate filtered rows");
    let f = std::fs::File::create(root.join("screen.png")).unwrap();
    let mut enc = png::Encoder::new(f, 1, 1);
    enc.set_color(png::ColorType::Rgb);
    enc.set_depth(png::BitDepth::Eight);
    enc.write_header().unwrap().write_image_data(&[255, 0, 0]).unwrap();
    let (image, error, blocks) = rpc.tool("artifact_read", json!({"path":"screen.png"}));
    assert!(!error && image["image"]["width"] == 1, "{image}");
    use base64::Engine;
    let data = blocks.iter().find(|b| b["type"] == "image").unwrap()["data"].as_str().unwrap();
    assert_eq!(base64::engine::general_purpose::STANDARD.decode(data).unwrap(), std::fs::read(root.join("screen.png")).unwrap());
    let (dir, error, _) = rpc.tool("artifact_read", json!({"path":".","limit":1}));
    assert!(!error && dir["format"] == "directory" && dir["page"]["truncated"] == true, "{dir}");
    assert_eq!(dir["items"][0]["read"]["tool"], "artifact_read");
    std::fs::write(root.join("raw.bin"), [0, 1, 255, 7]).unwrap();
    let (hex, error, _) = rpc.tool("artifact_read", json!({"path":"raw.bin","offset":1,"limit":2}));
    assert!(!error && hex["hex"] == "01 FF" && hex["page"]["next_offset"] == 3, "{hex}");
    std::fs::write(root.join("huge.jsonl"), "x".repeat(65537)).unwrap();
    let (large, error, _) = rpc.tool("artifact_read", json!({"path":"huge.jsonl"}));
    assert!(error && large["error"]["code"] == "limit_exceeded", "{large}");
    let text = "x".repeat(127) + "\n";
    std::fs::write(root.join("large.txt"), text.repeat(40000) + "needle\n").unwrap();
    let (bounded, error, _) = rpc.tool("artifact_read", json!({"path":"large.txt","contains":"needle"}));
    assert!(!error && bounded["items"] == json!([]) && bounded["page"]["truncated"] == true, "{bounded}");
    assert!(bounded["coverage"]["bytes_inspected"].as_u64().unwrap() <= 4 * 1024 * 1024 + 65536);
    let (last, error, _) =
        rpc.tool("artifact_read", json!({"path":"large.txt","contains":"needle","cursor":bounded["page"]["next_cursor"]}));
    assert!(!error && last["items"] == json!(["needle"]), "{last}");
    let (skip,error,_)=rpc.tool("artifact_read",json!({"path":"large.txt","offset":39999,"limit":3}));
    assert!(!error && skip["items"]==json!([]) && skip["page"]["truncated"]==true,"large initial offsets must remain bounded: {skip}");
    let (after_skip,error,_)=rpc.tool("artifact_read",json!({"path":"large.txt","limit":3,"cursor":skip["page"]["next_cursor"]}));
    assert!(!error && after_skip["items"]==json!(["x".repeat(127),"needle"]),"continuation must retain an initial skip beyond the scan budget: {after_skip}");
    drop(rpc);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn inline_inputs_match_replay_and_release_at_the_final_checkpoint_boundary() {
    let root = std::env::temp_dir().join(format!("psx-inline-input-{}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    let bios: Vec<u8> = [0x0bf00000u32, 0].iter().flat_map(|w| w.to_le_bytes()).collect();
    std::fs::write(root.join("bios.bin"), &bios).unwrap();
    let events = json!([{"frame":0,"buttons":["cross"],"until":2}]);
    std::fs::write(root.join("input.json"), events.to_string()).unwrap();
    let mut rpc = Rpc::start(&root);
    let (run, error, _) =
        rpc.tool("run", json!({"bios":"bios.bin","input":events,"frames":2,"save_state":true,"save_state_at":[1],"out":"inline"}));
    assert!(!error, "{run}");
    assert_eq!(
        run["input"]["events"],
        json!([{"frame":0,"buttons":["CROSS"],"sticks":[128,128,128,128]},{"frame":2,"buttons":[],"sticks":[128,128,128,128]}])
    );
    let (_, middle) = psx_core::Machine::load_state(&std::fs::read(root.join("inline/state_f000001.bin")).unwrap(), bios.clone()).unwrap();
    assert_eq!(middle.bus.sio0.buttons, psx_core::sio0::button::CROSS);
    let (_, last) = psx_core::Machine::load_state(&std::fs::read(root.join("inline/state.bin")).unwrap(), bios.clone()).unwrap();
    assert_eq!(last.bus.sio0.buttons, 0, "exclusive release must be reflected in the final checkpoint");
    assert_eq!(run["pad"]["buttons"],json!([]));
    assert_eq!(run["input"]["application"]["applied_events"], 2);
    assert_eq!(run["input"]["application"]["pending_events"], 0);
    let applied: Vec<Value> = std::fs::read_to_string(root.join("inline/input.jsonl")).unwrap().lines()
        .map(|line| serde_json::from_str(line).unwrap()).collect();
    assert_eq!(applied.iter().map(|e| e["run_frame"].as_u64().unwrap()).collect::<Vec<_>>(), [0,2]);
    assert_eq!(run["watch"]["enabled"],false);assert!(!root.join("inline/writes.jsonl").exists());
    let (file_run, ok) =
        cli(&root, &["run", "--bios", "bios.bin", "--replay", "input.json", "--frames", "2", "--save-state", "--out", "file"]);
    assert!(ok, "{file_run}");
    assert_eq!(file_run["input"]["events"], run["input"]["events"]);
    assert_eq!(file_run["input"]["application"]["applied_events"], run["input"]["application"]["applied_events"]);
    assert_eq!(file_run["files"]["state.bin"]["sha256"], run["files"]["state.bin"]["sha256"]);
    let (inline, ok) =
        cli(&root, &["run", "--bios", "bios.bin", "--input", &events.to_string(), "--frames", "2", "--save-state", "--out", "cli-inline"]);
    assert!(ok, "{inline}");
    assert_eq!(inline["files"]["state.bin"]["sha256"], run["files"]["state.bin"]["sha256"]);
    let mut checkpoint = psx_core::Machine::new(bios.clone());
    checkpoint.bus.frames = 37;
    std::fs::write(root.join("resume.bin"), checkpoint.save_state(None)).unwrap();
    let (resumed, error, _) = rpc.tool("run", json!({"bios":"bios.bin","from_state":"resume.bin","frames":2,
        "input":events,"save_state":true,"save_state_at":[1],"out":"resumed"}));
    assert!(!error, "{resumed}");
    let (_, held) = psx_core::Machine::load_state(&std::fs::read(root.join("resumed/state_f000001.bin")).unwrap(), bios.clone()).unwrap();
    assert_eq!(held.bus.sio0.buttons, psx_core::sio0::button::CROSS);
    assert_eq!(resumed["pad"]["buttons"], json!([]));
    let applied: Vec<Value> = std::fs::read_to_string(root.join("resumed/input.jsonl")).unwrap().lines()
        .map(|line| serde_json::from_str(line).unwrap()).collect();
    assert_eq!(applied.iter().map(|e| e["frame"].as_u64().unwrap()).collect::<Vec<_>>(), [37,39]);
    let (partial, error, _) = rpc.tool("run", json!({"bios":"bios.bin","frames":100,"timeout":0,
        "input":[{"frame":0,"buttons":["CROSS"],"until":100}],"out":"timed-out"}));
    assert!(!error && partial["termination"] == "timeout", "{partial}");
    assert_eq!(partial["input"]["application"]["applied_events"],1);
    assert_eq!(partial["input"]["application"]["pending_events"],1);
    for input in [
        json!([{"frame":0,"buttons":["JUMP"]}]),
        json!([{"frame":3,"buttons":[]}]),
        json!([{"frame":1,"buttons":[]},{"frame":0,"buttons":[]}]),
        json!([{"frame":0,"buttons":[]},{"frame":0,"buttons":[]}]),
        json!([{"frame":0,"buttons":["CROSS","cross"]}]),
        json!([{"frame":0,"buttons":[],"until":2},{"frame":1,"buttons":[]}]),
        json!([{"frame":0,"buttons":[],"until":0}]),
        json!([{"frame":0,"buttons":[],"sticks":[0,128,128,128]}]),
        json!([{"frame":0,"buttons":[],"typo":1}]),
    ] {
        let (bad, error, _) = rpc.tool("run", json!({"bios":"bios.bin","input":input,"frames":2,"out":"invalid"}));
        assert!(error && bad["error"]["code"] == "invalid_arguments", "{bad}");
        assert!(!root.join("invalid").exists(), "invalid inputs must fail before output creation");
    }
    let (bad, error, _) = rpc.tool("run", json!({"bios":"bios.bin","input":[],"replay":"input.json","frames":2}));
    assert!(error && bad["error"]["code"] == "invalid_arguments", "{bad}");
    let writer:Vec<u8>=[0x3c088001u32,0x24090001,0xad090000,0x0bf00002,0].iter().flat_map(|w|w.to_le_bytes()).collect();
    std::fs::write(root.join("writer.bin"),writer).unwrap();
    let (capped,error,_)=rpc.tool("run",json!({"bios":"writer.bin","frames":1,"watch":["80010000:4"],"max_events":2,"watch_changes_only":true,"out":"capped"}));
    assert!(!error && capped["watch"]["enabled"]==true && capped["watch"]["recorded"]==2 && capped["watch"]["emitted"]==1,"{capped}");
    assert_eq!(capped["watch"]["truncated"],true);assert_eq!(capped["watch"]["complete"],false);
    assert_eq!(std::fs::read_to_string(root.join("capped/writes.jsonl")).unwrap().lines().count(),1);
    drop(rpc);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn query_pages_preserve_totals_and_explain_scan_coverage() {
    let root = std::env::temp_dir().join(format!("psx-query-pages-{}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    let mut iso = vec![0u8; 30 * 2048];
    let record = |name: &[u8], lba: u32, size: u32, flags: u8| {
        let mut b = vec![0; (33 + name.len()).next_multiple_of(2)];
        b[0] = b.len() as u8;
        b[2..6].copy_from_slice(&lba.to_le_bytes());
        b[6..10].copy_from_slice(&lba.to_be_bytes());
        b[10..14].copy_from_slice(&size.to_le_bytes());
        b[14..18].copy_from_slice(&size.to_be_bytes());
        b[25] = flags;
        b[28] = 1;
        b[31] = 1;
        b[32] = name.len() as u8;
        b[33..33 + name.len()].copy_from_slice(name);
        b
    };
    let pvd = &mut iso[16 * 2048..17 * 2048];
    pvd[0] = 1;
    pvd[1..6].copy_from_slice(b"CD001");
    pvd[6] = 1;
    pvd[80..84].copy_from_slice(&30u32.to_le_bytes());
    pvd[128..130].copy_from_slice(&2048u16.to_le_bytes());
    pvd[156..190].copy_from_slice(&record(&[0], 20, 2048, 2));
    let tim: Vec<u8> = [16u32, 2, 20, 0, 0x00020002, 0x7fff001f, 0x03e07c00].iter().flat_map(|w| w.to_le_bytes()).collect();
    let payload = tim.repeat(3);
    let mut at = 20 * 2048;
    for i in 0..7 {
        let name = format!("FILE{i}.TIM;1");
        let r = record(name.as_bytes(), 21 + i, payload.len() as u32, 0);
        iso[at..at + r.len()].copy_from_slice(&r);
        at += r.len();
        let off = (21 + i as usize) * 2048;
        iso[off..off + payload.len()].copy_from_slice(&payload);
    }
    std::fs::write(root.join("files.iso"), iso).unwrap();
    let mut rpc = Rpc::start(&root);
    let (ls, error, _) = rpc.tool("ls", json!({"image":"files.iso","offset":2,"limit":2}));
    assert!(!error && ls["entries"].as_array().unwrap().len() == 2, "{ls}");
    assert_eq!(ls["page"], json!({"total":7,"offset":2,"returned":2,"limit":2,"truncated":true,"complete":false,"next_offset":4}));
    let (same, ok) = cli(&root, &["ls", "files.iso", "--offset", "2", "--limit", "2"]);
    assert!(ok);
    assert_eq!(same, ls);
    let (scan, error, _) = rpc.tool("assets_scan", json!({"source":"files.iso","filter":"file1","json":true,"limit":1}));
    assert!(!error && scan["totals"]["tim"] == 3, "{scan}");
    assert_eq!(scan["coverage"]["files_inspected"], 1);
    assert_eq!(scan["coverage"]["source_files"], 7);
    let (empty, error, _) =
        rpc.tool("assets_scan", json!({"source":"files.iso","filter":"missing-file","model_parser":"nonexistent-parser","json":true}));
    assert!(
        !error && empty["files_scanned"] == 0 && empty["coverage"]["selected_files"] == 0,
        "file filters must run before parser execution: {empty}"
    );
    assert_eq!(scan["files"][0]["hits"].as_array().unwrap().len(), 1);
    assert_eq!(scan["files"][0]["hits_page"]["total"], 3);
    let (next, error, _) = rpc.tool("assets_scan", json!({"source":"files.iso","filter":"file1","json":true,"limit":1,"item_offset":1}));
    assert!(!error && next["files"][0]["hits"][0]["offset"] == "0x1C", "{next}");
    let (text, error, _) = rpc.tool("assets_scan", json!({"source":"files.iso","limit":1}));
    assert!(!error && text["text"].is_string() && text.get("files").is_none(), "{text}");
    assert!(text["coverage"]["note"].as_str().unwrap().contains("custom"));
    let mut broken = std::fs::read(root.join("files.iso")).unwrap();
    let bad = record(b"BROKEN.BIN;1", 999, 2048, 0);
    broken[at..at + bad.len()].copy_from_slice(&bad);
    std::fs::write(root.join("broken.iso"), broken).unwrap();
    let (partial, error, _) = rpc.tool("assets_scan", json!({"source":"broken.iso","filter":"BROKEN","json":true}));
    assert!(
        !error && partial["coverage"]["complete"] == false && partial["coverage"]["skipped_page"]["total"] == 1,
        "failed reads must be visible: {partial}"
    );
    let mut a = vec![0u8; 2 * 1024 * 1024];
    std::fs::write(root.join("a.bin"), &a).unwrap();
    for i in 0..5 {
        a[i * 4..i * 4 + 4].copy_from_slice(&(i as u32 + 1).to_le_bytes());
    }
    std::fs::write(root.join("b.bin"), a).unwrap();
    for (tool, args, key) in [
        ("ram_diff", json!({"a":"a.bin","b":"b.bin","width":4,"limit":2,"offset":1}), "changes"),
        (
            "ram_search",
            json!({"snapshots":["a.bin","b.bin"],"width":4,"pred":["changed"],"range":"80000000-80000014","limit":2,"offset":1}),
            "matches",
        ),
    ] {
        let (r, error, _) = rpc.tool(tool, args);
        assert!(!error, "{r}");
        assert_eq!(r["page"]["total"], 5);
        assert_eq!(r["page"]["next_offset"], 3);
        assert_eq!(r[key][0]["address_hex"], "0x80000004");
        assert_eq!(r["coverage"]["complete"], true);
    }
    let mut exe = tiny_exe();
    for off in [0x800, 0x804, 0x808] {
        exe[off..off + 4].copy_from_slice(&0x0c004010u32.to_le_bytes());
    }
    exe[0x840..0x844].copy_from_slice(&0x03e00008u32.to_le_bytes());
    std::fs::write(root.join("refs.exe"), exe).unwrap();
    let (refs, error, _) = rpc.tool("xrefs", json!({"source":"refs.exe","target":"80010040","limit":1,"offset":1}));
    assert!(!error && refs["page"]["total"] == 3 && refs["refs"][0]["pc_hex"] == "0x80010004", "{refs}");
    let (text, error, _) = rpc.tool("xrefs", json!({"source":"refs.exe","target":"80010040","text":true}));
    assert!(!error && text["text"].is_string() && text.get("refs").is_none());
    let (text, error, _) = rpc.tool("disasm", json!({"source":"refs.exe","addr":"80010000","text":true}));
    assert!(!error && text["text"].is_string() && text.get("lines").is_none());
    let (spill, error, _) = rpc.tool("disasm", json!({"ram":"a.bin","addr":"80000000","count":4096}));
    assert!(!error && spill["result_file"].is_string() && spill["next_call"]["tool"] == "artifact_read", "{spill}");
    let (read, error, _) = rpc.tool("artifact_read", json!({"path":spill["result_file"],"pointer":"/lines","limit":3}));
    assert!(
        !error && read["items"].as_array().unwrap().len() == 3 && read["page"]["total"] == 4096,
        "spilled results must be navigable directly: {read}"
    );
    drop(rpc);
    std::fs::remove_dir_all(root).unwrap();
}
