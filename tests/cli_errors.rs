//! Typed failure contract at the built CLI boundary.
use std::path::PathBuf;
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT_REPO: AtomicU64 = AtomicU64::new(0);

struct Repo(PathBuf);
impl Repo {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "gf-errors-{}-{}",
            std::process::id(),
            NEXT_REPO.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&path).unwrap();
        assert!(Command::new("git")
            .args(["init", "-q"])
            .current_dir(&path)
            .status()
            .unwrap()
            .success());
        Self(path)
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
}
impl Drop for Repo {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).unwrap();
    }
}

fn assert_error(output: &Output, code: &str, message: &str, input: &str, retryable: bool) {
    assert_eq!(
        output.status.code(),
        Some(1),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&output.stderr),
        format!("git-forge: {message}\n")
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(stdout.lines().count(), 1, "one JSON document: {stdout}");
    let document: serde_json::Value = serde_json::from_str(&stdout).expect("valid complete JSON");
    let root = document.as_object().expect("object envelope");
    assert_eq!(
        root.keys().map(String::as_str).collect::<Vec<_>>(),
        ["error"]
    );
    let error = root["error"].as_object().expect("error object");
    assert_eq!(
        error.keys().map(String::as_str).collect::<Vec<_>>(),
        ["code", "hint", "input", "message", "retryable"]
    );
    for (key, expected) in [("code", code), ("message", message), ("input", input)] {
        assert_eq!(error[key].as_str(), Some(expected), "{key}");
    }
    let hint = match code {
        "E_USAGE" => "Run the command with --help and correct its arguments.",
        "E_NOT_FOUND" => "List the existing entities or branches and use an existing identifier.",
        "E_CONFLICT" => "Read the current state and retry the command.",
        "E_CI_FAILED" => "Fix the CI plan and run CI again.",
        "E_INVALID" => "Correct the input or repository state before retrying.",
        _ => panic!("supply an independent expected hint for {code}"),
    };
    assert_eq!(error["hint"].as_str(), Some(hint));
    assert_eq!(error["retryable"].as_bool(), Some(retryable));
    assert_eq!(
        error["message"].as_str().unwrap(),
        String::from_utf8_lossy(&output.stderr)
            .strip_prefix("git-forge: ")
            .unwrap()
            .strip_suffix('\n')
            .unwrap()
    );
}

#[test]
fn error_oracle_rejects_corrupted_cli_documents() {
    let repo = Repo::new();
    let message = "usage: git forge pr show <n> [--json]";
    for (from, to) in [
        (message, "wrong message"),
        ("\"hint\":\"", "\"hint\":false,\"ignored\":\""),
        ("\"input\":\"\"", "\"input\":\"\",\"extra\":1"),
        ("\"retryable\":false", "\"retryable\":\"false\""),
        ("\"hint\":", "\"hint\"::"),
    ] {
        let mut output = repo.run(&["pr", "show", "--json"]);
        let stdout = String::from_utf8(output.stdout).unwrap();
        assert!(stdout.contains(from));
        output.stdout = stdout.replace(from, to).into_bytes();
        assert!(
            std::panic::catch_unwind(|| assert_error(&output, "E_USAGE", message, "", false))
                .is_err(),
            "oracle accepted corrupted CLI output: {to}"
        );
    }
}

#[test]
fn missing_pr_show_is_usage_in_text_and_json() {
    let repo = Repo::new();
    let message = "usage: git forge pr show <n> [--json]";
    let plain = repo.run(&["pr", "show"]);
    assert_eq!(
        plain.status.code(),
        Some(1),
        "{}",
        String::from_utf8_lossy(&plain.stderr)
    );
    assert!(plain.stdout.is_empty());
    assert_eq!(
        String::from_utf8_lossy(&plain.stderr),
        format!("git-forge: {message}\n")
    );
    assert_error(
        &repo.run(&["pr", "show", "--json"]),
        "E_USAGE",
        message,
        "",
        false,
    );
}

#[test]
fn missing_pr_diff_is_usage_in_text_and_json() {
    let repo = Repo::new();
    let message = "usage: git forge pr diff <n> [--json]";
    let plain = repo.run(&["pr", "diff"]);
    assert_eq!(plain.status.code(), Some(1));
    assert!(plain.stdout.is_empty());
    assert_eq!(plain.stderr, format!("git-forge: {message}\n").as_bytes());
    assert_error(
        &repo.run(&["pr", "diff", "--json"]),
        "E_USAGE",
        message,
        "",
        false,
    );
}

