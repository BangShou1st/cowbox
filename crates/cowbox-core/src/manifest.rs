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
    capture_manifest(root, &file)
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
    entries: Vec<ManifestEntry>,
    jsonl: Vec<u8>,
    jsonl_ends: Vec<usize>,
}

fn capture_manifest(root: &Path, file: &File) -> Result<Vec<ManifestEntry>> {
    let mut frontier = vec![CaptureTask {
        directory: root.to_path_buf(),
        rel_path: PathBuf::new(),
    }];
    let mut directories = BTreeMap::new();
    while !frontier.is_empty() {
        let packets = capture_frontier(&frontier)?;
        let mut next_frontier = Vec::new();
        for packet in packets {
            let rel_path = frontier[packet.task_index].rel_path.clone();
            directories.insert(rel_path, packet.directory);
            next_frontier.extend(packet.children);
        }
        next_frontier.sort_unstable_by(|left, right| left.rel_path.cmp(&right.rel_path));
        frontier = next_frontier;
    }
    write_ordered_entries(root, directories, file)
}

fn write_ordered_entries(
    root: &Path,
    mut directories: BTreeMap<PathBuf, CapturedDirectory>,
    file: &File,
) -> Result<Vec<ManifestEntry>> {
    let root_entries =
        directories
            .remove(Path::new(""))
            .ok_or_else(|| CoreError::StateCorrupted {
                step: 0,
                reason: format!("manifest capture omitted {}", root.display()),
            })?;
    let mut stack = vec![DirectoryCursor::new(root_entries)];
    let mut entries = Vec::new();
    let mut writer = BufWriter::with_capacity(1024 * 1024, file);
    while let Some(cursor) = stack.last_mut() {
        let Some(entry) = cursor.entries.next() else {
            if cursor.jsonl_ends.next().is_some() || cursor.start != cursor.jsonl.len() {
                return Err(CoreError::StateCorrupted {
                    step: 0,
                    reason: "manifest serialization offsets were inconsistent".to_string(),
                });
            }
            stack.pop();
            continue;
        };
        let Some(end) = cursor.jsonl_ends.next() else {
            return Err(CoreError::StateCorrupted {
                step: 0,
                reason: "manifest serialization was missing an entry".to_string(),
            });
        };
        writer.write_all(&cursor.jsonl[cursor.start..end])?;
        cursor.start = end;
        let child = (entry.kind == EntryKind::Directory).then(|| entry.rel_path.clone());
        entries.push(entry);
        if let Some(child) = child {
            let child_entries =
                directories
                    .remove(&child)
                    .ok_or_else(|| CoreError::StateCorrupted {
                        step: 0,
                        reason: format!("manifest capture omitted directory {}", child.display()),
                    })?;
            stack.push(DirectoryCursor::new(child_entries));
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
    entries: std::vec::IntoIter<ManifestEntry>,
    jsonl: Vec<u8>,
    jsonl_ends: std::vec::IntoIter<usize>,
    start: usize,
}

impl DirectoryCursor {
    fn new(directory: CapturedDirectory) -> Self {
        Self {
            entries: directory.entries.into_iter(),
            jsonl: directory.jsonl,
            jsonl_ends: directory.jsonl_ends.into_iter(),
            start: 0,
        }
    }
}

fn serialize_entries(entries: &[ManifestEntry]) -> Result<(Vec<u8>, Vec<usize>)> {
    let mut output = Vec::with_capacity(entries.len().saturating_mul(220));
    let mut ends = Vec::with_capacity(entries.len());
    for entry in entries {
        serde_json::to_writer(&mut output, entry)?;
        output.push(b'\n');
        ends.push(output.len());
    }
    Ok((output, ends))
}

fn capture_frontier(tasks: &[CaptureTask]) -> Result<Vec<CapturePacket>> {
    let worker_count = capture_worker_count(tasks.len());
    if worker_count == 1 {
        return tasks
            .iter()
            .enumerate()
            .map(|(task_index, task)| capture_task(task_index, task))
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
                    let packet = match capture_task(task_index, task) {
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

fn capture_task(task_index: usize, task: &CaptureTask) -> Result<CapturePacket> {
    let reader = fs::read_dir(&task.directory).map_err(|error| CoreError::StateCorrupted {
        step: 0,
        reason: format!(
            "failed to walk base workspace {}: {error}",
            task.directory.display()
        ),
    })?;
    let mut entries = Vec::new();
    let mut children = Vec::new();
    for item in reader {
        let item = item.map_err(|error| CoreError::StateCorrupted {
            step: 0,
            reason: format!("failed to walk base workspace: {error}"),
        })?;
        let name = item.file_name();
        if is_skipped_name(&name) {
            continue;
        }
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
        entries.push(ManifestEntry::from_metadata(rel_path, &path, &metadata)?);
    }
    entries.sort_unstable_by(|left, right| left.rel_path.cmp(&right.rel_path));
    children.sort_unstable_by(|left, right| left.rel_path.cmp(&right.rel_path));
    let (jsonl, jsonl_ends) = serialize_entries(&entries)?;
    Ok(CapturePacket {
        task_index,
        directory: CapturedDirectory {
            entries,
            jsonl,
            jsonl_ends,
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
