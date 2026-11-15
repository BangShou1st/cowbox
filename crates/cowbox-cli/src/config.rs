use crate::error::{CliError, CliResult};
use crate::workspace::resolve_path;
use cowbox_core::policy::DEFAULT_EXCLUDES;
use cowbox_core::{CommitOptions, DiffOptions, ExecPolicy};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

pub const CONFIG_FILE: &str = ".cowbox.toml";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    #[serde(default = "default_version")]
    pub version: u32,
    #[serde(default)]
    pub sandbox: SandboxConfig,
    #[serde(default)]
    pub diff: DiffConfig,
    #[serde(default)]
    pub commit: CommitConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SandboxConfig {
    #[serde(default = "default_timeout")]
    pub timeout: String,
    #[serde(default = "default_true")]
    pub isolate_caches: bool,
    #[serde(default)]
    pub clean_env: bool,
    #[serde(default)]
    pub extra_ro_paths: Vec<PathBuf>,
    #[serde(default)]
    pub extra_rw_paths: Vec<PathBuf>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DiffConfig {
    #[serde(default = "default_true")]
    pub line_stats: bool,
    #[serde(default = "default_max_line_stat_files")]
    pub max_line_stat_files: usize,
    #[serde(default = "default_max_line_stat_bytes")]
    pub max_line_stat_bytes: u64,
    #[serde(default = "default_excluded")]
    pub excluded: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommitConfig {
    #[serde(default = "default_true")]
    pub require_clean_host: bool,
}

#[derive(Debug, Default)]
pub struct RunPolicyOverrides {
    pub allow_read: Vec<PathBuf>,
    pub allow_write: Vec<PathBuf>,
    pub env: Vec<String>,
    pub clean_env: bool,
    pub timeout: Option<String>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            version: 1,
            sandbox: SandboxConfig::default(),
            diff: DiffConfig::default(),
            commit: CommitConfig::default(),
        }
    }
}

impl Default for SandboxConfig {
    fn default() -> Self {
        Self {
            timeout: default_timeout(),
            isolate_caches: true,
            clean_env: false,
            extra_ro_paths: Vec::new(),
            extra_rw_paths: Vec::new(),
        }
    }
}

impl SandboxConfig {
    pub fn timeout_duration(&self) -> CliResult<Option<Duration>> {
        parse_timeout(&self.timeout)
    }
}

impl Default for DiffConfig {
    fn default() -> Self {
        Self {
            line_stats: true,
            max_line_stat_files: default_max_line_stat_files(),
            max_line_stat_bytes: default_max_line_stat_bytes(),
            excluded: default_excluded(),
        }
    }
}

impl Default for CommitConfig {
    fn default() -> Self {
        Self {
            require_clean_host: true,
        }
    }
}

impl Config {
    pub fn load(workspace: &Path) -> CliResult<Self> {
        let path = workspace.join(CONFIG_FILE);
        if !path.exists() {
            return Ok(Self::default());
        }
        let text = std::fs::read_to_string(&path)?;
        let config: Config = toml::from_str(&text)?;
        if config.version != 1 {
            return Err(CliError::new(
                "CONFIG_ERROR",
                format!("unsupported .cowbox.toml version {}", config.version),
            )
            .hint("delete the file or run `cowbox init --force` to regenerate it")
            .exit(2));
        }
        config.validate_timeout()?;
        Ok(config)
    }

    pub fn validate_timeout(&self) -> CliResult<()> {
        let _ = self.sandbox.timeout_duration()?;
        Ok(())
    }

