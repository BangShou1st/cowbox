use crate::error::{CliError, CliResult};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
#[cfg(any(not(windows), test))]
use std::path::Component;
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
    let path = state_root_setting()?;
    std::fs::create_dir_all(&path)?;
    Ok(path.canonicalize()?)
}

pub(crate) fn state_root_setting() -> CliResult<PathBuf> {
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
    Ok(path)
}

pub fn validate_state_root(state_root: &Path, workspace: &Path) -> CliResult<()> {
    let state_root = state_root.canonicalize()?;
    let workspace = workspace.canonicalize()?;
    ensure_state_root_outside(&state_root, &workspace)
}

#[cfg(any(not(windows), test))]
pub(crate) fn diagnostic_state_root(
    state_root_setting: &Path,
    workspace: &Path,
) -> CliResult<PathBuf> {
    let state_root = resolve_existing_prefix(state_root_setting)?;
    match std::fs::symlink_metadata(&state_root) {
        Ok(metadata) if !metadata.is_dir() => {
            return Err(state_root_error(format!(
                "state root {} is not a directory",
                state_root.display()
            )));
        }
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(state_root_error(format!(
                "cannot inspect state root {}: {error}",
                state_root.display()
            )));
        }
    }
    let workspace = workspace.canonicalize()?;
    ensure_state_root_outside(&state_root, &workspace)?;
    Ok(state_root)
}

fn ensure_state_root_outside(state_root: &Path, workspace: &Path) -> CliResult<()> {
    if state_root.starts_with(workspace) || workspace.starts_with(state_root) {
        return Err(CliError::new(
            "STATE_ROOT",
            format!(
                "state root {} must be outside the workspace {}",
                state_root.display(),
                workspace.display()
            ),
        )
        .hint(
            "set COWBOX_HOME to a directory on an xattr-capable Linux filesystem, outside the project (WSL ext4 is recommended)",
        )
        .exit(2));
    }
    Ok(())
}

#[cfg(any(not(windows), test))]
fn state_root_error(message: impl Into<String>) -> CliError {
    CliError::new("STATE_ROOT", message)
        .hint(
            "set COWBOX_HOME to a directory on an xattr-capable Linux filesystem, outside the project (WSL ext4 is recommended)",
        )
        .exit(2)
}

#[cfg(any(not(windows), test))]
fn resolve_existing_prefix(path: &Path) -> CliResult<PathBuf> {
    let cwd = std::env::current_dir()?;
    resolve_existing_prefix_from(&cwd, path)
}

#[cfg(any(not(windows), test))]
fn resolve_existing_prefix_from(base: &Path, path: &Path) -> CliResult<PathBuf> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        base.join(path)
    };
    let mut resolved = PathBuf::new();

    for component in absolute.components() {
        match component {
            Component::Prefix(prefix) => resolved.push(prefix.as_os_str()),
            Component::RootDir => resolved.push(component.as_os_str()),
            Component::CurDir => {}
            Component::ParentDir => {
                resolved.pop();
            }
            Component::Normal(name) => {
                let candidate = resolved.join(name);
                match std::fs::symlink_metadata(&candidate) {
                    Ok(metadata) if metadata.file_type().is_symlink() => {
                        resolved = candidate.canonicalize().map_err(|error| {
                            state_root_error(format!(
                                "state root {} contains a symbolic link that cannot be resolved at {}: {error}",
                                absolute.display(),
                                candidate.display()
                            ))
                        })?;
                    }
                    Ok(_) => resolved = candidate,
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                        resolved = candidate;
                    }
                    Err(error) => {
                        return Err(state_root_error(format!(
                            "cannot inspect state root {} at {}: {error}",
                            absolute.display(),
                            candidate.display()
                        )));
                    }
                }
            }
        }
    }

    Ok(resolved)
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

#[cfg(test)]
mod tests {
    use super::*;

    fn workspace_at(root: &Path) -> PathBuf {
        let workspace = root.join("workspace");
        std::fs::create_dir(&workspace).expect("workspace");
        std::fs::create_dir(workspace.join(".git")).expect("git marker");
        workspace
    }

