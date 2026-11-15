use crate::cli::DemoArgs;
use crate::error::{CliError, CliResult};
use crate::json;
use crate::render::{self, DiffSummary};
use crate::workspace;
use cowbox_core::FileChange;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Instant;

const DEMO_STEPS: usize = 2;

pub fn run(args: &DemoArgs) -> CliResult<i32> {
    let started = Instant::now();
    let paths = DemoPaths::create()?;
    let outcome = match execute(&paths, args.json, args.keep) {
        Ok(outcome) => outcome,
        Err(error) => return Err(preserve_error(error, &paths)),
    };
    let kept = args.keep;

    if !kept {
        paths.cleanup()?;
    }
    let duration_ms = started.elapsed().as_millis() as u64;

    if args.json {
        json::print(&json::ok(
            "demo",
            None,
            json!({
                "duration_ms": duration_ms,
                "workspace": paths.workspace,
                "state_home": paths.state_home,
                "steps": DEMO_STEPS,
                "summary": render::summary_json(&outcome.summary),
                "host_unchanged": true,
                "kept": kept,
            }),
        ))?;
    } else {
        println!();
        println!(
            "demo passed in {duration_ms} ms: {} file(s) changed in the sandbox, host fixture unchanged.",
            outcome.summary.files
        );
        if kept {
            println!("fixture kept at {}", paths.workspace.display());
            println!("state kept at {}", paths.state_home.display());
            println!(
                "inspect with `COWBOX_HOME={} cowbox log` from the fixture",
                paths.state_home.display()
            );
            println!(
                "then remove the fixture and state paths after `COWBOX_HOME={} cowbox discard --yes --purge`",
                paths.state_home.display()
            );
        } else {
            println!("temporary fixture and state removed");
        }
    }
    Ok(0)
}

fn execute(paths: &DemoPaths, json_mode: bool, keep: bool) -> CliResult<DemoOutcome> {
    let executable = std::env::current_exe()?;
    stage(json_mode, "Preparing isolated demo fixture");
    prepare_fixture(&paths.workspace)?;

    stage(json_mode, "Checking sandbox capabilities");
    let doctor = run_json_allow_failure(
        &executable,
        &paths.workspace,
        &paths.state_home,
        &["doctor", "--json"],
    )?;
    if doctor["data"]["ready"].as_bool() != Some(true) {
        return Err(
            CliError::new("DEMO_NOT_READY", "the sandbox is not ready on this machine")
                .hint("run `cowbox doctor` and fix the reported capability first")
                .exit(125),
        );
    }

    run_json(
        &executable,
        &paths.workspace,
        &paths.state_home,
        &["init", "--json"],
    )?;
    let baseline = snapshot_tree(&paths.workspace)?;

    stage(json_mode, "Breaking files inside the sandbox");
    run_json(
        &executable,
        &paths.workspace,
        &paths.state_home,
        &[
            "run",
            "--json",
            "--",
            "sh",
            "-c",
            "rm -rf src local-state && printf 'corrupted\\n' > BROKEN.md",
        ],
    )?;
    ensure_host_unchanged(&paths.workspace, &baseline)?;

    stage(json_mode, "Inspecting net changes");
    let diff = run_json(
        &executable,
        &paths.workspace,
        &paths.state_home,
        &["diff", "--stat", "--json"],
    )?;
    let diff_changes = parse_changes(&diff)?;
    let diff_summary = render::summarize(&diff_changes);
    let paths_changed = diff_changes
        .iter()
        .map(|change| change.rel_path.as_path())
        .collect::<Vec<_>>();
    for expected in [
        Path::new("BROKEN.md"),
        Path::new("src/main.rs"),
        Path::new("local-state/session.json"),
    ] {
        if !paths_changed.contains(&expected) {
            return Err(CliError::new(
                "DEMO_UNEXPECTED_DIFF",
                format!("demo diff did not include {}", expected.display()),
            ));
        }
    }
    if diff_summary.added == 0 || diff_summary.deleted < 2 {
        return Err(CliError::new(
            "DEMO_UNEXPECTED_DIFF",
            "demo diff did not report the expected added and deleted files",
        ));
    }
    if !json_mode {
        println!(
            "sandbox diff: {} added, {} modified, {} deleted",
            diff_summary.added, diff_summary.modified, diff_summary.deleted
        );
    }

    stage(json_mode, "Previewing a host commit");
    let preview = run_json(
        &executable,
        &paths.workspace,
        &paths.state_home,
        &["commit", "--dry-run", "--json"],
    )?;
    let preview_changes = parse_changes(&preview)?;
    let summary = render::summarize(&preview_changes);
    if preview["data"]["committed"].as_bool() != Some(false)
        || preview["data"]["dry_run"].as_bool() != Some(true)
        || summary.files == 0
    {
        return Err(CliError::new(
            "DEMO_UNEXPECTED_PREVIEW",
            "commit dry-run did not preview the destructive step",
        ));
    }
    if !json_mode {
        println!(
            "commit preview: {} file(s) would be written; nothing changed",
            summary.files
        );
    }

    stage(json_mode, "Rolling back to the base step");
    let rollback = run_json(
        &executable,
        &paths.workspace,
        &paths.state_home,
        &["rollback", "0", "--json"],
    )?;
    if rollback["data"]["head"].as_u64() != Some(0) {
        return Err(CliError::new(
            "DEMO_ROLLBACK_FAILED",
            "rollback did not move the session head to step 0",
        ));
    }
    if !json_mode {
        println!("session head moved to step 0");
    }
    ensure_host_unchanged(&paths.workspace, &baseline)?;

    stage(json_mode, "Verifying the restored sandbox view");
    let verify = run_json(
        &executable,
        &paths.workspace,
        &paths.state_home,
        &[
            "run",
            "--json",
            "--",
            "sh",
            "-c",
            "test -f src/main.rs && test -f local-state/session.json && test ! -e BROKEN.md && printf 'restored\\n'",
        ],
    )?;
    if verify["data"]["stdout"].as_str() != Some("restored\n") {
        return Err(CliError::new(
            "DEMO_RESTORE_FAILED",
            "the verification step did not report restored files",
        ));
    }
    if !json_mode {
        println!("sandbox files restored after rollback");
    }

    let clean = run_json(
        &executable,
        &paths.workspace,
        &paths.state_home,
        &["diff", "--stat", "--json"],
    )?;
    if !parse_changes(&clean)?.is_empty() {
        return Err(CliError::new(
            "DEMO_RESTORE_FAILED",
            "the sandbox still reports net changes after rollback",
        ));
    }
    ensure_host_unchanged(&paths.workspace, &baseline)?;

    if !keep {
        stage(json_mode, "Discarding the isolated session");
        run_json(
            &executable,
            &paths.workspace,
            &paths.state_home,
            &["discard", "--yes", "--purge", "--json"],
        )?;
    }
    ensure_host_unchanged(&paths.workspace, &baseline)?;

    Ok(DemoOutcome { summary })
}

