use super::error::{WinError, WinResult};
use super::model::{WinChange, WinChangeKind, WinEntry, WinEntryKind, WinManifest, WinStepDelta};
use super::paths;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs;
use std::io::Read;
use std::path::Path;
use std::time::UNIX_EPOCH;

pub fn sha256_bytes(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hex::encode(hasher.finalize())
}

pub fn hash_file(path: &Path) -> WinResult<(u64, String)> {
    let mut file = fs::File::open(path).map_err(|error| {
        WinError::io(
            "WIN_FILE_UNREADABLE",
            format!("cannot read {}", paths::display(path)),
            error,
        )
    })?;
    let before = file.metadata().ok().map(|metadata| metadata.len());
    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; 128 * 1024];
    let mut total = 0u64;
    loop {
        let read = file.read(&mut buffer).map_err(|error| {
            WinError::io(
                "WIN_FILE_UNREADABLE",
                format!("cannot read {}", paths::display(path)),
                error,
            )
        })?;
        if read == 0 {
            break;
        }
        total = total.saturating_add(read as u64);
        hasher.update(&buffer[..read]);
    }
    let after = file.metadata().ok().map(|metadata| metadata.len());
    if before != after || before != Some(total) {
        return Err(WinError::new(
            "WIN_HOST_CHANGED_DURING_IMPORT",
            format!(
                "{} changed while cowbox was reading it",
                paths::display(path)
            ),
        )
        .hint("stop the writer, then retry")
        .exit(2));
    }
    Ok((total, hex::encode(hasher.finalize())))
}

pub fn scan_tree(root: &Path, excludes: &[String]) -> WinResult<WinManifest> {
    let root = paths::canonicalize(root)?;
    if !root.is_dir() {
        return Err(WinError::new(
            "WIN_WORKSPACE",
            format!("{} is not a directory", paths::display(&root)),
        )
        .exit(2));
    }
    let mut manifest = WinManifest::default();
    let mut stack = vec![root.clone()];
    let mut seen = HashMap::<String, String>::new();

    while let Some(directory) = stack.pop() {
        let mut entries = fs::read_dir(&directory)
            .map_err(|error| {
                WinError::io(
                    "WIN_FILE_UNREADABLE",
                    format!("cannot list {}", paths::display(&directory)),
                    error,
                )
            })?
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(|error| {
                WinError::io(
                    "WIN_FILE_UNREADABLE",
                    format!("cannot list {}", paths::display(&directory)),
                    error,
                )
            })?;
        entries.sort_by_key(|entry| entry.file_name());

        for entry in entries {
            let name = entry.file_name();
            let name_text = name.to_str().ok_or_else(|| {
                WinError::new(
                    "WIN_PATH_UNICODE",
                    format!(
                        "{} contains a name that is not valid UTF-16",
                        paths::display(&entry.path())
                    ),
                )
                .hint("rename the path to well-formed Unicode before using cowbox")
                .exit(2)
            })?;
            let key = paths::relative_key(&root, &entry.path())?;
            if paths::is_excluded_key(&key, excludes) {
                continue;
            }
            paths::validate_key(&key)?;
            let folded = key.to_lowercase();
            if let Some(previous) = seen.insert(folded, key.clone()) {
                return Err(WinError::new(
                    "WIN_CASE_COLLISION",
                    format!(
                        "{} and {} collide on a case-insensitive NTFS volume",
                        previous, key
                    ),
                )
                .hint("rename one of the paths so the names differ by more than case")
                .exit(2));
            }
            let metadata = fs::symlink_metadata(entry.path()).map_err(|error| {
                WinError::io(
                    "WIN_PATH",
                    format!("cannot inspect {}", paths::display(&entry.path())),
                    error,
                )
            })?;
            #[cfg(windows)]
            if paths::is_reparse(&metadata) {
                return Err(WinError::new(
                    "WIN_REPARSE_UNSUPPORTED",
                    format!(
                        "{} is a symlink, junction, or reparse point; cowbox does not follow it",
                        paths::display(&entry.path())
                    ),
                )
                .hint("remove the reparse point or keep that content outside the managed workspace")
                .exit(2));
            }
            if metadata.is_dir() {
                manifest.entries.insert(key.clone(), WinEntry::directory());
                stack.push(entry.path());
            } else if metadata.is_file() {
                let (size, sha256, hardlinks) = file_details(&entry.path())?;
                let readonly = metadata.permissions().readonly();
                let mtime_unix_ms = metadata.modified().ok().and_then(|time| {
                    time.duration_since(UNIX_EPOCH)
                        .ok()
                        .map(|duration| duration.as_millis() as i64)
                });
                manifest.entries.insert(
                    key,
                    WinEntry::file(size, sha256, readonly, mtime_unix_ms, hardlinks),
                );
            } else {
                return Err(WinError::new(
                    "WIN_UNSUPPORTED_FILE_TYPE",
                    format!(
                        "{} is neither a regular file nor a directory",
                        paths::display(&entry.path())
                    ),
                )
                .hint("remove the special file before using cowbox")
                .exit(2));
            }
            let _ = name_text;
        }
    }

    Ok(manifest)
}

