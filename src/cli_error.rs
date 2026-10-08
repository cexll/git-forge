//! Stable command failure classes; storage and core errors remain independent.
use crate::store::StoreError;

/// A CLI failure. `Display` preserves the human stderr message.
#[derive(Debug)]
pub struct CliError {
    /// Stable machine-readable class, independent of message text.
    pub code: &'static str,
    /// Human-readable failure message.
    pub message: String,
    /// Offending argument, or empty when the failure has no single input.
    pub input: String,
    /// Supported next action.
    pub hint: String,
    /// Whether retrying after a fresh read can succeed.
    pub retryable: bool,
    /// Optional command-specific diagnostics, serialized as the additive `data` field.
    pub data: Option<Box<crate::event::JsonValue>>,
}

impl CliError {
    /// Construct a classified failure without changing its message.
    pub fn new(
        code: &'static str,
        message: impl Into<String>,
        input: impl Into<String>,
        hint: impl Into<String>,
        retryable: bool,
    ) -> Self {
        Self {
            code,
            message: message.into(),
            input: input.into(),
            hint: hint.into(),
            retryable,
            data: None,
        }
    }
    pub(crate) fn usage(message: impl Into<String>, input: impl Into<String>) -> Self {
        Self::new(
            "E_USAGE",
            message,
            input,
            "Run the command with --help and correct its arguments.",
            false,
        )
    }
    pub(crate) fn not_found(message: impl Into<String>, input: impl Into<String>) -> Self {
        Self::new(
            "E_NOT_FOUND",
            message,
            input,
            "List the existing entities or branches and use an existing identifier.",
            false,
        )
    }
    pub(crate) fn invalid(message: impl Into<String>) -> Self {
        Self::new(
            "E_INVALID",
            message,
            "",
            "Correct the input or repository state before retrying.",
            false,
        )
    }
    /// Render one JSON error document with the shared wire escaping.
    pub fn to_json(&self) -> String {
        let escape = crate::event::json_escape_for_machine;
        let mut out = format!(
            "{{\"error\":{{\"code\":{},\"message\":{},\"input\":{},\"hint\":{},\"retryable\":{}",
            escape(self.code),
            escape(&self.message),
            escape(&self.input),
            escape(&self.hint),
            self.retryable
        );
        if let Some(data) = &self.data {
            out.push_str(",\"data\":");
            out.push_str(&crate::event::json_value(data));
        }
        out.push_str("}}");
        out
    }
}
impl std::fmt::Display for CliError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}
impl std::error::Error for CliError {}
impl From<String> for CliError {
    fn from(message: String) -> Self {
        Self::new(
            "E_INTERNAL",
            message,
            "",
            "Inspect the repository and the reported failure before retrying.",
            false,
        )
    }
}
impl From<&str> for CliError {
    fn from(message: &str) -> Self {
        message.to_owned().into()
    }
}
impl From<StoreError> for CliError {
    fn from(error: StoreError) -> Self {
        let (code, retryable, hint) = match &error {
            StoreError::CasConflict | StoreError::Exhausted => (
                "E_CONFLICT",
                true,
                "Read the current state and retry the command.",
            ),
            StoreError::MissingRef => ("E_NOT_FOUND", false, "Check the referenced entity exists."),
            StoreError::InvalidState(_)
            | StoreError::RefExists(_)
            | StoreError::NotAGitRepository => (
                "E_INVALID",
                false,
                "Correct the repository state before retrying.",
            ),
            _ => (
                "E_INTERNAL",
                false,
                "Inspect the repository and the reported failure before retrying.",
            ),
        };
        Self::new(code, error.to_string(), "", hint, retryable)
    }
}
