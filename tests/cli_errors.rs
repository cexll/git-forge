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
            let input = if matches!((namespace, sub), ("pr", "review" | "update" | "abandon")) {
                "--json"
            } else {
                ""
            };
            assert_error(&json, "E_USAGE", &message, input, false);
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
    assert_error(
        &repo.run(&["ci", "run", "1", "--json"]),
        "E_CI_FAILED",
        "CI run for PR #1 failed (.forge/ci.sh): exited with status 1",
        "1",
        false,
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
