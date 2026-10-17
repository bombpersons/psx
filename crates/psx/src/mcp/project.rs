//! Explicit per-request project roots without changing the MCP server's process CWD.
//! Closing the worker's input cooperatively cancels supervised descendants first.
use super::{error_value, Ctx, Outcome, ARGS_LIMIT};
use serde_json::{json, Value};
use std::{
    io::{BufRead, Read, Write},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};

pub(super) fn resolve(default: &Path, value: Option<&Value>) -> Result<PathBuf, Value> {
    let Some(value) = value else {
        return Ok(default.to_owned());
    };
    let path = value.as_str().filter(|p| !p.is_empty()).ok_or_else(|| {
        error_value(
            "invalid_arguments",
            "project_root must be a nonempty directory path",
        )
    })?;
    let absolute = default.join(path);
    let root = absolute.canonicalize().map_err(|e| {
        error_value(
            "io_error",
            format!(
                "project_root {} (base {}): {e}",
                absolute.display(),
                default.display()
            ),
        )
    })?;
    if !root.is_dir() {
        return Err(error_value(
            "invalid_arguments",
            format!("project_root {} is not a directory", root.display()),
        ));
    }
    Ok(root)
}

/// The client's single local MCP root, if it advertises roots and lists exactly
/// one existing directory. Anything else keeps the server's root.
// MCP roots are deprecated by SEP-2577 but still sent by current clients; the
// server's working directory remains the default when they disappear.
#[allow(deprecated)]
pub(super) async fn client_root(
    peer: &rmcp::service::Peer<rmcp::service::RoleServer>,
) -> Option<PathBuf> {
    peer.peer_info()?.capabilities.roots.as_ref()?;
    let roots = match tokio::time::timeout(Duration::from_secs(5), peer.list_roots()).await {
        Ok(Ok(listed)) => listed.roots,
        Ok(Err(e)) => {
            eprintln!("psx mcp: roots/list failed ({e}); using the server root");
            return None;
        }
        Err(_) => {
            eprintln!("psx mcp: roots/list timed out; using the server root");
            return None;
        }
    };
    let [root] = roots.as_slice() else {
        eprintln!(
            "psx mcp: client reported {} roots; using the server root",
            roots.len()
        );
        return None;
    };
    let dir = file_uri_path(&root.uri)
        .and_then(|p| p.canonicalize().ok())
        .filter(|p| p.is_dir());
    if dir.is_none() {
        eprintln!(
            "psx mcp: client root {:?} is not a local directory; using the server root",
            root.uri
        );
    }
    dir
}

