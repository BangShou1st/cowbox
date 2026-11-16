#![cfg(windows)]

use assert_cmd::Command;
use serde_json::Value;
use std::fs;
use std::os::windows::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use tempfile::TempDir;

struct Fixture {
    _root: TempDir,
    workspace: PathBuf,
    state_root: PathBuf,
    index_root: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let root = tempfile::tempdir().expect("temp root");
        let workspace = root.path().join("workspace");
        fs::create_dir_all(&workspace).expect("workspace");
        let state_root = root.path().join("state");
        let index_root = root.path().join("index");
        Self {
            _root: root,
            workspace,
            state_root,
            index_root,
        }
    }

    fn command(&self) -> Command {
        let mut command = Command::cargo_bin("cowbox").expect("cowbox binary");
        command
            .env("COWBOX_WIN_HOME", &self.index_root)
            .current_dir(&self.workspace);
        command
    }

    fn run(&self, args: &[&str]) -> (i32, Value) {
        let output = self.command().args(args).output().expect("run cowbox");
        let code = output.status.code().unwrap_or(-1);
        let value = serde_json::from_slice(&output.stdout).unwrap_or_else(|_| {
            panic!(
                "invalid JSON for {args:?}: stdout={} stderr={}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            )
        });
        (code, value)
    }

    fn ok(&self, args: &[&str]) -> Value {
        let (code, value) = self.run(args);
        assert_eq!(
            code,
            0,
            "command {args:?} failed: {value}\nroot={}",
            self._root.path().display()
        );
        assert_eq!(value["ok"], true);
        value
    }

    fn init(&self) -> (String, PathBuf) {
        let value = self.ok(&[
            "win",
            "init",
            "--workspace",
            self.workspace.to_str().expect("workspace path"),
            "--state-root",
            self.state_root.to_str().expect("state path"),
            "--json",
        ]);
        let id = value["data"]["session_id"]
            .as_str()
            .expect("session id")
            .to_string();
        let managed = PathBuf::from(
            value["data"]["managed_workspace"]
                .as_str()
                .expect("managed workspace"),
        );
        (id, managed)
    }
}

fn write(path: &Path, text: &str) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("parent");
    }
    fs::write(path, text).expect("write");
}

fn read(path: &Path) -> String {
    fs::read_to_string(path).expect("read")
}

fn session_arg(id: &str) -> String {
    id.to_string()
}

fn state_args(fx: &Fixture) -> String {
    fx.state_root.to_str().expect("state path").to_string()
}

fn checkpoint(fx: &Fixture, id: &str) -> Value {
    fx.ok(&[
        "win",
        "checkpoint",
        "--session",
        &session_arg(id),
        "--state-root",
        &state_args(fx),
        "--json",
    ])
}

fn commit(fx: &Fixture, id: &str, extra: &[&str]) -> (i32, Value) {
    let session = session_arg(id);
    let state = state_args(fx);
    let mut args = vec![
        "win",
        "commit",
        "--session",
        session.as_str(),
        "--state-root",
        state.as_str(),
        "--yes",
        "--json",
    ];
    args.extend_from_slice(extra);
    fx.run(&args)
}

fn recover(fx: &Fixture, id: &str) -> (i32, Value) {
    let session = session_arg(id);
    let state = state_args(fx);
    fx.run(&[
        "win",
        "commit",
        "--recover",
        "--session",
        session.as_str(),
        "--state-root",
        state.as_str(),
        "--yes",
        "--json",
    ])
}

fn set_ads(path: &Path, stream: &str, value: &str) {
    let escaped = path.to_string_lossy().replace('\'', "''");
    let status = std::process::Command::new("powershell.exe")
        .args([
            "-NoProfile",
            "-Command",
            &format!("Set-Content -LiteralPath '{escaped}' -Stream {stream} -Value '{value}'"),
        ])
        .status()
        .expect("powershell");
    assert!(status.success());
    std::thread::sleep(std::time::Duration::from_millis(100));
}

fn stream_names(path: &Path) -> String {
    let escaped = path.to_string_lossy().replace('\'', "''");
    let output = std::process::Command::new("powershell.exe")
        .args([
            "-NoProfile",
            "-Command",
            &format!("(Get-Item -LiteralPath '{escaped}' -Stream *).Stream"),
        ])
        .output()
        .expect("powershell");
    assert!(output.status.success());
    String::from_utf8_lossy(&output.stdout).into_owned()
}

