use cowbox_dag::SessionDag;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;

pub const WIN_SESSION_SCHEMA_VERSION: u32 = 1;
pub const WIN_STORAGE_MODE: &str = "ntfs_materialized_copy";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WinEntryKind {
    File,
    Directory,
}

impl WinEntryKind {
    pub fn is_file(self) -> bool {
        self == Self::File
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WinEntry {
    pub kind: WinEntryKind,
    pub size: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sha256: Option<String>,
    pub readonly: bool,
    pub mtime_unix_ms: Option<i64>,
    pub hardlinks: u32,
}

impl WinEntry {
    pub fn file(
        size: u64,
        sha256: String,
        readonly: bool,
        mtime_unix_ms: Option<i64>,
        hardlinks: u32,
    ) -> Self {
        Self {
            kind: WinEntryKind::File,
            size,
            sha256: Some(sha256),
            readonly,
            mtime_unix_ms,
            hardlinks,
        }
    }

    pub fn directory() -> Self {
        Self {
            kind: WinEntryKind::Directory,
            size: 0,
            sha256: None,
            readonly: false,
            mtime_unix_ms: None,
            hardlinks: 1,
        }
    }

    pub fn content_equals(&self, other: &Self) -> bool {
        self.kind == other.kind
            && self.size == other.size
            && self.sha256 == other.sha256
            && self.readonly == other.readonly
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct WinManifest {
    pub entries: BTreeMap<String, WinEntry>,
}

impl WinManifest {
    pub fn sha256(&self) -> String {
        let bytes = serde_json::to_vec(self).unwrap_or_default();
        crate::win::scan::sha256_bytes(&bytes)
    }

    pub fn files(&self) -> impl Iterator<Item = (&String, &WinEntry)> {
        self.entries
            .iter()
            .filter(|(_, entry)| entry.kind.is_file())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WinStepDelta {
    pub schema_version: u32,
    pub step: u64,
    pub parent: u64,
    pub label: String,
    pub timestamp_unix_ms: i64,
    pub entries: BTreeMap<String, Option<WinEntry>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WinSessionStatus {
    Active,
    Committed,
    Discarded,
}

impl WinSessionStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Committed => "committed",
            Self::Discarded => "discarded",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WinSessionRecord {
    pub schema_version: u32,
    pub storage_mode: String,
    pub session_id: String,
    pub workspace: PathBuf,
    pub state_root: PathBuf,
    pub live_workspace: String,
    pub editor_workspace: String,
    pub status: WinSessionStatus,
    pub created_at_unix_ms: i64,
    pub updated_at_unix_ms: i64,
    pub base_manifest_sha256: String,
    pub rollback_count: u64,
    pub rescue_count: u64,
    pub last_commit_unix_ms: Option<i64>,
    pub last_journal: Option<String>,
    pub dag: SessionDag,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WinChangeKind {
    Added,
    Modified,
    Deleted,
    TypeChanged,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WinRecoveryAction {
    RestoreHost,
    FinalizeCommit,
    AlreadyFinalized,
    AlreadyRestored,
    NoJournal,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WinChange {
    pub rel_path: String,
    pub kind: WinChangeKind,
    pub entry_kind: WinEntryKind,
    pub size_delta: i64,
    pub old_sha256: Option<String>,
    pub new_sha256: Option<String>,
    pub lines_added: Option<u32>,
    pub lines_removed: Option<u32>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct WinDiffSummary {
    pub files: usize,
    pub added: usize,
    pub modified: usize,
    pub deleted: usize,
    pub type_changed: usize,
    pub bytes_added: u64,
    pub bytes_removed: u64,
}

impl WinDiffSummary {
    pub fn from_changes(changes: &[WinChange]) -> Self {
        let mut summary = Self {
            files: changes.len(),
            ..Self::default()
        };
        for change in changes {
            match change.kind {
                WinChangeKind::Added => summary.added += 1,
                WinChangeKind::Modified => summary.modified += 1,
                WinChangeKind::Deleted => summary.deleted += 1,
                WinChangeKind::TypeChanged => summary.type_changed += 1,
            }
            if change.size_delta >= 0 {
                summary.bytes_added += change.size_delta as u64;
            } else {
                summary.bytes_removed += change.size_delta.unsigned_abs();
            }
        }
        summary
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WinPathReport {
    pub path: String,
    pub exists: bool,
    pub filesystem: Option<String>,
    pub available_bytes: Option<u64>,
    pub total_bytes: Option<u64>,
    pub ntfs: bool,
    pub writable: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WinPathTest {
    pub long_paths: bool,
    pub tested_length: usize,
    pub acl_inspection: bool,
    pub ads_copy: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WinDoctorReport {
    pub platform: String,
    pub ready: bool,
    pub storage_mode: String,
    pub workspace: Option<WinPathReport>,
    pub state_root: Option<WinPathReport>,
    pub path_test: WinPathTest,
    pub capabilities: Vec<String>,
    pub blockers: Vec<String>,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WinInitReport {
    pub session_id: String,
    pub workspace: String,
    pub managed_workspace: String,
    pub generation_workspace: String,
    pub state_root: String,
    pub storage_mode: String,
    pub files: usize,
    pub directories: usize,
    pub source_bytes: u64,
    pub estimated_bytes: u64,
    pub available_bytes: u64,
    pub hardlinks: Vec<String>,
    pub excludes: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WinCheckpointReport {
    pub session_id: String,
    pub step: u64,
    pub parent: u64,
    pub head: u64,
    pub created: bool,
    pub added: usize,
    pub modified: usize,
    pub deleted: usize,
    pub bytes_added: u64,
    pub managed_workspace: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WinStepSummary {
    pub step: u64,
    pub parent: Option<u64>,
    pub label: String,
    pub timestamp_unix_ms: u64,
    pub head: bool,
    pub rescue: bool,
    pub changes: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WinLogReport {
    pub session_id: String,
    pub status: String,
    pub head: u64,
    pub workspace: String,
    pub managed_workspace: String,
    pub steps: Vec<WinStepSummary>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WinDiffReport {
    pub session_id: String,
    pub base_step: u64,
    pub step: u64,
    pub head: u64,
    pub changes: Vec<WinChange>,
    pub summary: WinDiffSummary,
    pub uncheckpointed: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WinRollbackReport {
    pub session_id: String,
    pub target: u64,
    pub previous_head: u64,
    pub rescue_step: Option<u64>,
    pub head: u64,
    pub managed_workspace: String,
    pub generation_workspace: String,
    pub materialized_files: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WinUnsupported {
    pub rel_path: String,
    pub code: String,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WinCommitPreview {
    pub session_id: String,
    pub step: u64,
    pub changes: Vec<WinChange>,
    pub summary: WinDiffSummary,
    pub conflicts: Vec<String>,
    pub unsupported: Vec<WinUnsupported>,
    pub host_drift: bool,
    pub bytes_to_write: u64,
    pub can_commit: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WinCommitReport {
    pub session_id: String,
    pub step: u64,
    pub committed: bool,
    pub changes: usize,
    pub journal: Option<String>,
    pub finalized: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WinRecoveryReport {
    pub session_id: String,
    pub journal: Option<String>,
    pub action: WinRecoveryAction,
    pub restored: usize,
    pub finalized: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WinDiscardReport {
    pub session_id: String,
    pub workspace: String,
    pub freed_bytes: u64,
    pub discarded: bool,
    pub uncheckpointed: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WinSessionSummary {
    pub id: String,
    pub workspace: String,
    pub managed_workspace: String,
    pub state_root: String,
    pub status: String,
    pub head: u64,
    pub storage_mode: String,
}
