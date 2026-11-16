use crate::error::{CoreError, Result};
use crate::layout::SessionLayout;
use crate::manifest::{self, ManifestEntry};
use crate::model::{EntryKind, LayerUsage};
use crate::whiteout::{self, UpperEntryKind};
use filetime::{FileTime, set_file_mtime, set_symlink_file_times};
use rustix::fs::{CWD, FileType, Mode, XattrFlags, mknodat};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::ops::Bound::{Excluded, Unbounded};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Component, Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};
use walkdir::WalkDir;

pub(crate) const MAX_MOUNT_LAYERS: usize = 150;
pub(crate) const MAX_LOWERDIR_BYTES: usize = 3800;

const COMPACTION_SCHEMA_VERSION: u32 = 1;
const KEEP_RECENT_LAYERS: usize = 32;
const OPAQUE_XATTR: &[u8] = b"user.overlay.opaque";
const PERMISSION_JOURNAL: &str = "source-permissions.jsonl";
const PERMISSION_JOURNAL_SUFFIX: &str = ".permissions.jsonl";

#[derive(Debug, Clone)]
pub(crate) struct MountLayer {
    pub upper_dir: PathBuf,
    pub alias: String,
    pub covered_logical: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct BaseFingerprint {
    len: u64,
    mtime_ns: i128,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct CompactionMetadata {
    schema_version: u32,
    source_layers: Vec<u64>,
    base: BaseFingerprint,
    entries: usize,
    disk_bytes: u64,
    created_at_unix_ms: u64,
}

#[derive(Debug, Clone)]
struct Compaction {
    upper_dir: PathBuf,
    metadata: CompactionMetadata,
}

#[derive(Debug, Clone)]
struct EffectiveEntry {
    kind: EntryKind,
    mode: u32,
    size: u64,
    symlink_target: Option<PathBuf>,
    xattr_digest: Option<String>,
    source_path: PathBuf,
    dev: Option<u64>,
    ino: Option<u64>,
    nlink: Option<u64>,
    mtime_ns: i128,
}

#[derive(Debug, Clone)]
struct LayerItem {
    rel_path: PathBuf,
    kind: UpperEntryKind,
    metadata: fs::Metadata,
    source_path: PathBuf,
}

#[derive(Debug, Default)]
struct EffectiveState {
    entries: BTreeMap<PathBuf, EffectiveEntry>,
    deletions: BTreeSet<PathBuf>,
    opaques: BTreeSet<PathBuf>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct PermissionRecord {
    path: PathBuf,
    mode: u32,
}

#[derive(Debug)]
pub(crate) struct AccessRecords {
    session_dir: PathBuf,
    journal: Option<PathBuf>,
    records: Vec<PermissionRecord>,
    overrides: HashMap<PathBuf, u32>,
    restored: bool,
}

impl AccessRecords {
    pub(crate) fn overrides(&self) -> &HashMap<PathBuf, u32> {
        &self.overrides
    }

    pub(crate) fn restore(&mut self) -> Result<()> {
        if self.restored {
            return Ok(());
        }
        restore_permission_records(&self.session_dir, &self.records, self.journal.is_some())?;
        if let Some(journal) = self.journal.take() {
            let parent = journal.parent().map(Path::to_path_buf);
            match fs::remove_file(&journal) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.into()),
            }
            if let Some(parent) = parent {
                sync_directory(&parent);
            }
        }
        self.restored = true;
        Ok(())
    }
}

impl Drop for AccessRecords {
    fn drop(&mut self) {
        let _ = self.restore();
    }
}

#[derive(Debug)]
enum DirectorySource {
    Layer(PathBuf, u32),
    Base(PathBuf),
    Generated,
}

pub(crate) fn alias_dir(layout: &SessionLayout, step_id: u64) -> PathBuf {
    std::env::temp_dir().join(format!("cw{}{}", layout.session_id, step_id))
}

pub(crate) fn prepare_mount_layers(
    layout: &SessionLayout,
    step_id: u64,
    parent_layers: &[u64],
) -> Result<Vec<MountLayer>> {
    cleanup_stale_temporary_directories(layout)?;
    if parent_layers.is_empty() {
        return Ok(Vec::new());
    }
    for layer in parent_layers {
        if !layout.upper_dir(*layer).is_dir() {
            return Err(CoreError::StateCorrupted {
                step: *layer,
                reason: format!(
                    "missing parent layer {}",
                    layout.upper_dir(*layer).display()
                ),
            });
        }
    }

    let physical = resolve_physical_layers(layout, parent_layers)?;
    if !needs_compaction(layout, step_id, &physical)? {
        return Ok(physical);
    }
    if physical.len() <= 1 {
        return Err(compaction_limit_error(layout, step_id, &physical));
    }

    let mut keep_recent = KEEP_RECENT_LAYERS.min(physical.len() - 1);
    loop {
        let prefix_len = physical.len() - keep_recent;
        let covered_logical = physical[..prefix_len]
            .iter()
            .map(|layer| layer.covered_logical)
            .sum::<usize>();
        if covered_logical == 0 || covered_logical > parent_layers.len() {
            return Err(CoreError::StateCorrupted {
                step: parent_layers[0],
                reason: "compaction metadata covered the wrong logical prefix".to_string(),
            });
        }
        let compacted = build_compacted_layer(
            layout,
            step_id,
            &parent_layers[..covered_logical],
            &physical[..prefix_len],
        )?;
        let mut candidate = Vec::with_capacity(physical.len() - prefix_len + 1);
        candidate.push(compacted);
        candidate.extend_from_slice(&physical[prefix_len..]);
        if fits_mount_limits(layout, step_id, &candidate)? || keep_recent == 1 {
            if !fits_mount_limits(layout, step_id, &candidate)? {
                return Err(compaction_limit_error(layout, step_id, &candidate));
            }
            return Ok(candidate);
        }
        keep_recent = (keep_recent / 2).max(1);
    }
}

pub(crate) fn inspect_layer_usage(
    layout: &SessionLayout,
    logical_layers: &[u64],
) -> Result<LayerUsage> {
    let physical = resolve_physical_layers(layout, logical_layers)?;
    let compactions = discover_compactions(layout)?;
    Ok(LayerUsage {
        logical_steps: logical_layers.len(),
        mounted_layers: physical.len().saturating_add(1),
        compact_layers: compactions.len(),
        compacted_bytes: compactions
            .iter()
            .map(|compaction| compaction.metadata.disk_bytes)
            .sum(),
    })
}

fn resolve_physical_layers(
    layout: &SessionLayout,
    logical_layers: &[u64],
) -> Result<Vec<MountLayer>> {
    let base = base_fingerprint(layout)?;
    let mut reusable = discover_compactions(layout)?
        .into_iter()
        .filter(|compaction| compaction.metadata.base == base)
        .filter(|compaction| {
            !compaction.metadata.source_layers.is_empty()
                && compaction.metadata.source_layers.len() <= logical_layers.len()
                && logical_layers.starts_with(&compaction.metadata.source_layers)
        })
        .collect::<Vec<_>>();
    reusable.sort_by_key(|compaction| std::cmp::Reverse(compaction.metadata.source_layers.len()));

    if let Some(compaction) = reusable.into_iter().next() {
        let mut layers = Vec::new();
        layers.push(MountLayer {
            upper_dir: compaction.upper_dir,
            alias: format!(
                "c{}",
                compaction
                    .metadata
                    .source_layers
                    .last()
                    .copied()
                    .unwrap_or_default()
            ),
            covered_logical: compaction.metadata.source_layers.len(),
        });
        layers.extend(
            logical_layers[compaction.metadata.source_layers.len()..]
                .iter()
                .map(|layer| MountLayer {
                    upper_dir: layout.upper_dir(*layer),
                    alias: format!("l{layer}"),
                    covered_logical: 1,
                }),
        );
        return Ok(layers);
    }

    Ok(logical_layers
        .iter()
        .map(|layer| MountLayer {
            upper_dir: layout.upper_dir(*layer),
            alias: format!("l{layer}"),
            covered_logical: 1,
        })
        .collect())
}

fn discover_compactions(layout: &SessionLayout) -> Result<Vec<Compaction>> {
    let root = compacted_root(layout);
    let Ok(entries) = fs::read_dir(&root) else {
        return Ok(Vec::new());
    };
    let mut compactions = Vec::new();
    for entry in entries {
        let entry = entry?;
        let metadata = fs::symlink_metadata(entry.path())?;
        if !metadata.is_dir() {
            continue;
        }
        let dir = entry.path();
        let upper_dir = dir.join("upper");
        if fs::symlink_metadata(&upper_dir)
            .map(|item| !item.is_dir())
            .unwrap_or(true)
        {
            continue;
        }
        let metadata_path = dir.join("layer.json");
        let bytes = fs::read(&metadata_path).map_err(|error| CoreError::StateCorrupted {
            step: 0,
            reason: format!(
                "cannot read compacted layer metadata {}: {error}",
                metadata_path.display()
            ),
        })?;
        let metadata: CompactionMetadata =
            serde_json::from_slice(&bytes).map_err(|error| CoreError::StateCorrupted {
                step: 0,
                reason: format!(
                    "invalid compacted layer metadata {}: {error}",
                    metadata_path.display()
                ),
            })?;
        if metadata.schema_version != COMPACTION_SCHEMA_VERSION {
            return Err(CoreError::StateCorrupted {
                step: 0,
                reason: format!(
                    "unsupported compacted layer schema {} in {}",
                    metadata.schema_version,
                    metadata_path.display()
                ),
            });
        }
        compactions.push(Compaction {
            upper_dir,
            metadata,
        });
    }
    Ok(compactions)
}

fn build_compacted_layer(
    layout: &SessionLayout,
    step_id: u64,
    source_layers: &[u64],
    source_physical: &[MountLayer],
) -> Result<MountLayer> {
    let base = base_fingerprint(layout)?;
    let key = compaction_key(source_layers);
    let target = compacted_root(layout).join(&key);
    if target.exists() {
        let metadata = read_compaction_metadata(&target)?;
        if metadata.source_layers != source_layers || metadata.base != base {
            return Err(CoreError::StateCorrupted {
                step: source_layers[0],
                reason: format!(
                    "compacted layer {} does not match its source chain",
                    target.display()
                ),
            });
        }
        return Ok(MountLayer {
            upper_dir: target.join("upper"),
            alias: format!("c{}", source_layers.last().copied().unwrap_or_default()),
            covered_logical: source_layers.len(),
        });
    }

    let temp_root = temporary_root(layout, step_id);
    if temp_root.exists() {
        remove_temporary_tree(&temp_root)?;
    }
    let temp_upper = temp_root.join("upper");
    fs::create_dir_all(&temp_upper)?;
    fs::set_permissions(&temp_upper, fs::Permissions::from_mode(0o700))?;

    let source_dirs = source_physical
        .iter()
        .map(|layer| layer.upper_dir.clone())
        .collect::<Vec<_>>();
    let permission_journal = temporary_permission_journal(&temp_root);
    let mut source_access =
        prepare_access(layout, &source_dirs, Some(permission_journal.as_path()))?;
    test_crash("compaction-after-source-permissions");
    let result = (|| -> Result<CompactionMetadata> {
        let expected = analyze_layers_with_modes(&source_dirs, Some(&source_access.overrides))
            .map_err(|error| compaction_phase("analyze source layers", error))?;
        materialize(&expected, &temp_upper, layout)
            .map_err(|error| compaction_phase("materialize compacted layer", error))?;
        test_crash("compaction-after-materialize");
        test_fail(
            "compaction-after-materialize",
            "injected failure after materializing the compacted layer",
        )?;

        let mut temp_access = prepare_access(layout, std::slice::from_ref(&temp_upper), None)
            .map_err(|error| compaction_phase("prepare compacted validation", error))?;
        let actual = analyze_layers_with_modes(
            std::slice::from_ref(&temp_upper),
            Some(&temp_access.overrides),
        )
        .map_err(|error| compaction_phase("analyze compacted layer", error))?;
        validate(&expected, &actual, layout)
            .map_err(|error| compaction_phase("validate compacted layer", error))?;
        let disk_bytes = allocated_bytes(&temp_upper)
            .map_err(|error| compaction_phase("measure compacted layer", error))?;
        source_access
            .restore()
            .map_err(|error| compaction_phase("restore source modes", error))?;
        temp_access
            .restore()
            .map_err(|error| compaction_phase("restore compacted modes", error))?;
        test_crash("compaction-after-validate");

        let metadata = CompactionMetadata {
            schema_version: COMPACTION_SCHEMA_VERSION,
            source_layers: source_layers.to_vec(),
            base,
            entries: expected.entries.len(),
            disk_bytes,
            created_at_unix_ms: now_unix_ms(),
        };
        write_metadata(&temp_root, &metadata)
            .map_err(|error| compaction_phase("write compacted metadata", error))?;

        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent)?;
        }
        if target.exists() {
            remove_temporary_tree(&temp_root)?;
            return read_compaction_metadata(&target);
        }
        test_fail(
            "compaction-before-publish",
            "injected failure before publishing the compacted layer",
        )?;
        fs::rename(&temp_root, &target)
            .map_err(|error| compaction_phase("publish compacted layer", error.into()))?;
        sync_directory(target.parent().unwrap_or(&target));
        test_crash("compaction-after-publish");
        Ok(metadata)
    })();

    match result {
        Ok(metadata) => Ok(MountLayer {
            upper_dir: target.join("upper"),
            alias: format!(
                "c{}",
                metadata.source_layers.last().copied().unwrap_or_default()
            ),
            covered_logical: metadata.source_layers.len(),
        }),
        Err(error) => {
            let _ = source_access.restore();
            let _ = remove_temporary_tree(&temp_root);
            Err(error)
        }
    }
}

