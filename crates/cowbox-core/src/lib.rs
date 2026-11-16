#![forbid(unsafe_code)]

pub mod error;
pub mod layout;
pub mod model;
pub mod policy;

#[cfg(target_os = "linux")]
pub mod commit;
#[cfg(target_os = "linux")]
pub mod compact;
#[cfg(target_os = "linux")]
pub mod diff;
#[cfg(target_os = "linux")]
pub mod linux;
#[cfg(target_os = "linux")]
pub mod manifest;
#[cfg(target_os = "linux")]
pub mod whiteout;
#[cfg(target_os = "linux")]
mod xattr;

pub use error::{CoreError, Result};
pub use layout::SessionLayout;
pub use model::{
    Capabilities, ChangeKind, CommitOptions, CommitRecoveryAction, CommitReport, DiffOptions,
    EntryKind, ExecOutcome, FileChange, LayerUsage, PendingRecovery, RecoveryReport, WhiteoutKind,
};
pub use policy::{ExecPolicy, HostCacheKind, HostCacheSource};

use std::ffi::OsString;
use std::path::Path;

pub trait SandboxBackend: Send + Sync {
    fn probe(&self, state_root: &Path) -> Result<Capabilities>;

    fn init_session(&self, layout: &SessionLayout, repo_root: &Path) -> Result<()>;

    fn exec_step(
        &self,
        layout: &SessionLayout,
        parent_layers: &[u64],
        step_id: u64,
        cmd: &[OsString],
        policy: &ExecPolicy,
    ) -> Result<ExecOutcome>;

    fn diff_layers(
        &self,
        layout: &SessionLayout,
        base_layers: &[u64],
        target_layers: &[u64],
        options: &DiffOptions,
    ) -> Result<Vec<FileChange>>;

    fn commit_layers(
        &self,
        layout: &SessionLayout,
        target_layers: &[u64],
        options: &CommitOptions,
    ) -> Result<CommitReport>;

    fn recover_commit(&self, layout: &SessionLayout) -> Result<RecoveryReport>;

    fn inspect_recovery(&self, layout: &SessionLayout) -> Result<Option<PendingRecovery>>;

    fn finalize_commit(&self, layout: &SessionLayout) -> Result<()>;

    fn destroy_session(&self, layout: &SessionLayout) -> Result<()>;

    fn layer_usage(&self, layout: &SessionLayout, logical_layers: &[u64]) -> Result<LayerUsage>;

    /// Restore any source-layer permission changes left by an interrupted
    /// compaction or read transaction. Callers must hold the session lock.
    fn recover_compaction_permissions(&self, layout: &SessionLayout) -> Result<()>;
}

pub fn platform_backend() -> Box<dyn SandboxBackend> {
    #[cfg(target_os = "linux")]
    {
        Box::new(linux::LinuxBackend::new())
    }
    #[cfg(not(target_os = "linux"))]
    {
        Box::new(UnsupportedBackend)
    }
}

pub struct UnsupportedBackend;

impl UnsupportedBackend {
    fn unsupported() -> CoreError {
        CoreError::CowUnsupported {
            path: Path::new("/").to_path_buf(),
            reason:
                "cowbox M1 requires Linux (WSL2 is supported); native Windows is planned for M3"
                    .to_string(),
        }
    }
}

impl SandboxBackend for UnsupportedBackend {
    fn probe(&self, _state_root: &Path) -> Result<Capabilities> {
        Err(Self::unsupported())
    }

    fn init_session(&self, _layout: &SessionLayout, _repo_root: &Path) -> Result<()> {
        Err(Self::unsupported())
    }

    fn exec_step(
        &self,
        _layout: &SessionLayout,
        _parent_layers: &[u64],
        _step_id: u64,
        _cmd: &[OsString],
        _policy: &ExecPolicy,
    ) -> Result<ExecOutcome> {
        Err(Self::unsupported())
    }

    fn diff_layers(
        &self,
        _layout: &SessionLayout,
        _base_layers: &[u64],
        _target_layers: &[u64],
        _options: &DiffOptions,
    ) -> Result<Vec<FileChange>> {
        Err(Self::unsupported())
    }

    fn commit_layers(
        &self,
        _layout: &SessionLayout,
        _target_layers: &[u64],
        _options: &CommitOptions,
    ) -> Result<CommitReport> {
        Err(Self::unsupported())
    }

    fn recover_commit(&self, _layout: &SessionLayout) -> Result<RecoveryReport> {
        Err(Self::unsupported())
    }

    fn inspect_recovery(&self, _layout: &SessionLayout) -> Result<Option<PendingRecovery>> {
        Err(Self::unsupported())
    }

    fn finalize_commit(&self, _layout: &SessionLayout) -> Result<()> {
        Err(Self::unsupported())
    }

    fn destroy_session(&self, _layout: &SessionLayout) -> Result<()> {
        Err(Self::unsupported())
    }

    fn layer_usage(&self, _layout: &SessionLayout, logical_layers: &[u64]) -> Result<LayerUsage> {
        Ok(LayerUsage {
            logical_steps: logical_layers.len(),
            mounted_layers: logical_layers.len().saturating_add(1),
            compact_layers: 0,
            compacted_bytes: 0,
        })
    }

    fn recover_compaction_permissions(&self, _layout: &SessionLayout) -> Result<()> {
        Ok(())
    }
}
