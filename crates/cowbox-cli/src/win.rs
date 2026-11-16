use crate::cli::WinArgs;
#[cfg(windows)]
use crate::cli::{
    WinCheckpointArgs, WinCommand, WinCommitArgs, WinDiffArgs, WinDiscardArgs, WinDoctorArgs,
    WinInitArgs, WinLogArgs, WinRollbackArgs,
};
use crate::error::{CliError, CliResult};
#[cfg(windows)]
use crate::json;
#[cfg(windows)]
use serde_json::{Value, json};
#[cfg(windows)]
use std::io::{IsTerminal, Write};

pub fn dispatch(args: &WinArgs) -> CliResult<i32> {
    #[cfg(not(windows))]
    {
        let _ = args;
        Err(platform_error())
    }
    #[cfg(windows)]
    {
        windows::dispatch(&args.command)
    }
}

#[cfg(not(windows))]
fn platform_error() -> CliError {
    CliError::new(
        "WIN_PLATFORM",
        "native Windows sessions require Windows 11 with a local NTFS volume",
    )
    .hint("use the regular cowbox commands inside WSL2 for the Linux engine")
    .exit(125)
}

#[cfg(windows)]
mod windows {
    use super::*;
    use cowbox_core::win as cw;
    use cowbox_core::win::WinSession;
    use std::path::{Path, PathBuf};

    pub fn dispatch(command: &WinCommand) -> CliResult<i32> {
        match command {
            WinCommand::Doctor(args) => doctor(args),
            WinCommand::Init(args) => init(args),
            WinCommand::Checkpoint(args) => checkpoint(args),
            WinCommand::Log(args) => log(args),
            WinCommand::Diff(args) => diff(args),
            WinCommand::Rollback(args) => rollback(args),
            WinCommand::Commit(args) => commit(args),
            WinCommand::Discard(args) => discard(args),
        }
    }

    fn map(error: cw::WinError) -> CliError {
        CliError {
            code: error.code.to_string(),
            message: error.message,
            hint: error.hint,
            exit_code: error.exit_code,
        }
    }

    fn doctor(args: &WinDoctorArgs) -> CliResult<i32> {
        let report =
            cw::doctor(args.workspace.as_deref(), args.state_root.as_deref()).map_err(map)?;
        if args.json {
            print_ok("win doctor", None, serde_json::to_value(&report)?)?;
        } else {
            println!(
                "native Windows storage: {} ({})",
                report.storage_mode,
                if report.ready { "ready" } else { "not ready" }
            );
            if let Some(workspace) = &report.workspace {
                println!(
                    "workspace: {} [{}] free={}",
                    workspace.path,
                    workspace.filesystem.as_deref().unwrap_or("unknown"),
                    format_bytes(workspace.available_bytes.unwrap_or(0))
                );
            }
            if let Some(state) = &report.state_root {
                println!(
                    "state root: {} [{}] free={} writable={}",
                    state.path,
                    state.filesystem.as_deref().unwrap_or("unknown"),
                    format_bytes(state.available_bytes.unwrap_or(0)),
                    state
                        .writable
                        .map(|value| if value { "yes" } else { "no" })
                        .unwrap_or("not tested")
                );
            }
            println!(
                "long paths: {} ({} chars), ACL inspection: {}, ADS copy: {}",
                report.path_test.long_paths,
                report.path_test.tested_length,
                report.path_test.acl_inspection,
                report.path_test.ads_copy
            );
            for blocker in &report.blockers {
                println!("blocker: {blocker}");
            }
            for warning in &report.warnings {
                println!("warning: {warning}");
            }
        }
        Ok(if report.ready { 0 } else { 2 })
    }

    fn init(args: &WinInitArgs) -> CliResult<i32> {
        let report = cw::init_session(&args.workspace, &args.state_root).map_err(map)?;
        if args.json {
            print_ok("win init", None, serde_json::to_value(&report)?)?;
        } else {
            println!("session: {}", report.session_id);
            println!("open in the editor: {}", report.managed_workspace);
            println!(
                "imported {} files in {} directories, {} source bytes; estimated usage {}",
                report.files,
                report.directories,
                report.source_bytes,
                format_bytes(report.estimated_bytes)
            );
            if !report.hardlinks.is_empty() {
                println!(
                    "warning: {} hard-linked file(s) are imported read-only for commit purposes",
                    report.hardlinks.len()
                );
            }
        }
        Ok(0)
    }

