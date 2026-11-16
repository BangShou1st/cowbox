use super::error::{WinError, WinResult};
use super::paths;
use fs2::FileExt;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

static COUNTER: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone)]
pub struct WinStore {
    pub state_root: PathBuf,
}

impl WinStore {
    pub fn new(state_root: &Path) -> WinResult<Self> {
        let state_root = paths::canonicalize_existing_prefix(state_root)?;
        Ok(Self { state_root })
    }

    pub fn sessions_dir(&self) -> PathBuf {
        self.state_root.join("win").join("sessions")
    }

    pub fn session_dir(&self, session_id: &str) -> WinResult<PathBuf> {
        validate_session_id(session_id)?;
        Ok(self.sessions_dir().join(session_id))
    }

    pub fn ensure_layout(&self) -> WinResult<()> {
        fs::create_dir_all(self.sessions_dir()).map_err(|error| {
            WinError::io(
                "WIN_STATE_ROOT",
                format!(
                    "cannot create state root {}",
                    paths::display(&self.state_root)
                ),
                error,
            )
        })?;
        Ok(())
    }

    pub fn session_json(&self, session_id: &str) -> WinResult<PathBuf> {
        Ok(self.session_dir(session_id)?.join("session.json"))
    }

    pub fn base_manifest(&self, session_id: &str) -> WinResult<PathBuf> {
        Ok(self.session_dir(session_id)?.join("base.json"))
    }

    pub fn step_delta(&self, session_id: &str, step: u64) -> WinResult<PathBuf> {
        Ok(self
            .session_dir(session_id)?
            .join("steps")
            .join(format!("{step}.json")))
    }

    pub fn objects(&self, session_id: &str) -> WinResult<ObjectStore> {
        Ok(ObjectStore::new(
            self.session_dir(session_id)?.join("objects"),
        ))
    }

    pub fn lock_session(&self, session_id: &str) -> WinResult<File> {
        let dir = self.session_dir(session_id)?;
        fs::create_dir_all(&dir)?;
        let file = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(dir.join("lock"))?;
        file.try_lock_exclusive().map_err(|error| {
            WinError::new(
                "WIN_SESSION_LOCKED",
                format!("session {session_id} is locked by another cowbox process: {error}"),
            )
            .hint("finish the other cowbox command, then retry")
            .exit(2)
        })?;
        Ok(file)
    }
}

#[derive(Debug, Clone)]
pub struct ObjectStore {
    pub root: PathBuf,
}

impl ObjectStore {
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }

    pub fn object_path(&self, sha256: &str) -> WinResult<PathBuf> {
        if sha256.len() != 64 || !sha256.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(WinError::new(
                "WIN_STATE_CORRUPTED",
                format!("invalid object hash {sha256:?}"),
            ));
        }
        let lower = sha256.to_ascii_lowercase();
        Ok(self.root.join(&lower[..2]).join(&lower[2..]))
    }

    pub fn put_file(&self, source: &Path, expected_sha256: &str) -> WinResult<u64> {
        let object = self.object_path(expected_sha256)?;
        if let Ok(metadata) = fs::metadata(&object) {
            return Ok(metadata.len());
        }
        let parent = object
            .parent()
            .ok_or_else(|| WinError::new("WIN_STATE_CORRUPTED", "object path has no parent"))?;
        fs::create_dir_all(parent)?;
        let temp = parent.join(format!(
            ".tmp-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        let result = (|| -> WinResult<u64> {
            fs::copy(source, &temp).map_err(|error| {
                WinError::io(
                    "WIN_HOST_CHANGED_DURING_IMPORT",
                    format!("cannot copy {}", paths::display(source)),
                    error,
                )
            })?;
            clear_readonly(&temp)?;
            let (size, actual) = super::scan::hash_file(&temp)?;
            if actual != expected_sha256 {
                return Err(WinError::new(
                    "WIN_HOST_CHANGED_DURING_IMPORT",
                    format!(
                        "{} changed while cowbox was importing it (expected {}, got {})",
                        paths::display(source),
                        expected_sha256,
                        actual
                    ),
                )
                .hint("stop the writer, then retry")
                .exit(2));
            }
            sync_file(&temp)?;
            fs::rename(&temp, &object)?;
            Ok(size)
        })();
        if result.is_err() {
            let _ = fs::remove_file(&temp);
        }
        result
    }

    pub fn copy_to(
        &self,
        sha256: &str,
        destination: &Path,
        readonly: bool,
        mtime_unix_ms: Option<i64>,
    ) -> WinResult<()> {
        let object = self.object_path(sha256)?;
        if !object.exists() {
            return Err(WinError::new(
                "WIN_STATE_CORRUPTED",
                format!("object {sha256} is missing at {}", paths::display(&object)),
            ));
        }
        if let Some(parent) = destination.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::copy(&object, destination).map_err(|error| {
            WinError::io(
                "WIN_IO",
                format!("cannot materialize {}", paths::display(destination)),
                error,
            )
        })?;
        if let Some(mtime) = mtime_unix_ms {
            set_mtime(destination, mtime)?;
        }
        let mut permissions = fs::metadata(destination)?.permissions();
        permissions.set_readonly(readonly);
        fs::set_permissions(destination, permissions)?;
        Ok(())
    }

    pub fn total_bytes(&self) -> u64 {
        directory_size(&self.root)
    }
}

