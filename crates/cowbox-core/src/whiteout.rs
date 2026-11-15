use crate::model::WhiteoutKind;
use std::fs;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::FileTypeExt;
use std::os::unix::fs::MetadataExt;
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UpperEntryKind {
    Whiteout(WhiteoutKind),
    OpaqueDirectory,
    Entry,
}

pub fn classify_upper_entry(path: &Path, metadata: &fs::Metadata) -> UpperEntryKind {
    if let Some(kind) = is_whiteout_metadata(metadata) {
        return UpperEntryKind::Whiteout(kind);
    }
    if has_whiteout_xattr(path, metadata) {
        return UpperEntryKind::Whiteout(WhiteoutKind::Xattr);
    }
    if is_opaque_directory(path, metadata) {
        return UpperEntryKind::OpaqueDirectory;
    }
    UpperEntryKind::Entry
}

pub fn is_whiteout_metadata(metadata: &fs::Metadata) -> Option<WhiteoutKind> {
    if metadata.file_type().is_char_device() && metadata.rdev() == 0 {
        return Some(WhiteoutKind::CharDevice);
    }
    None
}

pub fn is_whiteout(path: &Path, metadata: &fs::Metadata) -> bool {
    is_whiteout_metadata(metadata).is_some() || has_whiteout_xattr(path, metadata)
}

pub fn has_whiteout_xattr(path: &Path, metadata: &fs::Metadata) -> bool {
    if metadata.file_type().is_symlink() {
        return false;
    }
    xattr_value(path, b"user.overlay.whiteout").is_some()
        || xattr_value(path, b"trusted.overlay.whiteout").is_some()
}

pub fn is_opaque_directory(path: &Path, metadata: &fs::Metadata) -> bool {
    if !metadata.is_dir() {
        return false;
    }
    xattr_value(path, b"user.overlay.opaque")
        .map(|value| value == b"y")
        .unwrap_or(false)
        || xattr_value(path, b"trusted.overlay.opaque")
            .map(|value| value == b"y")
            .unwrap_or(false)
}

fn xattr_value(path: &Path, name: &[u8]) -> Option<Vec<u8>> {
    let name = std::ffi::OsStr::from_bytes(name);
    let size = rustix::fs::lgetxattr_query(path, name).ok()?;
    if size == 0 {
        return Some(Vec::new());
    }
    let mut buffer = vec![0u8; size];
    let len = rustix::fs::lgetxattr(path, name, &mut buffer[..]).ok()?;
    buffer.truncate(len);
    Some(buffer)
}

pub fn classify_parts(
    is_char_device: bool,
    rdev: u64,
    has_whiteout_xattr: bool,
    opaque_value: Option<&[u8]>,
) -> UpperEntryKind {
    if is_char_device && rdev == 0 {
        return UpperEntryKind::Whiteout(WhiteoutKind::CharDevice);
    }
    if has_whiteout_xattr {
        return UpperEntryKind::Whiteout(WhiteoutKind::Xattr);
    }
    if opaque_value == Some(b"y") {
        return UpperEntryKind::OpaqueDirectory;
    }
    UpperEntryKind::Entry
}
