use assert_cmd::Command;
use rusqlite::Connection;
use rustix::fs::XattrFlags;
use serde_json::Value;
use std::fs;
use std::os::unix::fs::{MetadataExt, PermissionsExt, symlink};
use std::os::unix::process::ExitStatusExt;
use std::path::{Path, PathBuf};
use std::process::Command as StdCommand;
use std::process::Output;
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tempfile::TempDir;

struct TestEnv {
    workspace: TempDir,
    state: TempDir,
}

impl TestEnv {
    fn new() -> Self {
        let workspace = tempfile::Builder::new()
            .prefix("cowbox-adversarial-ws-")
            .tempdir_in("/tmp")
            .expect("workspace tempdir");
        let state = tempfile::Builder::new()
            .prefix("cowbox-adversarial-state-")
            .tempdir_in("/tmp")
            .expect("state tempdir");
        fs::create_dir(workspace.path().join(".git")).expect("fake git dir");
        Self { workspace, state }
    }

    fn cowbox(&self) -> Command {
        let mut command = Command::cargo_bin("cowbox").expect("cowbox binary");
        command
            .current_dir(self.workspace.path())
            .env("COWBOX_HOME", self.state.path());
        command
    }

    fn output(&self, args: &[&str]) -> std::process::Output {
        self.cowbox().args(args).output().expect("run cowbox")
    }