#[test]
fn merge_json_reaches_missing_pr_boundary() {
    let repo = Repo::new();
    assert_error(
        &repo.run(&["pr", "merge", "999", "--json"]),
        "E_NOT_FOUND",
        "PR #999 does not exist",
        "999",
        false,
    );
}

#[test]
fn missing_local_source_and_base_are_not_found() {
    let repo = Repo::new();
    for args in [
        vec!["config", "user.name", "Test"],
        vec!["config", "user.email", "test@example.com"],
        vec!["commit", "--allow-empty", "-qm", "base"],
        vec!["branch", "-M", "main"],
    ] {
        assert!(Command::new("git")
            .args(args)
            .current_dir(&repo.0)
            .status()
            .unwrap()
            .success());
    }
    for (source, base, missing) in [
        ("missing", "main", "missing"),
        ("main", "missing", "missing"),
        ("refs/heads/missing", "main", "refs/heads/missing"),
        ("main", "refs/heads/missing", "refs/heads/missing"),
    ] {
        let message = format!("'{missing}' is not a canonical local branch; use a plain branch name (tags, remote-tracking refs, OIDs, and revision expressions are rejected)");
        assert_error(
            &repo.run(&[
                "pr", "create", "--source", source, "--base", base, "title", "--json",
            ]),
            "E_NOT_FOUND",
            &message,
            missing,
            false,
        );
    }
}

#[test]
fn invalid_role_reports_configured_input() {
    let repo = Repo::new();
    let output = Command::new(env!("CARGO_BIN_EXE_git-forge"))
        .args(["issue", "new", "title", "--json"])
        .current_dir(&repo.0)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_FORGE_ROLE", "bogus")
        .output()
        .unwrap();
    assert_error(
        &output,
        "E_INVALID",
        "unknown GIT_FORGE_ROLE 'bogus' (expected one of: worker, reviewer, orchestrator)",
        "bogus",
        false,
    );
    assert_eq!(repo.run(&["issue", "list", "--json"]).stdout, b"[]\n");
}

#[test]
fn version_errors_identify_flag_and_positional() {
    let repo = Repo::new();
    for (input, message) in [
        ("--bogus", "unknown option '--bogus'"),
        ("unexpected", "unexpected argument 'unexpected'"),
    ] {
        let plain = repo.run(&["version", input]);
        assert_eq!(plain.status.code(), Some(1));
        assert!(plain.stdout.is_empty());
        assert_eq!(plain.stderr, format!("git-forge: {message}\n").as_bytes());
        assert_error(
            &repo.run(&["version", input, "--json"]),
            "E_USAGE",
            message,
            input,
            false,
        );
    }
}
#[test]
fn every_subcommand_handles_no_arguments() {
    let repo = Repo::new();
    for (namespace, subcommands) in [
        ("issue", "new apply list show comment close reopen help"),
        (
            "pr",
            "create list show comment review update prepare diff merge abandon help",
        ),
        ("ci", "run help"),
    ] {
        for sub in subcommands.split_whitespace() {
            let success = matches!(sub, "list" | "help");
            let plain = repo.run(&[namespace, sub]);
            assert_eq!(
                plain.status.code(),
                Some(if success { 0 } else { 1 }),
                "{namespace} {sub}: {}",
                String::from_utf8_lossy(&plain.stderr)
            );
            if success {
                continue;
            }
            assert!(plain.stdout.is_empty());
            let stderr = String::from_utf8_lossy(&plain.stderr);
            assert!(
                stderr.starts_with(&format!("git-forge: usage: git forge {namespace} {sub} ")),
                "{stderr}"
            );
            let json = repo.run(&[namespace, sub, "--json"]);
            let message = String::from_utf8_lossy(&json.stderr)
                .trim_end()
                .strip_prefix("git-forge: ")
                .unwrap()
                .to_string();
            // Every subcommand accepts `--json`, so it reports the same
            // missing-argument usage error as the plain run.
            assert_eq!(stderr, format!("git-forge: {message}\n"));
            assert_error(&json, "E_USAGE", &message, "", false);
        }
    }
    assert!(repo.run(&["version"]).status.success());
    assert!(repo.run(&["help"]).status.success());
}

