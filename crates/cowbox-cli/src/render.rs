#[cfg(not(windows))]
use crate::doctor;
#[cfg(not(windows))]
use crate::doctor::{HostCacheReport, RunCheckReport};
use crate::state::{RunPolicyRecord, SessionRecord};
#[cfg(not(windows))]
use cowbox_core::Capabilities;
use cowbox_core::{ChangeKind, EntryKind, FileChange};
#[cfg(not(windows))]
use std::path::Path;

#[derive(Debug, Default)]
pub struct DiffSummary {
    pub files: usize,
    pub added: usize,
    pub modified: usize,
    pub deleted: usize,
    pub size_delta: i64,
    pub lines_added: u64,
    pub lines_removed: u64,
}

#[derive(Debug)]
pub struct Stats {
    pub session_id: String,
    pub status: String,
    pub steps: usize,
    pub mounted_layers: usize,
    pub compact_layers: usize,
    pub compacted_bytes: u64,
    pub rollbacks: u64,
    pub orphans: usize,
    pub disk_bytes: u64,
    pub landlock_abi: Option<i32>,
    pub network_isolation: bool,
    pub policy: Option<RunPolicyRecord>,
    pub blocked_writes: Option<u64>,
}

pub fn summarize(changes: &[FileChange]) -> DiffSummary {
    let mut summary = DiffSummary {
        files: changes.len(),
        ..DiffSummary::default()
    };
    for change in changes {
        summary.size_delta += change.size_delta;
        summary.lines_added += u64::from(change.lines_added.unwrap_or(0));
        summary.lines_removed += u64::from(change.lines_removed.unwrap_or(0));
        match change.kind {
            ChangeKind::Added => summary.added += 1,
            ChangeKind::Modified | ChangeKind::SymlinkUpdated { .. } => summary.modified += 1,
            ChangeKind::Deleted => summary.deleted += 1,
            ChangeKind::TypeChanged { .. } => summary.modified += 1,
        }
    }
    summary
}

pub fn summary_json(summary: &DiffSummary) -> serde_json::Value {
    serde_json::json!({
        "files": summary.files,
        "added": summary.added,
        "modified": summary.modified,
        "deleted": summary.deleted,
        "size_delta": summary.size_delta,
        "lines_added": summary.lines_added,
        "lines_removed": summary.lines_removed,
    })
}

pub fn print_diff(
    record: &SessionRecord,
    base: u64,
    target: u64,
    changes: &[FileChange],
    stat_only: bool,
    name_only: bool,
) {
    let summary = summarize(changes);
    println!(
        "Session {}  base step {} -> step {}  (head {})",
        record.session_id, base, target, record.dag.head_step
    );
    println!(
        "{} files: {} added, {} modified, {} deleted, size {:+}, lines +{} -{}",
        summary.files,
        summary.added,
        summary.modified,
        summary.deleted,
        summary.size_delta,
        summary.lines_added,
        summary.lines_removed
    );
    if stat_only {
        return;
    }
    if changes.is_empty() {
        println!("  (no changes)");
        return;
    }
    println!();
    for change in changes {
        let code = change_code(change);
        let path = change.rel_path.display();
        if name_only {
            println!("{code} {path}");
            continue;
        }
        let lines = match (change.lines_added, change.lines_removed) {
            (Some(added), Some(removed)) => format!(" +{added} -{removed}"),
            _ => String::new(),
        };
        let excluded = if change.excluded { "  [excluded]" } else { "" };
        let size = if change.size_delta == 0 {
            String::new()
        } else {
            format!("  {:+}", change.size_delta)
        };
        println!("{code}  {path}{size}{lines}{excluded}");
        if let ChangeKind::SymlinkUpdated { target } = &change.kind {
            println!("     -> {}", target.display());
        }
        if change.entry_kind == EntryKind::Other {
            println!("     (special file; not committable)");
        }
    }
}

pub fn print_log(record: &SessionRecord) {
    println!(
        "Session {}  status={:?}  workspace={}",
        record.session_id,
        record.status,
        record.workspace.display()
    );
    println!(
        "head={}  steps={}  rollbacks={}  last_commit={}",
        record.dag.head_step,
        record.dag.nodes.len().saturating_sub(1),
        record.rollback_count,
        record
            .last_commit_unix_ms
            .map(crate::workspace::format_timestamp)
            .unwrap_or_else(|| "never".to_string())
    );
    println!();
    for (id, node) in &record.dag.nodes {
        let depth = record
            .dag
            .path_to_base(*id)
            .map(|path| path.len().saturating_sub(1))
            .unwrap_or(0);
        let indent = "  ".repeat(depth);
        let head = if *id == record.dag.head_step {
            " (HEAD)"
        } else {
            ""
        };
        let parent = node
            .parent_id
            .map(|parent| format!(" <- {parent}"))
            .unwrap_or_default();
        let exit = node
            .exit_code
            .map(|code| format!(" exit={code}"))
            .unwrap_or_default();
        let timed_out = if node.timed_out { " TIMEOUT" } else { "" };
        println!(
            "{indent}* Step {id}{head}{parent}{exit}  {}  {}ms  orphans={}{}",
            node.label, node.duration_ms, node.orphans_reaped, timed_out
        );
    }
}

