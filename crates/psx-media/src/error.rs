use std::fmt;
use std::path::PathBuf;

pub type Result<T> = std::result::Result<T, MediaError>;

#[derive(Debug)]
pub enum MediaError {
    Io { path: PathBuf, source: std::io::Error },
    /// Well-formed input using a feature this crate does not implement.
    Unsupported(String),
    /// Input that violates its format.
    Malformed(String),
    /// A requested file, sector or track does not exist.
    NotFound(String),
}

impl MediaError {
    pub fn io(path: impl Into<PathBuf>, source: std::io::Error) -> Self {
        MediaError::Io { path: path.into(), source }
    }

    pub fn code(&self) -> &'static str {
        match self {
            MediaError::Io { .. } => "io_error",
            MediaError::Unsupported(_) => "unsupported_format",
            MediaError::Malformed(_) => "malformed_input",
            MediaError::NotFound(_) => "not_found",
        }
    }
}

impl fmt::Display for MediaError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            MediaError::Io { path, source } => write!(f, "{}: {source}", path.display()),
            MediaError::Unsupported(m) => write!(f, "unsupported: {m}"),
            MediaError::Malformed(m) => write!(f, "malformed: {m}"),
            MediaError::NotFound(m) => write!(f, "not found: {m}"),
        }
    }
}

impl std::error::Error for MediaError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            MediaError::Io { source, .. } => Some(source),
            _ => None,
        }
    }
}

pub(crate) fn malformed<T>(msg: impl Into<String>) -> Result<T> {
    Err(MediaError::Malformed(msg.into()))
}
