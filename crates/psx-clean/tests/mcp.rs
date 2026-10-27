//! psx-clean over MCP on a made-up port: requests by unpublished names are
//! refused, compare finds the first difference, and every request is logged
//! in the implementer's notes/oracle.jsonl, chained.

use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Command, Stdio};

use serde_json::{Value, json};

fn put(p: &Path, text: &str) {
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, text).unwrap();
}

#[test]
fn requests_by_published_names_only_and_every_one_logged() {
    let root = std::env::temp_dir().join(format!("psx-clean-test-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let (inv, imp) = (root.join("inv"), root.join("imp"));
    put(
        &inv.join("protocol.toml"),
        &format!("[repositories]\ninvestigator = \".\"\nimplementer = {:?}\n", imp.display().to_string()),
    );
    put(&imp.join("specs/glossary.md"), "# Glossary\n\n- **player**: the subject.\n- **x**: across.\n");
    put(&imp.join("specs/SPEC-0001-walking.md"), "---\nid: SPEC-0001\ntitle: Walking\n---\n\nWalking.\n");
    put(&imp.join("specs/states/start.json"), "{\"name\": \"start\", \"description\": \"the first room\"}\n");
    put(
        &imp.join("specs/cases/walk.json"),
        "{\"id\": \"walk\", \"spec\": \"SPEC-0001\", \"subject\": \"player\", \"observations\": \"walk.csv\", \"compare\": {\"x\": 0, \"y\": 1}}\n",
    );
    put(&imp.join("specs/cases/walk.csv"), "run_frame,frame,tick,x,y\n0,100,0,10,5\n1,101,1,12,5\n2,102,2,14,6\n");
    put(&imp.join("target/tmp/cases/walk.json"), "{\"id\": \"walk\", \"recorded_from\": \"rebuild\"}\n");
    put(&imp.join("target/tmp/cases/walk.csv"), "run_frame,frame,tick,x,y\n0,0,0,10,5\n1,1,1,12,7\n2,2,2,15,6\n");

    let mut child = Command::new(env!("CARGO_BIN_EXE_psx-clean"))
        .args(["--investigator", inv.to_str().unwrap(), "mcp"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let mut lines = BufReader::new(child.stdout.take().unwrap()).lines();
    let mut id = 0;
    let mut call = |method: &str, params: Value| -> Value {
        id += 1;
        writeln!(stdin, "{}", json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params})).unwrap();
        serde_json::from_str(&lines.next().unwrap().unwrap()).unwrap()
    };
    let init = call("initialize", json!({"protocolVersion": "2025-06-18", "capabilities": {}}));
    assert_eq!(init["result"]["serverInfo"]["name"], "psx-clean");
    let tools = call("tools/list", json!({}));
    let names: Vec<&str> =
        tools["result"]["tools"].as_array().unwrap().iter().map(|t| t["name"].as_str().unwrap()).collect();
    assert_eq!(names, ["record", "compare", "heldout", "screens", "next", "template_update"]);

    let tool = |call: &mut dyn FnMut(&str, Value) -> Value, name: &str, args: Value| -> (bool, Value) {
        let r = call("tools/call", json!({"name": name, "arguments": args}));
        let text = r["result"]["content"][0]["text"].as_str().unwrap().to_string();
        (r["result"]["isError"] == json!(true), serde_json::from_str(&text).unwrap())
    };
    let record = |sample: &str, start: &str| json!({"name": "probe", "start": start, "frames": 10, "subject": "player", "sample": [sample], "spec": "SPEC-0001"});
    // An address, an unpublished name, a start that never crossed.
    let (err, v) = tool(&mut call, "record", record("80012345:u32:x", "start"));
    assert!(err && v["error"].as_str().unwrap().contains("not a published name"), "{v}");
    let (err, v) = tool(&mut call, "record", record("speed", "start"));
    assert!(err && v["error"].as_str().unwrap().contains("\"speed\" is not a published name"), "{v}");
    let (err, v) = tool(&mut call, "record", record("x", "secret_room"));
    assert!(err && v["error"].as_str().unwrap().contains("not a published start (start)"), "{v}");
    // Published, but not built on the investigator's side.
    let (err, v) = tool(&mut call, "record", record("x", "start"));
    assert!(err && v["error"].as_str().unwrap().contains("not built"), "{v}");

    // y differs on row 1 beyond its tolerance; x on row 2.
    let (err, v) = tool(&mut call, "compare", json!({"case": "walk"}));
    assert!(!err, "{v}");
    assert_eq!(v["same"], false);
    assert_eq!(v["first_difference"]["row"], 1);
    assert_eq!(v["first_difference"]["column"], "y");
    assert_eq!(v["first_difference"]["by"], 2.0);
    assert_eq!(v["columns"]["x"]["first_row"], 2);
    assert_eq!(v["first_tick_differing"], Value::Null);
    // Only files in the port's repository.
    let (err, v) = tool(&mut call, "compare", json!({"case": "walk", "port": inv.join("protocol.toml")}));
    assert!(err && v["error"].as_str().unwrap().contains("not in this repository"), "{v}");
    let (err, _) = tool(&mut call, "compare", json!({"case": "../../inv/x"}));
    assert!(err);
    // A port that is not a template project (no .psx-template.toml).
    let (err, v) = tool(&mut call, "template_update", json!({"dry_run": true}));
    assert!(err, "{v}");
    drop(stdin);
    child.wait().unwrap();

    let log = std::fs::read_to_string(imp.join("notes/oracle.jsonl")).unwrap();
    let entries: Vec<&str> = log.lines().collect();
    assert_eq!(entries.len(), 8);
    let mut prev = Value::Null;
    for line in &entries {
        let v: Value = serde_json::from_str(line).unwrap();
        assert_eq!(v["prev"], prev);
        use sha2::Digest;
        prev = json!(format!("{:x}", sha2::Sha256::digest(line.as_bytes())));
    }
    let compare: Value = serde_json::from_str(entries[4]).unwrap();
    assert_eq!(compare["tool"], "compare");
    assert_eq!(compare["result"]["first_difference"]["column"], "y");
    // Its working folders are gone.
    assert!(std::fs::read_dir(inv.join(".psx/clean")).map_or(true, |d| d.count() == 0));
    let _ = std::fs::remove_dir_all(&root);
}
