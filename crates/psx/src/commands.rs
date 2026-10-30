//! What every command shares: where it runs ([`Ctx`]), what it returns
//! ([`Outcome`]) and the [`Op`] trait the CLI and the MCP server call.
//!
//! Each command's argument type lives with its implementation (`media_cmd`,
//! `run`, `ram`, ...) and serves both front ends: clap reads it from flags,
//! serde and JSON Schema from MCP arguments. Field names are the CLI long
//! flags with `_` for `-` (`--trace-cd` is `trace_cd`); repeatable flags are
//! arrays. Positional CLI arguments are plain fields.

use std::path::PathBuf;

use crate::Error;
use schemars::JsonSchema;
use serde_json::{Value, json};

/// Where a command runs: `root` is the project directory (bios/, media/,
/// external/, .psx/). Relative paths in arguments resolve against the process
/// working directory, which for both the CLI and `psx mcp` is that root
/// (`--root`/`PSX_ROOT` change into it before any command runs).
pub struct Ctx {
    pub root: PathBuf,
    /// Cooperative cancellation for supervised workers.
    pub cancel: Option<tokio_util::sync::CancellationToken>,
}

/// A command's JSON result and how the CLI prints it.
pub struct Outcome {
    pub value: Value,
    /// Process exit code (0 success; 1 failure, partial run or divergence; 2 unsupported media).
    pub exit: u8,
    /// Plain-text mode (`--text`): the CLI prints `value["text"]` as is; MCP
    /// still returns the whole JSON value (with the text in its `text` field).
    pub text: bool,
}

impl Outcome {
    pub(crate) fn with_path_context(mut self, ctx: &Ctx) -> Self {
        if self.value["error"]["code"] == "io_error" {
            self.value["error"]["project_root"] = json!(ctx.root);
            self.value["error"]["path_resolution"] =
                json!("Relative paths are resolved against project_root, not the client working directory.");
        }
        self
    }

    pub(crate) fn ok(value: Value) -> Self {
        Outcome { value, exit: 0, text: false }
    }

    pub(crate) fn result(r: Result<Value, impl Into<Error>>) -> Self {
        match r {
            Ok(value) => Self::ok(value),
            Err(e) => e.into().into(),
        }
    }

    /// A report that may describe a partial run (`false`: exit 1, report printed as usual).
    pub(crate) fn report(r: crate::Result<(bool, Value)>) -> Self {
        match r {
            Ok((true, value)) => Self::ok(value),
            Ok((false, value)) => Outcome { exit: 1, ..Self::ok(value) },
            Err(e) => e.into(),
        }
    }

    /// A result printed as plain text (its `text` field) when `text` is set.
    pub(crate) fn result_text(r: Result<Value, impl Into<Error>>, text: bool) -> Self {
        let mut o = Self::result(r);
        o.text = text && o.exit == 0;
        o
    }

    /// The CLI's stdout text (without the trailing newline): errors on one
    /// line, results pretty-printed.
    pub fn render(&self) -> String {
        if self.text {
            return self.value["text"].as_str().unwrap_or_default().trim_end().to_string();
        }
        if self.value.get("error").is_some() {
            self.value.to_string()
        } else {
            serde_json::to_string_pretty(&self.value).unwrap()
        }
    }
}

impl From<Error> for Outcome {
    fn from(e: Error) -> Self {
        Outcome { value: e.to_value(), exit: e.exit, text: false }
    }
}

/// A command usable from both front ends.
pub trait Op: serde::de::DeserializeOwned + JsonSchema {
    fn exec(self, ctx: &Ctx) -> Outcome;
}

/// An address field psx wrote: hex (with or without `0x`), or a number from
/// files written before addresses were hex.
pub(crate) fn addr_value(v: &serde_json::Value) -> Option<u32> {
    v.as_u64().and_then(|n| u32::try_from(n).ok()).or_else(|| v.as_str().and_then(psx_media::memmap::parse_hex))
}

/// An optional hex argument (`what` names it in the error).
pub(crate) fn opt_hex(s: &Option<String>, what: &str) -> crate::Result<Option<u32>> {
    s.as_deref()
        .map(|a| psx_media::memmap::parse_hex(a).ok_or_else(|| Error::invalid(format!("bad {what} {a:?} (hex)"))))
        .transpose()
}
