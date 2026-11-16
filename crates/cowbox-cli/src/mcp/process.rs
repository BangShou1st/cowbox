use crate::error::{CliError, CliResult};
use std::ffi::OsString;
use std::io::{Read, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant};

pub struct ProcessSpec {
    pub program: OsString,
    pub args: Vec<OsString>,
    pub cwd: PathBuf,
    pub env: Vec<(OsString, OsString)>,
    pub stdin: Option<Vec<u8>>,
    pub timeout: Option<Duration>,
    pub disconnect: Arc<AtomicBool>,
    pub max_stdout: usize,
    pub max_stderr: usize,
}

pub struct ProcessOutput {
    pub exit_code: Option<i32>,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    pub timed_out: bool,
    pub disconnected: bool,
    pub stdout_truncated: bool,
    pub stderr_truncated: bool,
}

pub fn run(spec: ProcessSpec) -> CliResult<ProcessOutput> {
    let mut command = Command::new(&spec.program);
    command
        .args(&spec.args)
        .current_dir(&spec.cwd)
        .stdin(if spec.stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (key, value) in spec.env {
        command.env(key, value);
    }
    let mut child = command.spawn().map_err(|error| {
        CliError::new(
            "MCP_PROCESS_START_FAILED",
            format!("cannot start {}: {error}", spec.program.to_string_lossy()),
        )
    })?;

    let stdin_thread = spec.stdin.map(|request| {
        let mut stdin = child.stdin.take().expect("piped stdin");
        thread::spawn(move || {
            let _ = stdin.write_all(&request);
            let _ = stdin.flush();
        })
    });
    let stdout = child.stdout.take().expect("piped stdout");
    let stderr = child.stderr.take().expect("piped stderr");
    let stdout_thread = thread::spawn(move || read_limited(stdout, spec.max_stdout));
    let stderr_thread = thread::spawn(move || read_limited(stderr, spec.max_stderr));

    let started = Instant::now();
    let mut timed_out = false;
    let mut disconnected = false;
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if spec.disconnect.load(Ordering::Relaxed) {
            disconnected = true;
            terminate_tree(child.id());
            let _ = child.kill();
            break child.wait()?;
        }
        if spec
            .timeout
            .is_some_and(|timeout| started.elapsed() >= timeout)
        {
            timed_out = true;
            terminate_tree(child.id());
            let _ = child.kill();
            break child.wait()?;
        }
        thread::sleep(Duration::from_millis(20));
    };

    if let Some(thread) = stdin_thread {
        let _ = thread.join();
    }
    let (stdout, stdout_truncated) = stdout_thread
        .join()
        .map_err(|_| CliError::new("MCP_PROCESS_FAILED", "stdout reader panicked"))?;
    let (stderr, stderr_truncated) = stderr_thread
        .join()
        .map_err(|_| CliError::new("MCP_PROCESS_FAILED", "stderr reader panicked"))?;

    Ok(ProcessOutput {
        exit_code: status.code(),
        stdout,
        stderr,
        timed_out,
        disconnected,
        stdout_truncated,
        stderr_truncated,
    })
}

fn read_limited(mut reader: impl Read, limit: usize) -> (Vec<u8>, bool) {
    let mut output = Vec::new();
    let mut truncated = false;
    let mut buffer = [0u8; 8192];
    loop {
        match reader.read(&mut buffer) {
            Ok(0) | Err(_) => break,
            Ok(read) => {
                let remaining = limit.saturating_sub(output.len());
                if remaining > 0 {
                    output.extend_from_slice(&buffer[..read.min(remaining)]);
                }
                if read > remaining {
                    truncated = true;
                }
            }
        }
    }
    (output, truncated)
}

#[cfg(target_os = "linux")]
fn terminate_tree(root: u32) {
    use rustix::process::{Pid, Signal, kill_process};

    fn collect(pid: u32, output: &mut Vec<u32>) {
        let tasks = format!("/proc/{pid}/task");
        let Ok(entries) = std::fs::read_dir(tasks) else {
            return;
        };
        for entry in entries.flatten() {
            let children = format!(
                "/proc/{pid}/task/{}/children",
                entry.file_name().to_string_lossy()
            );
            let Ok(text) = std::fs::read_to_string(children) else {
                continue;
            };
            for child in text.split_whitespace().filter_map(|item| item.parse().ok()) {
                if !output.contains(&child) {
                    output.push(child);
                    collect(child, output);
                }
            }
        }
    }

    let mut pids = Vec::new();
    collect(root, &mut pids);
    for pid in pids.into_iter().rev() {
        if let Some(pid) = Pid::from_raw(pid as i32) {
            let _ = kill_process(pid, Signal::KILL);
        }
    }
    if let Some(pid) = Pid::from_raw(root as i32) {
        let _ = kill_process(pid, Signal::KILL);
    }
}

#[cfg(not(target_os = "linux"))]
fn terminate_tree(_root: u32) {}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;

    #[test]
    fn timeout_kills_a_descendant_process() {
        let temp = tempfile::tempdir().expect("temp");
        let marker = temp.path().join("survived");
        let script = format!("sh -c 'sleep 2; : > {}' & wait", marker.to_string_lossy());
        let output = run(ProcessSpec {
            program: OsString::from("/bin/sh"),
            args: vec![OsString::from("-c"), OsString::from(script)],
            cwd: temp.path().to_path_buf(),
            env: Vec::new(),
            stdin: None,
            timeout: Some(Duration::from_millis(200)),
            disconnect: Arc::new(AtomicBool::new(false)),
            max_stdout: 1024,
            max_stderr: 1024,
        })
        .expect("process output");
        assert!(output.timed_out);
        thread::sleep(Duration::from_millis(2200));
        assert!(!marker.exists(), "descendant survived process cancellation");
    }
}
