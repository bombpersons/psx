//! The MCP server: JSON-RPC over stdio, one message a line, with the
//! requests as tools. Requests are answered one at a time, so the log's
//! entries keep their order.

use std::io::{BufRead, Write};
use std::process::ExitCode;

use serde_json::{Value, json};

use crate::clean::{Clean, CompareArgs, HeldoutArgs, RecordArgs, ScreensArgs, TemplateUpdateArgs};

fn schema<T: schemars::JsonSchema>() -> Value {
    let mut v = serde_json::to_value(schemars::schema_for!(T)).unwrap_or_default();
    if let Some(m) = v.as_object_mut() {
        m.remove("$schema");
        m.remove("title");
    }
    v
}

fn tools() -> Value {
    json!([
        {"name": "record", "inputSchema": schema::<RecordArgs>(), "description":
            "Record an exploratory case of the original from a published start: any input for a bounded number of \
             fields, the subject's published fields and the samples named as columns. Writes notes/oracle/NAME.json \
             and NAME.csv in a case's form (role exploratory, the published start it was recorded from as start_state) for the port's \
             own case runner to replay unchanged; they are evidence for you only and never back a spec."},
        {"name": "compare", "inputSchema": schema::<CompareArgs>(), "description":
            "Compare the port's run of a case (write_run's NAME.csv, or port) with the original's recording, over \
             the case's compared columns and tolerances: the first row and column that differ and by how much, \
             each column's differing rows and largest difference, and the first row whose tick differs."},
        {"name": "heldout", "inputSchema": schema::<HeldoutArgs>(), "description":
            "Run the port's case runner on the cases the investigator kept back, in a sandbox with no network whose \
             writes are discarded, and say only pass, fail, waiting (no spec it claims is implemented) or no_result \
             per case, with the spec each belongs to. The runner's output is not returned: this checks the spec \
             was enough, on cases you have never seen."},
        {"name": "screens", "inputSchema": schema::<ScreensArgs>(), "description":
            "The original's pictures from a published start and input, every N fields, as \
             notes/oracle/NAME/fNNNNNN.png by row; with port, a folder of the port's pictures named the same way, \
             each also side by side with the port's (original left) as fNNNNNN.side.png."},
        {"name": "next", "inputSchema": {"type": "object", "properties": {}}, "description":
            "What you do now: the ledger brought up to date and your notes/NEXT.md returned (the crossed specs \
             waiting for code, those whose cases fail, questions answered). Start every session with it."},
        {"name": "template_update", "inputSchema": schema::<TemplateUpdateArgs>(), "description":
            "Update this repository's psx-bevy template code (vendor/, psx-bevy/) to this psx, as psx template \
             update does: files the project never edited are replaced, edited ones get the new version beside them \
             as FILE.psx-new to merge, and the result lists each with what to do next. It touches only the \
             template's own code, never the game's."},
    ])
}

fn reply(out: &mut impl Write, id: &Value, result: Value) {
    let msg = json!({"jsonrpc": "2.0", "id": id, "result": result});
    let _ = writeln!(out, "{}", serde_json::to_string(&msg).unwrap_or_default());
    let _ = out.flush();
}

fn fail(out: &mut impl Write, id: &Value, code: i64, message: &str) {
    let msg = json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}});
    let _ = writeln!(out, "{}", serde_json::to_string(&msg).unwrap_or_default());
    let _ = out.flush();
}

pub fn serve(clean: &Clean) -> ExitCode {
    let stdin = std::io::stdin();
    let mut out = std::io::stdout();
    for line in stdin.lock().lines() {
        let Ok(line) = line else { break };
        if line.trim().is_empty() {
            continue;
        }
        let Ok(msg) = serde_json::from_str::<Value>(&line) else {
            fail(&mut out, &Value::Null, -32700, "parse error");
            continue;
        };
        let Some(id) = msg.get("id").cloned() else { continue }; // a notification
        let params = &msg["params"];
        match msg["method"].as_str().unwrap_or_default() {
            "initialize" => {
                let version = params["protocolVersion"].as_str().unwrap_or("2025-06-18");
                reply(
                    &mut out,
                    &id,
                    json!({
                        "protocolVersion": version,
                        "capabilities": {"tools": {}},
                        "serverInfo": {"name": "psx-clean", "version": env!("CARGO_PKG_VERSION")},
                        "instructions": "Clean mode: the original game's behaviour, in the names the investigator published. \
                            Start with next. record makes an exploratory case from a published start (specs/states/), \
                            compare checks the port's run of a case against it, heldout runs your case runner on the \
                            cases kept back, and screens gives pictures beside the port's. template_update brings the \
                            template's code up to this psx. Every request is logged to \
                            notes/oracle.jsonl, which the audit reads.",
                    }),
                );
            }
            "ping" => reply(&mut out, &id, json!({})),
            "tools/list" => reply(&mut out, &id, json!({"tools": tools()})),
            "tools/call" => {
                let name = params["name"].as_str().unwrap_or_default();
                let args = params.get("arguments").cloned().filter(Value::is_object).unwrap_or(json!({}));
                let (text, error) = match clean.call(name, args) {
                    Ok(v) => (serde_json::to_string_pretty(&v).unwrap_or_default(), false),
                    Err(e) => (json!({"error": e}).to_string(), true),
                };
                reply(&mut out, &id, json!({"content": [{"type": "text", "text": text}], "isError": error}));
            }
            _ => fail(&mut out, &id, -32601, "method not found"),
        }
    }
    ExitCode::SUCCESS
}