pub fn print_stats(stats: &Stats) {
    println!("Session {}  status={}", stats.session_id, stats.status);
    println!("Steps (logical):  {}", stats.steps);
    println!(
        "Mounted layers:   {} (including live base)",
        stats.mounted_layers
    );
    println!(
        "Compacted layers: {} ({})",
        stats.compact_layers,
        format_bytes(stats.compacted_bytes)
    );
    println!("Rollbacks:        {}", stats.rollbacks);
    println!("Orphans reaped:   {}", stats.orphans);
    println!("Incremental disk: {}", format_bytes(stats.disk_bytes));
    println!(
        "Landlock ABI:     {}",
        stats
            .landlock_abi
            .map(|abi| abi.to_string())
            .unwrap_or_else(|| "unknown".to_string())
    );
    println!(
        "Network policy:    {}",
        stats
            .policy
            .as_ref()
            .map(|policy| format!(
                "{} ({})",
                policy.network.as_str(),
                policy.network_source.as_str()
            ))
            .unwrap_or_else(|| "unknown (legacy session)".to_string())
    );
    println!(
        "Network isolated:  {}",
        if stats.network_isolation { "yes" } else { "no" }
    );
    println!(
        "Blocked writes:   {}",
        stats
            .blocked_writes
            .map(|count| count.to_string())
            .unwrap_or_else(|| "not counted in M1".to_string())
    );
}

#[cfg(not(windows))]
pub fn print_doctor(
    capabilities: &Capabilities,
    state_root: &Path,
    host_caches: &[HostCacheReport],
    ready: bool,
) {
    println!("cowbox doctor ({})", env!("CARGO_PKG_VERSION"));
    println!("State root: {}", state_root.display());
    println!();
    line("OS", true, &capabilities.os);
    line(
        "Kernel",
        !capabilities.kernel.is_empty(),
        &capabilities.kernel,
    );
    line(
        "WSL2",
        capabilities.wsl,
        if capabilities.wsl {
            "detected"
        } else {
            "not detected"
        },
    );
    line(
        "User namespaces",
        capabilities.user_namespace,
        if capabilities.user_namespace {
            "available"
        } else {
            "unavailable"
        },
    );
    line(
        "OverlayFS",
        capabilities.overlayfs,
        if capabilities.overlayfs {
            "available"
        } else {
            "unavailable"
        },
    );
    line(
        "userxattr",
        capabilities.userxattr,
        if capabilities.userxattr {
            "available"
        } else {
            "unavailable"
        },
    );
    line(
        "Landlock",
        doctor::landlock_ready(capabilities),
        &capabilities
            .landlock_abi
            .map(|abi| format!("ABI {abi} (ABI 3 required)"))
            .unwrap_or_else(|| "unavailable".to_string()),
    );
    line(
        "Whiteout format",
        capabilities.whiteout_kind != cowbox_core::WhiteoutKind::Unknown,
        &format!("{:?}", capabilities.whiteout_kind),
    );
    let state_fs = capabilities.state_fs.as_deref().unwrap_or("unknown");
    let state_detail = if state_fs == "9p" {
        "9p (COWBOX_HOME requires an xattr-capable Linux filesystem; WSL ext4 is recommended)"
    } else {
        state_fs
    };
    line(
        "State filesystem",
        doctor::state_fs_ready(capabilities),
        state_detail,
    );
    if host_caches.is_empty() {
        println!("[ok] Host caches: none configured; sessions use isolated caches");
    } else {
        for cache in host_caches {
            let paths = cache
                .paths
                .iter()
                .map(|path| {
                    format!(
                        "{} {}",
                        path.name,
                        if path.available {
                            "available"
                        } else {
                            "missing"
                        }
                    )
                })
                .collect::<Vec<_>>()
                .join(", ");
            line(
                &format!("{} host cache", cache.tool),
                cache.available,
                &format!("{} ({paths})", cache.source.display()),
            );
        }
    }
    for note in &capabilities.notes {
        println!("  note: {note}");
    }
    println!();
    println!(
        "Status: {}",
        if ready {
            "READY — workspace can be sandboxed"
        } else {
            "NEEDS ATTENTION — see the failed checks above"
        }
    );
}

#[cfg(not(windows))]
pub fn print_run_check(report: &RunCheckReport) {
    println!();
    println!(
        "Active run check: {} (network policy: {})",
        if report.ready { "READY" } else { "FAILED" },
        report.network.as_str()
    );
    stage_line("State disk", Some(report.stages.state_disk));
    stage_line("User namespace", report.stages.user_namespace);
    stage_line("Network namespace", report.stages.network_namespace);
    stage_line("OverlayFS", report.stages.overlayfs);
    stage_line("Landlock", report.stages.landlock);
    line(
        "User command",
        report.command_started,
        if report.command_started {
            "started"
        } else {
            "not started"
        },
    );
    if let Some(error) = &report.error {
        println!(
            "  error: {}: {} (exit {})",
            error.code,
            error.message,
            error
                .exit_code
                .map(|code| code.to_string())
                .unwrap_or_else(|| "-".to_string())
        );
    }
}

#[cfg(not(windows))]
fn stage_line(label: &str, status: Option<bool>) {
    match status {
        Some(ok) => line(label, ok, if ok { "passed" } else { "failed" }),
        None => println!("[--] {label}: not reached"),
    }
}

#[cfg(not(windows))]
fn line(label: &str, ok: bool, detail: &str) {
    println!("[{}] {label}: {detail}", if ok { "ok" } else { "!!" });
}

fn change_code(change: &FileChange) -> &'static str {
    match change.kind {
        ChangeKind::Added => "A",
        ChangeKind::Modified => "M",
        ChangeKind::Deleted => "D",
        ChangeKind::SymlinkUpdated { .. } => "S",
        ChangeKind::TypeChanged { .. } => "T",
    }
}

pub fn format_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut size = bytes as f64;
    let mut unit = 0;
    while size >= 1024.0 && unit + 1 < UNITS.len() {
        size /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{size:.1} {}", UNITS[unit])
    }
}
