use crate::error::{CoreError, Result};
use crate::model::EntryKind;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::fs::{self, File};
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc;
use std::thread;

const MAX_CAPTURE_THREADS: usize = 8;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ManifestEntry {
    pub rel_path: PathBuf,
    pub kind: EntryKind,
    pub mode: u32,
    pub size: u64,
    pub mtime_ns: i128,
    pub symlink_target: Option<PathBuf>,
    #[serde(default)]
    pub dev: Option<u64>,
    #[serde(default)]
    pub ino: Option<u64>,
    #[serde(default)]
    pub nlink: Option<u64>,
    #[serde(default)]
    pub xattr_digest: Option<String>,
}

impl ManifestEntry {
    pub fn from_metadata(rel_path: PathBuf, path: &Path, metadata: &fs::Metadata) -> Result<Self> {
        let kind = entry_kind(metadata);
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
            rel_path,
            kind,
            mode: metadata.mode() & 0o7777,
            size: metadata.len(),
            mtime_ns: mtime_ns(metadata),
            symlink_target,
            dev: matches!(kind, EntryKind::File).then(|| metadata.dev()),
            ino: matches!(kind, EntryKind::File).then(|| metadata.ino()),
            nlink: matches!(kind, EntryKind::File).then(|| metadata.nlink()),
            xattr_digest,
        })
    }
}

pub fn entry_kind(metadata: &fs::Metadata) -> EntryKind {
    let file_type = metadata.file_type();
    if file_type.is_file() {
        EntryKind::File
    } else if file_type.is_dir() {
        EntryKind::Directory
    } else if file_type.is_symlink() {
        EntryKind::Symlink
    } else {
        EntryKind::Other
    }
}

pub fn mtime_ns(metadata: &fs::Metadata) -> i128 {
    i128::from(metadata.mtime()) * 1_000_000_000 + i128::from(metadata.mtime_nsec())
}

pub fn capture_base_manifest(root: &Path, output: &Path) -> Result<Vec<ManifestEntry>> {
    if let Some(parent) = output.parent() {
        fs::create_dir_all(parent)?;
    }
    let file = File::create(output)?;
    capture_manifest(root, &file, true)?.ok_or_else(|| CoreError::StateCorrupted {
        step: 0,
        reason: "manifest capture omitted collected entries".to_string(),
    })
}

pub(crate) fn capture_base_manifest_file(root: &Path, output: &Path) -> Result<()> {
    // Session initialization needs the persisted bytes but not the parsed
    // entries, so workers can release each entry immediately after
    // serialization instead of handing it to the main thread.
    if let Some(parent) = output.parent() {
        fs::create_dir_all(parent)?;
    }
    let file = File::create(output)?;
    capture_manifest(root, &file, false).map(|_| ())
}

pub fn load_manifest(path: &Path) -> Result<BTreeMap<PathBuf, ManifestEntry>> {
    load_manifest_where(path, |_| true)
}

pub(crate) fn load_manifest_filtered<F>(
    path: &Path,
    include: F,
) -> Result<BTreeMap<PathBuf, ManifestEntry>>
where
    F: FnMut(&Path) -> bool,
{
    load_manifest_where(path, include)
}

fn load_manifest_where<F>(path: &Path, mut include: F) -> Result<BTreeMap<PathBuf, ManifestEntry>>
where
    F: FnMut(&Path) -> bool,
{
    let file = File::open(path).map_err(|error| CoreError::StateCorrupted {
        step: 0,
        reason: format!("cannot open base manifest {}: {error}", path.display()),
    })?;
    let mut reader = BufReader::new(file);
    let mut entries = BTreeMap::new();
    let mut line = String::new();
    loop {
        line.clear();
        if reader.read_line(&mut line)? == 0 {
            break;
        }
        if line.trim().is_empty() {
            continue;
        }
        let entry: ManifestEntry =
            serde_json::from_str(&line).map_err(|error| CoreError::StateCorrupted {
                step: 0,
                reason: format!("invalid base manifest line: {error}"),
            })?;
        if include(&entry.rel_path) {
            entries.insert(entry.rel_path.clone(), entry);
        }
    }
    Ok(entries)
}