#[cfg(test)]
fn analyze_layers(directories: &[PathBuf]) -> Result<EffectiveState> {
    analyze_layers_with_modes(directories, None)
}

fn analyze_layers_with_modes(
    directories: &[PathBuf],
    mode_overrides: Option<&HashMap<PathBuf, u32>>,
) -> Result<EffectiveState> {
    let mut state = EffectiveState::default();
    for directory in directories {
        let mut items = Vec::new();
        for entry in WalkDir::new(directory)
            .follow_links(false)
            .min_depth(1)
            .into_iter()
        {
            let entry = entry.map_err(|error| CoreError::StateCorrupted {
                step: 0,
                reason: format!(
                    "cannot scan compacted source layer {}: {error}",
                    directory.display()
                ),
            })?;
            let rel_path = entry
                .path()
                .strip_prefix(directory)
                .map_err(|_| CoreError::StateCorrupted {
                    step: 0,
                    reason: "compaction input escaped its layer".to_string(),
                })?
                .to_path_buf();
            if rel_path.as_os_str().is_empty() {
                continue;
            }
            let metadata = fs::symlink_metadata(entry.path())?;
            items.push(LayerItem {
                rel_path,
                kind: whiteout::classify_upper_entry(entry.path(), &metadata),
                metadata,
                source_path: entry.path().to_path_buf(),
            });
        }
        items.sort_unstable_by(|left, right| left.rel_path.cmp(&right.rel_path));
        for item in items {
            match item.kind {
                UpperEntryKind::Whiteout(_) => {
                    remove_subtree(&mut state, &item.rel_path);
                    state.deletions.insert(item.rel_path);
                }
                UpperEntryKind::OpaqueDirectory => {
                    remove_subtree(&mut state, &item.rel_path);
                    state.opaques.insert(item.rel_path.clone());
                    state.entries.insert(
                        item.rel_path,
                        EffectiveEntry::from_metadata(
                            &item.source_path,
                            &item.metadata,
                            mode_overrides
                                .and_then(|overrides| overrides.get(&item.source_path).copied()),
                        )?,
                    );
                }
                UpperEntryKind::Entry => {
                    let was_deleted = state.deletions.remove(&item.rel_path);
                    clear_ancestor_conflicts(&mut state, &item.rel_path);
                    let entry = EffectiveEntry::from_metadata(
                        &item.source_path,
                        &item.metadata,
                        mode_overrides
                            .and_then(|overrides| overrides.get(&item.source_path).copied()),
                    )?;
                    let previous_kind = state.entries.get(&item.rel_path).map(|entry| entry.kind);
                    let merge_directory_children = entry.kind == EntryKind::Directory
                        && !was_deleted
                        && !matches!(
                            previous_kind,
                            Some(EntryKind::File | EntryKind::Symlink | EntryKind::Other)
                        );
                    if !merge_directory_children {
                        remove_subtree(&mut state, &item.rel_path);
                    }
                    if entry.kind == EntryKind::Directory && was_deleted {
                        state.opaques.insert(item.rel_path.clone());
                    }
                    state.entries.insert(item.rel_path, entry);
                }
            }
        }
    }
    validate_state_shape(&state)?;
    Ok(state)
}

pub(crate) fn prepare_access(
    layout: &SessionLayout,
    roots: &[PathBuf],
    journal_path: Option<&Path>,
) -> Result<AccessRecords> {
    let mut access = AccessRecords {
        session_dir: layout.session_dir.clone(),
        journal: None,
        records: Vec::new(),
        overrides: HashMap::new(),
        restored: false,
    };
    let mut journal = None;
    let mut file_records = Vec::new();
    let result = (|| -> Result<()> {
        for root in roots {
            scan_source_access(
                layout,
                root,
                &mut access,
                journal_path,
                &mut journal,
                &mut file_records,
            )?;
        }
        if !file_records.is_empty() {
            if let Some(journal) =
                ensure_permission_journal(&mut access, &mut journal, journal_path)?
            {
                for record in &file_records {
                    write_permission_record(journal, record)?;
                }
                journal.flush()?;
                journal.sync_all()?;
                if let Some(parent) = journal_path.and_then(Path::parent) {
                    sync_directory(parent);
                }
            }
        }
        for record in file_records {
            let path = resolve_permission_path(&layout.session_dir, &record.path)?;
            access.records.push(record.clone());
            access.overrides.insert(path.clone(), record.mode);
            fs::set_permissions(&path, fs::Permissions::from_mode(record.mode | 0o400))?;
        }
        Ok(())
    })();
    if let Err(error) = result {
        let _ = access.restore();
        return Err(error);
    }
    Ok(access)
}

