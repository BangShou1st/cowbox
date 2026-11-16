use crate::NetworkMode;
use crate::error::{CoreError, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunSpec {
    pub session_id: String,
    pub workspace: PathBuf,
    pub merged_dir: PathBuf,
    pub upper_dir: PathBuf,
    pub work_dir: PathBuf,
    pub lower_dirs: Vec<PathBuf>,
    pub home_dir: PathBuf,
    pub tmp_dir: PathBuf,
    pub cache_dir: PathBuf,
    pub cmd: Vec<String>,
    pub cwd: Option<PathBuf>,
    pub env: BTreeMap<String, String>,
    pub clean_env: bool,
    pub isolate_caches: bool,
    pub extra_ro_paths: Vec<PathBuf>,
    pub extra_rw_paths: Vec<PathBuf>,
    pub network: NetworkMode,
    pub cache_mounts: Vec<CacheMountSpec>,
    pub timeout_ms: Option<u64>,
    pub result_path: PathBuf,
    pub ready_path: PathBuf,
    pub go_path: PathBuf,
    pub capture_output: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CacheMountSpec {
    pub tool: String,
    pub source: PathBuf,
    pub destination: PathBuf,
    pub upper_dir: PathBuf,
    pub work_dir: PathBuf,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecResult {
    pub exit_code: i32,
    pub signal: Option<i32>,
    pub duration_ms: u64,
    pub orphans_reaped: usize,
    pub timed_out: bool,
    pub error: Option<String>,
    #[serde(default)]
    pub error_code: Option<String>,
}

impl ExecResult {
    pub fn error(message: impl Into<String>) -> Self {
        Self {
            exit_code: 125,
            signal: None,
            duration_ms: 0,
            orphans_reaped: 0,
            timed_out: false,
            error: Some(message.into()),
            error_code: None,
        }
    }

    pub fn error_with_code(message: impl Into<String>, code: impl Into<String>) -> Self {
        Self {
            error_code: Some(code.into()),
            ..Self::error(message)
        }
    }
}

pub fn spec_path_from_args() -> Result<PathBuf> {
    let mut args = std::env::args_os().skip(1);
    while let Some(arg) = args.next() {
        if arg == "--spec" {
            return args
                .next()
                .map(PathBuf::from)
                .ok_or_else(|| CoreError::Config("--spec requires a path".to_string()));
        }
    }
    Err(CoreError::Config(
        "internal helper requires --spec <path>".to_string(),
    ))
}

pub fn load_spec() -> Result<(PathBuf, RunSpec)> {
    let path = spec_path_from_args()?;
    let bytes = std::fs::read(&path)?;
    let spec = serde_json::from_slice(&bytes)?;
    Ok((path, spec))
}

pub fn write_result(path: &std::path::Path, result: &ExecResult) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let file = std::fs::File::create(path)?;
    serde_json::to_writer_pretty(file, result)?;
    Ok(())
}
