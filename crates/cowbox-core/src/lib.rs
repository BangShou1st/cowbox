#![forbid(unsafe_code)]

pub mod error;
pub mod layout;
pub mod model;
pub mod policy;

#[cfg(target_os = "linux")]
pub mod linux;

pub use error::{CoreError, Result};
pub use layout::SessionLayout;
pub use model::{
    Capabilities, ChangeKind, CommitOptions, CommitReport, DiffOptions, EntryKind, ExecOutcome,
    FileChange, WhiteoutKind,
};
pub use policy::ExecPolicy;

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

    fn destroy_session(&self, layout: &SessionLayout) -> Result<()>;
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

    fn destroy_session(&self, _layout: &SessionLayout) -> Result<()> {
        Err(Self::unsupported())
    }
}
