mod env;
mod helper;
mod landlock;
mod mount;
mod probe;
mod spec;
mod supervisor;

pub use helper::sandbox_exec_main;
pub use supervisor::supervisor_main;

use crate::SandboxBackend;
use crate::commit;
use crate::compact::{self, MountLayer};
use crate::diff;
use crate::error::{CoreError, Result};
use crate::layout::SessionLayout;
use crate::manifest;
use crate::model::{
    Capabilities, CommitOptions, CommitReport, DiffOptions, ExecOutcome, FileChange, LayerUsage,
};
use crate::policy::ExecPolicy;
use rustix::process::{Pid, Signal, getgid, getuid, kill_process_group};
use spec::{ExecResult, RunSpec};
use std::ffi::OsString;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

const CAPTURE_LIMIT: usize = 1024 * 1024;

pub struct LinuxBackend;

impl LinuxBackend {
    pub fn new() -> Self {
        Self
    }
}

impl Default for LinuxBackend {
    fn default() -> Self {
        Self::new()
    }
}

impl SandboxBackend for LinuxBackend {
    fn probe(&self, state_root: &Path) -> Result<Capabilities> {
        probe::probe(state_root)
    }

    fn init_session(&self, layout: &SessionLayout, repo_root: &Path) -> Result<()> {
        if layout.state_root.starts_with(&layout.workspace) {
            return Err(CoreError::Config(
                "the state directory must not be inside the workspace".to_string(),
            ));
        }
        if !repo_root.is_dir() {
            return Err(CoreError::Config(format!(
                "workspace {} is not a directory",
                repo_root.display()
            )));
        }
        fs::create_dir_all(layout.steps_dir())?;
        fs::create_dir_all(layout.home_dir())?;
        fs::create_dir_all(layout.tmp_dir())?;
        fs::create_dir_all(layout.cache_dir())?;
        manifest::capture_base_manifest_file(repo_root, &layout.base_manifest_path())?;
        Ok(())
    }

