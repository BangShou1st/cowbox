use std::fmt;

pub type CliResult<T> = Result<T, CliError>;

#[derive(Debug)]
pub struct CliError {
    pub code: String,
    pub message: String,
    pub hint: Option<String>,
    pub exit_code: i32,
}

impl CliError {
    pub fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
            hint: None,
            exit_code: 1,
        }
    }

    pub fn hint(mut self, hint: impl Into<String>) -> Self {
        self.hint = Some(hint.into());
        self
    }

    pub fn exit(mut self, exit_code: i32) -> Self {
        self.exit_code = exit_code;
        self
    }

    pub fn print_human(&self) {
        eprintln!("cowbox: {}", self.message);
        if let Some(hint) = &self.hint {
            eprintln!("hint: {hint}");
        }
    }
}

impl fmt::Display for CliError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}", self.message)
    }
}

impl std::error::Error for CliError {}

impl From<cowbox_core::CoreError> for CliError {
    fn from(error: cowbox_core::CoreError) -> Self {
        use cowbox_core::CoreError;
        match error {
            CoreError::CowUnsupported { path, reason } => CliError::new(
                "COW_UNSUPPORTED",
                format!(
                    "copy-on-write is unavailable at {}: {reason}",
                    path.display()
                ),
            )
            .hint("cowbox M1 needs Linux/WSL2 with unprivileged OverlayFS and userxattr")
            .exit(125),
            CoreError::IsolationFailed(reason) => {
                CliError::new("ISOLATION_FAILED", reason).exit(125)
            }
            CoreError::Landlock(reason) => CliError::new("LANDLOCK_FAILED", reason).exit(125),
            CoreError::StateCorrupted { step, reason } => CliError::new(
                "STATE_CORRUPTED",
                format!("session state is corrupted at step {step}: {reason}"),
            ),
            CoreError::CommitConflict(paths) => CliError::new(
                "COMMIT_CONFLICT",
                format!("host workspace changed since the session started: {paths:?}"),
            )
            .hint("inspect the listed paths, then retry with --force if they should be overwritten")
            .exit(2),
            CoreError::CommitFailed(reason) => CliError::new("COMMIT_FAILED", reason),
            CoreError::Timeout(duration) => {
                CliError::new("TIMEOUT", format!("command timed out after {duration:?}")).exit(124)
            }
            CoreError::Config(reason) => CliError::new("CONFIG_ERROR", reason).exit(2),
            CoreError::Io(error) => CliError::new("IO_ERROR", error.to_string()),
            CoreError::Json(error) => CliError::new("JSON_ERROR", error.to_string()),
        }
    }
}

impl From<cowbox_dag::DagError> for CliError {
    fn from(error: cowbox_dag::DagError) -> Self {
        CliError::new("DAG_ERROR", error.to_string())
    }
}

impl From<std::io::Error> for CliError {
    fn from(error: std::io::Error) -> Self {
        CliError::new("IO_ERROR", error.to_string())
    }
}

impl From<serde_json::Error> for CliError {
    fn from(error: serde_json::Error) -> Self {
        CliError::new("JSON_ERROR", error.to_string())
    }
}

impl From<toml::de::Error> for CliError {
    fn from(error: toml::de::Error) -> Self {
        CliError::new("CONFIG_ERROR", error.to_string()).exit(2)
    }
}
