use crate::agent::{config as agent_config, harness, policy, rules, skill};
use crate::cli::{
    CliCommand, CommitArgs, DemoArgs, DiffArgs, DiscardArgs, DoctorArgs, GcArgs, HiddenArgs,
    InitArgs, InstallRulesArgs, InstallSkillArgs, LogArgs, PathArgs, PolicyArgs, RollbackArgs,
    RunArgs, SessionsArgs, SetupArgs, StatsArgs, UndoArgs, UpdateArgs,
};
use crate::config::{Config, RunPolicyOverrides};
#[cfg(not(windows))]
use crate::doctor;
use crate::error::{CliError, CliResult};
use crate::json;
use crate::render::{self, Stats};
use crate::setup;
use crate::state::{GlobalSession, GlobalSessionStore, SessionRecord, SessionStatus, SessionStore};
use crate::workspace;
use cowbox_core::{CommitRecoveryAction, PendingRecovery, SandboxBackend, SessionLayout};
use cowbox_dag::{BASE_STEP, StepNode};
use serde_json::json;
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::io::{BufRead, IsTerminal};
use std::path::PathBuf;
use std::time::Duration;

struct Context {
    workspace: PathBuf,
    config: Config,
    state_root: PathBuf,
    store: SessionStore,
}

pub fn dispatch(command: &CliCommand) -> CliResult<i32> {
    #[cfg(windows)]
    if let Some(result) = crate::windows_bridge::delegate_if_needed(command) {
        return result;
    }

    match command {
        CliCommand::Init(args) => command_init(args),
        CliCommand::Run(args) => command_run(args),
        CliCommand::Diff(args) => command_diff(args),
        CliCommand::Log(args) => command_log(args),
        CliCommand::Rollback(args) => command_rollback(args),
        CliCommand::Undo(args) => command_undo(args),
        CliCommand::Commit(args) => command_commit(args),
        CliCommand::Demo(args) => command_demo(args),
        CliCommand::Discard(args) => command_discard(args),
        CliCommand::Doctor(args) => command_doctor(args),
        CliCommand::Sessions(args) => command_sessions(args),
        CliCommand::Gc(args) => command_gc(args),
        CliCommand::Stats(args) => command_stats(args),
        CliCommand::Setup(args) => command_setup(args),
        CliCommand::InstallSkill(args) => command_install_skill(args),
        CliCommand::InstallRules(args) => command_install_rules(args),
        CliCommand::Policy(args) => command_policy(args),
        CliCommand::Update(args) => command_update(args),
        CliCommand::Path(args) => command_path(args),
        CliCommand::SandboxExec(args) => command_hidden(args, true),
        CliCommand::Supervisor(args) => command_hidden(args, false),
    }
}

fn setup() -> CliResult<Context> {
    let cwd = std::env::current_dir()?;
    let workspace = workspace::find_workspace_root(&cwd)?;
    let config = Config::load(&workspace)?;
    let state_root = workspace::state_root()?;
    workspace::validate_state_root(&state_root, &workspace)?;
    let store = SessionStore::new(workspace.clone(), state_root.clone())?;
    Ok(Context {
        workspace,
        config,
        state_root,
        store,
    })
}

fn backend() -> Box<dyn SandboxBackend> {
    cowbox_core::platform_backend()
}

fn command_init(args: &InitArgs) -> CliResult<i32> {
    let cwd = std::env::current_dir()?;
    let workspace = workspace::find_workspace_root(&cwd)?;
    let path = workspace.join(crate::config::CONFIG_FILE);
    let existed = path.exists();
    let path = Config::write_template(&workspace, args.force)?;
    if args.json {
        json::print(&json::ok(
            "init",
            None,
            json!({ "path": path, "existed": existed }),
        ))?;
    } else {
        println!("wrote {}", path.display());
        println!("next: cowbox doctor");
    }
    if args.agent {
        let results = rules::install(&workspace, false, false)?;
        if !args.json {
            for result in results {
                println!("{}: {}", result.path.display(), result.action);
            }
        }
    }
    Ok(0)
}

fn command_run(args: &RunArgs) -> CliResult<i32> {
    let ctx = setup()?;
    let _lock = ctx.store.lock()?;
    let backend = backend();
    let (mut record, created) = load_or_create(&ctx, backend.as_ref(), args.session.as_deref())?;
    let step_id = record.dag.next_step_id();
    let parent_layers = record.dag.active_layer_chain();
    let policy = ctx.config.to_exec_policy(
        &ctx.workspace,
        &RunPolicyOverrides {
            allow_read: args.allow_read.clone(),
            allow_write: args.allow_write.clone(),
            env: args.env.clone(),
            clean_env: args.clean_env,
            timeout: args.timeout.clone(),
        },
        args.json,
    )?;
    let layout = ctx.store.layout(&record.session_id);
    let outcome = match backend.exec_step(&layout, &parent_layers, step_id, &args.cmd, &policy) {
        Ok(outcome) => outcome,
        Err(error) => {
            if created {
                let _ = ctx.store.clear_active();
                let _ = ctx.store.remove_session(&record.session_id);
            }
            return Err(error.into());
        }
    };

    let previous_head = record.dag.head_step;
    let mut node = StepNode::new(
        step_id,
        Some(previous_head),
        truncate(&command_label(&args.cmd), 120),
    );
    node.timestamp_unix_ms = workspace::now_ms() as u64;
    node.exit_code = Some(outcome.exit_code);
    node.signal = outcome.signal;
    node.duration_ms = outcome.duration_ms;
    node.orphans_reaped = outcome.orphans_reaped;
    node.timed_out = outcome.timed_out;
    record.dag.add_step(node)?;
    record.updated_at_unix_ms = workspace::now_ms();
    ctx.store.save(&record)?;
    ctx.store.append_event(
        &record.session_id,
        &json!({
            "event": "run",
            "ts": workspace::now_ms(),
            "step": step_id,
            "exit_code": outcome.exit_code,
            "timed_out": outcome.timed_out,
            "orphans_reaped": outcome.orphans_reaped,
        }),
    )?;

    if args.json {
        json::print(&json::ok(
            "run",
            Some(&record),
            json!({
                "step": step_id,
                "exit_code": outcome.exit_code,
                "signal": outcome.signal,
                "duration_ms": outcome.duration_ms,
                "orphans_reaped": outcome.orphans_reaped,
                "timed_out": outcome.timed_out,
                "stdout": outcome.stdout,
                "stderr": outcome.stderr,
                "stdout_truncated": outcome.stdout_truncated,
                "stderr_truncated": outcome.stderr_truncated,
            }),
        ))?;
    } else {
        eprintln!(
            "[cowbox] Step {} recorded (exit {}, {} ms). Run `cowbox diff` to inspect or `cowbox rollback {}` to undo.",
            step_id, outcome.exit_code, outcome.duration_ms, previous_head
        );
    }
    Ok(outcome.exit_code)
}