fn stage(json_mode: bool, message: &str) {
    if !json_mode {
        println!("== {message} ==");
    }
}

fn prepare_fixture(root: &Path) -> CliResult<()> {
    fs::create_dir_all(root.join(".git"))?;
    fs::create_dir_all(root.join("src"))?;
    fs::create_dir_all(root.join("local-state"))?;
    fs::write(root.join(".gitignore"), "local-state/\n")?;
    fs::write(
        root.join("src/main.rs"),
        "fn main() {\n    println!(\"cowbox demo\");\n}\n",
    )?;
    fs::write(
        root.join("local-state/session.json"),
        "{\"schema\":1,\"state\":\"local-only\"}\n",
    )?;
    Ok(())
}

fn run_json(
    executable: &Path,
    workspace: &Path,
    state_home: &Path,
    args: &[&str],
) -> CliResult<Value> {
    run_json_inner(executable, workspace, state_home, args, true)
}

fn run_json_allow_failure(
    executable: &Path,
    workspace: &Path,
    state_home: &Path,
    args: &[&str],
) -> CliResult<Value> {
    run_json_inner(executable, workspace, state_home, args, false)
}

fn run_json_inner(
    executable: &Path,
    workspace: &Path,
    state_home: &Path,
    args: &[&str],
    require_success: bool,
) -> CliResult<Value> {
    let output = Command::new(executable)
        .current_dir(workspace)
        .env("COWBOX_HOME", state_home)
        .args(args)
        .output()
        .map_err(|error| {
            CliError::new(
                "DEMO_STEP_FAILED",
                format!(
                    "cannot run {} {}: {error}",
                    executable.display(),
                    args.join(" ")
                ),
            )
        })?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let value: Value = serde_json::from_slice(&output.stdout).map_err(|error| {
        CliError::new(
            "DEMO_STEP_FAILED",
            format!(
                "demo step `{}` returned invalid JSON: {error}; stderr={}",
                args.join(" "),
                stderr.trim()
            ),
        )
    })?;
    if value["ok"].as_bool() != Some(true) {
        let message = value["error"]["message"]
            .as_str()
            .filter(|message| !message.is_empty())
            .unwrap_or_else(|| stdout.trim());
        return Err(CliError::new(
            "DEMO_STEP_FAILED",
            format!("demo step `{}` failed: {message}", args.join(" ")),
        ));
    }
    if require_success && !output.status.success() {
        let message = value["error"]["message"]
            .as_str()
            .filter(|message| !message.is_empty())
            .unwrap_or_else(|| stdout.trim());
        return Err(CliError::new(
            "DEMO_STEP_FAILED",
            format!("demo step `{}` failed: {message}", args.join(" ")),
        ));
    }
    Ok(value)
}

