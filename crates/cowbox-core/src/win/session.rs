use super::error::{WinError, WinResult};
use super::model::*;
use super::paths;
use super::scan;
use super::store::{self, ObjectStore, WinStore};
use cowbox_dag::{BASE_STEP, StepNode};
use std::fs::{self, File};
use std::path::{Path, PathBuf};

const RESCUE_PREFIX: &str = "rescue before rollback";

pub struct WinSession {
    pub store: WinStore,
    pub record: WinSessionRecord,
    pub dir: PathBuf,
    pub live_workspace: PathBuf,
    lock: Option<File>,
}

impl WinSession {
    pub fn open(session_id: &str, state_root: Option<&Path>) -> WinResult<Self> {
        let explicit = state_root.map(WinStore::new).transpose()?;
        let indexed = if explicit.is_none() {
            store::lookup_index(session_id)?
        } else {
            None
        };
        let state_root = explicit
            .map(|store| store.state_root)
            .or(indexed)
            .ok_or_else(|| {
                WinError::new(
                    "WIN_SESSION_NOT_FOUND",
                    format!(
                        "cannot find session {session_id}; pass --state-root or run `cowbox win init` first"
                    ),
                )
            })?;
        let store = WinStore::new(&state_root)?;
        let dir = store.session_dir(session_id)?;
        let session_json = dir.join("session.json");
        if !session_json.exists() {
            return Err(WinError::new(
                "WIN_SESSION_NOT_FOUND",
                format!(
                    "session {session_id} does not exist at {}",
                    paths::display(&session_json)
                ),
            ));
        }
        let lock = store.lock_session(session_id)?;
        let record: WinSessionRecord = store::read_json(&session_json)?;
        validate_record(&record, session_id, &store.state_root)?;
        if record.status == WinSessionStatus::Discarded {
            return Err(WinError::new(
                "WIN_SESSION_DISCARDED",
                format!("session {session_id} was discarded"),
            ));
        }
        let live_workspace = paths::join_key(&dir, &record.live_workspace)?;
        if !live_workspace.is_dir() {
            return Err(WinError::new(
                "WIN_STATE_CORRUPTED",
                format!(
                    "managed workspace {} is missing",
                    paths::display(&live_workspace)
                ),
            ));
        }
        Ok(Self {
            store,
            record,
            dir,
            live_workspace,
            lock: Some(lock),
        })
    }

    pub fn summary(&self) -> WinSessionSummary {
        WinSessionSummary {
            id: self.record.session_id.clone(),
            workspace: paths::display(&self.record.workspace),
            managed_workspace: paths::display(&self.managed_workspace()),
            state_root: paths::display(&self.store.state_root),
            status: self.record.status.as_str().to_string(),
            head: self.record.dag.head_step,
            storage_mode: self.record.storage_mode.clone(),
        }
    }

    pub fn managed_workspace(&self) -> PathBuf {
        let editor = self.dir.join(&self.record.editor_workspace);
        if editor.exists() {
            editor
        } else {
            self.live_workspace.clone()
        }
    }

    pub fn excludes(&self) -> Vec<String> {
        crate::policy::DEFAULT_EXCLUDES
            .iter()
            .map(|item| (*item).to_string())
            .collect()
    }

    pub fn base_manifest(&self) -> WinResult<WinManifest> {
        let path = self.store.base_manifest(&self.record.session_id)?;
        let manifest: WinManifest = store::read_json(&path)?;
        if manifest.sha256() != self.record.base_manifest_sha256 {
            return Err(WinError::new(
                "WIN_STATE_CORRUPTED",
                format!(
                    "base manifest {} does not match the session digest",
                    paths::display(&path)
                ),
            ));
        }
        Ok(manifest)
    }

    pub fn step_delta(&self, step: u64) -> WinResult<WinStepDelta> {
        let path = self.store.step_delta(&self.record.session_id, step)?;
        let delta: WinStepDelta = store::read_json(&path)?;
        if delta.schema_version != 1 || delta.step != step {
            return Err(WinError::new(
                "WIN_STATE_CORRUPTED",
                format!("invalid step delta {}", paths::display(&path)),
            ));
        }
        Ok(delta)
    }