/// A local `file://` URI as a path, percent-decoded. Other hosts and schemes are rejected.
pub(super) fn file_uri_path(uri: &str) -> Option<PathBuf> {
    let rest = uri.strip_prefix("file://")?;
    let path = rest.strip_prefix("localhost").unwrap_or(rest);
    if !path.starts_with('/') {
        return None;
    }
    let (bytes, mut out, mut i) = (path.as_bytes(), Vec::with_capacity(path.len()), 0);
    while i < bytes.len() {
        if bytes[i] == b'%' {
            out.push(
                u8::from_str_radix(std::str::from_utf8(bytes.get(i + 1..i + 3)?).ok()?, 16).ok()?,
            );
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    #[cfg(unix)]
    return Some(PathBuf::from(
        <std::ffi::OsString as std::os::unix::ffi::OsStringExt>::from_vec(out),
    ));
    #[cfg(not(unix))]
    {
        // file:///C:/dir -> C:/dir
        let text = String::from_utf8(out).ok()?;
        let drive = text.as_bytes().get(2) == Some(&b':');
        Some(PathBuf::from(if drive { &text[1..] } else { &text[..] }))
    }
}

struct Worker(Child);
impl Drop for Worker {
    fn drop(&mut self) {
        self.0.stdin.take();
        if self.0.try_wait().ok().flatten().is_some() {
            return;
        }
        // Ordinary descendants inherit this process group. Ghidra and external
        // parsers have their own supervisors; stdin EOF signals their token.
        #[cfg(unix)]
        let _ = Command::new("kill")
            .args(["-KILL", "--", &format!("-{}", self.0.id())])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

pub(super) fn call(tool: &str, args: Value, ctx: &Ctx) -> Result<Value, String> {
    let mut command = Command::new(std::env::current_exe().map_err(|e| e.to_string())?);
    command
        .env_remove("PSX_ROOT")
        .current_dir(&ctx.root)
        .arg("__mcp-call")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    let mut child = Worker(
        command
            .spawn()
            .map_err(|e| format!("project worker in {}: {e}", ctx.root.display()))?,
    );
    writeln!(
        child.0.stdin.as_mut().unwrap(),
        "{}",
        json!({"tool":tool,"arguments":args})
    )
    .map_err(|e| e.to_string())?;
    let output = child.0.stdout.take().unwrap();
    let reader = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        output
            .take(64 * 1024 * 1024 + 1)
            .read_to_end(&mut bytes)
            .map(|_| bytes)
    });
    let mut cancelled_at = None;
    let status = loop {
        if ctx.cancel.as_ref().is_some_and(|c| c.is_cancelled()) {
            if cancelled_at.is_none() {
                child.0.stdin.take();
                cancelled_at = Some(Instant::now());
            }
            if cancelled_at.is_some_and(|at| at.elapsed() >= Duration::from_secs(2)) {
                return Err("project worker cancelled; termination requested".into());
            }
        }
        if let Some(status) = child.0.try_wait().map_err(|e| e.to_string())? {
            break status;
        }
        if reader.is_finished() {
            // Also handles a worker exceeding the output cap without blocking
            // forever on a full stdout pipe.
            if let Some(status) = child.0.try_wait().map_err(|e| e.to_string())? {
                break status;
            }
            child.0.kill().map_err(|e| e.to_string())?;
            break child.0.wait().map_err(|e| e.to_string())?;
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    let bytes = reader
        .join()
        .map_err(|_| "project worker output reader failed")?
        .map_err(|e| e.to_string())?;
    if bytes.len() > 64 * 1024 * 1024 {
        return Err("project worker response exceeds 64 MiB".into());
    }
    let mut value: Value = serde_json::from_slice(&bytes)
        .map_err(|e| format!("project worker exited {status} without a valid result: {e}"))?;
    if value.is_object() {
        value["project_root"] = json!(ctx.root);
    }
    propagate_root(&mut value, &ctx.root);
    Ok(value)
}

fn propagate_root(value: &mut Value, root: &Path) {
    match value {
        Value::Object(map) => {
            // Concrete continuation calls must retain the project selection.
            if map
                .get("tool")
                .and_then(Value::as_str)
                .is_some_and(|name| super::tools().iter().any(|t| t.name == name))
                && map.get("arguments").is_some_and(Value::is_object)
            {
                map.get_mut("arguments").unwrap()["project_root"] = json!(root);
            }
            for child in map.values_mut() {
                propagate_root(child, root);
            }
        }
        Value::Array(values) => values.iter_mut().for_each(|v| propagate_root(v, root)),
        _ => {}
    }
}

/// Private CLI entry point used by the MCP adapter. No shell interpolation or
/// persistent configuration is involved; it dispatches the same typed Op.
pub(crate) fn worker(ctx: &Ctx) -> Outcome {
    fn run(ctx: &Ctx) -> Result<Outcome, String> {
        let mut input = std::io::BufReader::new(std::io::stdin()).take(ARGS_LIMIT as u64 + 4096);
        let mut line = String::new();
        input.read_line(&mut line).map_err(|e| e.to_string())?;
        if !line.ends_with('\n') {
            return Err("project worker request exceeds limit or is incomplete".into());
        }
        let request: Value = serde_json::from_str(&line).map_err(|e| e.to_string())?;
        let name = request["tool"]
            .as_str()
            .ok_or("project worker request needs a tool")?;
        let tool = super::tools()
            .into_iter()
            .find(|t| t.name == name)
            .ok_or("unknown project worker tool")?;
        let token = tokio_util::sync::CancellationToken::new();
        let cancellation = token.clone();
        std::thread::spawn(move || {
            let _ = std::io::copy(&mut input, &mut std::io::sink());
            cancellation.cancel();
        });
        let ctx = Ctx {
            root: ctx.root.clone(),
            cancel: Some(token),
        };
        let mut result = (tool.call)(request["arguments"].clone(), &ctx)?;
        result.text = false; // The private transport always carries JSON, even for text-mode tools.
        if result.value["error"].is_object() {
            result.value["error"]["project_root"] = json!(ctx.root);
            result.value["error"]["path_resolution"] =
                json!("Relative paths are resolved against project_root, not the client working directory.");
        }
        Ok(result)
    }
    run(ctx).unwrap_or_else(|error| Outcome {
        value: error_value("invalid_arguments", error),
        exit: 1,
        pretty: false,
        text: false,
    })
}

#[cfg(test)]
mod tests {
    use super::file_uri_path;

    #[cfg(unix)]
    #[test]
    fn file_uris_decode_to_local_paths_only() {
        assert_eq!(
            file_uri_path("file:///home/a%20b/c%25"),
            Some("/home/a b/c%".into())
        );
        assert_eq!(file_uri_path("file://localhost/srv"), Some("/srv".into()));
        for uri in [
            "file://other/srv",
            "https://x/y",
            "file:relative",
            "file:///bad%2",
            "file:///bad%zz",
        ] {
            assert_eq!(file_uri_path(uri), None, "{uri}");
        }
    }
}