pub fn write_manifest(path: &Path, entries: &BTreeMap<PathBuf, ManifestEntry>) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let file = File::create(path)?;
    let mut writer = BufWriter::new(file);
    for entry in entries.values() {
        serde_json::to_writer(&mut writer, entry)?;
        writer.write_all(b"\n")?;
    }
    writer.flush()?;
    writer.get_ref().sync_all()?;
    Ok(())
}

pub fn metadata_matches(entry: &ManifestEntry, path: &Path) -> bool {
    let Ok(metadata) = fs::symlink_metadata(path) else {
        return false;
    };
    let kind = entry_kind(&metadata);
    if kind != entry.kind || (metadata.mode() & 0o7777) != entry.mode {
        return false;
    }
    if let (Some(dev), Some(ino)) = (entry.dev, entry.ino) {
        if metadata.dev() != dev || metadata.ino() != ino {
            return false;
        }
    }
    if let Some(nlink) = entry.nlink {
        if metadata.nlink() != nlink {
            return false;
        }
    }
    if let Some(expected) = &entry.xattr_digest {
        let Some(actual) = crate::xattr::digest_lossy(path) else {
            return false;
        };
        if &actual != expected {
            return false;
        }
    }
    match kind {
        EntryKind::File => metadata.len() == entry.size && mtime_ns(&metadata) == entry.mtime_ns,
        EntryKind::Symlink => fs::read_link(path)
            .map(|target| Some(target) == entry.symlink_target)
            .unwrap_or(false),
        EntryKind::Directory => true,
        EntryKind::Other => false,
    }
}

pub fn top_layer_for(
    layout: &crate::layout::SessionLayout,
    layers: &[u64],
    rel_path: &Path,
) -> Option<(u64, PathBuf)> {
    for step in layers.iter().rev() {
        let candidate = layout.upper_dir(*step).join(rel_path);
        let Ok(metadata) = fs::symlink_metadata(&candidate) else {
            continue;
        };
        if crate::whiteout::is_whiteout_metadata(&metadata).is_some() {
            continue;
        }
        if crate::whiteout::is_opaque_directory(&candidate, &metadata) {
            continue;
        }
        return Some((*step, candidate));
    }
    None
}

pub fn visible_path(
    layout: &crate::layout::SessionLayout,
    layers: &[u64],
    rel_path: &Path,
    base: &BTreeMap<PathBuf, ManifestEntry>,
) -> Option<PathBuf> {
    if let Some(path) = top_layer_visible_path(layout, layers, rel_path) {
        return Some(path);
    }
    let host_path = layout.workspace.join(rel_path);
    if base.contains_key(rel_path) && fs::symlink_metadata(&host_path).is_ok() {
        Some(host_path)
    } else {
        None
    }
}

pub fn top_layer_visible_path(
    layout: &crate::layout::SessionLayout,
    layers: &[u64],
    rel_path: &Path,
) -> Option<PathBuf> {
    for step in layers.iter().rev() {
        let upper = layout.upper_dir(*step);
        let candidate = upper.join(rel_path);
        if let Ok(metadata) = fs::symlink_metadata(&candidate) {
            if crate::whiteout::is_whiteout_metadata(&metadata).is_some() {
                return None;
            }
            return Some(candidate);
        }
        for ancestor in rel_path.ancestors().skip(1) {
            let ancestor_path = upper.join(ancestor);
            let Ok(metadata) = fs::symlink_metadata(&ancestor_path) else {
                continue;
            };
            if crate::whiteout::is_whiteout_metadata(&metadata).is_some() {
                return None;
            }
            if !metadata.is_dir() || crate::whiteout::is_opaque_directory(&ancestor_path, &metadata)
            {
                return None;
            }
        }
    }
    None
}

