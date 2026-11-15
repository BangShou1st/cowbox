use crate::diff::compute_diff;
use crate::error::{CoreError, Result};
use crate::layout::SessionLayout;
use crate::manifest::{self, ManifestEntry};
use crate::model::{
    ChangeKind, CommitOptions, CommitRecoveryAction, CommitReport, DiffOptions, EntryKind,
    FileChange, PendingRecovery, RecoveryReport,
};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::fs::{MetadataExt, PermissionsExt, symlink};
use std::path::{Path, PathBuf};
use walkdir::WalkDir;

const JOURNAL_SCHEMA_VERSION: u32 = 1;
const SQLITE_HEADER: &[u8] = b"SQLite format 3\0";
const SQLITE_SIDECARS: [&str; 3] = ["-wal", "-shm", "-journal"];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum CommitPhase {
    Prepared,
    Applying,
    Restoring,
    Applied,
    Complete,
    RolledBack,
}

impl CommitPhase {
    fn incomplete(self) -> bool {
        matches!(
            self,
            Self::Prepared | Self::Applying | Self::Restoring | Self::Applied
        )
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct JournalMeta {
    schema_version: u32,
    session_id: String,
    workspace: PathBuf,
    created_at_unix_ms: i64,
    #[serde(default)]
    target_step: u64,
    target_layers: Vec<u64>,
    phase: CommitPhase,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum Fingerprint {
    Absent,
    File {
        mode: u32,
        size: u64,
        sha256: String,
        xattr_digest: String,
    },
    Directory {
        mode: u32,
        xattr_digest: String,
    },
    Symlink {
        target: PathBuf,
    },
    Other,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct CommitOp {
    rel_path: PathBuf,
    action: String,
    backup: Option<PathBuf>,
    #[serde(default)]
    original: Option<PathBuf>,
    created: bool,
    entry_kind: EntryKind,
    pre: Fingerprint,
    post: Fingerprint,
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

    if let Some(journal) = find_pending_journal(layout)? {
        return Err(CoreError::CommitRecoveryRequired(journal));
    }
    cleanup_finished_backups(layout)?;

    let diff_options = DiffOptions {
        include_excluded: options.include_excluded,
        line_stats: false,
        max_line_stat_files: 0,
        max_line_stat_bytes: 0,
        excluded: options.excluded.clone(),
    };
    let changes = compute_diff(layout, &[], target_layers, &diff_options)?;
    if changes.is_empty() {
        return Ok(CommitReport {
            changes,
            committed: !options.dry_run,
            conflicts: Vec::new(),
            journal: None,
        });
    }

    let base = manifest::load_manifest(&layout.base_manifest_path())?;
    let changes = expand_sqlite_sidecars(layout, target_layers, &base, changes)?;
    reject_vcs_metadata(layout, &changes)?;

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
    let mut meta = JournalMeta {
        schema_version: JOURNAL_SCHEMA_VERSION,
        session_id: layout.session_id.clone(),
        workspace: layout.workspace.clone(),
        created_at_unix_ms: chrono::Utc::now().timestamp_millis(),
        target_step: target_layers.last().copied().unwrap_or(0),
        target_layers: target_layers.to_vec(),
        phase: CommitPhase::Prepared,
    };
    write_json_atomic(&journal_dir.join("meta.json"), &meta)?;

    let ops = match build_ops(layout, target_layers, &base, &changes, &backup_dir) {
        Ok(ops) => ops,
        Err(error) => {
            let _ = fs::remove_dir_all(&journal_dir);
            return Err(error);
        }
    };
    if let Err(error) = write_json_atomic(&journal_dir.join("ops.json"), &ops) {
        let _ = fs::remove_dir_all(&journal_dir);
        return Err(error);
    }

    meta.phase = CommitPhase::Applying;
    write_json_atomic(&journal_dir.join("meta.json"), &meta)?;
    match apply_changes(layout, target_layers, &changes) {
        Ok(()) => {
            meta.phase = CommitPhase::Applied;
            if let Err(error) = write_json_atomic(&journal_dir.join("meta.json"), &meta) {
                return Err(CoreError::CommitFailed(format!(
                    "commit applied, but journal phase update failed: {error}; run `cowbox commit --recover` for journal {}",
                    journal_dir.display()
                )));
            }
            test_crash("commit-after-applied");
            Ok(CommitReport {
                changes,
                committed: true,
                conflicts,
                journal: Some(journal_dir),
            })
        }
        Err(error) => {
            meta.phase = CommitPhase::Restoring;
            let _ = write_json_atomic(&journal_dir.join("meta.json"), &meta);
            let restore = restore_ops(layout, &ops);
            if let Err(restore_error) = restore {
                return Err(CoreError::CommitFailed(format!(
                    "{error}; journal restore also failed: {restore_error}; journal kept at {}",
                    journal_dir.display()
                )));
            }
            if let Err(refresh_error) = refresh_base_manifest(layout, &ops, &journal_dir) {
                return Err(CoreError::CommitFailed(format!(
                    "{error}; host restored, but the base manifest refresh failed: {refresh_error}; journal kept at {}",
                    journal_dir.display()
                )));
            }
            meta.phase = CommitPhase::RolledBack;
            if let Err(meta_error) = write_json_atomic(&journal_dir.join("meta.json"), &meta) {
                return Err(CoreError::CommitFailed(format!(
                    "{error}; host restored, but the journal could not record the rollback: {meta_error}; journal kept at {}",
                    journal_dir.display()
                )));
            }
            remove_backup_material(&journal_dir);
            Err(CoreError::CommitFailed(format!(
                "{error}; host restored from journal {}",
                journal_dir.display()
            )))
        }
    }
}

pub fn recover_latest(layout: &SessionLayout) -> Result<RecoveryReport> {
    let journals = scan_journals(layout)?;
    let Some(record) = select_pending(&journals)? else {
        let Some(latest) = journals.last() else {
            return Ok(RecoveryReport {
                journal: None,
                action: CommitRecoveryAction::NoJournal,
                restored: 0,
            });
        };
        let action = match latest.meta.phase {
            CommitPhase::Complete => CommitRecoveryAction::AlreadyFinalized,
            CommitPhase::RolledBack => CommitRecoveryAction::AlreadyRolledBack,
            _ => CommitRecoveryAction::NoJournal,
        };
        return Ok(RecoveryReport {
            journal: Some(latest.dir.clone()),
            action,
            restored: 0,
        });
    };
    let mut meta = record.meta.clone();
    match meta.phase {
        CommitPhase::Prepared => {
            if record.dir.join("ops.json").exists() {
                let ops: Vec<CommitOp> = read_json(&record.dir.join("ops.json"))?;
                validate_ops(&record.dir, &ops)?;
            }
            meta.phase = CommitPhase::RolledBack;
            write_json_atomic(&record.dir.join("meta.json"), &meta)?;
            remove_backup_material(&record.dir);
            Ok(RecoveryReport {
                journal: Some(record.dir.clone()),
                action: CommitRecoveryAction::RestoreHost,
                restored: 0,
            })
        }
        CommitPhase::Applying | CommitPhase::Restoring => {
            let ops = load_ops(&record.dir)?;
            validate_ops(&record.dir, &ops)?;
            ensure_host_state(layout, &ops, HostState::PreOrPost)?;
            meta.phase = CommitPhase::Restoring;
            write_json_atomic(&record.dir.join("meta.json"), &meta)?;
            restore_ops(layout, &ops)?;
            refresh_base_manifest(layout, &ops, &record.dir)?;
            meta.phase = CommitPhase::RolledBack;
            write_json_atomic(&record.dir.join("meta.json"), &meta)?;
            remove_backup_material(&record.dir);
            Ok(RecoveryReport {
                journal: Some(record.dir.clone()),
                action: CommitRecoveryAction::RestoreHost,
                restored: ops.len(),
            })
        }
        CommitPhase::Applied => {
            let ops = load_ops(&record.dir)?;
            validate_ops(&record.dir, &ops)?;
            ensure_host_state(layout, &ops, HostState::PostOnly)?;
            Ok(RecoveryReport {
                journal: Some(record.dir.clone()),
                action: CommitRecoveryAction::FinalizeCommit,
                restored: 0,
            })
        }
        CommitPhase::Complete | CommitPhase::RolledBack => unreachable!("pending phase"),
    }
}

pub fn inspect_recovery(layout: &SessionLayout) -> Result<Option<PendingRecovery>> {
    let journals = scan_journals(layout)?;
    let Some(record) = select_pending(&journals)? else {
        return Ok(None);
    };
    let action = match record.meta.phase {
        CommitPhase::Prepared | CommitPhase::Applying | CommitPhase::Restoring => {
            CommitRecoveryAction::RestoreHost
        }
        CommitPhase::Applied => CommitRecoveryAction::FinalizeCommit,
        CommitPhase::Complete | CommitPhase::RolledBack => unreachable!("pending phase"),
    };
    let target_step = if record.meta.target_step == 0 {
        record.meta.target_layers.last().copied()
    } else {
        Some(record.meta.target_step)
    };
    Ok(Some(PendingRecovery {
        journal: record.dir.clone(),
        action,
        target_step,
    }))
}

pub fn finalize_commit(layout: &SessionLayout) -> Result<()> {
    let journals = scan_journals(layout)?;
    if let Some(record) = select_pending(&journals)? {
        if record.meta.phase != CommitPhase::Applied {
            return Err(CoreError::CommitFailed(format!(
                "journal {} is in phase {:?}, not applied; recovery must restore the host before finalization",
                record.dir.display(),
                record.meta.phase
            )));
        }
        let ops = load_ops(&record.dir)?;
        validate_ops(&record.dir, &ops)?;
        ensure_host_state(layout, &ops, HostState::PostOnly)?;
        let mut meta = record.meta.clone();
        meta.phase = CommitPhase::Complete;
        write_json_atomic(&record.dir.join("meta.json"), &meta)?;
        remove_backup_material(&record.dir);
        return Ok(());
    }
    match journals.last().map(|record| (record, record.meta.phase)) {
        Some((record, CommitPhase::Complete)) => {
            remove_backup_material(&record.dir);
            Ok(())
        }
        Some((record, CommitPhase::RolledBack)) => Err(CoreError::CommitFailed(format!(
            "journal {} was rolled back; there is no applied commit to finalize",
            record.dir.display()
        ))),
        Some((record, phase)) => Err(CoreError::CommitFailed(format!(
            "journal {} is in unexpected phase {:?}",
            record.dir.display(),
            phase
        ))),
        None => Err(CoreError::CommitFailed(
            "no commit journal was found".to_string(),
        )),
    }
}

fn build_ops(
    layout: &SessionLayout,
    target_layers: &[u64],
    base: &BTreeMap<PathBuf, ManifestEntry>,
    changes: &[FileChange],
    backup_dir: &Path,
) -> Result<Vec<CommitOp>> {
    let mut ops = Vec::with_capacity(changes.len());
    let mut backup_links = BTreeMap::new();
    let original_dir = backup_dir.with_file_name("original");
    for change in changes {
        let host_path = layout.workspace.join(&change.rel_path);
        let pre = fingerprint(&host_path)?;
        let (backup_path, original_path) = if matches!(pre, Fingerprint::Absent) {
            (None, None)
        } else {
            let destination = backup_dir.join(&change.rel_path);
            backup_entry(&host_path, &destination, &mut backup_links)?;
            let original =
                create_original_backup(&host_path, &original_dir.join(&change.rel_path))?;
            (Some(destination), original)
        };
        let post = match change.kind {
            ChangeKind::Deleted => Fingerprint::Absent,
            _ => {
                let source = manifest::visible_path(layout, target_layers, &change.rel_path, base)
                    .ok_or_else(|| {
                        CoreError::CommitFailed(format!(
                            "no visible sandbox entry for {}",
                            change.rel_path.display()
                        ))
                    })?;
                let mut post = fingerprint(&source)?;
                normalize_applied_fingerprint(&mut post);
                post
            }
        };
        ops.push(CommitOp {
            rel_path: change.rel_path.clone(),
            action: action_name(&change.kind).to_string(),
            backup: backup_path,
            original: original_path,
            created: matches!(pre, Fingerprint::Absent),
            entry_kind: change.entry_kind,
            pre,
            post,
        });
    }
    Ok(ops)
}

fn normalize_applied_fingerprint(fingerprint: &mut Fingerprint) {
    match fingerprint {
        Fingerprint::File { mode, .. } | Fingerprint::Directory { mode, .. } => {
            *mode &= !0o6000;
        }
        Fingerprint::Absent | Fingerprint::Symlink { .. } | Fingerprint::Other => {}
    }
}

fn fingerprint(path: &Path) -> Result<Fingerprint> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(Fingerprint::Absent);
        }
        Err(error) => return Err(error.into()),
    };
    let kind = manifest::entry_kind(&metadata);
    match kind {
        EntryKind::File => Ok(Fingerprint::File {
            mode: metadata.mode() & 0o7777,
            size: metadata.len(),
            sha256: hash_file(path)?,
            xattr_digest: crate::xattr::digest_checked(path)?,
        }),
        EntryKind::Directory => Ok(Fingerprint::Directory {
            mode: metadata.mode() & 0o7777,
            xattr_digest: crate::xattr::digest_checked(path)?,
        }),
        EntryKind::Symlink => Ok(Fingerprint::Symlink {
            target: fs::read_link(path)?,
        }),
        EntryKind::Other => Ok(Fingerprint::Other),
    }
}

fn hash_file(path: &Path) -> Result<String> {
    let mut file = File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(hex::encode(hasher.finalize()))
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
        remove_path_if_exists(&layout.workspace.join(&change.rel_path))?;
    }