pub fn file_hardlinks(path: &Path) -> WinResult<u32> {
    let file = fs::File::open(path).map_err(|error| {
        WinError::io(
            "WIN_FILE_UNREADABLE",
            format!("cannot open {}", paths::display(path)),
            error,
        )
    })?;
    let information = winapi_util::file::information(&file).map_err(|error| {
        WinError::io(
            "WIN_FILE_UNREADABLE",
            format!("cannot inspect {}", paths::display(path)),
            error,
        )
    })?;
    Ok(u32::try_from(information.number_of_links()).unwrap_or(u32::MAX))
}

fn file_details(path: &Path) -> WinResult<(u64, String, u32)> {
    let mut file = fs::File::open(path).map_err(|error| {
        WinError::io(
            "WIN_FILE_UNREADABLE",
            format!("cannot read {}", paths::display(path)),
            error,
        )
    })?;
    let information = winapi_util::file::information(&file).map_err(|error| {
        WinError::io(
            "WIN_FILE_UNREADABLE",
            format!("cannot inspect {}", paths::display(path)),
            error,
        )
    })?;
    let before = file.metadata().ok().map(|metadata| metadata.len());
    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; 128 * 1024];
    let mut total = 0u64;
    loop {
        let read = file.read(&mut buffer).map_err(|error| {
            WinError::io(
                "WIN_FILE_UNREADABLE",
                format!("cannot read {}", paths::display(path)),
                error,
            )
        })?;
        if read == 0 {
            break;
        }
        total = total.saturating_add(read as u64);
        hasher.update(&buffer[..read]);
    }
    let after = file.metadata().ok().map(|metadata| metadata.len());
    if before != after || before != Some(total) {
        return Err(WinError::new(
            "WIN_HOST_CHANGED_DURING_IMPORT",
            format!(
                "{} changed while cowbox was reading it",
                paths::display(path)
            ),
        )
        .hint("stop the writer, then retry")
        .exit(2));
    }
    Ok((
        total,
        hex::encode(hasher.finalize()),
        u32::try_from(information.number_of_links()).unwrap_or(u32::MAX),
    ))
}

pub fn acl_has_explicit_entries(path: &Path) -> WinResult<bool> {
    const INHERITED_ACE: u8 = 0x10;
    let plain = path.to_str().ok_or_else(|| {
        WinError::new(
            "WIN_PATH_UNICODE",
            format!(
                "{} is not valid UTF-16; ACL inspection is unavailable",
                paths::display(path)
            ),
        )
    })?;
    let verbatim = path
        .canonicalize()
        .ok()
        .and_then(|path| path.to_str().map(str::to_string));
    let acl = match verbatim
        .as_deref()
        .and_then(|text| windows_acl::acl::ACL::from_file_path(text, false).ok())
    {
        Some(acl) => acl,
        None => windows_acl::acl::ACL::from_file_path(plain, false).map_err(|code| {
            WinError::new(
                "WIN_ACL_CHECK_FAILED",
                format!(
                    "cannot read the DACL of {} (Windows error {code})",
                    paths::display(path)
                ),
            )
            .hint("inspect the file ACL with `icacls`, then retry")
            .exit(2)
        })?,
    };
    let entries = acl.all().map_err(|code| {
        WinError::new(
            "WIN_ACL_CHECK_FAILED",
            format!(
                "cannot enumerate the DACL of {} (Windows error {code})",
                paths::display(path)
            ),
        )
        .exit(2)
    })?;
    Ok(entries.iter().any(|entry| entry.flags & INHERITED_ACE == 0))
}