struct CaptureTask {
    directory: PathBuf,
    rel_path: PathBuf,
}

struct CapturePacket {
    task_index: usize,
    directory: CapturedDirectory,
    children: Vec<CaptureTask>,
}

struct CapturedDirectory {
    entries: Option<Vec<ManifestEntry>>,
    jsonl: Vec<u8>,
    directory_ends: Vec<usize>,
    child_paths: Vec<PathBuf>,
}

fn capture_manifest(
    root: &Path,
    file: &File,
    retain_entries: bool,
) -> Result<Option<Vec<ManifestEntry>>> {
    let mut frontier = vec![CaptureTask {
        directory: root.to_path_buf(),
        rel_path: PathBuf::new(),
    }];
    let mut directories = BTreeMap::new();
    while !frontier.is_empty() {
        let packets = capture_frontier(&frontier, retain_entries)?;
        let mut next_frontier = Vec::new();
        for packet in packets {
            let rel_path = frontier[packet.task_index].rel_path.clone();
            directories.insert(rel_path, packet.directory);
            next_frontier.extend(packet.children);
        }
        next_frontier.sort_unstable_by(|left, right| left.rel_path.cmp(&right.rel_path));
        frontier = next_frontier;
    }
    write_ordered_entries(root, directories, file, retain_entries)
}

fn write_ordered_entries(
    root: &Path,
    mut directories: BTreeMap<PathBuf, CapturedDirectory>,
    file: &File,
    retain_entries: bool,
) -> Result<Option<Vec<ManifestEntry>>> {
    let root_entries =
        directories
            .remove(Path::new(""))
            .ok_or_else(|| CoreError::StateCorrupted {
                step: 0,
                reason: format!("manifest capture omitted {}", root.display()),
            })?;
    let mut stack = vec![DirectoryCursor::new(root_entries)];
    let total_entries = directories
        .values()
        .filter_map(|directory| directory.entries.as_ref())
        .map(Vec::len)
        .sum::<usize>()
        + stack[0].entries.as_ref().map_or(0, |entries| entries.len());
    let mut entries = retain_entries.then(|| Vec::with_capacity(total_entries));
    let mut writer = BufWriter::with_capacity(1024 * 1024, file);
    loop {
        let mut child = None;
        let mut pop = false;
        if let Some(cursor) = stack.last_mut() {
            if let Some(end) = cursor.directory_ends.get(cursor.directory_index).copied() {
                let expected_child = cursor.child_paths.get(cursor.directory_index).cloned();
                let Some(expected_child) = expected_child else {
                    return Err(CoreError::StateCorrupted {
                        step: 0,
                        reason: "manifest serialization child offsets were inconsistent"
                            .to_string(),
                    });
                };
                if let Some(entries_iter) = cursor.entries.as_mut() {
                    let mut child_path = None;
                    for entry in entries_iter.by_ref() {
                        let is_directory = entry.kind == EntryKind::Directory;
                        if is_directory {
                            child_path = Some(entry.rel_path.clone());
                        }
                        if let Some(entries) = entries.as_mut() {
                            entries.push(entry);
                        }
                        if is_directory {
                            break;
                        }
                    }
                    if child_path.as_deref() != Some(expected_child.as_path()) {
                        return Err(CoreError::StateCorrupted {
                            step: 0,
                            reason: "manifest serialization directory order was inconsistent"
                                .to_string(),
                        });
                    }
                }
                if end > cursor.jsonl.len() || end < cursor.start {
                    return Err(CoreError::StateCorrupted {
                        step: 0,
                        reason: "manifest serialization offsets were inconsistent".to_string(),
                    });
                }
                writer.write_all(&cursor.jsonl[cursor.start..end])?;
                cursor.start = end;
                cursor.directory_index += 1;
                child = Some(expected_child);
            } else {
                if let Some(entries_iter) = cursor.entries.as_mut() {
                    for entry in entries_iter.by_ref() {
                        if let Some(entries) = entries.as_mut() {
                            entries.push(entry);
                        }
                    }
                }
                writer.write_all(&cursor.jsonl[cursor.start..])?;
                cursor.start = cursor.jsonl.len();
                pop = true;
            }
        } else {
            break;
        }
        if let Some(child) = child {
            let child_entries =
                directories
                    .remove(&child)
                    .ok_or_else(|| CoreError::StateCorrupted {
                        step: 0,
                        reason: format!("manifest capture omitted directory {}", child.display()),
                    })?;
            stack.push(DirectoryCursor::new(child_entries));
        } else if pop {
            stack.pop();
        }
    }
    writer.flush()?;
    if let Some(path) = directories.keys().next() {
        return Err(CoreError::StateCorrupted {
            step: 0,
            reason: format!(
                "manifest capture found unreachable directory {}",
                path.display()
            ),
        });
    }
    Ok(entries)
}

