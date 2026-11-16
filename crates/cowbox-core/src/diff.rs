use crate::compact;
use crate::error::{CoreError, Result};
use crate::layout::SessionLayout;
use crate::manifest::{self, ManifestEntry};
use crate::model::{ChangeKind, DiffOptions, EntryKind, FileChange};
use crate::whiteout::{self, UpperEntryKind};
use sha2::{Digest, Sha256};
use similar::{ChangeTag, TextDiff};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs;
use std::io::Read;
use std::os::unix::fs::MetadataExt;
use std::path::{Component, Path, PathBuf};
use walkdir::WalkDir;

#[derive(Debug, Clone)]
struct LayerEntry {
    kind: EntryKind,
    mode: u32,
    size: u64,
    symlink_target: Option<PathBuf>,
    source_path: PathBuf,
    xattr_digest: Option<String>,
}

impl LayerEntry {
    fn from_metadata(
        path: &Path,
        metadata: &fs::Metadata,
        mode_overrides: &HashMap<PathBuf, u32>,
    ) -> Result<Self> {
        let kind = manifest::entry_kind(metadata);
        let symlink_target = if kind == EntryKind::Symlink {
            Some(fs::read_link(path)?)
        } else {
            None
        };
        let xattr_digest = if matches!(kind, EntryKind::File | EntryKind::Directory) {
            Some(crate::xattr::digest_checked(path)?)
        } else {
            None
        };
        Ok(Self {
            kind,
            mode: mode_overrides
                .get(path)
                .copied()
                .unwrap_or(metadata.mode() & 0o7777),
            size: metadata.len(),
            symlink_target,
            source_path: path.to_path_buf(),
            xattr_digest,
        })
    }
}

pub fn compute_diff(
    layout: &SessionLayout,
    base_layers: &[u64],
    target_layers: &[u64],
    options: &DiffOptions,
) -> Result<Vec<FileChange>> {
    if !base_layers.is_empty() {
        return Err(CoreError::Config(
            "M1 only supports diffing from the session base (step 0)".to_string(),
        ));
    }
    let roots = target_layers
        .iter()
        .map(|step| layout.upper_dir(*step))
        .filter(|upper| upper.is_dir())
        .collect::<Vec<_>>();
    let journal = compact::access_journal_path(layout, "diff");
    let mut access = compact::prepare_access(layout, &roots, Some(&journal))?;
    let result = compute_diff_with_modes(
        layout,
        base_layers,
        target_layers,
        options,
        access.overrides(),
    );
    let restore = access.restore();
    match (result, restore) {
        (Ok(changes), Ok(())) => Ok(changes),
        (Ok(_), Err(error)) => Err(CoreError::IsolationFailed(format!(
            "diff completed but source permissions could not be restored: {error}"
        ))),
        (Err(error), _) => Err(error),
    }
}