    pub fn manifest_at(&self, step: u64) -> WinResult<WinManifest> {
        if !self.record.dag.nodes.contains_key(&step) {
            return Err(WinError::new(
                "WIN_DAG",
                format!("step {step} does not exist in this session"),
            ));
        }
        let mut chain = self.record.dag.path_to_base(step)?;
        chain.reverse();
        let base = self.base_manifest()?;
        let mut deltas = Vec::new();
        for id in chain.into_iter().filter(|id| *id != BASE_STEP) {
            let delta = self.step_delta(id)?;
            let expected_parent = self
                .record
                .dag
                .nodes
                .get(&id)
                .and_then(|node| node.parent_id)
                .ok_or_else(|| {
                    WinError::new("WIN_STATE_CORRUPTED", format!("step {id} has no parent"))
                })?;
            if delta.parent != expected_parent {
                return Err(WinError::new(
                    "WIN_STATE_CORRUPTED",
                    format!(
                        "step {id} delta names parent {} but the DAG names {expected_parent}",
                        delta.parent
                    ),
                ));
            }
            deltas.push(delta);
        }
        scan::replay(&base, &deltas)
    }

    pub fn head_manifest(&self) -> WinResult<WinManifest> {
        self.manifest_at(self.record.dag.head_step)
    }

    pub fn scan_live(&self) -> WinResult<WinManifest> {
        scan::scan_tree(&self.live_workspace, &self.excludes())
    }

    pub fn checkpoint(&mut self, label: Option<String>) -> WinResult<WinCheckpointReport> {
        self.ensure_active()?;
        let parent = self.record.dag.head_step;
        let parent_manifest = self.manifest_at(parent)?;
        let current = self.scan_live()?;
        let delta_entries = scan::delta_entries(&parent_manifest, &current);
        if delta_entries.is_empty() {
            return Ok(WinCheckpointReport {
                session_id: self.record.session_id.clone(),
                step: parent,
                parent,
                head: parent,
                created: false,
                added: 0,
                modified: 0,
                deleted: 0,
                bytes_added: 0,
                managed_workspace: paths::display(&self.managed_workspace()),
            });
        }

        let step = self.record.dag.next_step_id();
        let label = label.unwrap_or_else(|| format!("checkpoint {step}"));
        let objects = self.store.objects(&self.record.session_id)?;
        let mut added = 0usize;
        let mut modified = 0usize;
        let mut deleted = 0usize;
        let mut bytes_added = 0u64;
        for (key, entry) in &delta_entries {
            match (parent_manifest.entries.get(key), entry.as_ref()) {
                (None, Some(entry)) if entry.kind.is_file() => {
                    added += 1;
                    bytes_added = bytes_added.saturating_add(entry.size);
                    objects.put_file(
                        &paths::join_key(&self.live_workspace, key)?,
                        entry.sha256.as_deref().unwrap_or_default(),
                    )?;
                }
                (Some(_), Some(entry)) if entry.kind.is_file() => {
                    modified += 1;
                    bytes_added = bytes_added.saturating_add(entry.size);
                    objects.put_file(
                        &paths::join_key(&self.live_workspace, key)?,
                        entry.sha256.as_deref().unwrap_or_default(),
                    )?;
                }
                (Some(before), None) if before.kind.is_file() => {
                    deleted += 1;
                }
                (Some(before), Some(entry))
                    if before.kind.is_file() && entry.kind == WinEntryKind::Directory =>
                {
                    deleted += 1;
                }
                (Some(before), Some(entry))
                    if before.kind == WinEntryKind::Directory && entry.kind.is_file() =>
                {
                    added += 1;
                    bytes_added = bytes_added.saturating_add(entry.size);
                    objects.put_file(
                        &paths::join_key(&self.live_workspace, key)?,
                        entry.sha256.as_deref().unwrap_or_default(),
                    )?;
                }
                _ => {}
            }
        }

        let timestamp = store::now_ms();
        let delta = WinStepDelta {
            schema_version: 1,
            step,
            parent,
            label: label.clone(),
            timestamp_unix_ms: timestamp,
            entries: delta_entries,
        };
        let delta_path = self.store.step_delta(&self.record.session_id, step)?;
        store::write_json_atomic(&delta_path, &delta)?;
        let node = StepNode {
            id: step,
            parent_id: Some(parent),
            label,
            timestamp_unix_ms: timestamp as u64,
            exit_code: None,
            signal: None,
            duration_ms: 0,
            orphans_reaped: 0,
            timed_out: false,
        };
        self.record.dag.add_step(node)?;
        self.record.updated_at_unix_ms = timestamp;
        self.save()?;

        Ok(WinCheckpointReport {
            session_id: self.record.session_id.clone(),
            step,
            parent,
            head: self.record.dag.head_step,
            created: true,
            added,
            modified,
            deleted,
            bytes_added,
            managed_workspace: paths::display(&self.managed_workspace()),
        })
    }

