use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Duration;

pub const DEFAULT_EXCLUDES: &[&str] = &[
    ".git",
    ".cowbox",
    "target",
    "node_modules",
    ".venv",
    "__pycache__",
    ".next",
    "dist",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostCacheKind {
    Cargo,
    Npm,
    Pip,
}

impl HostCacheKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Cargo => "cargo",
            Self::Npm => "npm",
            Self::Pip => "pip",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostCacheSource {
    pub kind: HostCacheKind,
    pub root: PathBuf,
}

#[derive(Debug, Clone)]
pub struct ExecPolicy {
    pub extra_ro_paths: Vec<PathBuf>,
    pub extra_rw_paths: Vec<PathBuf>,
    pub isolate_caches: bool,
    pub timeout: Option<Duration>,
    pub clean_env: bool,
    pub env: BTreeMap<String, String>,
    pub excluded: Vec<String>,
    pub capture_output: bool,
    pub cwd: Option<PathBuf>,
    pub host_cache_sources: Vec<HostCacheSource>,
}

impl Default for ExecPolicy {
    fn default() -> Self {
        Self {
            extra_ro_paths: Vec::new(),
            extra_rw_paths: Vec::new(),
            isolate_caches: true,
            timeout: Some(Duration::from_secs(30 * 60)),
            clean_env: false,
            env: BTreeMap::new(),
            excluded: DEFAULT_EXCLUDES
                .iter()
                .map(|item| (*item).to_string())
                .collect(),
            capture_output: false,
            cwd: None,
            host_cache_sources: Vec::new(),
        }
    }
}
