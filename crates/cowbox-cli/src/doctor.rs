#[cfg(not(windows))]
use crate::config::Config;
#[cfg(not(windows))]
use crate::error::CliResult;
#[cfg(not(windows))]
use crate::workspace;
use cowbox_core::Capabilities;
#[cfg(not(windows))]
use cowbox_core::HostCacheKind;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

pub const REQUIRED_LANDLOCK_ABI: i32 = 3;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct HostCachePathReport {
    pub name: String,
    pub path: PathBuf,
    pub available: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct HostCacheReport {
    pub tool: String,
    pub source: PathBuf,
    pub available: bool,
    pub paths: Vec<HostCachePathReport>,
}

#[cfg(not(windows))]
#[derive(Debug)]
pub struct DiagnosticContext {
    pub state_root: PathBuf,
}

#[cfg(not(windows))]
pub fn diagnostic_context() -> CliResult<DiagnosticContext> {
    let cwd = std::env::current_dir()?;
    let workspace = workspace::find_workspace_root(&cwd)?;
    Config::load(&workspace)?;
    let state_root_setting = workspace::state_root_setting()?;
    let state_root = workspace::diagnostic_state_root(&state_root_setting, &workspace)?;
    Ok(DiagnosticContext { state_root })
}

#[cfg(not(windows))]
pub fn host_cache_reports(
    config: &Config,
    workspace: &std::path::Path,
) -> CliResult<Vec<HostCacheReport>> {
    let mut reports = Vec::new();
    for source in config.host_cache_sources(workspace)? {
        let paths = match source.kind {
            HostCacheKind::Cargo => ["registry", "git"]
                .into_iter()
                .map(|name| HostCachePathReport {
                    name: name.to_string(),
                    path: source.root.join(name),
                    available: source.root.join(name).is_dir(),
                })
                .collect::<Vec<_>>(),
            HostCacheKind::Npm => {
                let path = source.root.join("_cacache");
                vec![HostCachePathReport {
                    name: "_cacache".to_string(),
                    available: path.is_dir(),
                    path,
                }]
            }
            HostCacheKind::Pip => vec![HostCachePathReport {
                name: "cache".to_string(),
                path: source.root.clone(),
                available: source.root.is_dir(),
            }],
        };
        reports.push(HostCacheReport {
            tool: source.kind.as_str().to_string(),
            source: source.root,
            available: paths.iter().any(|path| path.available),
            paths,
        });
    }
    Ok(reports)
}

#[cfg(any(not(windows), test))]
pub fn capabilities_ready(capabilities: &Capabilities) -> bool {
    capabilities.user_namespace
        && capabilities.overlayfs
        && capabilities.userxattr
        && landlock_ready(capabilities)
        && state_fs_ready(capabilities)
}

pub fn landlock_ready(capabilities: &Capabilities) -> bool {
    capabilities
        .landlock_abi
        .is_some_and(|abi| abi >= REQUIRED_LANDLOCK_ABI)
}

pub fn state_fs_ready(capabilities: &Capabilities) -> bool {
    capabilities
        .state_fs
        .as_deref()
        .is_some_and(|filesystem| filesystem != "9p")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ready_capabilities() -> Capabilities {
        Capabilities {
            user_namespace: true,
            overlayfs: true,
            userxattr: true,
            landlock_abi: Some(REQUIRED_LANDLOCK_ABI),
            state_fs: Some("ext4".to_string()),
            ..Capabilities::default()
        }
    }

    #[test]
    fn doctor_requires_landlock_abi_three() {
        let mut capabilities = ready_capabilities();
        assert!(capabilities_ready(&capabilities));

        capabilities.landlock_abi = None;
        assert!(!capabilities_ready(&capabilities));

        capabilities.landlock_abi = Some(1);
        assert!(!capabilities_ready(&capabilities));

        capabilities.landlock_abi = Some(2);
        assert!(!capabilities_ready(&capabilities));

        capabilities.landlock_abi = Some(3);
        assert!(capabilities_ready(&capabilities));
    }

    #[test]
    fn doctor_requires_a_diagnosable_non_9p_state_filesystem() {
        let mut capabilities = ready_capabilities();
        capabilities.state_fs = None;
        assert!(!capabilities_ready(&capabilities));

        capabilities.state_fs = Some("9p".to_string());
        assert!(!capabilities_ready(&capabilities));

        capabilities.state_fs = Some("ext4".to_string());
        assert!(capabilities_ready(&capabilities));
    }
}
