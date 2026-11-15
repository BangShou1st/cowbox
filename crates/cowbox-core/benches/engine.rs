use cowbox_core::SessionLayout;
use cowbox_core::diff::compute_diff;
use cowbox_core::manifest;
use cowbox_core::model::DiffOptions;
use cowbox_dag::{SessionDag, StepNode};
use criterion::{Criterion, criterion_group, criterion_main};
use std::fs;
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};
use std::process::Command as StdCommand;
use std::time::Duration;
use tempfile::TempDir;

fn bench_files() -> usize {
    std::env::var("COWBOX_BENCH_FILES")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(10_000)
        .max(100)
}

fn populate(root: &Path, files: usize) {
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

fn session_fixture(files: usize) -> (TempDir, SessionLayout) {
    let root = tempfile::Builder::new()
        .prefix("cowbox-bench-")
        .tempdir_in("/tmp")
        .expect("bench fixture");
    let workspace = root.path().join("workspace");
    let state = root.path().join("state");
    fs::create_dir_all(&workspace).expect("workspace");
    fs::create_dir_all(&state).expect("state");
    fs::create_dir_all(workspace.join(".git")).expect("git marker");
    populate(&workspace, files);
    let layout = SessionLayout::new("bench-session", &workspace, &state);
    fs::create_dir_all(&layout.session_dir).expect("session");
    manifest::capture_base_manifest(&workspace, &layout.base_manifest_path())
        .expect("base manifest");
    (root, layout)
}

fn bench_diff_scan(c: &mut Criterion) {
    let files = bench_files();
    let (_root, layout) = session_fixture(files);
    let upper = layout.upper_dir(1);
    fs::create_dir_all(&upper).expect("upper");
    for index in 0..10 {
        let path = upper.join(format!("src/d000/f{index}.txt"));
        fs::create_dir_all(path.parent().expect("modified parent")).expect("modified parent");
        fs::write(path, "modified\n").expect("modified fixture");
    }
    let options = DiffOptions {
        line_stats: false,
        ..DiffOptions::default()
    };
    let mut group = c.benchmark_group("diff_scan");
    group.sample_size(20);
    group.bench_function(format!("{files}_files_10_changes"), |b| {
        b.iter(|| compute_diff(&layout, &[], &[1], &options).expect("diff"));
    });
    group.finish();
}

fn bench_rollback(c: &mut Criterion) {
    let mut dag = SessionDag::new("bench", "/tmp/cowbox-bench");
    for id in 1..=100 {
        let mut node = StepNode::new(id, Some(id - 1), format!("step {id}"));
        node.exit_code = Some(0);
        dag.add_step(node).expect("add step");
    }
    let mut group = c.benchmark_group("rollback_dag");
    group.bench_function("100_steps", |b| {
        b.iter(|| {
            dag.rollback(0).expect("rollback base");
            dag.rollback(99).expect("rollback head");
        });
    });
    group.finish();
}

fn cli_binary() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("CARGO_BIN_EXE_cowbox") {
        let path = PathBuf::from(path);
        if path.exists() {
            return Some(path);
        }
    }
    let target = std::env::var_os("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target"));
    let candidate = target.join("debug/cowbox");
    candidate.exists().then_some(candidate)
}

fn bench_run_true(c: &mut Criterion) {
    let Some(binary) = cli_binary() else {
        eprintln!(
            "skipping run_true benchmark: build cowbox-cli first with `cargo build -p cowbox-cli`"
        );
        return;
    };
    let files = bench_files();
    let workspace = tempfile::Builder::new()
        .prefix("cowbox-bench-run-")
        .tempdir_in("/tmp")
        .expect("run workspace");
    let state = tempfile::Builder::new()
        .prefix("cowbox-bench-run-state-")
        .tempdir_in("/tmp")
        .expect("run state");
    fs::create_dir_all(workspace.path().join(".git")).expect("git marker");
    populate(workspace.path(), files);

    let init = StdCommand::new(&binary)
        .current_dir(workspace.path())
        .env("COWBOX_HOME", state.path())
        .args(["init", "--json"])
        .output()
        .expect("init benchmark fixture");
    assert!(
        init.status.success(),
        "{}",
        String::from_utf8_lossy(&init.stderr)
    );

    let warmup = StdCommand::new(&binary)
        .current_dir(workspace.path())
        .env("COWBOX_HOME", state.path())
        .args(["run", "--json", "--", "true"])
        .output()
        .expect("warmup run");
    assert!(
        warmup.status.success(),
        "warmup failed: stdout={} stderr={}",
        String::from_utf8_lossy(&warmup.stdout),
        String::from_utf8_lossy(&warmup.stderr)
    );

    let mut group = c.benchmark_group("run_true");
    group.sample_size(10);
    group.warm_up_time(Duration::from_millis(1));
    group.measurement_time(Duration::from_millis(100));
    group.bench_function(format!("{files}_files_end_to_end"), |b| {
        b.iter(|| {
            let output = StdCommand::new(&binary)
                .current_dir(workspace.path())
                .env("COWBOX_HOME", state.path())
                .args(["run", "--json", "--", "true"])
                .output()
                .expect("benchmark run");
            assert!(
                output.status.success(),
                "benchmark run failed: stdout={} stderr={}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
        });
    });
    group.finish();
}

criterion_group! {
    name = benches;
    config = Criterion::default().sample_size(20);
    targets = bench_diff_scan, bench_rollback, bench_run_true
}
criterion_main!(benches);