fn command_diff(args: &DiffArgs) -> CliResult<i32> {
    if args.base != BASE_STEP {
        return Err(CliError::new(
            "UNSUPPORTED_BASE",
            "M1 can only diff from the base step (base 0)",
        )
        .exit(2));
    }
    let ctx = setup()?;
    let record = active_record(&ctx)?;
    let target = args.step.unwrap_or(record.dag.head_step);
    if !record.dag.nodes.contains_key(&target) {
        return Err(CliError::new(
            "DAG_ERROR",
            format!("step {target} does not exist"),
        ));
    }
    let chain = target_chain(&record, target)?;
    let options = ctx.config.to_diff_options(args.all);
    let layout = ctx.store.layout(&record.session_id);
    let changes = backend().diff_layers(&layout, &[], &chain, &options)?;
    let summary = render::summarize(&changes);
    if args.json {
        json::print(&json::ok(
            "diff",
            Some(&record),
            json!({
                "base": args.base,
                "step": target,
                "head": record.dag.head_step,
                "changes": changes,
                "summary": render::summary_json(&summary),
            }),
        ))?;
    } else {
        render::print_diff(
            &record,
            args.base,
            target,
            &changes,
            args.stat,
            args.name_only,
        );
    }
    Ok(0)
}

fn command_log(args: &LogArgs) -> CliResult<i32> {
    let ctx = setup()?;
    let record = active_record(&ctx)?;
    if args.json {
        json::print(&json::ok(
            "log",
            Some(&record),
            json!({
                "dag": record.dag,
                "rollback_count": record.rollback_count,
                "last_commit_unix_ms": record.last_commit_unix_ms,
            }),
        ))?;
    } else {
        render::print_log(&record);
    }
    Ok(0)
}

fn command_rollback(args: &RollbackArgs) -> CliResult<i32> {
    let ctx = setup()?;
    let _lock = ctx.store.lock()?;
    let mut record = active_record(&ctx)?;
    ensure_no_pending_commit(&ctx, &record)?;
    let previous = record.dag.head_step;
    record.dag.rollback(args.step)?;
    record.rollback_count += u64::from(previous != record.dag.head_step);
    record.updated_at_unix_ms = workspace::now_ms();
    ctx.store.save(&record)?;
    ctx.store.append_event(
        &record.session_id,
        &json!({"event":"rollback","ts":workspace::now_ms(),"from":previous,"to":record.dag.head_step}),
    )?;
    if args.json {
        json::print(&json::ok(
            "rollback",
            Some(&record),
            json!({ "previous_head": previous, "head": record.dag.head_step }),
        ))?;
    } else {
        println!(
            "rolled back from step {previous} to step {}",
            record.dag.head_step
        );
    }
    Ok(0)
}

fn command_undo(args: &UndoArgs) -> CliResult<i32> {
    let ctx = setup()?;
    let _lock = ctx.store.lock()?;
    let mut record = active_record(&ctx)?;
    ensure_no_pending_commit(&ctx, &record)?;
    let previous = record.dag.head_step;
    let target = record.dag.undo(args.steps)?;
    record.rollback_count += u64::from(previous != target);
    record.updated_at_unix_ms = workspace::now_ms();
    ctx.store.save(&record)?;
    ctx.store.append_event(
        &record.session_id,
        &json!({"event":"undo","ts":workspace::now_ms(),"from":previous,"to":target,"steps":args.steps}),
    )?;
    if args.json {
        json::print(&json::ok(
            "undo",
            Some(&record),
            json!({ "previous_head": previous, "head": target, "steps": args.steps }),
        ))?;
    } else {
        println!("undo: step {previous} -> step {target}");
    }
    Ok(0)
}

