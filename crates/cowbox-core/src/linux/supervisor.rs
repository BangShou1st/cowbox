use super::env;
use super::landlock;
use super::mount;
use super::spec::{ExecResult, RunSpec, load_spec, write_result};
use crate::error::{CoreError, Result};
use rustix::event::{PollFd, PollFlags, Timespec, poll};
use rustix::process::{Pid, PidfdFlags, Signal, WaitOptions, kill_process, pidfd_open, waitpid};
use std::fs;
use std::os::unix::process::ExitStatusExt;
use std::process::{Command, ExitStatus, Stdio};
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
    mount::mount_cache_overlays(spec)?;
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
    let (status, timed_out) = wait_for_command(&mut child, timeout, started)?;
    let exit_code = if timed_out {
        124
    } else {
        status
            .code()
            .unwrap_or_else(|| 128 + status.signal().unwrap_or(0))
    };
    let signal = if timed_out { None } else { status.signal() };

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

fn wait_for_command(
    child: &mut std::process::Child,
    timeout: Option<Duration>,
    started: Instant,
) -> Result<(ExitStatus, bool)> {
    let Some(limit) = timeout else {
        return Ok((child.wait()?, false));
    };
    match pidfd_open(Pid::from_child(child), PidfdFlags::empty()) {
        Ok(pidfd) => wait_for_command_with_pidfd(child, &pidfd, limit, started),
        Err(_) => wait_for_command_polling(child, limit, started),
    }
}

fn wait_for_command_with_pidfd(
    child: &mut std::process::Child,
    pidfd: &rustix::fd::OwnedFd,
    limit: Duration,
    started: Instant,
) -> Result<(ExitStatus, bool)> {
    let mut poll_fd = PollFd::new(pidfd, PollFlags::IN);
    if let Some(status) = child.try_wait()? {
        return Ok((status, false));
    }
    let remaining = limit.saturating_sub(started.elapsed());
    let Ok(poll_timeout) = Timespec::try_from(remaining) else {
        return wait_for_command_polling(child, limit, started);
    };
    match poll(std::slice::from_mut(&mut poll_fd), Some(&poll_timeout)) {
        Ok(0) => {
            if let Some(status) = child.try_wait()? {
                return Ok((status, false));
            }
            kill_namespace();
            Ok((child.wait()?, true))
        }
        Ok(_) => {
            if let Some(status) = child.try_wait()? {
                return Ok((status, false));
            }
            wait_for_command_polling(child, limit, started)
        }
        Err(_) => wait_for_command_polling(child, limit, started),
    }
}

fn wait_for_command_polling(
    child: &mut std::process::Child,
    limit: Duration,
    started: Instant,
) -> Result<(ExitStatus, bool)> {
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok((status, false));
        }
        if started.elapsed() > limit {
            if let Some(status) = child.try_wait()? {
                return Ok((status, false));
            }
            kill_namespace();
            return Ok((child.wait()?, true));
        }
        thread::sleep(Duration::from_millis(10));
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_wait_without_timeout_preserves_exit_code() {
        let mut child = Command::new("/bin/sh")
            .args(["-c", "exit 7"])
            .spawn()
            .expect("spawn test command");

        let (status, timed_out) =
            wait_for_command(&mut child, None, Instant::now()).expect("wait for test command");

        assert!(!timed_out);
        assert_eq!(status.code(), Some(7));
    }

    #[test]
    fn command_wait_with_pidfd_preserves_exit_code() {
        let mut child = Command::new("/bin/sh")
            .args(["-c", "exit 7"])
            .spawn()
            .expect("spawn test command");

        let (status, timed_out) =
            wait_for_command(&mut child, Some(Duration::from_secs(5)), Instant::now())
                .expect("wait for test command");

        assert!(!timed_out);
        assert_eq!(status.code(), Some(7));
    }

    #[test]
    fn polling_fallback_preserves_exit_code() {
        let mut child = Command::new("/bin/sh")
            .args(["-c", "exit 7"])
            .spawn()
            .expect("spawn test command");

        let (status, timed_out) =
            wait_for_command_polling(&mut child, Duration::from_secs(5), Instant::now())
                .expect("poll for test command");

        assert!(!timed_out);
        assert_eq!(status.code(), Some(7));
    }
}