    #[test]
    fn diagnostic_state_root_does_not_create_missing_directories() {
        let root = tempfile::tempdir().expect("tempdir");
        let workspace = workspace_at(root.path());
        let state_root = root.path().join("state/nested");

        let resolved = diagnostic_state_root(&state_root, &workspace).expect("diagnostic state");

        assert_eq!(resolved, state_root);
        assert!(!state_root.exists());
    }

    #[test]
    fn diagnostic_state_root_resolves_missing_relative_parent_components() {
        let root = tempfile::tempdir().expect("tempdir");
        let base = root.path().join("base");
        std::fs::create_dir(&base).expect("base");

        let resolved = resolve_existing_prefix_from(&base, Path::new("missing/../state"))
            .expect("relative state");
        let trailing_parent =
            resolve_existing_prefix_from(&base, Path::new("missing/..")).expect("trailing parent");

        assert_eq!(resolved, base.join("state"));
        assert_eq!(trailing_parent, base);
        assert!(!base.join("missing").exists());
        assert!(!base.join("state").exists());
    }

    #[cfg(unix)]
    #[test]
    fn diagnostic_state_root_canonicalizes_existing_symlinks() {
        use std::os::unix::fs::symlink;

        let root = tempfile::tempdir().expect("tempdir");
        let workspace = workspace_at(root.path());
        let target = root.path().join("state-target");
        let link = root.path().join("state-link");
        std::fs::create_dir(&target).expect("state target");
        symlink(&target, &link).expect("state symlink");

        let resolved = diagnostic_state_root(&link, &workspace).expect("state symlink");

        assert_eq!(resolved, target.canonicalize().expect("canonical target"));
    }

    #[cfg(unix)]
    #[test]
    fn diagnostic_state_root_rejects_dangling_symlinks() {
        use std::os::unix::fs::symlink;

        let root = tempfile::tempdir().expect("tempdir");
        let workspace = workspace_at(root.path());
        let link = root.path().join("state-link");
        let missing_target = root.path().join("missing-target");
        symlink("missing-target", &link).expect("dangling symlink");

        let error = diagnostic_state_root(&link, &workspace).expect_err("dangling symlink");

        assert_eq!(error.code, "STATE_ROOT");
        assert_eq!(error.exit_code, 2);
        assert!(error.message.contains("symbolic link"));
        assert!(!missing_target.exists());
    }

    #[cfg(unix)]
    #[test]
    fn diagnostic_state_root_rejects_symlinks_into_the_workspace() {
        use std::os::unix::fs::symlink;

        let root = tempfile::tempdir().expect("tempdir");
        let workspace = workspace_at(root.path());
        let link = root.path().join("state-link");
        symlink(&workspace, &link).expect("workspace symlink");

        let error = diagnostic_state_root(&link, &workspace).expect_err("workspace symlink");

        assert_eq!(error.code, "STATE_ROOT");
        assert_eq!(error.exit_code, 2);
        assert!(error.message.contains("outside the workspace"));
    }

    #[test]
    fn diagnostic_state_root_rejects_a_non_directory_path() {
        let root = tempfile::tempdir().expect("tempdir");
        let workspace = workspace_at(root.path());
        let state_root = root.path().join("state-file");
        std::fs::write(&state_root, "not a directory").expect("state file");

        let error = diagnostic_state_root(&state_root, &workspace).expect_err("not a directory");

        assert_eq!(error.code, "STATE_ROOT");
        assert_eq!(error.exit_code, 2);
    }

    #[test]
    fn diagnostic_state_root_rejects_paths_inside_the_workspace() {
        let root = tempfile::tempdir().expect("tempdir");
        let workspace = workspace_at(root.path());
        let state_root = workspace.join("state");

        let error = diagnostic_state_root(&state_root, &workspace).expect_err("overlap");

        assert_eq!(error.code, "STATE_ROOT");
        assert_eq!(error.exit_code, 2);
        assert!(!state_root.exists());
    }
}