fn command_commit(args: &CommitArgs) -> CliResult<i32> {
    let ctx = setup()?;
    let _lock = ctx.store.lock()?;
    if args.recover {
        return command_commit_recover(&ctx, args);
    }
    let mut record = active_record(&ctx)?;
    ensure_no_pending_commit(&ctx, &record)?;
    let target = args.step.unwrap_or(record.dag.head_step);
    if !record.dag.nodes.contains_key(&target) {
        return Err(CliError::new(
            "DAG_ERROR",
            format!("step {target} does not exist"),
        ));
    }
    let chain = target_chain(&record, target)?;
    let preview_options = ctx
        .config
        .to_commit_options(args.force, true, args.include_excluded);
    let layout = ctx.store.layout(&record.session_id);
    let preview = backend().commit_layers(&layout, &chain, &preview_options)?;
    if !preview.conflicts.is_empty() && !preview_options.force {
        let paths = preview
            .conflicts
            .iter()
            .map(|path| path.display().to_string())
            .collect::<Vec<_>>()
            .join(", ");
        return Err(CliError::new(
            "COMMIT_CONFLICT",
            format!("host workspace changed since the session started: {paths}"),
        )
        .hint("inspect the paths, then retry with --force if they should be overwritten")
        .exit(2));
    }

    if args.dry_run {
        let summary = render::summarize(&preview.changes);
        if args.json {
            json::print(&json::ok(
                "commit",
                Some(&record),
                json!({
                    "dry_run": true,
                    "committed": false,
                    "changes": preview.changes,
                    "conflicts": preview.conflicts,
                    "summary": render::summary_json(&summary),
                }),
            ))?;
        } else {
            println!(
                "dry-run: {} change(s), {} conflict(s); nothing written",
                preview.changes.len(),
                preview.conflicts.len()
            );
        }
        return Ok(0);
    }

    if !args.yes && !preview.changes.is_empty() {
        if !std::io::stdin().is_terminal() {
            return Err(CliError::new(
                "CONFIRMATION_REQUIRED",
                "commit requires explicit approval; stdin is not a terminal",
            )
            .hint("review the diff, then run `cowbox commit --yes` after human approval")
            .exit(2));
        }
        if !confirm_commit(&ctx.workspace, &preview.changes)? {
            if args.json {
                json::print(&json::ok(
                    "commit",
                    Some(&record),
                    json!({
                        "committed": false,
                        "cancelled": true,
                        "changes": preview.changes,
                        "conflicts": preview.conflicts,
                    }),
                ))?;
            } else {
                println!("commit cancelled; active session left unchanged");
            }
            return Ok(0);
        }
    }

    let options = ctx
        .config
        .to_commit_options(args.force, false, args.include_excluded);
    let report = backend().commit_layers(&layout, &chain, &options)?;
    if !report.conflicts.is_empty() && !options.force {
        let paths = report
            .conflicts
            .iter()
            .map(|path| path.display().to_string())
            .collect::<Vec<_>>()
            .join(", ");
        return Err(CliError::new(
            "COMMIT_CONFLICT",
            format!("host workspace changed since the session started: {paths}"),
        )
        .hint("inspect the paths, then retry with --force if they should be overwritten")
        .exit(2));
    }
    if report.committed {
        let journal = report.journal.clone();
        finish_committed_session(&ctx, &mut record, target, journal.as_deref(), false)?;
    }

    if args.json {
        json::print(&json::ok(
            "commit",
            Some(&record),
            json!({
                "committed": report.committed,
                "changes": report.changes,
                "conflicts": report.conflicts,
                "journal": report.journal,
            }),
        ))?;
    } else if report.committed {
        println!(
            "committed {} change(s) from step {target}; active session closed",
            report.changes.len()
        );
        if let Some(journal) = report.journal {
            println!("journal: {}", journal.display());
        }
    } else {
        println!("nothing committed (no net changes)");
    }
    Ok(0)
}

fn command_commit_recover(ctx: &Context, args: &CommitArgs) -> CliResult<i32> {
    let Some((mut record, pending)) = find_pending_recovery(ctx)? else {
        if args.json {
            json::print(&json::ok(
                "commit",
                None,
                json!({
                    "recovered": false,
                    "action": CommitRecoveryAction::NoJournal,
                    "already_complete": true,
                }),
            ))?;
        } else {
            println!("no interrupted commit to recover");
        }
        return Ok(0);
    };
    if !args.yes {
        if !std::io::stdin().is_terminal() {
            return Err(CliError::new(
                "CONFIRMATION_REQUIRED",
                "commit recovery requires --yes when stdin is not a terminal",
            )
            .hint("inspect the journal, then run `cowbox commit --recover --yes`")
            .exit(2));
        }
        if !confirm_recovery(&ctx.workspace, &record.session_id, &pending)? {
            if args.json {
                json::print(&json::ok(
                    "commit",
                    Some(&record),
                    json!({
                        "recovered": false,
                        "cancelled": true,
                        "action": pending.action,
                        "journal": pending.journal,
                    }),
                ))?;
            } else {
                println!("recovery cancelled; journal left unchanged");
            }
            return Ok(0);
        }
    }

    let layout = ctx.store.layout(&record.session_id);
    let report = backend().recover_commit(&layout)?;
    match report.action {
        CommitRecoveryAction::RestoreHost | CommitRecoveryAction::AlreadyRolledBack => {
            ensure_recovered_session_active(ctx, &record)?;
            ctx.store.append_event(
                &record.session_id,
                &json!({
                    "event": "commit_recovered",
                    "ts": workspace::now_ms(),
                    "action": report.action,
                    "journal": report.journal,
                }),
            )?;
        }
        CommitRecoveryAction::FinalizeCommit | CommitRecoveryAction::AlreadyFinalized => {
            let target = pending.target_step.unwrap_or(record.dag.head_step);
            finish_committed_session(ctx, &mut record, target, report.journal.as_deref(), true)?;
        }
        CommitRecoveryAction::NoJournal => {}
    }

    if args.json {
        json::print(&json::ok(
            "commit",
            Some(&record),
            json!({
                "recovered": true,
                "action": report.action,
                "journal": report.journal,
                "restored": report.restored,
            }),
        ))?;
    } else {
        match report.action {
            CommitRecoveryAction::RestoreHost => println!(
                "restored {} path(s) from journal {}; session remains active",
                report.restored,
                report
                    .journal
                    .as_ref()
                    .map(|path| path.display().to_string())
                    .unwrap_or_else(|| "-".to_string())
            ),
            CommitRecoveryAction::FinalizeCommit | CommitRecoveryAction::AlreadyFinalized => {
                println!(
                    "commit was already applied; finalized session {} and removed its recovery journal",
                    record.session_id
                )
            }
            CommitRecoveryAction::AlreadyRolledBack => {
                println!("commit had already been rolled back; session remains active")
            }
            CommitRecoveryAction::NoJournal => println!("nothing to recover"),
        }
    }
    Ok(0)
}

