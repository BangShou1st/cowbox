use crate::error::{CliError, CliResult};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

static COUNTER: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActivePointer {
    pub schema_version: u32,
    pub session_id: String,
    pub workspace: PathBuf,
}

pub fn find_workspace_root(start: &Path) -> CliResult<PathBuf> {
    let start = start.canonicalize().map_err(|error| {
        CliError::new(
            "WORKSPACE",
            format!("cannot resolve {}: {error}", start.display()),
        )
    })?;
    for candidate in start.ancestors() {
        if candidate.join(".cowbox.toml").exists() || candidate.join(".git").exists() {
            return Ok(candidate.to_path_buf());
        }
    }
    Ok(start)
}

pub fn state_root() -> CliResult<PathBuf> {
    let path = if let Some(home) = std::env::var_os("COWBOX_HOME") {
        PathBuf::from(home)
    } else if let Some(xdg) = std::env::var_os("XDG_STATE_HOME") {
        PathBuf::from(xdg).join("cowbox")
    } else if let Some(home) = std::env::var_os("HOME") {
        PathBuf::from(home)
            .join(".local")
            .join("state")
            .join("cowbox")
    } else {
        return Err(CliError::new(
            "STATE_ROOT",
            "HOME is not set and COWBOX_HOME/XDG_STATE_HOME are absent",
        ));
    };
    std::fs::create_dir_all(&path)?;
    Ok(path.canonicalize()?)
}

pub fn validate_state_root(state_root: &Path, workspace: &Path) -> CliResult<()> {
    let state_root = state_root.canonicalize()?;
    let workspace = workspace.canonicalize()?;
    if state_root.starts_with(&workspace) || workspace.starts_with(&state_root) {
        return Err(CliError::new(
            "STATE_ROOT",
            format!(
                "state root {} must be outside the workspace {}",
                state_root.display(),
                workspace.display()
            ),
        )
        .hint("set COWBOX_HOME to a directory on WSL ext4, outside the project")
        .exit(2));
    }
    Ok(())
}

pub fn resolve_path(workspace: &Path, path: &Path) -> PathBuf {
    let candidate = if path.is_absolute() {
        path.to_path_buf()
    } else {
        workspace.join(path)
    };
    candidate.canonicalize().unwrap_or(candidate)
}

pub fn workspace_hash(workspace: &Path) -> String {
    let mut hasher = Sha256::new();
    hasher.update(workspace.to_string_lossy().as_bytes());
    let digest = hasher.finalize();
    hex::encode(&digest[..8])
}

pub fn new_session_id() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or_default();
    let counter = COUNTER.fetch_add(1, Ordering::Relaxed);
    let mut hasher = Sha256::new();
    hasher.update(nanos.to_le_bytes());
    hasher.update(std::process::id().to_le_bytes());
    hasher.update(counter.to_le_bytes());
    let digest = hasher.finalize();
    hex::encode(&digest[..4])
}

pub fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or_default()
}

pub fn format_timestamp(unix_ms: i64) -> String {
    chrono::DateTime::<chrono::Utc>::from_timestamp_millis(unix_ms)
        .map(|timestamp| timestamp.format("%Y-%m-%d %H:%M:%S UTC").to_string())
        .unwrap_or_else(|| unix_ms.to_string())
}
