//! Supervised external parser protocol: bounded stdin bytes, versioned JSON on
//! stdout, diagnostics on stderr. Executables are launched directly, not through
//! a shell. They run with the caller's normal permissions, not in a new sandbox.

use crate::{Error, Result};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::io::{Read, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};
use tokio_util::sync::CancellationToken;

pub(crate) fn default_parser_timeout() -> u64 {
    30
}

const OUTPUT_LIMIT: usize = 16 * 1024 * 1024;
const STDERR_LIMIT: usize = 64 * 1024;

#[derive(Clone, Serialize)]
pub struct Spec {
    pub program: PathBuf,
    pub args: Vec<String>,
    pub timeout: u64,
}

impl Spec {
    pub fn new(program: Option<&PathBuf>, args: &[String], timeout: u64) -> Result<Option<Self>> {
        if !(1..=300).contains(&timeout) {
            return Err(Error::invalid("parser timeout must be 1..=300 seconds"));
        }
        match program {
            Some(program) => Ok(Some(Self { program: program.clone(), args: args.to_vec(), timeout })),
            None if args.is_empty() => Ok(None),
            None => Err(Error::invalid("parser arguments require a parser executable")),
        }
    }
}

#[derive(Default)]
pub struct Parsers {
    pub archive: Option<Spec>,
    pub model: Option<Spec>,
    pub asset: Option<Spec>,
    pub cancel: Option<CancellationToken>,
}

pub struct Output {
    pub bytes: Vec<u8>,
    pub provenance: Value,
}

pub fn run(
    spec: &Spec,
    bytes: &[u8],
    kind: &str,
    context: &[(&str, String)],
    cancel: Option<&CancellationToken>,
) -> Result<Output> {
    if !cfg!(unix) {
        return Err(Error::unsupported("external parser supervision currently requires Unix"));
    }
    if bytes.len() > 512 * 1024 * 1024 {
        return Err(Error::new("limit_exceeded", "parser input exceeds 512 MiB"));
    }
    if cancel.is_some_and(|c| c.is_cancelled()) {
        return Err(Error::new("cancelled", "parser cancelled before launch"));
    }
    let mut command = Command::new(&spec.program);
    command
        .args(&spec.args)
        .env("PSX_PARSER_PROTOCOL", "1")
        .env("PSX_PARSER_KIND", kind)
        .envs(context.iter().map(|(k, v)| (*k, v)))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    let mut child =
        command.spawn().map_err(|e| Error::parser(format!("cannot launch parser {}: {e}", spec.program.display())))?;
    let input = child.stdin.take().unwrap();
    let output = child.stdout.take().unwrap();
    let error = child.stderr.take().unwrap();
    let overflow = AtomicBool::new(false);
    let deadline = Instant::now() + Duration::from_secs(spec.timeout);
    let result = std::thread::scope(|scope| {
        let writer = scope.spawn(move || {
            let mut input = input;
            input.write_all(bytes)
        });
        let collect = |reader: Box<dyn Read + Send>, limit| {
            let mut result = Vec::new();
            let read = reader.take(limit as u64 + 1).read_to_end(&mut result);
            if result.len() > limit {
                overflow.store(true, Ordering::Release);
            }
            read.map(|_| result)
        };
        let stdout = scope.spawn(move || collect(Box::new(output), OUTPUT_LIMIT));
        let stderr = scope.spawn(move || collect(Box::new(error), STDERR_LIMIT));
        let mut status = None;
        let outcome = loop {
            if overflow.load(Ordering::Acquire) {
                break Err(Error::parser("parser stdout exceeds 16 MiB or stderr exceeds 64 KiB"));
            }
            if cancel.is_some_and(|c| c.is_cancelled()) {
                break Err(Error::new("cancelled", "parser cancelled"));
            }
            if Instant::now() >= deadline {
                break Err(Error::parser(format!("parser timed out after {} seconds", spec.timeout)));
            }
            if status.is_none() {
                match child.try_wait() {
                    Ok(s) => status = s,
                    Err(e) => break Err(Error::parser(format!("parser wait failed: {e}"))),
                }
            }
            if let Some(status) = status {
                if stdout.is_finished() && stderr.is_finished() && writer.is_finished() {
                    break Ok(status);
                }
            }
            std::thread::sleep(Duration::from_millis(10));
        };
        // Also stop ordinary descendants after the main executable exits.
        #[cfg(unix)]
        {
            let _ = Command::new("kill")
                .args(["-KILL", "--", &format!("-{}", child.id())])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
        }
        let _ = child.kill();
        let _ = child.wait();
        let output = stdout
            .join()
            .map_err(|_| Error::parser("parser stdout reader failed"))?
            .map_err(|e| Error::parser(format!("parser stdout: {e}")))?;
        let diagnostics = stderr
            .join()
            .map_err(|_| Error::parser("parser stderr reader failed"))?
            .map_err(|e| Error::parser(format!("parser stderr: {e}")))?;
        let written = writer.join().map_err(|_| Error::parser("parser stdin writer failed"))?;
        let status = outcome?;
        if overflow.load(Ordering::Acquire) {
            return Err(Error::parser("parser output limit exceeded"));
        }
        if !status.success() {
            return Err(Error::parser(format!(
                "parser exited {status}: {}",
                String::from_utf8_lossy(&diagnostics[..diagnostics.len().min(4096)])
            )));
        }
        written.map_err(|e| Error::parser(format!("parser did not accept its input: {e}")))?;
        Ok((output, diagnostics))
    })?;
    let provenance = json!({"protocol_version":1,"executable":spec,"input_sha256":psx_media::disc::sha256_bytes(bytes),
        "output_sha256":psx_media::disc::sha256_bytes(&result.0),"stderr":String::from_utf8_lossy(&result.1)});
    Ok(Output { bytes: result.0, provenance })
}