    fn checkpoint(args: &WinCheckpointArgs) -> CliResult<i32> {
        let mut session = open_session(&args.session, args.state_root.as_deref())?;
        let report = session.checkpoint(args.label.clone()).map_err(map)?;
        if args.json {
            print_ok(
                "win checkpoint",
                Some(&session),
                serde_json::to_value(&report)?,
            )?;
        } else if report.created {
            println!(
                "checkpoint step {} (head {}): {} added, {} modified, {} deleted, {} new bytes",
                report.step,
                report.head,
                report.added,
                report.modified,
                report.deleted,
                report.bytes_added
            );
            println!("managed workspace: {}", report.managed_workspace);
        } else {
            println!(
                "no checkpoint created; the managed workspace matches step {}",
                report.head
            );
        }
        Ok(0)
    }

    fn log(args: &WinLogArgs) -> CliResult<i32> {
        let session_id = resolve_session(args.session.as_deref())?;
        let session = open_session(&session_id, args.state_root.as_deref())?;
        let report = session.log();
        if args.json {
            print_ok("win log", Some(&session), serde_json::to_value(&report)?)?;
        } else {
            println!(
                "session {} ({}) head={} workspace={}",
                report.session_id, report.status, report.head, report.workspace
            );
            for step in &report.steps {
                println!(
                    "{} step {}{} parent={} changes={} {}",
                    if step.head { "*" } else { " " },
                    step.step,
                    if step.rescue { " [rescue]" } else { "" },
                    step.parent
                        .map(|parent| parent.to_string())
                        .unwrap_or_else(|| "-".to_string()),
                    step.changes,
                    step.label
                );
            }
            println!("managed workspace: {}", report.managed_workspace);
        }
        Ok(0)
    }

    fn diff(args: &WinDiffArgs) -> CliResult<i32> {
        let session_id = resolve_session(args.session.as_deref())?;
        let session = open_session(&session_id, args.state_root.as_deref())?;
        let report = session.diff(args.step).map_err(map)?;
        if args.json {
            print_ok("win diff", Some(&session), serde_json::to_value(&report)?)?;
        } else if args.stat {
            println!(
                "{} file(s): {} added, {} modified, {} deleted, {} type-changed; +{} -{} bytes",
                report.summary.files,
                report.summary.added,
                report.summary.modified,
                report.summary.deleted,
                report.summary.type_changed,
                report.summary.bytes_added,
                report.summary.bytes_removed
            );
            if report.uncheckpointed {
                println!("note: the managed workspace has uncheckpointed edits");
            }
        } else {
            for change in &report.changes {
                let marker = match change.kind {
                    cw::WinChangeKind::Added => "A",
                    cw::WinChangeKind::Modified => "M",
                    cw::WinChangeKind::Deleted => "D",
                    cw::WinChangeKind::TypeChanged => "T",
                };
                let line_stats = match (change.lines_added, change.lines_removed) {
                    (Some(added), Some(removed)) => format!(" +{added} -{removed}"),
                    _ => String::new(),
                };
                println!("{marker} {}{}", change.rel_path, line_stats);
            }
            println!(
                "step {} vs base: {} file(s), {} bytes added, {} bytes removed{}",
                report.step,
                report.summary.files,
                report.summary.bytes_added,
                report.summary.bytes_removed,
                if report.uncheckpointed {
                    "; managed workspace has uncheckpointed edits"
                } else {
                    ""
                }
            );
        }
        Ok(0)
    }

    fn rollback(args: &WinRollbackArgs) -> CliResult<i32> {
        let mut session = open_session(&args.session, args.state_root.as_deref())?;
        let report = session.rollback(args.step).map_err(map)?;
        if args.json {
            print_ok(
                "win rollback",
                Some(&session),
                serde_json::to_value(&report)?,
            )?;
        } else {
            println!(
                "rollback: step {} -> step {} (head {})",
                report.previous_head, report.target, report.head
            );
            if let Some(rescue) = report.rescue_step {
                println!("saved uncheckpointed edits as rescue step {rescue}");
            }
            println!("continue in: {}", report.managed_workspace);
            println!(
                "generation workspace: {} ({} files)",
                report.generation_workspace, report.materialized_files
            );
        }
        Ok(0)
    }