#[test]
fn doctor_reports_ntfs_long_paths_and_writable_state_root() {
    let fx = Fixture::new();
    let value = fx.ok(&[
        "win",
        "doctor",
        "--workspace",
        fx.workspace.to_str().expect("workspace"),
        "--state-root",
        fx.state_root.to_str().expect("state"),
        "--json",
    ]);
    assert_eq!(value["data"]["ready"], true);
    assert_eq!(value["data"]["storage_mode"], "ntfs_materialized_copy");
    assert_eq!(value["data"]["state_root"]["writable"], true);
    assert_eq!(value["data"]["path_test"]["long_paths"], true);
    assert_eq!(value["data"]["path_test"]["acl_inspection"], true);
}

#[test]
fn init_imports_untracked_files_and_excludes_git_and_node_modules() {
    let fx = Fixture::new();
    write(&fx.workspace.join(".git/config"), "[core]\n");
    write(&fx.workspace.join("src/hello.txt"), "hello");
    write(&fx.workspace.join("untracked.txt"), "untracked");
    write(&fx.workspace.join("node_modules/dep/package.json"), "{}");
    write(&fx.workspace.join("src/unicode-测试-🙂.txt"), "unicode");
    let readonly = fx.workspace.join("readonly.txt");
    write(&readonly, "readonly");
    let mut permissions = fs::metadata(&readonly).expect("metadata").permissions();
    permissions.set_readonly(true);
    fs::set_permissions(&readonly, permissions).expect("readonly");

    let (id, managed) = fx.init();
    assert!(managed.join("src/hello.txt").is_file());
    assert!(managed.join("untracked.txt").is_file());
    assert!(managed.join("src/unicode-测试-🙂.txt").is_file());
    assert!(!managed.join(".git").exists());
    assert!(!managed.join("node_modules").exists());
    assert!(
        fs::metadata(managed.join("readonly.txt"))
            .expect("managed readonly")
            .permissions()
            .readonly()
    );

    let value = fx.ok(&[
        "win",
        "discard",
        "--session",
        &id,
        "--state-root",
        &state_args(&fx),
        "--yes",
        "--json",
    ]);
    assert_eq!(value["data"]["discarded"], true);
    assert!(!fx.state_root.join("win/sessions").join(&id).exists());
}

#[test]
fn checkpoint_diff_rollback_rescue_and_commit_flow() {
    let fx = Fixture::new();
    write(&fx.workspace.join("src/hello.txt"), "hello");
    write(&fx.workspace.join("keep.txt"), "keep");
    let (id, managed) = fx.init();

    write(&managed.join("src/hello.txt"), "WRONG");
    write(&managed.join("new-file.txt"), "new");
    let checkpoint_value = checkpoint(&fx, &id);
    assert_eq!(checkpoint_value["data"]["step"], 1);

    let diff = fx.ok(&[
        "win",
        "diff",
        "--session",
        &id,
        "--state-root",
        &state_args(&fx),
        "--json",
    ]);
    let changes = diff["data"]["changes"].as_array().expect("changes");
    assert!(
        changes.iter().any(|change| {
            change["rel_path"] == "src/hello.txt" && change["kind"] == "modified"
        })
    );
    assert!(
        changes
            .iter()
            .any(|change| { change["rel_path"] == "new-file.txt" && change["kind"] == "added" })
    );

    write(&managed.join("src/hello.txt"), "UNCHECKPOINTED");
    let rollback = fx.ok(&[
        "win",
        "rollback",
        "0",
        "--session",
        &id,
        "--state-root",
        &state_args(&fx),
        "--json",
    ]);
    assert_eq!(rollback["data"]["rescue_step"], 2);
    let managed = PathBuf::from(
        rollback["data"]["managed_workspace"]
            .as_str()
            .expect("managed"),
    );
    assert_eq!(read(&managed.join("src/hello.txt")), "hello");
    assert!(!managed.join("new-file.txt").exists());

    write(&managed.join("src/hello.txt"), "RIGHT");
    let checkpoint_value = checkpoint(&fx, &id);
    assert_eq!(checkpoint_value["data"]["step"], 3);
    let (code, value) = commit(&fx, &id, &["--step", "3"]);
    assert_eq!(code, 0, "{value}");
    assert_eq!(value["data"]["committed"], true);
    assert_eq!(read(&fx.workspace.join("src/hello.txt")), "RIGHT");
    assert!(!fx.workspace.join("new-file.txt").exists());
    assert_eq!(read(&fx.workspace.join("keep.txt")), "keep");
}

