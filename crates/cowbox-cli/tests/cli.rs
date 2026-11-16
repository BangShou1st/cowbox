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

#[test]
#[cfg(not(windows))]
fn doctor_resolves_relative_missing_parent_components() {
    let root = tempfile::tempdir().expect("root");
    let workspace = root.path().join("workspace");
    std::fs::create_dir(&workspace).expect("workspace");
    std::fs::create_dir(workspace.join(".git")).expect("git marker");
    let state = root.path().join("state");

    let output = cowbox()
        .current_dir(&workspace)
        .env("COWBOX_HOME", "../missing/../state")
        .args(["doctor", "--json"])
        .output()
        .expect("run doctor");
    let value: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("doctor json output");

    assert_eq!(output.status.code(), Some(0));
    assert_eq!(value["data"]["ready"].as_bool(), Some(true));
    assert!(!root.path().join("missing").exists());
    assert!(!state.exists());
}

#[test]
#[cfg(not(windows))]
fn doctor_does_not_create_the_state_layout() {
    let dir = workspace();
    let state_parent = tempfile::tempdir().expect("state parent");
    let state = state_parent.path().join("nested/cowbox-state");

    let output = cowbox()
        .current_dir(dir.path())
        .env("COWBOX_HOME", &state)
        .args(["doctor", "--json"])
        .output()
        .expect("run doctor");
    let value: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("doctor json output");

    assert_eq!(value["ok"].as_bool(), Some(true));
    assert!(
        !state.exists(),
        "doctor must not create {}",
        state.display()
    );
}

#[test]
#[cfg(not(windows))]
fn doctor_rejects_a_dedicated_9p_state_path_without_creating_it() {
    let nine_p_root = Path::new("/mnt/e");
    let Ok(stat) = rustix::fs::statfs(nine_p_root) else {
        return;
    };
    if stat.f_type as u64 != 0x0102_1997 {
        return;
    }

    let dir = workspace();
    let state_parent = tempfile::Builder::new()
        .prefix("cowbox-doctor-9p-")
        .tempdir_in(nine_p_root)
        .expect("dedicated 9p state parent");
    let state = state_parent.path().join("state");

    let output = cowbox()
        .current_dir(dir.path())
        .env("COWBOX_HOME", &state)
        .args(["doctor", "--json"])
        .output()
        .expect("run doctor");
    let value: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("doctor json output");
    let notes = value["data"]["capabilities"]["notes"]
        .as_array()
        .expect("doctor notes")
        .iter()
        .filter_map(serde_json::Value::as_str)
        .collect::<Vec<_>>()
        .join("\n");

    assert_eq!(output.status.code(), Some(1));
    assert_eq!(value["data"]["ready"].as_bool(), Some(false));
    assert_eq!(
        value["data"]["capabilities"]["state_fs"].as_str(),
        Some("9p")
    );
    assert_eq!(
        value["data"]["state_root"].as_str(),
        state.to_str(),
        "doctor JSON should report the inspected state root"
    );
    assert!(notes.contains("COWBOX_HOME"));
    assert!(notes.contains("xattr-capable"));
    assert!(notes.contains("ext4"));
    assert!(
        !state.exists(),
        "doctor must not create {}",
        state.display()
    );

    let human = cowbox()
        .current_dir(dir.path())
        .env("COWBOX_HOME", &state)
        .arg("doctor")
        .output()
        .expect("run human doctor");
    let human_stdout = String::from_utf8_lossy(&human.stdout);

    assert_eq!(human.status.code(), Some(1));
    assert!(human_stdout.contains("State filesystem"));
    assert!(human_stdout.contains("COWBOX_HOME"));
    assert!(human_stdout.contains("xattr-capable"));
    assert!(human_stdout.contains("ext4"));
}

