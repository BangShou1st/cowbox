use cowbox_core::SessionLayout;
use cowbox_core::commit::commit_layers;
use cowbox_core::diff::compute_diff;
use cowbox_core::manifest;
use cowbox_core::model::{ChangeKind, CommitOptions, DiffOptions, EntryKind, WhiteoutKind};
use cowbox_core::whiteout::{UpperEntryKind, classify_parts};
use std::fs;
use std::os::unix::fs::symlink;
use std::path::Path;
use tempfile::TempDir;

fn fixture() -> (TempDir, SessionLayout) {
    let root = tempfile::Builder::new()
        .prefix("cowbox-core-unit-")
        .tempdir_in("/tmp")
        .expect("create fixture");
    let workspace = root.path().join("workspace");
    let state = root.path().join("state");
    fs::create_dir_all(&workspace).expect("workspace");
    fs::create_dir_all(&state).expect("state");
    let layout = SessionLayout::new("unit-session", &workspace, &state);
    fs::create_dir_all(&layout.session_dir).expect("session dir");
    (root, layout)
}

fn write(path: &Path, contents: &str) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("parent");
    }
    fs::write(path, contents).expect("write fixture");
}

#[test]
fn manifest_captures_and_skips_vcs_metadata() {
    let (_root, layout) = fixture();
    write(&layout.workspace.join("src/lib.rs"), "fn main() {}\n");
    write(&layout.workspace.join("README.md"), "hello\n");
    write(&layout.workspace.join(".git/config"), "[core]\n");
    symlink("README.md", layout.workspace.join("readme-link")).expect("symlink");

    let entries = manifest::capture_base_manifest(&layout.workspace, &layout.base_manifest_path())
        .expect("capture manifest");
    assert!(
        entries
            .iter()
            .any(|entry| entry.rel_path == Path::new("README.md"))
    );
    assert!(
        entries
            .iter()
            .any(|entry| entry.rel_path == Path::new("readme-link"))
    );
    assert!(
        !entries
            .iter()
            .any(|entry| entry.rel_path.starts_with(".git"))
    );

    let loaded = manifest::load_manifest(&layout.base_manifest_path()).expect("load manifest");
    assert_eq!(loaded.len(), entries.len());
    let link = loaded.get(Path::new("readme-link")).expect("link entry");
    assert_eq!(link.kind, EntryKind::Symlink);
    assert_eq!(link.symlink_target.as_deref(), Some(Path::new("README.md")));
}

#[test]
fn whiteout_classifier_handles_both_kernel_forms() {
    assert_eq!(
        classify_parts(true, 0, false, None),
        UpperEntryKind::Whiteout(WhiteoutKind::CharDevice)
    );
    assert_eq!(
        classify_parts(false, 0, true, None),
        UpperEntryKind::Whiteout(WhiteoutKind::Xattr)
    );
    assert_eq!(
        classify_parts(false, 0, false, Some(b"y")),
        UpperEntryKind::OpaqueDirectory
    );
    assert_eq!(classify_parts(false, 0, false, None), UpperEntryKind::Entry);
}

fn prepare_diff_fixture() -> (TempDir, SessionLayout) {
    let (root, layout) = fixture();
    write(&layout.workspace.join("keep.txt"), "keep\n");
    write(&layout.workspace.join("modify.txt"), "old\n");
    write(&layout.workspace.join("delete.txt"), "delete me\n");
    write(&layout.workspace.join("opaque/child.txt"), "hidden\n");
    symlink("old-target", layout.workspace.join("link")).expect("base symlink");
    manifest::capture_base_manifest(&layout.workspace, &layout.base_manifest_path())
        .expect("base manifest");

    let upper = layout.upper_dir(1);
    fs::create_dir_all(&upper).expect("upper");
    write(&upper.join("modify.txt"), "new\n");
    write(&upper.join("delete.txt"), "");
    rustix::fs::setxattr(
        upper.join("delete.txt"),
        "user.overlay.whiteout",
        b"y",
        rustix::fs::XattrFlags::empty(),
    )
    .expect("whiteout xattr");
    write(&upper.join("added.txt"), "added\n");
    fs::create_dir_all(upper.join("opaque")).expect("opaque dir");
    rustix::fs::setxattr(
        upper.join("opaque"),
        "user.overlay.opaque",
        b"y",
        rustix::fs::XattrFlags::empty(),
    )
    .expect("opaque xattr");
    write(&upper.join("opaque/new.txt"), "new child\n");
    symlink("new-target", upper.join("link")).expect("upper symlink");
    (root, layout)
}

