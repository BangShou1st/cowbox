use crate::diff::compute_diff;
use crate::error::{CoreError, Result};
use crate::layout::SessionLayout;
use crate::manifest::{self, ManifestEntry};
use crate::model::{ChangeKind, CommitOptions, CommitReport, DiffOptions, EntryKind, FileChange};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::{Read, Write};
use std::os::unix::fs::{MetadataExt, PermissionsExt, symlink};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize)]
struct CommitOp {
    rel_path: PathBuf,
    action: String,
    backup: Option<PathBuf>,
    created: bool,
}

pub fn commit_layers(
    layout: &SessionLayout,
    target_layers: &[u64],
    options: &CommitOptions,
) -> Result<CommitReport> {
    let _span = tracing::info_span!(
        "commit_layers",
        layers = target_layers.len(),
        workspace = %layout.workspace.display()
    )
    .entered();
    let diff_options = DiffOptions {
        include_excluded: options.include_excluded,
        line_stats: false,
        max_line_stat_files: 0,
        max_line_stat_bytes: 0,
        excluded: options.excluded.clone(),
    };
    let changes = compute_diff(layout, &[], target_layers, &diff_options)?;
    reject_vcs_metadata(layout, &changes)?;

    if changes.is_empty() {
        return Ok(CommitReport {
            changes,
            committed: !options.dry_run,
            conflicts: Vec::new(),
            journal: None,
        });
    }

    let base = manifest::load_manifest(&layout.base_manifest_path())?;
    let conflicts = find_conflicts(layout, &base, &changes);
    if !conflicts.is_empty() && !options.force {
        return Ok(CommitReport {
            changes,
            committed: false,
            conflicts,
            journal: None,
        });
    }
    if options.dry_run {
        return Ok(CommitReport {
            changes,
            committed: false,
            conflicts,
            journal: None,
        });
    }

    let journal_dir = layout.session_dir.join("commit").join(format!(
        "{}-{}",
        chrono::Utc::now().timestamp_millis(),
        std::process::id()
    ));
    let backup_dir = journal_dir.join("backup");
    fs::create_dir_all(&backup_dir)?;

    let mut ops = Vec::new();
    let journal_result = (|| -> Result<()> {
        for change in &changes {
            let host_path = layout.workspace.join(&change.rel_path);
            let exists = fs::symlink_metadata(&host_path).is_ok();
            if !exists {
                ops.push(CommitOp {
                    rel_path: change.rel_path.clone(),
                    action: action_name(&change.kind).to_string(),
                    backup: None,
                    created: !matches!(change.kind, ChangeKind::Deleted),
                });
                continue;
            }
            let backup_path = backup_dir.join(&change.rel_path);
            copy_entry(&host_path, &backup_path)?;
            ops.push(CommitOp {
                rel_path: change.rel_path.clone(),
                action: action_name(&change.kind).to_string(),
                backup: Some(backup_path),
                created: false,
            });
        }
        write_ops(&journal_dir.join("ops.json"), &ops)?;
        apply_changes(layout, target_layers, &changes)
    })();

    if let Err(error) = journal_result {
        let restore = restore_ops(layout, &ops);
        if let Err(restore_error) = restore {
            return Err(CoreError::CommitFailed(format!(
                "{error}; journal restore also failed: {restore_error}; journal kept at {}",
                journal_dir.display()
            )));
        }
        return Err(CoreError::CommitFailed(format!(
            "{error}; host restored from journal {}",
            journal_dir.display()
        )));
    }

    let _ = fs::remove_dir_all(&backup_dir);
    Ok(CommitReport {
        changes,
        committed: true,
        conflicts,
        journal: Some(journal_dir),
    })
}

