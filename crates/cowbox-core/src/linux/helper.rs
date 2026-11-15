use super::mount;
use super::spec::{ExecResult, RunSpec, load_spec, write_result};
use crate::error::{CoreError, Result};
#[allow(deprecated)]
use rustix::thread::{UnshareFlags, unshare};
use std::fs::{self, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::process::{Command, Stdio};
use std::thread;
use std::time::Duration;

pub fn sandbox_exec_main() -> Result<()> {
    let (spec_path, spec) = load_spec()?;
    if let Err(error) = run_helper(&spec, &spec_path) {
        let _ = write_result(&spec.result_path, &ExecResult::error(error.to_string()));
        return Err(error);
    }
    Ok(())
}

#[allow(deprecated)]
fn run_helper(spec: &RunSpec, spec_path: &std::path::Path) -> Result<()> {
    let mut ready = OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .open(&spec.ready_path)?;
    let mut go = OpenOptions::new()
        .create(true)
        .truncate(true)
        .read(true)
        .write(true)
        .open(&spec.go_path)?;

    #[allow(deprecated)]
    unshare(UnshareFlags::NEWUSER).map_err(|error| {
        CoreError::IsolationFailed(format!(
            "unshare(CLONE_NEWUSER) failed: {error}. Unprivileged user namespaces may be disabled."
        ))
    })?;
    ready.write_all(b"ready\n")?;
    ready.sync_all()?;
    wait_for_go(&mut go)?;

    #[allow(deprecated)]
    unshare(UnshareFlags::NEWNS).map_err(|error| {
        CoreError::IsolationFailed(format!("unshare(CLONE_NEWNS) failed: {error}"))
    })?;
    mount::mount_overlay(spec)?;

    #[allow(deprecated)]
    unshare(UnshareFlags::NEWPID).map_err(|error| {
        CoreError::IsolationFailed(format!("unshare(CLONE_NEWPID) failed: {error}"))
    })?;

    let exe = std::env::current_exe()?;
    let status = Command::new(exe)
        .arg("__supervisor")
        .arg("--spec")
        .arg(spec_path)
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()?;
    let code = status.code().unwrap_or(125);

    if !spec.result_path.exists() {
        let _ = write_result(
            &spec.result_path,
            &ExecResult::error("supervisor exited without writing a result"),
        );
    }
    let _ = mount::unmount_overlay(spec);
    let _ = fs::remove_dir_all(&spec.work_dir);
    let _ = fs::remove_dir_all(&spec.merged_dir);
    std::process::exit(code);
}

fn wait_for_go(go: &mut fs::File) -> Result<()> {
    let mut buffer = String::new();
    for _ in 0..500 {
        go.seek(SeekFrom::Start(0))?;
        buffer.clear();
        go.read_to_string(&mut buffer)?;
        if buffer.contains("go") {
            return Ok(());
        }
        thread::sleep(Duration::from_millis(10));
    }
    Err(CoreError::IsolationFailed(
        "timed out waiting for the uid/gid map handshake".to_string(),
    ))
}