#[test]
#[allow(clippy::permissions_set_readonly_false)]
fn commit_preserves_readonly_ads_deletions_and_new_directories() {
    let fx = Fixture::new();
    write(&fx.workspace.join("src/hello.txt"), "OLD");
    write(&fx.workspace.join("keep.txt"), "keep");
    write(&fx.workspace.join("dir-to-delete/old.txt"), "old-dir");
    let zone = fx.workspace.join("zone.txt");
    write(&zone, "ZONE-OLD");
    set_ads(&zone, "Zone.Identifier", "[ZoneTransfer]");
    let readonly = fx.workspace.join("readonly.txt");
    write(&readonly, "RO-OLD");
    let mut permissions = fs::metadata(&readonly).expect("metadata").permissions();
    permissions.set_readonly(true);
    fs::set_permissions(&readonly, permissions).expect("readonly");
    let (id, managed) = fx.init();

    write(&managed.join("src/hello.txt"), "NEW");
    write(&managed.join("zone.txt"), "ZONE-NEW");
    let managed_readonly = managed.join("readonly.txt");
    let mut permissions = fs::metadata(&managed_readonly)
        .expect("metadata")
        .permissions();
    permissions.set_readonly(false);
    fs::set_permissions(&managed_readonly, permissions).expect("writable");
    write(&managed_readonly, "RO-NEW");
    let mut permissions = fs::metadata(&managed_readonly)
        .expect("metadata")
        .permissions();
    permissions.set_readonly(true);
    fs::set_permissions(&managed_readonly, permissions).expect("readonly");
    fs::remove_file(managed.join("keep.txt")).expect("remove keep");
    fs::remove_file(managed.join("dir-to-delete/old.txt")).expect("remove old");
    fs::remove_dir(managed.join("dir-to-delete")).expect("remove dir");
    write(&managed.join("newdir/file.txt"), "newdir");
    checkpoint(&fx, &id);

    let (code, value) = commit(&fx, &id, &[]);
    assert_eq!(code, 0, "{value}");
    assert_eq!(read(&fx.workspace.join("src/hello.txt")), "NEW");
    assert_eq!(read(&fx.workspace.join("zone.txt")), "ZONE-NEW");
    assert!(stream_names(&fx.workspace.join("zone.txt")).contains("Zone.Identifier"));
    assert_eq!(read(&fx.workspace.join("readonly.txt")), "RO-NEW");
    assert!(
        fs::metadata(fx.workspace.join("readonly.txt"))
            .expect("readonly")
            .permissions()
            .readonly()
    );
    assert!(!fx.workspace.join("keep.txt").exists());
    assert!(!fx.workspace.join("dir-to-delete").exists());
    assert_eq!(read(&fx.workspace.join("newdir/file.txt")), "newdir");
}

#[test]
fn host_drift_is_refused_and_force_commits() {
    let fx = Fixture::new();
    write(&fx.workspace.join("a.txt"), "A0");
    let (id, managed) = fx.init();
    write(&managed.join("a.txt"), "A1");
    checkpoint(&fx, &id);
    write(&fx.workspace.join("a.txt"), "HOST-DRIFT");

    let (code, value) = commit(&fx, &id, &[]);
    assert_eq!(code, 2);
    assert_eq!(value["error"]["code"], "WIN_HOST_DRIFT");
    assert_eq!(read(&fx.workspace.join("a.txt")), "HOST-DRIFT");

    let (code, value) = commit(&fx, &id, &["--force"]);
    assert_eq!(code, 0, "{value}");
    assert_eq!(read(&fx.workspace.join("a.txt")), "A1");
}