    fn json(&self, args: &[&str]) -> (i32, Value) {
        let output = self.output(args);
        let value: Value = serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
            panic!(
                "invalid JSON for {:?}: {error}; stdout={}; stderr={}",
                args,
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            )
        });
        (output.status.code().unwrap_or(-1), value)
    }

    fn init(&self) {
        let output = self.output(&["init", "--json"]);
        assert!(
            output.status.success(),
            "init failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

fn assert_ok(value: &Value, command: &str) {
    assert_eq!(value["schema_version"].as_u64(), Some(1));
    assert_eq!(value["ok"].as_bool(), Some(true), "not ok: {value}");
    assert_eq!(value["command"].as_str(), Some(command));
}

fn require_sandbox(env: &TestEnv) -> bool {
    let output = env.output(&["doctor", "--json"]);
    let value: Value = serde_json::from_slice(&output.stdout).unwrap_or(Value::Null);
    let ready = output.status.success() && value["data"]["ready"].as_bool() == Some(true);
    if !ready {
        let message = format!(
            "sandbox not ready; stdout={}; stderr={}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        if std::env::var("COWBOX_REQUIRE_SANDBOX").as_deref() == Ok("1") {
            panic!("{message}");
        }
        eprintln!("skipping sandbox test: {message}");
        return false;
    }
    true
}

fn run_script(env: &TestEnv, script: &str) -> (i32, Value) {
    env.json(&["run", "--json", "--", "sh", "-c", script])
}

fn unique_suffix() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    format!("{}-{nanos}", std::process::id())
}

fn shell_output(root: &Path, script: &str) -> String {
    let output = StdCommand::new("sh")
        .arg("-c")
        .arg(script)
        .current_dir(root)
        .output()
        .expect("shell command");
    assert!(
        output.status.success(),
        "shell failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

fn file_hash(root: &Path) -> String {
    shell_output(
        root,
        "find . -type f -print0 | sort -z | xargs -0 sha256sum | sha256sum",
    )
}

fn symlink_count(root: &Path) -> usize {
    shell_output(root, "find . -type l | wc -l")
        .parse()
        .expect("symlink count")
}

fn set_user_xattr(path: &Path, name: &str, value: &[u8]) {
    rustix::fs::setxattr(path, name, value, XattrFlags::empty()).expect("set user xattr");
}

fn user_xattr(path: &Path, name: &str) -> Vec<u8> {
    let size = rustix::fs::lgetxattr_query(path, name).expect("query user xattr");
    let mut value = vec![0u8; size];
    let read = rustix::fs::lgetxattr(path, name, &mut value[..]).expect("read user xattr");
    value.truncate(read);
    value
}

fn create_sqlite(path: &Path) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("sqlite parent");
    }
    let connection = Connection::open(path).expect("open sqlite");
    connection
        .execute_batch(
            "create table users (id integer primary key, email text not null unique);
             insert into users (email) values ('adversarial@example.com');",
        )
        .expect("seed sqlite");
}

fn sqlite_integrity(path: &Path) -> String {
    let connection = Connection::open(path).expect("open sqlite");
    connection
        .query_row("pragma integrity_check", [], |row| row.get(0))
        .expect("integrity_check")
}

fn sqlite_user_count(path: &Path) -> i64 {
    let connection = Connection::open(path).expect("open sqlite");
    connection
        .query_row("select count(*) from users", [], |row| row.get(0))
        .expect("count users")
}

fn populate_large_workspace(root: &Path, files: usize) {
    let src = root.join("src");
    fs::create_dir_all(&src).expect("src");
    for index in 0..files {
        if index % 100 == 0 {
            fs::create_dir_all(src.join(format!("d{:03}", index / 100))).expect("shard");
        }
        fs::write(
            src.join(format!("d{:03}", index / 100))
                .join(format!("f{index}.txt")),
            format!("{index}\n"),
        )
        .expect("fixture file");
    }
    for index in 0..10 {
        symlink("d000/f0.txt", src.join(format!("l{index}"))).expect("fixture symlink");
    }
}

fn process_cmdline_contains(needle: &str) -> bool {
    let Ok(entries) = fs::read_dir("/proc") else {
        return false;
    };
    for entry in entries.flatten() {
        if entry.file_name().to_string_lossy().parse::<u32>().is_err() {
            continue;
        }
        let Ok(bytes) = fs::read(entry.path().join("cmdline")) else {
            continue;
        };
        if String::from_utf8_lossy(&bytes).contains(needle) {
            return true;
        }
    }
    false
}

fn crash_cowbox(env: &TestEnv, args: &[&str], point: &str) -> Output {
    let mut command = env.cowbox();
    command
        .args(args)
        .env("COWBOX_TEST_CRASH", point)
        .output()
        .expect("run cowbox with crash injection")
}

fn assert_sigkill(output: &Output) {
    assert_eq!(
        output.status.signal(),
        Some(9),
        "process was not killed at the requested commit point: status={:?} stdout={} stderr={}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn assert_abort(output: &Output) {
    assert_eq!(
        output.status.signal(),
        Some(6),
        "process was not aborted at the requested commit point: status={:?} stdout={} stderr={}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn session_dir(env: &TestEnv, session_id: &str) -> PathBuf {
    env.state.path().join("sessions").join(session_id)
}

fn only_commit_journal(env: &TestEnv, session_id: &str) -> PathBuf {
    let root = session_dir(env, session_id).join("commit");
    let journals = fs::read_dir(&root)
        .expect("commit journal root")
        .map(|entry| entry.expect("journal entry").path())
        .filter(|path| path.is_dir())
        .collect::<Vec<_>>();
    assert_eq!(journals.len(), 1, "expected one journal: {journals:?}");
    journals.into_iter().next().expect("journal")
}

fn journal_phase(journal: &Path) -> String {
    let meta: Value =
        serde_json::from_slice(&fs::read(journal.join("meta.json")).expect("journal meta"))
            .expect("journal meta json");
    meta["phase"].as_str().expect("journal phase").to_string()
}

fn active_pointers(env: &TestEnv) -> Vec<PathBuf> {
    let root = env.state.path().join("workspaces");
    let Ok(entries) = fs::read_dir(root) else {
        return Vec::new();
    };
    entries
        .map(|entry| entry.expect("workspace entry").path())
        .filter(|path| path.extension().and_then(|value| value.to_str()) == Some("json"))
        .collect()
}

fn session_status(env: &TestEnv, session_id: &str) -> String {
    let value: Value = serde_json::from_slice(
        &fs::read(session_dir(env, session_id).join("session.json")).expect("session record"),
    )
    .expect("session json");
    value["status"]
        .as_str()
        .expect("session status")
        .to_string()
}

#[test]
fn doctor_reports_linux_sandbox_capabilities() {
    let env = TestEnv::new();
    if !require_sandbox(&env) {
        return;
    }
    let (code, value) = env.json(&["doctor", "--json"]);
    assert_eq!(code, 0);
    assert_ok(&value, "doctor");
    assert_eq!(value["data"]["ready"].as_bool(), Some(true));
    assert_eq!(value["data"]["capabilities"]["os"].as_str(), Some("linux"));
    assert_eq!(
        value["data"]["capabilities"]["landlock_abi"].as_i64(),
        Some(3)
    );
    assert_eq!(
        value["data"]["capabilities"]["state_fs"].as_str(),
        Some("ext4")
    );
}

#[test]
fn ten_thousand_file_rollback_restores_tree_symlinks_and_sqlite() {
    let env = TestEnv::new();
    if !require_sandbox(&env) {
        return;
    }
    populate_large_workspace(env.workspace.path(), 10_000);
    create_sqlite(&env.workspace.path().join("data/app.sqlite"));
    env.init();

    let before_hash = file_hash(env.workspace.path());
    let before_links = symlink_count(env.workspace.path());

    let (code, run) = run_script(&env, "rm -rf src data/app.sqlite");
    assert_eq!(code, 0, "run failed: {run}");
    assert_ok(&run, "run");
    assert_eq!(file_hash(env.workspace.path()), before_hash);
    assert_eq!(symlink_count(env.workspace.path()), before_links);
    assert_eq!(
        sqlite_integrity(&env.workspace.path().join("data/app.sqlite")),
        "ok"
    );

    let diff = env.output(&["diff", "--stat"]);
    assert!(
        diff.status.success(),
        "diff failed: {}",
        String::from_utf8_lossy(&diff.stderr)
    );
    assert!(
        String::from_utf8_lossy(&diff.stdout).contains("deleted"),
        "diff did not report deletions: {}",
        String::from_utf8_lossy(&diff.stdout)
    );

    let (code, rollback) = env.json(&["rollback", "0", "--json"]);
    assert_eq!(code, 0, "rollback failed: {rollback}");
    assert_ok(&rollback, "rollback");
    assert_eq!(rollback["data"]["head"].as_u64(), Some(0));

    let (code, sandbox_hash) = run_script(
        &env,
        "find . -type f -print0 | sort -z | xargs -0 sha256sum | sha256sum",
    );
    assert_eq!(code, 0, "sandbox hash failed: {sandbox_hash}");
    assert_eq!(
        sandbox_hash["data"]["stdout"].as_str().unwrap_or("").trim(),
        before_hash
    );

    let (code, sandbox_links) = run_script(&env, "find . -type l | wc -l");
    assert_eq!(code, 0, "sandbox symlink count failed: {sandbox_links}");
    assert_eq!(
        sandbox_links["data"]["stdout"]
            .as_str()
            .unwrap_or("")
            .trim(),
        before_links.to_string()
    );
}

#[test]
fn orphan_processes_are_killed_at_step_end() {
    let env = TestEnv::new();
    if !require_sandbox(&env) {
        return;
    }
    env.init();
    let script_path = env.workspace.path().join("orphan.py");
    fs::write(&script_path, "import time\ntime.sleep(60)\n").expect("orphan script");

    let (code, value) = run_script(&env, "python3 orphan.py & sleep 0.2");
    assert_eq!(code, 0, "run failed: {value}");
    assert_ok(&value, "run");
    assert!(
        value["data"]["orphans_reaped"].as_u64().unwrap_or(0) >= 1,
        "orphan was not reaped: {value}"
    );
    thread::sleep(Duration::from_millis(100));
    assert!(
        !process_cmdline_contains("orphan.py"),
        "orphan.py still running after step end"
    );
}

#[test]
fn landlock_blocks_out_of_tree_writes() {
    let env = TestEnv::new();
    if !require_sandbox(&env) {
        return;
    }
    env.init();
    let target = PathBuf::from(format!("/tmp/cowbox-escape-{}", unique_suffix()));
    let script = format!("echo evil > {}", target.display());
    let (code, value) = run_script(&env, &script);
    assert_eq!(code, 2, "unexpected shell exit: {value}");
    assert_ok(&value, "run");
    assert_ne!(value["data"]["exit_code"].as_i64(), Some(0));
    assert!(
        value["data"]["stderr"]
            .as_str()
            .unwrap_or("")
            .contains("Permission denied"),
        "Landlock did not produce EACCES: {value}"
    );
    assert!(
        !target.exists(),
        "out-of-tree write escaped: {}",
        target.display()
    );

    let (code, log) = env.json(&["log", "--json"]);
    assert_eq!(code, 0);
    assert_eq!(
        log["data"]["dag"]["nodes"]["1"]["exit_code"].as_i64(),
        Some(2)
    );
}

#[test]
fn commit_applies_all_change_kinds_and_closes_the_session() {
    let env = TestEnv::new();
    if !require_sandbox(&env) {
        return;
    }
    fs::write(env.workspace.path().join("keep.txt"), "keep\n").expect("keep");
    fs::write(env.workspace.path().join("modify.txt"), "old\n").expect("modify");
    fs::write(env.workspace.path().join("delete.txt"), "delete\n").expect("delete");
    symlink("keep.txt", env.workspace.path().join("link")).expect("link");
    create_sqlite(&env.workspace.path().join("data/app.sqlite"));
    env.init();

    let script = r"printf added > added.txt; printf changed > modify.txt; rm -f delete.txt; rm -f link; ln -s modify.txt link; printf '\000\001' > bin.dat; printf x > setuid.bin; chmod 4755 setuid.bin";
    let (code, run) = run_script(&env, script);
    assert_eq!(code, 0, "run failed: {run}");
    assert_ok(&run, "run");

    let (code, commit) = env.json(&["commit", "--yes", "--json"]);
    assert_eq!(code, 0, "commit failed: {commit}");
    assert_ok(&commit, "commit");
    assert_eq!(commit["data"]["committed"].as_bool(), Some(true));

    assert_eq!(
        fs::read_to_string(env.workspace.path().join("added.txt")).expect("added"),
        "added"
    );
    assert_eq!(
        fs::read_to_string(env.workspace.path().join("modify.txt")).expect("modify"),
        "changed"
    );
    assert!(!env.workspace.path().join("delete.txt").exists());
    assert_eq!(
        fs::read_link(env.workspace.path().join("link")).expect("link"),
        Path::new("modify.txt")
    );
    assert_eq!(
        fs::read(env.workspace.path().join("bin.dat")).expect("binary"),
        vec![0, 1]
    );
    let setuid_mode = fs::metadata(env.workspace.path().join("setuid.bin"))
        .expect("setuid file")
        .permissions()
        .mode();
    assert_eq!(
        setuid_mode & 0o6000,
        0,
        "setuid/setgid bits were not cleared"
    );
    assert_eq!(
        sqlite_integrity(&env.workspace.path().join("data/app.sqlite")),
        "ok"
    );

    let (code, second) = env.json(&["commit", "--json"]);
    assert_eq!(code, 1);
    assert_eq!(second["ok"].as_bool(), Some(false));
    assert_eq!(second["error"]["code"].as_str(), Some("NO_ACTIVE_SESSION"));
}

#[test]
fn commit_requires_explicit_approval_without_tty() {
    let env = TestEnv::new();
    if !require_sandbox(&env) {
        return;
    }
    fs::write(env.workspace.path().join("file.txt"), "base\n").expect("file");
    env.init();
    let (code, run) = run_script(&env, "printf sandbox > file.txt");
    assert_eq!(code, 0, "run failed: {run}");
    assert_ok(&run, "run");

    let (code, blocked) = env.json(&["commit", "--json"]);
    assert_eq!(code, 2, "commit unexpectedly succeeded: {blocked}");
    assert_eq!(blocked["ok"].as_bool(), Some(false));
    assert_eq!(
        blocked["error"]["code"].as_str(),
        Some("CONFIRMATION_REQUIRED")
    );
    assert_eq!(
        fs::read_to_string(env.workspace.path().join("file.txt")).expect("host file"),
        "base\n"
    );

    let (code, log) = env.json(&["log", "--json"]);
    assert_eq!(code, 0);
    assert_eq!(log["data"]["dag"]["head_step"].as_u64(), Some(1));

    let (code, preview) = env.json(&["commit", "--dry-run", "--json"]);
    assert_eq!(code, 0, "dry-run failed: {preview}");
    assert_eq!(preview["data"]["committed"].as_bool(), Some(false));
    assert_eq!(preview["data"]["dry_run"].as_bool(), Some(true));

    let (code, committed) = env.json(&["commit", "--yes", "--json"]);
    assert_eq!(code, 0, "approved commit failed: {committed}");
    assert_eq!(committed["data"]["committed"].as_bool(), Some(true));
    assert_eq!(
        fs::read_to_string(env.workspace.path().join("file.txt")).expect("host file"),
        "sandbox"
    );
}

#[test]
fn sessions_and_gc_preserve_active_and_recent_state() {
    let env = TestEnv::new();
    if !require_sandbox(&env) {
        return;
    }
    fs::write(env.workspace.path().join("file.txt"), "base\n").expect("file");
    env.init();
    let (code, run) = run_script(&env, "printf sandbox > file.txt");
    assert_eq!(code, 0, "run failed: {run}");
    assert_ok(&run, "run");
    let session_id = run["session"]["id"]
        .as_str()
        .expect("session id")
        .to_string();
    let session_dir = env.state.path().join("sessions").join(&session_id);
    assert!(session_dir.is_dir());

    let (code, sessions) = env.json(&["sessions", "--json"]);
    assert_eq!(code, 0, "sessions failed: {sessions}");
    assert_ok(&sessions, "sessions");
    let active = sessions["data"]["sessions"]
        .as_array()
        .expect("sessions")
        .iter()
        .find(|session| session["session_id"].as_str() == Some(session_id.as_str()))
        .expect("active session listed");
    assert_eq!(active["status"].as_str(), Some("active"));
    assert_eq!(active["active"].as_bool(), Some(true));

    let (code, gc_active) = env.json(&["gc", "--all", "--yes", "--json"]);
    assert_eq!(code, 0, "active gc failed: {gc_active}");
    assert_eq!(
        gc_active["data"]["removed"].as_array().map(Vec::len),
        Some(0)
    );
    assert_eq!(gc_active["data"]["skipped_active"].as_u64(), Some(1));
    assert!(session_dir.is_dir());

    let (code, commit) = env.json(&["commit", "--yes", "--json"]);
    assert_eq!(code, 0, "commit failed: {commit}");
    assert_ok(&commit, "commit");

    let (code, recent) = env.json(&["gc", "--yes", "--json"]);
    assert_eq!(code, 0, "recent gc failed: {recent}");
    assert_eq!(recent["data"]["removed"].as_array().map(Vec::len), Some(0));
    assert_eq!(recent["data"]["skipped_recent"].as_u64(), Some(1));
    assert!(session_dir.is_dir());

    let (code, blocked) = env.json(&["gc", "--all", "--json"]);
    assert_eq!(
        code, 2,
        "gc unexpectedly ran without confirmation: {blocked}"
    );
    assert_eq!(
        blocked["error"]["code"].as_str(),
        Some("CONFIRMATION_REQUIRED")
    );
    assert!(session_dir.is_dir());

    let (code, dry_run) = env.json(&["gc", "--all", "--dry-run", "--json"]);
    assert_eq!(code, 0, "gc dry-run failed: {dry_run}");
    assert_eq!(
        dry_run["data"]["candidates"].as_array().map(Vec::len),
        Some(1)
    );
    assert_eq!(dry_run["data"]["removed"].as_array().map(Vec::len), Some(0));
    assert!(session_dir.is_dir());

    let (code, removed) = env.json(&["gc", "--all", "--yes", "--json"]);
    assert_eq!(code, 0, "gc failed: {removed}");
    assert_ok(&removed, "gc");
    assert_eq!(removed["data"]["removed"].as_array().map(Vec::len), Some(1));
    assert!(removed["data"]["freed_bytes"].as_u64().unwrap_or_default() > 0);
    assert!(!session_dir.exists());

    let (code, empty) = env.json(&["sessions", "--json"]);
    assert_eq!(code, 0);
    assert!(
        empty["data"]["sessions"]
            .as_array()
            .expect("sessions")
            .is_empty()
    );
}

#[test]
fn commit_interruption_before_commit_point_restores_host_and_preserves_backup() {
    let env = TestEnv::new();
    if !require_sandbox(&env) {
        return;
    }
    let names = ["a.txt", "b.txt", "c.txt"];
    for name in names {
        fs::write(env.workspace.path().join(name), "before\n").expect("fixture file");
    }
    env.init();
    let (code, run) = run_script(
        &env,
        "printf after > a.txt; printf after > b.txt; printf after > c.txt",
    );
    assert_eq!(code, 0, "run failed: {run}");
    let session_id = run["session"]["id"]
        .as_str()
        .expect("session id")
        .to_string();

    let crashed = crash_cowbox(
        &env,
        &["commit", "--yes", "--json"],
        "commit-after-first-write",
    );
    assert_sigkill(&crashed);
    let journal = only_commit_journal(&env, &session_id);
    assert_eq!(journal_phase(&journal), "applying");
    for name in names {
        assert_eq!(
            fs::read_to_string(journal.join("backup").join(name)).expect("backup"),
            "before\n",
            "backup for {name} was removed or rewritten"
        );
    }
    let contents = names
        .iter()
        .map(|name| fs::read_to_string(env.workspace.path().join(name)).expect("host"))
        .collect::<Vec<_>>();
    assert!(
        contents.iter().any(|value| value == "after"),
        "the interrupted commit wrote no host file: {contents:?}"
    );
    assert!(
        contents.iter().any(|value| value == "before\n"),
        "the interrupted commit unexpectedly wrote every host file: {contents:?}"
    );
    assert_eq!(active_pointers(&env).len(), 1);

    let (code, blocked) = env.json(&["run", "--json", "--", "true"]);
    assert_eq!(code, 2, "run should require recovery: {blocked}");
    assert_eq!(
        blocked["error"]["code"].as_str(),
        Some("COMMIT_RECOVERY_REQUIRED")
    );

    let (code, recovered) = env.json(&["commit", "--recover", "--yes", "--json"]);
    assert_eq!(code, 0, "recovery failed: {recovered}");
    assert_eq!(recovered["data"]["recovered"].as_bool(), Some(true));
    assert_eq!(recovered["data"]["action"].as_str(), Some("restore_host"));
    assert_eq!(recovered["data"]["restored"].as_u64(), Some(3));
    for name in names {
        assert_eq!(
            fs::read_to_string(env.workspace.path().join(name)).expect("restored host"),
            "before\n"
        );
    }
    assert_eq!(journal_phase(&journal), "rolled_back");
    assert!(!journal.join("backup").exists());
    assert_eq!(active_pointers(&env).len(), 1);
    let (code, log) = env.json(&["log", "--json"]);
    assert_eq!(code, 0, "session did not remain usable: {log}");

    let (code, repeated) = env.json(&["commit", "--recover", "--yes", "--json"]);
    assert_eq!(code, 0, "repeated recovery failed: {repeated}");
    assert_eq!(repeated["data"]["recovered"].as_bool(), Some(false));
    assert_eq!(repeated["data"]["already_complete"].as_bool(), Some(true));

    let (code, committed) = env.json(&["commit", "--yes", "--json"]);
    assert_eq!(code, 0, "retry after recovery failed: {committed}");
    assert_eq!(committed["data"]["committed"].as_bool(), Some(true));
    for name in names {
        assert_eq!(
            fs::read_to_string(env.workspace.path().join(name)).expect("committed host"),
            "after"
        );
    }
    assert!(active_pointers(&env).is_empty());
    assert_eq!(session_status(&env, &session_id), "committed");
}

#[test]
fn interrupted_recovery_is_idempotent_and_can_resume() {
    let env = TestEnv::new();
    if !require_sandbox(&env) {
        return;
    }
    let names = ["a.txt", "b.txt"];
    for name in names {
        fs::write(env.workspace.path().join(name), "before\n").expect("fixture file");
    }
    env.init();
    let (code, run) = run_script(&env, "printf after > a.txt; printf after > b.txt");
    assert_eq!(code, 0, "run failed: {run}");
    let session_id = run["session"]["id"]
        .as_str()
        .expect("session id")
        .to_string();

    let crashed = crash_cowbox(
        &env,
        &["commit", "--yes", "--json"],
        "commit-after-first-write",
    );
    assert_sigkill(&crashed);
    let journal = only_commit_journal(&env, &session_id);

    let interrupted = crash_cowbox(
        &env,
        &["commit", "--recover", "--yes", "--json"],
        "recover-after-first-restore",
    );
    assert_sigkill(&interrupted);
    assert_eq!(journal_phase(&journal), "restoring");
    for name in names {
        assert_eq!(
            fs::read_to_string(journal.join("backup").join(name)).expect("backup"),
            "before\n"
        );
    }
    let partial = names
        .iter()
        .map(|name| fs::read_to_string(env.workspace.path().join(name)).expect("host"))
        .collect::<Vec<_>>();
    assert!(
        partial.iter().any(|value| value == "before\n")
            && partial.iter().any(|value| value == "after"),
        "recovery interrupt did not leave a partial restore: {partial:?}"
    );
    assert_eq!(active_pointers(&env).len(), 1);

    let (code, recovered) = env.json(&["commit", "--recover", "--yes", "--json"]);
    assert_eq!(code, 0, "resumed recovery failed: {recovered}");
    assert_eq!(recovered["data"]["action"].as_str(), Some("restore_host"));
    assert_eq!(recovered["data"]["restored"].as_u64(), Some(2));
    for name in names {
        assert_eq!(
            fs::read_to_string(env.workspace.path().join(name)).expect("restored host"),
            "before\n"
        );
    }
    assert_eq!(journal_phase(&journal), "rolled_back");
    assert!(!journal.join("backup").exists());
    let (code, log) = env.json(&["log", "--json"]);
    assert_eq!(code, 0, "session is not active after recovery: {log}");
}

#[test]
fn applied_commit_without_session_finalization_is_completed_on_restart() {
    let env = TestEnv::new();
    if !require_sandbox(&env) {
        return;
    }
    let names = ["a.txt", "b.txt"];
    for name in names {
        fs::write(env.workspace.path().join(name), "before\n").expect("fixture file");
    }
    env.init();
    let (code, run) = run_script(&env, "printf after > a.txt; printf after > b.txt");
    assert_eq!(code, 0, "run failed: {run}");
    let session_id = run["session"]["id"]
        .as_str()
        .expect("session id")
        .to_string();

    let crashed = crash_cowbox(&env, &["commit", "--yes", "--json"], "commit-after-applied");
    assert_sigkill(&crashed);
    let journal = only_commit_journal(&env, &session_id);
    assert_eq!(journal_phase(&journal), "applied");
    for name in names {
        assert_eq!(
            fs::read_to_string(env.workspace.path().join(name)).expect("committed host"),
            "after"
        );
        assert_eq!(
            fs::read_to_string(journal.join("backup").join(name)).expect("backup"),
            "before\n"
        );
    }
    assert_eq!(session_status(&env, &session_id), "active");
    assert_eq!(active_pointers(&env).len(), 1);

    let (code, blocked) = env.json(&["commit", "--yes", "--json"]);
    assert_eq!(code, 2, "commit should require recovery: {blocked}");
    assert_eq!(
        blocked["error"]["code"].as_str(),
        Some("COMMIT_RECOVERY_REQUIRED")
    );

    let (code, recovered) = env.json(&["commit", "--recover", "--yes", "--json"]);
    assert_eq!(code, 0, "finalizing recovery failed: {recovered}");
    assert_eq!(recovered["data"]["recovered"].as_bool(), Some(true));
    assert_eq!(
        recovered["data"]["action"].as_str(),
        Some("finalize_commit")
    );
    assert_eq!(recovered["data"]["restored"].as_u64(), Some(0));
    for name in names {
        assert_eq!(
            fs::read_to_string(env.workspace.path().join(name)).expect("committed host"),
            "after"
        );
    }
    assert_eq!(session_status(&env, &session_id), "committed");
    assert_eq!(journal_phase(&journal), "complete");
    assert!(!journal.join("backup").exists());
    assert!(active_pointers(&env).is_empty());

    let (code, repeated) = env.json(&["commit", "--recover", "--yes", "--json"]);
    assert_eq!(code, 0, "repeated finalization failed: {repeated}");
    assert_eq!(repeated["data"]["recovered"].as_bool(), Some(false));
    assert_eq!(repeated["data"]["already_complete"].as_bool(), Some(true));

    let (code, sessions) = env.json(&["sessions", "--json"]);
    assert_eq!(code, 0);
    let session = sessions["data"]["sessions"]
        .as_array()
        .expect("sessions")
        .iter()
        .find(|session| session["session_id"].as_str() == Some(session_id.as_str()))
        .expect("session listed");
    assert_eq!(session["status"].as_str(), Some("committed"));
    assert_eq!(session["active"].as_bool(), Some(false));

    let (code, discarded) = env.json(&["discard", "--yes", "--json"]);
    assert_eq!(code, 0, "discard after recovery failed: {discarded}");
    assert_eq!(discarded["data"]["discarded"].as_bool(), Some(false));
}

#[test]
fn stale_active_pointer_after_session_save_is_finalized_idempotently() {
    let env = TestEnv::new();
    if !require_sandbox(&env) {
        return;
    }
    fs::write(env.workspace.path().join("file.txt"), "before\n").expect("fixture file");
    env.init();
    let (code, run) = run_script(&env, "printf after > file.txt");
    assert_eq!(code, 0, "run failed: {run}");
    let session_id = run["session"]["id"]
        .as_str()
        .expect("session id")
        .to_string();

    let crashed = crash_cowbox(
        &env,
        &["commit", "--yes", "--json"],
        "commit-after-session-save",
    );
    assert_abort(&crashed);
    let journal = only_commit_journal(&env, &session_id);
    assert_eq!(
        fs::read_to_string(env.workspace.path().join("file.txt")).expect("committed host"),
        "after"
    );
    assert_eq!(session_status(&env, &session_id), "committed");
    assert_eq!(active_pointers(&env).len(), 1);
    assert_eq!(journal_phase(&journal), "applied");
    assert!(journal.join("backup/file.txt").exists());

    let (code, recovered) = env.json(&["commit", "--recover", "--yes", "--json"]);
    assert_eq!(code, 0, "stale-pointer recovery failed: {recovered}");
    assert_eq!(
        recovered["data"]["action"].as_str(),
        Some("finalize_commit")
    );
    assert_eq!(
        fs::read_to_string(env.workspace.path().join("file.txt")).expect("committed host"),
        "after"
    );
    assert_eq!(session_status(&env, &session_id), "committed");
    assert_eq!(journal_phase(&journal), "complete");
    assert!(!journal.join("backup").exists());
    assert!(active_pointers(&env).is_empty());

    let (code, repeated) = env.json(&["commit", "--recover", "--yes", "--json"]);
    assert_eq!(code, 0, "repeated recovery failed: {repeated}");
    assert_eq!(repeated["data"]["recovered"].as_bool(), Some(false));
    assert_eq!(repeated["data"]["already_complete"].as_bool(), Some(true));
}

#[test]
fn gc_skips_a_committed_session_until_its_journal_is_finalized() {
    let env = TestEnv::new();
    if !require_sandbox(&env) {
        return;
    }
    fs::write(env.workspace.path().join("file.txt"), "before\n").expect("fixture file");
    env.init();
    let (code, run) = run_script(&env, "printf after > file.txt");
    assert_eq!(code, 0, "run failed: {run}");
    let session_id = run["session"]["id"]
        .as_str()
        .expect("session id")
        .to_string();

    let crashed = crash_cowbox(
        &env,
        &["commit", "--yes", "--json"],
        "commit-after-active-clear",
    );
    assert_abort(&crashed);
    let journal = only_commit_journal(&env, &session_id);
    assert_eq!(journal_phase(&journal), "applied");
    assert!(journal.join("backup/file.txt").exists());
    assert_eq!(session_status(&env, &session_id), "committed");
    assert!(active_pointers(&env).is_empty());

    let (code, gc) = env.json(&["gc", "--all", "--yes", "--json"]);
    assert_eq!(code, 0, "gc failed: {gc}");
    assert_eq!(gc["data"]["removed"].as_array().map(Vec::len), Some(0));
    assert_eq!(gc["data"]["skipped_recovery"].as_u64(), Some(1));
    assert!(session_dir(&env, &session_id).is_dir());
    assert!(journal.join("backup/file.txt").exists());

    let (code, recovered) = env.json(&["commit", "--recover", "--yes", "--json"]);
    assert_eq!(code, 0, "finalization failed: {recovered}");
    assert_eq!(journal_phase(&journal), "complete");
    assert!(!journal.join("backup").exists());

    let (code, gc) = env.json(&["gc", "--all", "--yes", "--json"]);
    assert_eq!(code, 0, "gc after finalization failed: {gc}");
    assert_eq!(gc["data"]["removed"].as_array().map(Vec::len), Some(1));
    assert!(!session_dir(&env, &session_id).exists());
}

#[test]
fn recovery_refuses_external_edits_and_keeps_the_backup() {
    let env = TestEnv::new();
    if !require_sandbox(&env) {
        return;
    }
    let names = ["a.txt", "b.txt"];
    for name in names {
        fs::write(env.workspace.path().join(name), "before\n").expect("fixture file");
    }
    env.init();
    let (code, run) = run_script(&env, "printf after > a.txt; printf after > b.txt");
    assert_eq!(code, 0, "run failed: {run}");
    let session_id = run["session"]["id"]
        .as_str()
        .expect("session id")
        .to_string();

    let crashed = crash_cowbox(
        &env,
        &["commit", "--yes", "--json"],
        "commit-after-first-write",
    );
    assert_sigkill(&crashed);
    let journal = only_commit_journal(&env, &session_id);
    let drifted = names
        .iter()
        .find(|name| {
            fs::read_to_string(env.workspace.path().join(name))
                .map(|value| value == "before\n")
                .unwrap_or(false)
        })
        .expect("one host file should remain at its pre-state");
    fs::write(env.workspace.path().join(drifted), "external\n").expect("external edit");

    let (code, refused) = env.json(&["commit", "--recover", "--yes", "--json"]);
    assert_eq!(code, 1, "drift recovery unexpectedly succeeded: {refused}");
    assert_eq!(refused["error"]["code"].as_str(), Some("COMMIT_FAILED"));
    assert!(
        refused["error"]["message"]
            .as_str()
            .unwrap_or("")
            .contains("changed after the recorded pre/post state"),
        "error did not identify external drift: {refused}"
    );
    assert_eq!(
        fs::read_to_string(env.workspace.path().join(drifted)).expect("external host file"),
        "external\n"
    );
    assert_eq!(
        fs::read_to_string(journal.join("backup").join(drifted)).expect("backup"),
        "before\n"
    );
    assert_eq!(journal_phase(&journal), "applying");
    assert_eq!(active_pointers(&env).len(), 1);
}

#[test]
fn commit_refuses_fifo_entries() {
    let env = TestEnv::new();
    if !require_sandbox(&env) {
        return;
    }
    env.init();
    let (code, run) = run_script(&env, "mkfifo fifo");
    assert_eq!(code, 0, "mkfifo failed: {run}");
    let (code, commit) = env.json(&["commit", "--yes", "--json"]);
    assert_eq!(code, 1);
    assert_eq!(commit["ok"].as_bool(), Some(false));
    assert!(!env.workspace.path().join("fifo").exists());
}

#[test]
fn commit_refuses_host_drift_but_force_writes_with_a_journal() {
    let env = TestEnv::new();
    if !require_sandbox(&env) {
        return;
    }
    fs::write(env.workspace.path().join("file.txt"), "base\n").expect("file");
    env.init();
    let (code, run) = run_script(&env, "printf sandbox > file.txt");
    assert_eq!(code, 0, "run failed: {run}");
    fs::write(env.workspace.path().join("file.txt"), "host-edit\n").expect("host edit");

    let (code, conflict) = env.json(&["commit", "--yes", "--json"]);
    assert_eq!(code, 2, "expected conflict: {conflict}");
    assert_eq!(conflict["ok"].as_bool(), Some(false));
    assert_eq!(conflict["error"]["code"].as_str(), Some("COMMIT_CONFLICT"));
    assert_eq!(
        fs::read_to_string(env.workspace.path().join("file.txt")).expect("host"),
        "host-edit\n"
    );

    let (code, forced) = env.json(&["commit", "--force", "--yes", "--json"]);
    assert_eq!(code, 0, "force commit failed: {forced}");
    assert_ok(&forced, "commit");
    assert_eq!(forced["data"]["committed"].as_bool(), Some(true));
    assert_eq!(
        fs::read_to_string(env.workspace.path().join("file.txt")).expect("host"),
        "sandbox"
    );
    let journal = forced["data"]["journal"].as_str().expect("journal path");
    assert!(Path::new(journal).join("ops.json").exists());
}

#[test]
fn manifest_captures_xattrs_hardlinks_symlinks_and_rejects_host_drift() {
    let env = TestEnv::new();
    if !require_sandbox(&env) {
        return;
    }
    let payload = env.workspace.path().join("payload.txt");
    let hardlink = env.workspace.path().join("hard.txt");
    let symlink_path = env.workspace.path().join("link");
    fs::write(&payload, "base\n").expect("payload");
    fs::hard_link(&payload, &hardlink).expect("hardlink fixture");
    symlink("payload.txt", &symlink_path).expect("symlink fixture");
    set_user_xattr(&payload, "user.cowbox.fixture", b"base");
    env.init();

    let (code, run) = env.json(&["run", "--json", "--", "true"]);
    assert_eq!(code, 0, "initial run failed: {run}");
    let (code, log) = env.json(&["log", "--json"]);
    assert_eq!(code, 0, "log failed: {log}");
    let session_id = log["data"]["dag"]["session_id"]
        .as_str()
        .expect("session id");
    let manifest = fs::read_to_string(
        session_dir(&env, session_id)
            .join("base")
            .join("manifest.jsonl"),
    )
    .expect("base manifest");
    let entries = manifest
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).expect("manifest entry"))
        .collect::<Vec<_>>();
    let entry = |path: &str| {
        entries
            .iter()
            .find(|entry| entry["rel_path"].as_str() == Some(path))
            .unwrap_or_else(|| panic!("missing manifest entry {path}: {entries:?}"))
    };
    let payload_entry = entry("payload.txt");
    let hardlink_entry = entry("hard.txt");
    assert_eq!(payload_entry["dev"], hardlink_entry["dev"]);
    assert_eq!(payload_entry["ino"], hardlink_entry["ino"]);
    assert_eq!(payload_entry["nlink"].as_u64(), Some(2));
    assert_eq!(
        payload_entry["xattr_digest"],
        hardlink_entry["xattr_digest"]
    );
    assert!(payload_entry["xattr_digest"].is_string());
    assert_eq!(entry("link")["kind"].as_str(), Some("symlink"));
    assert_eq!(
        entry("link")["symlink_target"].as_str(),
        Some("payload.txt")
    );

    let (code, changed) = run_script(
        &env,
        "python3 -c \"import os; os.setxattr('payload.txt', 'user.cowbox.fixture', b'sandbox')\"",
    );
    assert_eq!(code, 0, "xattr run failed: {changed}");
    assert_eq!(user_xattr(&payload, "user.cowbox.fixture"), b"base");
    assert_eq!(user_xattr(&hardlink, "user.cowbox.fixture"), b"base");
    let (code, diff) = env.json(&["diff", "--json"]);
    assert_eq!(code, 0, "diff failed: {diff}");
    assert!(
        diff["data"]["changes"]
            .as_array()
            .expect("diff changes")
            .iter()
            .any(|change| {
                change["rel_path"].as_str() == Some("payload.txt")
                    && change["kind"].as_str() == Some("modified")
            }),
        "xattr-only sandbox change was not detected: {diff}"
    );

    set_user_xattr(&payload, "user.cowbox.fixture", b"host-drift");
    fs::hard_link(&payload, env.workspace.path().join("extra.txt")).expect("host link drift");
    let (code, conflict) = env.json(&["commit", "--yes", "--json"]);
    assert_eq!(code, 2, "host drift was not refused: {conflict}");
    assert_eq!(conflict["error"]["code"].as_str(), Some("COMMIT_CONFLICT"));
    assert_eq!(user_xattr(&payload, "user.cowbox.fixture"), b"host-drift");
    assert_eq!(fs::metadata(&payload).expect("payload metadata").nlink(), 3);
    assert_eq!(
        fs::read_link(&symlink_path).expect("symlink target"),
        Path::new("payload.txt")
    );
}

#[test]
fn first_run_captures_existing_files_across_wide_deep_tree_before_command() {
    let env = TestEnv::new();
    if !require_sandbox(&env) {
        return;
    }
    let outside = env.state.path().join("outside-target");
    fs::create_dir_all(&outside).expect("outside target");
    fs::write(outside.join("not-in-workspace.txt"), "outside\n").expect("outside file");
    for branch in 0..40 {
        let path = env.workspace.path().join(format!(
            "branches/branch-{branch:02}/deep/nested/payload.txt"
        ));
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("deep directory");
        }
        fs::write(path, format!("branch {branch}\n")).expect("deep file");
    }
    symlink(&outside, env.workspace.path().join("outside-link")).expect("directory symlink");
    env.init();

    let (code, run) = run_script(
        &env,
        "printf 'sandbox changed\\n' > branches/branch-00/deep/nested/payload.txt",
    );
    assert_eq!(code, 0, "first run failed: {run}");
    let (code, log) = env.json(&["log", "--json"]);
    assert_eq!(code, 0, "log failed: {log}");
    let session_id = log["data"]["dag"]["session_id"]
        .as_str()
        .expect("session id");
    let manifest = fs::read_to_string(
        session_dir(&env, session_id)
            .join("base")
            .join("manifest.jsonl"),
    )
    .expect("base manifest");
    let entries = manifest
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).expect("manifest entry"))
        .collect::<Vec<_>>();
    for branch in 0..40 {
        let rel_path = format!("branches/branch-{branch:02}/deep/nested/payload.txt");
        assert!(
            entries
                .iter()
                .any(|entry| entry["rel_path"].as_str() == Some(&rel_path)),
            "missing deep manifest entry {rel_path}"
        );
    }
    assert!(entries.iter().any(|entry| {
        entry["rel_path"].as_str() == Some("outside-link")
            && entry["kind"].as_str() == Some("symlink")
            && entry["symlink_target"].as_str() == outside.to_str()
    }));
    assert!(
        !entries.iter().any(|entry| entry["rel_path"]
            .as_str()
            .is_some_and(|path| path.starts_with("outside-link/"))),
        "manifest followed the directory symlink"
    );

    let (code, diff) = env.json(&["diff", "--json"]);
    assert_eq!(code, 0, "diff failed: {diff}");
    assert!(
        diff["data"]["changes"]
            .as_array()
            .expect("diff changes")
            .iter()
            .any(|change| {
                change["rel_path"].as_str() == Some("branches/branch-00/deep/nested/payload.txt")
                    && change["kind"].as_str() == Some("modified")
            }),
        "first command change was not compared with its base manifest entry: {diff}"
    );
}