fn find_pending_recovery(ctx: &Context) -> CliResult<Option<(SessionRecord, PendingRecovery)>> {
    let backend = backend();
    let mut records = BTreeMap::new();
    if let Some(record) = ctx.store.load_active_record_any_status()? {
        records.insert(record.session_id.clone(), record);
    }
    let inventory = GlobalSessionStore::new(&ctx.state_root).list()?;
    for session in inventory.sessions {
        if !session.valid || session.workspace.as_deref() != Some(ctx.workspace.as_path()) {
            continue;
        }
        let record = ctx.store.load(&session.session_id)?;
        records.insert(record.session_id.clone(), record);
    }
    let mut found = Vec::new();
    for record in records.into_values() {
        let layout = ctx.store.layout(&record.session_id);
        if let Some(pending) = backend.inspect_recovery(&layout)? {
            found.push((record, pending));
        }
    }
    if found.len() > 1 {
        let sessions = found
            .iter()
            .map(|(record, pending)| {
                format!("{} ({})", record.session_id, pending.journal.display())
            })
            .collect::<Vec<_>>()
            .join(", ");
        return Err(CliError::new(
            "STATE_CORRUPTED",
            format!("multiple sessions have interrupted commits: {sessions}"),
        )
        .hint("inspect each journal under the state root before recovering manually")
        .exit(1));
    }
    Ok(found.pop())
}

fn ensure_no_pending_commit(ctx: &Context, record: &SessionRecord) -> CliResult<()> {
    let layout = ctx.store.layout(&record.session_id);
    if backend().inspect_recovery(&layout)?.is_some() {
        return Err(CliError::new(
            "COMMIT_RECOVERY_REQUIRED",
            format!(
                "session {} has an interrupted commit that must be reconciled first",
                record.session_id
            ),
        )
        .hint(
            "run `cowbox commit --recover --yes`; it restores the host before the commit point or finalizes an already-applied commit",
        )
        .exit(2));
    }
    Ok(())
}

fn ensure_recovered_session_active(ctx: &Context, record: &SessionRecord) -> CliResult<()> {
    if record.status != SessionStatus::Active {
        return Err(CliError::new(
            "STATE_CORRUPTED",
            format!(
                "journal requires host restore, but session {} is {:?}",
                record.session_id, record.status
            ),
        )
        .hint("inspect the session and journal before changing either")
        .exit(1));
    }
    match ctx.store.load_active_record_any_status()? {
        Some(existing) if existing.session_id == record.session_id => Ok(()),
        Some(existing) => Err(CliError::new(
            "SESSION_CONFLICT",
            format!(
                "workspace active pointer references {}, expected {}",
                existing.session_id, record.session_id
            ),
        )
        .exit(2)),
        None => ctx.store.write_active(record),
    }
}

fn finish_committed_session(
    ctx: &Context,
    record: &mut SessionRecord,
    target: u64,
    journal: Option<&std::path::Path>,
    recovered: bool,
) -> CliResult<()> {
    match record.status {
        SessionStatus::Active => {
            record.status = SessionStatus::Committed;
            record.last_commit_unix_ms = Some(workspace::now_ms());
            record.updated_at_unix_ms = workspace::now_ms();
            ctx.store.save(record)?;
            ctx.store.append_event(
                &record.session_id,
                &json!({
                    "event": "commit",
                    "ts": workspace::now_ms(),
                    "step": target,
                    "recovered": recovered,
                    "journal": journal,
                }),
            )?;
            test_crash("commit-after-session-save");
        }
        SessionStatus::Committed => {}
        SessionStatus::Discarded => {
            return Err(CliError::new(
                "STATE_CORRUPTED",
                format!(
                    "session {} was discarded during commit finalization",
                    record.session_id
                ),
            )
            .exit(1));
        }
    }
    ctx.store.clear_active()?;
    test_crash("commit-after-active-clear");
    if let Some(journal) = journal {
        let layout = ctx.store.layout(&record.session_id);
        if let Err(error) = backend().finalize_commit(&layout) {
            let error: CliError = error.into();
            return Err(CliError::new(
                "COMMIT_FINALIZE_FAILED",
                format!(
                    "commit was applied and the session was marked committed, but journal {} could not be finalized: {}",
                    journal.display(),
                    error.message
                ),
            )
            .hint("run `cowbox commit --recover --yes` to finish cleanup; backups were kept")
            .exit(1));
        }
    }
    Ok(())
}

fn test_crash(point: &str) {
    if !cfg!(debug_assertions) {
        return;
    }
    if std::env::var_os("COWBOX_TEST_CRASH").as_deref() != Some(std::ffi::OsStr::new(point)) {
        return;
    }
    std::process::abort();
}

fn command_demo(args: &DemoArgs) -> CliResult<i32> {
    crate::demo::run(args)
}

fn confirm_commit(
    workspace: &std::path::Path,
    changes: &[cowbox_core::FileChange],
) -> CliResult<bool> {
    let summary = render::summarize(changes);
    eprintln!(
        "About to write {} file(s) to {}: {} added, {} modified, {} deleted.",
        summary.files,
        workspace.display(),
        summary.added,
        summary.modified,
        summary.deleted
    );
    for change in changes.iter().take(20) {
        eprintln!("  {}", change.rel_path.display());
    }
    if changes.len() > 20 {
        eprintln!("  ... and {} more", changes.len() - 20);
    }
    eprint!("Commit these changes to the host? [y/N] ");
    std::io::Write::flush(&mut std::io::stderr())?;
    let mut answer = String::new();
    std::io::stdin().lock().read_line(&mut answer)?;
    Ok(matches!(answer.trim(), "y" | "Y" | "yes" | "YES"))
}