#[test]
fn case_only_rename_commits_as_delete_plus_add() {
    let fx = Fixture::new();
    write(&fx.workspace.join("Name.txt"), "CASE");
    let (id, managed) = fx.init();
    fs::rename(managed.join("Name.txt"), managed.join("tmp-rename.txt")).expect("rename 1");
    fs::rename(managed.join("tmp-rename.txt"), managed.join("name.txt")).expect("rename 2");
    let value = checkpoint(&fx, &id);
    assert_eq!(value["data"]["added"], 1);
    assert_eq!(value["data"]["deleted"], 1);
    let (code, value) = commit(&fx, &id, &[]);
    assert_eq!(code, 0, "{value}");
    let names = fs::read_dir(&fx.workspace)
        .expect("workspace listing")
        .flatten()
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .collect::<Vec<_>>();
    assert!(!names.iter().any(|name| name == "Name.txt"));
    assert!(names.iter().any(|name| name == "name.txt"));
    assert_eq!(read(&fx.workspace.join("name.txt")), "CASE");
}

#[test]
fn long_path_and_unicode_commit_work() {
    let fx = Fixture::new();
    let directory = fx.workspace.join("a".repeat(110)).join("b".repeat(110));
    let file = directory.join(format!("{}.txt", "c".repeat(110)));
    write(&file, "LONG0");
    write(&fx.workspace.join("unicode-测试-🙂.txt"), "U0");
    let (id, managed) = fx.init();
    let managed_file = managed
        .join("a".repeat(110))
        .join("b".repeat(110))
        .join(format!("{}.txt", "c".repeat(110)));
    write(&managed_file, "LONG1");
    write(&managed.join("unicode-测试-🙂.txt"), "U1");
    checkpoint(&fx, &id);
    let (code, value) = commit(&fx, &id, &[]);
    assert_eq!(code, 0, "{value}");
    assert_eq!(read(&file), "LONG1");
    assert_eq!(read(&fx.workspace.join("unicode-测试-🙂.txt")), "U1");
}

#[test]
fn interrupted_apply_recovers_and_retry_commits() {
    let fx = Fixture::new();
    write(&fx.workspace.join("a.txt"), "A0");
    write(&fx.workspace.join("b.txt"), "B0");
    write(&fx.workspace.join("c.txt"), "C0");
    let (id, managed) = fx.init();
    write(&managed.join("a.txt"), "A1");
    write(&managed.join("b.txt"), "B1");
    write(&managed.join("c.txt"), "C1");
    checkpoint(&fx, &id);

    let output = fx
        .command()
        .env("COWBOX_WIN_FAULT", "abort-after-apply:1")
        .args([
            "win",
            "commit",
            "--session",
            &session_arg(&id),
            "--state-root",
            &state_args(&fx),
            "--yes",
            "--json",
        ])
        .output()
        .expect("commit");
    assert_eq!(output.status.code(), Some(70));
    assert_eq!(read(&fx.workspace.join("a.txt")), "A1");
    assert_eq!(read(&fx.workspace.join("b.txt")), "B0");

    let (code, value) = recover(&fx, &id);
    assert_eq!(code, 0, "{value}");
    assert_eq!(value["data"]["action"], "restore_host");
    assert_eq!(read(&fx.workspace.join("a.txt")), "A0");
    assert_eq!(read(&fx.workspace.join("b.txt")), "B0");
    assert_eq!(read(&fx.workspace.join("c.txt")), "C0");

    let (code, value) = commit(&fx, &id, &[]);
    assert_eq!(code, 0, "{value}");
    assert_eq!(read(&fx.workspace.join("a.txt")), "A1");
    assert_eq!(read(&fx.workspace.join("b.txt")), "B1");
    assert_eq!(read(&fx.workspace.join("c.txt")), "C1");
}