struct DirectoryCursor {
    entries: Option<std::vec::IntoIter<ManifestEntry>>,
    jsonl: Vec<u8>,
    directory_ends: Vec<usize>,
    child_paths: Vec<PathBuf>,
    directory_index: usize,
    start: usize,
}

impl DirectoryCursor {
    fn new(directory: CapturedDirectory) -> Self {
        Self {
            entries: directory.entries.map(Vec::into_iter),
            jsonl: directory.jsonl,
            directory_ends: directory.directory_ends,
            child_paths: directory.child_paths,
            directory_index: 0,
            start: 0,
        }
    }
}

fn append_serialized_entry(
    entry: &ManifestEntry,
    output: &mut Vec<u8>,
    directory_ends: &mut Vec<usize>,
    child_paths: &mut Vec<PathBuf>,
) -> Result<()> {
    serde_json::to_writer(&mut *output, entry)?;
    output.push(b'\n');
    if entry.kind == EntryKind::Directory {
        directory_ends.push(output.len());
        child_paths.push(entry.rel_path.clone());
    }
    Ok(())
}

fn capture_frontier(tasks: &[CaptureTask], retain_entries: bool) -> Result<Vec<CapturePacket>> {
    let worker_count = capture_worker_count(tasks.len());
    if worker_count == 1 {
        return tasks
            .iter()
            .enumerate()
            .map(|(task_index, task)| capture_task(task_index, task, retain_entries))
            .collect();
    }

    let next_task = AtomicUsize::new(0);
    let (sender, receiver) = mpsc::sync_channel::<Result<CapturePacket>>(worker_count);
    thread::scope(|scope| -> Result<Vec<CapturePacket>> {
        let mut handles = Vec::with_capacity(worker_count);
        let next_task = &next_task;
        for _ in 0..worker_count {
            let sender = sender.clone();
            handles.push(scope.spawn(move || {
                loop {
                    let task_index = next_task.fetch_add(1, Ordering::Relaxed);
                    let Some(task) = tasks.get(task_index) else {
                        break;
                    };
                    let packet = match capture_task(task_index, task, retain_entries) {
                        Ok(packet) => packet,
                        Err(error) => {
                            let _ = sender.send(Err(error));
                            break;
                        }
                    };
                    if sender.send(Ok(packet)).is_err() {
                        break;
                    }
                }
            }));
        }
        drop(sender);

        let mut packets = (0..tasks.len()).map(|_| None).collect::<Vec<_>>();
        let mut first_error = None;
        for packet in receiver {
            match packet {
                Ok(packet) => {
                    let task_index = packet.task_index;
                    packets[task_index] = Some(packet);
                }
                Err(error) => {
                    first_error.get_or_insert(error);
                }
            }
        }
        for handle in handles {
            if handle.join().is_err() {
                first_error.get_or_insert_with(|| {
                    CoreError::Io(std::io::Error::other("manifest capture worker panicked"))
                });
            }
        }
        if let Some(error) = first_error {
            return Err(error);
        }
        packets
            .into_iter()
            .map(|packet| {
                packet.ok_or_else(|| CoreError::StateCorrupted {
                    step: 0,
                    reason: "manifest capture worker returned no packet".to_string(),
                })
            })
            .collect()
    })
}

