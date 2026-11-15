use assert_cmd::Command;
use predicates::prelude::*;
use std::path::Path;
use tempfile::TempDir;

fn cowbox() -> Command {
    Command::cargo_bin("cowbox").expect("cowbox binary")
}

fn workspace() -> TempDir {
    let dir = tempfile::tempdir().expect("temp dir");
    std::fs::create_dir(dir.path().join(".git")).expect("fake git dir");
    dir
}

fn state_home(workspace: &Path) -> std::path::PathBuf {
    let name = workspace
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "workspace".to_string());
    workspace
        .parent()
        .expect("workspace parent")
        .join(format!("cowbox-state-{name}"))
}

#[test]
fn version_and_help_work() {
    cowbox()
        .arg("--version")
        .assert()
        .success()
        .stdout(predicate::str::contains("cowbox"));

    cowbox()
        .arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains("run"));
}

#[test]
fn init_writes_config_and_json_envelope() {
    let dir = workspace();
    let state = state_home(dir.path());
    let output = cowbox()
        .current_dir(dir.path())
        .env("COWBOX_HOME", &state)
        .args(["init", "--json"])
        .output()
        .expect("run init");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(dir.path().join(".cowbox.toml").exists());
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).expect("json output");
    assert_eq!(value["schema_version"], 1);
    assert_eq!(value["ok"], true);
    assert_eq!(value["command"], "init");
}

#[test]
#[cfg(not(windows))]
fn diff_without_session_returns_json_error() {
    let dir = workspace();
    let state = state_home(dir.path());
    let output = cowbox()
        .current_dir(dir.path())
        .env("COWBOX_HOME", &state)
        .args(["diff", "--json"])
        .output()
        .expect("run diff");
    assert!(!output.status.success());
    let value: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("json error envelope");
    assert_eq!(value["ok"], false);
    assert_eq!(value["error"]["code"], "NO_ACTIVE_SESSION");
}

#[test]
#[cfg(not(windows))]
fn discard_without_session_is_a_noop() {
    let dir = workspace();
    let state = state_home(dir.path());
    let output = cowbox()
        .current_dir(dir.path())
        .env("COWBOX_HOME", &state)
        .args(["discard", "--yes", "--json"])
        .output()
        .expect("run discard");
    assert!(output.status.success());
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).expect("json output");
    assert_eq!(value["ok"], true);
    assert_eq!(value["data"]["discarded"], false);
}