    pub fn log(&self) -> WinLogReport {
        let mut steps = Vec::new();
        for (id, node) in &self.record.dag.nodes {
            let changes = if *id == BASE_STEP {
                self.base_manifest()
                    .map(|manifest| manifest.entries.len())
                    .unwrap_or(0)
            } else {
                self.step_delta(*id)
                    .map(|delta| delta.entries.len())
                    .unwrap_or(0)
            };
            steps.push(WinStepSummary {
                step: *id,
                parent: node.parent_id,
                label: node.label.clone(),
                timestamp_unix_ms: node.timestamp_unix_ms,
                head: *id == self.record.dag.head_step,
                rescue: node.label.starts_with(RESCUE_PREFIX),
                changes,
            });
        }
        WinLogReport {
            session_id: self.record.session_id.clone(),
            status: self.record.status.as_str().to_string(),
            head: self.record.dag.head_step,
            workspace: paths::display(&self.record.workspace),
            managed_workspace: paths::display(&self.managed_workspace()),
            steps,
        }
    }

    pub fn diff(&self, step: Option<u64>) -> WinResult<WinDiffReport> {
        let step = step.unwrap_or(self.record.dag.head_step);
        let base = self.base_manifest()?;
        let target = self.manifest_at(step)?;
        let mut changes = scan::diff_manifests(&base, &target);
        self.attach_line_stats(&mut changes)?;
        let head_manifest = self.head_manifest()?;
        let live = self.scan_live()?;
        let uncheckpointed = !scan::manifests_equal(&head_manifest, &live);
        Ok(WinDiffReport {
            session_id: self.record.session_id.clone(),
            base_step: BASE_STEP,
            step,
            head: self.record.dag.head_step,
            summary: WinDiffSummary::from_changes(&changes),
            changes,
            uncheckpointed,
        })
    }

    pub fn rollback(&mut self, target: u64) -> WinResult<WinRollbackReport> {
        self.ensure_active()?;
        if !self.record.dag.nodes.contains_key(&target) {
            return Err(WinError::new(
                "WIN_DAG",
                format!("step {target} does not exist in this session"),
            ));
        }
        let previous_head = self.record.dag.head_step;
        let head_manifest = self.manifest_at(previous_head)?;
        let live = self.scan_live()?;
        let mut rescue_step = None;
        if !scan::manifests_equal(&head_manifest, &live) {
            let rescue = self.checkpoint(Some(format!("{RESCUE_PREFIX} to step {target}")))?;
            rescue_step = Some(rescue.step);
            self.record.rescue_count = self.record.rescue_count.saturating_add(1);
        }

        let target_manifest = self.manifest_at(target)?;
        let generation = self.next_generation()?;
        let relative_live = format!("generations/gen-{generation}");
        let destination = self.dir.join(&relative_live);
        let objects = self.store.objects(&self.record.session_id)?;
        let files = materialize(&objects, &target_manifest, &destination)?;

        self.record.live_workspace = relative_live;
        self.record.dag.rollback(target)?;
        self.record.rollback_count = self.record.rollback_count.saturating_add(1);
        self.record.updated_at_unix_ms = store::now_ms();
        self.live_workspace = destination.clone();
        self.save()?;

        let editor = self.update_editor_workspace(generation)?;
        let report = WinRollbackReport {
            session_id: self.record.session_id.clone(),
            target,
            previous_head,
            rescue_step,
            head: self.record.dag.head_step,
            managed_workspace: paths::display(&editor),
            generation_workspace: paths::display(&destination),
            materialized_files: files,
        };
        Ok(report)
    }