pub fn sync_file(path: &Path) -> WinResult<()> {
    let file = OpenOptions::new().read(true).write(true).open(path)?;
    file.sync_all()?;
    Ok(())
}

#[allow(clippy::permissions_set_readonly_false)]
pub fn clear_readonly(path: &Path) -> WinResult<()> {
    let mut permissions = fs::metadata(path)?.permissions();
    if permissions.readonly() {
        permissions.set_readonly(false);
        fs::set_permissions(path, permissions)?;
    }
    Ok(())
}

pub fn set_mtime(path: &Path, unix_ms: i64) -> WinResult<()> {
    let seconds = unix_ms.div_euclid(1000);
    let nanos = (unix_ms.rem_euclid(1000) as u32) * 1_000_000;
    let time = filetime::FileTime::from_unix_time(seconds, nanos);
    filetime::set_file_mtime(path, time).map_err(|error| {
        WinError::io(
            "WIN_IO",
            format!("cannot set modification time on {}", paths::display(path)),
            error,
        )
    })
}

pub fn write_json_atomic<T: Serialize>(path: &Path, value: &T) -> WinResult<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let name = path
        .file_name()
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or_else(|| "state".to_string());
    let temp = path.with_file_name(format!(
        ".{name}.tmp-{}-{}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    let data = serde_json::to_vec_pretty(value)?;
    let result = (|| -> WinResult<()> {
        let mut file = File::create(&temp)?;
        file.write_all(&data)?;
        file.write_all(b"\n")?;
        file.sync_all()?;
        drop(file);
        fs::rename(&temp, path)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temp);
    }
    result
}

pub fn read_json<T: DeserializeOwned>(path: &Path) -> WinResult<T> {
    let bytes = fs::read(path).map_err(|error| {
        WinError::io(
            "WIN_STATE_CORRUPTED",
            format!("cannot read {}", paths::display(path)),
            error,
        )
    })?;
    serde_json::from_slice(&bytes).map_err(|error| {
        WinError::new(
            "WIN_STATE_CORRUPTED",
            format!("invalid JSON in {}: {error}", paths::display(path)),
        )
    })
}

pub fn new_session_id() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or_default();
    let counter = COUNTER.fetch_add(1, Ordering::Relaxed);
    let mut hasher = Sha256::new();
    hasher.update(nanos.to_le_bytes());
    hasher.update(std::process::id().to_le_bytes());
    hasher.update(counter.to_le_bytes());
    hex::encode(&hasher.finalize()[..6])
}

pub fn validate_session_id(session_id: &str) -> WinResult<()> {
    if session_id.len() < 6
        || session_id.len() > 64
        || !session_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
    {
        return Err(WinError::new(
            "WIN_SESSION_NOT_FOUND",
            format!("invalid session id {session_id:?}"),
        ));
    }
    Ok(())
}