fn scan_source_access(
    layout: &SessionLayout,
    path: &Path,
    access: &mut AccessRecords,
    journal_path: Option<&Path>,
    journal: &mut Option<fs::File>,
    file_records: &mut Vec<PermissionRecord>,
) -> Result<()> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() {
        return Ok(());
    }
    let mode = metadata.mode() & 0o7777;
    if metadata.is_dir() {
        if mode & 0o500 != 0o500 {
            let record = permission_record(layout, path, mode)?;
            if let Some(journal) = ensure_permission_journal(access, journal, journal_path)? {
                write_permission_record(journal, &record)?;
                journal.flush()?;
                journal.sync_all()?;
            }
            access.records.push(record.clone());
            access.overrides.insert(path.to_path_buf(), mode);
            fs::set_permissions(path, fs::Permissions::from_mode(mode | 0o500))?;
        }
        for entry in fs::read_dir(path)? {
            scan_source_access(
                layout,
                &entry?.path(),
                access,
                journal_path,
                journal,
                file_records,
            )?;
        }
    } else if metadata.is_file() && mode & 0o400 == 0 {
        file_records.push(permission_record(layout, path, mode)?);
    }
    Ok(())
}

fn ensure_permission_journal<'a>(
    access: &mut AccessRecords,
    journal: &'a mut Option<fs::File>,
    journal_path: Option<&Path>,
) -> Result<Option<&'a mut fs::File>> {
    if journal.is_none() {
        let Some(path) = journal_path else {
            return Ok(None);
        };
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let file = OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .open(path)?;
        file.sync_all()?;
        if let Some(parent) = path.parent() {
            sync_directory(parent);
        }
        *journal = Some(OpenOptions::new().append(true).open(path)?);
        access.journal = Some(path.to_path_buf());
    }
    Ok(journal.as_mut())
}

fn permission_record(layout: &SessionLayout, path: &Path, mode: u32) -> Result<PermissionRecord> {
    let relative =
        path.strip_prefix(&layout.session_dir)
            .map_err(|_| CoreError::StateCorrupted {
                step: 0,
                reason: format!(
                    "permission adjustment escaped the session: {}",
                    path.display()
                ),
            })?;
    validate_permission_relative(relative)?;
    Ok(PermissionRecord {
        path: relative.to_path_buf(),
        mode,
    })
}

fn write_permission_record(file: &mut fs::File, record: &PermissionRecord) -> Result<()> {
    serde_json::to_writer(&mut *file, record)?;
    file.write_all(b"\n")?;
    Ok(())
}

fn restore_permission_records(
    session_dir: &Path,
    records: &[PermissionRecord],
    fault_sensitive: bool,
) -> Result<()> {
    let unique = unique_permission_records(records)?;

    let mut ancestors = BTreeSet::new();
    for relative in unique.keys() {
        for ancestor in relative.ancestors().skip(1) {
            if !ancestor.as_os_str().is_empty() {
                ancestors.insert(ancestor.to_path_buf());
            }
        }
    }
    let mut ancestors = ancestors.into_iter().collect::<Vec<_>>();
    ancestors.sort_by_key(|relative| (relative.components().count(), relative.clone()));
    for relative in ancestors {
        let path = session_dir.join(&relative);
        let recorded_mode = unique.get(&relative).copied();
        match fs::symlink_metadata(&path) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(CoreError::StateCorrupted {
                    step: 0,
                    reason: format!(
                        "permission journal referenced a symlink: {}",
                        path.display()
                    ),
                });
            }
            Ok(metadata) if !metadata.is_dir() => {
                return Err(CoreError::StateCorrupted {
                    step: 0,
                    reason: format!(
                        "permission journal expected a directory at {}",
                        path.display()
                    ),
                });
            }
            Ok(metadata) => match recorded_mode {
                Some(original) => {
                    let elevated = original | 0o500;
                    if metadata.mode() & 0o7777 != elevated {
                        fs::set_permissions(&path, fs::Permissions::from_mode(elevated))?;
                    }
                }
                None if metadata.mode() & 0o100 == 0 => {
                    return Err(CoreError::StateCorrupted {
                        step: 0,
                        reason: format!(
                            "permission journal left an unrecorded ancestor inaccessible: {}",
                            path.display()
                        ),
                    });
                }
                None => {}
            },
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    if fault_sensitive {
        test_crash("permission-restore-after-access");
    }

    let mut directories = Vec::new();
    let mut other_entries = Vec::new();
    for (relative, mode) in &unique {
        let path = session_dir.join(relative);
        let metadata = match fs::symlink_metadata(&path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error.into()),
        };
        if metadata.file_type().is_symlink() {
            return Err(CoreError::StateCorrupted {
                step: 0,
                reason: format!(
                    "permission journal referenced a symlink: {}",
                    path.display()
                ),
            });
        }
        let depth = relative.components().count();
        if metadata.is_dir() {
            directories.push((depth, relative.clone(), path, *mode));
        } else {
            other_entries.push((depth, relative.clone(), path, *mode));
        }
    }

    other_entries
        .sort_by_key(|(depth, relative, _, _)| (std::cmp::Reverse(*depth), relative.clone()));
    for (_, _, path, mode) in other_entries {
        fs::set_permissions(&path, fs::Permissions::from_mode(mode))?;
    }
    if fault_sensitive {
        test_crash("permission-restore-after-files");
        test_fail(
            "permission-restore-before-directories",
            "injected failure before restoring directory modes",
        )?;
    }

    directories
        .sort_by_key(|(depth, relative, _, _)| (std::cmp::Reverse(*depth), relative.clone()));
    for (index, (_, _, path, mode)) in directories.into_iter().enumerate() {
        fs::set_permissions(&path, fs::Permissions::from_mode(mode))?;
        if fault_sensitive && index == 0 {
            test_crash("permission-restore-after-first-directory");
        }
    }
    Ok(())
}

fn unique_permission_records(records: &[PermissionRecord]) -> Result<BTreeMap<PathBuf, u32>> {
    let mut unique = BTreeMap::new();
    for record in records {
        validate_permission_relative(&record.path)?;
        if record.mode & !0o7777 != 0 {
            return Err(CoreError::StateCorrupted {
                step: 0,
                reason: format!(
                    "permission journal contains an invalid mode {:o} for {}",
                    record.mode,
                    record.path.display()
                ),
            });
        }
        match unique.get(&record.path) {
            None => {
                unique.insert(record.path.clone(), record.mode);
            }
            Some(existing) if *existing == record.mode => {}
            Some(existing) => {
                return Err(CoreError::StateCorrupted {
                    step: 0,
                    reason: format!(
                        "permission journal contains conflicting modes {:o} and {:o} for {}",
                        existing,
                        record.mode,
                        record.path.display()
                    ),
                });
            }
        }
    }
    Ok(unique)
}

fn read_permission_journal(path: &Path) -> Result<Vec<PermissionRecord>> {
    let bytes = fs::read(path)?;
    let mut records = Vec::new();
    let mut offset = 0usize;
    let mut line_number = 0usize;
    while offset < bytes.len() {
        line_number += 1;
        let rest = &bytes[offset..];
        let (line, terminated, next_offset) = match rest.iter().position(|byte| *byte == b'\n') {
            Some(index) => (&rest[..index], true, offset + index + 1),
            None => (rest, false, bytes.len()),
        };
        if !line.iter().all(|byte| byte.is_ascii_whitespace()) {
            match serde_json::from_slice::<PermissionRecord>(line) {
                Ok(record) => records.push(record),
                Err(error) if !terminated && error.is_eof() => break,
                Err(error) => {
                    return Err(CoreError::StateCorrupted {
                        step: 0,
                        reason: format!(
                            "invalid permission journal {} at line {}: {error}",
                            path.display(),
                            line_number
                        ),
                    });
                }
            }
        }
        offset = next_offset;
    }
    Ok(records)
}

fn restore_permission_journal_file(session_dir: &Path, journal_path: &Path) -> Result<()> {
    let records = read_permission_journal(journal_path)?;
    restore_permission_records(session_dir, &records, true)?;
    match fs::remove_file(journal_path) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    if let Some(parent) = journal_path.parent() {
        sync_directory(parent);
    }
    Ok(())
}

fn is_permission_journal_name(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.ends_with(PERMISSION_JOURNAL_SUFFIX) || name == PERMISSION_JOURNAL)
}

pub(crate) fn access_journal_path(layout: &SessionLayout, prefix: &str) -> PathBuf {
    layout.session_dir.join("layers").join("tmp").join(format!(
        "{prefix}-{}-{}{}",
        std::process::id(),
        now_unix_ms(),
        PERMISSION_JOURNAL_SUFFIX
    ))
}

