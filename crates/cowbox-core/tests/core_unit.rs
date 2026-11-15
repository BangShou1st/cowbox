use cowbox_core::SessionLayout;
use cowbox_core::commit::commit_layers;
use cowbox_core::diff::compute_diff;
use cowbox_core::manifest;
use cowbox_core::model::{ChangeKind, CommitOptions, DiffOptions, EntryKind, WhiteoutKind};
use cowbox_core::whiteout::{UpperEntryKind, classify_parts};
use std::fs;
use std::os::unix::fs::MetadataExt;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::fs::symlink;
use std::path::Path;
use std::process::Command;
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
fn old_manifest_without_hardlink_or_xattr_fields_still_loads() {
    let (_root, layout) = fixture();
    if let Some(parent) = layout.base_manifest_path().parent() {
        fs::create_dir_all(parent).expect("manifest parent");
    }
    fs::write(
        layout.base_manifest_path(),
        r#"{"rel_path":"old.txt","kind":"file","mode":420,"size":3,"mtime_ns":0,"symlink_target":null}"#,
    )
    .expect("old manifest");
    let loaded = manifest::load_manifest(&layout.base_manifest_path()).expect("load old manifest");
    let entry = loaded.get(Path::new("old.txt")).expect("old entry");
    assert_eq!(entry.dev, None);
    assert_eq!(entry.ino, None);
    assert_eq!(entry.nlink, None);
    assert_eq!(entry.xattr_digest, None);
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

fn set_user_xattr(path: &Path, name: &str, value: &[u8]) {
    rustix::fs::setxattr(path, name, value, rustix::fs::XattrFlags::empty())
        .expect("set user xattr");
}

fn user_xattr(path: &Path, name: &str) -> Vec<u8> {
    let size = rustix::fs::lgetxattr_query(path, name).expect("query xattr");
    let mut value = vec![0u8; size];
    let read = rustix::fs::lgetxattr(path, name, &mut value[..]).expect("read xattr");
    value.truncate(read);
    value
}

#[test]
fn diff_detects_and_commit_preserves_user_xattrs() {
    let (_root, layout) = fixture();
    let host = layout.workspace.join("file.txt");
    write(&host, "same\n");
    set_user_xattr(&host, "user.cowbox.test", b"old");
    manifest::capture_base_manifest(&layout.workspace, &layout.base_manifest_path())
        .expect("base manifest");

    let upper = layout.upper_dir(1);
    fs::create_dir_all(&upper).expect("upper");
    write(&upper.join("file.txt"), "same\n");
    set_user_xattr(&upper.join("file.txt"), "user.cowbox.test", b"new");

    let changes = compute_diff(&layout, &[], &[1], &DiffOptions::default()).expect("diff");
    assert!(
        changes
            .iter()
            .any(|change| change.rel_path == Path::new("file.txt")
                && matches!(change.kind, ChangeKind::Modified)),
        "xattr-only change was not detected: {changes:?}"
    );

    let report = commit_layers(&layout, &[1], &CommitOptions::default()).expect("commit");
    assert!(report.committed);
    assert_eq!(user_xattr(&host, "user.cowbox.test"), b"new");
}

#[test]
fn commit_preserves_directory_user_xattrs() {
    let (_root, layout) = fixture();
    let host = layout.workspace.join("dir");
    fs::create_dir_all(&host).expect("host dir");
    set_user_xattr(&host, "user.cowbox.dir", b"old");
    manifest::capture_base_manifest(&layout.workspace, &layout.base_manifest_path())
        .expect("base manifest");

    let upper = layout.upper_dir(1);
    fs::create_dir_all(upper.join("dir")).expect("upper dir");
    set_user_xattr(&upper.join("dir"), "user.cowbox.dir", b"new");
    let changes = compute_diff(&layout, &[], &[1], &DiffOptions::default()).expect("diff");
    assert!(
        changes.iter().any(
            |change| change.rel_path == Path::new("dir") && change.kind == ChangeKind::Modified
        ),
        "directory xattr change was not detected: {changes:?}"
    );

    let report = commit_layers(&layout, &[1], &CommitOptions::default()).expect("commit");
    assert!(report.committed);
    assert_eq!(user_xattr(&host, "user.cowbox.dir"), b"new");
}

#[test]
fn commit_applies_xattrs_before_read_only_permissions() {
    let (_root, layout) = fixture();
    let host = layout.workspace.join("readonly.txt");
    write(&host, "same\n");
    set_user_xattr(&host, "user.cowbox.readonly", b"old");
    fs::set_permissions(&host, fs::Permissions::from_mode(0o444)).expect("host read-only");
    manifest::capture_base_manifest(&layout.workspace, &layout.base_manifest_path())
        .expect("base manifest");

    let upper = layout.upper_dir(1);
    fs::create_dir_all(&upper).expect("upper");
    write(&upper.join("readonly.txt"), "same\n");
    set_user_xattr(&upper.join("readonly.txt"), "user.cowbox.readonly", b"new");
    fs::set_permissions(
        upper.join("readonly.txt"),
        fs::Permissions::from_mode(0o444),
    )
    .expect("upper read-only");

    let report = commit_layers(&layout, &[1], &CommitOptions::default()).expect("commit");
    assert!(report.committed);
    assert_eq!(user_xattr(&host, "user.cowbox.readonly"), b"new");
    assert_eq!(
        fs::metadata(&host).expect("mode").permissions().mode() & 0o777,
        0o444
    );
}

#[test]
fn commit_preserves_hardlinks_within_the_change_set() {
    let (_root, layout) = fixture();
    manifest::capture_base_manifest(&layout.workspace, &layout.base_manifest_path())
        .expect("base manifest");
    let upper = layout.upper_dir(1);
    fs::create_dir_all(&upper).expect("upper");
    write(&upper.join("a.txt"), "linked\n");
    fs::hard_link(upper.join("a.txt"), upper.join("b.txt")).expect("upper hardlink");

    let report = commit_layers(&layout, &[1], &CommitOptions::default()).expect("commit");
    assert!(report.committed);
    let a = fs::metadata(layout.workspace.join("a.txt")).expect("a metadata");
    let b = fs::metadata(layout.workspace.join("b.txt")).expect("b metadata");
    assert_eq!(a.ino(), b.ino(), "committed hardlink was not preserved");
    assert_eq!(a.nlink(), 2);
}

#[test]
fn commit_handles_added_and_deleted_directory_trees() {
    let (_root, layout) = fixture();
    write(&layout.workspace.join("old/child.txt"), "old\n");
    manifest::capture_base_manifest(&layout.workspace, &layout.base_manifest_path())
        .expect("base manifest");

    let upper = layout.upper_dir(1);
    fs::create_dir_all(&upper).expect("upper");
    write(&upper.join("old"), "");
    rustix::fs::setxattr(
        upper.join("old"),
        "user.overlay.whiteout",
        b"y",
        rustix::fs::XattrFlags::empty(),
    )
    .expect("old whiteout");
    write(&upper.join("new/child.txt"), "new\n");

    let report = commit_layers(&layout, &[1], &CommitOptions::default()).expect("commit");
    assert!(report.committed);
    assert!(!layout.workspace.join("old").exists());
    assert_eq!(
        fs::read_to_string(layout.workspace.join("new/child.txt")).expect("new child"),
        "new\n"
    );
}

#[test]
fn failed_commit_restores_hardlinks_and_xattrs() {
    let (_root, layout) = fixture();
    let host_a = layout.workspace.join("a.txt");
    write(&host_a, "before\n");
    fs::hard_link(&host_a, layout.workspace.join("b.txt")).expect("host hardlink");
    set_user_xattr(&host_a, "user.cowbox.test", b"old");
    manifest::capture_base_manifest(&layout.workspace, &layout.base_manifest_path())
        .expect("base manifest");

    let upper = layout.upper_dir(1);
    fs::create_dir_all(&upper).expect("upper");
    write(&upper.join("a.txt"), "after\n");
    set_user_xattr(&upper.join("a.txt"), "user.cowbox.test", b"new");
    let fifo = upper.join("fifo");
    assert!(
        Command::new("mkfifo")
            .arg(&fifo)
            .status()
            .expect("spawn mkfifo")
            .success()
    );

    let error = commit_layers(&layout, &[1], &CommitOptions::default()).expect_err("commit fails");
    assert!(error.to_string().contains("non-regular entry"), "{error}");
    assert_eq!(fs::read_to_string(&host_a).expect("host a"), "before\n");
    assert_eq!(user_xattr(&host_a, "user.cowbox.test"), b"old");
    let a = fs::metadata(&host_a).expect("a metadata");
    let b = fs::metadata(layout.workspace.join("b.txt")).expect("b metadata");
    assert_eq!(a.ino(), b.ino(), "rollback did not restore the hardlink");
    assert_eq!(a.nlink(), 2);
}