pub fn delta_entries(
    parent: &WinManifest,
    current: &WinManifest,
) -> BTreeMap<String, Option<WinEntry>> {
    let keys = parent
        .entries
        .keys()
        .chain(current.entries.keys())
        .cloned()
        .collect::<BTreeSet<_>>();
    let mut delta = BTreeMap::new();
    for key in keys {
        match (parent.entries.get(&key), current.entries.get(&key)) {
            (None, Some(entry)) => {
                delta.insert(key, Some(entry.clone()));
            }
            (Some(_), None) => {
                delta.insert(key, None);
            }
            (Some(before), Some(after)) if !before.content_equals(after) => {
                delta.insert(key, Some(after.clone()));
            }
            _ => {}
        }
    }
    delta
}

pub fn apply_delta(manifest: &mut WinManifest, delta: &WinStepDelta) {
    for (key, entry) in &delta.entries {
        match entry {
            Some(entry) => {
                manifest.entries.insert(key.clone(), entry.clone());
            }
            None => {
                manifest.entries.remove(key);
            }
        }
    }
}

pub fn replay(base: &WinManifest, deltas: &[WinStepDelta]) -> WinResult<WinManifest> {
    let mut manifest = base.clone();
    for delta in deltas {
        apply_delta(&mut manifest, delta);
    }
    Ok(manifest)
}

pub fn diff_manifests(base: &WinManifest, target: &WinManifest) -> Vec<WinChange> {
    let keys = base
        .entries
        .keys()
        .chain(target.entries.keys())
        .cloned()
        .collect::<BTreeSet<_>>();
    let mut changes = Vec::new();
    for key in keys {
        let before = base.entries.get(&key);
        let after = target.entries.get(&key);
        match (before, after) {
            (None, Some(after)) if after.kind.is_file() => {
                changes.push(change_for(
                    key,
                    WinChangeKind::Added,
                    WinEntryKind::File,
                    after.size as i64,
                    None,
                    after.sha256.clone(),
                    after,
                ));
            }
            (Some(before), None) if before.kind.is_file() => {
                changes.push(change_for(
                    key,
                    WinChangeKind::Deleted,
                    WinEntryKind::File,
                    -(before.size as i64),
                    before.sha256.clone(),
                    None,
                    before,
                ));
            }
            (Some(before), Some(after)) if before.content_equals(after) => {}
            (Some(before), Some(after)) => {
                let kind = if before.kind != after.kind {
                    WinChangeKind::TypeChanged
                } else if before.kind.is_file() {
                    WinChangeKind::Modified
                } else {
                    continue;
                };
                changes.push(change_for(
                    key,
                    kind,
                    after.kind,
                    after.size as i64 - before.size as i64,
                    before.sha256.clone(),
                    after.sha256.clone(),
                    after,
                ));
            }
            _ => {}
        }
    }
    changes
}

fn change_for(
    rel_path: String,
    kind: WinChangeKind,
    entry_kind: WinEntryKind,
    size_delta: i64,
    old_sha256: Option<String>,
    new_sha256: Option<String>,
    _entry: &WinEntry,
) -> WinChange {
    WinChange {
        rel_path,
        kind,
        entry_kind,
        size_delta,
        old_sha256,
        new_sha256,
        lines_added: None,
        lines_removed: None,
    }
}

pub fn manifests_equal(left: &WinManifest, right: &WinManifest) -> bool {
    if left.entries.len() != right.entries.len() {
        return false;
    }
    left.entries.iter().all(|(key, entry)| {
        right
            .entries
            .get(key)
            .is_some_and(|other| entry.content_equals(other))
    })
}

pub fn changed_keys(base: &WinManifest, current: &WinManifest) -> Vec<String> {
    let keys = base
        .entries
        .keys()
        .chain(current.entries.keys())
        .cloned()
        .collect::<BTreeSet<_>>();
    keys.into_iter()
        .filter(
            |key| match (base.entries.get(key), current.entries.get(key)) {
                (Some(before), Some(after)) => !before.content_equals(after),
                (None, None) => false,
                _ => true,
            },
        )
        .collect()
}