fn find_conflicts(
    layout: &SessionLayout,
    base: &BTreeMap<PathBuf, ManifestEntry>,
    changes: &[FileChange],
) -> Vec<PathBuf> {
    let mut conflicts = Vec::new();
    for change in changes {
        let host_path = layout.workspace.join(&change.rel_path);
        match base.get(&change.rel_path) {
            Some(entry) => {
                if !manifest::metadata_matches(entry, &host_path) {
                    conflicts.push(change.rel_path.clone());
                    continue;
                }
                if change.entry_kind == EntryKind::Directory
                    && matches!(
                        change.kind,
                        ChangeKind::Deleted | ChangeKind::TypeChanged { .. }
                    )
                {
                    for (rel_path, child) in base {
                        if rel_path.starts_with(&change.rel_path)
                            && rel_path != &change.rel_path
                            && !manifest::metadata_matches(child, &layout.workspace.join(rel_path))
                        {
                            conflicts.push(change.rel_path.clone());
                            break;
                        }
                    }
                }
            }
            None => {
                if fs::symlink_metadata(&host_path).is_ok() {
                    conflicts.push(change.rel_path.clone());
                }
            }
        }
    }
    conflicts
}

fn apply_changes(
    layout: &SessionLayout,
    target_layers: &[u64],
    changes: &[FileChange],
) -> Result<()> {
    let mut deletions: Vec<&FileChange> = changes
        .iter()
        .filter(|change| {
            matches!(
                change.kind,
                ChangeKind::Deleted | ChangeKind::TypeChanged { .. }
            )
        })
        .collect();
    deletions.sort_by_key(|change| std::cmp::Reverse(path_depth(&change.rel_path)));

    for change in deletions {
        let host_path = layout.workspace.join(&change.rel_path);
        remove_path_if_exists(&host_path)?;
    }

    let mut directories: Vec<&FileChange> = changes
        .iter()
        .filter(|change| {
            change.entry_kind == EntryKind::Directory && !matches!(change.kind, ChangeKind::Deleted)
        })
        .collect();
    directories.sort_by_key(|change| path_depth(&change.rel_path));
    for change in directories {
        fs::create_dir_all(layout.workspace.join(&change.rel_path))?;
    }

    let mut files: Vec<&FileChange> = changes
        .iter()
        .filter(|change| {
            change.entry_kind != EntryKind::Directory && !matches!(change.kind, ChangeKind::Deleted)
        })
        .collect();
    files.sort_by_key(|change| {
        let symlink_last = change.entry_kind == EntryKind::Symlink;
        (symlink_last, path_depth(&change.rel_path))
    });
    for change in files {
        let host_path = layout.workspace.join(&change.rel_path);
        let Some((_, source)) = manifest::top_layer_for(layout, target_layers, &change.rel_path)
        else {
            return Err(CoreError::CommitFailed(format!(
                "no upper layer contains {}",
                change.rel_path.display()
            )));
        };
        if let Some(parent) = host_path.parent() {
            fs::create_dir_all(parent)?;
        }
        let metadata = fs::symlink_metadata(&source)?;
        if metadata.file_type().is_symlink() {
            let target = fs::read_link(&source)?;
            let temp = temp_path(&host_path);
            let _ = fs::remove_file(&temp);
            symlink(target, &temp)?;
            replace_path(&temp, &host_path)?;
        } else if metadata.is_file() {
            write_file_atomically(&source, &host_path, metadata.mode() & 0o7777)?;
        } else {
            return Err(CoreError::CommitFailed(format!(
                "refusing to commit non-regular entry {}",
                change.rel_path.display()
            )));
        }
    }
    Ok(())
}

fn write_file_atomically(source: &Path, target: &Path, mode: u32) -> Result<()> {
    let temp = temp_path(target);
    let mut reader = File::open(source)?;
    let mut writer = File::create(&temp)?;
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let read = reader.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        writer.write_all(&buffer[..read])?;
    }
    writer.sync_all()?;
    let metadata = fs::metadata(source)?;
    let safe_mode = mode & !0o6000;
    fs::set_permissions(&temp, fs::Permissions::from_mode(safe_mode))?;
    let mtime = filetime::FileTime::from_unix_time(
        metadata.mtime(),
        u32::try_from(metadata.mtime_nsec()).unwrap_or(0),
    );
    filetime::set_file_mtime(&temp, mtime)?;
    replace_path(&temp, target)
}