#[test]
fn interrupted_recovery_can_be_retried() {
    let fx = Fixture::new();
    write(&fx.workspace.join("a.txt"), "A0");
    write(&fx.workspace.join("b.txt"), "B0");
    write(&fx.workspace.join("c.txt"), "C0");
    let (id, managed) = fx.init();
    write(&managed.join("a.txt"), "A1");
    write(&managed.join("b.txt"), "B1");
    write(&managed.join("c.txt"), "C1");
    checkpoint(&fx, &id);
    let output = fx
        .command()
        .env("COWBOX_WIN_FAULT", "abort-after-apply:1")
        .args([
            "win",
            "commit",
            "--session",
            &session_arg(&id),
            "--state-root",
            &state_args(&fx),
            "--yes",
            "--json",
        ])
        .output()
        .expect("commit");
    assert_eq!(output.status.code(), Some(70));

    let output = fx
        .command()
        .env("COWBOX_WIN_FAULT", "abort-during-recover:0")
        .args([
            "win",
            "commit",
            "--recover",
            "--session",
            &session_arg(&id),
            "--state-root",
            &state_args(&fx),
            "--yes",
            "--json",
        ])
        .output()
        .expect("recover 1");
    assert_eq!(output.status.code(), Some(70));

    let (code, value) = recover(&fx, &id);
    assert_eq!(code, 0, "{value}");
    assert_eq!(read(&fx.workspace.join("a.txt")), "A0");
    assert_eq!(read(&fx.workspace.join("b.txt")), "B0");
    assert_eq!(read(&fx.workspace.join("c.txt")), "C0");
}

#[test]
fn mid_commit_host_edit_is_preserved() {
    let fx = Fixture::new();
    write(&fx.workspace.join("a.txt"), "A0");
    write(&fx.workspace.join("b.txt"), "B0");
    write(&fx.workspace.join("c.txt"), "C0");
    let (id, managed) = fx.init();
    write(&managed.join("a.txt"), "A1");
    write(&managed.join("b.txt"), "B1");
    write(&managed.join("c.txt"), "C1");
    checkpoint(&fx, &id);
    let output = fx
        .command()
        .env("COWBOX_WIN_FAULT_MID_EDIT", "2|b.txt|USER-EDIT")
        .args([
            "win",
            "commit",
            "--session",
            &session_arg(&id),
            "--state-root",
            &state_args(&fx),
            "--yes",
            "--json",
        ])
        .output()
        .expect("commit");
    assert_eq!(output.status.code(), Some(2));
    assert_eq!(read(&fx.workspace.join("a.txt")), "A0");
    assert_eq!(read(&fx.workspace.join("b.txt")), "USER-EDIT");
    assert_eq!(read(&fx.workspace.join("c.txt")), "C0");
    let commits = fx.state_root.join("win/sessions").join(&id).join("commits");
    let journal = fs::read_dir(commits)
        .expect("commits")
        .flatten()
        .next()
        .expect("journal")
        .path();
    assert_eq!(
        read(&journal.join("conflicts/b.txt")),
        "USER-EDIT",
        "conflict copy must preserve the user's bytes"
    );
}

#[test]
fn busy_host_file_returns_actionable_error_without_changes() {
    let fx = Fixture::new();
    write(&fx.workspace.join("a.txt"), "A0");
    let (id, managed) = fx.init();
    write(&managed.join("a.txt"), "A1");
    checkpoint(&fx, &id);
    let handle = fs::OpenOptions::new()
        .read(true)
        .share_mode(1)
        .open(fx.workspace.join("a.txt"))
        .expect("hold file");
    let (code, value) = commit(&fx, &id, &[]);
    assert_ne!(code, 0);
    assert_eq!(value["error"]["code"], "WIN_FILE_BUSY");
    assert_eq!(read(&fx.workspace.join("a.txt")), "A0");
    drop(handle);
    let (code, value) = commit(&fx, &id, &[]);
    assert_eq!(code, 0, "{value}");
    assert_eq!(read(&fx.workspace.join("a.txt")), "A1");
}

#[test]
fn explicit_acl_and_hardlink_changes_are_refused_before_writeback() {
    let fx = Fixture::new();
    write(&fx.workspace.join("acl.txt"), "ACL0");
    write(&fx.workspace.join("hard-a.txt"), "HARD0");
    fs::hard_link(
        fx.workspace.join("hard-a.txt"),
        fx.workspace.join("hard-b.txt"),
    )
    .expect("hard link");
    let (id, managed) = fx.init();
    write(&managed.join("acl.txt"), "ACL1");
    write(&managed.join("hard-a.txt"), "HARD1");
    checkpoint(&fx, &id);
    let status = std::process::Command::new("icacls.exe")
        .args([
            fx.workspace.join("acl.txt").to_str().expect("path"),
            "/grant",
            "BUILTIN\\Users:(F)",
        ])
        .status()
        .expect("icacls");
    assert!(status.success());
    let (code, value) = commit(&fx, &id, &[]);
    assert_eq!(code, 2);
    let error_code = value["error"]["code"].as_str().expect("code");
    assert!(
        error_code == "WIN_ACL_UNSUPPORTED" || error_code == "WIN_HARDLINK_UNSUPPORTED",
        "{value}"
    );
    assert_eq!(read(&fx.workspace.join("acl.txt")), "ACL0");
    assert_eq!(read(&fx.workspace.join("hard-a.txt")), "HARD0");
}

