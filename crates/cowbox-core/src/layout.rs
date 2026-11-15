use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct SessionLayout {
    pub session_id: String,
    pub workspace: PathBuf,
    pub state_root: PathBuf,
    pub session_dir: PathBuf,
}

impl SessionLayout {
    pub fn new(
        session_id: impl Into<String>,
        workspace: impl Into<PathBuf>,
        state_root: impl Into<PathBuf>,
    ) -> Self {
        let session_id = session_id.into();
        let state_root = state_root.into();
        let session_dir = state_root.join("sessions").join(&session_id);
        Self {
            session_id,
            workspace: workspace.into(),
            state_root,
            session_dir,
        }
    }

    pub fn steps_dir(&self) -> PathBuf {
        self.session_dir.join("steps")
    }

    pub fn step_dir(&self, id: u64) -> PathBuf {
        self.steps_dir().join(id.to_string())
    }

    pub fn upper_dir(&self, id: u64) -> PathBuf {
        self.step_dir(id).join("upper")
    }

    pub fn work_dir(&self, id: u64) -> PathBuf {
        self.step_dir(id).join("work")
    }

    pub fn home_dir(&self) -> PathBuf {
        self.session_dir.join("home")
    }

    pub fn tmp_dir(&self) -> PathBuf {
        self.session_dir.join("tmp")
    }

    pub fn cache_dir(&self) -> PathBuf {
        self.session_dir.join("cache")
    }

    pub fn base_manifest_path(&self) -> PathBuf {
        self.session_dir.join("base").join("manifest.jsonl")
    }

    pub fn events_path(&self) -> PathBuf {
        self.session_dir.join("events.jsonl")
    }

    pub fn session_file(&self) -> PathBuf {
        self.session_dir.join("session.json")
    }

    pub fn lock_path(&self) -> PathBuf {
        self.session_dir.join("lock")
    }

    pub fn is_inside_workspace(&self, path: &Path) -> bool {
        path.starts_with(&self.workspace)
    }
}
