use crate::config::AgentMode;
use clap::ValueEnum;
use clap::{Args, Parser, Subcommand};
use cowbox_core::NetworkMode;
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
    /// Run a self-contained copy-on-write demo without touching the current project.
    Demo(DemoArgs),
    /// Discard the active session.
    Discard(DiscardArgs),
    /// Check kernel and sandbox capabilities.
    Doctor(DoctorArgs),
    /// List sessions across all workspaces.
    Sessions(SessionsArgs),
    /// Remove finalized session state directories.
    Gc(GcArgs),
    /// Show session telemetry.
    Stats(StatsArgs),
    /// Install the Windows launcher and the WSL engine (Windows only).
    Setup(SetupArgs),
    /// Install the cowbox skill into local agent harnesses.
    InstallSkill(InstallSkillArgs),
    /// Add or remove project-level cowbox rules.
    InstallRules(InstallRulesArgs),
    /// Print the effective agent trigger policy.
    Policy(PolicyArgs),
    /// Check for a newer cowbox release.
    Update(UpdateArgs),
    /// Convert a Windows path to its WSL path.
    Path(PathArgs),

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
    pub agent: bool,
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
    #[arg(long, value_enum)]
    pub network: Option<NetworkModeArg>,
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
    #[arg(
        long,
        help = "Recover the latest interrupted commit from its journal",
        conflicts_with_all = ["step", "dry_run", "force", "include_excluded"]
    )]
    pub recover: bool,
    #[arg(
        long,
        help = "Skip the interactive prompt; use only after explicit human approval"
    )]
    pub yes: bool,
    #[arg(long = "include-excluded")]
    pub include_excluded: bool,
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub struct DemoArgs {
    #[arg(long)]
    pub json: bool,
    #[arg(long, help = "Run the destructive demo step with network isolation")]
    pub offline: bool,
    #[arg(long, help = "Keep the fixture and isolated state for inspection")]
    pub keep: bool,
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
    #[arg(long = "check-run")]
    pub check_run: bool,
    #[arg(long, value_enum, requires = "check_run")]
    pub network: Option<NetworkModeArg>,
    #[arg(long = "trace-chrome")]
    pub trace_chrome: Option<PathBuf>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum NetworkModeArg {
    Host,
    Off,
}

impl From<NetworkModeArg> for NetworkMode {
    fn from(value: NetworkModeArg) -> Self {
        match value {
            NetworkModeArg::Host => Self::Host,
            NetworkModeArg::Off => Self::Off,
        }
    }
}

#[derive(Debug, Args)]
pub struct SessionsArgs {
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub struct GcArgs {
    #[arg(
        long = "older-than",
        conflicts_with = "all",
        help = "Only remove finalized sessions older than this duration"
    )]
    pub older_than: Option<String>,
    #[arg(
        long,
        conflicts_with = "older_than",
        help = "Remove every finalized session regardless of age"
    )]
    pub all: bool,
    #[arg(long = "dry-run", help = "Show candidates without removing anything")]
    pub dry_run: bool,
    #[arg(long, help = "Confirm removal without an interactive prompt")]
    pub yes: bool,
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub struct StatsArgs {
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub struct SetupArgs {
    #[arg(long)]
    pub distro: Option<String>,
    #[arg(long)]
    pub version: Option<String>,
    #[arg(long)]
    pub force: bool,
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub struct InstallSkillArgs {
    #[arg(long)]
    pub list: bool,
    #[arg(long)]
    pub all: bool,
    #[arg(long = "harness")]
    pub harness: Vec<String>,
    #[arg(long, value_enum)]
    pub mode: Option<AgentMode>,
    #[arg(long)]
    pub force: bool,
    #[arg(long = "dry-run")]
    pub dry_run: bool,
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub struct InstallRulesArgs {
    #[arg(long)]
    pub project: Option<PathBuf>,
    #[arg(long)]
    pub remove: bool,
    #[arg(long = "dry-run")]
    pub dry_run: bool,
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub struct PolicyArgs {
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub struct UpdateArgs {
    #[arg(long)]
    pub check: bool,
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub struct PathArgs {
    pub path: PathBuf,
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
            CliCommand::Demo(_) => "demo",
            CliCommand::Discard(_) => "discard",
            CliCommand::Doctor(_) => "doctor",
            CliCommand::Sessions(_) => "sessions",
            CliCommand::Gc(_) => "gc",
            CliCommand::Stats(_) => "stats",
            CliCommand::Setup(_) => "setup",
            CliCommand::InstallSkill(_) => "install-skill",
            CliCommand::InstallRules(_) => "install-rules",
            CliCommand::Policy(_) => "policy",
            CliCommand::Update(_) => "update",
            CliCommand::Path(_) => "path",
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
            CliCommand::Demo(args) => args.json,
            CliCommand::Discard(args) => args.json,
            CliCommand::Doctor(args) => args.json,
            CliCommand::Sessions(args) => args.json,
            CliCommand::Gc(args) => args.json,
            CliCommand::Stats(args) => args.json,
            CliCommand::Setup(args) => args.json,
            CliCommand::InstallSkill(args) => args.json,
            CliCommand::InstallRules(args) => args.json,
            CliCommand::Policy(args) => args.json,
            CliCommand::Update(args) => args.json,
            CliCommand::Path(args) => args.json,
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