#[test]
fn wrong_session_is_rejected_without_touching_host() {
    let fx = Fixture::new();
    write(&fx.workspace.join("a.txt"), "A0");
    let (id, managed) = fx.init();
    write(&managed.join("a.txt"), "A1");
    checkpoint(&fx, &id);
    let (code, value) = commit(&fx, "deadbeef00", &[]);
    assert_ne!(code, 0);
    assert_eq!(value["error"]["code"], "WIN_SESSION_NOT_FOUND");
    assert_eq!(read(&fx.workspace.join("a.txt")), "A0");
}

#[test]
fn log_and_diff_resolve_the_active_session_from_the_workspace() {
    let fx = Fixture::new();
    write(&fx.workspace.join("a.txt"), "A0");
    let (id, managed) = fx.init();
    write(&managed.join("a.txt"), "A1");
    checkpoint(&fx, &id);

    let log = fx.ok(&["win", "log", "--json"]);
    assert_eq!(log["data"]["session_id"], id);
    assert_eq!(log["data"]["head"], 1);
    let diff = fx.ok(&["win", "diff", "--json"]);
    assert_eq!(diff["data"]["session_id"], id);
    assert_eq!(diff["data"]["changes"][0]["rel_path"], "a.txt");
}

#[test]
fn crash_after_operation_before_journal_accounting_is_recovered() {
    let fx = Fixture::new();
    write(&fx.workspace.join("a.txt"), "A0");
    write(&fx.workspace.join("b.txt"), "B0");
    let (id, managed) = fx.init();
    write(&managed.join("a.txt"), "A1");
    write(&managed.join("b.txt"), "B1");
    checkpoint(&fx, &id);

    let output = fx
        .command()
        .env("COWBOX_WIN_FAULT", "abort-after-operation:1")
        .args([
            "win",
            "commit",
            "--session",
            &id,
            "--state-root",
            &state_args(&fx),
            "--yes",
            "--json",
        ])
        .output()
        .expect("commit");
    assert_eq!(output.status.code(), Some(70));
    assert_eq!(read(&fx.workspace.join("a.txt")), "A1");
    assert_eq!(read(&fx.workspace.join("b.txt")), "B0");

    let (code, value) = recover(&fx, &id);
    assert_eq!(code, 0, "{value}");
    assert_eq!(read(&fx.workspace.join("a.txt")), "A0");
    assert_eq!(read(&fx.workspace.join("b.txt")), "B0");
}

#[test]
fn crash_after_delete_before_journal_accounting_restores_the_deleted_file() {
    let fx = Fixture::new();
    write(&fx.workspace.join("a.txt"), "A0");
    write(&fx.workspace.join("b.txt"), "B0");
    let (id, managed) = fx.init();
    fs::remove_file(managed.join("a.txt")).expect("delete managed a");
    write(&managed.join("b.txt"), "B1");
    checkpoint(&fx, &id);

    let output = fx
        .command()
        .env("COWBOX_WIN_FAULT", "abort-after-operation:1")
        .args([
            "win",
            "commit",
            "--session",
            &id,
            "--state-root",
            &state_args(&fx),
            "--yes",
            "--json",
        ])
        .output()
        .expect("commit");
    assert_eq!(output.status.code(), Some(70));
    assert!(!fx.workspace.join("a.txt").exists());
    assert_eq!(read(&fx.workspace.join("b.txt")), "B0");

    let (code, value) = recover(&fx, &id);
    assert_eq!(code, 0, "{value}");
    assert_eq!(read(&fx.workspace.join("a.txt")), "A0");
    assert_eq!(read(&fx.workspace.join("b.txt")), "B0");
}