pub(crate) fn compute_diff_with_modes(
    layout: &SessionLayout,
    base_layers: &[u64],
    target_layers: &[u64],
    options: &DiffOptions,
    mode_overrides: &HashMap<PathBuf, u32>,
) -> Result<Vec<FileChange>> {
    let _span = tracing::info_span!(
        "diff_layers",
        layers = target_layers.len(),
        workspace = %layout.workspace.display()
    )
    .entered();
    if !base_layers.is_empty() {
        return Err(CoreError::Config(
            "M1 only supports diffing from the session base (step 0)".to_string(),
        ));
    }
    let mut upserts: BTreeMap<PathBuf, LayerEntry> = BTreeMap::new();
    let mut deleted: BTreeSet<PathBuf> = BTreeSet::new();
    let mut opaque_dirs: BTreeSet<PathBuf> = BTreeSet::new();

    for step in target_layers {
        let upper = layout.upper_dir(*step);
        if !upper.is_dir() {
            return Err(CoreError::StateCorrupted {
                step: *step,
                reason: format!("missing upper layer {}", upper.display()),
            });
        }
        for entry in WalkDir::new(&upper)
            .follow_links(false)
            .min_depth(1)
            .into_iter()
        {
            let entry = entry.map_err(|error| CoreError::StateCorrupted {
                step: *step,
                reason: format!("cannot scan upper layer {}: {error}", upper.display()),
            })?;
            let rel_path = entry
                .path()
                .strip_prefix(&upper)
                .map_err(|_| CoreError::StateCorrupted {
                    step: *step,
                    reason: "overlay entry escaped its upper layer".to_string(),
                })?
                .to_path_buf();
            if rel_path.as_os_str().is_empty() {
                continue;
            }
            let metadata = fs::symlink_metadata(entry.path())?;
            match whiteout::classify_upper_entry(entry.path(), &metadata) {
                UpperEntryKind::Whiteout(_) => {
                    upserts.retain(|candidate, _| !candidate.starts_with(&rel_path));
                    deleted.insert(rel_path);
                }
                UpperEntryKind::OpaqueDirectory => {
                    upserts.retain(|candidate, _| !candidate.starts_with(&rel_path));
                    opaque_dirs.insert(rel_path.clone());
                    upserts.insert(
                        rel_path.clone(),
                        LayerEntry::from_metadata(entry.path(), &metadata, mode_overrides)?,
                    );
                }
                UpperEntryKind::Entry => {
                    upserts.insert(
                        rel_path,
                        LayerEntry::from_metadata(entry.path(), &metadata, mode_overrides)?,
                    );
                }
            }
        }
    }

    let has_removals = !deleted.is_empty() || !opaque_dirs.is_empty();
    let base = manifest::load_manifest_filtered(&layout.base_manifest_path(), |path| {
        upserts.contains_key(path)
            || has_path_or_ancestor_in(path, &deleted)
            || has_path_or_ancestor_in(path, &opaque_dirs)
    })?;

    let deleted_dirs: BTreeSet<PathBuf> = base
        .iter()
        .filter(|(path, entry)| entry.kind == EntryKind::Directory && deleted.contains(*path))
        .map(|(path, _)| path.clone())
        .collect();

    let mut changes = Vec::new();
    for (rel_path, entry) in &upserts {
        let excluded = is_excluded(rel_path, &options.excluded);
        if excluded && !options.include_excluded {
            continue;
        }
        let base_entry = base.get(rel_path);
        let (kind, size_delta) = match base_entry {
            Some(base_entry) => {
                if base_entry.kind != entry.kind {
                    (
                        ChangeKind::TypeChanged {
                            from: base_entry.kind,
                            to: entry.kind,
                        },
                        i64::try_from(entry.size).unwrap_or(i64::MAX)
                            - i64::try_from(base_entry.size).unwrap_or(i64::MAX),
                    )
                } else {
                    match entry.kind {
                        EntryKind::File => {
                            let host_path = layout.workspace.join(rel_path);
                            if files_equal(&host_path, &entry.source_path)
                                && entry.mode == base_entry.mode
                                && xattrs_match(entry, base_entry)
                            {
                                continue;
                            }
                            (
                                ChangeKind::Modified,
                                i64::try_from(entry.size).unwrap_or(i64::MAX)
                                    - i64::try_from(base_entry.size).unwrap_or(i64::MAX),
                            )
                        }
                        EntryKind::Symlink => {
                            if entry.symlink_target == base_entry.symlink_target {
                                continue;
                            }
                            (
                                ChangeKind::SymlinkUpdated {
                                    target: entry.symlink_target.clone().unwrap_or_default(),
                                },
                                0,
                            )
                        }
                        EntryKind::Directory => {
                            if xattrs_match(entry, base_entry) {
                                continue;
                            }
                            (ChangeKind::Modified, 0)
                        }
                        EntryKind::Other => (
                            ChangeKind::Modified,
                            i64::try_from(entry.size).unwrap_or(i64::MAX)
                                - i64::try_from(base_entry.size).unwrap_or(i64::MAX),
                        ),
                    }
                }
            }
            None => (
                ChangeKind::Added,
                i64::try_from(entry.size).unwrap_or(i64::MAX),
            ),
        };
        changes.push(FileChange {
            rel_path: rel_path.clone(),
            kind,
            entry_kind: entry.kind,
            size_delta,
            lines_added: None,
            lines_removed: None,
            mode: Some(entry.mode),
            excluded,
        });
    }

    if has_removals {
        for (rel_path, base_entry) in &base {
            if upserts.contains_key(rel_path) {
                continue;
            }
            let removed = deleted.contains(rel_path)
                || deleted_dirs.iter().any(|dir| rel_path.starts_with(dir))
                || has_ancestor_in(rel_path, &opaque_dirs);
            if !removed {
                continue;
            }
            let excluded = is_excluded(rel_path, &options.excluded);
            if excluded && !options.include_excluded {
                continue;
            }
            changes.push(FileChange {
                rel_path: rel_path.clone(),
                kind: ChangeKind::Deleted,
                entry_kind: base_entry.kind,
                size_delta: -(i64::try_from(base_entry.size).unwrap_or(i64::MAX)),
                lines_added: None,
                lines_removed: None,
                mode: Some(base_entry.mode),
                excluded,
            });
        }
    }

    changes.sort_by(|left, right| left.rel_path.cmp(&right.rel_path));
    if options.line_stats {
        add_line_stats(layout, target_layers, &base, &mut changes, options);
    }
    Ok(changes)
}