    let base = manifest::load_manifest(&layout.base_manifest_path())?;
    let mut directories: Vec<&FileChange> = changes
        .iter()
        .filter(|change| {
            change.entry_kind == EntryKind::Directory && !matches!(change.kind, ChangeKind::Deleted)
        })
        .collect();
    directories.sort_by_key(|change| path_depth(&change.rel_path));
    for change in directories {
        let host_path = layout.workspace.join(&change.rel_path);
        fs::create_dir_all(&host_path)?;
        let source = manifest::visible_path(layout, target_layers, &change.rel_path, &base)
            .ok_or_else(|| {
                CoreError::CommitFailed(format!(
                    "no visible sandbox directory for {}",
                    change.rel_path.display()
                ))
            })?;
        let metadata = fs::symlink_metadata(&source)?;
        crate::xattr::copy(&source, &host_path)?;
        fs::set_permissions(
            &host_path,
            fs::Permissions::from_mode(metadata.mode() & !0o6000),
        )?;
    }

    let mut files: Vec<&FileChange> = changes
        .iter()
        .filter(|change| {
            change.entry_kind != EntryKind::Directory && !matches!(change.kind, ChangeKind::Deleted)
        })
        .collect();
    let mut link_groups: BTreeMap<(u64, u64), Vec<PathBuf>> = BTreeMap::new();
    for change in &files {
        if change.entry_kind != EntryKind::File {
            continue;
        }
        let Some(source) = manifest::visible_path(layout, target_layers, &change.rel_path, &base)
        else {
            continue;
        };
        let metadata = fs::symlink_metadata(&source)?;
        if metadata.is_file() && metadata.nlink() > 1 {
            link_groups
                .entry((metadata.dev(), metadata.ino()))
                .or_default()
                .push(change.rel_path.clone());
        }
    }
    let mut primary_by_member = BTreeMap::new();
    for group in link_groups.values_mut() {
        group.sort();
        if let Some(primary) = group.first() {
            for member in group.iter() {
                primary_by_member.insert(member.clone(), primary.clone());
            }
        }
    }

