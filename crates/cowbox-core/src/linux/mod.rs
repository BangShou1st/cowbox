use crate::error::CoreError;
use crate::layout::SessionLayout;
use crate::model::{
    Capabilities, CommitOptions, CommitReport, DiffOptions, ExecOutcome, FileChange,
};
use crate::policy::ExecPolicy;
use crate::{Result, SandboxBackend};
use std::ffi::OsString;
use std::path::Path;

pub struct LinuxBackend;

impl LinuxBackend {
    pub fn new() -> Self {
        Self
    }

    fn pending() -> CoreError {
        CoreError::IsolationFailed("Linux backend implementation is pending".to_string())
    }
}

impl Default for LinuxBackend {
    fn default() -> Self {
        Self::new()
    }
}

impl SandboxBackend for LinuxBackend {
    fn probe(&self, _state_root: &Path) -> Result<Capabilities> {
        Err(Self::pending())
    }

    fn init_session(&self, _layout: &SessionLayout, _repo_root: &Path) -> Result<()> {
        Err(Self::pending())
    }

    fn exec_step(
        &self,
        _layout: &SessionLayout,
        _parent_layers: &[u64],
        _step_id: u64,
        _cmd: &[OsString],
        _policy: &ExecPolicy,
    ) -> Result<ExecOutcome> {
        Err(Self::pending())
    }

    fn diff_layers(
        &self,
        _layout: &SessionLayout,
        _base_layers: &[u64],
        _target_layers: &[u64],
        _options: &DiffOptions,
    ) -> Result<Vec<FileChange>> {
        Err(Self::pending())
    }

    fn commit_layers(
        &self,
        _layout: &SessionLayout,
        _target_layers: &[u64],
        _options: &CommitOptions,
    ) -> Result<CommitReport> {
        Err(Self::pending())
    }

    fn destroy_session(&self, _layout: &SessionLayout) -> Result<()> {
        Err(Self::pending())
    }
}