fn add_line_stats(
    layout: &SessionLayout,
    target_layers: &[u64],
    base: &BTreeMap<PathBuf, ManifestEntry>,
    changes: &mut [FileChange],
    options: &DiffOptions,
) {
    let mut budget = options.max_line_stat_files;
    for change in changes {
        if budget == 0 {
            break;
        }
        if change.entry_kind != EntryKind::File {
            continue;
        }
        let new_path = if matches!(
            change.kind,
            ChangeKind::Added | ChangeKind::Modified | ChangeKind::TypeChanged { .. }
        ) {
            manifest::top_layer_for(layout, target_layers, &change.rel_path).map(|(_, path)| path)
        } else {
            None
        };
        let base_path = if matches!(
            change.kind,
            ChangeKind::Modified | ChangeKind::Deleted | ChangeKind::TypeChanged { .. }
        ) {
            base.get(&change.rel_path)
                .map(|_| layout.workspace.join(&change.rel_path))
        } else {
            None
        };
        if let Some((added, removed)) = line_stats(
            base_path.as_deref(),
            new_path.as_deref(),
            options.max_line_stat_bytes,
        ) {
            change.lines_added = Some(added);
            change.lines_removed = Some(removed);
            budget -= 1;
        }
    }
}

fn line_stats(base: Option<&Path>, new: Option<&Path>, max_bytes: u64) -> Option<(u32, u32)> {
    let old_text = match base {
        Some(path) => read_text(path, max_bytes)?,
        None => String::new(),
    };
    let new_text = match new {
        Some(path) => read_text(path, max_bytes)?,
        None => String::new(),
    };
    let diff = TextDiff::from_lines(&old_text, &new_text);
    let mut added = 0u32;
    let mut removed = 0u32;
    for change in diff.iter_all_changes() {
        match change.tag() {
            ChangeTag::Insert => added = added.saturating_add(1),
            ChangeTag::Delete => removed = removed.saturating_add(1),
            ChangeTag::Equal => {}
        }
    }
    Some((added, removed))
}

fn read_text(path: &Path, max_bytes: u64) -> Option<String> {
    let metadata = fs::metadata(path).ok()?;
    if metadata.len() > max_bytes {
        return None;
    }
    let mut bytes = Vec::new();
    fs::File::open(path).ok()?.read_to_end(&mut bytes).ok()?;
    if bytes.contains(&0) {
        return None;
    }
    String::from_utf8(bytes).ok()
}

fn files_equal(left: &Path, right: &Path) -> bool {
    let Ok(left_meta) = fs::metadata(left) else {
        return false;
    };
    let Ok(right_meta) = fs::metadata(right) else {
        return false;
    };
    if left_meta.len() != right_meta.len() {
        return false;
    }
    let Ok(left_file) = fs::File::open(left) else {
        return false;
    };
    let Ok(right_file) = fs::File::open(right) else {
        return false;
    };
    hash_reader(left_file) == hash_reader(right_file)
}

fn xattrs_match(entry: &LayerEntry, base_entry: &ManifestEntry) -> bool {
    match (&entry.xattr_digest, &base_entry.xattr_digest) {
        (Some(current), Some(base)) => current == base,
        _ => true,
    }
}

fn hash_reader(mut reader: fs::File) -> String {
    let mut hasher = Sha256::new();
    let mut buffer = [0u8; 64 * 1024];
    while let Ok(read) = reader.read(&mut buffer) {
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    hex::encode(hasher.finalize())
}

fn has_ancestor_in(path: &Path, ancestors: &BTreeSet<PathBuf>) -> bool {
    path.ancestors()
        .skip(1)
        .any(|ancestor| ancestors.contains(ancestor))
}

fn has_path_or_ancestor_in(path: &Path, candidates: &BTreeSet<PathBuf>) -> bool {
    !candidates.is_empty()
        && path
            .ancestors()
            .any(|candidate| candidates.contains(candidate))
}

fn is_excluded(path: &Path, excluded: &[String]) -> bool {
    if path
        .components()
        .any(|component| matches!(component, Component::Normal(name) if name == ".git"))
    {
        return true;
    }
    excluded.iter().any(|entry| {
        let entry_path = Path::new(entry);
        if entry_path.as_os_str().is_empty() {
            return false;
        }
        path.starts_with(entry_path)
            || path
                .components()
                .any(|component| component.as_os_str() == entry_path.as_os_str())
    })
}
