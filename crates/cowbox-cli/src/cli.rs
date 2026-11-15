use clap::{Args, Parser, Subcommand};
use std::ffi::OsString;
use std::path::PathBuf;

#[derive(Debug, Parser)]
#[command(
    name = "cowbox",
    version,
    about = "Millisecond Copy-on-Write sandboxes for AI coding agents",
    disable_help_subcommand = true
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: CliCommand,
}

#[derive(Debug, Subcommand)]
pub enum CliCommand {
    /// Write a .cowbox.toml configuration template.
    Init(InitArgs),
    /// Run a command inside a new CoW step.
    Run(RunArgs),
    /// Show the net changes from the base step.
    Diff(DiffArgs),
    /// Show the session DAG.
    Log(LogArgs),
    /// Move the session head to a historical step.
    Rollback(RollbackArgs),
    /// Move the session head back N steps.
    Undo(UndoArgs),
    /// Apply the net changes to the host workspace.
    Commit(CommitArgs),
    /// Discard the active session.
    Discard(DiscardArgs),
    /// Check kernel and sandbox capabilities.
    Doctor(DoctorArgs),
    /// Show session telemetry.
    Stats(StatsArgs),

    #[command(name = "__sandbox-exec", hide = true)]
    SandboxExec(HiddenArgs),
    #[command(name = "__supervisor", hide = true)]
    Supervisor(HiddenArgs),
}

#[derive(Debug, Args)]
pub struct InitArgs {
    #[arg(long)]
    pub force: bool,
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub struct RunArgs {
    #[arg(long)]
    pub session: Option<String>,
    #[arg(long)]
    pub timeout: Option<String>,
    #[arg(long = "allow-read")]
    pub allow_read: Vec<PathBuf>,
    #[arg(long = "allow-write")]
    pub allow_write: Vec<PathBuf>,
    #[arg(long)]
    pub env: Vec<String>,
    #[arg(long)]
    pub clean_env: bool,
    #[arg(long)]
    pub json: bool,
    #[arg(long = "trace-chrome")]
    pub trace_chrome: Option<PathBuf>,
    #[arg(last = true, required = true, num_args = 1.., allow_hyphen_values = true)]
    pub cmd: Vec<OsString>,
}

#[derive(Debug, Args)]
pub struct DiffArgs {
    #[arg(long, default_value_t = 0)]
    pub base: u64,
    #[arg(long)]
    pub step: Option<u64>,
    #[arg(long)]
    pub stat: bool,
    #[arg(long = "name-only")]
    pub name_only: bool,
    #[arg(long)]
    pub all: bool,
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub struct LogArgs {
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub struct RollbackArgs {
    pub step: u64,
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub struct UndoArgs {
    #[arg(default_value_t = 1)]
    pub steps: usize,
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub struct CommitArgs {
    #[arg(long)]
    pub step: Option<u64>,
    #[arg(long)]
    pub dry_run: bool,
    #[arg(long)]
    pub force: bool,
    #[arg(long = "include-excluded")]
    pub include_excluded: bool,
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub struct DiscardArgs {
    #[arg(long)]
    pub yes: bool,
    #[arg(long)]
    pub purge: bool,
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub struct DoctorArgs {
    #[arg(long)]
    pub json: bool,
    #[arg(long = "trace-chrome")]
    pub trace_chrome: Option<PathBuf>,
}

#[derive(Debug, Args)]
pub struct StatsArgs {
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub struct HiddenArgs {
    #[arg(long)]
    pub spec: PathBuf,
}

impl CliCommand {
    pub fn name(&self) -> &'static str {
        match self {
            CliCommand::Init(_) => "init",
            CliCommand::Run(_) => "run",
            CliCommand::Diff(_) => "diff",
            CliCommand::Log(_) => "log",
            CliCommand::Rollback(_) => "rollback",
            CliCommand::Undo(_) => "undo",
            CliCommand::Commit(_) => "commit",
            CliCommand::Discard(_) => "discard",
            CliCommand::Doctor(_) => "doctor",
            CliCommand::Stats(_) => "stats",
            CliCommand::SandboxExec(_) => "__sandbox-exec",
            CliCommand::Supervisor(_) => "__supervisor",
        }
    }

    pub fn json(&self) -> bool {
        match self {
            CliCommand::Init(args) => args.json,
            CliCommand::Run(args) => args.json,
            CliCommand::Diff(args) => args.json,
            CliCommand::Log(args) => args.json,
            CliCommand::Rollback(args) => args.json,
            CliCommand::Undo(args) => args.json,
            CliCommand::Commit(args) => args.json,
            CliCommand::Discard(args) => args.json,
            CliCommand::Doctor(args) => args.json,
            CliCommand::Stats(args) => args.json,
            _ => false,
        }
    }

    pub fn trace_chrome(&self) -> Option<&PathBuf> {
        match self {
            CliCommand::Run(args) => args.trace_chrome.as_ref(),
            CliCommand::Doctor(args) => args.trace_chrome.as_ref(),
            _ => None,
        }
    }
}