    fn commit(args: &WinCommitArgs) -> CliResult<i32> {
        let mut session = open_session(&args.session, args.state_root.as_deref())?;
        if args.recover {
            if !args.yes {
                if !std::io::stdin().is_terminal() {
                    return Err(CliError::new(
                        "CONFIRMATION_REQUIRED",
                        "recovery requires explicit approval; stdin is not a terminal",
                    )
                    .hint("review the journal, then run `cowbox win commit --recover --yes`")
                    .exit(2));
                }
                eprint!("Recover the interrupted native Windows commit? [y/N] ");
                std::io::stderr().flush()?;
                let mut answer = String::new();
                std::io::stdin().read_line(&mut answer)?;
                if !matches!(answer.trim(), "y" | "Y" | "yes" | "YES") {
                    if args.json {
                        print_ok(
                            "win commit",
                            Some(&session),
                            json!({"recovered": false, "cancelled": true}),
                        )?;
                    } else {
                        println!("recovery cancelled");
                    }
                    return Ok(0);
                }
            }
            let faults = cw::WinFaults::from_env();
            let report = cw::commit::recover(&mut session, &faults).map_err(map)?;
            if args.json {
                print_ok("win commit", Some(&session), serde_json::to_value(&report)?)?;
            } else {
                println!(
                    "recovery: {:?}, restored={}, finalized={}",
                    report.action, report.restored, report.finalized
                );
                if let Some(journal) = &report.journal {
                    println!("journal: {journal}");
                }
            }
            return Ok(0);
        }

        let preview = cw::commit::preview(&session, args.step, args.force).map_err(map)?;
        if args.dry_run {
            if args.json {
                print_ok(
                    "win commit",
                    Some(&session),
                    serde_json::to_value(&preview)?,
                )?;
            } else {
                print_preview(&preview);
            }
            return Ok(0);
        }
        if !preview.changes.is_empty() && !args.yes {
            if !std::io::stdin().is_terminal() {
                return Err(CliError::new(
                    "CONFIRMATION_REQUIRED",
                    "commit requires explicit approval; stdin is not a terminal",
                )
                .hint("review `cowbox win diff` and `cowbox win commit --dry-run`, then run --yes")
                .exit(2));
            }
            print_preview(&preview);
            eprint!("Commit these changes to the host? [y/N] ");
            std::io::stderr().flush()?;
            let mut answer = String::new();
            std::io::stdin().read_line(&mut answer)?;
            if !matches!(answer.trim(), "y" | "Y" | "yes" | "YES") {
                if args.json {
                    print_ok(
                        "win commit",
                        Some(&session),
                        json!({"committed": false, "cancelled": true}),
                    )?;
                } else {
                    println!("commit cancelled; the host was not modified");
                }
                return Ok(0);
            }
        }
        let faults = cw::WinFaults::from_env();
        let report =
            cw::commit::commit(&mut session, args.step, args.force, &faults).map_err(map)?;
        if args.json {
            print_ok("win commit", Some(&session), serde_json::to_value(&report)?)?;
        } else if report.committed {
            println!(
                "committed step {}: {} change(s), journal={}",
                report.step,
                report.changes,
                report.journal.as_deref().unwrap_or("-")
            );
        } else {
            println!("nothing to commit");
        }
        Ok(0)
    }