    files.sort_by_key(|change| {
        let symlink_last = change.entry_kind == EntryKind::Symlink;
        let secondary = primary_by_member
            .get(&change.rel_path)
            .map(|primary| primary != &change.rel_path)
            .unwrap_or(false);
        (
            symlink_last,
            secondary,
            path_depth(&change.rel_path),
            change.rel_path.clone(),
        )
    });
    let mut first_write_seen = false;
    for change in files {
        let host_path = layout.workspace.join(&change.rel_path);
        if let Some(parent) = host_path.parent() {
            fs::create_dir_all(parent)?;
        }
        let source = manifest::visible_path(layout, target_layers, &change.rel_path, &base)
            .ok_or_else(|| {
                CoreError::CommitFailed(format!(
                    "no visible sandbox entry for {}",
                    change.rel_path.display()
                ))
            })?;
        let metadata = fs::symlink_metadata(&source)?;
        if metadata.file_type().is_symlink() {
            let target = fs::read_link(&source)?;
            let temp = temp_path(&host_path);
            let _ = fs::remove_file(&temp);
            symlink(target, &temp)?;
            replace_path(&temp, &host_path)?;
            test_crash_once(&mut first_write_seen, "commit-after-first-write");
        } else if metadata.is_file() {
            if let Some(primary) = primary_by_member.get(&change.rel_path) {
                if primary != &change.rel_path {
                    let primary_host = layout.workspace.join(primary);
                    let temp = temp_path(&host_path);
                    let _ = fs::remove_file(&temp);
                    fs::hard_link(&primary_host, &temp)?;
                    replace_path(&temp, &host_path)?;
                    test_crash_once(&mut first_write_seen, "commit-after-first-write");
                    continue;
                }
            }
            write_file_atomically(&source, &host_path, metadata.mode() & 0o7777)?;
            test_crash_once(&mut first_write_seen, "commit-after-first-write");
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
    let _ = fs::remove_file(&temp);
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
    let metadata = fs::symlink_metadata(source)?;
    let safe_mode = mode & !0o6000;
    crate::xattr::copy(source, &temp)?;
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
        Ok(()) => {
            sync_parent(target);
            Ok(())
        }
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
            })?;
            sync_parent(target);
            Ok(())
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
    let mut links = BTreeMap::new();
    let mut restored = false;
    for op in ops.iter().rev() {
        let host_path = layout.workspace.join(&op.rel_path);
        if op.created {
            remove_path_if_exists(&host_path)?;
            if !restored {
                restored = true;
                test_crash("recover-after-first-restore");
            }
            continue;
        }
        let backup = op.backup.as_ref().ok_or_else(|| {
            CoreError::CommitFailed(format!(
                "journal operation for {} has no backup",
                op.rel_path.display()
            ))
        })?;
        let backup_state = fingerprint(backup)?;
        if backup_state != op.pre {
            return Err(CoreError::CommitFailed(format!(
                "journal backup for {} changed after it was captured; recovery material was preserved",
                op.rel_path.display()
            )));
        }
        let source = match &op.original {
            Some(original) => {
                let original_state = fingerprint(original)?;
                if original_state != op.pre {
                    return Err(CoreError::CommitFailed(format!(
                        "original hardlink backup for {} changed after it was captured; recovery material was preserved",
                        op.rel_path.display()
                    )));
                }
                original
            }
            None => backup,
        };
        remove_path_if_exists(&host_path)?;
        if let Some(parent) = host_path.parent() {
            fs::create_dir_all(parent)?;
        }
        restore_entry(source, &host_path, &mut links)?;
        if !restored {
            restored = true;
            test_crash("recover-after-first-restore");
        }
    }
    Ok(())
}