fn confirm_recovery(
    workspace: &std::path::Path,
    session_id: &str,
    pending: &PendingRecovery,
) -> CliResult<bool> {
    eprintln!(
        "Session {session_id} in {} has an interrupted commit at journal {}.",
        workspace.display(),
        pending.journal.display()
    );
    match pending.action {
        CommitRecoveryAction::RestoreHost => {
            eprintln!("The commit did not reach its durable commit point.");
            eprint!("Restore the affected host paths and keep the session active? [y/N] ");
        }
        CommitRecoveryAction::FinalizeCommit => {
            eprintln!("The host already contains the committed content.");
            eprint!("Keep it, mark the session committed, and remove the journal? [y/N] ");
        }
        CommitRecoveryAction::AlreadyFinalized
        | CommitRecoveryAction::AlreadyRolledBack
        | CommitRecoveryAction::NoJournal => {
            eprint!("Finish commit reconciliation? [y/N] ");
        }
    }
    std::io::Write::flush(&mut std::io::stderr())?;
    let mut answer = String::new();
    std::io::stdin().lock().read_line(&mut answer)?;
    Ok(matches!(answer.trim(), "y" | "Y" | "yes" | "YES"))
}

fn command_discard(args: &DiscardArgs) -> CliResult<i32> {
    let ctx = setup()?;
    let _lock = ctx.store.lock()?;
    let Some(record) = ctx.store.load_active()? else {
        if args.json {
            json::print(&json::ok("discard", None, json!({ "discarded": false })))?;
        } else {
            println!("no active session");
        }
        return Ok(0);
    };
    ensure_no_pending_commit(&ctx, &record)?;
    if !args.yes {
        if !std::io::stdin().is_terminal() {
            return Err(CliError::new(
                "CONFIRMATION_REQUIRED",
                "discard requires --yes when stdin is not a terminal",
            )
            .exit(2));
        }
        print!("discard session {}? [y/N] ", record.session_id);
        std::io::Write::flush(&mut std::io::stdout())?;
        let mut answer = String::new();
        std::io::stdin().lock().read_line(&mut answer)?;
        if !matches!(answer.trim(), "y" | "Y" | "yes" | "YES") {
            println!("not discarded");
            return Ok(0);
        }
    }
    let layout = ctx.store.layout(&record.session_id);
    backend().destroy_session(&layout)?;
    ctx.store.clear_active()?;
    let session_id = record.session_id.clone();
    let session_dir = ctx.store.session_dir(&session_id);
    if args.purge {
        ctx.store.remove_session(&record.session_id)?;
    } else if session_dir.exists() {
        let mut record = record;
        record.status = SessionStatus::Discarded;
        record.updated_at_unix_ms = workspace::now_ms();
        ctx.store.save(&record)?;
    }
    if args.json {
        json::print(&json::ok(
            "discard",
            None,
            json!({ "discarded": true, "purged": args.purge, "session_id": session_id }),
        ))?;
    } else {
        println!(
            "discarded session {}{}",
            session_id,
            if args.purge { " (purged)" } else { "" }
        );
    }
    Ok(0)
}

fn command_doctor(args: &DoctorArgs) -> CliResult<i32> {
    #[cfg(windows)]
    {
        crate::windows_bridge::doctor(args)
    }
    #[cfg(not(windows))]
    {
        let context = doctor::diagnostic_context()?;
        let capabilities = backend().probe(&context.state_root)?;
        let ready = doctor::capabilities_ready(&capabilities);
        if args.json {
            json::print(&json::ok(
                "doctor",
                None,
                json!({ "ready": ready, "capabilities": capabilities }),
            ))?;
        } else {
            render::print_doctor(&capabilities, &context.state_root, ready);
        }
        Ok(if ready { 0 } else { 1 })
    }
}

fn command_sessions(args: &SessionsArgs) -> CliResult<i32> {
    let state_root = workspace::state_root()?;
    let inventory = GlobalSessionStore::new(&state_root).list()?;
    if args.json {
        json::print(&json::ok(
            "sessions",
            None,
            json!({
                "state_root": state_root,
                "sessions": inventory.sessions,
                "warnings": inventory.warnings,
            }),
        ))?;
    } else {
        print_session_inventory(&state_root, &inventory.sessions, &inventory.warnings);
    }
    Ok(0)
}

fn command_gc(args: &GcArgs) -> CliResult<i32> {
    let state_root = workspace::state_root()?;
    let store = GlobalSessionStore::new(&state_root);
    let inventory = store.list()?;
    if !inventory.warnings.is_empty() {
        return Err(CliError::new(
            "STATE_CORRUPTED",
            format!(
                "cannot safely garbage-collect sessions: {}",
                inventory.warnings.join("; ")
            ),
        )
        .hint("inspect the state root with `cowbox sessions` and repair unreadable pointers")
        .exit(1));
    }

    let age = if args.all {
        None
    } else {
        Some(parse_gc_age(args.older_than.as_deref().unwrap_or("7d"))?)
    };
    let cutoff_ms = age.map(|age| {
        workspace::now_ms().saturating_sub(i64::try_from(age.as_millis()).unwrap_or(i64::MAX))
    });
    let mut candidates = Vec::new();
    let mut skipped_active = 0usize;
    let mut skipped_invalid = 0usize;
    let mut skipped_recent = 0usize;
    let mut skipped_recovery = 0usize;
    let backend = backend();
    for session in inventory.sessions {
        if !session.valid {
            skipped_invalid += 1;
            continue;
        }
        if session.active || session.status == "active" {
            skipped_active += 1;
            continue;
        }
        if let (Some(cutoff), Some(updated)) = (cutoff_ms, session.updated_at_unix_ms) {
            if updated > cutoff {
                skipped_recent += 1;
                continue;
            }
        }
        let Some(workspace) = session.workspace.as_ref() else {
            skipped_invalid += 1;
            continue;
        };
        let layout = SessionLayout::new(&session.session_id, workspace, &state_root);
        if backend.inspect_recovery(&layout)?.is_some() {
            skipped_recovery += 1;
            continue;
        }
        candidates.push(session);
    }
    let plan = GcPlan {
        age,
        candidate_bytes: candidates.iter().map(|session| session.disk_bytes).sum(),
        candidates,
        skipped_active,
        skipped_invalid,
        skipped_recent,
        skipped_recovery,
    };

    if plan.candidates.is_empty() {
        return finish_gc(args, &state_root, &plan, &[], 0, false);
    }

    if args.json {
        if !args.dry_run && !args.yes {
            eprintln!(
                "About to remove {} finalized session(s), freeing {}.",
                plan.candidates.len(),
                render::format_bytes(plan.candidate_bytes)
            );
        }
    } else {
        print_gc_candidates(&plan.candidates, plan.candidate_bytes);
    }

    if args.dry_run {
        return finish_gc(args, &state_root, &plan, &[], 0, false);
    }

    if !args.yes {
        if !std::io::stdin().is_terminal() {
            return Err(CliError::new(
                "CONFIRMATION_REQUIRED",
                "gc requires --yes when stdin is not a terminal",
            )
            .hint("review with `cowbox gc --dry-run --json`, then rerun with --yes")
            .exit(2));
        }
        if !confirm_gc(plan.candidates.len(), plan.candidate_bytes)? {
            return finish_gc(args, &state_root, &plan, &[], 0, true);
        }
    }

    let mut removed = Vec::new();
    let mut freed_bytes = 0u64;
    for session in &plan.candidates {
        let workspace = session.workspace.as_ref().ok_or_else(|| {
            CliError::new(
                "STATE_CORRUPTED",
                format!("session {} has no workspace path", session.session_id),
            )
        })?;
        let layout = SessionLayout::new(&session.session_id, workspace, &state_root);
        if let Err(error) = backend.destroy_session(&layout) {
            let error: CliError = error.into();
            return Err(CliError::new(
                error.code,
                format!(
                    "failed to remove session {}: {}; {} session(s) were removed before the failure",
                    session.session_id,
                    error.message,
                    removed.len()
                ),
            )
            .hint(error.hint.unwrap_or_else(|| {
                "inspect the state root permissions before retrying".to_string()
            }))
            .exit(error.exit_code));
        }
        removed.push(session.session_id.clone());
        freed_bytes += session.disk_bytes;
    }

    finish_gc(args, &state_root, &plan, &removed, freed_bytes, false)
}

