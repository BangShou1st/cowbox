use crate::agent::config::{WslConfig, load_global};
use crate::cli::{CliCommand, DoctorArgs, PathArgs};
use crate::error::{CliError, CliResult};
use crate::json;
use crate::setup;
use std::ffi::OsString;
use std::io::{IsTerminal, Write};
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
        | CliCommand::Stats(_)
        | CliCommand::McpProxy(_) => Some(delegate(command)),
        CliCommand::McpAdapter(_) | CliCommand::SandboxExec(_) | CliCommand::Supervisor(_) => {
            Some(Err(CliError::new(
                "INTERNAL_COMMAND",
                "internal commands are only valid inside WSL",
            )
            .exit(125)))
        }
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
    let interactive = needs_inherited_stdio(command);
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

fn needs_inherited_stdio(command: &CliCommand) -> bool {
    if matches!(command, CliCommand::McpProxy(_)) {
        return true;
    }
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

pub fn doctor(_args: &DoctorArgs) -> CliResult<i32> {
    let config = load_global()?;
    let distro = config.wsl.distro.clone();
    let linux_binary = resolve_linux_binary(&distro, &config.wsl)?;
    let cwd = std::env::current_dir()?;
    let args = std::env::args_os().skip(1).collect::<Vec<OsString>>();
    let status = Command::new("wsl.exe")
        .args([
            "-d",
            &distro,
            "--cd",
            &cwd.to_string_lossy(),
            "--exec",
            "bash",
            "-lc",
            "exec \"$0\" \"$@\"",
            &linux_binary,
        ])
        .args(args)
        .status()
        .map_err(|error| {
            CliError::new("WSL_NOT_FOUND", format!("cannot run WSL doctor: {error}"))
        })?;
    Ok(status.code().unwrap_or(1))
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
}
