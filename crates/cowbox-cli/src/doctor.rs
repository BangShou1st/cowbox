#[cfg(not(windows))]
use crate::config::Config;
#[cfg(not(windows))]
use crate::error::CliResult;
#[cfg(not(windows))]
use crate::workspace;
use cowbox_core::Capabilities;
#[cfg(not(windows))]
use std::path::PathBuf;

pub const REQUIRED_LANDLOCK_ABI: i32 = 3;

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