#[test]
fn missing_entities_and_unknown_flags_have_specific_codes() {
    let repo = Repo::new();
    for args in [
        vec!["issue", "show", "999"],
        vec!["issue", "comment", "999", "body"],
        vec!["issue", "close", "999"],
        vec!["issue", "reopen", "999"],
        vec!["pr", "show", "999"],
        vec!["pr", "diff", "999"],
        vec!["pr", "comment", "999", "body"],
        vec!["ci", "run", "999"],
    ] {
        let plain = repo.run(&args);
        assert_eq!(plain.status.code(), Some(1));
        assert!(plain.stdout.is_empty());
        let expected = format!(
            "{} #999 does not exist",
            if args[0] == "issue" { "issue" } else { "PR" }
        );
        assert_eq!(
            String::from_utf8_lossy(&plain.stderr),
            format!("git-forge: {expected}\n")
        );
        let mut json_args = args.clone();
        json_args.push("--json");
        assert_error(
            &repo.run(&json_args),
            "E_NOT_FOUND",
            &expected,
            "999",
            false,
        );
    }
    for args in [
        vec!["issue", "new", "title", "--bogus"],
        vec!["issue", "list", "--bogus"],
        vec!["pr", "create", "--bogus"],
        vec!["pr", "show", "999", "--bogus"],
        vec!["ci", "run", "--bogus"],
    ] {
        let plain = repo.run(&args);
        assert!(plain.stdout.is_empty());
        let message = String::from_utf8_lossy(&plain.stderr)
            .trim_end()
            .strip_prefix("git-forge: ")
            .unwrap()
            .to_owned();
        assert!(message.contains("--bogus"));
        let mut json_args = args;
        json_args.push("--json");
        assert_error(&repo.run(&json_args), "E_USAGE", &message, "--bogus", false);
    }
}

#[cfg(unix)]
#[test]
fn competing_apply_reports_retryable_conflict_without_partial_publication() {
    use std::os::unix::fs::PermissionsExt;
    let repo = Repo::new();
    assert!(repo.run(&["issue", "new", "Initial"]).status.success());
    let plan = repo.0.join("plan.md");
    std::fs::write(&plan, "---\nkey: requested\ntitle: Requested\n---\n").unwrap();
    let shims = repo.0.join("shims");
    std::fs::create_dir(&shims).unwrap();
    let marker = shims.join("competed");
    let shim = shims.join("git");
    std::fs::write(&shim, format!("#!/bin/bash\nif [[ \"$*\" == *'update-ref --stdin'* && ! -e '{}' ]]; then\n touch '{}'\n PATH=/usr/bin:/bin '{}' issue new Competitor >/dev/null || exit $?\nfi\nexec /usr/bin/git \"$@\"\n", marker.display(), marker.display(), env!("CARGO_BIN_EXE_git-forge"))).unwrap();
    std::fs::set_permissions(&shim, std::fs::Permissions::from_mode(0o755)).unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_git-forge"))
        .args(["issue", "apply", plan.to_str().unwrap(), "--json"])
        .current_dir(&repo.0)
        .env(
            "PATH",
            format!("{}:{}", shims.display(), std::env::var("PATH").unwrap()),
        )
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env_remove("GIT_FORGE_ROLE")
        .output()
        .unwrap();
    assert!(marker.exists(), "competing real allocation must execute");
    assert_error(
        &output,
        "E_CONFLICT",
        "issue apply: atomic transaction failed: ref CAS conflict — ref moved, retry",
        "",
        true,
    );
    let listed = repo.run(&["issue", "list", "--json"]);
    assert!(listed.status.success());
    let list = String::from_utf8_lossy(&listed.stdout);
    assert!(list.contains("Competitor"), "{list}");
    assert!(!list.contains("Requested"), "{list}");
    assert_eq!(repo.run(&["issue", "show", "3"]).status.code(), Some(1));
}

#[test]
fn failed_ci_has_json_error_and_records_failed_check() {
    let repo = Repo::new();
    for args in [
        vec!["config", "user.name", "Test"],
        vec!["config", "user.email", "test@example.com"],
        vec!["commit", "--allow-empty", "-qm", "base"],
        vec!["branch", "-M", "main"],
        vec!["checkout", "-qb", "feature"],
    ] {
        assert!(Command::new("git")
            .args(args)
            .current_dir(&repo.0)
            .status()
            .unwrap()
            .success());
    }
    std::fs::create_dir(repo.0.join(".forge")).unwrap();
    std::fs::write(repo.0.join(".forge/ci.sh"), "#!/bin/bash\nexit 1\n").unwrap();
    for args in [
        vec!["add", ".forge/ci.sh"],
        vec!["commit", "-qm", "CI plan"],
        vec!["checkout", "-q", "main"],
    ] {
        assert!(Command::new("git")
            .args(args)
            .current_dir(&repo.0)
            .status()
            .unwrap()
            .success());
    }
    assert!(repo
        .run(&["pr", "create", "--source", "feature", "--base", "main", "PR"])
        .status
        .success());
    // #38: the CI failure adds the reason to the message and `data` with
    // the reason and the (empty) output tail.
    let message =
        "CI run for PR #1 failed (.forge/ci.sh): exited with status 1; reason: nonzero_exit";
    let output = repo.run(&["ci", "run", "1", "--json"]);
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(
        String::from_utf8_lossy(&output.stderr),
        format!("git-forge: {message}\n")
    );
    let document: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        document,
        serde_json::json!({"error": {
            "code": "E_CI_FAILED",
            "message": message,
            "input": "1",
            "hint": "Fix the CI plan and run CI again.",
            "retryable": false,
            "data": {"pr": 1, "reason": "nonzero_exit", "tail": ""},
        }})
    );
    let show = repo.run(&["pr", "show", "1", "--json"]);
    assert!(show.status.success());
    assert!(String::from_utf8_lossy(&show.stdout).contains("\"status\":\"failed\""));
}