#[test]
fn commit_keeps_sqlite_wal_sidecars_consistent() {
    let env = TestEnv::new();
    if !require_sandbox(&env) {
        return;
    }
    let database = env.workspace.path().join("data/app.sqlite");
    create_sqlite(&database);
    env.init();
    let (code, run) = run_script(
        &env,
        r#"python3 - <<'PY'
import os
import sqlite3

conn = sqlite3.connect("data/app.sqlite")
conn.execute("pragma journal_mode=wal")
conn.execute("insert into users (email) values ('wal@example.com')")
conn.commit()
os._exit(0)
PY"#,
    );
    assert_eq!(code, 0, "WAL run failed: {run}");

    let (code, diff) = env.json(&["diff", "--json"]);
    assert_eq!(code, 0, "diff failed: {diff}");
    let paths = diff["data"]["changes"]
        .as_array()
        .expect("changes")
        .iter()
        .filter_map(|change| change["rel_path"].as_str())
        .collect::<Vec<_>>();
    assert!(
        paths.contains(&"data/app.sqlite-wal"),
        "missing WAL: {paths:?}"
    );
    assert!(
        paths.contains(&"data/app.sqlite-shm"),
        "missing SHM: {paths:?}"
    );

    let (code, commit) = env.json(&["commit", "--yes", "--json"]);
    assert_eq!(code, 0, "commit failed: {commit}");
    assert_ok(&commit, "commit");
    assert!(env.workspace.path().join("data/app.sqlite-wal").exists());
    assert!(env.workspace.path().join("data/app.sqlite-shm").exists());
    assert_eq!(
        sqlite_integrity(&env.workspace.path().join("data/app.sqlite")),
        "ok"
    );
    assert_eq!(sqlite_user_count(&database), 2);
}