pub(crate) fn recover_pending_permissions(layout: &SessionLayout) -> Result<()> {
    let root = layout.session_dir.join("layers").join("tmp");
    let entries = match fs::read_dir(&root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.into()),
    };
    let mut journals = Vec::new();
    for entry in entries {
        let path = entry?.path();
        let metadata = fs::symlink_metadata(&path)?;
        if metadata.file_type().is_symlink() {
            if is_permission_journal_name(&path) {
                return Err(CoreError::StateCorrupted {
                    step: 0,
                    reason: format!("permission journal is a symlink: {}", path.display()),
                });
            }
            continue;
        }
        if metadata.is_file() {
            if is_permission_journal_name(&path) {
                journals.push(path);
            }
            continue;
        }
        if metadata.is_dir() {
            let legacy = path.join(PERMISSION_JOURNAL);
            match fs::symlink_metadata(&legacy) {
                Ok(metadata) if metadata.is_file() && !metadata.file_type().is_symlink() => {
                    journals.push(legacy);
                }
                Ok(_) => {
                    return Err(CoreError::StateCorrupted {
                        step: 0,
                        reason: format!(
                            "legacy permission journal is not a regular file: {}",
                            legacy.display()
                        ),
                    });
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.into()),
            }
        }
    }
    journals.sort();
    for journal in journals {
        restore_permission_journal_file(&layout.session_dir, &journal)?;
    }
    Ok(())
}

fn resolve_permission_path(session_dir: &Path, relative: &Path) -> Result<PathBuf> {
    validate_permission_relative(relative)?;
    Ok(session_dir.join(relative))
}

fn validate_permission_relative(relative: &Path) -> Result<()> {
    if relative.as_os_str().is_empty()
        || relative
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
        || !(relative.starts_with("steps")
            || relative.starts_with("layers/compacted")
            || relative.starts_with("layers/tmp"))
    {
        return Err(CoreError::StateCorrupted {
            step: 0,
            reason: format!(
                "permission journal path is outside session layers: {}",
                relative.display()
            ),
        });
    }
    Ok(())
}

fn remove_subtree(state: &mut EffectiveState, rel_path: &Path) {
    let entries = state
        .entries
        .range(rel_path.to_path_buf()..)
        .take_while(|(candidate, _)| candidate.starts_with(rel_path))
        .map(|(candidate, _)| candidate.clone())
        .collect::<Vec<_>>();
    for candidate in entries {
        state.entries.remove(&candidate);
    }
    let deletions = state
        .deletions
        .range(rel_path.to_path_buf()..)
        .take_while(|candidate| candidate.starts_with(rel_path))
        .cloned()
        .collect::<Vec<_>>();
    for candidate in deletions {
        state.deletions.remove(&candidate);
    }
    let opaques = state
        .opaques
        .range(rel_path.to_path_buf()..)
        .take_while(|candidate| candidate.starts_with(rel_path))
        .cloned()
        .collect::<Vec<_>>();
    for candidate in opaques {
        state.opaques.remove(&candidate);
    }
}

fn clear_ancestor_conflicts(state: &mut EffectiveState, rel_path: &Path) {
    for ancestor in rel_path.ancestors().skip(1) {
        if state
            .entries
            .get(ancestor)
            .is_some_and(|entry| entry.kind != EntryKind::Directory)
        {
            state.entries.remove(ancestor);
        }
        if state.deletions.remove(ancestor) {
            state.opaques.insert(ancestor.to_path_buf());
        }
    }
}

fn validate_state_shape(state: &EffectiveState) -> Result<()> {
    for (path, entry) in &state.entries {
        let live_descendant = state
            .entries
            .range((Excluded(path.clone()), Unbounded))
            .next()
            .is_some_and(|(candidate, _)| candidate.starts_with(path));
        if entry.kind != EntryKind::Directory && live_descendant {
            return Err(CoreError::StateCorrupted {
                step: 0,
                reason: format!(
                    "compaction found live descendants below non-directory {}",
                    path.display()
                ),
            });
        }
        if entry.kind != EntryKind::Other && state.deletions.contains(path) {
            return Err(CoreError::StateCorrupted {
                step: 0,
                reason: format!(
                    "compaction found both a live entry and a whiteout at {}",
                    path.display()
                ),
            });
        }
    }
    for path in &state.opaques {
        if state
            .entries
            .get(path)
            .is_some_and(|entry| entry.kind != EntryKind::Directory)
        {
            return Err(CoreError::StateCorrupted {
                step: 0,
                reason: format!(
                    "compaction found an opaque marker on non-directory {}",
                    path.display()
                ),
            });
        }
    }
    Ok(())
}

impl EffectiveEntry {
    fn from_metadata(
        source_path: &Path,
        metadata: &fs::Metadata,
        mode_override: Option<u32>,
    ) -> Result<Self> {
        let kind = manifest::entry_kind(metadata);
        if kind == EntryKind::Other {
            return Err(CoreError::CowUnsupported {
                path: source_path.to_path_buf(),
                reason: "layer compaction refuses device nodes, FIFOs, and sockets".to_string(),
            });
        }
        let symlink_target = if kind == EntryKind::Symlink {
            Some(fs::read_link(source_path)?)
        } else {
            None
        };
        let xattr_digest = if matches!(kind, EntryKind::File | EntryKind::Directory) {
            Some(crate::xattr::digest_checked(source_path)?)
        } else {
            None
        };
        Ok(Self {
            kind,
            mode: mode_override.unwrap_or(metadata.mode() & 0o7777),
            size: metadata.len(),
            symlink_target,
            xattr_digest,
            source_path: source_path.to_path_buf(),
            dev: (kind == EntryKind::File).then(|| metadata.dev()),
            ino: (kind == EntryKind::File).then(|| metadata.ino()),
            nlink: (kind == EntryKind::File).then(|| metadata.nlink()),
            mtime_ns: manifest::mtime_ns(metadata),
        })
    }
}

fn materialize(state: &EffectiveState, upper: &Path, layout: &SessionLayout) -> Result<()> {
    let base = manifest::load_manifest(&layout.base_manifest_path())?;
    let mut directories: BTreeMap<PathBuf, DirectorySource> = BTreeMap::new();
    for (path, entry) in &state.entries {
        if entry.kind == EntryKind::Directory {
            directories.insert(
                path.clone(),
                DirectorySource::Layer(entry.source_path.clone(), entry.mode),
            );
        }
    }
    for path in state
        .entries
        .keys()
        .chain(state.deletions.iter())
        .chain(state.opaques.iter())
    {
        for ancestor in path
            .ancestors()
            .skip(1)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
        {
            if ancestor.as_os_str().is_empty() {
                continue;
            }
            directories
                .entry(ancestor.to_path_buf())
                .or_insert_with(|| directory_source(ancestor, &base, layout));
        }
    }

    for path in directories.keys() {
        let target = upper.join(path);
        fs::create_dir_all(&target)?;
        fs::set_permissions(&target, fs::Permissions::from_mode(0o700))?;
    }

    let mut hardlinks: HashMap<(u64, u64), PathBuf> = HashMap::new();
    for (path, entry) in &state.entries {
        if entry.kind == EntryKind::Directory {
            continue;
        }
        let target = upper.join(path);
        match entry.kind {
            EntryKind::File => {
                let key = (entry.dev.unwrap_or(0), entry.ino.unwrap_or(0));
                let linked = entry.nlink.unwrap_or(1) > 1 && hardlinks.contains_key(&key);
                if linked {
                    fs::hard_link(hardlinks.get(&key).expect("checked hardlink"), &target)?;
                } else {
                    fs::copy(&entry.source_path, &target)?;
                    fs::set_permissions(&target, fs::Permissions::from_mode(0o600))?;
                    crate::xattr::copy(&entry.source_path, &target)?;
                    set_file_mtime(
                        &target,
                        FileTime::from_unix_time(
                            entry.mtime_ns.div_euclid(1_000_000_000) as i64,
                            entry.mtime_ns.rem_euclid(1_000_000_000) as u32,
                        ),
                    )?;
                    fs::set_permissions(&target, fs::Permissions::from_mode(entry.mode))?;
                    if entry.nlink.unwrap_or(1) > 1 {
                        hardlinks.insert(key, target);
                    }
                }
            }
            EntryKind::Symlink => {
                let Some(link_target) = &entry.symlink_target else {
                    return Err(CoreError::StateCorrupted {
                        step: 0,
                        reason: format!("symlink {} has no target", path.display()),
                    });
                };
                std::os::unix::fs::symlink(link_target, &target)?;
                let _ = set_symlink_file_times(
                    &target,
                    FileTime::from_unix_time(
                        entry.mtime_ns.div_euclid(1_000_000_000) as i64,
                        entry.mtime_ns.rem_euclid(1_000_000_000) as u32,
                    ),
                    FileTime::from_unix_time(
                        entry.mtime_ns.div_euclid(1_000_000_000) as i64,
                        entry.mtime_ns.rem_euclid(1_000_000_000) as u32,
                    ),
                );
            }
            EntryKind::Directory => {}
            EntryKind::Other => {
                return Err(CoreError::CowUnsupported {
                    path: path.clone(),
                    reason: "layer compaction refuses special files".to_string(),
                });
            }
        }
    }

    for path in &state.deletions {
        let target = upper.join(path);
        if fs::symlink_metadata(&target).is_ok() {
            return Err(CoreError::StateCorrupted {
                step: 0,
                reason: format!(
                    "compaction target {} already has a live entry",
                    path.display()
                ),
            });
        }
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent)?;
        }
        mknodat(
            CWD,
            &target,
            FileType::CharacterDevice,
            Mode::from_raw_mode(0o644),
            0,
        )
        .map_err(|error| {
            CoreError::IsolationFailed(format!(
                "cannot materialize whiteout {}: {error}",
                target.display()
            ))
        })?;
    }

    let mut ordered_directories = directories.keys().cloned().collect::<Vec<_>>();
    ordered_directories.sort_by_key(|path| std::cmp::Reverse(path.components().count()));
    for path in ordered_directories {
        let target = upper.join(&path);
        finalize_directory_metadata(
            directories.get(&path),
            &target,
            state.opaques.contains(&path),
        )?;
    }
    Ok(())
}

