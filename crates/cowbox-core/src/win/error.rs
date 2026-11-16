use std::fmt;

pub type WinResult<T> = Result<T, WinError>;

#[derive(Debug, Clone)]
pub struct WinError {
    pub code: &'static str,
    pub message: String,
    pub hint: Option<String>,
    pub exit_code: i32,
}

impl WinError {
    pub fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
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

    pub fn io(code: &'static str, context: impl fmt::Display, error: std::io::Error) -> Self {
        Self::new(code, format!("{context}: {error}"))
    }

    pub fn is_fault_abort(&self) -> bool {
        self.code == "WIN_FAULT_ABORT"
    }

    pub fn fault_abort(point: &str, journal: &std::path::Path) -> Self {
        Self::new(
            "WIN_FAULT_ABORT",
            format!(
                "injected fault abort at {point}; recovery journal is {}",
                super::paths::display(journal)
            ),
        )
        .hint("run `cowbox win commit --recover --session <id>` to reconcile the host")
        .exit(70)
    }
}

impl fmt::Display for WinError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}", self.message)
    }
}

impl std::error::Error for WinError {}

impl From<cowbox_dag::DagError> for WinError {
    fn from(error: cowbox_dag::DagError) -> Self {
        Self::new("WIN_DAG", error.to_string())
    }
}

impl From<std::io::Error> for WinError {
    fn from(error: std::io::Error) -> Self {
        Self::new("WIN_IO", error.to_string())
    }
}

impl From<serde_json::Error> for WinError {
    fn from(error: serde_json::Error) -> Self {
        Self::new("WIN_JSON", error.to_string())
    }
}