pub fn directory_size(path: &Path) -> u64 {
    let Ok(entries) = fs::read_dir(path) else {
        return 0;
    };
    let mut total: u64 = 0;
    for entry in entries.flatten() {
        let Ok(metadata) = fs::symlink_metadata(entry.path()) else {
            continue;
        };
        #[cfg(windows)]
        if paths::is_reparse(&metadata) {
            continue;
        }
        if metadata.is_dir() {
            total = total.saturating_add(directory_size(&entry.path()));
        } else if metadata.is_file() {
            total = total.saturating_add(metadata.len());
        }
    }
    total
}

#[derive(Debug, Clone)]
pub struct FsInfo {
    pub mount_point: String,
    pub filesystem: Option<String>,
    pub available_bytes: u64,
    pub total_bytes: u64,
}

impl FsInfo {
    pub fn is_ntfs(&self) -> bool {
        self.filesystem
            .as_deref()
            .is_some_and(|name| name.eq_ignore_ascii_case("NTFS"))
    }
}

pub fn filesystem_info(path: &Path) -> Option<FsInfo> {
    let path = paths::canonicalize_existing_prefix(path).unwrap_or_else(|_| path.to_path_buf());
    let haystack = paths::display(&path).to_ascii_lowercase();
    let disks = sysinfo::Disks::new_with_refreshed_list();
    let mut best: Option<FsInfo> = None;
    let mut best_len = 0usize;
    for disk in disks.list() {
        let mount = paths::display(disk.mount_point()).to_ascii_lowercase();
        if haystack == mount
            || haystack.starts_with(&(mount.clone() + "\\"))
            || haystack.starts_with(&mount)
        {
            let length = mount.len();
            if best.is_none() || length > best_len {
                best_len = length;
                best = Some(FsInfo {
                    mount_point: paths::display(disk.mount_point()),
                    filesystem: Some(disk.file_system().to_string_lossy().to_string()),
                    available_bytes: disk.available_space(),
                    total_bytes: disk.total_space(),
                });
            }
        }
    }
    best
}

pub fn ensure_ntfs(path: &Path, role: &str) -> WinResult<FsInfo> {
    let info = filesystem_info(path).ok_or_else(|| {
        WinError::new(
            "WIN_FILESYSTEM_UNKNOWN",
            format!(
                "cannot determine the filesystem for {role} {}",
                paths::display(path)
            ),
        )
        .hint("use a local NTFS volume")
        .exit(2)
    })?;
    if !info.is_ntfs() {
        return Err(WinError::new(
            "WIN_NTFS_REQUIRED",
            format!(
                "{role} {} is on {}, but the native Windows preview requires NTFS",
                paths::display(path),
                info.filesystem
                    .as_deref()
                    .unwrap_or("an unknown filesystem")
            ),
        )
        .hint("move the workspace or state root to a local NTFS volume")
        .exit(2));
    }
    Ok(info)
}

pub fn create_junction(link: &Path, target: &Path) -> WinResult<()> {
    remove_junction_entry(link)?;
    junction::create(target, link).map_err(|error| {
        WinError::io(
            "WIN_JUNCTION",
            format!(
                "cannot create junction {} -> {}",
                paths::display(link),
                paths::display(target)
            ),
            error,
        )
        .hint("the managed generation is still available at its direct path")
    })
}

fn remove_junction_entry(link: &Path) -> WinResult<()> {
    let metadata = match fs::symlink_metadata(link) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => {
            return Err(WinError::io(
                "WIN_JUNCTION",
                format!("cannot inspect {}", paths::display(link)),
                error,
            ));
        }
    };
    #[cfg(windows)]
    if paths::is_reparse(&metadata) {
        junction::delete(link).map_err(|error| {
            WinError::io(
                "WIN_JUNCTION",
                format!("cannot remove junction {}", paths::display(link)),
                error,
            )
        })?;
        let _ = fs::remove_dir(link);
        return Ok(());
    }
    if metadata.is_dir() {
        fs::remove_dir(link).map_err(|error| {
            WinError::io(
                "WIN_JUNCTION",
                format!(
                    "cannot replace {} with a junction because it is a non-empty directory",
                    paths::display(link)
                ),
                error,
            )
        })?;
        return Ok(());
    }
    Err(WinError::new(
        "WIN_JUNCTION",
        format!(
            "{} already exists and is not a junction or directory",
            paths::display(link)
        ),
    ))
}

