use super::error::{WinError, WinResult};
use super::model::*;
use super::paths;
use super::scan;
use super::session::WinSession;
use super::store::clear_readonly;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Default)]
pub struct WinFaults {
    pub abort_after_backup: bool,
    pub abort_after_apply: Option<usize>,
    pub abort_after_operation: Option<usize>,
    pub abort_before_applied: bool,
    pub abort_after_applied: bool,
    pub abort_during_recover: Option<usize>,
    pub mid_commit_edit: Option<WinMidCommitEdit>,
}

#[derive(Debug, Clone)]
pub struct WinMidCommitEdit {
    pub after_apply: usize,
    pub rel_path: String,
    pub bytes: Vec<u8>,
}

impl WinFaults {
    pub fn from_env() -> Self {
        let mut faults = Self::default();
        if let Ok(value) = std::env::var("COWBOX_WIN_FAULT") {
            for point in value
                .split(',')
                .map(str::trim)
                .filter(|point| !point.is_empty())
            {
                match point {
                    "abort-after-backup" => faults.abort_after_backup = true,
                    "abort-before-applied" => faults.abort_before_applied = true,
                    "abort-after-applied" => faults.abort_after_applied = true,
                    other => {
                        if let Some(count) = other.strip_prefix("abort-after-apply:") {
                            faults.abort_after_apply = count.parse().ok();
                        } else if let Some(count) = other.strip_prefix("abort-after-operation:") {
                            faults.abort_after_operation = count.parse().ok();
                        } else if let Some(count) = other.strip_prefix("abort-during-recover:") {
                            faults.abort_during_recover = count.parse().ok();
                        }
                    }
                }
            }
        }
        if let Ok(value) = std::env::var("COWBOX_WIN_FAULT_MID_EDIT") {
            let mut parts = value.splitn(3, '|');
            if let (Some(count), Some(rel_path), Some(bytes)) =
                (parts.next(), parts.next(), parts.next())
            {
                if let Ok(after_apply) = count.parse() {
                    faults.mid_commit_edit = Some(WinMidCommitEdit {
                        after_apply,
                        rel_path: rel_path.to_string(),
                        bytes: bytes.as_bytes().to_vec(),
                    });
                }
            }
        }
        faults
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum WinJournalPhase {
    Prepared,
    BackedUp,
    Applying,
    Applied,
    Finalized,
    Restoring,
    Restored,
    RestoreFailed,
    DriftAborted,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum WinOpKind {
    DeleteFile,
    DeleteDir,
    CreateDir,
    WriteFile,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct WinJournalOp {
    kind: WinOpKind,
    rel_path: String,
    object: Option<String>,
    backup: Option<String>,
    readonly: bool,
    mtime_unix_ms: Option<i64>,
    existed: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct WinBackup {
    rel_path: String,
    sha256: String,
    size: u64,
    readonly: bool,
    mtime_unix_ms: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct WinJournal {
    schema_version: u32,
    journal_id: String,
    session_id: String,
    step: u64,
    workspace: PathBuf,
    base_manifest_sha256: String,
    target_manifest_sha256: String,
    force: bool,
    phase: WinJournalPhase,
    created_at_unix_ms: i64,
    updated_at_unix_ms: i64,
    ops: Vec<WinJournalOp>,
    backups: BTreeMap<String, WinBackup>,
    applied_ops: usize,
    #[serde(default)]
    active_op: Option<usize>,
    conflicts: Vec<String>,
    error: Option<String>,
}

pub fn preview(
    session: &WinSession,
    step: Option<u64>,
    force: bool,
) -> WinResult<WinCommitPreview> {
    session.ensure_active()?;
    let step = step.unwrap_or(session.record.dag.head_step);
    let base = session.base_manifest()?;
    let target = session.manifest_at(step)?;
    let host = scan::scan_tree(&session.record.workspace, &session.excludes())?;
    let drift = scan::changed_keys(&base, &host);
    let changes = scan::diff_manifests(&base, &target);
    let mut unsupported = Vec::new();
    for change in &changes {
        if paths::is_excluded_key(&change.rel_path, &[]) {
            unsupported.push(WinUnsupported {
                rel_path: change.rel_path.clone(),
                code: "WIN_GIT_PROTECTED".to_string(),
                message: ".git and other excluded paths are never written".to_string(),
            });
        }
        if matches!(
            change.kind,
            WinChangeKind::Deleted | WinChangeKind::TypeChanged
        ) {
            continue;
        }
        let host_path = paths::join_key(&session.record.workspace, &change.rel_path)?;
        if host_path.exists() && host_path.is_file() {
            if change.kind == WinChangeKind::Modified {
                if let Ok(links) = scan::file_hardlinks(&host_path) {
                    if links > 1 {
                        unsupported.push(WinUnsupported {
                            rel_path: change.rel_path.clone(),
                            code: "WIN_HARDLINK_UNSUPPORTED".to_string(),
                            message: format!(
                                "{} has {links} hard links; replacing one name would not preserve link semantics",
                                change.rel_path
                            ),
                        });
                    }
                }
            }
            match scan::acl_has_explicit_entries(&host_path) {
                Ok(true) => unsupported.push(WinUnsupported {
                    rel_path: change.rel_path.clone(),
                    code: "WIN_ACL_UNSUPPORTED".to_string(),
                    message: format!(
                        "{} has explicit (non-inherited) ACL entries; cowbox refuses to replace it",
                        change.rel_path
                    ),
                }),
                Ok(false) => {}
                Err(error) => unsupported.push(WinUnsupported {
                    rel_path: change.rel_path.clone(),
                    code: error.code.to_string(),
                    message: error.message,
                }),
            }
        }
    }
    let bytes_to_write = target
        .entries
        .iter()
        .filter(|(key, entry)| {
            entry.kind.is_file()
                && changes.iter().any(|change| {
                    change.rel_path == **key
                        && matches!(
                            change.kind,
                            WinChangeKind::Added
                                | WinChangeKind::Modified
                                | WinChangeKind::TypeChanged
                        )
                })
        })
        .map(|(_, entry)| entry.size)
        .sum();
    let summary = WinDiffSummary::from_changes(&changes);
    let host_drift = !drift.is_empty();
    let can_commit = (!host_drift || force) && unsupported.is_empty();
    Ok(WinCommitPreview {
        session_id: session.record.session_id.clone(),
        step,
        changes,
        summary,
        conflicts: drift,
        unsupported,
        host_drift,
        bytes_to_write,
        can_commit,
    })
}

pub fn commit(
    session: &mut WinSession,
    step: Option<u64>,
    force: bool,
    faults: &WinFaults,
) -> WinResult<WinCommitReport> {
    if let Some(journal) = unresolved_journal(session)? {
        return Err(WinError::new(
            "WIN_COMMIT_RECOVERY_REQUIRED",
            format!(
                "an interrupted commit journal needs recovery: {}",
                paths::display(&journal)
            ),
        )
        .hint(format!(
            "run `cowbox win commit --recover --session {}`",
            session.record.session_id
        ))
        .exit(2));
    }
    let preview = preview(session, step, force)?;
    if !preview.unsupported.is_empty() {
        let first = &preview.unsupported[0];
        return Err(WinError::new(
            Box::leak(first.code.clone().into_boxed_str()),
            format!("{}: {}", first.rel_path, first.message),
        )
        .hint("resolve the unsupported file or exclude it from the workspace")
        .exit(2));
    }
    if !preview.conflicts.is_empty() && !force {
        return Err(WinError::new(
            "WIN_HOST_DRIFT",
            format!(
                "host workspace changed since the session started: {}",
                preview.conflicts.join(", ")
            ),
        )
        .hint("inspect the paths and retry with --force only after explicit approval")
        .exit(2));
    }
    if preview.changes.is_empty() {
        return Ok(WinCommitReport {
            session_id: session.record.session_id.clone(),
            step: preview.step,
            committed: false,
            changes: 0,
            journal: None,
            finalized: true,
        });
    }

    let base = session.base_manifest()?;
    let target = session.manifest_at(preview.step)?;
    let ops = build_ops(&base, &target)?;
    let journal_id = format!("{}-{}", preview.step, super::store::now_ms());
    let journal_dir = session
        .dir
        .join("commits")
        .join(format!("journal-{journal_id}"));
    fs::create_dir_all(&journal_dir)?;
    cleanup_temp_files(session, &journal_id)?;

    let mut journal = WinJournal {
        schema_version: 1,
        journal_id: journal_id.clone(),
        session_id: session.record.session_id.clone(),
        step: preview.step,
        workspace: session.record.workspace.clone(),
        base_manifest_sha256: session.record.base_manifest_sha256.clone(),
        target_manifest_sha256: target.sha256(),
        force,
        phase: WinJournalPhase::Prepared,
        created_at_unix_ms: super::store::now_ms(),
        updated_at_unix_ms: super::store::now_ms(),
        ops,
        backups: BTreeMap::new(),
        applied_ops: 0,
        active_op: None,
        conflicts: Vec::new(),
        error: None,
    };
    let journal_path = journal_dir.join("journal.json");
    write_journal(&journal_path, &journal)?;

    let result = prepare_backups(session, &mut journal, &journal_dir);
    if let Err(error) = result {
        journal.error = Some(error.message.clone());
        journal.phase = WinJournalPhase::RestoreFailed;
        let _ = write_journal(&journal_path, &journal);
        return Err(error);
    }
    journal.phase = WinJournalPhase::BackedUp;
    journal.updated_at_unix_ms = super::store::now_ms();
    write_journal(&journal_path, &journal)?;
    if faults.abort_after_backup {
        return Err(WinError::fault_abort("after-backup", &journal_path));
    }

    journal.phase = WinJournalPhase::Applying;
    journal.updated_at_unix_ms = super::store::now_ms();
    write_journal(&journal_path, &journal)?;

    let apply_result = apply_ops(session, &mut journal, &journal_dir, &journal_path, faults);
    if let Err(error) = apply_result {
        if error.is_fault_abort() {
            return Err(error);
        }
        journal.error = Some(error.message.clone());
        let _ = preserve_host_conflicts(session, &mut journal, &journal_dir);
        let restored = restore_ops(session, &mut journal, &journal_dir, &journal_path, faults);
        if let Err(restore_error) = restored {
            journal.phase = WinJournalPhase::RestoreFailed;
            journal.updated_at_unix_ms = super::store::now_ms();
            let _ = write_journal(&journal_path, &journal);
            return Err(WinError::new(
                "WIN_RECOVERY_FAILED",
                format!(
                    "{}; restoring the host also failed: {}",
                    error.message, restore_error.message
                ),
            )
            .hint(format!(
                "keep {} and run `cowbox win commit --recover`",
                paths::display(&journal_path)
            ))
            .exit(3));
        }
        journal.phase = WinJournalPhase::Restored;
        journal.updated_at_unix_ms = super::store::now_ms();
        let _ = write_journal(&journal_path, &journal);
        return Err(error.hint(format!(
            "the host was restored; recovery journal: {}",
            paths::display(&journal_path)
        )));
    }

    let verify = verify_target(session, &base, &target, &journal_dir, &mut journal);
    if let Err(error) = verify {
        journal.error = Some(error.message.clone());
        let _ = preserve_host_conflicts(session, &mut journal, &journal_dir);
        let restored = restore_ops(session, &mut journal, &journal_dir, &journal_path, faults);
        journal.phase = if restored.is_ok() {
            WinJournalPhase::DriftAborted
        } else {
            WinJournalPhase::RestoreFailed
        };
        journal.updated_at_unix_ms = super::store::now_ms();
        let _ = write_journal(&journal_path, &journal);
        return Err(error.hint(format!(
            "the host was restored; user content is preserved under {}",
            paths::display(&journal_dir.join("conflicts"))
        )));
    }
    if faults.abort_before_applied {
        return Err(WinError::fault_abort("before-applied", &journal_path));
    }
    journal.phase = WinJournalPhase::Applied;
    journal.updated_at_unix_ms = super::store::now_ms();
    write_journal(&journal_path, &journal)?;
    if faults.abort_after_applied {
        return Err(WinError::fault_abort("after-applied", &journal_path));
    }
    finalize(session, &mut journal, &journal_dir, &journal_path)?;
    Ok(WinCommitReport {
        session_id: session.record.session_id.clone(),
        step: preview.step,
        committed: true,
        changes: preview.changes.len(),
        journal: Some(paths::display(&journal_path)),
        finalized: true,
    })
}

pub fn recover(session: &mut WinSession, faults: &WinFaults) -> WinResult<WinRecoveryReport> {
    let (journal_dir, mut journal) = match find_journal(session)? {
        Some(value) => value,
        None => {
            return Ok(WinRecoveryReport {
                session_id: session.record.session_id.clone(),
                journal: None,
                action: WinRecoveryAction::NoJournal,
                restored: 0,
                finalized: false,
            });
        }
    };
    let journal_path = journal_dir.join("journal.json");
    if journal.session_id != session.record.session_id {
        return Err(WinError::new(
            "WIN_SESSION_NOT_FOUND",
            format!(
                "journal {} belongs to session {}, not {}",
                paths::display(&journal_path),
                journal.session_id,
                session.record.session_id
            ),
        )
        .exit(2));
    }
    match journal.phase {
        WinJournalPhase::Applied => {
            let target = session.manifest_at(journal.step)?;
            let host = scan::scan_tree(&session.record.workspace, &session.excludes())?;
            if !scan::manifests_equal(&target, &host) {
                return Err(WinError::new(
                    "WIN_COMMIT_VERIFY_FAILED",
                    "the host no longer matches the applied commit target",
                )
                .hint(format!(
                    "inspect the host and the journal at {}",
                    paths::display(&journal_path)
                ))
                .exit(3));
            }
            finalize(session, &mut journal, &journal_dir, &journal_path)?;
            Ok(WinRecoveryReport {
                session_id: session.record.session_id.clone(),
                journal: Some(paths::display(&journal_path)),
                action: WinRecoveryAction::FinalizeCommit,
                restored: 0,
                finalized: true,
            })
        }
        WinJournalPhase::Prepared
        | WinJournalPhase::BackedUp
        | WinJournalPhase::Applying
        | WinJournalPhase::Restoring
        | WinJournalPhase::RestoreFailed => {
            cleanup_temp_files(session, &journal.journal_id)?;
            let _ = preserve_host_conflicts(session, &mut journal, &journal_dir);
            journal.phase = WinJournalPhase::Restoring;
            journal.updated_at_unix_ms = super::store::now_ms();
            write_journal(&journal_path, &journal)?;
            let restored = restore_ops(session, &mut journal, &journal_dir, &journal_path, faults);
            let count = match restored {
                Ok(count) => count,
                Err(error) => {
                    if error.is_fault_abort() {
                        return Err(error);
                    }
                    journal.phase = WinJournalPhase::RestoreFailed;
                    journal.error = Some(error.message.clone());
                    journal.updated_at_unix_ms = super::store::now_ms();
                    let _ = write_journal(&journal_path, &journal);
                    return Err(WinError::new(
                        "WIN_RECOVERY_FAILED",
                        format!("cannot restore the host: {}", error.message),
                    )
                    .hint(format!(
                        "keep {} and retry `cowbox win commit --recover`",
                        paths::display(&journal_path)
                    ))
                    .exit(3));
                }
            };
            if !journal.force {
                let base = session.base_manifest()?;
                let host = scan::scan_tree(&session.record.workspace, &session.excludes())?;
                let mismatched = scan::changed_keys(&base, &host)
                    .into_iter()
                    .filter(|key| !journal.conflicts.iter().any(|path| path == key))
                    .collect::<Vec<_>>();
                if !mismatched.is_empty() {
                    journal.phase = WinJournalPhase::RestoreFailed;
                    journal.error = Some(format!(
                        "host does not match the base manifest at {}",
                        mismatched.join(", ")
                    ));
                    write_journal(&journal_path, &journal)?;
                    return Err(WinError::new(
                        "WIN_RECOVERY_VERIFY_FAILED",
                        format!(
                            "the host does not match its pre-commit manifest after recovery: {}",
                            mismatched.join(", ")
                        ),
                    )
                    .hint(format!(
                        "inspect the journal and backups at {}",
                        paths::display(&journal_dir)
                    ))
                    .exit(3));
                }
            }
            journal.phase = WinJournalPhase::Restored;
            journal.updated_at_unix_ms = super::store::now_ms();
            write_journal(&journal_path, &journal)?;
            prune_backups(&journal_dir)?;
            Ok(WinRecoveryReport {
                session_id: session.record.session_id.clone(),
                journal: Some(paths::display(&journal_path)),
                action: WinRecoveryAction::RestoreHost,
                restored: count,
                finalized: false,
            })
        }
        WinJournalPhase::Finalized => Ok(WinRecoveryReport {
            session_id: session.record.session_id.clone(),
            journal: Some(paths::display(&journal_path)),
            action: WinRecoveryAction::AlreadyFinalized,
            restored: 0,
            finalized: true,
        }),
        WinJournalPhase::Restored | WinJournalPhase::DriftAborted => Ok(WinRecoveryReport {
            session_id: session.record.session_id.clone(),
            journal: Some(paths::display(&journal_path)),
            action: WinRecoveryAction::AlreadyRestored,
            restored: 0,
            finalized: false,
        }),
    }
}

fn build_ops(base: &WinManifest, target: &WinManifest) -> WinResult<Vec<WinJournalOp>> {
    let mut removed = Vec::new();
    for (key, entry) in &base.entries {
        let keep = target
            .entries
            .get(key)
            .is_some_and(|after| after.kind == entry.kind);
        if !keep {
            removed.push((
                paths::depth(key),
                key.clone(),
                if entry.kind.is_file() {
                    WinOpKind::DeleteFile
                } else {
                    WinOpKind::DeleteDir
                },
                entry.readonly,
            ));
        }
    }
    removed.sort_by(|left, right| right.0.cmp(&left.0).then_with(|| right.1.cmp(&left.1)));

    let mut created_dirs = Vec::new();
    let mut written_files = Vec::new();
    for (key, entry) in &target.entries {
        if entry.kind == WinEntryKind::Directory {
            let existing = base
                .entries
                .get(key)
                .is_some_and(|before| before.kind == WinEntryKind::Directory);
            if !existing {
                created_dirs.push((paths::depth(key), key.clone()));
            }
        } else {
            let unchanged = base
                .entries
                .get(key)
                .is_some_and(|before| before.kind.is_file() && before.content_equals(entry));
            if !unchanged {
                written_files.push((paths::depth(key), key.clone(), entry.clone()));
            }
        }
    }
    created_dirs.sort_by(|left, right| left.0.cmp(&right.0).then_with(|| left.1.cmp(&right.1)));
    written_files.sort_by(|left, right| left.0.cmp(&right.0).then_with(|| left.1.cmp(&right.1)));

    let mut ops = Vec::new();
    for (_, key, kind, readonly) in removed {
        ops.push(WinJournalOp {
            kind,
            rel_path: key,
            object: None,
            backup: None,
            readonly,
            mtime_unix_ms: None,
            existed: false,
        });
    }
    for (_, key) in created_dirs {
        ops.push(WinJournalOp {
            kind: WinOpKind::CreateDir,
            rel_path: key,
            object: None,
            backup: None,
            readonly: false,
            mtime_unix_ms: None,
            existed: false,
        });
    }
    for (_, key, entry) in written_files {
        ops.push(WinJournalOp {
            kind: WinOpKind::WriteFile,
            rel_path: key,
            object: entry.sha256,
            backup: None,
            readonly: entry.readonly,
            mtime_unix_ms: entry.mtime_unix_ms,
            existed: false,
        });
    }
    Ok(ops)
}

fn prepare_backups(
    session: &WinSession,
    journal: &mut WinJournal,
    journal_dir: &Path,
) -> WinResult<()> {
    let base = session.base_manifest()?;
    for index in 0..journal.ops.len() {
        let mut op = journal.ops[index].clone();
        let host_path = paths::join_key(&session.record.workspace, &op.rel_path)?;
        let base_entry = base.entries.get(&op.rel_path);
        match op.kind {
            WinOpKind::CreateDir => {
                op.existed = false;
            }
            WinOpKind::DeleteDir => {
                if host_path.exists() {
                    paths::ensure_no_reparse(&host_path)?;
                    paths::ensure_within(&session.record.workspace, &host_path)?;
                    op.existed = host_path.is_dir();
                } else {
                    op.existed = false;
                }
            }
            WinOpKind::DeleteFile | WinOpKind::WriteFile => {
                let replaces_directory = op.kind == WinOpKind::WriteFile
                    && base_entry.is_some_and(|entry| entry.kind == WinEntryKind::Directory);
                if replaces_directory || !host_path.exists() {
                    op.existed = false;
                } else {
                    paths::ensure_no_reparse(&host_path)?;
                    paths::ensure_within(&session.record.workspace, &host_path)?;
                    let case_only_rename = op.kind == WinOpKind::WriteFile
                        && base_entry.is_none()
                        && case_alias(&base, &op.rel_path).is_some();
                    if !host_path.is_file() || case_only_rename {
                        op.existed = false;
                    } else {
                        let backup = backup_existing_file(
                            journal_dir,
                            &mut op,
                            &host_path,
                            base_entry,
                            journal.force,
                        )?;
                        journal.backups.insert(op.rel_path.clone(), backup);
                    }
                }
            }
        }
        journal.ops[index] = op;
    }
    Ok(())
}

fn backup_existing_file(
    journal_dir: &Path,
    op: &mut WinJournalOp,
    host_path: &Path,
    base_entry: Option<&WinEntry>,
    force: bool,
) -> WinResult<WinBackup> {
    let backup_path = journal_dir
        .join("backup")
        .join(path_from_key(&op.rel_path)?);
    if let Some(parent) = backup_path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::copy(host_path, &backup_path).map_err(|error| {
        WinError::io(
            "WIN_FILE_BUSY",
            format!("cannot back up {}", paths::display(host_path)),
            error,
        )
    })?;
    let (size, sha256) = scan::hash_file(&backup_path)?;
    let expected = base_entry.and_then(|entry| entry.sha256.clone());
    if !force && expected.as_deref() != Some(sha256.as_str()) {
        return Err(WinError::new(
            "WIN_HOST_DRIFT",
            format!("{} changed after the preview was prepared", op.rel_path),
        )
        .exit(2));
    }
    let metadata = fs::metadata(host_path)?;
    let readonly = metadata.permissions().readonly();
    let mtime_unix_ms = metadata.modified().ok().and_then(|time| {
        time.duration_since(std::time::UNIX_EPOCH)
            .ok()
            .map(|duration| duration.as_millis() as i64)
    });
    op.existed = true;
    op.backup = Some(op.rel_path.clone());
    Ok(WinBackup {
        rel_path: paths::display(&backup_path),
        sha256,
        size,
        readonly,
        mtime_unix_ms,
    })
}

fn case_alias(manifest: &WinManifest, key: &str) -> Option<String> {
    let folded = key.to_lowercase();
    manifest
        .entries
        .keys()
        .find(|candidate| candidate.to_lowercase() == folded && candidate.as_str() != key)
        .cloned()
}

fn apply_ops(
    session: &WinSession,
    journal: &mut WinJournal,
    _journal_dir: &Path,
    journal_path: &Path,
    faults: &WinFaults,
) -> WinResult<()> {
    let objects = session.store.objects(&session.record.session_id)?;
    for index in 0..journal.ops.len() {
        let op = journal.ops[index].clone();
        if let Some(edit) = &faults.mid_commit_edit {
            if index == edit.after_apply {
                write_mid_commit_edit(session, edit)?;
            }
        }
        if !journal.force {
            verify_expected_host(session, journal, &op)?;
        }
        journal.active_op = Some(index);
        journal.updated_at_unix_ms = super::store::now_ms();
        write_journal(journal_path, journal)?;
        let host_path = paths::join_key(&session.record.workspace, &op.rel_path)?;
        match op.kind {
            WinOpKind::CreateDir => {
                fs::create_dir_all(&host_path).map_err(|error| {
                    WinError::io(
                        "WIN_IO",
                        format!("cannot create {}", paths::display(&host_path)),
                        error,
                    )
                })?;
            }
            WinOpKind::DeleteFile => {
                if host_path.exists() {
                    clear_readonly(&host_path)?;
                    fs::remove_file(&host_path)
                        .map_err(|error| busy_or_io("cannot remove", &host_path, error))?;
                }
            }
            WinOpKind::DeleteDir => {
                if host_path.exists() {
                    fs::remove_dir(&host_path).map_err(|error| {
                        if error.raw_os_error() == Some(145) {
                            WinError::new(
                                "WIN_DIR_NOT_EMPTY",
                                format!(
                                    "{} contains untracked files and cannot be removed",
                                    op.rel_path
                                ),
                            )
                            .hint("move the untracked content out of the deleted directory")
                        } else {
                            busy_or_io("cannot remove", &host_path, error)
                        }
                    })?;
                }
            }
            WinOpKind::WriteFile => {
                write_file_atomic(&objects, &op, index, &host_path, journal)?;
            }
        }
        if faults.abort_after_operation == Some(index + 1) {
            return Err(WinError::fault_abort(
                &format!("after-operation:{index}"),
                journal_path,
            ));
        }
        journal.applied_ops = index + 1;
        journal.active_op = None;
        journal.updated_at_unix_ms = super::store::now_ms();
        write_journal(journal_path, journal)?;
        if faults.abort_after_apply == Some(index + 1) {
            return Err(WinError::fault_abort(
                &format!("after-apply:{index}"),
                journal_path,
            ));
        }
    }
    Ok(())
}

fn write_file_atomic(
    objects: &super::store::ObjectStore,
    op: &WinJournalOp,
    index: usize,
    destination: &Path,
    journal: &WinJournal,
) -> WinResult<()> {
    let object = objects.object_path(op.object.as_deref().unwrap_or_default())?;
    if let Some(parent) = destination.parent() {
        fs::create_dir_all(parent)?;
    }
    let temp = destination.with_file_name(format!(".cowbox-tmp-{}-{index}", journal.journal_id));
    if temp.exists() {
        let _ = fs::remove_file(&temp);
    }
    let result = (|| -> WinResult<()> {
        if destination.exists() {
            if destination.is_file() {
                if scan::acl_has_explicit_entries(destination)? {
                    return Err(WinError::new(
                        "WIN_ACL_UNSUPPORTED",
                        format!(
                            "{} has explicit ACL entries; refusing to replace it",
                            paths::display(destination)
                        ),
                    )
                    .hint("reset the file ACL so it inherits from its parent, then retry")
                    .exit(2));
                }
                fs::copy(destination, &temp).map_err(|error| {
                    WinError::io(
                        "WIN_FILE_BUSY",
                        format!("cannot stage {}", paths::display(destination)),
                        error,
                    )
                })?;
            } else {
                return Err(WinError::new(
                    "WIN_HOST_DRIFT",
                    format!(
                        "{} is not a regular file at commit time",
                        paths::display(destination)
                    ),
                )
                .exit(2));
            }
        } else {
            File::create(&temp)?;
        }
        clear_readonly(&temp)?;
        let mut source = File::open(&object)?;
        let mut target = OpenOptions::new().write(true).truncate(true).open(&temp)?;
        std::io::copy(&mut source, &mut target).map_err(|error| {
            WinError::io(
                "WIN_IO",
                format!("cannot write staged file for {}", op.rel_path),
                error,
            )
        })?;
        target.sync_all()?;
        drop(target);
        let (size, sha256) = scan::hash_file(&temp)?;
        if Some(sha256.as_str()) != op.object.as_deref() {
            return Err(WinError::new(
                "WIN_COMMIT_VERIFY_FAILED",
                format!(
                    "staged bytes for {} do not match the checkpoint",
                    op.rel_path
                ),
            ));
        }
        let _ = size;
        if let Some(mtime) = op.mtime_unix_ms {
            super::store::set_mtime(&temp, mtime)?;
        }
        let mut permissions = fs::metadata(&temp)?.permissions();
        permissions.set_readonly(op.readonly);
        fs::set_permissions(&temp, permissions)?;
        let destination_was_file = destination.is_file();
        if destination_was_file {
            clear_readonly(destination)?;
        }
        if let Err(error) = fs::rename(&temp, destination) {
            if destination_was_file && destination.is_file() {
                if let Some(backup) = journal.backups.get(&op.rel_path) {
                    if let Ok(mut permissions) =
                        fs::metadata(destination).map(|metadata| metadata.permissions())
                    {
                        permissions.set_readonly(backup.readonly);
                        let _ = fs::set_permissions(destination, permissions);
                    }
                }
            }
            return Err(busy_or_io("cannot replace", destination, error)
                .hint(format!("the staged file is {}", paths::display(&temp))));
        }
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temp);
    }
    result
}

fn verify_expected_host(
    session: &WinSession,
    journal: &WinJournal,
    op: &WinJournalOp,
) -> WinResult<()> {
    if op.existed {
        let path = paths::join_key(&session.record.workspace, &op.rel_path)?;
        if !path.exists() {
            return Err(WinError::new(
                "WIN_HOST_DRIFT",
                format!("{} disappeared during commit", op.rel_path),
            )
            .exit(2));
        }
        paths::ensure_no_reparse(&path)?;
        if path.is_file() {
            if let Some(backup) = journal.backups.get(&op.rel_path) {
                let (_, sha256) = scan::hash_file(&path)?;
                if backup.sha256 != sha256 {
                    return Err(WinError::new(
                        "WIN_HOST_DRIFT",
                        format!("{} changed while the commit was applying", op.rel_path),
                    )
                    .exit(2));
                }
            }
        }
    } else {
        let path = paths::join_key(&session.record.workspace, &op.rel_path)?;
        if op.kind == WinOpKind::CreateDir {
            if path.exists() && !path.is_dir() {
                return Err(WinError::new(
                    "WIN_HOST_DRIFT",
                    format!("{} appeared during commit", op.rel_path),
                )
                .exit(2));
            }
        } else if path.exists() {
            return Err(WinError::new(
                "WIN_HOST_DRIFT",
                format!("{} appeared during commit", op.rel_path),
            )
            .exit(2));
        }
    }
    Ok(())
}

fn verify_target(
    session: &WinSession,
    base: &WinManifest,
    target: &WinManifest,
    journal_dir: &Path,
    journal: &mut WinJournal,
) -> WinResult<()> {
    let host = scan::scan_tree(&session.record.workspace, &session.excludes())?;
    let keys = if journal.force {
        scan::changed_keys(base, target)
    } else {
        base.entries
            .keys()
            .chain(target.entries.keys())
            .cloned()
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect()
    };
    let mut conflicts = Vec::new();
    for key in keys {
        let expected = target.entries.get(&key);
        let actual = host.entries.get(&key);
        let matches = match (expected, actual) {
            (None, None) => true,
            (Some(expected), Some(actual)) => expected.content_equals(actual),
            _ => false,
        };
        if !matches {
            conflicts.push(key.clone());
            if let Some(actual) = actual {
                if actual.kind.is_file() {
                    let source = paths::join_key(&session.record.workspace, &key)?;
                    let destination = journal_dir.join("conflicts").join(path_from_key(&key)?);
                    if let Some(parent) = destination.parent() {
                        fs::create_dir_all(parent)?;
                    }
                    fs::copy(&source, &destination).map_err(|error| {
                        WinError::io(
                            "WIN_RECOVERY_FAILED",
                            format!(
                                "cannot preserve user content at {}",
                                paths::display(&source)
                            ),
                            error,
                        )
                        .exit(3)
                    })?;
                }
            }
        }
    }
    if conflicts.is_empty() {
        return Ok(());
    }
    journal.conflicts = conflicts.clone();
    Err(WinError::new(
        "WIN_HOST_DRIFT",
        format!(
            "the host changed while cowbox was applying the commit: {}",
            conflicts.join(", ")
        ),
    )
    .exit(2))
}

fn restore_ops(
    session: &WinSession,
    journal: &mut WinJournal,
    journal_dir: &Path,
    journal_path: &Path,
    faults: &WinFaults,
) -> WinResult<usize> {
    let mut restored = 0usize;
    for (index, op) in journal
        .ops
        .iter()
        .take(restore_limit(journal))
        .enumerate()
        .rev()
    {
        if faults.abort_during_recover == Some(restored) {
            journal.phase = WinJournalPhase::Restoring;
            journal.updated_at_unix_ms = super::store::now_ms();
            write_journal(journal_path, journal)?;
            return Err(WinError::fault_abort(
                &format!("during-recover:{restored}"),
                journal_path,
            ));
        }
        let destination = paths::join_key(&session.record.workspace, &op.rel_path)?;
        if journal.conflicts.iter().any(|path| path == &op.rel_path) && destination.exists() {
            restored += 1;
            continue;
        }
        if !op_was_applied(journal, op, &destination)? {
            restored += 1;
            continue;
        }
        match op.kind {
            WinOpKind::WriteFile => {
                if let Some(backup) = journal.backups.get(&op.rel_path) {
                    restore_backup(backup, &destination, journal, index)?;
                } else if destination.exists() {
                    clear_readonly(&destination)?;
                    fs::remove_file(&destination).map_err(|error| {
                        busy_or_io("cannot remove new file", &destination, error)
                    })?;
                }
            }
            WinOpKind::DeleteFile => {
                if let Some(backup) = journal.backups.get(&op.rel_path) {
                    restore_backup(backup, &destination, journal, index)?;
                }
            }
            WinOpKind::CreateDir => {
                if destination.exists() {
                    match fs::remove_dir(&destination) {
                        Ok(()) => {}
                        Err(error) if error.raw_os_error() == Some(145) => {}
                        Err(error) => {
                            return Err(busy_or_io(
                                "cannot remove created directory",
                                &destination,
                                error,
                            ));
                        }
                    }
                }
            }
            WinOpKind::DeleteDir => {
                fs::create_dir_all(&destination)?;
            }
        }
        restored += 1;
        journal.updated_at_unix_ms = super::store::now_ms();
        write_journal(journal_path, journal)?;
    }
    journal.active_op = None;
    journal.updated_at_unix_ms = super::store::now_ms();
    write_journal(journal_path, journal)?;
    cleanup_temp_files(session, &journal.journal_id)?;
    let _ = journal_dir;
    Ok(restored)
}

fn restore_limit(journal: &WinJournal) -> usize {
    journal
        .applied_ops
        .max(journal.active_op.map(|index| index + 1).unwrap_or(0))
}

fn op_was_applied(journal: &WinJournal, op: &WinJournalOp, destination: &Path) -> WinResult<bool> {
    match op.kind {
        WinOpKind::WriteFile => {
            if !destination.is_file() {
                return Ok(false);
            }
            let (_, sha256) = scan::hash_file(destination)?;
            if op.object.as_deref() == Some(sha256.as_str()) {
                return Ok(true);
            }
            if let Some(backup) = journal.backups.get(&op.rel_path) {
                if backup.sha256 == sha256 {
                    return Ok(false);
                }
            }
            Ok(false)
        }
        WinOpKind::DeleteFile | WinOpKind::DeleteDir => Ok(!destination.exists()),
        WinOpKind::CreateDir => Ok(destination.is_dir()),
    }
}

fn preserve_host_conflicts(
    session: &WinSession,
    journal: &mut WinJournal,
    journal_dir: &Path,
) -> WinResult<()> {
    for op in &journal.ops {
        let destination = paths::join_key(&session.record.workspace, &op.rel_path)?;
        if !destination.is_file() || destination.is_symlink() {
            continue;
        }
        let (_, current_sha) = scan::hash_file(&destination)?;
        let backup_sha = journal
            .backups
            .get(&op.rel_path)
            .map(|backup| backup.sha256.as_str());
        if backup_sha == Some(current_sha.as_str())
            || op.object.as_deref() == Some(current_sha.as_str())
        {
            continue;
        }
        let copy = journal_dir
            .join("conflicts")
            .join(path_from_key(&op.rel_path)?);
        if let Some(parent) = copy.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::copy(&destination, &copy).map_err(|error| {
            WinError::io(
                "WIN_RECOVERY_FAILED",
                format!(
                    "cannot preserve user content at {}",
                    paths::display(&destination)
                ),
                error,
            )
            .exit(3)
        })?;
        if !journal.conflicts.iter().any(|path| path == &op.rel_path) {
            journal.conflicts.push(op.rel_path.clone());
        }
    }
    Ok(())
}

fn restore_backup(
    backup: &WinBackup,
    destination: &Path,
    journal: &WinJournal,
    index: usize,
) -> WinResult<()> {
    let backup_path = PathBuf::from(&backup.rel_path);
    if let Some(parent) = destination.parent() {
        fs::create_dir_all(parent)?;
    }
    let temp =
        destination.with_file_name(format!(".cowbox-restore-{}-{index}", journal.journal_id));
    let result = (|| -> WinResult<()> {
        fs::copy(&backup_path, &temp).map_err(|error| {
            WinError::io(
                "WIN_RECOVERY_FAILED",
                format!("cannot stage backup for {}", paths::display(destination)),
                error,
            )
        })?;
        clear_readonly(&temp)?;
        if destination.exists() {
            clear_readonly(destination)?;
        }
        fs::rename(&temp, destination)
            .map_err(|error| busy_or_io("cannot restore", destination, error))?;
        if let Some(mtime) = backup.mtime_unix_ms {
            super::store::set_mtime(destination, mtime)?;
        }
        let mut permissions = fs::metadata(destination)?.permissions();
        permissions.set_readonly(backup.readonly);
        fs::set_permissions(destination, permissions)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temp);
    }
    result
}

fn finalize(
    session: &mut WinSession,
    journal: &mut WinJournal,
    journal_dir: &Path,
    journal_path: &Path,
) -> WinResult<()> {
    session.record.status = WinSessionStatus::Committed;
    session.record.last_commit_unix_ms = Some(super::store::now_ms());
    session.record.last_journal = Some(
        journal_path
            .strip_prefix(&session.dir)
            .unwrap_or(journal_path)
            .to_string_lossy()
            .replace('/', "\\"),
    );
    session.record.updated_at_unix_ms = super::store::now_ms();
    session.save()?;
    journal.phase = WinJournalPhase::Finalized;
    journal.updated_at_unix_ms = super::store::now_ms();
    write_journal(journal_path, journal)?;
    prune_backups(journal_dir)?;
    Ok(())
}

fn find_journal(session: &WinSession) -> WinResult<Option<(PathBuf, WinJournal)>> {
    let root = session.dir.join("commits");
    if !root.exists() {
        return Ok(None);
    }
    let mut journals = Vec::new();
    for entry in fs::read_dir(&root)? {
        let entry = entry?;
        let path = entry.path().join("journal.json");
        if !path.exists() {
            continue;
        }
        let journal: WinJournal = match super::store::read_json(&path) {
            Ok(journal) => journal,
            Err(_) => continue,
        };
        journals.push((entry.path(), journal));
    }
    journals.sort_by_key(|(_, journal)| journal.created_at_unix_ms);
    let pending = journals.iter().rev().find(|(_, journal)| {
        !matches!(
            journal.phase,
            WinJournalPhase::Finalized | WinJournalPhase::Restored
        )
    });
    if let Some((dir, journal)) = pending {
        return Ok(Some((dir.clone(), journal.clone())));
    }
    Ok(journals.into_iter().next_back())
}

fn write_journal(path: &Path, journal: &WinJournal) -> WinResult<()> {
    super::store::write_json_atomic(path, journal)
}

fn cleanup_temp_files(session: &WinSession, journal_id: &str) -> WinResult<()> {
    let prefix = format!(".cowbox-tmp-{journal_id}-");
    let restore_prefix = format!(".cowbox-restore-{journal_id}-");
    cleanup_temp_dir(&session.record.workspace, &prefix, &restore_prefix)?;
    Ok(())
}

fn cleanup_temp_dir(directory: &Path, prefix: &str, restore_prefix: &str) -> WinResult<()> {
    let Ok(entries) = fs::read_dir(directory) else {
        return Ok(());
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(metadata) = fs::symlink_metadata(&path) else {
            continue;
        };
        #[cfg(windows)]
        if paths::is_reparse(&metadata) {
            continue;
        }
        if metadata.is_dir() {
            cleanup_temp_dir(&path, prefix, restore_prefix)?;
        } else {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if name.starts_with(prefix) || name.starts_with(restore_prefix) {
                let _ = fs::remove_file(&path);
            }
        }
    }
    Ok(())
}

fn prune_backups(journal_dir: &Path) -> WinResult<()> {
    let backup = journal_dir.join("backup");
    if backup.exists() {
        fs::remove_dir_all(&backup)?;
    }
    Ok(())
}

fn path_from_key(key: &str) -> WinResult<PathBuf> {
    paths::validate_key(key)?;
    let mut path = PathBuf::new();
    for component in key.split('/') {
        path.push(component);
    }
    Ok(path)
}

fn busy_or_io(action: &str, path: &Path, error: std::io::Error) -> WinError {
    let busy = matches!(error.raw_os_error(), Some(5 | 32 | 33));
    let code = if busy { "WIN_FILE_BUSY" } else { "WIN_IO" };
    let hint = if busy {
        Some("close the file in editors and other applications, then retry")
    } else {
        None
    };
    let mut result = WinError::io(code, format!("{action} {}", paths::display(path)), error);
    if let Some(hint) = hint {
        result = result.hint(hint);
    }
    result
}

fn write_mid_commit_edit(session: &WinSession, edit: &WinMidCommitEdit) -> WinResult<()> {
    let path = paths::join_key(&session.record.workspace, &edit.rel_path)?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    if path.exists() {
        clear_readonly(&path)?;
    }
    let mut file = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(&path)?;
    file.write_all(&edit.bytes)?;
    file.sync_all()?;
    Ok(())
}

pub fn pending_recovery(session: &WinSession) -> WinResult<Option<String>> {
    Ok(find_journal(session)?.map(|(dir, _)| paths::display(&dir.join("journal.json"))))
}

fn unresolved_journal(session: &WinSession) -> WinResult<Option<PathBuf>> {
    let root = session.dir.join("commits");
    if !root.exists() {
        return Ok(None);
    }
    for entry in fs::read_dir(&root)? {
        let entry = entry?;
        let path = entry.path().join("journal.json");
        if !path.exists() {
            continue;
        }
        let Ok(journal) = super::store::read_json::<WinJournal>(&path) else {
            continue;
        };
        if !matches!(
            journal.phase,
            WinJournalPhase::Finalized | WinJournalPhase::Restored | WinJournalPhase::DriftAborted
        ) {
            return Ok(Some(path));
        }
    }
    Ok(None)
}

#[allow(dead_code)]
fn read_all(path: &Path) -> WinResult<Vec<u8>> {
    let mut file = File::open(path)?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)?;
    Ok(bytes)
}