    fn exec_step(
        &self,
        layout: &SessionLayout,
        parent_layers: &[u64],
        step_id: u64,
        cmd: &[OsString],
        policy: &ExecPolicy,
    ) -> Result<ExecOutcome> {
        let _span = tracing::info_span!(
            "exec_step",
            session = %layout.session_id,
            step = step_id,
            command = %cmd.first().map(|arg| arg.to_string_lossy()).unwrap_or_default()
        )
        .entered();
        if cmd.is_empty() {
            return Err(CoreError::Config("no command supplied".to_string()));
        }
        let step_dir = layout.step_dir(step_id);
        if step_dir.exists() {
            return Err(CoreError::StateCorrupted {
                step: step_id,
                reason: format!("step directory {} already exists", step_dir.display()),
            });
        }
        let mount_layers = compact::prepare_mount_layers(layout, step_id, parent_layers)?;
        fs::create_dir_all(layout.upper_dir(step_id))?;
        fs::create_dir_all(layout.work_dir(step_id))?;
        fs::create_dir_all(layout.home_dir())?;
        fs::create_dir_all(layout.tmp_dir())?;
        fs::create_dir_all(layout.cache_dir())?;

        let (alias_dir, lower_dirs) = match build_layer_aliases(layout, step_id, &mount_layers) {
            Ok(value) => value,
            Err(error) => {
                let _ = fs::remove_dir_all(&step_dir);
                return Err(error);
            }
        };
        let spec = RunSpec {
            session_id: layout.session_id.clone(),
            workspace: layout.workspace.clone(),
            merged_dir: step_dir.join("merged"),
            upper_dir: layout.upper_dir(step_id),
            work_dir: layout.work_dir(step_id),
            lower_dirs,
            home_dir: layout.home_dir(),
            tmp_dir: layout.tmp_dir(),
            cache_dir: layout.cache_dir(),
            cmd: cmd
                .iter()
                .map(|arg| arg.to_string_lossy().into_owned())
                .collect(),
            cwd: policy.cwd.clone(),
            env: policy.env.clone(),
            clean_env: policy.clean_env,
            isolate_caches: policy.isolate_caches,
            extra_ro_paths: policy.extra_ro_paths.clone(),
            extra_rw_paths: policy.extra_rw_paths.clone(),
            timeout_ms: policy
                .timeout
                .map(|timeout| u64::try_from(timeout.as_millis()).unwrap_or(u64::MAX)),
            result_path: step_dir.join("result.json"),
            ready_path: step_dir.join("ready"),
            go_path: step_dir.join("go"),
            capture_output: policy.capture_output,
        };
        let spec_path = step_dir.join("spec.json");
        if let Err(error) = fs::write(&spec_path, serde_json::to_vec_pretty(&spec)?) {
            let _ = fs::remove_dir_all(&step_dir);
            let _ = fs::remove_dir_all(&alias_dir);
            return Err(error.into());
        }

        let exe = helper_exe()?;
        let mut command = Command::new(exe);
        command
            .arg("__sandbox-exec")
            .arg("--spec")
            .arg(&spec_path)
            .stdin(Stdio::inherit());
        if policy.capture_output {
            command.stdout(Stdio::piped()).stderr(Stdio::piped());
        } else {
            command.stdout(Stdio::inherit()).stderr(Stdio::inherit());
        }
        command.process_group(0);
        let mut child = match command.spawn() {
            Ok(child) => child,
            Err(error) => {
                let _ = fs::remove_dir_all(&step_dir);
                let _ = fs::remove_dir_all(&alias_dir);
                return Err(CoreError::IsolationFailed(format!(
                    "cannot spawn cowbox helper: {error}"
                )));
            }
        };

        if let Err(error) = handshake(&mut child, &spec) {
            let _ = kill_process_group(Pid::from_child(&child), Signal::KILL);
            let _ = child.wait();
            let _ = fs::remove_dir_all(&step_dir);
            let _ = fs::remove_dir_all(&alias_dir);
            return Err(error);
        }

        let (status, stdout, stderr, stdout_truncated, stderr_truncated) = if policy.capture_output
        {
            let output = child.wait_with_output()?;
            let (stdout, stdout_truncated) = capture_output(output.stdout);
            let (stderr, stderr_truncated) = capture_output(output.stderr);
            (
                output.status,
                stdout,
                stderr,
                stdout_truncated,
                stderr_truncated,
            )
        } else {
            let status = child.wait()?;
            (status, String::new(), String::new(), false, false)
        };

        let result = match read_result(&spec.result_path) {
            Ok(result) => result,
            Err(error) => {
                let _ = fs::remove_dir_all(&step_dir);
                let _ = fs::remove_dir_all(&alias_dir);
                return Err(error);
            }
        };
        if let Some(error) = result.error {
            let _ = fs::remove_dir_all(&step_dir);
            let _ = fs::remove_dir_all(&alias_dir);
            return Err(CoreError::IsolationFailed(error));
        }

        let _ = fs::remove_dir_all(&spec.merged_dir);
        let _ = fs::remove_dir_all(&spec.work_dir);
        let _ = fs::remove_dir_all(&alias_dir);
        fs::set_permissions(&step_dir, fs::Permissions::from_mode(0o500))?;

        let exit_code = if result.exit_code != 0 {
            result.exit_code
        } else {
            status
                .code()
                .unwrap_or_else(|| 128 + status.signal().unwrap_or(0))
        };
        Ok(ExecOutcome {
            exit_code,
            signal: result.signal,
            duration_ms: result.duration_ms,
            orphans_reaped: result.orphans_reaped,
            timed_out: result.timed_out,
            stdout,
            stderr,
            stdout_truncated,
            stderr_truncated,
        })
    }

    fn diff_layers(
        &self,
        layout: &SessionLayout,
        base_layers: &[u64],
        target_layers: &[u64],
        options: &DiffOptions,
    ) -> Result<Vec<FileChange>> {
        diff::compute_diff(layout, base_layers, target_layers, options)
    }

    fn commit_layers(
        &self,
        layout: &SessionLayout,
        target_layers: &[u64],
        options: &CommitOptions,
    ) -> Result<CommitReport> {
        commit::commit_layers(layout, target_layers, options)
    }

    fn recover_commit(&self, layout: &SessionLayout) -> Result<crate::RecoveryReport> {
        commit::recover_latest(layout)
    }

    fn inspect_recovery(&self, layout: &SessionLayout) -> Result<Option<crate::PendingRecovery>> {
        commit::inspect_recovery(layout)
    }

    fn finalize_commit(&self, layout: &SessionLayout) -> Result<()> {
        commit::finalize_commit(layout)
    }

    fn destroy_session(&self, layout: &SessionLayout) -> Result<()> {
        if !layout
            .session_dir
            .starts_with(layout.state_root.join("sessions"))
        {
            return Err(CoreError::Config(
                "refusing to delete a session outside the state root".to_string(),
            ));
        }
        if layout.session_dir.exists() {
            make_tree_writable(&layout.session_dir)?;
            fs::remove_dir_all(&layout.session_dir)?;
        }
        Ok(())
    }

    fn layer_usage(&self, layout: &SessionLayout, logical_layers: &[u64]) -> Result<LayerUsage> {
        compact::inspect_layer_usage(layout, logical_layers)
    }
}