#[test]
#[cfg(not(windows))]
fn doctor_check_run_reports_an_unwritable_ext4_state_parent() {
    use std::os::unix::fs::PermissionsExt;

    if rustix::process::geteuid().as_raw() == 0 {
        return;
    }
    let root = match tempfile::Builder::new()
        .prefix("cowbox-doctor-state-disk-")
        .tempdir_in("/tmp")
    {
        Ok(root) => root,
        Err(_) => return,
    };
    let stat = match rustix::fs::statfs(root.path()) {
        Ok(stat) => stat,
        Err(_) => return,
    };
    if stat.f_type as u64 != 0xEF53 {
        return;
    }

    let dir = workspace();
    let state_parent = root.path().join("state-parent");
    std::fs::create_dir(&state_parent).expect("state parent");
    let state = state_parent.join("state");
    std::fs::set_permissions(&state_parent, std::fs::Permissions::from_mode(0o500))
        .expect("make state parent read-only");

    let output = cowbox()
        .current_dir(dir.path())
        .env("COWBOX_HOME", &state)
        .args(["doctor", "--check-run", "--json"])
        .output()
        .expect("run active doctor");
    std::fs::set_permissions(&state_parent, std::fs::Permissions::from_mode(0o700))
        .expect("restore state parent mode");

    let value: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("doctor json output");
    let check = &value["data"]["run_check"];
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(value["data"]["ready"].as_bool(), Some(false));
    assert_eq!(check["attempted"].as_bool(), Some(false));
    assert_eq!(check["command_started"].as_bool(), Some(false));
    assert_eq!(check["stages"]["state_disk"].as_bool(), Some(false));
    assert_eq!(
        check["error"]["code"].as_str(),
        Some("STATE_DISK_NOT_READY")
    );
    assert_eq!(check["state_disk"]["path"].as_str(), state.to_str());
    assert_eq!(
        check["state_disk"]["existing_prefix"].as_str(),
        state_parent.to_str()
    );
    assert!(
        check["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .contains("not writable and searchable")
    );
    assert!(!state.exists());
}

#[test]
#[cfg(unix)]
fn doctor_rejects_a_dangling_state_symlink() {
    use std::os::unix::fs::symlink;

    let dir = workspace();
    let state_parent = tempfile::tempdir().expect("state parent");
    let state_link = state_parent.path().join("state");
    let missing_target = state_parent.path().join("missing-state");
    symlink("missing-state", &state_link).expect("dangling state symlink");

    let output = cowbox()
        .current_dir(dir.path())
        .env("COWBOX_HOME", &state_link)
        .args(["doctor", "--json"])
        .output()
        .expect("run doctor");
    let value: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("doctor json output");

    assert_eq!(output.status.code(), Some(2));
    assert_eq!(value["error"]["code"].as_str(), Some("STATE_ROOT"));
    assert!(
        value["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .contains("symbolic link")
    );
    assert!(
        value["error"]["hint"]
            .as_str()
            .unwrap_or_default()
            .contains("xattr-capable")
    );
    assert!(!missing_target.exists());
    assert!(!dir.path().join("sessions").exists());
    assert!(!dir.path().join("workspaces").exists());
}

#[test]
#[cfg(unix)]
fn doctor_rejects_a_state_symlink_into_the_workspace() {
    use std::os::unix::fs::symlink;

    let dir = workspace();
    let state_parent = tempfile::tempdir().expect("state parent");
    let state_link = state_parent.path().join("state");
    symlink(dir.path(), &state_link).expect("workspace state symlink");

    let output = cowbox()
        .current_dir(dir.path())
        .env("COWBOX_HOME", &state_link)
        .args(["doctor", "--json"])
        .output()
        .expect("run doctor");
    let value: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("doctor json output");

    assert_eq!(output.status.code(), Some(2));
    assert_eq!(value["error"]["code"].as_str(), Some("STATE_ROOT"));
    assert!(
        value["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .contains("outside the workspace")
    );
    assert!(!dir.path().join("sessions").exists());
    assert!(!dir.path().join("workspaces").exists());
}
