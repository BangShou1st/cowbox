use super::env;
use super::landlock;
use super::mount;
use super::spec::{ExecResult, RunSpec, load_spec, write_result};
use crate::error::{CoreError, Result};
use rustix::process::{Pid, Signal, WaitOptions, kill_process, waitpid};
use std::fs;
use std::os::unix::process::ExitStatusExt;
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

pub fn supervisor_main() -> Result<()> {
    let (_spec_path, spec) = load_spec()?;
    let started = Instant::now();
    let result = match run_supervisor(&spec, started) {
        Ok(result) => result,
        Err(error) => ExecResult::error(error.to_string()),
    };
    write_result(&spec.result_path, &result)?;
    std::process::exit(result.exit_code);
}

fn run_supervisor(spec: &RunSpec, started: Instant) -> Result<ExecResult> {
    fs::create_dir_all(&spec.home_dir)?;
    fs::create_dir_all(&spec.tmp_dir)?;
    fs::create_dir_all(&spec.cache_dir)?;

    mount::mount_proc()?;
    landlock::apply(spec)?;

    let Some(program) = spec.cmd.first() else {
        return Err(CoreError::Config("no command supplied".to_string()));
    };
    let cwd = match &spec.cwd {
        Some(path) if path.is_absolute() => path.clone(),
        Some(path) => spec.merged_dir.join(path),
        None => spec.merged_dir.clone(),
    };
    if !cwd.starts_with(&spec.merged_dir) {
        return Err(CoreError::Config(format!(
            "command cwd {} escapes the sandbox workspace",
            cwd.display()
        )));
    }

    let mut command = Command::new(program);
    command.args(&spec.cmd[1..]);
    command.current_dir(&cwd);
    command.stdin(Stdio::inherit());
    command.stdout(Stdio::inherit());
    command.stderr(Stdio::inherit());
    command.env_clear();
    for (key, value) in env::build_env(spec) {
        command.env(key, value);
    }
    let mut child = command.spawn().map_err(|error| {
        CoreError::IsolationFailed(format!("cannot execute {program}: {error}"))
    })?;

    let timeout = spec.timeout_ms.map(Duration::from_millis);
    let mut timed_out = false;
    let exit_code;
    let mut signal = None;
    loop {
        if let Some(status) = child.try_wait()? {
            exit_code = status
                .code()
                .unwrap_or_else(|| 128 + status.signal().unwrap_or(0));
            signal = status.signal();
            break;
        }
        if timeout.is_some_and(|limit| started.elapsed() > limit) {
            timed_out = true;
            exit_code = 124;
            kill_namespace();
            let _ = child.wait();
            break;
        }
        thread::sleep(Duration::from_millis(10));
    }

    let survivors = namespace_pids();
    let orphans_reaped = survivors.len();
    if !survivors.is_empty() {
        kill_namespace();
    }
    reap_children();

    Ok(ExecResult {
        exit_code,
        signal,
        duration_ms: u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
        orphans_reaped,
        timed_out,
        error: None,
    })
}

fn namespace_pids() -> Vec<i32> {
    let Ok(entries) = fs::read_dir("/proc") else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter_map(|entry| entry.file_name().to_string_lossy().parse::<i32>().ok())
        .filter(|pid| *pid > 1)
        .collect()
}

fn kill_namespace() {
    for raw_pid in namespace_pids() {
        if let Some(pid) = Pid::from_raw(raw_pid) {
            let _ = kill_process(pid, Signal::KILL);
        }
    }
}

fn reap_children() {
    while let Ok(Some(_)) = waitpid(None, WaitOptions::NOHANG) {}
}
