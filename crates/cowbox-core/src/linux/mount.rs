use super::spec::RunSpec;
use crate::error::{CoreError, Result};
use rustix::mount::{MountFlags, UnmountFlags, mount, unmount};
use std::ffi::CString;
use std::fs;

pub fn mount_overlay(spec: &RunSpec) -> Result<()> {
    fs::create_dir_all(&spec.merged_dir)?;
    let lower = lower_option(&spec.lower_dirs)?;

    let upper = spec
        .upper_dir
        .to_str()
        .ok_or_else(|| CoreError::Config("upperdir must be valid UTF-8".to_string()))?;
    let work = spec
        .work_dir
        .to_str()
        .ok_or_else(|| CoreError::Config("workdir must be valid UTF-8".to_string()))?;
    let data = format!("lowerdir={lower},upperdir={upper},workdir={work},userxattr");
    let data = CString::new(data)
        .map_err(|_| CoreError::Config("overlay mount options contained a NUL byte".to_string()))?;
    mount(
        "overlay",
        &spec.merged_dir,
        "overlay",
        MountFlags::empty(),
        Some(data.as_c_str()),
    )
    .map_err(|error| {
        CoreError::OverlayFilesystem(format!(
            "mounting unprivileged OverlayFS at {} failed: {error}",
            spec.merged_dir.display()
        ))
    })
}

fn lower_option(directories: &[std::path::PathBuf]) -> Result<String> {
    let mut lower = String::new();
    for directory in directories {
        let text = directory.to_str().ok_or_else(|| {
            CoreError::Config(format!(
                "overlay paths must be valid UTF-8: {}",
                directory.display()
            ))
        })?;
        if text.contains(':') || text.contains(',') {
            return Err(CoreError::CowUnsupported {
                path: directory.clone(),
                reason: "OverlayFS option paths cannot contain ':' or ','".to_string(),
            });
        }
        if !lower.is_empty() {
            lower.push(':');
        }
        lower.push_str(text);
    }
    Ok(lower)
}

pub fn mount_proc() -> Result<()> {
    mount(
        "proc",
        "/proc",
        "proc",
        MountFlags::NOSUID | MountFlags::NODEV | MountFlags::NOEXEC,
        None,
    )
    .map_err(|error| {
        CoreError::IsolationFailed(format!("mounting a private procfs failed: {error}"))
    })
}

pub fn mount_cache_overlays(spec: &RunSpec) -> Result<()> {
    for cache in &spec.cache_mounts {
        fs::create_dir_all(&cache.destination)?;
        fs::create_dir_all(&cache.upper_dir)?;
        if cache.work_dir.exists() {
            fs::remove_dir_all(&cache.work_dir)?;
        }
        fs::create_dir_all(&cache.work_dir)?;
        let lower = lower_option(std::slice::from_ref(&cache.source))?;
        let upper = cache
            .upper_dir
            .to_str()
            .ok_or_else(|| CoreError::Config("cache upperdir must be valid UTF-8".to_string()))?;
        let work = cache
            .work_dir
            .to_str()
            .ok_or_else(|| CoreError::Config("cache workdir must be valid UTF-8".to_string()))?;
        let data = format!("lowerdir={lower},upperdir={upper},workdir={work},userxattr");
        let data = CString::new(data).map_err(|_| {
            CoreError::Config("cache overlay mount options contained a NUL byte".to_string())
        })?;
        mount(
            "overlay",
            &cache.destination,
            "overlay",
            MountFlags::empty(),
            Some(data.as_c_str()),
        )
        .map_err(|error| {
            CoreError::OverlayFilesystem(format!(
                "mounting {} host cache {} at {} failed: {error}",
                cache.tool,
                cache.source.display(),
                cache.destination.display()
            ))
        })?;
    }
    Ok(())
}

pub fn unmount_overlay(spec: &RunSpec) -> Result<()> {
    unmount(&spec.merged_dir, UnmountFlags::DETACH).map_err(|error| {
        CoreError::OverlayFilesystem(format!(
            "unmounting {} failed: {error}",
            spec.merged_dir.display()
        ))
    })
}
