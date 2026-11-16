use crate::error::{CliError, CliResult};
use crate::workspace::resolve_path;
use clap::ValueEnum;
use cowbox_core::policy::DEFAULT_EXCLUDES;
use cowbox_core::{
    CommitOptions, DiffOptions, ExecPolicy, HostCacheKind, HostCacheSource, NetworkMode,
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

pub const CONFIG_FILE: &str = ".cowbox.toml";

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PolicySource {
    Cli,
    Project,
    Default,
    Legacy,
}

impl PolicySource {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Cli => "cli",
            Self::Project => "project",
            Self::Default => "default",
            Self::Legacy => "legacy",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResolvedNetworkPolicy {
    pub mode: NetworkMode,
    pub source: PolicySource,
}

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
    #[serde(default)]
    pub agent: Option<AgentConfig>,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq, ValueEnum)]
#[serde(rename_all = "snake_case")]
pub enum AgentMode {
    #[default]
    Ask,
    Explicit,
    Auto,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct AgentConfig {
    pub mode: AgentMode,
    pub require_confirmation: bool,
    pub triggers: Vec<String>,
    pub max_files_without_prompt: usize,
}

impl Default for AgentConfig {
    fn default() -> Self {
        Self {
            mode: AgentMode::Ask,
            require_confirmation: true,
            triggers: vec![
                "destructive_shell".to_string(),
                "database_migration".to_string(),
                "dependency_upgrade".to_string(),
                "multi_file_refactor".to_string(),
            ],
            max_files_without_prompt: 5,
        }
    }
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
    pub network: Option<NetworkMode>,
    #[serde(default)]
    pub extra_ro_paths: Vec<PathBuf>,
    #[serde(default)]
    pub extra_rw_paths: Vec<PathBuf>,
    #[serde(default)]
    pub host_caches: HostCachesConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(untagged)]
pub enum HostCacheSetting {
    Enabled(bool),
    Path(PathBuf),
}

impl Default for HostCacheSetting {
    fn default() -> Self {
        Self::Enabled(false)
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct HostCachesConfig {
    pub cargo: HostCacheSetting,
    pub npm: HostCacheSetting,
    pub pip: HostCacheSetting,
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
    pub network: Option<NetworkMode>,
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
            agent: None,
        }
    }
}

impl Default for SandboxConfig {
    fn default() -> Self {
        Self {
            timeout: default_timeout(),
            isolate_caches: true,
            clean_env: false,
            network: None,
            extra_ro_paths: Vec::new(),
            extra_rw_paths: Vec::new(),
            host_caches: HostCachesConfig::default(),
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

    pub fn network_policy(&self, cli: Option<NetworkMode>) -> ResolvedNetworkPolicy {
        if let Some(mode) = cli {
            return ResolvedNetworkPolicy {
                mode,
                source: PolicySource::Cli,
            };
        }
        if let Some(mode) = self.sandbox.network {
            return ResolvedNetworkPolicy {
                mode,
                source: PolicySource::Project,
            };
        }
        ResolvedNetworkPolicy {
            mode: NetworkMode::Host,
            source: PolicySource::Default,
        }
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
network = "host"
extra_ro_paths = []
extra_rw_paths = []

[sandbox.host_caches]
# false uses only the isolated session cache. true selects the default host
# cache. A path selects a custom cache root. Only data subdirectories are
# mounted; host credentials and tool configuration remain isolated.
cargo = false
npm = false
pip = false

[diff]
line_stats = true
max_line_stat_files = 500
max_line_stat_bytes = 1048576
excluded = [{excluded}]

[commit]
require_clean_host = true

[agent]
mode = "ask"
require_confirmation = true
triggers = ["destructive_shell", "database_migration", "dependency_upgrade", "multi_file_refactor"]
max_files_without_prompt = 5
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
            network: self.network_policy(overrides.network).mode,
            isolate_caches: self.sandbox.isolate_caches,
            timeout,
            clean_env: overrides.clean_env || self.sandbox.clean_env,
            env: env_map,
            excluded: self.diff.excluded.clone(),
            capture_output,
            cwd: None,
            host_cache_sources: self.sandbox.host_caches.sources(workspace)?,
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

    pub fn host_cache_sources(&self, workspace: &Path) -> CliResult<Vec<HostCacheSource>> {
        self.sandbox.host_caches.sources(workspace)
    }
}

impl HostCachesConfig {
    pub fn sources(&self, workspace: &Path) -> CliResult<Vec<HostCacheSource>> {
        let mut sources = Vec::new();
        self.push_source(
            &mut sources,
            HostCacheKind::Cargo,
            &self.cargo,
            workspace,
            "cargo",
        )?;
        self.push_source(
            &mut sources,
            HostCacheKind::Npm,
            &self.npm,
            workspace,
            "npm",
        )?;
        self.push_source(
            &mut sources,
            HostCacheKind::Pip,
            &self.pip,
            workspace,
            "pip",
        )?;
        Ok(sources)
    }

    fn push_source(
        &self,
        sources: &mut Vec<HostCacheSource>,
        kind: HostCacheKind,
        setting: &HostCacheSetting,
        workspace: &Path,
        label: &str,
    ) -> CliResult<()> {
        let root = match setting {
            HostCacheSetting::Enabled(false) => return Ok(()),
            HostCacheSetting::Enabled(true) => {
                let home = host_home()?;
                match kind {
                    HostCacheKind::Cargo => home.join(".cargo"),
                    HostCacheKind::Npm => home.join(".npm"),
                    HostCacheKind::Pip => home.join(".cache").join("pip"),
                }
            }
            HostCacheSetting::Path(path) if path.as_os_str().is_empty() => {
                return Err(CliError::new(
                    "CONFIG_ERROR",
                    format!("{label} host cache path cannot be empty"),
                )
                .exit(2));
            }
            HostCacheSetting::Path(path) => resolve_path(workspace, path),
        };
        sources.push(HostCacheSource { kind, root });
        Ok(())
    }
}

fn host_home() -> CliResult<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .ok_or_else(|| {
            CliError::new(
                "HOME_UNAVAILABLE",
                "host cache setting `true` requires HOME or USERPROFILE",
            )
            .exit(2)
        })
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_cache_settings_accept_boolean_and_path_forms() {
        let config: Config = toml::from_str(
            r#"version = 1

[sandbox.host_caches]
cargo = true
npm = "/var/cache/npm"
pip = false
"#,
        )
        .expect("config");
        assert_eq!(
            config.sandbox.host_caches.cargo,
            HostCacheSetting::Enabled(true)
        );
        assert_eq!(
            config.sandbox.host_caches.npm,
            HostCacheSetting::Path(PathBuf::from("/var/cache/npm"))
        );
        assert_eq!(
            config.sandbox.host_caches.pip,
            HostCacheSetting::Enabled(false)
        );
    }

    #[test]
    fn network_policy_precedence_is_cli_then_project_then_host() {
        let mut config = Config::default();
        let fallback = config.network_policy(None);
        assert_eq!(fallback.mode, NetworkMode::Host);
        assert_eq!(fallback.source, PolicySource::Default);

        config.sandbox.network = Some(NetworkMode::Off);
        let project = config.network_policy(None);
        assert_eq!(project.mode, NetworkMode::Off);
        assert_eq!(project.source, PolicySource::Project);

        let cli = config.network_policy(Some(NetworkMode::Host));
        assert_eq!(cli.mode, NetworkMode::Host);
        assert_eq!(cli.source, PolicySource::Cli);
    }

    #[cfg(not(windows))]
    #[test]
    fn host_caches_are_disabled_by_default() {
        let config = Config::default();
        assert!(
            config
                .host_cache_sources(Path::new("/tmp/workspace"))
                .expect("sources")
                .is_empty()
        );
    }
}
