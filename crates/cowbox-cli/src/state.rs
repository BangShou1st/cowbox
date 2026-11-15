use crate::error::{CliError, CliResult};
use crate::workspace::{self, ActivePointer};
use cowbox_core::SessionLayout;
use cowbox_dag::SessionDag;
use fs2::FileExt;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

pub const SESSION_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SessionStatus {
    Active,
    Committed,
    Discarded,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionRecord {
    pub schema_version: u32,
    pub session_id: String,
    pub workspace: PathBuf,
    pub status: SessionStatus,
    pub created_at_unix_ms: i64,
    pub updated_at_unix_ms: i64,
    pub rollback_count: u64,
    pub last_commit_unix_ms: Option<i64>,
    pub dag: SessionDag,
}

#[derive(Debug, Clone)]
pub struct SessionStore {
    pub workspace: PathBuf,
    pub state_root: PathBuf,
    workspaces_dir: PathBuf,
    sessions_dir: PathBuf,
    active_path: PathBuf,
    lock_path: PathBuf,
}

impl SessionStore {
    pub fn new(workspace: PathBuf, state_root: PathBuf) -> CliResult<Self> {
        let workspaces_dir = state_root.join("workspaces");
        let sessions_dir = state_root.join("sessions");
        fs::create_dir_all(&workspaces_dir)?;
        fs::create_dir_all(&sessions_dir)?;
        let key = workspace::workspace_hash(&workspace);
        let active_path = workspaces_dir.join(format!("{key}.json"));
        let lock_path = workspaces_dir.join(format!("{key}.lock"));
        Ok(Self {
            workspace,
            state_root,
            workspaces_dir,
            sessions_dir,
            active_path,
            lock_path,
        })
    }

    pub fn lock(&self) -> CliResult<File> {
        let file = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(&self.lock_path)?;
        file.lock_exclusive()
            .map_err(|error| CliError::new("LOCK_FAILED", error.to_string()))?;
        Ok(file)
    }

    pub fn load_active(&self) -> CliResult<Option<SessionRecord>> {
        if !self.active_path.exists() {
            return Ok(None);
        }
        let pointer: ActivePointer = read_json(&self.active_path)?;
        if pointer.schema_version != 1 {
            return Err(CliError::new(
                "STATE_CORRUPTED",
                format!(
                    "unsupported active pointer schema in {}",
                    self.active_path.display()
                ),
            ));
        }
        if pointer.workspace != self.workspace {
            return Err(CliError::new(
                "STATE_CORRUPTED",
                format!(
                    "active pointer belongs to {}, expected {}",
                    pointer.workspace.display(),
                    self.workspace.display()
                ),
            ));
        }
        let record = self.load(&pointer.session_id)?;
        if record.status != SessionStatus::Active {
            return Err(CliError::new(
                "STATE_CORRUPTED",
                format!(
                    "active pointer references a {:?} session {}",
                    record.status, record.session_id
                ),
            )
            .hint("run `cowbox discard --yes` or delete the workspace pointer"));
        }
        Ok(Some(record))
    }

    pub fn load(&self, session_id: &str) -> CliResult<SessionRecord> {
        let path = self.session_dir(session_id).join("session.json");
        if !path.exists() {
            return Err(CliError::new(
                "STATE_CORRUPTED",
                format!(
                    "session {} does not exist at {}",
                    session_id,
                    path.display()
                ),
            ));
        }
        let record: SessionRecord = read_json(&path)?;
        if record.schema_version != SESSION_SCHEMA_VERSION {
            return Err(CliError::new(
                "STATE_CORRUPTED",
                format!("unsupported session schema {}", record.schema_version),
            ));
        }
        if record.session_id != session_id {
            return Err(CliError::new(
                "STATE_CORRUPTED",
                format!(
                    "session file {} contains id {}",
                    path.display(),
                    record.session_id
                ),
            ));
        }
        if record.workspace != self.workspace {
            return Err(CliError::new(
                "STATE_CORRUPTED",
                format!(
                    "session {} belongs to {}, expected {}",
                    session_id,
                    record.workspace.display(),
                    self.workspace.display()
                ),
            ));
        }
        Ok(record)
    }

    pub fn new_record(&self, session_id: impl Into<String>) -> SessionRecord {
        let session_id = session_id.into();
        let now = workspace::now_ms();
        SessionRecord {
            schema_version: SESSION_SCHEMA_VERSION,
            session_id: session_id.clone(),
            workspace: self.workspace.clone(),
            status: SessionStatus::Active,
            created_at_unix_ms: now,
            updated_at_unix_ms: now,
            rollback_count: 0,
            last_commit_unix_ms: None,
            dag: SessionDag::new(session_id, self.workspace.clone()),
        }
    }

    pub fn save(&self, record: &SessionRecord) -> CliResult<()> {
        if record.workspace != self.workspace {
            return Err(CliError::new(
                "STATE_CORRUPTED",
                "refusing to save a session for a different workspace",
            ));
        }
        fs::create_dir_all(self.session_dir(&record.session_id))?;
        write_json_atomic(
            &self.session_dir(&record.session_id).join("session.json"),
            record,
        )
    }

    pub fn write_active(&self, record: &SessionRecord) -> CliResult<()> {
        fs::create_dir_all(&self.workspaces_dir)?;
        let pointer = ActivePointer {
            schema_version: 1,
            session_id: record.session_id.clone(),
            workspace: self.workspace.clone(),
        };
        write_json_atomic(&self.active_path, &pointer)
    }

    pub fn clear_active(&self) -> CliResult<()> {
        match fs::remove_file(&self.active_path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(CliError::new("IO_ERROR", error.to_string())),
        }
    }

    pub fn session_dir(&self, session_id: &str) -> PathBuf {
        self.sessions_dir.join(session_id)
    }

    pub fn layout(&self, session_id: &str) -> SessionLayout {
        SessionLayout::new(session_id, self.workspace.clone(), self.state_root.clone())
    }

    pub fn append_event(&self, session_id: &str, value: &serde_json::Value) -> CliResult<()> {
        let path = self.session_dir(session_id).join("events.jsonl");
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let mut file = OpenOptions::new().create(true).append(true).open(path)?;
        serde_json::to_writer(&mut file, value)?;
        file.write_all(b"\n")?;
        Ok(())
    }

    pub fn disk_usage(&self, session_id: &str) -> u64 {
        let mut total = 0;
        for entry in ["steps", "home", "tmp", "cache"] {
            total += directory_size(&self.session_dir(session_id).join(entry));
        }
        total
    }

    pub fn remove_session(&self, session_id: &str) -> CliResult<()> {
        let path = self.session_dir(session_id);
        if !path.exists() {
            return Ok(());
        }
        let canonical = path.canonicalize()?;
        let sessions = self.sessions_dir.canonicalize()?;
        if !canonical.starts_with(&sessions) {
            return Err(CliError::new(
                "STATE_CORRUPTED",
                format!(
                    "refusing to remove {} outside {}",
                    path.display(),
                    sessions.display()
                ),
            ));
        }
        fs::remove_dir_all(canonical)?;
        Ok(())
    }
}

fn read_json<T: DeserializeOwned>(path: &Path) -> CliResult<T> {
    let data = fs::read(path)?;
    Ok(serde_json::from_slice(&data)?)
}

fn write_json_atomic<T: Serialize>(path: &Path, value: &T) -> CliResult<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let temp = path.with_extension(format!("tmp-{}", std::process::id()));
    let data = serde_json::to_vec_pretty(value)?;
    fs::write(&temp, data)?;
    fs::rename(&temp, path)?;
    Ok(())
}

fn directory_size(path: &Path) -> u64 {
    let Ok(entries) = fs::read_dir(path) else {
        return 0;
    };
    let mut total = 0;
    for entry in entries.flatten() {
        let Ok(metadata) = fs::symlink_metadata(entry.path()) else {
            continue;
        };
        if metadata.file_type().is_dir() {
            total += directory_size(&entry.path());
        } else if metadata.file_type().is_file() {
            total += metadata.len();
        }
    }
    total
}