fn capture_task(
    task_index: usize,
    task: &CaptureTask,
    retain_entries: bool,
) -> Result<CapturePacket> {
    let reader = fs::read_dir(&task.directory).map_err(|error| CoreError::StateCorrupted {
        step: 0,
        reason: format!(
            "failed to walk base workspace {}: {error}",
            task.directory.display()
        ),
    })?;
    let mut children = Vec::new();
    let mut items = Vec::new();
    for item in reader {
        let item = item.map_err(|error| CoreError::StateCorrupted {
            step: 0,
            reason: format!("failed to walk base workspace: {error}"),
        })?;
        let name = item.file_name();
        if is_skipped_name(&name) {
            continue;
        }
        items.push((name, item));
    }
    items.sort_unstable_by(|left, right| left.0.cmp(&right.0));
    let mut entries = retain_entries.then(|| Vec::with_capacity(items.len()));
    let mut jsonl = Vec::with_capacity(items.len().saturating_mul(220));
    let mut directory_ends = Vec::new();
    let mut child_paths = Vec::new();
    for (name, item) in items {
        let path = item.path();
        let metadata = fs::symlink_metadata(&path)?;
        if !metadata.is_file() && !metadata.is_dir() && !metadata.is_symlink() {
            continue;
        }
        let rel_path = task.rel_path.join(name);
        if metadata.is_dir() {
            children.push(CaptureTask {
                directory: path.clone(),
                rel_path: rel_path.clone(),
            });
        }
        let entry = ManifestEntry::from_metadata(rel_path, &path, &metadata)?;
        append_serialized_entry(&entry, &mut jsonl, &mut directory_ends, &mut child_paths)?;
        if let Some(entries) = entries.as_mut() {
            entries.push(entry);
        }
    }
    Ok(CapturePacket {
        task_index,
        directory: CapturedDirectory {
            entries,
            jsonl,
            directory_ends,
            child_paths,
        },
        children,
    })
}

fn capture_worker_count(task_count: usize) -> usize {
    let available = thread::available_parallelism()
        .map(|value| value.get())
        .unwrap_or(1);
    available.min(MAX_CAPTURE_THREADS).min(task_count).max(1)
}

