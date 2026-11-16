use crate::error::{CoreError, Result};
use rustix::fs::XattrFlags;
use sha2::{Digest, Sha256};
use std::path::Path;

const EMPTY_XATTR_DIGEST: &str = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct UserXattr {
    pub name: Vec<u8>,
    pub value: Vec<u8>,
}

pub(crate) fn digest_checked(path: &Path) -> Result<String> {
    let attrs = list_user(path)?;
    Ok(digest_entries(&attrs))
}

pub(crate) fn digest_lossy(path: &Path) -> Option<String> {
    digest_checked(path).ok()
}

pub(crate) fn digest_entries(attrs: &[UserXattr]) -> String {
    if attrs.is_empty() {
        return EMPTY_XATTR_DIGEST.to_string();
    }
    let mut hasher = Sha256::new();
    for attr in attrs {
        hasher.update((attr.name.len() as u64).to_le_bytes());
        hasher.update(&attr.name);
        hasher.update((attr.value.len() as u64).to_le_bytes());
        hasher.update(&attr.value);
    }
    hex::encode(hasher.finalize())
}

pub(crate) fn list_user(path: &Path) -> Result<Vec<UserXattr>> {
    let size = match rustix::fs::llistxattr_query(path) {
        Ok(size) => size,
        Err(error) if xattr_unsupported(error) => return Ok(Vec::new()),
        Err(error) => return Err(io_error(path, "list", error)),
    };
    if size == 0 {
        return Ok(Vec::new());
    }

    let mut buffer = vec![0u8; size];
    let read = match rustix::fs::llistxattr(path, &mut buffer[..]) {
        Ok(read) => read,
        Err(error) if xattr_unsupported(error) => return Ok(Vec::new()),
        Err(error) => return Err(io_error(path, "list", error)),
    };
    buffer.truncate(read);

    let mut attrs = Vec::new();
    for name in buffer.split(|byte| *byte == 0) {
        if name.is_empty() || !name.starts_with(b"user.") || name.starts_with(b"user.overlay.") {
            continue;
        }
        let value = match read_value(path, name) {
            Ok(value) => value,
            Err(error) if error == rustix::io::Errno::NODATA => continue,
            Err(error) if error == rustix::io::Errno::NOTSUP => continue,
            Err(error) => return Err(io_error(path, "read", error)),
        };
        attrs.push(UserXattr {
            name: name.to_vec(),
            value,
        });
    }
    attrs.sort_by(|left, right| left.name.cmp(&right.name));
    Ok(attrs)
}

pub(crate) fn copy(source: &Path, target: &Path) -> Result<()> {
    let source_attrs = list_user(source)?;
    let target_attrs = list_user(target)?;
    for attr in &target_attrs {
        if !source_attrs.iter().any(|item| item.name == attr.name) {
            match rustix::fs::lremovexattr(target, &attr.name) {
                Ok(()) => {}
                Err(error) if error == rustix::io::Errno::NODATA => {}
                Err(error) if xattr_unsupported(error) => {}
                Err(error) => return Err(io_error(target, "remove", error)),
            }
        }
    }
    for attr in source_attrs {
        if let Err(error) =
            rustix::fs::lsetxattr(target, &attr.name, &attr.value, XattrFlags::empty())
        {
            if xattr_unsupported(error) {
                return Err(CoreError::CommitFailed(format!(
                    "cannot preserve user xattrs from {} to {}: target filesystem does not support xattrs",
                    source.display(),
                    target.display()
                )));
            }
            return Err(io_error(target, "write", error));
        }
    }
    Ok(())
}

fn read_value(path: &Path, name: &[u8]) -> std::result::Result<Vec<u8>, rustix::io::Errno> {
    let size = rustix::fs::lgetxattr_query(path, name)?;
    let mut value = vec![0u8; size];
    let read = rustix::fs::lgetxattr(path, name, &mut value[..])?;
    value.truncate(read);
    Ok(value)
}

fn xattr_unsupported(error: rustix::io::Errno) -> bool {
    error == rustix::io::Errno::NOTSUP
        || error == rustix::io::Errno::OPNOTSUPP
        || error == rustix::io::Errno::NOSYS
}

fn io_error(path: &Path, action: &str, error: rustix::io::Errno) -> CoreError {
    CoreError::Io(std::io::Error::new(
        std::io::Error::from(error).kind(),
        format!("{action} xattrs for {}: {error}", path.display()),
    ))
}