fn directory_source(
    rel_path: &Path,
    base: &BTreeMap<PathBuf, ManifestEntry>,
    layout: &SessionLayout,
) -> DirectorySource {
    if base
        .get(rel_path)
        .is_some_and(|entry| entry.kind == EntryKind::Directory)
    {
        DirectorySource::Base(layout.workspace.join(rel_path))
    } else {
        DirectorySource::Generated
    }
}

fn finalize_directory_metadata(
    source: Option<&DirectorySource>,
    target: &Path,
    opaque: bool,
) -> Result<()> {
    let source_path = match source {
        Some(DirectorySource::Layer(path, _) | DirectorySource::Base(path)) => Some(path),
        Some(DirectorySource::Generated) | None => None,
    };
    let metadata = source_path.and_then(|path| fs::symlink_metadata(path).ok());
    if let Some(source_path) = source_path.filter(|_| metadata.is_some()) {
        crate::xattr::copy(source_path, target)?;
    }
    if opaque {
        set_xattr(target, OPAQUE_XATTR, b"y")?;
    }
    let mode = match source {
        Some(DirectorySource::Layer(_, mode)) => *mode,
        Some(DirectorySource::Base(_)) => metadata
            .as_ref()
            .map(|metadata| metadata.mode() & 0o7777)
            .unwrap_or(0o755),
        Some(DirectorySource::Generated) | None => 0o755,
    };
    if let Some(metadata) = metadata {
        let mtime = manifest::mtime_ns(&metadata);
        let _ = set_file_mtime(
            target,
            FileTime::from_unix_time(
                mtime.div_euclid(1_000_000_000) as i64,
                mtime.rem_euclid(1_000_000_000) as u32,
            ),
        );
    }
    fs::set_permissions(target, fs::Permissions::from_mode(mode))?;
    Ok(())
}

fn validate(
    expected: &EffectiveState,
    actual: &EffectiveState,
    layout: &SessionLayout,
) -> Result<()> {
    let base = manifest::load_manifest(&layout.base_manifest_path())?;
    if expected.deletions != actual.deletions {
        return Err(validation_error(format!(
            "whiteouts differ: expected {:?}, got {:?}",
            expected.deletions, actual.deletions
        )));
    }
    if expected.opaques != actual.opaques {
        return Err(validation_error(format!(
            "opaque directories differ: expected {:?}, got {:?}",
            expected.opaques, actual.opaques
        )));
    }
    for (path, expected_entry) in &expected.entries {
        let Some(actual_entry) = actual.entries.get(path) else {
            return Err(validation_error(format!(
                "compacted layer omitted {}",
                path.display()
            )));
        };
        if !entries_match(expected_entry, actual_entry)? {
            return Err(validation_error(format!(
                "compacted metadata or content differs at {}",
                path.display()
            )));
        }
    }
    for (path, actual_entry) in &actual.entries {
        if expected.entries.contains_key(path) {
            continue;
        }
        let unchanged_directory = base.get(path).is_some_and(|base_entry| {
            base_entry.kind == EntryKind::Directory
                && actual_entry.kind == EntryKind::Directory
                && actual_entry.mode == base_entry.mode
                && actual_entry.xattr_digest == base_entry.xattr_digest
        });
        if !unchanged_directory {
            return Err(validation_error(format!(
                "compacted layer introduced unexpected entry {}",
                path.display()
            )));
        }
    }
    compare_hardlink_partitions(&expected.entries, &actual.entries)
}

fn entries_match(expected: &EffectiveEntry, actual: &EffectiveEntry) -> Result<bool> {
    if expected.kind != actual.kind
        || expected.mode != actual.mode
        || expected.symlink_target != actual.symlink_target
        || expected.xattr_digest != actual.xattr_digest
    {
        return Ok(false);
    }
    match expected.kind {
        EntryKind::File => {
            if expected.size != actual.size
                || hash_file(&expected.source_path)? != hash_file(&actual.source_path)?
            {
                return Ok(false);
            }
        }
        EntryKind::Symlink | EntryKind::Directory => {}
        EntryKind::Other => return Ok(false),
    }
    Ok(true)
}

fn compare_hardlink_partitions(
    expected: &BTreeMap<PathBuf, EffectiveEntry>,
    actual: &BTreeMap<PathBuf, EffectiveEntry>,
) -> Result<()> {
    let expected_groups = hardlink_groups(expected);
    let actual_groups = hardlink_groups(actual);
    if expected_groups != actual_groups {
        return Err(validation_error(
            "compacted layer did not preserve hardlink grouping".to_string(),
        ));
    }
    Ok(())
}

fn hardlink_groups(entries: &BTreeMap<PathBuf, EffectiveEntry>) -> BTreeSet<Vec<PathBuf>> {
    let mut by_inode: BTreeMap<(u64, u64), Vec<PathBuf>> = BTreeMap::new();
    for (path, entry) in entries {
        if entry.kind == EntryKind::File && entry.nlink.unwrap_or(1) > 1 {
            by_inode
                .entry((entry.dev.unwrap_or(0), entry.ino.unwrap_or(0)))
                .or_default()
                .push(path.clone());
        }
    }
    by_inode
        .into_values()
        .filter(|paths| paths.len() > 1)
        .map(|mut paths| {
            paths.sort();
            paths
        })
        .collect()
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

fn set_xattr(path: &Path, name: &[u8], value: &[u8]) -> Result<()> {
    let name = std::ffi::OsStr::from_bytes(name);
    rustix::fs::lsetxattr(path, name, value, XattrFlags::empty()).map_err(|error| {
        CoreError::IsolationFailed(format!(
            "cannot set xattr {} on {}: {error}",
            name.to_string_lossy(),
            path.display()
        ))
    })
}

fn needs_compaction(layout: &SessionLayout, step_id: u64, layers: &[MountLayer]) -> Result<bool> {
    Ok(layers.len() >= MAX_MOUNT_LAYERS
        || lowerdir_bytes(layout, step_id, layers)? > MAX_LOWERDIR_BYTES)
}

fn fits_mount_limits(layout: &SessionLayout, step_id: u64, layers: &[MountLayer]) -> Result<bool> {
    Ok(layers.len() < MAX_MOUNT_LAYERS
        && lowerdir_bytes(layout, step_id, layers)? <= MAX_LOWERDIR_BYTES)
}

fn lowerdir_bytes(layout: &SessionLayout, step_id: u64, layers: &[MountLayer]) -> Result<usize> {
    let alias_dir = alias_dir(layout, step_id);
    let alias_dir = alias_dir.to_str().ok_or_else(|| {
        CoreError::Config(format!(
            "temporary alias directory is not valid UTF-8: {}",
            alias_dir.display()
        ))
    })?;
    let workspace = layout.workspace.to_str().ok_or_else(|| {
        CoreError::Config(format!(
            "workspace path is not valid UTF-8: {}",
            layout.workspace.display()
        ))
    })?;
    let mut total = workspace.len();
    for layer in layers.iter().rev() {
        total = total
            .saturating_add(1)
            .saturating_add(alias_dir.len())
            .saturating_add(1)
            .saturating_add(layer.alias.len());
    }
    Ok(total)
}

fn compaction_limit_error(
    layout: &SessionLayout,
    step_id: u64,
    layers: &[MountLayer],
) -> CoreError {
    CoreError::Config(format!(
        "cannot mount the active session: {} physical layers require {} lowerdir bytes (limits: {} layers and {} bytes). The layer-compaction cache is not usable for this branch.",
        layers.len(),
        lowerdir_bytes(layout, step_id, layers).unwrap_or(usize::MAX),
        MAX_MOUNT_LAYERS,
        MAX_LOWERDIR_BYTES
    ))
}

fn compacted_root(layout: &SessionLayout) -> PathBuf {
    layout.session_dir.join("layers").join("compacted")
}

fn temporary_root(layout: &SessionLayout, step_id: u64) -> PathBuf {
    layout.session_dir.join("layers").join("tmp").join(format!(
        "{}-{}-{}",
        step_id,
        std::process::id(),
        now_unix_ms()
    ))
}

fn temporary_permission_journal(temporary_root: &Path) -> PathBuf {
    let mut name = temporary_root
        .file_name()
        .map(std::ffi::OsString::from)
        .unwrap_or_else(|| std::ffi::OsString::from("compaction"));
    name.push(PERMISSION_JOURNAL_SUFFIX);
    temporary_root.with_file_name(name)
}

fn cleanup_stale_temporary_directories(layout: &SessionLayout) -> Result<()> {
    recover_pending_permissions(layout)?;
    let root = layout.session_dir.join("layers").join("tmp");
    let Ok(entries) = fs::read_dir(&root) else {
        return Ok(());
    };
    for entry in entries {
        let entry = entry?;
        let metadata = fs::symlink_metadata(entry.path())?;
        if metadata.is_dir() && !metadata.file_type().is_symlink() {
            remove_temporary_tree(&entry.path())?;
        } else if metadata.file_type().is_symlink() {
            fs::remove_file(entry.path())?;
        }
    }
    Ok(())
}

fn remove_temporary_tree(path: &Path) -> Result<()> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() {
        fs::remove_file(path)?;
        return Ok(());
    }
    if !metadata.is_dir() {
        fs::remove_file(path)?;
        return Ok(());
    }
    make_tree_writable(path)?;
    fs::remove_dir_all(path)?;
    Ok(())
}

