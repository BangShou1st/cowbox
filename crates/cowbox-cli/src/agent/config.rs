use crate::config::{AgentConfig, AgentMode};
use crate::error::{CliError, CliResult};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct GlobalConfig {
    pub wsl: WslConfig,
    pub agent: AgentConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct WslConfig {
    pub distro: String,
    pub linux_binary: String,
}

impl Default for WslConfig {
    fn default() -> Self {
        Self {
            distro: "Ubuntu".to_string(),
            linux_binary: "~/.local/bin/cowbox".to_string(),
        }
    }
}

pub fn home_dir() -> CliResult<PathBuf> {
    #[cfg(windows)]
    let result = std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME"));
    #[cfg(not(windows))]
    let result = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE"));
    result.map(PathBuf::from).ok_or_else(|| {
        CliError::new(
            "HOME_UNAVAILABLE",
            "cannot determine the user home directory",
        )
    })
}

pub fn config_dir() -> CliResult<PathBuf> {
    #[cfg(windows)]
    {
        if let Some(app_data) = std::env::var_os("APPDATA") {
            Ok(PathBuf::from(app_data).join("cowbox"))
        } else {
            Ok(home_dir()?.join("AppData").join("Roaming").join("cowbox"))
        }
    }
    #[cfg(not(windows))]
    {
        if let Some(xdg) = std::env::var_os("XDG_CONFIG_HOME") {
            Ok(PathBuf::from(xdg).join("cowbox"))
        } else {
            Ok(home_dir()?.join(".config").join("cowbox"))
        }
    }
}

pub fn global_config_path() -> CliResult<PathBuf> {
    Ok(config_dir()?.join("config.toml"))
}

pub fn load_global() -> CliResult<GlobalConfig> {
    let path = global_config_path()?;
    if !path.exists() {
        return Ok(GlobalConfig::default());
    }
    let text = fs::read_to_string(&path)?;
    toml::from_str(&text).map_err(|error| {
        CliError::new(
            "CONFIG_ERROR",
            format!("invalid global config {}: {error}", path.display()),
        )
        .exit(2)
    })
}

pub fn save_global(config: &GlobalConfig) -> CliResult<()> {
    let path = global_config_path()?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let text = toml::to_string_pretty(config).map_err(|error| {
        CliError::new(
            "CONFIG_ERROR",
            format!("cannot serialize global config: {error}"),
        )
    })?;
    let temp = path.with_extension(format!("toml.tmp-{}", std::process::id()));
    fs::write(&temp, text)?;
    if path.exists() {
        fs::remove_file(&path)?;
    }
    fs::rename(&temp, &path)?;
    Ok(())
}

pub fn set_global_mode(mode: AgentMode) -> CliResult<()> {
    let mut config = load_global()?;
    config.agent.mode = mode;
    save_global(&config)
}
