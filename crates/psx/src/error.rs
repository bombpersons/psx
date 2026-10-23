//! The one error every command fails with. It prints as
//! `{"error": {"code", "message", ...}}`; `code` is stable for callers to
//! branch on, `message` is for people, and some errors add fields (a path,
//! BIOS candidates, a next action).

use std::fmt;
use std::path::Path;

use psx_media::MediaError;
use serde::Serialize;
use serde_json::{Map, Value, json};

pub type Result<T, E = Error> = std::result::Result<T, E>;

#[derive(Debug)]
pub struct Error {
    pub code: &'static str,
    pub message: String,
    /// Fields beside `code` and `message`.
    detail: Map<String, Value>,
    /// Process exit code: 1, or 2 for well-formed media this build cannot read.
    pub exit: u8,
}

impl Error {
    pub fn new(code: &'static str, message: impl fmt::Display) -> Self {
        Error { code, message: message.to_string(), detail: Map::new(), exit: 1 }
    }

    /// A missing, conflicting or out-of-range argument.
    pub fn invalid(message: impl fmt::Display) -> Self {
        Self::new("invalid_arguments", message)
    }

    /// An input file whose contents are not what the command needs.
    pub fn malformed(message: impl fmt::Display) -> Self {
        Self::new("malformed_input", message)
    }

    /// Well-formed media using a feature this build cannot read (exit code 2).
    pub fn unsupported(message: impl fmt::Display) -> Self {
        Error { exit: 2, ..Self::new("unsupported_format", message) }
    }

    /// An external parser that could not run or returned an unusable result.
    pub fn parser(message: impl fmt::Display) -> Self {
        Self::new("parser_failed", message)
    }

    /// A file operation on `path` that failed; the error carries the absolute path.
    pub fn io(path: impl AsRef<Path>, e: impl fmt::Display) -> Self {
        let path = path.as_ref();
        Self::new("io_error", format!("{}: {e}", path.display()))
            .with("path", std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf()))
    }

    /// Add a field beside `code` and `message`.
    pub fn with(mut self, key: &str, value: impl Serialize) -> Self {
        self.detail.insert(key.into(), json!(value));
        self
    }

    pub fn to_value(&self) -> Value {
        let mut error = Map::new();
        error.insert("code".into(), json!(self.code));
        error.insert("message".into(), json!(self.message));
        error.extend(self.detail.clone());
        json!({"error": error})
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl From<MediaError> for Error {
    fn from(e: MediaError) -> Self {
        match &e {
            MediaError::Io { path, source } => Error::io(path, source),
            MediaError::Unsupported(_) => Error::unsupported(&e),
            _ => Error::new(e.code(), &e),
        }
    }
}

/// An I/O failure with no single path to name.
impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Error::new("io_error", e)
    }
}
