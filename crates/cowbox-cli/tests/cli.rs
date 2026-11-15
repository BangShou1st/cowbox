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

#[test]
#[cfg(not(windows))]
fn sessions_report_invalid_state_and_gc_skips_it() {
    let dir = workspace();
    let state = tempfile::tempdir().expect("state");
    let invalid_session = state.path().join("sessions").join("bad-id");
    std::fs::create_dir_all(&invalid_session).expect("invalid session dir");
    std::fs::write(invalid_session.join("session.json"), "{not json").expect("invalid session");

    let output = cowbox()
        .current_dir(dir.path())
        .env("COWBOX_HOME", state.path())
        .args(["sessions", "--json"])
        .output()
        .expect("run sessions");
    assert!(output.status.success());
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).expect("sessions json");
    let session = &value["data"]["sessions"][0];
    assert_eq!(session["session_id"].as_str(), Some("bad-id"));
    assert_eq!(session["status"].as_str(), Some("invalid"));
    assert_eq!(session["valid"].as_bool(), Some(false));

    let output = cowbox()
        .current_dir(dir.path())
        .env("COWBOX_HOME", state.path())
        .args(["gc", "--all", "--yes", "--json"])
        .output()
        .expect("run gc");
    assert!(output.status.success());
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).expect("gc json");
    assert_eq!(value["data"]["removed"].as_array().map(Vec::len), Some(0));
    assert_eq!(value["data"]["skipped_invalid"].as_u64(), Some(1));
    assert!(invalid_session.is_dir());
}

#[test]
#[cfg(not(windows))]
fn sessions_warn_and_gc_refuses_when_active_pointer_is_invalid() {
    let dir = workspace();
    let state = tempfile::tempdir().expect("state");
    let workspaces = state.path().join("workspaces");
    std::fs::create_dir_all(&workspaces).expect("workspaces dir");
    std::fs::write(workspaces.join("broken.json"), "{not json").expect("broken pointer");

    let output = cowbox()
        .current_dir(dir.path())
        .env("COWBOX_HOME", state.path())
        .args(["sessions", "--json"])
        .output()
        .expect("run sessions");
    assert!(output.status.success());
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).expect("sessions json");
    assert_eq!(value["data"]["warnings"].as_array().map(Vec::len), Some(1));

    let output = cowbox()
        .current_dir(dir.path())
        .env("COWBOX_HOME", state.path())
        .args(["gc", "--all", "--yes", "--json"])
        .output()
        .expect("run gc");
    assert!(!output.status.success());
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).expect("gc json");
    assert_eq!(value["error"]["code"].as_str(), Some("STATE_CORRUPTED"));
}