fn refresh_base_manifest(
    layout: &SessionLayout,
    ops: &[CommitOp],
    journal_dir: &Path,
) -> Result<()> {
    let mut base = manifest::load_manifest(&layout.base_manifest_path())?;
    let backup_links = backup_link_counts(journal_dir)?;
    let mut refresh = BTreeSet::new();
    let mut absent = BTreeSet::new();
    for op in ops {
        refresh.insert(op.rel_path.clone());
        if matches!(op.pre, Fingerprint::Absent) {
            absent.insert(op.rel_path.clone());
        }
        if matches!(op.pre, Fingerprint::Directory { .. }) {
            for path in base.keys() {
                if path != &op.rel_path && path.starts_with(&op.rel_path) {
                    refresh.insert(path.clone());
                }
            }
        }
    }
    for rel_path in refresh {
        if absent.contains(&rel_path) {
            base.remove(&rel_path);
            continue;
        }
        let host_path = layout.workspace.join(&rel_path);
        let metadata = fs::symlink_metadata(&host_path).map_err(|error| {
            CoreError::CommitFailed(format!(
                "restored path {} cannot be re-read for the base manifest: {error}",
                rel_path.display()
            ))
        })?;
        let mut entry = ManifestEntry::from_metadata(rel_path.clone(), &host_path, &metadata)?;
        if let (Some(dev), Some(ino), Some(nlink)) = (entry.dev, entry.ino, entry.nlink) {
            if let Some(backup_links) = backup_links.get(&(dev, ino)) {
                entry.nlink = Some(nlink.saturating_sub(*backup_links));
            }
        }
        base.insert(rel_path, entry);
    }
    manifest::write_manifest(&layout.base_manifest_path(), &base)
}

