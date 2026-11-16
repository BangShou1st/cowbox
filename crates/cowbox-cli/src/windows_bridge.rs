use crate::agent::config::{WslConfig, load_global};
use crate::cli::{CliCommand, DoctorArgs, PathArgs};
use crate::error::{CliError, CliResult};
use crate::json;
use crate::render;
use crate::setup;
use cowbox_core::Capabilities;
use serde::Deserialize;
use std::ffi::OsString;
use std::io::{IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::process::Command;

pub fn delegate_if_needed(command: &CliCommand) -> Option<CliResult<i32>> {
    match command {
        CliCommand::Run(_)
        | CliCommand::Diff(_)
        | CliCommand::Log(_)
        | CliCommand::Rollback(_)
        | CliCommand::Undo(_)
        | CliCommand::Commit(_)
        | CliCommand::Demo(_)
        | CliCommand::Discard(_)
        | CliCommand::Sessions(_)
        | CliCommand::Gc(_)
        | CliCommand::Stats(_) => Some(delegate(command)),
        CliCommand::SandboxExec(_) | CliCommand::Supervisor(_) => Some(Err(CliError::new(
            "INTERNAL_COMMAND",
            "internal commands are only valid inside WSL",
        )
        .exit(125))),
        CliCommand::Init(_)
        | CliCommand::Doctor(_)
        | CliCommand::Setup(_)
        | CliCommand::InstallSkill(_)
        | CliCommand::InstallRules(_)
        | CliCommand::Policy(_)
        | CliCommand::Update(_)
        | CliCommand::Path(_) => None,
    }
}

fn delegate(command: &CliCommand) -> CliResult<i32> {
    let config = load_global()?;
    let distro = config.wsl.distro.clone();
    let linux_binary = resolve_linux_binary(&distro, &config.wsl)?;
    let interactive = needs_terminal(command);
    let cwd = std::env::current_dir()?;
    let cwd_text = cwd.to_string_lossy().to_string();
    let args = std::env::args_os().skip(1).collect::<Vec<OsString>>();

    let mut wsl_command = Command::new("wsl.exe");
    wsl_command
        .args(["-d", &distro, "--cd", &cwd_text, "--exec", "bash", "-lc"])
        .arg("exec \"$0\" \"$@\"")
        .arg(&linux_binary)
        .args(args);
    if interactive {
        return wsl_command
            .status()
            .map(|status| status.code().unwrap_or(1))
            .map_err(|error| {
                CliError::new(
                    "WSL_NOT_FOUND",
                    format!("cannot run cowbox through WSL: {error}"),
                )
                .hint("run `cowbox setup` first")
            });
    }

    let output = wsl_command.output().map_err(|error| {
        CliError::new(
            "WSL_NOT_FOUND",
            format!("cannot run cowbox through WSL: {error}"),
        )
        .hint("run `cowbox setup` first")
    })?;
    std::io::stdout().write_all(&output.stdout)?;
    let stderr = decode_wsl_text(&output.stderr);
    if !stderr.is_empty() {
        std::io::stderr().write_all(stderr.as_bytes())?;
    }
    Ok(output.status.code().unwrap_or(1))
}

fn needs_terminal(command: &CliCommand) -> bool {
    let prompts = match command {
        CliCommand::Commit(args) => !args.yes && !args.dry_run,
        CliCommand::Gc(args) => !args.yes && !args.dry_run,
        CliCommand::Discard(args) => !args.yes,
        _ => false,
    };
    prompts && std::io::stdin().is_terminal()
}

fn resolve_linux_binary(distro: &str, wsl: &WslConfig) -> CliResult<String> {
    if wsl.linux_binary.starts_with('/') {
        return Ok(wsl.linux_binary.clone());
    }
    let query = if wsl.linux_binary.starts_with('~') {
        "printf %s \"$HOME/.local/bin/cowbox\""
    } else {
        "command -v cowbox || printf %s \"$HOME/.local/bin/cowbox\""
    };
    let output = Command::new("wsl.exe")
        .args(["-d", distro, "--exec", "bash", "-lc", query])
        .output()?;
    let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if path.starts_with('/') {
        Ok(path)
    } else {
        Err(CliError::new(
            "LINUX_BINARY_NOT_FOUND",
            format!("cannot find the Linux cowbox binary in WSL distro {distro}"),
        )
        .hint("run `cowbox setup` to install it"))
    }
}

pub fn doctor(args: &DoctorArgs) -> CliResult<i32> {
    let config = load_global()?;
    let distro = config.wsl.distro.clone();
    let linux_binary = resolve_linux_binary(&distro, &config.wsl)?;
    let output = Command::new("wsl.exe")
        .args([
            "-d",
            &distro,
            "--exec",
            "bash",
            "-lc",
            "exec \"$0\" doctor --json",
            &linux_binary,
        ])
        .output()
        .map_err(|error| {
            CliError::new("WSL_NOT_FOUND", format!("cannot run WSL doctor: {error}"))
        })?;
    let response = parse_doctor_response(&output.stdout);
    match response {
        Ok(DoctorResponse::Report(report)) => {
            if args.json {
                std::io::stdout().write_all(&output.stdout)?;
            } else {
                println!("Windows bridge: WSL distro {distro}");
                render::print_doctor(
                    &report.capabilities,
                    report.state_root.as_deref().unwrap_or(Path::new("unknown")),
                    report.ready,
                );
            }
            Ok(if report.ready { 0 } else { 1 })
        }
        Ok(DoctorResponse::Failure(failure)) => {
            if args.json {
                std::io::stdout().write_all(&output.stdout)?;
                Ok(output.status.code().unwrap_or(failure.exit_code))
            } else {
                let mut error = CliError::new(failure.code, failure.message)
                    .exit(output.status.code().unwrap_or(failure.exit_code));
                if let Some(hint) = failure.hint {
                    error = error.hint(hint);
                }
                Err(error)
            }
        }
        Err(reason) => {
            let stderr = decode_wsl_text(&output.stderr);
            let stdout = decode_wsl_text(&output.stdout);
            let detail = if !stderr.trim().is_empty() {
                stderr.trim().to_string()
            } else if !stdout.trim().is_empty() {
                stdout.trim().to_string()
            } else {
                reason
            };
            let code = if output.status.success() {
                "DOCTOR_INVALID"
            } else {
                "WSL_START_FAILED"
            };
            Err(CliError::new(
                code,
                format!(
                    "WSL distro {distro} did not return a usable cowbox doctor response: {detail}"
                ),
            ))
        }
    }
}

#[derive(Debug, Deserialize)]
struct DoctorReport {
    ready: bool,
    capabilities: Capabilities,
    #[serde(default)]
    state_root: Option<PathBuf>,
}

#[derive(Debug)]
enum DoctorResponse {
    Report(DoctorReport),
    Failure(DoctorFailure),
}

#[derive(Debug)]
struct DoctorFailure {
    code: String,
    message: String,
    hint: Option<String>,
    exit_code: i32,
}

fn parse_doctor_response(bytes: &[u8]) -> Result<DoctorResponse, String> {
    let envelope: serde_json::Value =
        serde_json::from_slice(bytes).map_err(|error| format!("invalid JSON ({error})"))?;
    if envelope["command"].as_str() != Some("doctor") {
        return Err("JSON envelope is not a doctor response".to_string());
    }
    match envelope["ok"].as_bool() {
        Some(true) => {
            let data = envelope
                .get("data")
                .ok_or_else(|| "doctor response is missing data".to_string())?;
            let ready = data["ready"]
                .as_bool()
                .ok_or_else(|| "doctor response is missing data.ready".to_string())?;
            let capabilities = serde_json::from_value(
                data.get("capabilities")
                    .cloned()
                    .ok_or_else(|| "doctor response is missing data.capabilities".to_string())?,
            )
            .map_err(|error| format!("invalid data.capabilities ({error})"))?;
            let state_root = data
                .get("state_root")
                .cloned()
                .map(serde_json::from_value)
                .transpose()
                .map_err(|error| format!("invalid data.state_root ({error})"))?;
            Ok(DoctorResponse::Report(DoctorReport {
                ready,
                capabilities,
                state_root,
            }))
        }
        Some(false) => {
            let error = envelope
                .get("error")
                .ok_or_else(|| "doctor error response is missing error".to_string())?;
            Ok(DoctorResponse::Failure(DoctorFailure {
                code: error["code"]
                    .as_str()
                    .unwrap_or("DOCTOR_FAILED")
                    .to_string(),
                message: error["message"]
                    .as_str()
                    .unwrap_or("cowbox doctor failed")
                    .to_string(),
                hint: error["hint"].as_str().map(ToString::to_string),
                exit_code: error["exit_code"].as_i64().map_or(1, |code| code as i32),
            }))
        }
        None => Err("JSON envelope is missing a boolean ok field".to_string()),
    }
}

pub fn path(args: &PathArgs) -> CliResult<i32> {
    let mapped = setup::windows_path_to_wsl(&args.path)?;
    if args.json {
        json::print(&json::ok(
            "path",
            None,
            serde_json::json!({ "windows": args.path, "wsl": mapped }),
        ))?;
    } else {
        println!("{}", mapped.display());
    }
    Ok(0)
}

fn decode_wsl_text(bytes: &[u8]) -> String {
    if bytes.starts_with(&[0xFF, 0xFE]) {
        let mut units = Vec::new();
        for chunk in bytes[2..].chunks_exact(2) {
            units.push(u16::from_le_bytes([chunk[0], chunk[1]]));
        }
        return String::from_utf16_lossy(&units);
    }
    let null_ratio =
        bytes.iter().filter(|byte| **byte == 0).count() as f32 / bytes.len().max(1) as f32;
    if null_ratio > 0.2 {
        let mut units = Vec::new();
        for chunk in bytes.chunks_exact(2) {
            units.push(u16::from_le_bytes([chunk[0], chunk[1]]));
        }
        return String::from_utf16_lossy(&units);
    }
    String::from_utf8_lossy(bytes).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn decodes_utf16le_wsl_error() {
        let mut bytes = vec![0xFF, 0xFE];
        for unit in "WSL error".encode_utf16() {
            bytes.extend_from_slice(&unit.to_le_bytes());
        }
        assert_eq!(decode_wsl_text(&bytes), "WSL error");
    }

    #[test]
    fn maps_windows_drive_path_to_mnt() {
        let mapped = setup::windows_path_to_wsl(Path::new(r"E:\project\cowbox")).expect("path");
        assert_eq!(mapped, Path::new("/mnt/e/project/cowbox"));
    }

    #[test]
    fn parses_a_nonzero_capability_report() {
        let response = parse_doctor_response(
            br#"{
                "schema_version": 1,
                "ok": true,
                "command": "doctor",
                "session": null,
                "data": {
                    "ready": false,
                    "capabilities": {
                        "os": "linux",
                        "kernel": "6.6.0",
                        "wsl": true,
                        "user_namespace": true,
                        "overlayfs": true,
                        "userxattr": true,
                        "landlock_abi": 3,
                        "whiteout_kind": "char_device",
                        "state_fs": "9p",
                        "notes": ["set COWBOX_HOME to WSL ext4"]
                    },
                    "state_root": "/mnt/e/cowbox-state"
                }
            }"#,
        )
        .expect("parse doctor report");
        let DoctorResponse::Report(report) = response else {
            panic!("expected capability report");
        };
        assert!(!report.ready);
        assert_eq!(report.capabilities.state_fs.as_deref(), Some("9p"));
        assert_eq!(
            report.state_root.as_deref(),
            Some(Path::new("/mnt/e/cowbox-state"))
        );
    }

    #[test]
    fn parses_a_structured_doctor_error() {
        let response = parse_doctor_response(
            br#"{
                "schema_version": 1,
                "ok": false,
                "command": "doctor",
                "error": {
                    "code": "STATE_ROOT",
                    "message": "invalid state root",
                    "hint": "choose ext4",
                    "exit_code": 2
                }
            }"#,
        )
        .expect("parse doctor error");
        let DoctorResponse::Failure(failure) = response else {
            panic!("expected doctor failure");
        };
        assert_eq!(failure.code, "STATE_ROOT");
        assert_eq!(failure.exit_code, 2);
        assert_eq!(failure.hint.as_deref(), Some("choose ext4"));
    }

    #[test]
    fn rejects_a_wsl_error_without_a_doctor_envelope() {
        let error = parse_doctor_response(b"").expect_err("empty output is not a report");
        assert!(error.contains("invalid JSON"));

        let error =
            parse_doctor_response(br#"{"ok":true,"command":"diff","data":{}}"#).expect_err("diff");
        assert!(error.contains("not a doctor response"));
    }
}
