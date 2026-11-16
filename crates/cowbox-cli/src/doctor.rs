#[cfg(not(windows))]
use crate::config::Config;
#[cfg(not(windows))]
use crate::error::CliResult;
#[cfg(not(windows))]
use crate::workspace;
use cowbox_core::Capabilities;
#[cfg(not(windows))]
use cowbox_core::HostCacheKind;
use cowbox_core::NetworkMode;
use serde::{Deserialize, Serialize};
#[cfg(not(windows))]
use std::fs;
#[cfg(not(windows))]
use std::path::Path;
use std::path::PathBuf;
#[cfg(not(windows))]
use std::process::Command;
#[cfg(not(windows))]
use std::time::{SystemTime, UNIX_EPOCH};

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

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DiagnosticStages {
    pub state_disk: bool,
    pub user_namespace: Option<bool>,
    pub network_namespace: Option<bool>,
    pub overlayfs: Option<bool>,
    pub landlock: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DiagnosticFailure {
    pub code: String,
    pub message: String,
    pub exit_code: Option<i32>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RunCheckReport {
    pub attempted: bool,
    pub ready: bool,
    pub network: NetworkMode,
    pub command_started: bool,
    pub exit_code: Option<i32>,
    pub stages: DiagnosticStages,
    pub error: Option<DiagnosticFailure>,
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

#[cfg(not(windows))]
pub fn check_run(
    executable: &Path,
    network: NetworkMode,
    state_disk_ready: bool,
) -> CliResult<RunCheckReport> {
    let mut stages = DiagnosticStages {
        state_disk: state_disk_ready,
        user_namespace: None,
        network_namespace: None,
        overlayfs: None,
        landlock: None,
    };
    if !state_disk_ready {
        return Ok(RunCheckReport {
            attempted: false,
            ready: false,
            network,
            command_started: false,
            exit_code: None,
            stages,
            error: Some(DiagnosticFailure {
                code: "STATE_DISK_NOT_READY".to_string(),
                message: "the state filesystem is not ready for xattr-capable COW state"
                    .to_string(),
                exit_code: Some(1),
            }),
        });
    }

    let root = diagnostic_root()?;
    let result = run_check_in_fixture(executable, &root, network, &mut stages);
    let cleanup_result = remove_diagnostic_tree(&root);
    let mut report = result?;
    cleanup_result?;
    report.stages = stages;
    Ok(report)
}

#[cfg(not(windows))]
fn run_check_in_fixture(
    executable: &Path,
    root: &Path,
    network: NetworkMode,
    stages: &mut DiagnosticStages,
) -> CliResult<RunCheckReport> {
    let workspace = root.join("workspace");
    let state = root.join("state");
    let marker_dir = root.join("markers");
    fs::create_dir_all(workspace.join(".git"))?;
    fs::create_dir_all(&marker_dir)?;
    let marker = marker_dir.join("command-started");
    let script = format!(
        "printf started > {}",
        shell_quote(&marker.to_string_lossy())
    );
    let mut command = Command::new(executable);
    command
        .current_dir(&workspace)
        .env("COWBOX_HOME", &state)
        .env("COWBOX_HELPER_EXE", executable)
        .env("TMPDIR", "/tmp")
        .args([
            "run",
            "--json",
            "--allow-write",
            &marker_dir.to_string_lossy(),
        ]);
    if network.isolates_network() {
        command.args(["--network", "off"]);
    }
    command.args(["--", "sh", "-c", &script]);
    let output = command.output().map_err(|error| {
        crate::error::CliError::new(
            "RUN_CHECK_FAILED",
            format!("cannot start the real sandbox check: {error}"),
        )
    })?;
    let command_started = marker.exists();
    let exit_code = output.status.code();
    let value: serde_json::Value = match serde_json::from_slice(&output.stdout) {
        Ok(value) => value,
        Err(error) => {
            return Ok(failed_run_check(
                network,
                command_started,
                exit_code,
                "RUN_CHECK_INVALID",
                format!(
                    "real sandbox check returned invalid JSON: {error}; stderr={}",
                    String::from_utf8_lossy(&output.stderr).trim()
                ),
                stages,
            ));
        }
    };
    if value["ok"].as_bool() == Some(true) {
        let sandbox_ready = output.status.success();
        stages.user_namespace = Some(true);
        stages.overlayfs = Some(true);
        stages.landlock = Some(sandbox_ready);
        stages.network_namespace = network.isolates_network().then_some(sandbox_ready);
        return Ok(RunCheckReport {
            attempted: true,
            ready: sandbox_ready,
            network,
            command_started,
            exit_code,
            stages: stages.clone(),
            error: if sandbox_ready {
                None
            } else {
                Some(DiagnosticFailure {
                    code: "PROBE_COMMAND_FAILED".to_string(),
                    message: "the sandbox started but the probe command failed".to_string(),
                    exit_code,
                })
            },
        });
    }

    let code = value["error"]["code"]
        .as_str()
        .unwrap_or("RUN_CHECK_FAILED")
        .to_string();
    let message = value["error"]["message"]
        .as_str()
        .unwrap_or("the real sandbox check failed")
        .to_string();
    let error_exit = value["error"]["exit_code"]
        .as_i64()
        .and_then(|code| i32::try_from(code).ok())
        .or(exit_code);
    classify_failure(&code, network, stages);
    Ok(RunCheckReport {
        attempted: true,
        ready: false,
        network,
        command_started,
        exit_code,
        stages: stages.clone(),
        error: Some(DiagnosticFailure {
            code,
            message,
            exit_code: error_exit,
        }),
    })
}

#[cfg(not(windows))]
fn classify_failure(code: &str, network: NetworkMode, stages: &mut DiagnosticStages) {
    match code {
        "USER_NAMESPACE_FAILED" => {
            stages.user_namespace = Some(false);
        }
        "OVERLAYFS_FAILED" => {
            stages.user_namespace = Some(true);
            stages.overlayfs = Some(false);
        }
        "NETWORK_NAMESPACE_FAILED" => {
            stages.user_namespace = Some(true);
            stages.overlayfs = Some(true);
            stages.network_namespace = Some(false);
        }
        "LANDLOCK_FAILED" => {
            stages.user_namespace = Some(true);
            stages.overlayfs = Some(true);
            stages.network_namespace = network.isolates_network().then_some(true);
            stages.landlock = Some(false);
        }
        _ => {}
    }
}

#[cfg(not(windows))]
fn failed_run_check(
    network: NetworkMode,
    command_started: bool,
    exit_code: Option<i32>,
    code: &str,
    message: String,
    stages: &DiagnosticStages,
) -> RunCheckReport {
    RunCheckReport {
        attempted: true,
        ready: false,
        network,
        command_started,
        exit_code,
        stages: stages.clone(),
        error: Some(DiagnosticFailure {
            code: code.to_string(),
            message,
            exit_code,
        }),
    }
}

#[cfg(not(windows))]
fn diagnostic_root() -> CliResult<PathBuf> {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or_default();
    let root = PathBuf::from("/tmp").join(format!("cowbox-doctor-{}-{nanos}", std::process::id()));
    fs::create_dir(&root).map_err(|error| {
        crate::error::CliError::new(
            "RUN_CHECK_FAILED",
            format!(
                "cannot create diagnostic fixture {}: {error}",
                root.display()
            ),
        )
    })?;
    Ok(root)
}

#[cfg(not(windows))]
fn remove_diagnostic_tree(path: &Path) -> CliResult<()> {
    fn make_writable(path: &Path) -> std::io::Result<()> {
        let metadata = fs::symlink_metadata(path)?;
        if metadata.file_type().is_symlink() {
            return Ok(());
        }
        let mode = if metadata.is_dir() { 0o700 } else { 0o600 };
        fs::set_permissions(path, std::os::unix::fs::PermissionsExt::from_mode(mode))?;
        if metadata.is_dir() {
            for entry in fs::read_dir(path)? {
                make_writable(&entry?.path())?;
            }
        }
        Ok(())
    }
    make_writable(path).map_err(|error| {
        crate::error::CliError::new(
            "RUN_CHECK_CLEANUP_FAILED",
            format!(
                "cannot clean diagnostic fixture {}: {error}",
                path.display()
            ),
        )
    })?;
    fs::remove_dir_all(path).map_err(|error| {
        crate::error::CliError::new(
            "RUN_CHECK_CLEANUP_FAILED",
            format!(
                "cannot remove diagnostic fixture {}: {error}",
                path.display()
            ),
        )
    })
}

#[cfg(not(windows))]
fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
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
