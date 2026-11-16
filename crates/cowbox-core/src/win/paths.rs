use super::error::{WinError, WinResult};
use std::path::{Component, Path, PathBuf};

pub fn absolute(path: &Path) -> WinResult<PathBuf> {
    if path.is_absolute() {
        Ok(path.to_path_buf())
    } else {
        Ok(std::env::current_dir()?.join(path))
    }
}

pub fn canonicalize(path: &Path) -> WinResult<PathBuf> {
    path.canonicalize().map_err(|error| {
        WinError::io(
            "WIN_PATH",
            format!("cannot resolve {}", display(path)),
            error,
        )
    })
}

pub fn canonicalize_existing_prefix(path: &Path) -> WinResult<PathBuf> {
    let absolute = absolute(path)?;
    let mut missing = Vec::new();
    let mut current = absolute.as_path();
    loop {
        match current.canonicalize() {
            Ok(mut resolved) => {
                for name in missing.iter().rev() {
                    resolved.push(name);
                }
                return Ok(resolved);
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                let Some(name) = current.file_name() else {
                    return Ok(absolute);
                };
                missing.push(name.to_os_string());
                let Some(parent) = current.parent() else {
                    return Ok(absolute);
                };
                current = parent;
            }
            Err(error) => {
                return Err(WinError::io(
                    "WIN_PATH",
                    format!("cannot resolve {}", display(&absolute)),
                    error,
                ));
            }
        }
    }
}

pub fn display(path: &Path) -> String {
    let text = path.to_string_lossy().replace('/', "\\");
    if let Some(rest) = text.strip_prefix(r"\\?\UNC\") {
        return format!(r"\\{rest}");
    }
    if let Some(rest) = text.strip_prefix(r"\\?\") {
        return rest.to_string();
    }
    text
}

pub fn ensure_outside(state_root: &Path, workspace: &Path) -> WinResult<()> {
    let state = canonicalize_existing_prefix(state_root)?;
    let workspace = canonicalize(workspace)?;
    if state.starts_with(&workspace) || workspace.starts_with(&state) {
        return Err(WinError::new(
            "WIN_STATE_ROOT",
            format!(
                "state root {} must be outside the workspace {}",
                display(&state),
                display(&workspace)
            ),
        )
        .hint("choose a separate NTFS directory such as D:\\cowbox-state")
        .exit(2));
    }
    Ok(())
}

pub fn relative_key(root: &Path, path: &Path) -> WinResult<String> {
    let relative = path.strip_prefix(root).map_err(|_| {
        WinError::new(
            "WIN_PATH",
            format!(
                "{} is outside the managed root {}",
                display(path),
                display(root)
            ),
        )
    })?;
    let mut components = Vec::new();
    for component in relative.components() {
        match component {
            Component::Normal(name) => components.push(name.to_string_lossy().to_string()),
            Component::CurDir => {}
            _ => {
                return Err(WinError::new(
                    "WIN_PATH",
                    format!("unsupported relative path {}", display(relative)),
                ));
            }
        }
    }
    Ok(components.join("/"))
}

pub fn join_key(root: &Path, key: &str) -> WinResult<PathBuf> {
    validate_key(key)?;
    let mut path = root.to_path_buf();
    for component in key.split('/') {
        path.push(component);
    }
    Ok(path)
}

pub fn validate_key(key: &str) -> WinResult<()> {
    if key.is_empty() {
        return Err(WinError::new("WIN_PATH", "empty managed path"));
    }
    for component in key.split('/') {
        if component.is_empty()
            || component == "."
            || component == ".."
            || component.contains(':')
            || component.ends_with(' ')
            || component.ends_with('.')
        {
            return Err(WinError::new(
                "WIN_PATH",
                format!("unsupported managed path component {component:?} in {key}"),
            ));
        }
        if is_reserved_name(component) {
            return Err(WinError::new(
                "WIN_PATH",
                format!("reserved Windows name {component:?} in {key}"),
            ));
        }
    }
    Ok(())
}

fn is_reserved_name(component: &str) -> bool {
    let stem = component
        .split('.')
        .next()
        .unwrap_or(component)
        .to_ascii_uppercase();
    matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || (stem.len() == 4
            && (stem.starts_with("COM") || stem.starts_with("LPT"))
            && stem.as_bytes()[3].is_ascii_digit()
            && stem.as_bytes()[3] != b'0')
}

pub fn ensure_within(root: &Path, candidate: &Path) -> WinResult<()> {
    let root = canonicalize(root)?;
    let candidate = absolute(candidate)?;
    let parent = candidate.parent().ok_or_else(|| {
        WinError::new("WIN_PATH", format!("{} has no parent", display(&candidate)))
    })?;
    let parent = canonicalize_existing_prefix(parent)?;
    if !parent.starts_with(&root) {
        return Err(WinError::new(
            "WIN_PATH",
            format!(
                "refusing to touch {} because it is outside {}",
                display(&candidate),
                display(&root)
            ),
        ));
    }
    Ok(())
}

#[cfg(windows)]
pub fn is_reparse(metadata: &std::fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;
    const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
    metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
}

pub fn ensure_no_reparse(path: &Path) -> WinResult<()> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) => {
            #[cfg(windows)]
            if is_reparse(&metadata) {
                return Err(WinError::new(
                    "WIN_REPARSE_UNSUPPORTED",
                    format!(
                        "{} is a symlink, junction, or reparse point; cowbox refuses to follow it",
                        display(path)
                    ),
                )
                .hint("remove the reparse point or place that content outside cowbox")
                .exit(2));
            }
            let _ = metadata;
            Ok(())
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(WinError::io(
            "WIN_PATH",
            format!("cannot inspect {}", display(path)),
            error,
        )),
    }
}

pub fn depth(key: &str) -> usize {
    key.bytes().filter(|byte| *byte == b'/').count() + 1
}

pub fn is_excluded_key(key: &str, excludes: &[String]) -> bool {
    key.split('/').any(|component| {
        excludes
            .iter()
            .any(|exclude| component.eq_ignore_ascii_case(exclude))
            || component.eq_ignore_ascii_case(".git")
    })
}