    pub fn discard(&mut self) -> WinResult<WinDiscardReport> {
        let head = self.head_manifest();
        let live = self.scan_live();
        let uncheckpointed = match (head, live) {
            (Ok(head), Ok(live)) => !scan::manifests_equal(&head, &live),
            _ => true,
        };
        let freed = store::directory_size(&self.dir);
        self.record.status = WinSessionStatus::Discarded;
        self.record.updated_at_unix_ms = store::now_ms();
        self.save()?;
        self.lock.take();
        store::remove_session_tree(&self.store, &self.record.session_id)?;
        store::remove_index(&self.record.session_id)?;
        Ok(WinDiscardReport {
            session_id: self.record.session_id.clone(),
            workspace: paths::display(&self.record.workspace),
            freed_bytes: freed,
            discarded: true,
            uncheckpointed,
        })
    }

    pub(crate) fn ensure_active(&self) -> WinResult<()> {
        if self.record.status != WinSessionStatus::Active {
            return Err(WinError::new(
                "WIN_SESSION_NOT_ACTIVE",
                format!(
                    "session {} is {}",
                    self.record.session_id,
                    self.record.status.as_str()
                ),
            )
            .hint("start a new session with `cowbox win init`")
            .exit(2));
        }
        Ok(())
    }

    pub(crate) fn save(&self) -> WinResult<()> {
        store::write_json_atomic(
            &self.store.session_json(&self.record.session_id)?,
            &self.record,
        )
    }

    fn attach_line_stats(&self, changes: &mut [WinChange]) -> WinResult<()> {
        let objects = self.store.objects(&self.record.session_id)?;
        let mut budget = 500usize;
        for change in changes {
            if budget == 0 {
                break;
            }
            let Some(new_sha) = &change.new_sha256 else {
                continue;
            };
            let old_sha = change.old_sha256.as_deref().unwrap_or("");
            let new_path = objects.object_path(new_sha)?;
            if fs::metadata(&new_path).map(|m| m.len()).unwrap_or(0) > 1024 * 1024 {
                continue;
            }
            let new_text = fs::read_to_string(&new_path).ok();
            let old_text = if old_sha.is_empty() {
                Some(String::new())
            } else {
                let path = objects.object_path(old_sha)?;
                if fs::metadata(&path).map(|m| m.len()).unwrap_or(0) > 1024 * 1024 {
                    continue;
                }
                fs::read_to_string(path).ok()
            };
            let (Some(old_text), Some(new_text)) = (old_text, new_text) else {
                continue;
            };
            let diff = similar::TextDiff::from_lines(&old_text, &new_text);
            let mut added = 0u32;
            let mut removed = 0u32;
            for item in diff.iter_all_changes() {
                match item.tag() {
                    similar::ChangeTag::Insert => added = added.saturating_add(1),
                    similar::ChangeTag::Delete => removed = removed.saturating_add(1),
                    similar::ChangeTag::Equal => {}
                }
            }
            change.lines_added = Some(added);
            change.lines_removed = Some(removed);
            budget -= 1;
        }
        Ok(())
    }

    fn next_generation(&self) -> WinResult<u64> {
        let path = self.dir.join("generations");
        let mut highest = 0u64;
        if path.exists() {
            for entry in fs::read_dir(path)? {
                let entry = entry?;
                let name = entry.file_name().to_string_lossy().to_string();
                if let Some(number) = name.strip_prefix("gen-") {
                    if let Ok(number) = number.parse::<u64>() {
                        highest = highest.max(number);
                    }
                }
            }
        }
        Ok(highest + 1)
    }

    fn update_editor_workspace(&mut self, generation: u64) -> WinResult<PathBuf> {
        let live_relative = self.record.live_workspace.clone();
        let current = self.dir.join("current");
        if store::create_junction(&current, &self.live_workspace).is_ok() {
            self.record.editor_workspace = "current".to_string();
            self.record.updated_at_unix_ms = store::now_ms();
            self.save()?;
            return Ok(current);
        }
        let alternate = self.dir.join(format!("workspace-{generation}"));
        if store::create_junction(&alternate, &self.live_workspace).is_ok() {
            self.record.editor_workspace = format!("workspace-{generation}");
            self.record.updated_at_unix_ms = store::now_ms();
            self.save()?;
            return Ok(alternate);
        }
        self.record.editor_workspace = live_relative;
        self.record.updated_at_unix_ms = store::now_ms();
        self.save()?;
        Ok(self.live_workspace.clone())
    }
}