fn helper_exe() -> Result<PathBuf> {
    if let Some(path) = std::env::var_os("COWBOX_HELPER_EXE") {
        return Ok(PathBuf::from(path));
    }
    Ok(std::env::current_exe()?)
}

fn build_layer_aliases(
    layout: &SessionLayout,
    step_id: u64,
    mount_layers: &[MountLayer],
) -> Result<(PathBuf, Vec<PathBuf>)> {
    if mount_layers.len() > compact::MAX_MOUNT_LAYERS {
        return Err(CoreError::Config(format!(
            "session has {} physical layers; at most {} can be mounted even after compaction.",
            mount_layers.len(),
            compact::MAX_MOUNT_LAYERS
        )));
    }

    let alias_dir = compact::alias_dir(layout, step_id);
    if alias_dir.exists() {
        fs::remove_dir_all(&alias_dir)?;
    }
    fs::create_dir_all(&alias_dir)?;
    fs::set_permissions(&alias_dir, fs::Permissions::from_mode(0o700))?;

    let mut lower_dirs = Vec::with_capacity(mount_layers.len() + 1);
    for layer in mount_layers.iter().rev() {
        let alias = alias_dir.join(&layer.alias);
        if let Err(error) = std::os::unix::fs::symlink(&layer.upper_dir, &alias) {
            let _ = fs::remove_dir_all(&alias_dir);
            return Err(error.into());
        }
        lower_dirs.push(alias);
    }
    lower_dirs.push(layout.workspace.clone());

    let option_bytes = lower_dirs
        .iter()
        .filter_map(|path| path.to_str())
        .map(|path| path.len() + 1)
        .sum::<usize>();
    if option_bytes > compact::MAX_LOWERDIR_BYTES {
        let _ = fs::remove_dir_all(&alias_dir);
        return Err(CoreError::Config(format!(
            "lowerdir option would be {option_bytes} bytes; limit is {}. Layer compaction could not reduce it.",
            compact::MAX_LOWERDIR_BYTES
        )));
    }
    Ok((alias_dir, lower_dirs))
}

fn handshake(child: &mut Child, spec: &RunSpec) -> Result<()> {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Ok(value) = fs::read_to_string(&spec.ready_path) {
            if value.contains("ready") {
                break;
            }
        }
        if let Some(status) = child.try_wait()? {
            return Err(CoreError::IsolationFailed(format!(
                "cowbox helper exited before the namespace handshake: {status:?}"
            )));
        }
        if Instant::now() >= deadline {
            return Err(CoreError::IsolationFailed(
                "timed out waiting for the namespace handshake".to_string(),
            ));
        }
        thread::sleep(Duration::from_millis(10));
    }

    let pid = child.id();
    let uid = getuid().as_raw();
    let gid = getgid().as_raw();
    let _ = fs::write(format!("/proc/{pid}/setgroups"), "deny\n");
    fs::write(format!("/proc/{pid}/uid_map"), format!("0 {uid} 1\n")).map_err(|error| {
        CoreError::IsolationFailed(format!("cannot write uid_map for helper: {error}"))
    })?;
    fs::write(format!("/proc/{pid}/gid_map"), format!("0 {gid} 1\n")).map_err(|error| {
        CoreError::IsolationFailed(format!("cannot write gid_map for helper: {error}"))
    })?;
    fs::write(&spec.go_path, "go\n")?;
    Ok(())
}

fn read_result(path: &Path) -> Result<ExecResult> {
    let bytes = fs::read(path).map_err(|error| CoreError::StateCorrupted {
        step: 0,
        reason: format!("helper did not write {}: {error}", path.display()),
    })?;
    Ok(serde_json::from_slice(&bytes)?)
}

fn capture_output(bytes: Vec<u8>) -> (String, bool) {
    let truncated = bytes.len() > CAPTURE_LIMIT;
    let slice = if truncated {
        &bytes[..CAPTURE_LIMIT]
    } else {
        &bytes[..]
    };
    (String::from_utf8_lossy(slice).into_owned(), truncated)
}

fn make_tree_writable(root: &Path) -> Result<()> {
    for entry in walkdir::WalkDir::new(root)
        .contents_first(true)
        .follow_links(false)
    {
        let entry = entry.map_err(|error| {
            CoreError::Io(std::io::Error::other(format!(
                "cannot walk session tree for discard: {error}"
            )))
        })?;
        let metadata = fs::symlink_metadata(entry.path())?;
        if metadata.file_type().is_symlink() {
            continue;
        }
        let mode = if metadata.is_dir() { 0o700 } else { 0o600 };
        fs::set_permissions(entry.path(), fs::Permissions::from_mode(mode))?;
    }
    Ok(())
}