#[test]
fn commit_removes_stale_sqlite_sidecars_not_present_in_the_sandbox() {
    let env = TestEnv::new();
    if !require_sandbox(&env) {
        return;
    }
    let database = env.workspace.path().join("data/app.sqlite");
    create_sqlite(&database);
    fs::write(env.workspace.path().join("data/app.sqlite-wal"), b"").expect("stale sidecar");
    env.init();
    let (code, run) = run_script(
        &env,
        r#"python3 - <<'PY'
import sqlite3

conn = sqlite3.connect("data/app.sqlite")
conn.execute("insert into users (email) values ('stale-sidecar@example.com')")
conn.commit()
conn.close()
PY"#,
    );
    assert_eq!(code, 0, "sqlite rewrite failed: {run}");

    let (code, commit) = env.json(&["commit", "--yes", "--json"]);
    assert_eq!(code, 0, "commit failed: {commit}");
    assert_ok(&commit, "commit");
    assert!(
        !env.workspace.path().join("data/app.sqlite-wal").exists(),
        "stale WAL sidecar survived the commit"
    );
    assert_eq!(sqlite_user_count(&database), 2);
}

#[test]
fn commit_refuses_unexpected_host_sidecars_created_after_init() {
    let env = TestEnv::new();
    if !require_sandbox(&env) {
        return;
    }
    let database = env.workspace.path().join("data/app.sqlite");
    create_sqlite(&database);
    env.init();
    let (code, run) = run_script(
        &env,
        r#"python3 - <<'PY'
import sqlite3

conn = sqlite3.connect("data/app.sqlite")
conn.execute("insert into users (email) values ('external-sidecar@example.com')")
conn.commit()
conn.close()
PY"#,
    );
    assert_eq!(code, 0, "sqlite rewrite failed: {run}");
    fs::write(env.workspace.path().join("data/app.sqlite-wal"), b"").expect("external sidecar");

    let (code, conflict) = env.json(&["commit", "--yes", "--json"]);
    assert_eq!(code, 2, "unexpected sidecar was not rejected: {conflict}");
    assert_eq!(conflict["error"]["code"].as_str(), Some("COMMIT_CONFLICT"));
    assert!(env.workspace.path().join("data/app.sqlite-wal").exists());

    let (code, forced) = env.json(&["commit", "--force", "--yes", "--json"]);
    assert_eq!(code, 0, "forced commit failed: {forced}");
    assert!(!env.workspace.path().join("data/app.sqlite-wal").exists());
    assert_eq!(sqlite_user_count(&database), 2);
}