fn replace_path(temp: &Path, target: &Path) -> Result<()> {
    match fs::rename(temp, target) {
        Ok(()) => Ok(()),
        Err(error) => {
            let _ = fs::remove_file(target);
            fs::rename(temp, target).map_err(|second| {
                CoreError::Io(std::io::Error::new(
                    second.kind(),
                    format!(
                        "rename {} -> {} failed ({error}); {second}",
                        temp.display(),
                        target.display()
                    ),
                ))
            })
        }
    }
}

fn temp_path(target: &Path) -> PathBuf {
    let name = target
        .file_name()
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or_else(|| "entry".to_string());
    target.with_file_name(format!(".{name}.cowbox-{}.tmp", std::process::id()))
}

fn restore_ops(layout: &SessionLayout, ops: &[CommitOp]) -> Result<()> {
    for op in ops.iter().rev() {
        let host_path = layout.workspace.join(&op.rel_path);
        if op.created {
            remove_path_if_exists(&host_path)?;
            continue;
        }
        if let Some(backup) = &op.backup {
            remove_path_if_exists(&host_path)?;
            if fs::symlink_metadata(backup).is_ok() {
                if let Some(parent) = host_path.parent() {
                    fs::create_dir_all(parent)?;
                }
                copy_entry(backup, &host_path)?;
            }
        }
    }
    Ok(())
}

fn copy_entry(source: &Path, destination: &Path) -> Result<()> {
    let metadata = fs::symlink_metadata(source)?;
    if let Some(parent) = destination.parent() {
        fs::create_dir_all(parent)?;
    }
    if metadata.file_type().is_symlink() {
        let target = fs::read_link(source)?;
        let _ = fs::remove_file(destination);
        symlink(target, destination)?;
    } else if metadata.is_dir() {
        fs::create_dir_all(destination)?;
        fs::set_permissions(
            destination,
            fs::Permissions::from_mode(metadata.mode() & 0o7777),
        )?;
        for child in fs::read_dir(source)? {
            let child = child?;
            copy_entry(&child.path(), &destination.join(child.file_name()))?;
        }
    } else if metadata.is_file() {
        fs::copy(source, destination)?;
        fs::set_permissions(
            destination,
            fs::Permissions::from_mode(metadata.mode() & 0o7777),
        )?;
        let mtime = filetime::FileTime::from_unix_time(
            metadata.mtime(),
            u32::try_from(metadata.mtime_nsec()).unwrap_or(0),
        );
        filetime::set_file_mtime(destination, mtime)?;
    } else {
        return Err(CoreError::CommitFailed(format!(
            "cannot journal unsupported entry {}",
            source.display()
        )));
    }
    Ok(())
}

fn remove_path_if_exists(path: &Path) -> Result<()> {
    let Ok(metadata) = fs::symlink_metadata(path) else {
        return Ok(());
    };
    if metadata.is_dir() && !metadata.file_type().is_symlink() {
        fs::remove_dir_all(path)?;
    } else {
        fs::remove_file(path)?;
    }
    Ok(())
}

fn reject_vcs_metadata(layout: &SessionLayout, changes: &[FileChange]) -> Result<()> {
    for change in changes {
        if change
            .rel_path
            .components()
            .any(|component| component.as_os_str() == ".git")
        {
            return Err(CoreError::CommitFailed(format!(
                "refusing to commit VCS metadata: {}",
                change.rel_path.display()
            )));
        }
        if change.entry_kind == EntryKind::Directory {
            let host_path = layout.workspace.join(&change.rel_path);
            if host_path.join(".git").exists() {
                return Err(CoreError::CommitFailed(format!(
                    "refusing to commit a directory containing VCS metadata: {}",
                    change.rel_path.display()
                )));
            }
        }
    }
    Ok(())
}

fn write_ops(path: &Path, ops: &[CommitOp]) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let file = File::create(path)?;
    serde_json::to_writer_pretty(file, ops)?;
    Ok(())
}

fn action_name(kind: &ChangeKind) -> &'static str {
    match kind {
        ChangeKind::Added => "added",
        ChangeKind::Modified => "modified",
        ChangeKind::Deleted => "deleted",
        ChangeKind::SymlinkUpdated { .. } => "symlink",
        ChangeKind::TypeChanged { .. } => "type-changed",
    }
}

fn path_depth(path: &Path) -> usize {
    path.components().count()
}