#[test]
fn diff_computes_net_changes_across_whiteouts_and_opaque_dirs() {
    let (_root, layout) = prepare_diff_fixture();
    let changes = compute_diff(
        &layout,
        &[],
        &[1],
        &DiffOptions {
            line_stats: true,
            ..Default::default()
        },
    )
    .expect("diff");
    let by_path = |path: &str| {
        changes
            .iter()
            .find(|change| change.rel_path == Path::new(path))
            .unwrap_or_else(|| panic!("missing change for {path}: {changes:?}"))
    };

    assert!(matches!(by_path("added.txt").kind, ChangeKind::Added));
    assert!(matches!(by_path("modify.txt").kind, ChangeKind::Modified));
    assert!(matches!(by_path("delete.txt").kind, ChangeKind::Deleted));
    assert!(matches!(
        by_path("link").kind,
        ChangeKind::SymlinkUpdated { .. }
    ));
    assert!(matches!(
        by_path("opaque/child.txt").kind,
        ChangeKind::Deleted
    ));
    assert!(matches!(by_path("opaque/new.txt").kind, ChangeKind::Added));
    assert!(
        !changes
            .iter()
            .any(|change| change.rel_path == Path::new("keep.txt"))
    );
}

#[test]
fn diff_hides_excluded_paths_unless_requested() {
    let (_root, layout) = prepare_diff_fixture();
    write(
        &layout.upper_dir(1).join("target/generated.txt"),
        "generated\n",
    );
    let default_changes =
        compute_diff(&layout, &[], &[1], &DiffOptions::default()).expect("default diff");
    assert!(
        !default_changes
            .iter()
            .any(|change| change.rel_path == Path::new("target/generated.txt"))
    );

    let all_changes = compute_diff(
        &layout,
        &[],
        &[1],
        &DiffOptions {
            include_excluded: true,
            ..Default::default()
        },
    )
    .expect("all diff");
    let generated = all_changes
        .iter()
        .find(|change| change.rel_path == Path::new("target/generated.txt"))
        .expect("excluded change");
    assert!(generated.excluded);
}

#[test]
fn commit_applies_net_changes_and_dry_run_leaves_host_untouched() {
    let (_root, layout) = prepare_diff_fixture();
    let dry_run = commit_layers(
        &layout,
        &[1],
        &CommitOptions {
            dry_run: true,
            ..Default::default()
        },
    )
    .expect("dry run");
    assert!(!dry_run.committed);
    assert_eq!(
        fs::read_to_string(layout.workspace.join("modify.txt")).expect("host unchanged"),
        "old\n"
    );

    let report = commit_layers(&layout, &[1], &CommitOptions::default()).expect("commit");
    assert!(report.committed);
    assert_eq!(
        fs::read_to_string(layout.workspace.join("modify.txt")).expect("modified"),
        "new\n"
    );
    assert_eq!(
        fs::read_to_string(layout.workspace.join("added.txt")).expect("added"),
        "added\n"
    );
    assert!(!layout.workspace.join("delete.txt").exists());
    assert!(!layout.workspace.join("opaque/child.txt").exists());
    assert_eq!(
        fs::read_link(layout.workspace.join("link")).expect("link"),
        Path::new("new-target")
    );
}

#[test]
fn commit_refuses_host_drift_without_force() {
    let (_root, layout) = prepare_diff_fixture();
    write(&layout.workspace.join("modify.txt"), "external change\n");
    let report = commit_layers(&layout, &[1], &CommitOptions::default()).expect("commit");
    assert!(!report.committed);
    assert!(
        report
            .conflicts
            .iter()
            .any(|path| path == Path::new("modify.txt"))
    );
    assert_eq!(
        fs::read_to_string(layout.workspace.join("modify.txt")).expect("host"),
        "external change\n"
    );
}
