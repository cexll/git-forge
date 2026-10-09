//! A token consumed as an option value is data, also when it is the literal
//! `--json`. Only a standalone `--json` (not an option value, not after an
//! end-of-options `--`) requests JSON output. The table covers every
//! value-taking option of every `--json`-capable command, once without and
//! once with a separate standalone `--json`.
use std::path::PathBuf;
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT_REPO: AtomicU64 = AtomicU64::new(0);

struct Repo(PathBuf);
impl Repo {
    /// `main`, `feature` one commit ahead, Issue #1, and PR #2 from
    /// `feature` into `main`.
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "gf-json-values-{}-{}",
            std::process::id(),
            NEXT_REPO.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&path).unwrap();
        let repo = Self(path);
        repo.git(&["init", "-q"]);
        repo.git(&["config", "user.name", "Test"]);
        repo.git(&["config", "user.email", "test@example.com"]);
        repo.commit_file("base.txt");
        repo.git(&["branch", "-M", "main"]);
        repo.git(&["checkout", "-q", "-b", "feature"]);
        repo.commit_file("feature.txt");
        repo.git(&["checkout", "-q", "main"]);
        repo.ok(&["issue", "new", "one"]);
        repo.ok(&["pr", "create", "--source", "feature", "--base", "main", "p"]);
        repo
    }
    fn commit_file(&self, name: &str) {
        std::fs::write(self.0.join(name), name).unwrap();
        self.git(&["add", name]);
        self.git(&["commit", "-q", "-m", name]);
    }
    fn git(&self, args: &[&str]) -> String {
        let output = Command::new("git")
            .args(args)
            .current_dir(&self.0)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .output()
            .unwrap();
        assert!(output.status.success(), "git {args:?}");
        String::from_utf8(output.stdout).unwrap().trim().to_string()
    }
    fn run(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_git-forge"))
            .args(args)
            .current_dir(&self.0)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env_remove("GIT_FORGE_ROLE")
            .output()
            .unwrap()
    }
    fn ok(&self, args: &[&str]) -> String {
        let output = self.run(args);
        assert!(output.status.success(), "{args:?}: {}", stderr(&output));
        String::from_utf8(output.stdout).unwrap()
    }
    fn show(&self, args: &[&str]) -> serde_json::Value {
        serde_json::from_str(&self.ok(&[args, &["--json"]].concat())).unwrap()
    }
    /// The event JSON at the tip of `reference`.
    fn tip_event(&self, reference: &str) -> serde_json::Value {
        serde_json::from_str(&self.git(&["show", &format!("{reference}:.forge/event.json")]))
            .unwrap()
    }
}
impl Drop for Repo {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn stderr(output: &Output) -> String {
    String::from_utf8(output.stderr.clone()).unwrap()
}

/// What keeping `--json` as the option value looks like.
enum Kept {
    /// The command succeeds and stores the value; the check reads it back.
    Stored(fn(&Repo)),
    /// The command fails on the value itself: stderr is exactly this line.
    Rejected(&'static str),
}

fn record_ref(command: &str) -> String {
    let hex: String = "--json".bytes().map(|b| format!("{b:02x}")).collect();
    format!("refs/forge/idempotency/{command}/{hex}")
}

fn has_ref(repo: &Repo, name: &str) {
    assert_eq!(
        repo.git(&["for-each-ref", "--format=%(refname)", name]),
        name
    );
}

fn rows() -> Vec<(&'static str, Vec<&'static str>, Kept)> {
    use Kept::{Rejected, Stored};
    vec![
        (
            "issue new --label",
            vec!["issue", "new", "t", "--label", "--json"],
            Stored(|r| assert_eq!(r.show(&["issue", "show", "3"])["labels"], serde_json::json!(["--json"]))),
        ),
        (
            "issue new -l",
            vec!["issue", "new", "t", "-l", "--json"],
            Stored(|r| assert_eq!(r.show(&["issue", "show", "3"])["labels"], serde_json::json!(["--json"]))),
        ),
        (
            "issue new --parent",
            vec!["issue", "new", "t", "--parent", "--json"],
            Rejected("invalid entity id '--json' (expected a positive integer)"),
        ),
        (
            "issue new --blocked-by",
            vec!["issue", "new", "t", "--blocked-by", "--json"],
            Rejected("invalid entity id '--json' (expected a positive integer)"),
        ),
        (
            "issue new --idempotency-key",
            vec!["issue", "new", "t", "--idempotency-key", "--json"],
            Stored(|r| has_ref(r, &record_ref("issue-new"))),
        ),
        (
            "issue new -- (end of options)",
            vec!["issue", "new", "--", "--json"],
            Stored(|r| assert_eq!(r.show(&["issue", "show", "3"])["title"], "--json")),
        ),
        (
            "issue list --parent",
            vec!["issue", "list", "--parent", "--json"],
            Rejected("invalid entity id '--json' (expected a positive integer)"),
        ),
        (
            "issue list --state",
            vec!["issue", "list", "--state", "--json"],
            Rejected("invalid --state '--json'; expected open, closed, or all"),
        ),
        (
            "issue comment --idempotency-key",
            vec!["issue", "comment", "1", "c", "--idempotency-key", "--json"],
            Stored(|r| has_ref(r, &record_ref("issue-comment"))),
        ),
        (
            "pr create --source",
            vec!["pr", "create", "--source", "--json", "--base", "main", "q"],
            Rejected("'--json' is not a canonical local branch; use a plain branch name (tags, remote-tracking refs, OIDs, and revision expressions are rejected)"),
        ),
        (
            "pr create --base",
            vec!["pr", "create", "--source", "feature", "--base", "--json", "q"],
            Rejected("'--json' is not a canonical local branch; use a plain branch name (tags, remote-tracking refs, OIDs, and revision expressions are rejected)"),
        ),
        (
            "pr create --body",
            vec!["pr", "create", "--source", "feature", "--base", "main", "q", "--body", "--json"],
            Stored(|r| assert_eq!(r.show(&["pr", "show", "3"])["description"], "--json")),
        ),
        (
            "pr create --label",
            vec!["pr", "create", "--source", "feature", "--base", "main", "q", "--label", "--json"],
            Stored(|r| assert_eq!(r.show(&["pr", "show", "3"])["labels"], serde_json::json!(["--json"]))),
        ),
        (
            "pr create -l",
            vec!["pr", "create", "--source", "feature", "--base", "main", "q", "-l", "--json"],
            Stored(|r| assert_eq!(r.show(&["pr", "show", "3"])["labels"], serde_json::json!(["--json"]))),
        ),
        (
            "pr create --closes",
            vec!["pr", "create", "--source", "feature", "--base", "main", "q", "--closes", "--json"],
            Rejected("invalid entity id '--json' (expected a positive integer)"),
        ),
        (
            "pr create --idempotency-key",
            vec!["pr", "create", "--source", "feature", "--base", "main", "q", "--idempotency-key", "--json"],
            Stored(|r| has_ref(r, &record_ref("pr-create"))),
        ),
        (
            "pr create -- (end of options)",
            vec!["pr", "create", "--source", "feature", "--base", "main", "--", "--json"],
            Stored(|r| assert_eq!(r.show(&["pr", "show", "3"])["title"], "--json")),
        ),
        (
            "pr comment --idempotency-key",
            vec!["pr", "comment", "2", "c", "--idempotency-key", "--json"],
            Stored(|r| has_ref(r, &record_ref("pr-comment"))),
        ),
        (
            "pr review --file",
            vec!["pr", "review", "2", "--approve", "--file", "--json", "--line", "1", "--commit", "HEAD"],
            Stored(|r| assert_eq!(r.tip_event("refs/forge/prs/2/head")["body"]["file"], "--json")),
        ),
        (
            "pr review --line",
            vec!["pr", "review", "2", "--approve", "--file", "f", "--line", "--json", "--commit", "HEAD"],
            Stored(|r| assert_eq!(r.tip_event("refs/forge/prs/2/head")["body"]["line"], "--json")),
        ),
        (
            "pr review --commit",
            vec!["pr", "review", "2", "--approve", "--commit", "--json"],
            Rejected("inline review --commit '--json' does not resolve to a commit (FR-005 anchor must be a real commit)"),
        ),
        (
            "pr abandon --reason",
            vec!["pr", "abandon", "2", "--reason", "--json"],
            Stored(|r| assert_eq!(r.show(&["pr", "show", "2"])["abandoned"], "--json")),
        ),
        (
            "pr prepare --strategy",
            vec!["pr", "prepare", "2", "--strategy", "--json"],
            Rejected("unknown strategy '--json'; use --strategy merge|squash|rebase|ff"),
        ),
        (
            "pr merge --recycle-worktree",
            vec!["pr", "merge", "2", "--recycle-worktree", "--json"],
            // The path reaches the merge, which the gate refuses.
            Rejected("PR #2 is not approved (effective decision: none)"),
        ),
    ]
}

/// One table row in one output mode. Returns a failure line or `None`.
fn check(case: &str, args: &[&'static str], kept: &Kept, standalone: bool) -> Option<String> {
    let repo = Repo::new();
    let args: Vec<&str> = if standalone {
        [&args[..2], &["--json"], &args[2..]].concat()
    } else {
        args.to_vec()
    };
    let output = repo.run(&args);
    let stdout = String::from_utf8(output.stdout.clone()).unwrap();
    let mode = if standalone {
        "with --json"
    } else {
        "without --json"
    };
    let fail = |why: String| Some(format!("{case} ({mode}): {why}"));
    match kept {
        Kept::Stored(read_back) => {
            if output.status.code() != Some(0) {
                return fail(format!(
                    "exit {:?}: {}",
                    output.status.code(),
                    stderr(&output)
                ));
            }
            let is_json = serde_json::from_str::<serde_json::Value>(&stdout).is_ok();
            if is_json != standalone {
                return fail(format!("stdout JSON is {is_json}: {stdout}"));
            }
            let read = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| read_back(&repo)));
            read.is_err()
                .then(|| format!("{case} ({mode}): stored value differs"))
        }
        Kept::Rejected(message) => {
            if output.status.code() != Some(1) {
                return fail(format!("exit {:?}", output.status.code()));
            }
            let line = stderr(&output);
            if line != format!("git-forge: {message}\n") {
                return fail(format!("stderr: {line}"));
            }
            match (standalone, stdout.is_empty()) {
                (false, false) => fail(format!("JSON error without --json: {stdout}")),
                (true, _) => {
                    let error: serde_json::Value = match serde_json::from_str(&stdout) {
                        Ok(error) => error,
                        Err(_) => return fail(format!("no JSON error: {stdout}")),
                    };
                    let expected = line.trim_end().strip_prefix("git-forge: ").unwrap();
                    (error["error"]["message"] != expected)
                        .then(|| format!("{case} ({mode}): JSON error {error}"))
                }
                _ => None,
            }
        }
    }
}

#[test]
fn json_as_an_option_value_is_data_for_every_value_taking_option() {
    let mut failures = Vec::new();
    for (case, args, kept) in rows() {
        for standalone in [false, true] {
            failures.extend(check(case, &args, &kept, standalone));
        }
    }
    assert!(failures.is_empty(), "{failures:#?}");
}

/// `(program, args, message, hint)` for every dispatch-level failure path.
type DispatchCase = (&'static str, Vec<&'static str>, String, &'static str);

fn dispatch_cases() -> Vec<DispatchCase> {
    let unknown = |what: &str, token: &str| format!("unknown {what} '{token}'");
    vec![
        (
            "git-forge",
            vec!["issue", "--json"],
            unknown("issue subcommand", "--json"),
            "Run the command with --help and correct its arguments.",
        ),
        (
            "git-forge",
            vec!["pr", "--json"],
            unknown("pr subcommand", "--json"),
            "Run the command with --help and correct its arguments.",
        ),
        (
            "git-forge",
            vec!["ci", "--json"],
            unknown("ci subcommand", "--json"),
            "Run the command with --help and correct its arguments.",
        ),
        (
            "git-forge",
            vec!["issue", "--json", "bogus"],
            unknown("issue subcommand", "--json"),
            "Run the command with --help and correct its arguments.",
        ),
        (
            "git-forge",
            vec!["issue", "bogus", "--json"],
            unknown("issue subcommand", "bogus"),
            "Run the command with --help and correct its arguments.",
        ),
        (
            "git-forge",
            vec!["pr", "bogus", "--json"],
            unknown("pr subcommand", "bogus"),
            "Run the command with --help and correct its arguments.",
        ),
        (
            "git-forge",
            vec!["ci", "bogus", "--json"],
            unknown("ci subcommand", "bogus"),
            "Run the command with --help and correct its arguments.",
        ),
        (
            "git-forge",
            vec!["--json"],
            unknown("command", "--json"),
            "Run git forge help.",
        ),
        (
            "git-forge",
            vec!["bogus", "--json"],
            unknown("command", "bogus"),
            "Run git forge help.",
        ),
        (
            "git-forge",
            vec!["forge", "--json"],
            unknown("forge subcommand", "--json"),
            "Run git forge help.",
        ),
        (
            "git-forge",
            vec!["forge", "issue", "--json"],
            unknown("issue subcommand", "--json"),
            "Run the command with --help and correct its arguments.",
        ),
        (
            "git-forge",
            vec!["forge", "pr", "--json", "bogus"],
            unknown("pr subcommand", "--json"),
            "Run the command with --help and correct its arguments.",
        ),
        (
            "git-forge",
            vec!["version", "--json"],
            "unknown option '--json'".into(),
            "Run the command with --help and correct its arguments.",
        ),
        (
            "git-issue",
            vec!["--json"],
            unknown("issue subcommand", "--json"),
            "Run the command with --help and correct its arguments.",
        ),
        (
            "git-pr",
            vec!["bogus", "--json"],
            unknown("pr subcommand", "bogus"),
            "Run the command with --help and correct its arguments.",
        ),
    ]
}

/// Every dispatch path that fails before or instead of a command grammar
/// prints the JSON error when a standalone `--json` is present: the
/// namespace subcommand slot, an unknown subcommand, an unknown command,
/// the direct `forge` form and the `git-issue`/`git-pr` wrappers. Nothing is
/// written.
#[test]
fn dispatch_errors_with_a_standalone_json_print_the_json_error() {
    let dir = std::env::temp_dir().join(format!(
        "gf-json-dispatch-{}-{}",
        std::process::id(),
        NEXT_REPO.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir(&dir).unwrap();
    let git = |args: &[&str]| {
        assert!(Command::new("git")
            .args(args)
            .current_dir(&dir)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .output()
            .unwrap()
            .status
            .success());
    };
    git(&["init", "-q"]);
    let binary = PathBuf::from(env!("CARGO_BIN_EXE_git-forge"));
    for wrapper in ["git-issue", "git-pr"] {
        std::os::unix::fs::symlink(&binary, dir.join(wrapper)).unwrap();
    }
    let cases = dispatch_cases();
    let mut failures = Vec::new();
    for (program, args, message, hint) in &cases {
        let program = if *program == "git-forge" {
            binary.clone()
        } else {
            dir.join(program)
        };
        let output = Command::new(&program)
            .args(args)
            .current_dir(&dir)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env_remove("GIT_FORGE_ROLE")
            .output()
            .unwrap();
        let stdout = String::from_utf8(output.stdout.clone()).unwrap();
        let input = message.rsplit('\'').nth(1).unwrap();
        let expected = serde_json::json!({"error": {
            "code": "E_USAGE", "message": message, "input": input,
            "hint": hint, "retryable": false,
        }});
        let parsed = serde_json::from_str::<serde_json::Value>(&stdout).ok();
        if output.status.code() != Some(1)
            || stderr(&output) != format!("git-forge: {message}\n")
            || parsed.as_ref() != Some(&expected)
            || stdout.lines().count() != 1
        {
            failures.push(format!(
                "{} {args:?}: exit {:?}, stdout {stdout:?}, stderr {:?}",
                program.file_name().unwrap().to_string_lossy(),
                output.status.code(),
                stderr(&output)
            ));
        }
    }
    let refs = Command::new("git")
        .args(["for-each-ref", "refs/forge/"])
        .current_dir(&dir)
        .output()
        .unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    assert!(failures.is_empty(), "{failures:#?}");
    assert!(refs.stdout.is_empty(), "a dispatch error wrote forge refs");
}