#[test]
fn timeout_returns_124_and_leaves_no_process() {
    let env = TestEnv::new();
    if !require_sandbox(&env) {
        return;
    }
    env.init();
    fs::write(
        env.workspace.path().join("timeout.py"),
        "import time\ntime.sleep(60)\n",
    )
    .expect("timeout script");
    let (code, value) = env.json(&[
        "run",
        "--json",
        "--timeout",
        "1s",
        "--",
        "sh",
        "-c",
        "python3 timeout.py",
    ]);
    assert_eq!(code, 124, "timeout did not propagate 124: {value}");
    assert_ok(&value, "run");
    assert_eq!(value["data"]["timed_out"].as_bool(), Some(true));
    assert_eq!(value["data"]["exit_code"].as_i64(), Some(124));
    thread::sleep(Duration::from_millis(100));
    assert!(
        !process_cmdline_contains("timeout.py"),
        "timeout.py still running after timeout"
    );

    let (code, log) = env.json(&["log", "--json"]);
    assert_eq!(code, 0);
    assert_eq!(
        log["data"]["dag"]["nodes"]["1"]["timed_out"].as_bool(),
        Some(true)
    );
}

#[test]
fn demo_runs_isolated_and_cleans_up() {
    let env = TestEnv::new();
    if !require_sandbox(&env) {
        return;
    }
    let (code, demo) = env.json(&["demo", "--json"]);
    assert_eq!(code, 0, "demo failed: {demo}");
    assert_ok(&demo, "demo");
    assert_eq!(demo["data"]["host_unchanged"].as_bool(), Some(true));
    assert_eq!(demo["data"]["kept"].as_bool(), Some(false));
    assert_eq!(demo["data"]["steps"].as_u64(), Some(2));
    assert!(
        demo["data"]["summary"]["files"]
            .as_u64()
            .unwrap_or_default()
            > 0
    );
    let workspace = demo["data"]["workspace"]
        .as_str()
        .expect("demo workspace path");
    let state_home = demo["data"]["state_home"]
        .as_str()
        .expect("demo state path");
    assert!(!Path::new(workspace).exists());
    assert!(!Path::new(state_home).exists());
}

