use super::error::WinResult;
use super::model::*;
use super::paths;
use super::store;
use std::fs;
use std::path::{Path, PathBuf};

pub fn doctor(workspace: Option<&Path>, state_root: Option<&Path>) -> WinResult<WinDoctorReport> {
    let workspace = workspace
        .map(Path::to_path_buf)
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_default();
    let state_root = state_root
        .map(Path::to_path_buf)
        .unwrap_or_else(default_state_root);
    let workspace_report = path_report(&workspace, false);
    let state_report = path_report(&state_root, true);
    let path_test = run_path_test(&state_root);
    let mut blockers = Vec::new();
    let mut warnings = Vec::new();

    if !workspace.exists() {
        blockers.push(format!(
            "workspace {} does not exist",
            paths::display(&workspace)
        ));
    } else if let Some(report) = &workspace_report {
        if !report.ntfs {
            blockers.push(format!(
                "workspace filesystem is {}, but NTFS is required",
                report.filesystem.as_deref().unwrap_or("unknown")
            ));
        }
    }
    if !state_root.exists() {
        warnings.push(format!(
            "state root {} does not exist yet; win init will create it",
            paths::display(&state_root)
        ));
    }
    if let Some(report) = &state_report {
        if !report.ntfs {
            blockers.push(format!(
                "state root filesystem is {}, but NTFS is required",
                report.filesystem.as_deref().unwrap_or("unknown")
            ));
        }
        if report
            .available_bytes
            .is_some_and(|bytes| bytes < 256 * 1024 * 1024)
        {
            blockers.push("state root has less than 256 MiB available".to_string());
        }
    } else {
        blockers.push(format!(
            "cannot determine the filesystem for state root {}",
            paths::display(&state_root)
        ));
    }
    if workspace.exists() && state_root.exists() {
        if let Err(error) = paths::ensure_outside(&state_root, &workspace) {
            blockers.push(error.message);
        }
    }
    if !path_test.long_paths {
        blockers.push("the state root cannot create and read a long path".to_string());
    }
    if !path_test.acl_inspection {
        blockers.push("the state root does not support DACL inspection".to_string());
    }

    Ok(WinDoctorReport {
        platform: "windows".to_string(),
        ready: blockers.is_empty(),
        storage_mode: WIN_STORAGE_MODE.to_string(),
        workspace: workspace_report,
        state_root: state_report,
        path_test,
        capabilities: vec![
            "ntfs_materialized_copy".to_string(),
            "content_addressed_objects".to_string(),
            "junction_workspace".to_string(),
            "long_paths".to_string(),
            "ads_copy".to_string(),
            "acl_inspection".to_string(),
            "hardlink_detection".to_string(),
            "reparse_refusal".to_string(),
        ],
        blockers,
        warnings,
    })
}

fn default_state_root() -> PathBuf {
    if let Some(local) = std::env::var_os("LOCALAPPDATA") {
        return PathBuf::from(local).join("cowbox").join("state");
    }
    if let Some(profile) = std::env::var_os("USERPROFILE") {
        return PathBuf::from(profile)
            .join(".local")
            .join("state")
            .join("cowbox");
    }
    std::env::temp_dir().join("cowbox-state")
}

fn path_report(path: &Path, writable_test: bool) -> Option<WinPathReport> {
    let info = store::filesystem_info(path)?;
    Some(WinPathReport {
        path: paths::display(path),
        exists: path.exists(),
        filesystem: info.filesystem.clone(),
        available_bytes: Some(info.available_bytes),
        total_bytes: Some(info.total_bytes),
        ntfs: info.is_ntfs(),
        writable: if writable_test {
            Some(test_writable(path))
        } else {
            None
        },
    })
}

fn test_writable(path: &Path) -> bool {
    let base = nearest_existing_directory(path);
    if !base.is_dir() {
        return false;
    }
    let probe = base.join(format!(".cowbox-write-{}", std::process::id()));
    match fs::write(&probe, b"cowbox") {
        Ok(()) => {
            let _ = fs::remove_file(probe);
            true
        }
        Err(_) => false,
    }
}

fn run_path_test(state_root: &Path) -> WinPathTest {
    let base = nearest_existing_directory(state_root);
    let test_root = base.join(format!(
        ".cowbox-doctor-{}-{}",
        std::process::id(),
        super::store::now_ms()
    ));
    let deep = test_root
        .join("a".repeat(110))
        .join("b".repeat(110))
        .join("c".repeat(110));
    let file = deep.join("probe.txt");
    let mut long_paths = false;
    let mut tested_length = 0usize;
    let mut acl_inspection = false;
    if fs::create_dir_all(&deep).is_ok() {
        tested_length = file.as_os_str().len();
        if fs::write(&file, b"cowbox long path").is_ok()
            && fs::read(&file)
                .map(|bytes| bytes == b"cowbox long path")
                .unwrap_or(false)
        {
            long_paths = true;
        }
        acl_inspection = super::scan::acl_has_explicit_entries(&file).is_ok();
    }
    let _ = fs::remove_dir_all(&test_root);
    WinPathTest {
        long_paths,
        tested_length,
        acl_inspection,
        ads_copy: true,
    }
}

fn nearest_existing_directory(path: &Path) -> PathBuf {
    let mut current = path.to_path_buf();
    loop {
        if current.is_dir() {
            return current;
        }
        let Some(parent) = current.parent() else {
            return std::env::temp_dir();
        };
        current = parent.to_path_buf();
    }
}