fn is_skipped_name(name: &OsStr) -> bool {
    name == OsStr::new(".git") || name == OsStr::new(".cowbox")
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustix::fs::XattrFlags;
    use std::os::unix::fs::symlink;

    #[test]
    fn file_only_capture_matches_collected_manifest_bytes() {
        let root = tempfile::Builder::new()
            .prefix("cowbox-manifest-file-")
            .tempdir_in("/tmp")
            .expect("fixture");
        let workspace = root.path().join("workspace");
        fs::create_dir_all(workspace.join("branches/b/deep")).expect("directories");
        fs::create_dir_all(workspace.join("branches/a")).expect("directories");
        fs::write(workspace.join("branches/a.txt"), "sibling\n").expect("file");
        fs::write(workspace.join("branches/b/deep/value.txt"), "value\n").expect("file");
        fs::hard_link(
            workspace.join("branches/b/deep/value.txt"),
            workspace.join("branches/b/hardlink.txt"),
        )
        .expect("hardlink");
        rustix::fs::lsetxattr(
            workspace.join("branches/a.txt"),
            &b"user.cowbox.test"[..],
            b"present",
            XattrFlags::empty(),
        )
        .expect("xattr");
        symlink("branches/b/deep", workspace.join("link")).expect("symlink");

        let collected = root.path().join("collected.jsonl");
        let file_only = root.path().join("file-only.jsonl");
        let entries = capture_base_manifest(&workspace, &collected).expect("collected capture");
        capture_base_manifest_file(&workspace, &file_only).expect("file-only capture");
        assert_eq!(
            fs::read(&collected).expect("collected bytes"),
            fs::read(&file_only).expect("file-only bytes")
        );

        let paths = entries
            .iter()
            .map(|entry| entry.rel_path.clone())
            .collect::<Vec<_>>();
        let persisted_paths = fs::read_to_string(&file_only)
            .expect("manifest text")
            .lines()
            .map(|line| {
                serde_json::from_str::<ManifestEntry>(line)
                    .expect("manifest line")
                    .rel_path
            })
            .collect::<Vec<_>>();
        assert_eq!(paths, persisted_paths);
        assert_eq!(
            persisted_paths,
            [
                "branches",
                "branches/a",
                "branches/a.txt",
                "branches/b",
                "branches/b/deep",
                "branches/b/deep/value.txt",
                "branches/b/hardlink.txt",
                "link",
            ]
            .map(PathBuf::from)
        );
        assert_eq!(
            persisted_paths
                .iter()
                .filter(|path| path.as_path() == Path::new("link"))
                .count(),
            1
        );
        assert!(
            !persisted_paths
                .iter()
                .any(|path| { path.as_path() != Path::new("link") && path.starts_with("link") })
        );
    }

    #[test]
    fn file_only_capture_matches_a_wide_parallel_manifest() {
        let root = tempfile::Builder::new()
            .prefix("cowbox-manifest-wide-")
            .tempdir_in("/tmp")
            .expect("fixture");
        let workspace = root.path().join("workspace");
        for branch in 0..40 {
            let directory = workspace.join(format!("branches/branch-{branch:02}/deep/nested"));
            fs::create_dir_all(&directory).expect("directories");
            fs::write(directory.join("payload.txt"), format!("branch {branch}\n"))
                .expect("payload");
        }
        fs::write(workspace.join("z-root.txt"), "root\n").expect("root file");
        symlink(".", workspace.join("loop")).expect("symlink");
        rustix::fs::lsetxattr(
            workspace.join("z-root.txt"),
            &b"user.cowbox.test"[..],
            b"present",
            XattrFlags::empty(),
        )
        .expect("xattr");
        fs::hard_link(
            workspace.join("branches/branch-00/deep/nested/payload.txt"),
            workspace.join("branches/branch-00/hardlink.txt"),
        )
        .expect("hardlink");

        let collected = root.path().join("wide-collected.jsonl");
        let file_only = root.path().join("wide-file-only.jsonl");
        let entries = capture_base_manifest(&workspace, &collected).expect("collected capture");
        capture_base_manifest_file(&workspace, &file_only).expect("file-only capture");
        assert_eq!(
            fs::read(&collected).expect("collected bytes"),
            fs::read(&file_only).expect("file-only bytes")
        );

        let paths = entries
            .iter()
            .map(|entry| entry.rel_path.clone())
            .collect::<Vec<_>>();
        let mut sorted_paths = paths.clone();
        sorted_paths.sort();
        assert_eq!(
            paths, sorted_paths,
            "manifest paths were not globally sorted"
        );
        let persisted_paths = fs::read_to_string(&file_only)
            .expect("manifest text")
            .lines()
            .map(|line| {
                serde_json::from_str::<ManifestEntry>(line)
                    .expect("manifest line")
                    .rel_path
            })
            .collect::<Vec<_>>();
        assert_eq!(persisted_paths, paths);
        assert_eq!(paths.len(), 40 * 4 + 4);
        assert!(
            !paths
                .iter()
                .any(|path| path.as_path() != Path::new("loop") && path.starts_with("loop"))
        );
    }
}