#[test]
fn existing_hexadecimal_local_branch_remains_accepted() {
    let repo = Repo::new();
    let branch = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    for args in [
        vec!["config", "user.name", "Test"],
        vec!["config", "user.email", "test@example.com"],
        vec!["commit", "--allow-empty", "-qm", "base"],
        vec!["branch", "-M", "main"],
        vec!["checkout", "-qb", branch],
        vec!["commit", "--allow-empty", "-qm", "source"],
    ] {
        assert!(Command::new("git")
            .args(args)
            .current_dir(&repo.0)
            .status()
            .unwrap()
            .success());
    }
    for source in [branch.to_string(), format!("refs/heads/{branch}")] {
        let output = repo.run(&[
            "pr", "create", "--source", &source, "--base", "main", "title", "--json",
        ]);
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let object: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(object["source"].as_str(), Some(branch));
    }
}

#[test]
fn missing_leading_dash_branch_is_invalid() {
    let repo = Repo::new();
    for source in ["-bad", "refs/heads/-bad"] {
        let message = format!("'{source}' is not a canonical local branch; use a plain branch name (tags, remote-tracking refs, OIDs, and revision expressions are rejected)");
        assert_error(
            &repo.run(&[
                "pr", "create", "--source", source, "--base", "main", "title", "--json",
            ]),
            "E_INVALID",
            &message,
            "",
            false,
        );
    }
}

/// A repository with `main`, a `feature` branch one commit ahead, and PR #1
/// from `feature` into `main`.
fn pr_repo() -> Repo {
    let repo = Repo::new();
    for args in [
        vec!["config", "user.name", "Test"],
        vec!["config", "user.email", "test@example.com"],
        vec!["commit", "--allow-empty", "-qm", "base"],
        vec!["branch", "-M", "main"],
        vec!["checkout", "-qb", "feature"],
        vec!["commit", "--allow-empty", "-qm", "source"],
        vec!["checkout", "-q", "main"],
    ] {
        assert!(Command::new("git")
            .args(args)
            .current_dir(&repo.0)
            .status()
            .unwrap()
            .success());
    }
    assert!(repo
        .run(&["pr", "create", "--source", "feature", "--base", "main", "PR"])
        .status
        .success());
    repo
}

/// An inline-review anchor that does not name a commit is the caller's
/// input error and names the token: a well-formed revision that resolves to
/// nothing or to a non-commit is `E_INVALID`, a malformed revision is
/// `E_USAGE`. The review writes nothing.
#[test]
fn unresolvable_review_anchor_names_the_token() {
    let repo = pr_repo();
    let before = repo.run(&["pr", "show", "1", "--json"]).stdout;
    for (token, code) in [
        ("not-a-revision", "E_INVALID"),
        ("HEAD^{tree}", "E_INVALID"),
        ("main..feature", "E_USAGE"),
    ] {
        let message = format!(
            "inline review --commit '{token}' does not resolve to a commit (FR-005 anchor must \
             be a real commit)"
        );
        assert_error(
            &repo.run(&[
                "pr",
                "review",
                "1",
                "--approve",
                "--commit",
                token,
                "--json",
            ]),
            code,
            &message,
            token,
            false,
        );
    }
    assert_eq!(repo.run(&["pr", "show", "1", "--json"]).stdout, before);
}

