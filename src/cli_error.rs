//! Stable command failure classes; storage and core errors remain independent.
use crate::event::JsonValue;
use crate::store::StoreError;
use git2::Oid;

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
    /// Command-specific diagnostics, serialized in order as the additive
    /// `data` object; empty means the field is absent.
    pub data: Vec<(&'static str, crate::event::JsonValue)>,
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
            data: Vec::new(),
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
    /// The same failure naming `input` as the offending argument.
    pub(crate) fn with_input(mut self, input: impl Into<String>) -> Self {
        self.input = input.into();
        self
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
        if !self.data.is_empty() {
            let fields = self
                .data
                .iter()
                .map(|(key, value)| format!("{}:{}", escape(key), crate::event::json_value(value)))
                .collect::<Vec<_>>();
            out.push_str(",\"data\":{");
            out.push_str(&fields.join(","));
            out.push('}');
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

/// Command boundary that owns the conflict classification and wording.
#[derive(Clone, Copy)]
pub(crate) enum ConflictOperation {
    Prepare,
    Merge,
}
/// Most conflicted paths the human message lists; JSON `data.paths` is complete.
const MESSAGE_PATH_LIMIT: usize = 20;

/// One strategy execution that left unmerged index entries.
pub(crate) struct StrategyConflict<'a> {
    pub(crate) operation: ConflictOperation,
    pub(crate) id: u64,
    pub(crate) strategy: &'a str,
    pub(crate) base_ref: &'a str,
    pub(crate) source_ref: &'a str,
    pub(crate) base: Oid,
    pub(crate) source: Oid,
    pub(crate) paths: &'a [String],
    pub(crate) cleanup: &'a str,
}

/// Report a strategy conflict with the operation's code and cleanup outcome.
pub(crate) fn strategy_conflict_error(conflict: &StrategyConflict<'_>) -> CliError {
    let StrategyConflict {
        operation,
        id,
        strategy,
        base_ref,
        source_ref,
        base,
        source,
        paths,
        cleanup,
    } = *conflict;
    let (code, action) = match operation {
        ConflictOperation::Prepare => ("E_PREPARE_CONFLICT", "prepared"),
        ConflictOperation::Merge => ("E_MERGE_CONFLICT", "merged"),
    };
    // Paths and branch names come from repository content; escape control
    // characters in the human message. Git quoting leaves C1 controls
    // (U+0080-U+009F) in paths unescaped; sanitizing catches them.
    let base_ref = crate::cli::sanitize_terminal(base_ref);
    let source_ref = crate::cli::sanitize_terminal(source_ref);
    let count = match paths.len() {
        1 => "1 path".to_string(),
        n => format!("{n} paths"),
    };
    let mut shown = paths
        .iter()
        .take(MESSAGE_PATH_LIMIT)
        .map(|path| crate::cli::sanitize_terminal(path))
        .collect::<Vec<_>>()
        .join(", ");
    if paths.len() > MESSAGE_PATH_LIMIT {
        shown.push_str(&format!(" (and {} more)", paths.len() - MESSAGE_PATH_LIMIT));
    }
    let mut error = CliError::new(
        code,
        format!(
            "PR #{id} cannot be {action} with strategy {strategy}: source {source} conflicts \
             with base '{base_ref}' at {base} in {count}: {shown}; merge '{base_ref}' into the \
             source branch '{source_ref}', resolve the conflicts, then run `git forge pr update \
             {id}` or create a replacement PR; {cleanup}"
        ),
        id.to_string(),
        format!(
            "Merge the base branch into the source branch, resolve the conflicts, then run \
             `git forge pr update {id}` or create a replacement PR."
        ),
        false,
    );
    error.data = vec![
        ("pr", JsonValue::Number(id as i64)),
        ("strategy", JsonValue::String(strategy.to_string())),
        ("base", JsonValue::String(base.to_string())),
        ("source", JsonValue::String(source.to_string())),
        ("paths", crate::event::json_string_array(paths)),
    ];
    error
}
#[cfg(test)]
mod tests {
    use super::{strategy_conflict_error, ConflictOperation, StrategyConflict};
    use git2::Oid;

    fn conflict_json(paths: &[String]) -> (String, String) {
        let error = strategy_conflict_error(&StrategyConflict {
            operation: ConflictOperation::Prepare,
            id: 7,
            strategy: "squash",
            base_ref: "main",
            source_ref: "ticket",
            base: Oid::from_str("1111111111111111111111111111111111111111").unwrap(),
            source: Oid::from_str("2222222222222222222222222222222222222222").unwrap(),
            paths,
            cleanup: "worktree cleaned up, no ref changes made",
        });
        (error.message.clone(), error.to_json())
    }

    #[test]
    fn prepare_conflict_message_caps_paths_and_data_keeps_all() {
        let paths: Vec<String> = (0..21).map(|i| format!("p{i:02}")).collect();
        let (message, json) = conflict_json(&paths);
        let listed = paths[..20].join(", ");
        assert_eq!(
            message,
            format!(
                "PR #7 cannot be prepared with strategy squash: source \
                 2222222222222222222222222222222222222222 conflicts with base 'main' at \
                 1111111111111111111111111111111111111111 in 21 paths: {listed} (and 1 more); \
                 merge 'main' into the source branch 'ticket', resolve the conflicts, then run \
                 `git forge pr update 7` or create a replacement PR; worktree cleaned up, no ref \
                 changes made"
            )
        );
        let quoted: Vec<String> = paths.iter().map(|p| format!("\"{p}\"")).collect();
        assert!(
            json.ends_with(&format!(
                ",\"data\":{{\"pr\":7,\"strategy\":\"squash\",\"base\":\
                 \"1111111111111111111111111111111111111111\",\"source\":\
                 \"2222222222222222222222222222222222222222\",\"paths\":[{}]}}}}}}",
                quoted.join(",")
            )),
            "{json}"
        );
        assert!(json.starts_with("{\"error\":{\"code\":\"E_PREPARE_CONFLICT\","));
    }

    #[test]
    fn prepare_conflict_message_lists_all_paths_up_to_the_cap() {
        let paths: Vec<String> = (0..20).map(|i| format!("p{i:02}")).collect();
        let (message, _) = conflict_json(&paths);
        assert!(message.contains(&format!("in 20 paths: {};", paths.join(", "))));
        let (message, _) = conflict_json(&["only.txt".to_string()]);
        assert!(message.contains("in 1 path: only.txt;"), "{message}");
    }
}