pub fn document<T: serde::de::DeserializeOwned>(bytes: &[u8]) -> Result<T> {
    serde_json::from_slice(bytes).map_err(|e| Error::parser(format!("invalid parser JSON: {e}")))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArchiveDocument {
    pub version: u32,
    pub entries: Vec<ArchiveEntry>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArchiveEntry {
    pub offset: usize,
    pub size: usize,
    pub name: Option<String>,
}

impl ArchiveDocument {
    pub fn validate(self, source_len: usize) -> Result<psx_assets::archive::Archive> {
        if self.version != 1 {
            return Err(Error::parser("unsupported archive parser protocol version"));
        }
        let entries = self
            .entries
            .into_iter()
            .enumerate()
            .map(|(index, e)| psx_assets::archive::Entry { index, name: e.name, offset: e.offset, size: e.size })
            .collect();
        psx_assets::archive::Archive::external(entries, source_len).map_err(Error::parser)
    }
}

/// Wrapper adapters emit one standard TIM/TMD/VAB/VAG or BS payload. Original
/// bytes are retained separately, so transformed offsets are not misreported.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PayloadDocument {
    pub version: u32,
    pub data_base64: String,
}

impl PayloadDocument {
    pub fn decode(self) -> Result<Vec<u8>> {
        use base64::Engine;
        if self.version != 1 {
            return Err(Error::parser("unsupported payload parser protocol version"));
        }
        base64::engine::general_purpose::STANDARD
            .decode(&self.data_base64)
            .map_err(|e| Error::parser(format!("invalid parser base64: {e}")))
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    fn spec(code: &str, timeout: u64) -> Spec {
        Spec { program: "python3".into(), args: vec!["-c".into(), code.into()], timeout }
    }

    #[test]
    fn direct_arguments_bytes_diagnostics_and_hashes() {
        let mut configured =
            spec("import sys; data=sys.stdin.buffer.read(); print(sys.argv[1]); print(len(data),file=sys.stderr)", 2);
        configured.args.push("$(touch /tmp/should-never-execute) ; *".into());
        let output = run(&configured, &[0, 255, 0], "model", &[], None).unwrap();
        assert_eq!(String::from_utf8(output.bytes).unwrap().trim(), configured.args[2]);
        assert_eq!(output.provenance["stderr"], "3\n");
        assert_eq!(output.provenance["input_sha256"], psx_media::disc::sha256_bytes(&[0, 255, 0]));
        let error =
            run(&spec("import sys; print('bad format',file=sys.stderr); sys.exit(7)", 2), &[], "asset", &[], None)
                .err()
                .unwrap()
                .to_string();
        assert!(error.contains("bad format") && error.contains('7'), "{error}");
    }

    #[test]
    fn rejects_bad_documents_and_payloads() {
        for bytes in [b"not JSON".as_slice(), br#"{"version":1,"data_base64":"","typo":0}"#] {
            assert!(document::<PayloadDocument>(bytes).is_err());
        }
        for (version, data_base64) in [(2, ""), (1, "!!!!")] {
            assert!(PayloadDocument { version, data_base64: data_base64.into() }.decode().is_err());
        }
        assert!(ArchiveDocument { version: 2, entries: vec![] }.validate(0).is_err());
    }

    #[test]
    fn stops_timeout_overflow_and_cancellation() {
        // The descendant inherits pipes and process group; supervision must
        // terminate it as well as the parent, without blocking reader joins.
        let start = Instant::now();
        let err = run(
            &spec("import subprocess,time; subprocess.Popen(['sleep','30']); time.sleep(30)", 1),
            &[],
            "model",
            &[],
            None,
        )
        .err()
        .unwrap()
        .to_string();
        assert!(err.contains("timed out"), "{err}");
        assert!(start.elapsed() < Duration::from_secs(5));
        for code in [
            "import sys; sys.stdout.buffer.write(b'x'*(17*1024*1024))",
            "import sys; sys.stderr.buffer.write(b'x'*70000)",
        ] {
            let error = run(&spec(code, 2), &[], "asset", &[], None).err().unwrap().to_string();
            assert!(error.contains("limit") || error.contains("exceeds"), "{error}");
        }
        let cancel = CancellationToken::new();
        let other = cancel.clone();
        let thread = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(100));
            other.cancel();
        });
        let error =
            run(&spec("import time; time.sleep(30)", 3), &[], "asset", &[], Some(&cancel)).err().unwrap().to_string();
        thread.join().unwrap();
        assert!(error.contains("cancelled"), "{error}");
    }
}