/// Pure argument errors are decided before the repository is opened: run
/// outside any git repository, each is `E_USAGE`, not the missing-repository
/// `E_INVALID`.
#[test]
fn argument_errors_precede_repository_access() {
    let dir = std::env::temp_dir().join(format!("gf-errors-norepo-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let run = |args: &[&str]| run_outside_repository(&dir, args);
    let anchor = "inline review requires --commit <hash> to anchor the comment (FR-005); add \
                  --commit or drop --file/--line";
    let cases: [(&[&str], &str); 5] = [
        (
            &["pr", "review", "1", "--approve", "--file", "f", "--json"],
            anchor,
        ),
        (
            &["pr", "review", "1", "--reject", "--line", "3", "--json"],
            anchor,
        ),
        (
            &["pr", "merge", "1", "--force", "--json"],
            "--force requires --recycle-worktree",
        ),
        (
            &[
                "pr", "create", "--source", "", "--base", "main", "t", "--json",
            ],
            "branch name must be non-empty",
        ),
        (
            &["pr", "create", "--source", "f", "--base", "", "t", "--json"],
            "branch name must be non-empty",
        ),
    ];
    let mut failures = Vec::new();
    for (args, message) in cases {
        let output = run(args);
        if std::panic::catch_unwind(|| assert_error(&output, "E_USAGE", message, "", false))
            .is_err()
        {
            failures.push(format!(
                "{args:?}: {}",
                String::from_utf8_lossy(&output.stdout).trim_end()
            ));
        }
    }
    std::fs::remove_dir_all(&dir).unwrap();
    assert!(failures.is_empty(), "{failures:#?}");
}

/// Run the built CLI in `dir`, which is not inside any git repository.
fn run_outside_repository(dir: &std::path::Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_git-forge"))
        .args(args)
        .current_dir(dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CEILING_DIRECTORIES", dir.parent().unwrap())
        .env_remove("GIT_FORGE_ROLE")
        .output()
        .unwrap()
}

/// The `E_INVALID` checks that depend only on the arguments (branch-name
/// format, a self-PR by name, a duplicate `--blocked-by`) are decided before
/// the repository is opened and before an idempotency replay: run outside
/// any git repository, each returns its own error, not the missing
/// repository.
#[test]
fn argument_only_invalid_checks_precede_repository_access() {
    let dir = std::env::temp_dir().join(format!("gf-errors-norepo-invalid-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let branch = |arg: &str| {
        format!(
            "'{arg}' is not a canonical local branch; use a plain branch name (tags, \
             remote-tracking refs, OIDs, and revision expressions are rejected)"
        )
    };
    let self_pr = "source and base branch must differ (no self-PR)".to_string();
    let duplicate = "issue new: blocked_by: duplicate issue".to_string();
    let create = |source: &'static str, base: &'static str, keyed: bool| {
        let mut args = vec!["pr", "create", "--source", source, "--base", base, "t"];
        if keyed {
            args.extend(["--idempotency-key", "k"]);
        }
        args.push("--json");
        args
    };
    let blocked = |keyed: bool| {
        let mut args = vec![
            "issue",
            "new",
            "t",
            "--blocked-by",
            "1",
            "--blocked-by",
            "1",
        ];
        if keyed {
            args.extend(["--idempotency-key", "k"]);
        }
        args.push("--json");
        args
    };
    let mut cases: Vec<(Vec<&str>, String, &str)> = Vec::new();
    for keyed in [false, true] {
        cases.push((create("bad name", "main", keyed), branch("bad name"), ""));
        cases.push((
            create("f", "refs/tags/v1", keyed),
            branch("refs/tags/v1"),
            "",
        ));
        cases.push((create("f", "HEAD", keyed), branch("HEAD"), ""));
        cases.push((
            create("main", "refs/heads/main", keyed),
            self_pr.clone(),
            "",
        ));
        cases.push((blocked(keyed), duplicate.clone(), "blocked_by"));
    }
    let mut failures = Vec::new();
    for (args, message, input) in &cases {
        let output = run_outside_repository(&dir, args);
        let stdout = String::from_utf8_lossy(&output.stdout);
        let error = serde_json::from_str::<serde_json::Value>(stdout.trim_end())
            .unwrap_or_default()["error"]
            .clone();
        if output.status.code() != Some(1)
            || String::from_utf8_lossy(&output.stderr) != format!("git-forge: {message}\n")
            || error["code"] != "E_INVALID"
            || error["message"] != message.as_str()
            || error["input"] != *input
            || error["retryable"] != false
        {
            failures.push(format!("{args:?}: {}", stdout.trim_end()));
        }
    }
    std::fs::remove_dir_all(&dir).unwrap();
    assert!(failures.is_empty(), "{failures:#?}");
}