fn make_tree_writable(path: &Path) -> Result<()> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() {
        return Ok(());
    }
    if metadata.is_dir() {
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
        for entry in fs::read_dir(path)? {
            make_tree_writable(&entry?.path())?;
        }
    } else {
        fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
}

fn compaction_key(source_layers: &[u64]) -> String {
    let mut hasher = Sha256::new();
    for layer in source_layers {
        hasher.update(layer.to_le_bytes());
    }
    let digest = hex::encode(hasher.finalize());
    format!(
        "{}-{}",
        source_layers.last().copied().unwrap_or_default(),
        &digest[..16]
    )
}

fn read_compaction_metadata(dir: &Path) -> Result<CompactionMetadata> {
    let path = dir.join("layer.json");
    let bytes = fs::read(&path)?;
    serde_json::from_slice(&bytes).map_err(|error| CoreError::StateCorrupted {
        step: 0,
        reason: format!(
            "invalid compacted layer metadata {}: {error}",
            path.display()
        ),
    })
}

fn write_metadata(root: &Path, metadata: &CompactionMetadata) -> Result<()> {
    let path = root.join("layer.json");
    let temp = root.join("layer.json.tmp");
    let mut file = OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .open(&temp)?;
    serde_json::to_writer_pretty(&mut file, metadata)?;
    file.write_all(b"\n")?;
    file.sync_all()?;
    fs::rename(temp, path)?;
    sync_directory(root);
    Ok(())
}

fn base_fingerprint(layout: &SessionLayout) -> Result<BaseFingerprint> {
    let metadata = fs::metadata(layout.base_manifest_path())?;
    Ok(BaseFingerprint {
        len: metadata.len(),
        mtime_ns: manifest::mtime_ns(&metadata),
    })
}

fn allocated_bytes(root: &Path) -> Result<u64> {
    let mut seen = HashSet::new();
    let mut total = 0u64;
    for entry in WalkDir::new(root).follow_links(false) {
        let entry = entry.map_err(|error| {
            CoreError::Io(std::io::Error::other(format!(
                "cannot measure compacted layer {}: {error}",
                root.display()
            )))
        })?;
        let metadata = fs::symlink_metadata(entry.path())?;
        if metadata.file_type().is_symlink() {
            continue;
        }
        if !seen.insert((metadata.dev(), metadata.ino())) {
            continue;
        }
        total = total.saturating_add(metadata.blocks().saturating_mul(512));
    }
    Ok(total)
}

fn sync_directory(path: &Path) {
    if let Ok(directory) = File::open(path) {
        let _ = directory.sync_all();
    }
}

fn now_unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| u64::try_from(duration.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or_default()
}

fn validation_error(message: String) -> CoreError {
    CoreError::IsolationFailed(format!("compacted layer validation failed: {message}"))
}

