use crate::error::Result;
use crate::model::{Capabilities, WhiteoutKind};
use landlock::{ABI, Access, AccessFs, Ruleset, RulesetAttr};
use std::fs;
use std::path::{Path, PathBuf};

pub fn probe(state_root: &Path) -> Result<Capabilities> {
    let kernel = fs::read_to_string("/proc/sys/kernel/osrelease")
        .map(|value| value.trim().to_string())
        .unwrap_or_else(|_| "unknown".to_string());
    let wsl = fs::read_to_string("/proc/version")
        .map(|value| value.to_ascii_lowercase().contains("microsoft"))
        .unwrap_or(false);
    let overlayfs = fs::read_to_string("/proc/filesystems")
        .map(|value| {
            value
                .lines()
                .any(|line| line.split_whitespace().last() == Some("overlay"))
        })
        .unwrap_or(false);
    let user_namespace = user_namespaces_available();
    let landlock_abi = detect_landlock();
    let state_fs = detect_state_fs(state_root);
    let userxattr = kernel_at_least(&kernel, 5, 11);

    let mut notes = Vec::new();
    if !userxattr {
        notes.push("kernel is older than 5.11; userxattr OverlayFS is unavailable".to_string());
    }
    if landlock_abi.unwrap_or(0) < 3 {
        notes.push("Landlock ABI 3 is required; path fencing may be unavailable".to_string());
    }
    notes.push("network isolation is not implemented in M1".to_string());
    notes.push("whiteout kind is confirmed by the first real run".to_string());

    Ok(Capabilities {
        os: "linux".to_string(),
        kernel,
        wsl,
        user_namespace,
        overlayfs,
        userxattr,
        landlock_abi,
        whiteout_kind: WhiteoutKind::Unknown,
        state_fs,
        notes,
    })
}

fn user_namespaces_available() -> bool {
    let clone_setting = fs::read_to_string("/proc/sys/kernel/unprivileged_userns_clone")
        .ok()
        .map(|value| value.trim() != "0");
    let max_namespaces = fs::read_to_string("/proc/sys/user/max_user_namespaces")
        .ok()
        .and_then(|value| value.trim().parse::<u64>().ok())
        .map(|value| value > 0);
    clone_setting.unwrap_or(true) && max_namespaces.unwrap_or(true)
}

fn detect_landlock() -> Option<i32> {
    for (abi, number) in [(ABI::V3, 3), (ABI::V2, 2), (ABI::V1, 1)] {
        if Ruleset::default()
            .handle_access(AccessFs::from_all(abi))
            .is_ok()
        {
            return Some(number);
        }
    }
    None
}

fn detect_state_fs(state_root: &Path) -> Option<String> {
    let existing = nearest_existing(state_root)?;
    let stat = rustix::fs::statfs(&existing).ok()?;
    let magic = stat.f_type as u64;
    let name = match magic {
        0xEF53 => "ext4",
        0x9123_683E => "btrfs",
        0x5846_5342 => "xfs",
        0x0102_1994 => "tmpfs",
        0x794C_7630 => "overlayfs",
        0x6969 => "nfs",
        0x0102_1997 => "9p",
        0x2FC1_2FC1 => "zfs",
        _ => return Some(format!("0x{magic:x}")),
    };
    Some(name.to_string())
}

fn nearest_existing(path: &Path) -> Option<PathBuf> {
    let mut current = if path.exists() {
        path.to_path_buf()
    } else {
        path.parent()?.to_path_buf()
    };
    while !current.exists() {
        current = current.parent()?.to_path_buf();
    }
    Some(current)
}

fn kernel_at_least(kernel: &str, major: u64, minor: u64) -> bool {
    let mut parts = kernel.split('.');
    let Some(found_major) = parts.next().and_then(|value| value.parse::<u64>().ok()) else {
        return false;
    };
    let Some(found_minor) = parts.next().and_then(|value| value.parse::<u64>().ok()) else {
        return false;
    };
    (found_major, found_minor) >= (major, minor)
}