pub fn remove_junction(link: &Path) -> WinResult<()> {
    remove_junction_entry(link)
}

pub fn remove_session_tree(store: &WinStore, session_id: &str) -> WinResult<()> {
    let dir = store.session_dir(session_id)?;
    let root = paths::canonicalize(&store.sessions_dir())?;
    let dir = paths::canonicalize_existing_prefix(&dir)?;
    if !dir.starts_with(&root) || dir == root {
        return Err(WinError::new(
            "WIN_STATE_ROOT",
            format!(
                "refusing to remove session path {} outside {}",
                paths::display(&dir),
                paths::display(&root)
            ),
        ));
    }
    remove_junction(&dir.join("current"))?;
    fs::remove_dir_all(&dir).map_err(|error| {
        WinError::io(
            "WIN_IO",
            format!("cannot remove {}", paths::display(&dir)),
            error,
        )
    })?;
    Ok(())
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct WinIndexEntry {
    schema_version: u32,
    session_id: String,
    state_root: PathBuf,
    workspace: PathBuf,
    created_at_unix_ms: i64,
}

pub fn index_root() -> WinResult<PathBuf> {
    if let Some(home) = std::env::var_os("COWBOX_WIN_HOME") {
        return Ok(PathBuf::from(home));
    }
    if let Some(local) = std::env::var_os("LOCALAPPDATA") {
        return Ok(PathBuf::from(local).join("cowbox").join("win"));
    }
    if let Some(profile) = std::env::var_os("USERPROFILE") {
        return Ok(PathBuf::from(profile).join(".cowbox").join("win"));
    }
    Err(WinError::new(
        "WIN_STATE_ROOT",
        "LOCALAPPDATA and USERPROFILE are both unset",
    ))
}

pub fn register_index(session_id: &str, state_root: &Path, workspace: &Path) -> WinResult<PathBuf> {
    let root = index_root()?;
    fs::create_dir_all(&root)?;
    let path = root.join(format!("{session_id}.json"));
    let entry = WinIndexEntry {
        schema_version: 1,
        session_id: session_id.to_string(),
        state_root: state_root.to_path_buf(),
        workspace: workspace.to_path_buf(),
        created_at_unix_ms: now_ms(),
    };
    write_json_atomic(&path, &entry)?;
    Ok(path)
}

pub fn lookup_index(session_id: &str) -> WinResult<Option<PathBuf>> {
    validate_session_id(session_id)?;
    let path = index_root()?.join(format!("{session_id}.json"));
    if !path.exists() {
        return Ok(None);
    }
    let entry: WinIndexEntry = read_json(&path)?;
    if entry.schema_version != 1 || entry.session_id != session_id {
        return Err(WinError::new(
            "WIN_STATE_CORRUPTED",
            format!("invalid session index {}", paths::display(&path)),
        ));
    }
    Ok(Some(entry.state_root))
}

pub fn index_entries_for_workspace(workspace: &Path) -> WinResult<Vec<(String, PathBuf)>> {
    let root = index_root()?;
    if !root.exists() {
        return Ok(Vec::new());
    }
    let workspace = paths::canonicalize(workspace).unwrap_or_else(|_| workspace.to_path_buf());
    let mut entries = Vec::new();
    for item in fs::read_dir(&root)? {
        let item = item?;
        if item.path().extension().and_then(|ext| ext.to_str()) != Some("json") {
            continue;
        }
        let Ok(entry) = read_json::<WinIndexEntry>(&item.path()) else {
            continue;
        };
        let recorded =
            paths::canonicalize(&entry.workspace).unwrap_or_else(|_| entry.workspace.clone());
        if recorded == workspace {
            entries.push((entry.session_id, entry.state_root));
        }
    }
    entries.sort();
    Ok(entries)
}

pub fn remove_index(session_id: &str) -> WinResult<()> {
    let path = index_root()?.join(format!("{session_id}.json"));
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(WinError::io("WIN_IO", "cannot remove session index", error)),
    }
}

pub fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or_default()
}
