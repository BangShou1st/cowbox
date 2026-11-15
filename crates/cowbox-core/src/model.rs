use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum EntryKind {
    File,
    Directory,
    Symlink,
    Other,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WhiteoutKind {
    CharDevice,
    Xattr,
    #[default]
    Unknown,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ChangeKind {
    Added,
    Modified,
    Deleted,
    SymlinkUpdated { target: PathBuf },
    TypeChanged { from: EntryKind, to: EntryKind },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FileChange {
    pub rel_path: PathBuf,
    pub kind: ChangeKind,
    pub entry_kind: EntryKind,
    pub size_delta: i64,
    pub lines_added: Option<u32>,
    pub lines_removed: Option<u32>,
    pub mode: Option<u32>,
    pub excluded: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Capabilities {
    pub os: String,
    pub kernel: String,
    pub wsl: bool,
    pub user_namespace: bool,
    pub overlayfs: bool,
    pub userxattr: bool,
    pub landlock_abi: Option<i32>,
    pub whiteout_kind: WhiteoutKind,
    pub state_fs: Option<String>,
    pub notes: Vec<String>,
}

impl Default for Capabilities {
    fn default() -> Self {
        Self {
            os: std::env::consts::OS.to_string(),
            kernel: String::new(),
            wsl: false,
            user_namespace: false,
            overlayfs: false,
            userxattr: false,
            landlock_abi: None,
            whiteout_kind: WhiteoutKind::Unknown,
            state_fs: None,
            notes: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ExecOutcome {
    pub exit_code: i32,
    pub signal: Option<i32>,
    pub duration_ms: u64,
    pub orphans_reaped: usize,
    pub timed_out: bool,
    pub stdout: String,
    pub stderr: String,
    pub stdout_truncated: bool,
    pub stderr_truncated: bool,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub struct DiffOptions {
    pub include_excluded: bool,
    pub line_stats: bool,
    pub max_line_stat_files: usize,
    pub max_line_stat_bytes: u64,
}

impl Default for DiffOptions {
    fn default() -> Self {
        Self {
            include_excluded: false,
            line_stats: true,
            max_line_stat_files: 500,
            max_line_stat_bytes: 1024 * 1024,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct CommitOptions {
    pub force: bool,
    pub dry_run: bool,
    pub include_excluded: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CommitReport {
    pub changes: Vec<FileChange>,
    pub committed: bool,
    pub conflicts: Vec<PathBuf>,
    pub journal: Option<PathBuf>,
}
