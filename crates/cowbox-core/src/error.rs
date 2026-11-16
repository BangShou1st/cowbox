use std::path::PathBuf;
use std::time::Duration;

#[derive(Debug, thiserror::Error)]
pub enum CoreError {
    #[error("copy-on-write primitive unavailable at {path}: {reason}")]
    CowUnsupported { path: PathBuf, reason: String },
    #[error("sandbox isolation failed: {0}")]
    IsolationFailed(String),
    #[error("user namespace setup failed: {0}")]
    UserNamespace(String),
    #[error("network namespace setup failed: {0}")]
    NetworkNamespace(String),
    #[error("OverlayFS setup failed: {0}")]
    OverlayFilesystem(String),
    #[error("Landlock setup failed: {0}")]
    Landlock(String),
    #[error("session state corrupted at step {step}: {reason}")]
    StateCorrupted { step: u64, reason: String },
    #[error("commit conflict for {0:?}")]
    CommitConflict(Vec<PathBuf>),
    #[error("commit failed: {0}")]
    CommitFailed(String),
    #[error("commit recovery required for journal {0}")]
    CommitRecoveryRequired(PathBuf),
    #[error("command timed out after {0:?}")]
    Timeout(Duration),
    #[error("invalid configuration: {0}")]
    Config(String),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
}

impl CoreError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::CowUnsupported { .. } => "COW_UNSUPPORTED",
            Self::IsolationFailed(_) => "ISOLATION_FAILED",
            Self::UserNamespace(_) => "USER_NAMESPACE_FAILED",
            Self::NetworkNamespace(_) => "NETWORK_NAMESPACE_FAILED",
            Self::OverlayFilesystem(_) => "OVERLAYFS_FAILED",
            Self::Landlock(_) => "LANDLOCK_FAILED",
            Self::StateCorrupted { .. } => "STATE_CORRUPTED",
            Self::CommitConflict(_) => "COMMIT_CONFLICT",
            Self::CommitFailed(_) => "COMMIT_FAILED",
            Self::CommitRecoveryRequired(_) => "COMMIT_RECOVERY_REQUIRED",
            Self::Timeout(_) => "TIMEOUT",
            Self::Config(_) => "CONFIG_ERROR",
            Self::Io(_) => "IO_ERROR",
            Self::Json(_) => "JSON_ERROR",
        }
    }
}

pub type Result<T> = std::result::Result<T, CoreError>;