#[test]
fn demo_keep_preserves_the_fixture() {
    let env = TestEnv::new();
    if !require_sandbox(&env) {
        return;
    }
    let (code, demo) = env.json(&["demo", "--keep", "--json"]);
    assert_eq!(code, 0, "demo failed: {demo}");
    assert_eq!(demo["data"]["kept"].as_bool(), Some(true));
    let workspace = demo["data"]["workspace"]
        .as_str()
        .expect("demo workspace path");
    let state_home = demo["data"]["state_home"]
        .as_str()
        .expect("demo state path");
    assert!(Path::new(workspace).join("src/main.rs").is_file());
    assert!(Path::new(state_home).is_dir());
    assert!(
        fs::read_dir(Path::new(state_home).join("sessions"))
            .expect("demo sessions")
            .next()
            .is_some()
    );
    let cleanup = Command::cargo_bin("cowbox")
        .expect("cowbox binary")
        .current_dir(workspace)
        .env("COWBOX_HOME", state_home)
        .args(["discard", "--yes", "--purge", "--json"])
        .output()
        .expect("discard kept demo");
    assert!(
        cleanup.status.success(),
        "discard kept demo failed: {}",
        String::from_utf8_lossy(&cleanup.stderr)
    );
    fs::remove_dir_all(workspace).expect("remove kept demo workspace");
    fs::remove_dir_all(state_home).expect("remove kept demo state");
}