fn parse_gc_age(value: &str) -> CliResult<Duration> {
    humantime::parse_duration(value).map_err(|error| {
        CliError::new(
            "CONFIG_ERROR",
            format!("invalid --older-than value {value:?}: {error}"),
        )
        .exit(2)
    })
}

fn confirm_gc(count: usize, bytes: u64) -> CliResult<bool> {
    eprintln!(
        "Remove {count} finalized session(s) and free {}?",
        render::format_bytes(bytes)
    );
    eprint!("This deletes sandbox state only, not workspace files. [y/N] ");
    std::io::Write::flush(&mut std::io::stderr())?;
    let mut answer = String::new();
    std::io::stdin().lock().read_line(&mut answer)?;
    Ok(matches!(answer.trim(), "y" | "Y" | "yes" | "YES"))
}

fn print_session_inventory(
    state_root: &std::path::Path,
    sessions: &[GlobalSession],
    warnings: &[String],
) {
    println!("State root: {}", state_root.display());
    for warning in warnings {
        eprintln!("warning: {warning}");
    }
    if sessions.is_empty() {
        println!("no sessions");
        return;
    }
    println!(
        "{:<12} {:<10} {:<6} {:>5} {:>10}  {:<19}  WORKSPACE",
        "ID", "STATUS", "ACTIVE", "STEPS", "SIZE", "UPDATED"
    );
    let mut total_bytes = 0u64;
    for session in sessions {
        total_bytes += session.disk_bytes;
        println!(
            "{:<12} {:<10} {:<6} {:>5} {:>10}  {:<19}  {}",
            session.session_id,
            session.status,
            if session.active { "yes" } else { "-" },
            session.steps,
            render::format_bytes(session.disk_bytes),
            session
                .updated_at_unix_ms
                .map(workspace::format_timestamp)
                .unwrap_or_else(|| "-".to_string()),
            session
                .workspace
                .as_ref()
                .map(|path| path.display().to_string())
                .unwrap_or_else(|| "-".to_string())
        );
        if let Some(error) = &session.error {
            eprintln!(
                "warning: session {} is invalid: {error}",
                session.session_id
            );
        }
    }
    println!(
        "total: {} session(s), {}",
        sessions.len(),
        render::format_bytes(total_bytes)
    );
}

fn print_gc_candidates(candidates: &[GlobalSession], bytes: u64) {
    println!("Finalized session(s) eligible for removal:");
    for session in candidates {
        println!(
            "  {}  {:<10} {:>10}  {}",
            session.session_id,
            session.status,
            render::format_bytes(session.disk_bytes),
            session
                .workspace
                .as_ref()
                .map(|path| path.display().to_string())
                .unwrap_or_else(|| "-".to_string())
        );
    }
    println!(
        "{} session(s), {} reclaimable",
        candidates.len(),
        render::format_bytes(bytes)
    );
}

fn finish_gc(
    args: &GcArgs,
    state_root: &std::path::Path,
    plan: &GcPlan,
    removed: &[String],
    freed_bytes: u64,
    cancelled: bool,
) -> CliResult<i32> {
    if args.json {
        json::print(&json::ok(
            "gc",
            None,
            json!({
                "state_root": state_root,
                "older_than_seconds": plan.age.map(|age| age.as_secs()),
                "dry_run": args.dry_run,
                "cancelled": cancelled,
                "candidates": &plan.candidates,
                "candidate_bytes": plan.candidate_bytes,
                "removed": removed,
                "freed_bytes": freed_bytes,
                "skipped_active": plan.skipped_active,
                "skipped_invalid": plan.skipped_invalid,
                "skipped_recent": plan.skipped_recent,
                "skipped_recovery": plan.skipped_recovery,
            }),
        ))?;
    } else if cancelled {
        println!("gc cancelled; nothing removed");
    } else if args.dry_run {
        println!(
            "dry-run: would remove {} session(s) and free {}",
            plan.candidates.len(),
            render::format_bytes(plan.candidate_bytes)
        );
    } else if plan.candidates.is_empty() {
        println!("nothing to clean");
    } else {
        println!(
            "removed {} session(s), freed {}",
            removed.len(),
            render::format_bytes(freed_bytes)
        );
    }
    if !args.json
        && (plan.skipped_active
            + plan.skipped_invalid
            + plan.skipped_recent
            + plan.skipped_recovery
            > 0)
    {
        println!(
            "skipped: {} active, {} recent, {} invalid, {} with interrupted commits",
            plan.skipped_active, plan.skipped_recent, plan.skipped_invalid, plan.skipped_recovery
        );
    }
    Ok(0)
}