fn backup_link_counts(journal_dir: &Path) -> Result<BTreeMap<(u64, u64), u64>> {
    let mut counts = BTreeMap::new();
    for root in [journal_dir.join("backup"), journal_dir.join("original")] {
        if !root.exists() {
            continue;
        }
        for entry in WalkDir::new(&root).follow_links(false) {
            let entry = entry.map_err(|error| CoreError::StateCorrupted {
                step: 0,
                reason: format!("failed to walk commit backup {}: {error}", root.display()),
            })?;
            let metadata = fs::symlink_metadata(entry.path())?;
            if metadata.is_file() {
                *counts.entry((metadata.dev(), metadata.ino())).or_insert(0) += 1;
            }
        }
    }
    Ok(counts)
}

fn backup_entry(
    source: &Path,
    destination: &Path,
    links: &mut BTreeMap<(u64, u64), PathBuf>,
) -> Result<()> {
    let metadata = fs::symlink_metadata(source)?;
    if let Some(parent) = destination.parent() {
        fs::create_dir_all(parent)?;
    }
    if metadata.file_type().is_symlink() {
        let _ = fs::remove_file(destination);
        symlink(fs::read_link(source)?, destination)?;
    } else if metadata.is_dir() {
        fs::create_dir_all(destination)?;
        fs::set_permissions(destination, fs::Permissions::from_mode(0o700))?;
        crate::xattr::copy(source, destination)?;
        fs::set_permissions(
            destination,
            fs::Permissions::from_mode(metadata.mode() & 0o7777),
        )?;
        for child in fs::read_dir(source)? {
            let child = child?;
            backup_entry(&child.path(), &destination.join(child.file_name()), links)?;
        }
    } else if metadata.is_file() {
        let _ = fs::remove_file(destination);
        if metadata.nlink() > 1 {
            let key = (metadata.dev(), metadata.ino());
            if let Some(existing) = links.get(&key) {
                let _ = fs::remove_file(destination);
                fs::hard_link(existing, destination)?;
                return Ok(());
            }
            links.insert(key, destination.to_path_buf());
        }
        fs::copy(source, destination)?;
        fs::set_permissions(destination, fs::Permissions::from_mode(0o600))?;
        crate::xattr::copy(source, destination)?;
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

fn create_original_backup(source: &Path, destination: &Path) -> Result<Option<PathBuf>> {
    match create_original_entry(source, destination) {
        Ok(true) => Ok(Some(destination.to_path_buf())),
        Ok(false) => {
            let _ = remove_path_if_exists(destination);
            Ok(None)
        }
        Err(error) => {
            let _ = remove_path_if_exists(destination);
            Err(error)
        }
    }
}

fn create_original_entry(source: &Path, destination: &Path) -> Result<bool> {
    let metadata = fs::symlink_metadata(source)?;
    if let Some(parent) = destination.parent() {
        fs::create_dir_all(parent)?;
    }
    if metadata.is_dir() && !metadata.file_type().is_symlink() {
        fs::create_dir_all(destination)?;
        fs::set_permissions(destination, fs::Permissions::from_mode(0o700))?;
        crate::xattr::copy(source, destination)?;
        for child in fs::read_dir(source)? {
            let child = child?;
            if !create_original_entry(&child.path(), &destination.join(child.file_name()))? {
                return Ok(false);
            }
        }
        fs::set_permissions(
            destination,
            fs::Permissions::from_mode(metadata.mode() & 0o7777),
        )?;
        return Ok(true);
    }
    if !metadata.is_file() && !metadata.file_type().is_symlink() {
        return Ok(false);
    }
    let _ = fs::remove_file(destination);
    match fs::hard_link(source, destination) {
        Ok(()) => Ok(true),
        Err(_) => {
            let _ = fs::remove_file(destination);
            Ok(false)
        }
    }
}

fn restore_entry(
    source: &Path,
    destination: &Path,
    links: &mut BTreeMap<(u64, u64), PathBuf>,
) -> Result<()> {
    let metadata = fs::symlink_metadata(source)?;
    if let Some(parent) = destination.parent() {
        fs::create_dir_all(parent)?;
    }
    if metadata.file_type().is_symlink() {
        let _ = fs::remove_file(destination);
        if fs::hard_link(source, destination).is_err() {
            symlink(fs::read_link(source)?, destination)?;
        }
    } else if metadata.is_dir() {
        fs::create_dir_all(destination)?;
        fs::set_permissions(destination, fs::Permissions::from_mode(0o700))?;
        crate::xattr::copy(source, destination)?;
        fs::set_permissions(
            destination,
            fs::Permissions::from_mode(metadata.mode() & 0o7777),
        )?;
        for child in fs::read_dir(source)? {
            let child = child?;
            restore_entry(&child.path(), &destination.join(child.file_name()), links)?;
        }
    } else if metadata.is_file() {
        let _ = fs::remove_file(destination);
        if fs::hard_link(source, destination).is_ok() {
            return Ok(());
        }
        if metadata.nlink() > 1 {
            let key = (metadata.dev(), metadata.ino());
            if let Some(existing) = links.get(&key) {
                let _ = fs::remove_file(destination);
                fs::hard_link(existing, destination)?;
                return Ok(());
            }
            links.insert(key, destination.to_path_buf());
        }
        fs::copy(source, destination)?;
        fs::set_permissions(destination, fs::Permissions::from_mode(0o600))?;
        crate::xattr::copy(source, destination)?;
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
            "cannot restore unsupported entry {}",
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

fn expand_sqlite_sidecars(
    layout: &SessionLayout,
    target_layers: &[u64],
    base: &BTreeMap<PathBuf, ManifestEntry>,
    changes: Vec<FileChange>,
) -> Result<Vec<FileChange>> {
    let mut by_path: BTreeMap<PathBuf, FileChange> = changes
        .into_iter()
        .map(|change| (change.rel_path.clone(), change))
        .collect();
    let mut mains = Vec::new();
    for path in by_path.keys() {
        if !is_sidecar_path(path) && is_sqlite_path(layout, target_layers, base, path)? {
            mains.push(path.clone());
        }
    }

    for main in mains {
        for suffix in SQLITE_SIDECARS {
            let sidecar = PathBuf::from(format!("{}{}", main.to_string_lossy(), suffix));
            if by_path.contains_key(&sidecar) {
                continue;
            }
            match manifest::top_layer_visible_path(layout, target_layers, &sidecar) {
                Some(source) => {
                    by_path.insert(
                        sidecar.clone(),
                        synthesized_change(&sidecar, &source, base.get(&sidecar))?,
                    );
                }
                None => {
                    if let Some(entry) = base.get(&sidecar) {
                        by_path.insert(
                            sidecar.clone(),
                            FileChange {
                                rel_path: sidecar,
                                kind: ChangeKind::Deleted,
                                entry_kind: entry.kind,
                                size_delta: -(i64::try_from(entry.size).unwrap_or(i64::MAX)),
                                lines_added: None,
                                lines_removed: None,
                                mode: Some(entry.mode),
                                excluded: false,
                            },
                        );
                    } else if let Ok(metadata) =
                        fs::symlink_metadata(layout.workspace.join(&sidecar))
                    {
                        by_path.insert(
                            sidecar.clone(),
                            FileChange {
                                rel_path: sidecar,
                                kind: ChangeKind::Deleted,
                                entry_kind: manifest::entry_kind(&metadata),
                                size_delta: -(i64::try_from(metadata.len()).unwrap_or(i64::MAX)),
                                lines_added: None,
                                lines_removed: None,
                                mode: Some(metadata.mode() & 0o7777),
                                excluded: false,
                            },
                        );
                    }
                }
            }
        }
    }

    Ok(by_path.into_values().collect())
}

fn synthesized_change(
    rel_path: &Path,
    source: &Path,
    base_entry: Option<&ManifestEntry>,
) -> Result<FileChange> {
    let metadata = fs::symlink_metadata(source)?;
    let entry_kind = manifest::entry_kind(&metadata);
    let kind = match base_entry {
        None => ChangeKind::Added,
        Some(entry) if entry.kind != entry_kind => ChangeKind::TypeChanged {
            from: entry.kind,
            to: entry_kind,
        },
        Some(_) => ChangeKind::Modified,
    };
    let base_size = base_entry
        .map(|entry| i64::try_from(entry.size).unwrap_or(i64::MAX))
        .unwrap_or(0);
    Ok(FileChange {
        rel_path: rel_path.to_path_buf(),
        kind,
        entry_kind,
        size_delta: i64::try_from(metadata.len()).unwrap_or(i64::MAX) - base_size,
        lines_added: None,
        lines_removed: None,
        mode: Some(metadata.mode() & 0o7777),
        excluded: false,
    })
}

fn is_sidecar_path(path: &Path) -> bool {
    let value = path.to_string_lossy();
    SQLITE_SIDECARS.iter().any(|suffix| value.ends_with(suffix))
}

fn is_sqlite_path(
    layout: &SessionLayout,
    target_layers: &[u64],
    base: &BTreeMap<PathBuf, ManifestEntry>,
    path: &Path,
) -> Result<bool> {
    if let Some(source) = manifest::visible_path(layout, target_layers, path, base) {
        if is_sqlite_file(&source)? {
            return Ok(true);
        }
    }
    if base.contains_key(path) {
        return is_sqlite_file(&layout.workspace.join(path));
    }
    Ok(false)
}

fn is_sqlite_file(path: &Path) -> Result<bool> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error.into()),
    };
    if !metadata.is_file() {
        return Ok(false);
    }
    let mut file = match File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error.into()),
    };
    let mut header = [0u8; 16];
    match file.read_exact(&mut header) {
        Ok(()) => Ok(header.as_slice() == SQLITE_HEADER),
        Err(error) if error.kind() == std::io::ErrorKind::UnexpectedEof => Ok(false),
        Err(error) => Err(error.into()),
    }
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