fn parse_changes(value: &Value) -> CliResult<Vec<FileChange>> {
    serde_json::from_value(value["data"]["changes"].clone()).map_err(|error| {
        CliError::new(
            "DEMO_STEP_FAILED",
            format!("cannot parse demo change list: {error}"),
        )
    })
}

fn ensure_host_unchanged(root: &Path, baseline: &[SnapshotEntry]) -> CliResult<()> {
    let current = snapshot_tree(root)?;
    if current != baseline {
        return Err(CliError::new(
            "DEMO_HOST_CHANGED",
            format!(
                "the host fixture at {} changed outside the sandbox",
                root.display()
            ),
        ));
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct SnapshotEntry {
    path: PathBuf,
    kind: &'static str,
    digest: String,
}

fn snapshot_tree(root: &Path) -> CliResult<Vec<SnapshotEntry>> {
    let mut entries = Vec::new();
    snapshot_children(root, root, &mut entries)?;
    entries.sort();
    Ok(entries)
}

fn snapshot_children(
    root: &Path,
    current: &Path,
    entries: &mut Vec<SnapshotEntry>,
) -> CliResult<()> {
    let mut children = fs::read_dir(current)?.collect::<std::io::Result<Vec<_>>>()?;
    children.sort_by_key(|entry| entry.file_name());
    for child in children {
        let path = child.path();
        let relative = path.strip_prefix(root).unwrap_or(&path).to_path_buf();
        let metadata = fs::symlink_metadata(&path)?;
        let file_type = metadata.file_type();
        if file_type.is_symlink() {
            let target = fs::read_link(&path)?;
            entries.push(SnapshotEntry {
                path: relative,
                kind: "symlink",
                digest: digest(target.to_string_lossy().as_bytes()),
            });
        } else if metadata.is_dir() {
            entries.push(SnapshotEntry {
                path: relative,
                kind: "directory",
                digest: String::new(),
            });
            snapshot_children(root, &path, entries)?;
        } else if metadata.is_file() {
            let contents = fs::read(&path)?;
            entries.push(SnapshotEntry {
                path: relative,
                kind: "file",
                digest: digest(&contents),
            });
        } else {
            entries.push(SnapshotEntry {
                path: relative,
                kind: "other",
                digest: String::new(),
            });
        }
    }
    Ok(())
}

fn digest(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

fn preserve_error(mut error: CliError, paths: &DemoPaths) -> CliError {
    error.message = format!(
        "{}; demo files kept at workspace {} and state {}",
        error.message,
        paths.workspace.display(),
        paths.state_home.display()
    );
    let path_hint = "inspect the preserved demo files, then remove them manually when finished";
    error.hint = Some(match error.hint.take() {
        Some(hint) => format!("{hint}; {path_hint}"),
        None => path_hint.to_string(),
    });
    error
}

struct DemoOutcome {
    summary: DiffSummary,
}

struct DemoPaths {
    workspace: PathBuf,
    state_home: PathBuf,
}

impl DemoPaths {
    fn create() -> CliResult<Self> {
        let state_root = workspace::state_root()?;
        let id = format!("{}-{}", std::process::id(), workspace::new_session_id());
        let workspace = PathBuf::from("/tmp").join(format!("cowbox-demo-{id}"));
        let state_parent = state_root.join("demo");
        fs::create_dir_all(&state_parent)?;
        let state_home = state_parent.join(&id);
        fs::create_dir(&workspace).map_err(|error| {
            CliError::new(
                "DEMO_SETUP_FAILED",
                format!(
                    "cannot create demo workspace {}: {error}",
                    workspace.display()
                ),
            )
        })?;
        if let Err(error) = fs::create_dir(&state_home) {
            let _ = fs::remove_dir_all(&workspace);
            return Err(CliError::new(
                "DEMO_SETUP_FAILED",
                format!("cannot create demo state {}: {error}", state_home.display()),
            ));
        }
        Ok(Self {
            workspace,
            state_home,
        })
    }

    fn cleanup(&self) -> CliResult<()> {
        remove_if_present(&self.workspace)?;
        remove_if_present(&self.state_home)?;
        Ok(())
    }
}

fn remove_if_present(path: &Path) -> CliResult<()> {
    match fs::remove_dir_all(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(CliError::new(
            "DEMO_CLEANUP_FAILED",
            format!("cannot remove demo path {}: {error}", path.display()),
        )),
    }
}