    fn discard(args: &WinDiscardArgs) -> CliResult<i32> {
        let mut session = open_session(&args.session, args.state_root.as_deref())?;
        if !args.yes {
            if !std::io::stdin().is_terminal() {
                return Err(CliError::new(
                    "CONFIRMATION_REQUIRED",
                    "discard requires explicit approval; stdin is not a terminal",
                )
                .hint("run `cowbox win discard --yes` after reviewing the session")
                .exit(2));
            }
            eprint!(
                "Discard session {} and delete its managed files? [y/N] ",
                args.session
            );
            std::io::stderr().flush()?;
            let mut answer = String::new();
            std::io::stdin().read_line(&mut answer)?;
            if !matches!(answer.trim(), "y" | "Y" | "yes" | "YES") {
                if args.json {
                    print_ok(
                        "win discard",
                        Some(&session),
                        json!({"discarded": false, "cancelled": true}),
                    )?;
                } else {
                    println!("discard cancelled");
                }
                return Ok(0);
            }
        }
        let report = session.discard().map_err(map)?;
        if args.json {
            print_ok(
                "win discard",
                Some(&session),
                serde_json::to_value(&report)?,
            )?;
        } else {
            println!(
                "discarded session {}; freed {}",
                report.session_id,
                format_bytes(report.freed_bytes)
            );
            if report.uncheckpointed {
                println!("note: uncheckpointed managed edits were removed with the session");
            }
        }
        Ok(0)
    }

    fn open_session(session_id: &str, state_root: Option<&Path>) -> CliResult<WinSession> {
        WinSession::open(session_id, state_root).map_err(map)
    }

    fn resolve_session(session: Option<&str>) -> CliResult<String> {
        if let Some(session) = session {
            return Ok(session.to_string());
        }
        let cwd = std::env::current_dir()?;
        let workspace = crate::workspace::find_workspace_root(&cwd)?;
        let sessions = cw::active_sessions_for_workspace(&workspace).map_err(map)?;
        match sessions.len() {
            0 => Err(CliError::new(
                "WIN_SESSION_REQUIRED",
                format!(
                    "no active native Windows session is registered for {}",
                    workspace.display()
                ),
            )
            .hint("pass --session <id> or run `cowbox win init`")
            .exit(2)),
            1 => Ok(sessions[0].id.clone()),
            _ => Err(CliError::new(
                "WIN_SESSION_AMBIGUOUS",
                format!(
                    "{} active sessions exist for {}; pass --session",
                    sessions.len(),
                    workspace.display()
                ),
            )
            .exit(2)),
        }
    }

    fn print_preview(preview: &cw::WinCommitPreview) {
        eprintln!(
            "commit preview: step {} has {} change(s): {} added, {} modified, {} deleted; {} will be written",
            preview.step,
            preview.summary.files,
            preview.summary.added,
            preview.summary.modified,
            preview.summary.deleted,
            format_bytes(preview.bytes_to_write)
        );
        for change in preview.changes.iter().take(20) {
            eprintln!("  {}", change.rel_path);
        }
        if preview.changes.len() > 20 {
            eprintln!("  ... and {} more", preview.changes.len() - 20);
        }
        if preview.host_drift {
            eprintln!("host drift: {}", preview.conflicts.join(", "));
        }
        for unsupported in &preview.unsupported {
            eprintln!(
                "unsupported: {} [{}] {}",
                unsupported.rel_path, unsupported.code, unsupported.message
            );
        }
        eprintln!(
            "can commit: {}; bytes to write: {}",
            preview.can_commit,
            format_bytes(preview.bytes_to_write)
        );
    }

    fn print_ok(command: &str, session: Option<&WinSession>, data: Value) -> CliResult<()> {
        let session = match session {
            Some(session) => serde_json::to_value(session.summary())?,
            None => Value::Null,
        };
        json::print(&json!({
            "schema_version": 1,
            "ok": true,
            "command": command,
            "session": session,
            "data": data,
        }))
    }

    fn format_bytes(bytes: u64) -> String {
        const KIB: u64 = 1024;
        const MIB: u64 = 1024 * KIB;
        const GIB: u64 = 1024 * MIB;
        if bytes >= GIB {
            format!("{:.2} GiB", bytes as f64 / GIB as f64)
        } else if bytes >= MIB {
            format!("{:.1} MiB", bytes as f64 / MIB as f64)
        } else if bytes >= KIB {
            format!("{:.1} KiB", bytes as f64 / KIB as f64)
        } else {
            format!("{bytes} B")
        }
    }

    #[allow(dead_code)]
    fn _path(path: &Path) -> PathBuf {
        path.to_path_buf()
    }
}
