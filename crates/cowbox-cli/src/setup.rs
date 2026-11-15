#[cfg(windows)]
use crate::agent::config::home_dir;
#[cfg(windows)]
use crate::agent::config::{load_global, save_global};
use crate::cli::SetupArgs;
use crate::error::{CliError, CliResult};
use crate::json;
#[cfg(windows)]
use serde::Serialize;
use serde_json::Value;
#[cfg(windows)]
use sha2::{Digest, Sha256};
#[cfg(windows)]
use std::fs;
#[cfg(windows)]
use std::io::Read;
use std::path::{Path, PathBuf};
#[cfg(windows)]
use std::process::{Command, Stdio};

#[cfg(windows)]
#[derive(Debug, Clone)]
struct ReleaseAsset {
    version: String,
    linux_url: String,
    sha_url: String,
}

#[cfg(windows)]
#[derive(Debug, Serialize)]
struct SetupReport {
    version: String,
    distro: String,
    linux_binary: String,
    windows_exe: PathBuf,
    doctor: Value,
}

pub fn run(args: &SetupArgs) -> CliResult<i32> {
    #[cfg(not(windows))]
    {
        let _ = args;
        Err(CliError::new(
            "WINDOWS_ONLY",
            "`cowbox setup` installs the Windows launcher; on Linux/WSL use the release tarball or `cowbox install-skill`",
        )
        .exit(2))
    }

    #[cfg(windows)]
    {
        let mut global = load_global()?;
        let distro = args
            .distro
            .clone()
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| global.wsl.distro.clone());
        if !wsl_available()? {
            return Err(CliError::new(
                "WSL_NOT_FOUND",
                "wsl.exe was not found; install WSL2 and Ubuntu first",
            )
            .hint("run `wsl --install -d Ubuntu` in an elevated terminal")
            .exit(2));
        }
        let version = args
            .version
            .clone()
            .unwrap_or_else(|| env!("CARGO_PKG_VERSION").to_string());
        let release = release_asset(&version);
        let archive = download(&release.linux_url)?;
        let checksum = download(&release.sha_url)?;
        verify_checksum(&archive, &checksum)?;
        install_linux_tarball(&archive, &distro)?;
        let linux_binary = query_linux_binary(&distro)?;
        let windows_exe = install_windows_exe(args.force)?;

        global.wsl.distro = distro.clone();
        global.wsl.linux_binary = linux_binary.clone();
        save_global(&global)?;

        let doctor = run_wsl_doctor(&distro, &linux_binary)?;
        let report = SetupReport {
            version: release.version,
            distro,
            linux_binary,
            windows_exe: windows_exe.clone(),
            doctor,
        };
        if args.json {
            json::print(&json::ok("setup", None, serde_json::to_value(&report)?))?;
        } else {
            println!("installed cowbox {}", report.version);
            println!("WSL distro: {}", report.distro);
            println!("Linux binary: {}", report.linux_binary);
            println!("Windows launcher: {}", report.windows_exe.display());
            println!("skill not installed; run `cowbox install-skill --harness codex` when ready");
            if !path_contains(&report.windows_exe) {
                println!(
                    "note: add {} to PATH to run `cowbox` from any shell",
                    report
                        .windows_exe
                        .parent()
                        .unwrap_or(Path::new(""))
                        .display()
                );
            }
        }
        Ok(0)
    }
}

#[cfg(windows)]
fn wsl_available() -> CliResult<bool> {
    let result = Command::new("wsl.exe")
        .arg("--status")
        .output()
        .map_err(|error| CliError::new("WSL_NOT_FOUND", format!("cannot run wsl.exe: {error}")))?;
    Ok(result.status.success())
}

#[cfg(windows)]
fn release_asset(version: &str) -> ReleaseAsset {
    let version = version.trim_start_matches('v');
    let tag = format!("v{version}");
    let base = format!("https://github.com/BangShou1st/cowbox/releases/download/{tag}");
    let asset = format!("cowbox-{tag}-x86_64-unknown-linux-gnu.tar.gz");
    ReleaseAsset {
        version: version.to_string(),
        linux_url: format!("{base}/{asset}"),
        sha_url: format!("{base}/{asset}.sha256"),
    }
}

#[cfg(windows)]
fn download(url: &str) -> CliResult<Vec<u8>> {
    let response = ureq::get(url)
        .set("User-Agent", "cowbox")
        .call()
        .map_err(|error| CliError::new("DOWNLOAD_FAILED", format!("{url}: {error}")))?;
    let mut bytes = Vec::new();
    response
        .into_reader()
        .read_to_end(&mut bytes)
        .map_err(|error| CliError::new("DOWNLOAD_FAILED", format!("{url}: {error}")))?;
    Ok(bytes)
}

#[cfg(windows)]
fn verify_checksum(archive: &[u8], checksum: &[u8]) -> CliResult<()> {
    let expected = String::from_utf8_lossy(checksum)
        .split_whitespace()
        .next()
        .unwrap_or_default()
        .to_ascii_lowercase();
    if expected.len() != 64 {
        return Err(CliError::new(
            "CHECKSUM_INVALID",
            "release checksum file has an unexpected format",
        ));
    }
    let mut hasher = Sha256::new();
    hasher.update(archive);
    let actual = hex::encode(hasher.finalize());
    if actual != expected {
        return Err(CliError::new(
            "CHECKSUM_MISMATCH",
            "download checksum does not match the release",
        ));
    }
    Ok(())
}

