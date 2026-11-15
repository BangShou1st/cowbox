use crate::agent::{config as agent_config, harness, policy, rules, skill};
use crate::cli::{
    CliCommand, CommitArgs, DiffArgs, DiscardArgs, DoctorArgs, HiddenArgs, InitArgs,
    InstallRulesArgs, InstallSkillArgs, LogArgs, PathArgs, PolicyArgs, RollbackArgs, RunArgs,
    SetupArgs, StatsArgs, UndoArgs, UpdateArgs,
};
use crate::config::{Config, RunPolicyOverrides};
use crate::error::{CliError, CliResult};
use crate::json;
use crate::render::{self, Stats};
use crate::setup;
use crate::state::{SessionRecord, SessionStatus, SessionStore};
use crate::workspace;
use cowbox_core::{SandboxBackend, SessionLayout};
use cowbox_dag::{BASE_STEP, StepNode};
use serde_json::json;
use std::ffi::OsString;
use std::io::{BufRead, IsTerminal};
use std::path::PathBuf;

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
        CliCommand::Discard(args) => command_discard(args),
        CliCommand::Doctor(args) => command_doctor(args),
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
    let mut record = active_record(&ctx)?;
    let target = args.step.unwrap_or(record.dag.head_step);
    if !record.dag.nodes.contains_key(&target) {
        return Err(CliError::new(
            "DAG_ERROR",
            format!("step {target} does not exist"),
        ));
    }
    let chain = target_chain(&record, target)?;
    let options = ctx
        .config
        .to_commit_options(args.force, args.dry_run, args.include_excluded);
    let layout = ctx.store.layout(&record.session_id);
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

    if args.dry_run {
        let summary = render::summarize(&report.changes);
        if args.json {
            json::print(&json::ok(
                "commit",
                Some(&record),
                json!({
                    "dry_run": true,
                    "committed": false,
                    "changes": report.changes,
                    "conflicts": report.conflicts,
                    "summary": render::summary_json(&summary),
                }),
            ))?;
        } else {
            println!(
                "dry-run: {} change(s), {} conflict(s); nothing written",
                report.changes.len(),
                report.conflicts.len()
            );
        }
        return Ok(0);
    }

    if report.committed {
        record.status = SessionStatus::Committed;
        record.last_commit_unix_ms = Some(workspace::now_ms());
        record.updated_at_unix_ms = workspace::now_ms();
        ctx.store.save(&record)?;
        ctx.store.clear_active()?;
        ctx.store.append_event(
            &record.session_id,
            &json!({"event":"commit","ts":workspace::now_ms(),"step":target}),
        )?;
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
        let ctx = setup()?;
        let capabilities = backend().probe(&ctx.state_root)?;
        let ready = capabilities.user_namespace
            && capabilities.overlayfs
            && capabilities.userxattr
            && capabilities.landlock_abi.is_some();
        if args.json {
            json::print(&json::ok(
                "doctor",
                None,
                json!({ "ready": ready, "capabilities": capabilities }),
            ))?;
        } else {
            render::print_doctor(&capabilities, &ctx.state_root, ready);
        }
        Ok(if ready { 0 } else { 1 })
    }
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
        return Ok((record, false));
    }

    if let Some(record) = ctx.store.load_active()? {
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