pub fn init_session(workspace: &Path, state_root: &Path) -> WinResult<WinInitReport> {
    let workspace = paths::canonicalize(workspace)?;
    if !workspace.is_dir() {
        return Err(WinError::new(
            "WIN_WORKSPACE",
            format!("{} is not a directory", paths::display(&workspace)),
        )
        .exit(2));
    }
    let store = WinStore::new(state_root)?;
    store.ensure_layout()?;
    paths::ensure_outside(&store.state_root, &workspace)?;
    let state_fs = store::ensure_ntfs(&store.state_root, "state root")?;
    let workspace_fs = store::ensure_ntfs(&workspace, "workspace")?;
    let excludes = crate::policy::DEFAULT_EXCLUDES
        .iter()
        .map(|item| (*item).to_string())
        .collect::<Vec<_>>();
    let manifest = scan::scan_tree(&workspace, &excludes)?;
    let files = manifest.files().count();
    let directories = manifest.entries.len().saturating_sub(files);
    let source_bytes = manifest.files().map(|(_, entry)| entry.size).sum::<u64>();
    let estimated_bytes = source_bytes
        .saturating_mul(2)
        .saturating_add(64 * 1024 * 1024);
    if state_fs.available_bytes < estimated_bytes {
        return Err(WinError::new(
            "WIN_SPACE",
            format!(
                "state root {} has {} bytes available, but importing this workspace needs about {} bytes",
                paths::display(&store.state_root),
                state_fs.available_bytes,
                estimated_bytes
            ),
        )
        .hint("free space, choose a larger NTFS volume, or exclude generated directories")
        .exit(2));
    }
    let hardlinks = manifest
        .files()
        .filter(|(_, entry)| entry.hardlinks > 1)
        .map(|(key, _)| key.clone())
        .collect::<Vec<_>>();
    let _ = workspace_fs;

    let mut session_id = store::new_session_id();
    while store.session_dir(&session_id)?.exists() {
        session_id = store::new_session_id();
    }
    let staging = store.sessions_dir().join(format!(".init-{session_id}"));
    if staging.exists() {
        fs::remove_dir_all(&staging)?;
    }
    let result = (|| -> WinResult<WinInitReport> {
        fs::create_dir_all(staging.join("steps"))?;
        fs::create_dir_all(staging.join("commits"))?;
        let objects = ObjectStore::new(staging.join("objects"));
        fs::create_dir_all(&objects.root)?;
        for (key, entry) in manifest.files() {
            objects.put_file(
                &paths::join_key(&workspace, key)?,
                entry.sha256.as_deref().unwrap_or_default(),
            )?;
        }
        store::write_json_atomic(&staging.join("base.json"), &manifest)?;
        let generation_relative = "generations/gen-1".to_string();
        let generation = staging.join(&generation_relative);
        materialize(&objects, &manifest, &generation)?;
        let imported = scan::scan_tree(&generation, &excludes)?;
        if !scan::manifests_equal(&manifest, &imported) {
            return Err(WinError::new(
                "WIN_IMPORT_VERIFY_FAILED",
                "the managed workspace does not match the imported source manifest",
            )
            .hint("check disk health and retry init"));
        }
        let now = store::now_ms();
        let record = WinSessionRecord {
            schema_version: WIN_SESSION_SCHEMA_VERSION,
            storage_mode: WIN_STORAGE_MODE.to_string(),
            session_id: session_id.clone(),
            workspace: workspace.clone(),
            state_root: store.state_root.clone(),
            live_workspace: generation_relative.clone(),
            editor_workspace: generation_relative,
            status: WinSessionStatus::Active,
            created_at_unix_ms: now,
            updated_at_unix_ms: now,
            base_manifest_sha256: manifest.sha256(),
            rollback_count: 0,
            rescue_count: 0,
            last_commit_unix_ms: None,
            last_journal: None,
            dag: cowbox_dag::SessionDag::new(session_id.clone(), workspace.clone()),
        };
        store::write_json_atomic(&staging.join("session.json"), &record)?;
        let final_dir = store.session_dir(&session_id)?;
        fs::rename(&staging, &final_dir)?;
        let mut record = record;
        let current = final_dir.join("current");
        let generation_final = final_dir.join("generations/gen-1");
        let managed_workspace = if store::create_junction(&current, &generation_final).is_ok() {
            record.editor_workspace = "current".to_string();
            record.updated_at_unix_ms = store::now_ms();
            store::write_json_atomic(&final_dir.join("session.json"), &record)?;
            current
        } else {
            generation_final.clone()
        };
        let _ = store::register_index(&session_id, &store.state_root, &workspace);
        Ok(WinInitReport {
            session_id,
            workspace: paths::display(&workspace),
            managed_workspace: paths::display(&managed_workspace),
            generation_workspace: paths::display(&generation_final),
            state_root: paths::display(&store.state_root),
            storage_mode: WIN_STORAGE_MODE.to_string(),
            files,
            directories,
            source_bytes,
            estimated_bytes,
            available_bytes: state_fs.available_bytes,
            hardlinks,
            excludes,
        })
    })();
    if result.is_err() && staging.exists() {
        let _ = fs::remove_dir_all(&staging);
    }
    result
}