#[cfg(windows)]
fn install_linux_tarball(archive: &[u8], distro: &str) -> CliResult<()> {
    let temp = std::env::temp_dir().join(format!("cowbox-{}.tar.gz", std::process::id()));
    fs::write(&temp, archive)?;
    let file = fs::File::open(&temp)?;
    let status = Command::new("wsl.exe")
        .args([
            "-d",
            distro,
            "--",
            "bash",
            "-lc",
            "mkdir -p \"$HOME/.local/bin\" && tar -xzf - -C \"$HOME/.local/bin\" && chmod +x \"$HOME/.local/bin/cowbox\"",
        ])
        .stdin(Stdio::from(file))
        .status()
        .map_err(|error| CliError::new("WSL_INSTALL_FAILED", error.to_string()))?;
    let _ = fs::remove_file(&temp);
    if !status.success() {
        return Err(CliError::new(
            "WSL_INSTALL_FAILED",
            "failed to unpack the Linux binary inside WSL",
        ));
    }
    Ok(())
}

#[cfg(windows)]
fn query_linux_binary(distro: &str) -> CliResult<String> {
    let output = Command::new("wsl.exe")
        .args([
            "-d",
            distro,
            "--",
            "bash",
            "-lc",
            "printf %s \"$HOME/.local/bin/cowbox\"",
        ])
        .output()?;
    if !output.status.success() {
        return Err(CliError::new(
            "WSL_INSTALL_FAILED",
            format!(
                "cannot query WSL home: {}",
                String::from_utf8_lossy(&output.stderr)
            ),
        ));
    }
    let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if !path.starts_with('/') {
        return Err(CliError::new(
            "WSL_INSTALL_FAILED",
            format!("unexpected WSL binary path: {path}"),
        ));
    }
    Ok(path)
}

#[cfg(windows)]
fn run_wsl_doctor(distro: &str, linux_binary: &str) -> CliResult<Value> {
    let output = Command::new("wsl.exe")
        .args([
            "-d",
            distro,
            "--",
            "bash",
            "-lc",
            "exec \"$0\" doctor --json",
            linux_binary,
        ])
        .output()?;
    if !output.status.success() {
        return Err(CliError::new(
            "DOCTOR_FAILED",
            format!(
                "cowbox doctor failed: {}",
                String::from_utf8_lossy(&output.stderr)
            ),
        ));
    }
    serde_json::from_slice(&output.stdout)
        .map_err(|error| CliError::new("DOCTOR_INVALID", format!("invalid doctor JSON: {error}")))
}

#[cfg(windows)]
fn install_windows_exe(force: bool) -> CliResult<PathBuf> {
    let current = std::env::current_exe()?;
    let target = home_dir()?.join(".local").join("bin").join("cowbox.exe");
    if current == target {
        return Ok(target);
    }
    if let Some(parent) = target.parent() {
        fs::create_dir_all(parent)?;
    }
    if target.exists() && !force {
        let existing = fs::read(&target).unwrap_or_default();
        let current_bytes = fs::read(&current).unwrap_or_default();
        if existing == current_bytes {
            return Ok(target);
        }
    }
    fs::copy(&current, &target)?;
    Ok(target)
}

#[cfg(windows)]
fn path_contains(exe: &Path) -> bool {
    let Some(parent) = exe.parent() else {
        return false;
    };
    std::env::var_os("PATH")
        .map(|paths| {
            std::env::split_paths(&paths).any(|entry| {
                entry
                    .canonicalize()
                    .ok()
                    .zip(parent.canonicalize().ok())
                    .map(|(left, right)| left == right)
                    .unwrap_or(false)
            })
        })
        .unwrap_or(false)
}

pub fn update_check(json_mode: bool) -> CliResult<i32> {
    let response = ureq::get("https://api.github.com/repos/BangShou1st/cowbox/releases/latest")
        .set("User-Agent", "cowbox")
        .call()
        .map_err(|error| CliError::new("UPDATE_CHECK_FAILED", error.to_string()))?;
    let value: Value = response.into_json().map_err(|error| {
        CliError::new(
            "UPDATE_CHECK_FAILED",
            format!("invalid release response: {error}"),
        )
    })?;
    let latest = value["tag_name"]
        .as_str()
        .unwrap_or_default()
        .trim_start_matches('v')
        .to_string();
    let current = env!("CARGO_PKG_VERSION").to_string();
    let update_available = match (
        semver::Version::parse(&current),
        semver::Version::parse(&latest),
    ) {
        (Ok(current), Ok(latest)) => latest > current,
        _ => latest != current,
    };
    if json_mode {
        json::print(&json::ok(
            "update",
            None,
            serde_json::json!({
                "current": current,
                "latest": latest,
                "update_available": update_available,
            }),
        ))?;
    } else if update_available {
        println!("cowbox {latest} is available (current {current})");
        println!("re-run `cowbox setup` to update the CLI");
    } else {
        println!("cowbox {current} is up to date");
    }
    Ok(0)
}

pub fn windows_path_to_wsl(path: &Path) -> CliResult<PathBuf> {
    #[cfg(not(windows))]
    {
        Ok(path.to_path_buf())
    }
    #[cfg(windows)]
    {
        let text = path.to_string_lossy();
        if text.starts_with("\\\\") {
            return Err(CliError::new(
                "UNC_UNSUPPORTED",
                "UNC paths cannot be mapped to WSL automatically",
            ));
        }
        let bytes = text.as_bytes();
        if bytes.len() >= 2 && bytes[1] == b':' {
            let drive = (bytes[0] as char).to_ascii_lowercase();
            let rest = text[2..].replace('\\', "/");
            let rest = rest.trim_start_matches('/');
            return Ok(PathBuf::from(format!("/mnt/{drive}/{rest}")));
        }
        Ok(path.to_path_buf())
    }
}