fn compaction_phase(phase: &str, error: CoreError) -> CoreError {
    CoreError::Io(std::io::Error::other(format!("{phase}: {error}")))
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

fn test_fail(point: &str, message: &str) -> Result<()> {
    if !cfg!(debug_assertions) {
        return Ok(());
    }
    if std::env::var_os("COWBOX_TEST_FAIL").as_deref() == Some(std::ffi::OsStr::new(point)) {
        return Err(CoreError::IsolationFailed(message.to_string()));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest;
    use rustix::fs::XattrFlags;
    use std::os::unix::fs::FileTypeExt;

    fn fixture() -> (tempfile::TempDir, SessionLayout) {
        let root = tempfile::Builder::new()
            .prefix("cowbox-compact-")
            .tempdir_in("/tmp")
            .expect("fixture");
        let workspace = root.path().join("workspace");
        let state = root.path().join("state");
        fs::create_dir_all(&workspace).expect("workspace");
        fs::create_dir_all(&state).expect("state");
        let layout = SessionLayout::new("compact-test", &workspace, &state);
        fs::create_dir_all(&layout.session_dir).expect("session");
        (root, layout)
    }

    fn write(path: &Path, contents: &str) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("parent");
        }
        fs::write(path, contents).expect("write");
    }

    fn set_user_xattr(path: &Path, name: &str, value: &[u8]) {
        rustix::fs::setxattr(path, name, value, XattrFlags::empty()).expect("set xattr");
    }

    fn user_xattr(path: &Path, name: &str) -> Vec<u8> {
        let size = rustix::fs::lgetxattr_query(path, name).expect("query xattr");
        let mut value = vec![0u8; size];
        let read = rustix::fs::lgetxattr(path, name, &mut value[..]).expect("read xattr");
        value.truncate(read);
        value
    }

    #[test]
    fn compaction_preserves_overlay_and_filesystem_semantics() {
        let (_root, layout) = fixture();
        write(&layout.workspace.join("modify.txt"), "old\n");
        write(&layout.workspace.join("deleted.txt"), "remove\n");
        write(&layout.workspace.join("delete-dir/child.txt"), "hidden\n");
        write(&layout.workspace.join("opaque/child.txt"), "hidden\n");
        write(&layout.workspace.join("xattr.txt"), "same\n");
        set_user_xattr(
            &layout.workspace.join("xattr.txt"),
            "user.cowbox.test",
            b"old",
        );
        write(&layout.workspace.join("hard-a.txt"), "hardlink\n");
        fs::hard_link(
            layout.workspace.join("hard-a.txt"),
            layout.workspace.join("hard-b.txt"),
        )
        .expect("base hardlink");
        std::os::unix::fs::symlink("old-target", layout.workspace.join("link"))
            .expect("base symlink");
        manifest::capture_base_manifest(&layout.workspace, &layout.base_manifest_path())
            .expect("base manifest");

        let first = layout.upper_dir(1);
        write(&first.join("modify.txt"), "new\n");
        write(&first.join("deleted.txt"), "");
        set_user_xattr(&first.join("deleted.txt"), "user.overlay.whiteout", b"y");
        write(&first.join("delete-dir"), "");
        set_user_xattr(&first.join("delete-dir"), "user.overlay.whiteout", b"y");
        fs::create_dir_all(first.join("opaque")).expect("opaque");
        set_user_xattr(&first.join("opaque"), "user.overlay.opaque", b"y");
        write(&first.join("opaque/new.txt"), "visible\n");
        write(&first.join("xattr.txt"), "same\n");
        set_user_xattr(&first.join("xattr.txt"), "user.cowbox.test", b"new");
        write(&first.join("hard-a.txt"), "hardlink\n");
        fs::hard_link(first.join("hard-a.txt"), first.join("hard-b.txt")).expect("upper hardlink");
        std::os::unix::fs::symlink("new-target", first.join("link")).expect("new symlink");

        let second = layout.upper_dir(2);
        write(&second.join("delete-dir/recreated.txt"), "recreated\n");
        write(&second.join("added.txt"), "added\n");

        let physical = vec![
            MountLayer {
                upper_dir: first,
                alias: "l1".to_string(),
                covered_logical: 1,
            },
            MountLayer {
                upper_dir: second,
                alias: "l2".to_string(),
                covered_logical: 1,
            },
        ];
        let compacted =
            build_compacted_layer(&layout, 3, &[1, 2], &physical).expect("build compacted layer");
        let state =
            analyze_layers(std::slice::from_ref(&compacted.upper_dir)).expect("read compacted");

        assert_eq!(
            fs::read_to_string(compacted.upper_dir.join("modify.txt")).expect("modified file"),
            "new\n"
        );
        assert_eq!(
            user_xattr(&compacted.upper_dir.join("xattr.txt"), "user.cowbox.test"),
            b"new"
        );
        assert_eq!(
            fs::read_link(compacted.upper_dir.join("link")).expect("symlink"),
            Path::new("new-target")
        );
        let whiteout =
            fs::symlink_metadata(compacted.upper_dir.join("deleted.txt")).expect("whiteout");
        assert!(whiteout.file_type().is_char_device());
        assert_eq!(whiteout.rdev(), 0);
        assert_eq!(
            user_xattr(&compacted.upper_dir.join("opaque"), "user.overlay.opaque"),
            b"y"
        );
        let hard_a = fs::symlink_metadata(compacted.upper_dir.join("hard-a.txt")).expect("hard a");
        let hard_b = fs::symlink_metadata(compacted.upper_dir.join("hard-b.txt")).expect("hard b");
        assert_eq!((hard_a.dev(), hard_a.ino()), (hard_b.dev(), hard_b.ino()));
        assert_eq!(
            fs::read_to_string(compacted.upper_dir.join("delete-dir/recreated.txt"))
                .expect("recreated"),
            "recreated\n"
        );
        assert!(state.deletions.contains(Path::new("deleted.txt")));
        assert!(state.opaques.contains(Path::new("delete-dir")));
        assert!(state.opaques.contains(Path::new("opaque")));
    }

    #[test]
    fn compaction_reuses_only_an_exact_ancestor_prefix() {
        let (_root, layout) = fixture();
        manifest::capture_base_manifest(&layout.workspace, &layout.base_manifest_path())
            .expect("base manifest");
        for step in 1..=3 {
            fs::create_dir_all(layout.upper_dir(step)).expect("upper");
        }
        let physical = (1..=2)
            .map(|step| MountLayer {
                upper_dir: layout.upper_dir(step),
                alias: format!("l{step}"),
                covered_logical: 1,
            })
            .collect::<Vec<_>>();
        let compacted =
            build_compacted_layer(&layout, 3, &[1, 2], &physical).expect("build compacted layer");

        let reused = resolve_physical_layers(&layout, &[1, 2, 3]).expect("resolve exact prefix");
        assert_eq!(reused.len(), 2);
        assert_eq!(reused[0].upper_dir, compacted.upper_dir);
        assert_eq!(reused[0].covered_logical, 2);

        let branch = resolve_physical_layers(&layout, &[1]).expect("resolve unrelated prefix");
        assert_eq!(branch.len(), 1);
        assert_eq!(branch[0].upper_dir, layout.upper_dir(1));
    }

    #[test]
    fn compaction_triggers_at_the_mount_limit_and_reports_usage() {
        let (_root, layout) = fixture();
        manifest::capture_base_manifest(&layout.workspace, &layout.base_manifest_path())
            .expect("base manifest");
        for step in 1..=MAX_MOUNT_LAYERS as u64 {
            fs::create_dir_all(layout.upper_dir(step)).expect("upper");
        }
        let logical = (1..=MAX_MOUNT_LAYERS as u64).collect::<Vec<_>>();
        let physical =
            prepare_mount_layers(&layout, MAX_MOUNT_LAYERS as u64 + 1, &logical).expect("compact");
        assert_eq!(physical.len(), KEEP_RECENT_LAYERS + 1);
        assert_eq!(
            physical[0].covered_logical,
            MAX_MOUNT_LAYERS - KEEP_RECENT_LAYERS
        );
        assert_eq!(physical.last().expect("recent").covered_logical, 1);

        let usage = inspect_layer_usage(&layout, &logical).expect("usage");
        assert_eq!(usage.logical_steps, MAX_MOUNT_LAYERS);
        assert_eq!(usage.mounted_layers, KEEP_RECENT_LAYERS + 2);
        assert_eq!(usage.compact_layers, 1);
    }

    #[test]
    fn compaction_handles_a_directory_replaced_by_a_file() {
        let (_root, layout) = fixture();
        manifest::capture_base_manifest(&layout.workspace, &layout.base_manifest_path())
            .expect("base manifest");
        let first = layout.upper_dir(1);
        write(&first.join("replaced/child.txt"), "child\n");
        let second = layout.upper_dir(2);
        write(&second.join("replaced"), "now a file\n");
        let physical = vec![
            MountLayer {
                upper_dir: first,
                alias: "l1".to_string(),
                covered_logical: 1,
            },
            MountLayer {
                upper_dir: second,
                alias: "l2".to_string(),
                covered_logical: 1,
            },
        ];

        let compacted =
            build_compacted_layer(&layout, 3, &[1, 2], &physical).expect("compacted layer");
        assert_eq!(
            fs::read_to_string(compacted.upper_dir.join("replaced")).expect("replacement file"),
            "now a file\n"
        );
        assert!(!compacted.upper_dir.join("replaced/child.txt").exists());
    }

    #[test]
    fn compaction_preserves_xattrs_from_read_only_entries() {
        let (_root, layout) = fixture();
        manifest::capture_base_manifest(&layout.workspace, &layout.base_manifest_path())
            .expect("base manifest");
        let first = layout.upper_dir(1);
        write(&first.join("read-only.txt"), "same contents\n");
        set_user_xattr(&first.join("read-only.txt"), "user.cowbox.test", b"file");
        write(&first.join("read-only-dir/child.txt"), "child\n");
        set_user_xattr(
            &first.join("read-only-dir"),
            "user.cowbox.dir",
            b"directory",
        );
        fs::set_permissions(
            first.join("read-only.txt"),
            fs::Permissions::from_mode(0o444),
        )
        .expect("read-only file");
        fs::set_permissions(
            first.join("read-only-dir"),
            fs::Permissions::from_mode(0o555),
        )
        .expect("read-only directory");
        let physical = vec![MountLayer {
            upper_dir: first,
            alias: "l1".to_string(),
            covered_logical: 1,
        }];

        let compacted =
            build_compacted_layer(&layout, 2, &[1], &physical).expect("compacted layer");
        assert_eq!(
            user_xattr(
                &compacted.upper_dir.join("read-only.txt"),
                "user.cowbox.test"
            ),
            b"file"
        );
        assert_eq!(
            user_xattr(
                &compacted.upper_dir.join("read-only-dir"),
                "user.cowbox.dir"
            ),
            b"directory"
        );
        assert_eq!(
            fs::metadata(compacted.upper_dir.join("read-only.txt"))
                .expect("file metadata")
                .mode()
                & 0o777,
            0o444
        );
        assert_eq!(
            fs::metadata(compacted.upper_dir.join("read-only-dir"))
                .expect("directory metadata")
                .mode()
                & 0o777,
            0o555
        );
    }

    #[test]
    fn cleanup_removes_read_only_temporary_trees() {
        let (_root, layout) = fixture();
        let stale = layout.session_dir.join("layers/tmp/stale/upper");
        write(&stale.join("read-only-dir/file.txt"), "stale\n");
        fs::set_permissions(
            stale.join("read-only-dir/file.txt"),
            fs::Permissions::from_mode(0o444),
        )
        .expect("read-only file");
        fs::set_permissions(
            stale.join("read-only-dir"),
            fs::Permissions::from_mode(0o555),
        )
        .expect("read-only directory");

        cleanup_stale_temporary_directories(&layout).expect("cleanup");
        assert!(!layout.session_dir.join("layers/tmp/stale").exists());
    }

    #[test]
    fn compaction_handles_a_large_upper_layer() {
        let (_root, layout) = fixture();
        manifest::capture_base_manifest(&layout.workspace, &layout.base_manifest_path())
            .expect("base manifest");
        let first = layout.upper_dir(1);
        for directory in 0..100 {
            for file in 0..100 {
                write(
                    &first.join(format!("tree/d{directory:03}/f{file:03}.txt")),
                    "payload\n",
                );
            }
        }
        for index in 0..100 {
            let path = first.join(format!("deleted-{index:03}"));
            write(&path, "");
            set_user_xattr(&path, "user.overlay.whiteout", b"y");
        }
        let physical = vec![MountLayer {
            upper_dir: first,
            alias: "l1".to_string(),
            covered_logical: 1,
        }];

        let compacted =
            build_compacted_layer(&layout, 2, &[1], &physical).expect("compacted layer");
        assert!(compacted.upper_dir.join("tree/d099/f099.txt").is_file());
        assert!(
            fs::symlink_metadata(compacted.upper_dir.join("deleted-099"))
                .expect("whiteout")
                .file_type()
                .is_char_device()
        );
    }

    #[test]
    fn compaction_preserves_unreadable_files() {
        let (_root, layout) = fixture();
        manifest::capture_base_manifest(&layout.workspace, &layout.base_manifest_path())
            .expect("base manifest");
        let first = layout.upper_dir(1);
        write(&first.join("locked.txt"), "locked contents\n");
        set_user_xattr(&first.join("locked.txt"), "user.cowbox.locked", b"kept");
        fs::set_permissions(first.join("locked.txt"), fs::Permissions::from_mode(0o000))
            .expect("locked file");
        let physical = vec![MountLayer {
            upper_dir: first,
            alias: "l1".to_string(),
            covered_logical: 1,
        }];

        let compacted =
            build_compacted_layer(&layout, 2, &[1], &physical).expect("compacted layer");
        assert_eq!(
            fs::metadata(physical[0].upper_dir.join("locked.txt"))
                .expect("source metadata")
                .mode()
                & 0o777,
            0
        );
        assert!(
            !layout
                .session_dir
                .join("layers/tmp")
                .read_dir()
                .map(|entries| entries
                    .filter_map(|entry| entry.ok())
                    .any(|entry| is_permission_journal_name(&entry.path())))
                .unwrap_or(false)
        );
        let target = compacted.upper_dir.join("locked.txt");
        assert_eq!(
            fs::metadata(&target).expect("locked metadata").mode() & 0o777,
            0
        );
        fs::set_permissions(&target, fs::Permissions::from_mode(0o400))
            .expect("temporary read permission");
        assert_eq!(
            fs::read_to_string(&target).expect("locked contents"),
            "locked contents\n"
        );
        assert_eq!(user_xattr(&target, "user.cowbox.locked"), b"kept");
        fs::set_permissions(&target, fs::Permissions::from_mode(0o000))
            .expect("restore locked mode");
    }

    #[test]
    fn compaction_preserves_unreadable_directories_and_nested_entries() {
        let (_root, layout) = fixture();
        manifest::capture_base_manifest(&layout.workspace, &layout.base_manifest_path())
            .expect("base manifest");
        let first = layout.upper_dir(1);
        write(&first.join("locked/child.txt"), "visible child\n");
        write(&first.join("nested/inner/file.txt"), "nested contents\n");
        set_user_xattr(
            &first.join("nested/inner/file.txt"),
            "user.cowbox.locked",
            b"nested",
        );
        fs::set_permissions(
            first.join("nested/inner/file.txt"),
            fs::Permissions::from_mode(0o000),
        )
        .expect("locked nested file");
        fs::set_permissions(
            first.join("nested/inner"),
            fs::Permissions::from_mode(0o000),
        )
        .expect("locked inner");
        fs::set_permissions(first.join("nested"), fs::Permissions::from_mode(0o000))
            .expect("locked nested");
        fs::set_permissions(first.join("locked"), fs::Permissions::from_mode(0o000))
            .expect("locked directory");
        let physical = vec![MountLayer {
            upper_dir: first.clone(),
            alias: "l1".to_string(),
            covered_logical: 1,
        }];

        let compacted =
            build_compacted_layer(&layout, 2, &[1], &physical).expect("compacted layer");
        assert_eq!(
            fs::metadata(first.join("locked"))
                .expect("source locked")
                .mode()
                & 0o777,
            0
        );
        assert_eq!(
            fs::metadata(first.join("nested"))
                .expect("source nested")
                .mode()
                & 0o777,
            0
        );
        assert_eq!(
            fs::metadata(compacted.upper_dir.join("locked"))
                .expect("compacted locked")
                .mode()
                & 0o777,
            0
        );
        assert_eq!(
            fs::metadata(compacted.upper_dir.join("nested"))
                .expect("compacted nested")
                .mode()
                & 0o777,
            0
        );
        assert!(
            !layout
                .session_dir
                .join("layers/tmp")
                .read_dir()
                .map(|entries| entries
                    .filter_map(|entry| entry.ok())
                    .any(|entry| is_permission_journal_name(&entry.path())))
                .unwrap_or(false)
        );

        fs::set_permissions(
            compacted.upper_dir.join("nested"),
            fs::Permissions::from_mode(0o500),
        )
        .expect("open compacted nested");
        fs::set_permissions(
            compacted.upper_dir.join("nested/inner"),
            fs::Permissions::from_mode(0o500),
        )
        .expect("open compacted inner");
        assert_eq!(
            fs::metadata(compacted.upper_dir.join("nested/inner/file.txt"))
                .expect("nested file")
                .mode()
                & 0o777,
            0
        );
        fs::set_permissions(
            compacted.upper_dir.join("nested/inner/file.txt"),
            fs::Permissions::from_mode(0o400),
        )
        .expect("read compacted file");
        assert_eq!(
            fs::read_to_string(compacted.upper_dir.join("nested/inner/file.txt"))
                .expect("nested contents"),
            "nested contents\n"
        );
        assert_eq!(
            user_xattr(
                &compacted.upper_dir.join("nested/inner/file.txt"),
                "user.cowbox.locked"
            ),
            b"nested"
        );
        fs::set_permissions(
            compacted.upper_dir.join("nested/inner/file.txt"),
            fs::Permissions::from_mode(0o000),
        )
        .expect("restore compacted file");
        fs::set_permissions(
            compacted.upper_dir.join("nested/inner"),
            fs::Permissions::from_mode(0o000),
        )
        .expect("restore compacted inner");
        fs::set_permissions(
            compacted.upper_dir.join("nested"),
            fs::Permissions::from_mode(0o000),
        )
        .expect("restore compacted nested");
    }

    #[test]
    fn permission_journal_restores_nested_tree_after_partial_restore() {
        let (_root, layout) = fixture();
        let first = layout.upper_dir(1);
        write(&first.join("locked/inner/file.txt"), "locked\n");
        fs::set_permissions(
            first.join("locked/inner/file.txt"),
            fs::Permissions::from_mode(0o000),
        )
        .expect("locked file");
        fs::set_permissions(
            first.join("locked/inner"),
            fs::Permissions::from_mode(0o000),
        )
        .expect("locked inner");
        fs::set_permissions(first.join("locked"), fs::Permissions::from_mode(0o000))
            .expect("locked parent");
        let journal = access_journal_path(&layout, "read-test");

        let access = prepare_access(&layout, std::slice::from_ref(&first), Some(&journal))
            .expect("prepare access");
        assert!(journal.exists());
        assert_eq!(
            fs::metadata(first.join("locked"))
                .expect("elevated parent")
                .mode()
                & 0o777,
            0o500
        );
        std::mem::forget(access);

        fs::set_permissions(first.join("locked"), fs::Permissions::from_mode(0o000))
            .expect("simulate interrupted restore");
        recover_pending_permissions(&layout).expect("recover journal");
        assert_eq!(
            fs::metadata(first.join("locked"))
                .expect("restored parent")
                .mode()
                & 0o777,
            0
        );
        assert!(!journal.exists());

        fs::set_permissions(first.join("locked"), fs::Permissions::from_mode(0o500))
            .expect("inspect restored tree");
        assert_eq!(
            fs::metadata(first.join("locked/inner"))
                .expect("restored inner")
                .mode()
                & 0o777,
            0
        );
        fs::set_permissions(
            first.join("locked/inner"),
            fs::Permissions::from_mode(0o500),
        )
        .expect("inspect restored file");
        assert_eq!(
            fs::metadata(first.join("locked/inner/file.txt"))
                .expect("restored file")
                .mode()
                & 0o777,
            0
        );
        fs::set_permissions(
            first.join("locked/inner"),
            fs::Permissions::from_mode(0o000),
        )
        .expect("restore inner mode");
        fs::set_permissions(first.join("locked"), fs::Permissions::from_mode(0o000))
            .expect("restore parent mode");
    }

    #[test]
    fn legacy_permission_journal_restores_complete_prefix_and_ignores_partial_tail() {
        let (_root, layout) = fixture();
        let first = layout.upper_dir(1);
        write(&first.join("locked/inner/file.txt"), "locked\n");
        fs::set_permissions(
            first.join("locked/inner/file.txt"),
            fs::Permissions::from_mode(0o000),
        )
        .expect("locked file");
        fs::set_permissions(
            first.join("locked/inner"),
            fs::Permissions::from_mode(0o500),
        )
        .expect("elevated inner");
        fs::set_permissions(first.join("locked"), fs::Permissions::from_mode(0o500))
            .expect("elevated parent");
        let temp = layout.session_dir.join("layers/tmp/interrupted");
        fs::create_dir_all(&temp).expect("legacy temp");
        let journal = temp.join(PERMISSION_JOURNAL);
        let parent = permission_record(&layout, &first.join("locked"), 0).expect("parent record");
        let inner =
            permission_record(&layout, &first.join("locked/inner"), 0).expect("inner record");
        let mut payload = String::new();
        payload.push_str(&serde_json::to_string(&parent).expect("parent json"));
        payload.push('\n');
        payload.push_str(&serde_json::to_string(&inner).expect("inner json"));
        payload.push('\n');
        payload.push_str(r#"{"path":"steps/1/upper/locked/inner/file.txt","mode":0"#);
        fs::write(&journal, payload).expect("legacy journal");

        recover_pending_permissions(&layout).expect("recover journal");
        assert_eq!(
            fs::metadata(first.join("locked"))
                .expect("restored parent")
                .mode()
                & 0o777,
            0
        );
        assert!(!journal.exists());

        fs::set_permissions(first.join("locked"), fs::Permissions::from_mode(0o500))
            .expect("inspect restored tree");
        assert_eq!(
            fs::metadata(first.join("locked/inner"))
                .expect("restored inner")
                .mode()
                & 0o777,
            0
        );
        fs::set_permissions(first.join("locked"), fs::Permissions::from_mode(0o000))
            .expect("restore parent mode");
    }

    #[test]
    fn permission_journal_rejects_malformed_complete_lines() {
        let (_root, layout) = fixture();
        let first = layout.upper_dir(1);
        write(&first.join("locked"), "locked\n");
        fs::set_permissions(first.join("locked"), fs::Permissions::from_mode(0o500))
            .expect("elevated");
        let journal = layout
            .session_dir
            .join("layers/tmp/broken.permissions.jsonl");
        fs::create_dir_all(journal.parent().expect("journal parent")).expect("journal parent");
        fs::write(
            &journal,
            "{\"path\":\"steps/1/upper/locked\",\"mode\":0}\nnot-json\n",
        )
        .expect("broken journal");

        let error = recover_pending_permissions(&layout).expect_err("malformed journal");
        assert!(matches!(error, CoreError::StateCorrupted { .. }));
        assert!(journal.exists());
        assert_eq!(
            fs::metadata(first.join("locked"))
                .expect("mode unchanged")
                .mode()
                & 0o777,
            0o500
        );
    }
}