    pub fn write_template(workspace: &Path, force: bool) -> CliResult<PathBuf> {
        let path = workspace.join(CONFIG_FILE);
        if path.exists() && !force {
            return Err(CliError::new(
                "CONFIG_EXISTS",
                format!("{} already exists", path.display()),
            )
            .hint("use `cowbox init --force` to overwrite it")
            .exit(2));
        }
        let template = format!(
            r#"version = 1

[sandbox]
timeout = "30m"
isolate_caches = true
clean_env = false
extra_ro_paths = []
extra_rw_paths = []

[diff]
line_stats = true
max_line_stat_files = 500
max_line_stat_bytes = 1048576
excluded = [{excluded}]

[commit]
require_clean_host = true
"#,
            excluded = default_excluded()
                .iter()
                .map(|item| format!("{item:?}"))
                .collect::<Vec<_>>()
                .join(", ")
        );
        let temp = path.with_extension("toml.tmp");
        std::fs::write(&temp, template)?;
        std::fs::rename(&temp, &path)?;
        Ok(path)
    }

    pub fn to_exec_policy(
        &self,
        workspace: &Path,
        overrides: &RunPolicyOverrides,
        capture_output: bool,
    ) -> CliResult<ExecPolicy> {
        let timeout = match overrides.timeout.as_deref() {
            Some(value) => parse_timeout(value)?,
            None => self.sandbox.timeout_duration()?,
        };
        let mut extra_ro_paths = self
            .sandbox
            .extra_ro_paths
            .iter()
            .map(|path| resolve_path(workspace, path))
            .collect::<Vec<_>>();
        extra_ro_paths.extend(
            overrides
                .allow_read
                .iter()
                .map(|path| resolve_path(workspace, path)),
        );

        let mut extra_rw_paths = self
            .sandbox
            .extra_rw_paths
            .iter()
            .map(|path| resolve_path(workspace, path))
            .collect::<Vec<_>>();
        extra_rw_paths.extend(
            overrides
                .allow_write
                .iter()
                .map(|path| resolve_path(workspace, path)),
        );

        let mut env_map = BTreeMap::new();
        for item in &overrides.env {
            let (key, value) = item.split_once('=').ok_or_else(|| {
                CliError::new("USAGE", format!("--env expects KEY=VALUE, got {item:?}")).exit(2)
            })?;
            if key.is_empty() {
                return Err(CliError::new("USAGE", "--env key cannot be empty").exit(2));
            }
            env_map.insert(key.to_string(), value.to_string());
        }

        Ok(ExecPolicy {
            extra_ro_paths,
            extra_rw_paths,
            isolate_caches: self.sandbox.isolate_caches,
            timeout,
            clean_env: overrides.clean_env || self.sandbox.clean_env,
            env: env_map,
            excluded: self.diff.excluded.clone(),
            capture_output,
            cwd: None,
        })
    }

    pub fn to_diff_options(&self, include_excluded: bool) -> DiffOptions {
        DiffOptions {
            include_excluded,
            line_stats: self.diff.line_stats,
            max_line_stat_files: self.diff.max_line_stat_files,
            max_line_stat_bytes: self.diff.max_line_stat_bytes,
            excluded: self.diff.excluded.clone(),
        }
    }

    pub fn to_commit_options(
        &self,
        force: bool,
        dry_run: bool,
        include_excluded: bool,
    ) -> CommitOptions {
        CommitOptions {
            force: force || !self.commit.require_clean_host,
            dry_run,
            include_excluded,
            excluded: self.diff.excluded.clone(),
        }
    }
}

pub fn parse_timeout(value: &str) -> CliResult<Option<Duration>> {
    let value = value.trim();
    if value.is_empty() || value == "0" {
        return Ok(None);
    }
    humantime::parse_duration(value).map(Some).map_err(|error| {
        CliError::new(
            "CONFIG_ERROR",
            format!("invalid timeout {value:?}: {error}"),
        )
        .exit(2)
    })
}

fn default_version() -> u32 {
    1
}

fn default_timeout() -> String {
    "30m".to_string()
}

fn default_true() -> bool {
    true
}

fn default_max_line_stat_files() -> usize {
    500
}

fn default_max_line_stat_bytes() -> u64 {
    1024 * 1024
}

fn default_excluded() -> Vec<String> {
    DEFAULT_EXCLUDES
        .iter()
        .map(|item| (*item).to_string())
        .collect()
}
