use std::path::PathBuf;
use std::time::Duration;

#[derive(Debug, thiserror::Error)]
pub enum CoreError {
    #[error("copy-on-write primitive unavailable at {path}: {reason}")]
    CowUnsupported { path: PathBuf, reason: String },
    #[error("sandbox isolation failed: {0}")]
    IsolationFailed(String),
    #[error("Landlock setup failed: {0}")]
    Landlock(String),
    #[error("session state corrupted at step {step}: {reason}")]
    StateCorrupted { step: u64, reason: String },
    #[error("commit conflict for {0:?}")]
    CommitConflict(Vec<PathBuf>),
    #[error("commit failed: {0}")]
    CommitFailed(String),
    #[error("command timed out after {0:?}")]
    Timeout(Duration),
    #[error("invalid configuration: {0}")]
    Config(String),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
}

pub type Result<T> = std::result::Result<T, CoreError>;