fn materialize(
    objects: &ObjectStore,
    manifest: &WinManifest,
    destination: &Path,
) -> WinResult<usize> {
    fs::create_dir_all(destination)?;
    let mut directories = manifest
        .entries
        .iter()
        .filter(|(_, entry)| entry.kind == WinEntryKind::Directory)
        .collect::<Vec<_>>();
    directories.sort_by_key(|(key, _)| paths::depth(key));
    for (key, _) in &directories {
        fs::create_dir_all(paths::join_key(destination, key)?)?;
    }
    let mut files = manifest.files().collect::<Vec<_>>();
    files.sort_by(|left, right| left.0.cmp(right.0));
    for (key, entry) in &files {
        let target = paths::join_key(destination, key)?;
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent)?;
        }
        objects.copy_to(
            entry.sha256.as_deref().unwrap_or_default(),
            &target,
            entry.readonly,
            entry.mtime_unix_ms,
        )?;
    }
    Ok(files.len())
}

fn validate_record(
    record: &WinSessionRecord,
    session_id: &str,
    state_root: &Path,
) -> WinResult<()> {
    if record.schema_version != WIN_SESSION_SCHEMA_VERSION {
        return Err(WinError::new(
            "WIN_STATE_CORRUPTED",
            format!(
                "unsupported Windows session schema {}",
                record.schema_version
            ),
        ));
    }
    if record.session_id != session_id {
        return Err(WinError::new(
            "WIN_STATE_CORRUPTED",
            format!(
                "session file contains id {} but expected {session_id}",
                record.session_id
            ),
        ));
    }
    if record.storage_mode != WIN_STORAGE_MODE {
        return Err(WinError::new(
            "WIN_STATE_CORRUPTED",
            format!("unsupported storage mode {:?}", record.storage_mode),
        ));
    }
    if record.state_root != state_root {
        return Err(WinError::new(
            "WIN_STATE_ROOT",
            format!(
                "session {session_id} belongs to state root {}, not {}",
                paths::display(&record.state_root),
                paths::display(state_root)
            ),
        ));
    }
    paths::validate_key(&record.live_workspace)?;
    paths::validate_key(&record.editor_workspace)?;
    if record.dag.validate().is_err() || record.dag.session_id != session_id {
        return Err(WinError::new(
            "WIN_STATE_CORRUPTED",
            "session DAG is invalid",
        ));
    }
    Ok(())
}

pub fn active_sessions_for_workspace(workspace: &Path) -> WinResult<Vec<WinSessionSummary>> {
    let workspace = paths::canonicalize(workspace)?;
    let entries = store::index_entries_for_workspace(&workspace)?;
    let mut sessions = Vec::new();
    for (session_id, state_root) in entries {
        let store = WinStore::new(&state_root)?;
        let path = store.session_json(&session_id)?;
        let Ok(record) = store::read_json::<WinSessionRecord>(&path) else {
            continue;
        };
        if record.status == WinSessionStatus::Active {
            let dir = store.session_dir(&session_id)?;
            sessions.push(WinSessionSummary {
                id: record.session_id,
                workspace: paths::display(&record.workspace),
                managed_workspace: paths::display(&dir.join(&record.editor_workspace)),
                state_root: paths::display(&store.state_root),
                status: record.status.as_str().to_string(),
                head: record.dag.head_step,
                storage_mode: record.storage_mode,
            });
        }
    }
    Ok(sessions)
}