#[test]
#[allow(clippy::permissions_set_readonly_false)]
fn busy_readonly_file_keeps_its_attribute_after_refused_commit() {
    let fx = Fixture::new();
    write(&fx.workspace.join("a.txt"), "A0");
    let host = fx.workspace.join("a.txt");
    let mut permissions = fs::metadata(&host).expect("metadata").permissions();
    permissions.set_readonly(true);
    fs::set_permissions(&host, permissions).expect("readonly");
    let (id, managed) = fx.init();
    let managed_file = managed.join("a.txt");
    let mut permissions = fs::metadata(&managed_file)
        .expect("managed metadata")
        .permissions();
    permissions.set_readonly(false);
    fs::set_permissions(&managed_file, permissions).expect("writable");
    write(&managed_file, "A1");
    let mut permissions = fs::metadata(&managed_file)
        .expect("managed metadata")
        .permissions();
    permissions.set_readonly(true);
    fs::set_permissions(&managed_file, permissions).expect("readonly");
    checkpoint(&fx, &id);

    let handle = fs::OpenOptions::new()
        .read(true)
        .share_mode(1)
        .open(&host)
        .expect("hold file");
    let (code, value) = commit(&fx, &id, &[]);
    assert_ne!(code, 0);
    assert_eq!(value["error"]["code"], "WIN_FILE_BUSY");
    assert_eq!(read(&host), "A0");
    assert!(
        fs::metadata(&host)
            .expect("host metadata")
            .permissions()
            .readonly(),
        "a failed busy replacement must restore the host read-only attribute"
    );
    drop(handle);
}

#[test]
fn busy_readonly_file_keeps_its_attribute_after_refused_delete() {
    let fx = Fixture::new();
    write(&fx.workspace.join("a.txt"), "A0");
    let host = fx.workspace.join("a.txt");
    let mut permissions = fs::metadata(&host).expect("metadata").permissions();
    permissions.set_readonly(true);
    fs::set_permissions(&host, permissions).expect("readonly");
    let (id, managed) = fx.init();
    fs::remove_file(managed.join("a.txt")).expect("delete managed a");
    checkpoint(&fx, &id);

    let handle = fs::OpenOptions::new()
        .read(true)
        .share_mode(1)
        .open(&host)
        .expect("hold file");
    let (code, value) = commit(&fx, &id, &[]);
    assert_ne!(code, 0);
    assert_eq!(value["error"]["code"], "WIN_FILE_BUSY");
    assert_eq!(read(&host), "A0");
    assert!(
        fs::metadata(&host)
            .expect("host metadata")
            .permissions()
            .readonly(),
        "a failed busy delete must restore the host read-only attribute"
    );
    drop(handle);

    let (code, value) = commit(&fx, &id, &[]);
    assert_eq!(code, 0, "{value}");
    assert!(!host.exists());
}

#[test]
fn file_and_directory_swaps_commit_in_both_directions() {
    let fx = Fixture::new();
    write(&fx.workspace.join("file-to-dir"), "old-file");
    write(&fx.workspace.join("dir-to-file/old.txt"), "old-dir");
    let (id, managed) = fx.init();
    fs::remove_file(managed.join("file-to-dir")).expect("remove file");
    write(&managed.join("file-to-dir/new.txt"), "new-dir");
    fs::remove_dir_all(managed.join("dir-to-file")).expect("remove dir");
    write(&managed.join("dir-to-file"), "new-file");
    checkpoint(&fx, &id);

    let (code, preview) = fx.run(&[
        "win",
        "commit",
        "--session",
        &id,
        "--state-root",
        &state_args(&fx),
        "--dry-run",
        "--json",
    ]);
    assert_eq!(code, 0, "{preview}");
    assert_eq!(preview["data"]["can_commit"], true);

    let (code, value) = commit(&fx, &id, &[]);
    assert_eq!(code, 0, "{value}");
    assert!(fx.workspace.join("file-to-dir").is_dir());
    assert_eq!(read(&fx.workspace.join("file-to-dir/new.txt")), "new-dir");
    assert!(fx.workspace.join("dir-to-file").is_file());
    assert_eq!(read(&fx.workspace.join("dir-to-file")), "new-file");
}