#[test]
fn run_propagates_command_exit_code_and_json_schema() {
    let env = TestEnv::new();
    if !require_sandbox(&env) {
        return;
    }
    env.init();
    let (code, value) = run_script(&env, "exit 7");
    assert_eq!(code, 7);
    assert_ok(&value, "run");
    assert_eq!(value["data"]["exit_code"].as_i64(), Some(7));
    assert_eq!(value["session"]["head"].as_u64(), Some(1));
}

#[test]
fn trace_chrome_writes_a_parseable_trace() {
    let env = TestEnv::new();
    if !require_sandbox(&env) {
        return;
    }
    env.init();
    let trace = env.state.path().join("trace.json");
    let trace_arg = trace.to_string_lossy().into_owned();
    let (code, value) = env.json(&["run", "--json", "--trace-chrome", &trace_arg, "--", "true"]);
    assert_eq!(code, 0, "trace run failed: {value}");
    assert_ok(&value, "run");
    let trace_value: Value =
        serde_json::from_slice(&fs::read(&trace).expect("trace file")).expect("trace json");
    assert!(
        trace_value["traceEvents"]
            .as_array()
            .is_some_and(|events| !events.is_empty())
    );
}

#[test]
fn long_session_compacts_mount_layers_and_preserves_logical_diff() {
    let env = TestEnv::new();
    if !require_sandbox(&env) {
        return;
    }
    fs::write(env.workspace.path().join("modify.txt"), "old\n").expect("modify fixture");
    fs::write(env.workspace.path().join("delete.txt"), "remove\n").expect("delete fixture");
    fs::create_dir_all(env.workspace.path().join("nested")).expect("nested fixture");
    fs::write(env.workspace.path().join("nested/keep.txt"), "keep\n").expect("nested file");
    env.init();

    let (code, first) = run_script(
        &env,
        r#"printf 'new\n' > modify.txt
rm delete.txt
printf 'added\n' > added.txt
mkdir swap
printf 'swap child\n' > swap/child.txt
printf 'readonly\n' > read-only.txt
mkdir read-only-dir
printf 'child\n' > read-only-dir/child.txt
python3 - <<'PY'
import os
os.setxattr("read-only.txt", b"user.cowbox.test", b"file")
os.setxattr("read-only-dir", b"user.cowbox.test", b"directory")
os.chmod("read-only.txt", 0o444)
os.chmod("read-only-dir", 0o555)
PY"#,
    );
    assert_eq!(code, 0, "mutation run failed: {first}");
    let (code, replaced) = run_script(&env, "rm -rf swap && printf 'now a file\\n' > swap");
    assert_eq!(code, 0, "directory replacement run failed: {replaced}");
    let (code, before) = env.json(&["diff", "--json"]);
    assert_eq!(code, 0, "diff before compaction failed: {before}");
    let before_changes = before["data"]["changes"].clone();

    for expected in 3..=155 {
        let (code, run) = env.json(&["run", "--json", "--", "true"]);
        assert_eq!(code, 0, "long-session step {expected} failed: {run}");
        assert_eq!(run["data"]["step"].as_u64(), Some(expected));
    }

    let (code, after) = env.json(&["diff", "--json"]);
    assert_eq!(code, 0, "diff after compaction failed: {after}");
    assert_eq!(
        after["data"]["changes"], before_changes,
        "compaction changed the logical diff"
    );
    let (code, stats) = env.json(&["stats", "--json"]);
    assert_eq!(code, 0, "stats failed: {stats}");
    assert_eq!(stats["data"]["logical_steps"].as_u64(), Some(155));
    assert_eq!(stats["data"]["compact_layers"].as_u64(), Some(1));
    assert!(
        stats["data"]["mounted_layers"]
            .as_u64()
            .is_some_and(|layers| layers < 50),
        "physical mount count was not reduced: {stats}"
    );
    let (code, view) = run_script(
        &env,
        r#"set -e
test ! -e delete.txt
test -f added.txt
test "$(cat modify.txt)" = new
test "$(cat swap)" = "now a file"
test ! -e swap/child.txt
python3 - <<'PY'
import os
assert os.getxattr("read-only.txt", b"user.cowbox.test") == b"file"
assert os.getxattr("read-only-dir", b"user.cowbox.test") == b"directory"
assert os.stat("read-only.txt").st_mode & 0o777 == 0o444
assert os.stat("read-only-dir").st_mode & 0o777 == 0o555
PY"#,
    );
    assert_eq!(code, 0, "compacted sandbox view diverged: {view}");

    let (code, rollback) = env.json(&["rollback", "100", "--json"]);
    assert_eq!(
        code, 0,
        "rollback before compacted prefix failed: {rollback}"
    );
    let (code, branch) = env.json(&["run", "--json", "--", "true"]);
    assert_eq!(code, 0, "branch run failed: {branch}");
    let (code, branched_diff) = env.json(&["diff", "--json"]);
    assert_eq!(code, 0, "branched diff failed: {branched_diff}");
    assert_eq!(
        branched_diff["data"]["changes"], before_changes,
        "branching from the old snapshot did not preserve the logical diff"
    );
}