#[derive(Debug, Clone)]
struct JournalRecord {
    dir: PathBuf,
    meta: JournalMeta,
}

fn scan_journals(layout: &SessionLayout) -> Result<Vec<JournalRecord>> {
    let root = layout.session_dir.join("commit");
    let entries = match fs::read_dir(&root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error.into()),
    };
    let mut journals = Vec::new();
    for entry in entries {
        let entry = entry?;
        if !entry.path().is_dir() {
            continue;
        }
        let meta_path = entry.path().join("meta.json");
        if !meta_path.exists() {
            if entry.path().join("ops.json").exists()
                || entry.path().join("backup").exists()
                || entry.path().join("original").exists()
            {
                return Err(CoreError::StateCorrupted {
                    step: 0,
                    reason: format!(
                        "commit journal {} has recovery material but no meta.json",
                        entry.path().display()
                    ),
                });
            }
            continue;
        }
        let meta: JournalMeta = read_json(&meta_path)?;
        if meta.schema_version != JOURNAL_SCHEMA_VERSION {
            return Err(CoreError::StateCorrupted {
                step: 0,
                reason: format!(
                    "unsupported commit journal schema {} at {}",
                    meta.schema_version,
                    meta_path.display()
                ),
            });
        }
        if meta.session_id != layout.session_id {
            return Err(CoreError::StateCorrupted {
                step: 0,
                reason: format!(
                    "commit journal {} belongs to session {}, expected {}",
                    entry.path().display(),
                    meta.session_id,
                    layout.session_id
                ),
            });
        }
        if meta.workspace != layout.workspace {
            return Err(CoreError::StateCorrupted {
                step: 0,
                reason: format!(
                    "commit journal {} records workspace {}, expected {}",
                    entry.path().display(),
                    meta.workspace.display(),
                    layout.workspace.display()
                ),
            });
        }
        journals.push(JournalRecord {
            dir: entry.path(),
            meta,
        });
    }
    journals.sort_by_key(|record| (record.meta.created_at_unix_ms, record.dir.clone()));
    Ok(journals)
}