struct GcPlan {
    candidates: Vec<GlobalSession>,
    candidate_bytes: u64,
    age: Option<Duration>,
    skipped_active: usize,
    skipped_invalid: usize,
    skipped_recent: usize,
    skipped_recovery: usize,
}

fn command_stats(args: &StatsArgs) -> CliResult<i32> {
    let ctx = setup()?;
    let record = active_record(&ctx)?;
    let orphans = record
        .dag
        .nodes
        .values()
        .map(|node| node.orphans_reaped)
        .sum();
    let landlock_abi = backend()
        .probe(&ctx.state_root)
        .ok()
        .and_then(|capabilities| capabilities.landlock_abi);
    let stats = Stats {
        session_id: record.session_id.clone(),
        status: format!("{:?}", record.status),
        steps: record.dag.nodes.len().saturating_sub(1),
        rollbacks: record.rollback_count,
        orphans,
        disk_bytes: ctx.store.disk_usage(&record.session_id),
        landlock_abi,
        network_isolation: false,
        blocked_writes: None,
    };
    if args.json {
        json::print(&json::ok(
            "stats",
            Some(&record),
            json!({
                "steps": stats.steps,
                "rollbacks": stats.rollbacks,
                "orphans_reaped": stats.orphans,
                "disk_bytes": stats.disk_bytes,
                "landlock_abi": stats.landlock_abi,
                "network_isolation": stats.network_isolation,
                "blocked_writes": stats.blocked_writes,
            }),
        ))?;
    } else {
        render::print_stats(&stats);
    }
    Ok(0)
}

fn command_setup(args: &SetupArgs) -> CliResult<i32> {
    setup::run(args)
}

fn command_install_skill(args: &InstallSkillArgs) -> CliResult<i32> {
    let home = agent_config::home_dir()?;
    if args.list {
        return list_harnesses(&home, args.json);
    }
    if args.all && !args.harness.is_empty() {
        return Err(CliError::new("USAGE", "--all and --harness are mutually exclusive").exit(2));
    }

    let selected = if args.all {
        harness::ALL_HARNESSES.iter().collect::<Vec<_>>()
    } else if !args.harness.is_empty() {
        let mut selected = Vec::new();
        for id in &args.harness {
            let found = harness::find(id).ok_or_else(|| {
                CliError::new("UNKNOWN_HARNESS", format!("unknown harness {id}"))
                    .hint("run `cowbox install-skill --list`")
                    .exit(2)
            })?;
            selected.push(found);
        }
        selected
    } else {
        harness::ALL_HARNESSES
            .iter()
            .filter(|item| {
                item.paths
                    .iter()
                    .map(|spec| match spec.base {
                        harness::PathBase::Home => home.join(spec.relative),
                        harness::PathBase::Env(name) => std::env::var_os(name)
                            .map(|root| std::path::PathBuf::from(root).join(spec.relative))
                            .unwrap_or_default(),
                    })
                    .any(|path| harness::detected(std::slice::from_ref(&path)))
            })
            .collect::<Vec<_>>()
    };

    if selected.is_empty() {
        return Err(
            CliError::new("NO_HARNESS", "no supported agent harness was detected")
                .hint("use --all or --harness <id>")
                .exit(2),
        );
    }

    let mut targets = Vec::new();
    for item in &selected {
        for path in harness::resolve_paths(item, &home) {
            targets.push((item.id, path.join("cowbox")));
        }
    }
    let paths = targets
        .iter()
        .map(|(_, path)| path.clone())
        .collect::<Vec<_>>();
    if let Some(mode) = args.mode {
        if !args.dry_run {
            agent_config::set_global_mode(mode)?;
        }
    }
    let results = skill::install_paths(&paths, args.force, args.dry_run)?;
    if args.json {
        let items = results
            .iter()
            .map(|result| {
                json!({
                    "path": result.path,
                    "status": result.status,
                })
            })
            .collect::<Vec<_>>();
        json::print(&json::ok(
            "install-skill",
            None,
            json!({
                "mode": args.mode,
                "dry_run": args.dry_run,
                "installed": items,
            }),
        ))?;
    } else {
        for result in results {
            println!("{}: {}", result.path.display(), result.status);
        }
        if let Some(mode) = args.mode {
            println!("global trigger mode set to {mode:?}");
        }
    }
    Ok(0)
}

fn list_harnesses(home: &std::path::Path, json_mode: bool) -> CliResult<i32> {
    let mut items = Vec::new();
    for item in harness::ALL_HARNESSES {
        let paths = harness::resolve_paths(item, home);
        let is_detected = harness::detected(&paths);
        items.push((item, paths, is_detected));
    }
    if json_mode {
        let entries = items
            .iter()
            .map(|(item, paths, is_detected)| {
                json!({
                    "id": item.id,
                    "display": item.display,
                    "paths": paths.iter().map(|path| path.display().to_string()).collect::<Vec<_>>(),
                    "detected": is_detected,
                })
            })
            .collect::<Vec<_>>();
        json::print(&json::ok(
            "install-skill",
            None,
            json!({ "harnesses": entries }),
        ))?;
    } else {
        println!("Agent harnesses supported by `cowbox install-skill`:");
        for (index, (item, paths, is_detected)) in items.iter().enumerate() {
            let marker = if *is_detected { "detected" } else { "—" };
            println!("  {}. [{marker}] {}", index + 1, item.display);
            for path in paths {
                println!("       {}", path.display());
            }
        }
    }
    Ok(0)
}