#[test]
fn unreadable_file_survives_compaction() {
    let env = TestEnv::new();
    if !require_sandbox(&env) {
        return;
    }
    env.init();
    let (code, first) = run_script(
        &env,
        r#"printf 'locked contents\n' > locked.txt
python3 - <<'PY'
import os
os.setxattr("locked.txt", b"user.cowbox.test", b"locked")
os.chmod("locked.txt", 0o000)
PY"#,
    );
    assert_eq!(code, 0, "locked-file run failed: {first}");
    for expected in 2..=151 {
        let (code, run) = env.json(&["run", "--json", "--", "true"]);
        assert_eq!(code, 0, "long-session step {expected} failed: {run}");
        assert_eq!(run["data"]["step"].as_u64(), Some(expected));
    }
    let (code, stats) = env.json(&["stats", "--json"]);
    assert_eq!(code, 0, "stats failed: {stats}");
    assert_eq!(stats["data"]["logical_steps"].as_u64(), Some(151));
    assert_eq!(stats["data"]["compact_layers"].as_u64(), Some(1));

    let (code, view) = run_script(
        &env,
        r#"set -e
python3 - <<'PY'
import os
assert open("locked.txt", encoding="utf-8").read() == "locked contents\n"
assert os.getxattr("locked.txt", b"user.cowbox.test") == b"locked"
assert os.stat("locked.txt").st_mode & 0o777 == 0
PY"#,
    );
    assert_eq!(code, 0, "locked file changed after compaction: {view}");
}

#[test]
fn host_cache_mounts_are_read_only_sources_with_session_local_writes() {
    let env = TestEnv::new();
    if !require_sandbox(&env) {
        return;
    }
    env.init();
    let (code, default_doctor) = env.json(&["doctor", "--json"]);
    assert_eq!(code, 0, "default doctor failed: {default_doctor}");
    assert!(
        default_doctor["data"]["host_caches"]
            .as_array()
            .is_some_and(Vec::is_empty),
        "host caches were enabled by default: {default_doctor}"
    );

    let host_cargo = env.state.path().join("host-cargo");
    let host_npm = env.state.path().join("host-npm");
    let host_pip = env.state.path().join("host-pip");
    fs::create_dir_all(host_cargo.join("registry")).expect("cargo host cache");
    fs::create_dir_all(host_npm.join("_cacache")).expect("npm host cache");
    fs::create_dir_all(&host_pip).expect("pip host cache");
    fs::write(host_cargo.join("registry/marker"), "cargo\n").expect("cargo marker");
    fs::write(host_npm.join("_cacache/marker"), "npm\n").expect("npm marker");
    fs::write(host_pip.join("marker"), "pip\n").expect("pip marker");
    let config = env.workspace.path().join(".cowbox.toml");
    let text = fs::read_to_string(&config).expect("config");
    let text = text.replace(
        "cargo = false\nnpm = false\npip = false",
        &format!(
            "cargo = {:?}\nnpm = {:?}\npip = {:?}",
            host_cargo, host_npm, host_pip
        ),
    );
    assert!(
        text.contains(host_cargo.to_string_lossy().as_ref()),
        "host cache configuration was not inserted"
    );
    fs::write(&config, text).expect("host cache config");

    let (code, doctor) = env.json(&["doctor", "--json"]);
    assert_eq!(code, 0, "cache doctor failed: {doctor}");
    let caches = doctor["data"]["host_caches"]
        .as_array()
        .expect("host cache reports");
    assert_eq!(caches.len(), 3, "unexpected host cache reports: {doctor}");
    assert!(
        caches
            .iter()
            .all(|cache| cache["available"].as_bool() == Some(true)),
        "configured host caches were not available: {doctor}"
    );

    let (code, run) = run_script(
        &env,
        r#"test "$(cat "$CARGO_HOME/registry/marker")" = cargo
test "$(cat "$NPM_CONFIG_CACHE/_cacache/marker")" = npm
test "$(cat "$PIP_CACHE_DIR/marker")" = pip
printf 'session cargo\n' > "$CARGO_HOME/registry/session-only"
printf 'session npm\n' > "$NPM_CONFIG_CACHE/_cacache/session-only"
printf 'session pip\n' > "$PIP_CACHE_DIR/session-only"
"#,
    );
    assert_eq!(code, 0, "host cache mount run failed: {run}");
    assert!(!host_cargo.join("registry/session-only").exists());
    assert!(!host_npm.join("_cacache/session-only").exists());
    assert!(!host_pip.join("session-only").exists());

    let (code, second) = run_script(
        &env,
        r#"test "$(cat "$CARGO_HOME/registry/session-only")" = "session cargo"
test "$(cat "$NPM_CONFIG_CACHE/_cacache/session-only")" = "session npm"
test "$(cat "$PIP_CACHE_DIR/session-only")" = "session pip""#,
    );
    assert_eq!(code, 0, "session cache did not persist: {second}");
    assert_eq!(
        fs::read_to_string(host_cargo.join("registry/marker")).expect("cargo host marker"),
        "cargo\n"
    );
    assert_eq!(
        fs::read_to_string(host_npm.join("_cacache/marker")).expect("npm host marker"),
        "npm\n"
    );
    assert_eq!(
        fs::read_to_string(host_pip.join("marker")).expect("pip host marker"),
        "pip\n"
    );
}

#[test]
fn nine_p_smoke_is_opt_in_and_cleans_up() {
    if std::env::var("COWBOX_TEST_9P").as_deref() != Ok("1")
        || !Path::new("/mnt/e/project").is_dir()
    {
        eprintln!("skipping 9p smoke; set COWBOX_TEST_9P=1 on a WSL host with /mnt/e/project");
        return;
    }
    let workspace = tempfile::Builder::new()
        .prefix("cowbox-9p-")
        .tempdir_in("/mnt/e/project")
        .expect("9p workspace");
    let state = tempfile::tempdir().expect("9p state");
    fs::write(workspace.path().join("a.txt"), "base\n").expect("9p file");

    let run = |args: &[&str]| {
        Command::cargo_bin("cowbox")
            .expect("cowbox binary")
            .current_dir(workspace.path())
            .env("COWBOX_HOME", state.path())
            .args(args)
            .output()
            .expect("9p cowbox")
    };
    let init = run(&["init", "--json"]);
    assert!(init.status.success());
    let change = run(&["run", "--json", "--", "sh", "-c", "printf 9p > a.txt"]);
    assert_eq!(change.status.code(), Some(0));
    let commit = run(&["commit", "--yes", "--json"]);
    assert_eq!(commit.status.code(), Some(0));
    assert_eq!(
        fs::read_to_string(workspace.path().join("a.txt")).expect("9p result"),
        "9p"
    );
}