fn select_pending(journals: &[JournalRecord]) -> Result<Option<&JournalRecord>> {
    let pending = journals
        .iter()
        .filter(|record| record.meta.phase.incomplete())
        .collect::<Vec<_>>();
    if pending.len() > 1 {
        let paths = pending
            .iter()
            .map(|record| record.dir.display().to_string())
            .collect::<Vec<_>>()
            .join(", ");
        return Err(CoreError::StateCorrupted {
            step: 0,
            reason: format!(
                "multiple incomplete commit journals require manual inspection: {paths}"
            ),
        });
    }
    Ok(pending.into_iter().next())
}

fn find_pending_journal(layout: &SessionLayout) -> Result<Option<PathBuf>> {
    let journals = scan_journals(layout)?;
    Ok(select_pending(&journals)?.map(|record| record.dir.clone()))
}

fn load_ops(journal_dir: &Path) -> Result<Vec<CommitOp>> {
    let path = journal_dir.join("ops.json");
    if !path.is_file() {
        return Err(CoreError::CommitFailed(format!(
            "commit journal {} has no operation manifest; recovery material was preserved",
            journal_dir.display()
        )));
    }
    read_json(&path)
}

fn validate_ops(journal_dir: &Path, ops: &[CommitOp]) -> Result<()> {
    if ops.is_empty() {
        return Err(CoreError::CommitFailed(format!(
            "commit journal {} has an empty operation manifest",
            journal_dir.display()
        )));
    }
    let backup_root = journal_dir.join("backup");
    let original_root = journal_dir.join("original");
    let mut seen = BTreeSet::new();
    for op in ops {
        if !op.rel_path.is_relative()
            || op.rel_path.components().any(|component| {
                !matches!(
                    component,
                    std::path::Component::Normal(_) | std::path::Component::CurDir
                )
            })
            || op
                .rel_path
                .components()
                .any(|component| component.as_os_str() == ".git")
        {
            return Err(CoreError::CommitFailed(format!(
                "commit journal {} contains an unsafe path {:?}",
                journal_dir.display(),
                op.rel_path
            )));
        }
        if !seen.insert(op.rel_path.clone()) {
            return Err(CoreError::CommitFailed(format!(
                "commit journal {} contains duplicate path {}",
                journal_dir.display(),
                op.rel_path.display()
            )));
        }
        if matches!(op.pre, Fingerprint::Absent) {
            if !op.created || op.backup.is_some() {
                return Err(CoreError::CommitFailed(format!(
                    "commit journal {} has inconsistent backup metadata for {}",
                    journal_dir.display(),
                    op.rel_path.display()
                )));
            }
            continue;
        }
        if op.created {
            return Err(CoreError::CommitFailed(format!(
                "commit journal {} marks {} as created despite a present pre-state",
                journal_dir.display(),
                op.rel_path.display()
            )));
        }
        let backup = op.backup.as_ref().ok_or_else(|| {
            CoreError::CommitFailed(format!(
                "commit journal {} has no backup for {}",
                journal_dir.display(),
                op.rel_path.display()
            ))
        })?;
        if !backup.starts_with(&backup_root) {
            return Err(CoreError::CommitFailed(format!(
                "commit journal {} points outside its backup directory for {}",
                journal_dir.display(),
                op.rel_path.display()
            )));
        }
        if fs::symlink_metadata(backup).is_err() {
            return Err(CoreError::CommitFailed(format!(
                "commit journal {} is missing the backup for {}",
                journal_dir.display(),
                op.rel_path.display()
            )));
        }
        if let Some(original) = &op.original {
            if !original.starts_with(&original_root) || fs::symlink_metadata(original).is_err() {
                return Err(CoreError::CommitFailed(format!(
                    "commit journal {} has an invalid original hardlink backup for {}",
                    journal_dir.display(),
                    op.rel_path.display()
                )));
            }
        }
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HostState {
    PreOrPost,
    PostOnly,
}

fn ensure_host_state(layout: &SessionLayout, ops: &[CommitOp], expected: HostState) -> Result<()> {
    let mut conflicts = Vec::new();
    for op in ops {
        let current = fingerprint(&layout.workspace.join(&op.rel_path))?;
        let matches = match expected {
            HostState::PreOrPost => current == op.pre || current == op.post,
            HostState::PostOnly => current == op.post,
        };
        if !matches {
            conflicts.push(op.rel_path.clone());
        }
    }
    if !conflicts.is_empty() {
        let paths = conflicts
            .iter()
            .map(|path| path.display().to_string())
            .collect::<Vec<_>>()
            .join(", ");
        return Err(CoreError::CommitFailed(format!(
            "refusing commit recovery: affected host paths changed after the recorded pre/post state: {paths}; backups were preserved"
        )));
    }
    Ok(())
}

fn cleanup_finished_backups(layout: &SessionLayout) -> Result<()> {
    for record in scan_journals(layout)? {
        if matches!(
            record.meta.phase,
            CommitPhase::Complete | CommitPhase::RolledBack
        ) {
            remove_backup_material(&record.dir);
        }
    }
    Ok(())
}

fn remove_backup_material(journal_dir: &Path) {
    let _ = fs::remove_dir_all(journal_dir.join("backup"));
    let _ = fs::remove_dir_all(journal_dir.join("original"));
}

fn test_crash(point: &str) {
    if !cfg!(debug_assertions) {
        return;
    }
    if std::env::var_os("COWBOX_TEST_CRASH").as_deref() != Some(std::ffi::OsStr::new(point)) {
        return;
    }
    let _ = rustix::process::kill_process(rustix::process::getpid(), rustix::process::Signal::KILL);
    std::process::abort();
}

fn test_crash_once(seen: &mut bool, point: &str) {
    if !*seen {
        *seen = true;
        test_crash(point);
    }
}

fn write_json_atomic<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let temp = path.with_extension(format!("tmp-{}", std::process::id()));
    let data = serde_json::to_vec_pretty(value)?;
    {
        let mut file = OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .open(&temp)?;
        file.write_all(&data)?;
        file.sync_all()?;
    }
    fs::rename(&temp, path)?;
    sync_parent(path);
    Ok(())
}

fn read_json<T: DeserializeOwned>(path: &Path) -> Result<T> {
    let data = fs::read(path)?;
    Ok(serde_json::from_slice(&data)?)
}

fn sync_parent(path: &Path) {
    if let Some(parent) = path.parent() {
        if let Ok(directory) = File::open(parent) {
            let _ = directory.sync_all();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn commit_fixture() -> (TempDir, SessionLayout) {
        let root = tempfile::Builder::new()
            .prefix("cowbox-commit-")
            .tempdir_in("/tmp")
            .expect("fixture");
        let workspace = root.path().join("workspace");
        let state = root.path().join("state");
        fs::create_dir_all(&workspace).expect("workspace");
        fs::create_dir_all(&state).expect("state");
        fs::create_dir_all(workspace.join(".git")).expect("git marker");
        let layout = SessionLayout::new("commit-test", &workspace, &state);
        fs::create_dir_all(&layout.session_dir).expect("session");
        let host = workspace.join("file.txt");
        fs::write(&host, "before\n").expect("host before");
        manifest::capture_base_manifest(&workspace, &layout.base_manifest_path())
            .expect("base manifest");
        let upper = layout.upper_dir(1);
        fs::create_dir_all(&upper).expect("upper");
        fs::write(upper.join("file.txt"), "after\n").expect("upper file");
        (root, layout)
    }

    #[test]
    fn successful_commit_keeps_backup_until_finalize() {
        let (_root, layout) = commit_fixture();
        let report = commit_layers(&layout, &[1], &CommitOptions::default()).expect("commit");
        assert!(report.committed);
        let journal = report.journal.expect("journal");
        let meta: JournalMeta = read_json(&journal.join("meta.json")).expect("meta");
        assert_eq!(meta.phase, CommitPhase::Applied);
        assert_eq!(
            fs::read_to_string(layout.workspace.join("file.txt")).expect("host"),
            "after\n"
        );
        assert_eq!(
            fs::read_to_string(journal.join("backup/file.txt")).expect("backup"),
            "before\n"
        );
        assert!(journal.join("original/file.txt").is_file());
        assert!(inspect_recovery(&layout).expect("inspect").is_some());

        finalize_commit(&layout).expect("finalize");
        let meta: JournalMeta = read_json(&journal.join("meta.json")).expect("meta");
        assert_eq!(meta.phase, CommitPhase::Complete);
        assert!(!journal.join("backup").exists());
        assert!(!journal.join("original").exists());
        assert!(inspect_recovery(&layout).expect("inspect").is_none());

        finalize_commit(&layout).expect("finalize is idempotent");
    }
}