fn command_install_rules(args: &InstallRulesArgs) -> CliResult<i32> {
    let project = args.project.clone().unwrap_or(std::env::current_dir()?);
    if !project.is_dir() {
        return Err(CliError::new(
            "PROJECT_NOT_FOUND",
            format!("{} is not a directory", project.display()),
        )
        .exit(2));
    }
    let results = rules::install(&project, args.remove, args.dry_run)?;
    if args.json {
        let items = results
            .iter()
            .map(|result| {
                json!({
                    "path": result.path,
                    "action": result.action,
                })
            })
            .collect::<Vec<_>>();
        json::print(&json::ok(
            "install-rules",
            None,
            json!({ "dry_run": args.dry_run, "files": items }),
        ))?;
    } else {
        for result in results {
            println!("{}: {}", result.path.display(), result.action);
        }
    }
    Ok(0)
}

fn command_policy(args: &PolicyArgs) -> CliResult<i32> {
    let project = std::env::current_dir()?;
    let effective = policy::effective(&project)?;
    if args.json {
        json::print(&json::ok("policy", None, serde_json::to_value(&effective)?))?;
    } else {
        println!("mode: {:?}", effective.mode);
        println!("require_confirmation: {}", effective.require_confirmation);
        println!(
            "max_files_without_prompt: {}",
            effective.max_files_without_prompt
        );
        println!("triggers: {}", effective.triggers.join(", "));
        println!("source: {}", effective.source);
    }
    Ok(0)
}

fn command_update(args: &UpdateArgs) -> CliResult<i32> {
    if !args.check {
        return Err(CliError::new(
            "USAGE",
            "this version supports `cowbox update --check` only",
        )
        .exit(2));
    }
    setup::update_check(args.json)
}

fn command_path(args: &PathArgs) -> CliResult<i32> {
    #[cfg(windows)]
    {
        crate::windows_bridge::path(args)
    }
    #[cfg(not(windows))]
    {
        let mapped = setup::windows_path_to_wsl(&args.path)?;
        if args.json {
            json::print(&json::ok(
                "path",
                None,
                json!({ "input": args.path, "wsl": mapped }),
            ))?;
        } else {
            println!("{}", mapped.display());
        }
        Ok(0)
    }
}

fn command_hidden(args: &HiddenArgs, sandbox_exec: bool) -> CliResult<i32> {
    let _ = &args.spec;
    #[cfg(target_os = "linux")]
    {
        if sandbox_exec {
            cowbox_core::linux::sandbox_exec_main()?;
        } else {
            cowbox_core::linux::supervisor_main()?;
        }
        Ok(0)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = sandbox_exec;
        Err(CliError::new(
            "UNSUPPORTED_PLATFORM",
            "cowbox M1 helpers require Linux/WSL2",
        )
        .exit(125))
    }
}

fn load_or_create(
    ctx: &Context,
    backend: &dyn SandboxBackend,
    requested: Option<&str>,
) -> CliResult<(SessionRecord, bool)> {
    if let Some(session_id) = requested {
        let record = ctx.store.load(session_id)?;
        if record.status != SessionStatus::Active {
            return Err(CliError::new(
                "SESSION_NOT_ACTIVE",
                format!("session {} is {:?}", session_id, record.status),
            ));
        }
        if let Some(active) = ctx.store.load_active()? {
            if active.session_id != session_id {
                return Err(CliError::new(
                    "SESSION_CONFLICT",
                    format!(
                        "workspace already has active session {}; finish or discard it first",
                        active.session_id
                    ),
                )
                .exit(2));
            }
        } else {
            ctx.store.write_active(&record)?;
        }
        ensure_no_pending_commit(ctx, &record)?;
        return Ok((record, false));
    }

    if let Some(record) = ctx.store.load_active()? {
        ensure_no_pending_commit(ctx, &record)?;
        return Ok((record, false));
    }

    let session_id = workspace::new_session_id();
    let layout = SessionLayout::new(
        session_id.clone(),
        ctx.workspace.clone(),
        ctx.state_root.clone(),
    );
    if let Err(error) = backend.init_session(&layout, &ctx.workspace) {
        let _ = ctx.store.remove_session(&session_id);
        return Err(error.into());
    }
    let record = ctx.store.new_record(session_id);
    ctx.store.save(&record)?;
    ctx.store.write_active(&record)?;
    ctx.store.append_event(
        &record.session_id,
        &json!({"event":"session_created","ts":workspace::now_ms()}),
    )?;
    Ok((record, true))
}

fn active_record(ctx: &Context) -> CliResult<SessionRecord> {
    ctx.store.load_active()?.ok_or_else(|| {
        CliError::new(
            "NO_ACTIVE_SESSION",
            "no active cowbox session for this workspace",
        )
        .hint("run `cowbox run -- <cmd>` first")
        .exit(1)
    })
}

fn target_chain(record: &SessionRecord, target: u64) -> CliResult<Vec<u64>> {
    let mut chain = record.dag.path_to_base(target)?;
    chain.reverse();
    chain.retain(|step| *step != BASE_STEP);
    Ok(chain)
}

fn command_label(command: &[OsString]) -> String {
    command
        .iter()
        .map(|item| quote_argument(&item.to_string_lossy()))
        .collect::<Vec<_>>()
        .join(" ")
}

fn quote_argument(value: &str) -> String {
    let safe = !value.is_empty()
        && value
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || "-_./=:".contains(character));
    if safe {
        value.to_string()
    } else {
        format!("{value:?}")
    }
}

fn truncate(value: &str, max_chars: usize) -> String {
    if value.chars().count() <= max_chars {
        return value.to_string();
    }
    let mut truncated = value
        .chars()
        .take(max_chars.saturating_sub(3))
        .collect::<String>();
    truncated.push_str("...");
    truncated
}
