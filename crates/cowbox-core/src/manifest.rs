use crate::error::{CoreError, Result};
use crate::model::EntryKind;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use walkdir::{DirEntry, WalkDir};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ManifestEntry {
    pub rel_path: PathBuf,
    pub kind: EntryKind,
    pub mode: u32,
    pub size: u64,
    pub mtime_ns: i128,
    pub symlink_target: Option<PathBuf>,
}

impl ManifestEntry {
    pub fn from_metadata(rel_path: PathBuf, path: &Path, metadata: &fs::Metadata) -> Result<Self> {
        let kind = entry_kind(metadata);
        let symlink_target = if kind == EntryKind::Symlink {
            Some(fs::read_link(path)?)
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
    let mut entries = Vec::new();
    let walker = WalkDir::new(root)
        .follow_links(false)
        .min_depth(1)
        .into_iter()
        .filter_entry(|entry| !is_skipped(entry));

    for entry in walker {
        let entry = entry.map_err(|error| CoreError::StateCorrupted {
            step: 0,
            reason: format!("failed to walk base workspace: {error}"),
        })?;
        if !entry.file_type().is_file()
            && !entry.file_type().is_dir()
            && !entry.file_type().is_symlink()
        {
            continue;
        }
        let rel_path = entry
            .path()
            .strip_prefix(root)
            .map_err(|_| CoreError::Config("manifest path escaped the workspace".to_string()))?
            .to_path_buf();
        if rel_path.as_os_str().is_empty() {
            continue;
        }
        let metadata = fs::symlink_metadata(entry.path())?;
        entries.push(ManifestEntry::from_metadata(
            rel_path,
            entry.path(),
            &metadata,
        )?);
    }

    entries.sort_by(|left, right| left.rel_path.cmp(&right.rel_path));
    if let Some(parent) = output.parent() {
        fs::create_dir_all(parent)?;
    }
    let file = File::create(output)?;
    let mut writer = BufWriter::new(file);
    for entry in &entries {
        serde_json::to_writer(&mut writer, entry)?;
        writer.write_all(b"\n")?;
    }
    writer.flush()?;
    Ok(entries)
}

pub fn load_manifest(path: &Path) -> Result<BTreeMap<PathBuf, ManifestEntry>> {
    let file = File::open(path).map_err(|error| CoreError::StateCorrupted {
        step: 0,
        reason: format!("cannot open base manifest {}: {error}", path.display()),
    })?;
    let reader = BufReader::new(file);
    let mut entries = BTreeMap::new();
    for line in reader.lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let entry: ManifestEntry =
            serde_json::from_str(&line).map_err(|error| CoreError::StateCorrupted {
                step: 0,
                reason: format!("invalid base manifest line: {error}"),
            })?;
        entries.insert(entry.rel_path.clone(), entry);
    }
    Ok(entries)
}

pub fn metadata_matches(entry: &ManifestEntry, path: &Path) -> bool {
    let Ok(metadata) = fs::symlink_metadata(path) else {
        return false;
    };
    let kind = entry_kind(&metadata);
    if kind != entry.kind || (metadata.mode() & 0o7777) != entry.mode {
        return false;
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

fn is_skipped(entry: &DirEntry) -> bool {
    if entry.depth() == 0 {
        return false;
    }
    let name = entry.file_name().to_string_lossy();
    name == ".git" || name == ".cowbox"
}
