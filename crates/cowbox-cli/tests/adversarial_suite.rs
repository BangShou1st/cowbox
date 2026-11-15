use assert_cmd::Command;
use rusqlite::Connection;
use serde_json::Value;
use std::fs;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::{Path, PathBuf};
use std::process::Command as StdCommand;
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
    assert!(
        value["data"]["capabilities"]["landlock_abi"]
            .as_i64()
            .unwrap_or(0)
            >= 3
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

    let (code, commit) = env.json(&["commit", "--json"]);
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
fn commit_refuses_fifo_entries() {
    let env = TestEnv::new();
    if !require_sandbox(&env) {
        return;
    }
    env.init();
    let (code, run) = run_script(&env, "mkfifo fifo");
    assert_eq!(code, 0, "mkfifo failed: {run}");
    let (code, commit) = env.json(&["commit", "--json"]);
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

    let (code, conflict) = env.json(&["commit", "--json"]);
    assert_eq!(code, 2, "expected conflict: {conflict}");
    assert_eq!(conflict["ok"].as_bool(), Some(false));
    assert_eq!(conflict["error"]["code"].as_str(), Some("COMMIT_CONFLICT"));
    assert_eq!(
        fs::read_to_string(env.workspace.path().join("file.txt")).expect("host"),
        "host-edit\n"
    );

    let (code, forced) = env.json(&["commit", "--force", "--json"]);
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
    let commit = run(&["commit", "--json"]);
    assert_eq!(commit.status.code(), Some(0));
    assert_eq!(
        fs::read_to_string(workspace.path().join("a.txt")).expect("9p result"),
        "9p"
    );
}
