//! `ci run` failure diagnostics at the built CLI boundary: every way a CI run
//! fails records exactly one `reason` and the last 4 KiB of combined output
//! with the failed Check, prints both on failure (`E_CI_FAILED` under
//! `--json`), exposes them in `pr show --json`, and the merge gate still
//! refuses the failed Check.
use std::path::PathBuf;
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT_REPO: AtomicU64 = AtomicU64::new(0);

const HINT: &str = "Fix the CI plan and run CI again.";
const CLEANUP_HINT: &str = "Inspect the reported worktree cleanup failure.";

struct Repo(PathBuf);

impl Drop for Repo {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

impl Repo {
    fn new(tag: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "gf-ci-diag-{tag}-{}-{}",
            std::process::id(),
            NEXT_REPO.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&path).unwrap();
        let repo = Self(path);
        repo.git(&["init", "-q", "-b", "main"]);
        repo.git(&["config", "user.name", "Test"]);
        repo.git(&["config", "user.email", "test@example.com"]);
        repo.git(&["commit", "-q", "--allow-empty", "-m", "base"]);
        repo
    }

    fn git(&self, args: &[&str]) -> String {
        let out = Command::new("git")
            .args(args)
            .current_dir(&self.0)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    fn forge(&self, args: &[&str], env: &[(&str, &str)]) -> Output {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_git-forge"));
        cmd.args(args)
            .current_dir(&self.0)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env_remove("GIT_FORGE_ROLE")
            .env_remove("GIT_FORGE_CI_TIMEOUT");
        for (key, value) in env {
            cmd.env(key, value);
        }
        cmd.output().unwrap()
    }

    /// Commit `files` on a `feature` branch and open PR #1 into `main`.
    fn pr_with(&self, files: &[(&str, Plan)]) {
        self.git(&["checkout", "-q", "-b", "feature"]);
        for (path, plan) in files {
            let dest = self.0.join(path);
            std::fs::create_dir_all(dest.parent().unwrap()).unwrap();
            match plan {
                Plan::Bytes(bytes) => std::fs::write(&dest, bytes).unwrap(),
                Plan::Symlink(target) => std::os::unix::fs::symlink(target, &dest).unwrap(),
            }
        }
        std::fs::write(self.0.join("feature.txt"), "feature\n").unwrap();
        self.git(&["add", "-A"]);
        self.git(&["commit", "-q", "-m", "feature"]);
        self.git(&["checkout", "-q", "main"]);
        let out = self.forge(
            &[
                "pr", "create", "--source", "feature", "--base", "main", "PR",
            ],
            &[],
        );
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    fn head_body(&self) -> serde_json::Value {
        let event = self.git(&["show", "refs/forge/prs/1/head:.forge/event.json"]);
        let event: serde_json::Value = serde_json::from_str(&event).unwrap();
        assert_eq!(event["kind"], "ci.check", "{event}");
        event["body"].clone()
    }

    fn ci_worktrees(&self) -> Vec<String> {
        self.git(&["worktree", "list", "--porcelain"])
            .lines()
            .filter_map(|line| line.strip_prefix("worktree "))
            .filter(|path| path.contains("git-forge-pr1-ci"))
            .map(String::from)
            .collect()
    }
}

enum Plan {
    Bytes(Vec<u8>),
    Symlink(PathBuf),
}

fn ci_sh(script: &str) -> Vec<(&'static str, Plan)> {
    vec![(".forge/ci.sh", Plan::Bytes(script.as_bytes().to_vec()))]
}

/// Independent oracle for `sanitize_terminal`: C0, DEL and C1 controls become
/// `\xNN`; everything else is kept.
fn terminal(text: &str) -> String {
    text.chars()
        .map(|c| match c as u32 {
            cp @ (0..=0x1f | 0x7f..=0x9f) => format!("\\x{cp:02x}"),
            _ => c.to_string(),
        })
        .collect()
}

/// The `ci run` failure line: `<failure>; reason: <r>` plus the sanitized
/// tail when the run produced output.
fn failure_message(failure: &str, reason: &str, tail: &str) -> String {
    if tail.is_empty() {
        format!("{failure}; reason: {reason}")
    } else {
        format!(
            "{failure}; reason: {reason}; output tail: {}",
            terminal(tail)
        )
    }
}

struct Expected<'a> {
    message: String,
    hint: &'a str,
    reason: &'a str,
    tail: String,
    plan: Option<&'a str>,
}

/// Assert the complete failure contract of one `ci run 1 --json`: exit code,
/// stderr, the JSON error object, the stored Check, `pr show --json`, and
/// the merge gate refusing the failed Check.
fn assert_ci_failure(repo: &Repo, output: &Output, expected: &Expected<'_>) {
    assert_eq!(
        output.status.code(),
        Some(1),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8(output.stderr.clone()).unwrap(),
        format!("git-forge: {}\n", expected.message)
    );
    let stdout = String::from_utf8(output.stdout.clone()).unwrap();
    assert_eq!(stdout.lines().count(), 1, "one JSON document: {stdout}");
    let document: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(
        document,
        serde_json::json!({"error": {
            "code": "E_CI_FAILED",
            "message": expected.message,
            "input": "1",
            "hint": expected.hint,
            "retryable": false,
            "data": {"pr": 1, "reason": expected.reason, "tail": expected.tail},
        }})
    );
    let body = repo.head_body();
    assert_eq!(body["status"], "failed", "{body}");
    assert_eq!(body["reason"], expected.reason, "{body}");
    assert_eq!(body["tail"], expected.tail.as_str(), "{body}");
    assert_eq!(body.get("plan").and_then(|p| p.as_str()), expected.plan);

    let show = repo.forge(&["pr", "show", "1", "--json"], &[]);
    assert!(show.status.success());
    let show: serde_json::Value = serde_json::from_slice(&show.stdout).unwrap();
    assert_eq!(
        show["ci"],
        serde_json::json!({
            "status": "failed",
            "plan": expected.plan,
            "reason": expected.reason,
            "tail": expected.tail,
        })
    );

    assert_gate_refuses(repo);
}

/// Approve PR #1: the merge gate still refuses the failed Check and the base
/// branch does not move.
fn assert_gate_refuses(repo: &Repo) {
    let review = repo.forge(&["pr", "review", "1", "--approve"], &[]);
    assert!(
        review.status.success(),
        "{}",
        String::from_utf8_lossy(&review.stderr)
    );
    let main_before = repo.git(&["rev-parse", "main"]);
    let merge = repo.forge(&["pr", "merge", "1", "--json"], &[]);
    assert_eq!(merge.status.code(), Some(1));
    let merge: serde_json::Value = serde_json::from_slice(&merge.stdout).unwrap();
    assert_eq!(merge["error"]["code"], "E_GATE");
    assert_eq!(
        merge["error"]["message"],
        "PR #1 is not mergeable: latest CI Check is `failed` (expected `success`); run `git forge ci run 1`"
    );
    assert_eq!(repo.git(&["rev-parse", "main"]), main_before);
}

/// One `ci run` failure path: the PR content, the run environment, and the
/// expected first part of the message, reason, tail and plan.
struct Case {
    name: &'static str,
    files: Vec<(&'static str, Plan)>,
    env: Vec<(&'static str, String)>,
    failure: String,
    reason: &'static str,
    tail: String,
    plan: Option<&'static str>,
}

fn run_case(case: Case) {
    let repo = Repo::new(case.name);
    repo.pr_with(&case.files);
    let env: Vec<(&str, &str)> = case.env.iter().map(|(k, v)| (*k, v.as_str())).collect();
    let output = repo.forge(&["ci", "run", "1", "--json"], &env);
    assert_ci_failure(
        &repo,
        &output,
        &Expected {
            message: failure_message(&case.failure, case.reason, &case.tail),
            hint: HINT,
            reason: case.reason,
            tail: case.tail,
            plan: case.plan,
        },
    );
    assert!(
        repo.ci_worktrees().is_empty(),
        "{}: CI worktree left",
        case.name
    );
}

const SH: &str = "CI run for PR #1 failed (.forge/ci.sh)";

#[test]
fn nonzero_exit_records_reason_and_marker_tail() {
    run_case(Case {
        name: "nonzero",
        files: ci_sh("#!/bin/bash\necho before\necho MARKER-LINE >&2\nexit 1\n"),
        env: vec![],
        failure: format!("{SH}: exited with status 1"),
        reason: "nonzero_exit",
        tail: "before\nMARKER-LINE\n".into(),
        plan: Some(".forge/ci.sh"),
    });
}

#[test]
fn signal_killed_plan_is_nonzero_exit() {
    run_case(Case {
        name: "signal",
        files: ci_sh("#!/bin/bash\necho before-kill\nkill -9 $$\n"),
        env: vec![],
        failure: format!("{SH}: terminated by signal 9"),
        reason: "nonzero_exit",
        tail: "before-kill\n".into(),
        plan: Some(".forge/ci.sh"),
    });
}

#[test]
fn deadline_records_timeout_with_partial_tail() {
    run_case(Case {
        name: "timeout",
        files: ci_sh("#!/bin/bash\necho slow-start\nsleep 30\n"),
        env: vec![("GIT_FORGE_CI_TIMEOUT", "1".into())],
        failure: format!("{SH}: timed out"),
        reason: "timeout",
        tail: "slow-start\n".into(),
        plan: Some(".forge/ci.sh"),
    });
}

#[test]
fn surviving_background_process_is_cleanup_failed() {
    run_case(Case {
        name: "background",
        files: ci_sh("#!/bin/bash\necho bg\nsleep 30 &\nexit 0\n"),
        env: vec![],
        failure: format!("{SH}: left a background process running"),
        reason: "cleanup_failed",
        tail: "bg\n".into(),
        plan: Some(".forge/ci.sh"),
    });
}

/// No trusted `just` install: the debug-only seam makes every trusted
/// location unavailable, independent of what the host has installed.
#[cfg(debug_assertions)]
#[test]
fn missing_trusted_just_is_spawn_failed() {
    run_case(Case {
        name: "spawn",
        files: vec![("justfile", Plan::Bytes(b"check:\n    echo hi\n".to_vec()))],
        env: vec![("GIT_FORGE_TEST_NO_TRUSTED_JUST", "1".into())],
        failure: "CI run for PR #1 failed (just check): no trusted `just` found in system or \
                  operator install locations"
            .into(),
        reason: "spawn_failed",
        tail: String::new(),
        plan: Some("just check"),
    });
}

/// A release build ignores the debug-only seam: with a trusted `just` on the
/// host, `ci run` succeeds without `GIT_FORGE_TEST_NO_TRUSTED_JUST` and gives
/// the same success with `GIT_FORGE_TEST_NO_TRUSTED_JUST=1`. Runs only under
/// `cargo test --release`. A trusted `just` is a host precondition, decided by
/// the built binary's own resolution; without one, the test fails with the
/// binary's `no trusted \`just\`` diagnostic instead of passing vacuously.
#[cfg(not(debug_assertions))]
#[test]
fn release_build_ignores_the_no_trusted_just_seam() {
    let run = |env: &[(&str, &str)]| {
        let repo = Repo::new("seam-release");
        repo.pr_with(&[("justfile", Plan::Bytes(b"check:\n    echo hi\n".to_vec()))]);
        let output = repo.forge(&["ci", "run", "1", "--json"], env);
        let body = repo.head_body();
        (
            output.status.code(),
            body["status"].clone(),
            body["reason"].clone(),
            String::from_utf8_lossy(&output.stderr).into_owned(),
        )
    };
    let (code, status, reason, stderr) = run(&[]);
    assert!(
        code == Some(0) && status == "success",
        "this release test requires a trusted `just` on the host; the baseline \
         `ci run` without the seam variable must succeed, got exit {code:?}, \
         status {status}, reason {reason}: {stderr}"
    );
    let (seam_code, seam_status, seam_reason, seam_stderr) =
        run(&[("GIT_FORGE_TEST_NO_TRUSTED_JUST", "1")]);
    assert_eq!(
        (seam_code, seam_status, seam_reason),
        (code, status, reason),
        "a release build must ignore GIT_FORGE_TEST_NO_TRUSTED_JUST: {seam_stderr}"
    );
}

#[test]
fn missing_plan_is_plan_invalid() {
    run_case(Case {
        name: "noplan",
        files: vec![],
        env: vec![],
        failure: "CI run for PR #1 failed: no justfile in PR snapshot for the `just check` \
                  fallback (F-012)"
            .into(),
        reason: "plan_invalid",
        tail: String::new(),
        plan: None,
    });
}

#[test]
fn symlinked_plan_is_plan_invalid() {
    run_case(Case {
        name: "symlink",
        files: vec![(".forge/ci.sh", Plan::Symlink("/bin/true".into()))],
        env: vec![],
        failure: "CI run for PR #1 failed: refusing .forge/ci.sh: symlink (F-001)".into(),
        reason: "plan_invalid",
        tail: String::new(),
        plan: None,
    });
}

#[test]
fn justfile_failing_validation_is_plan_invalid() {
    run_case(Case {
        name: "justimport",
        files: vec![(
            "justfile",
            Plan::Bytes(b"import 'other.just'\ncheck:\n    echo hi\n".to_vec()),
        )],
        env: vec![],
        failure: "CI run for PR #1 failed (just check): refusing justfile: an `import`/`mod` \
                  source makes the `just check` fallback non-immutable (F-012)"
            .into(),
        reason: "plan_invalid",
        tail: String::new(),
        plan: Some("just check"),
    });
}

/// More than 64 KiB of output: the pipe is drained while the plan runs (no
/// timeout) and only the last 4096 bytes are kept.
#[test]
fn large_output_keeps_last_4096_bytes() {
    let all: String = (1..=20000).map(|i| format!("{i}\n")).collect();
    assert!(all.len() > 64 * 1024);
    run_case(Case {
        name: "large",
        files: ci_sh("#!/bin/bash\nseq 1 20000\nexit 3\n"),
        env: vec![("GIT_FORGE_CI_TIMEOUT", "60".into())],
        failure: format!("{SH}: exited with status 3"),
        reason: "nonzero_exit",
        tail: all[all.len() - 4096..].to_string(),
        plan: Some(".forge/ci.sh"),
    });
}

/// The 4096-byte cut lands inside a two-byte character: the partial
/// character is dropped, never stored as a corrupt sequence.
#[test]
fn tail_cut_never_splits_a_character() {
    let tail = format!("{}\n", "é".repeat(2047));
    assert_eq!(tail.len(), 4095);
    run_case(Case {
        name: "straddle",
        files: ci_sh(
            "#!/bin/bash\nfor i in $(seq 1 3000); do printf '\\xc3\\xa9'; done\nprintf '\\n'\nexit 1\n",
        ),
        env: vec![],
        failure: format!("{SH}: exited with status 1"),
        reason: "nonzero_exit",
        tail,
        plan: Some(".forge/ci.sh"),
    });
}

/// Invalid UTF-8 decodes to U+FFFD and control bytes stay exact in stored and
/// JSON data; the terminal line escapes them.
#[test]
fn hostile_bytes_are_exact_in_data_and_escaped_on_terminal() {
    run_case(Case {
        name: "hostile",
        files: ci_sh("#!/bin/bash\nprintf '\\x80\\xff ok \\033[31mred\\x00nul\\n'\nexit 1\n"),
        env: vec![],
        failure: format!("{SH}: exited with status 1"),
        reason: "nonzero_exit",
        tail: "\u{fffd}\u{fffd} ok \u{1b}[31mred\u{0}nul\n".into(),
        plan: Some(".forge/ci.sh"),
    });
}

/// U+FFFD takes three bytes, so 4096 invalid bytes would decode to 12 KiB;
/// the stored tail is still at most 4096 bytes.
#[test]
fn replacement_characters_stay_within_the_byte_bound() {
    let tail = "\u{fffd}".repeat(1365);
    assert_eq!(tail.len(), 4095);
    run_case(Case {
        name: "flood",
        files: ci_sh("#!/bin/bash\nfor i in $(seq 1 5000); do printf '\\xff'; done\nexit 1\n"),
        env: vec![],
        failure: format!("{SH}: exited with status 1"),
        reason: "nonzero_exit",
        tail,
        plan: Some(".forge/ci.sh"),
    });
}

/// A passing plan that locks its worktree: removal fails, the Check is
/// `failed` with reason `cleanup_failed` (F-025), and the leftover is named.
#[test]
fn worktree_cleanup_failure_is_cleanup_failed() {
    let repo = Repo::new("leftover");
    repo.pr_with(&ci_sh(
        "#!/bin/bash\necho locking\ngit worktree lock --reason held \"$PWD\" || exit 9\nexit 0\n",
    ));
    let output = repo.forge(&["ci", "run", "1", "--json"], &[]);
    let stderr = String::from_utf8(output.stderr.clone()).unwrap();
    let prefix = "git-forge: CI run recorded failed but temp worktree cleanup failed: \
                  temp worktree removal failed: ";
    let suffix = "; reason: cleanup_failed; output tail: locking\\x0a\n";
    assert!(stderr.starts_with(prefix), "{stderr}");
    assert!(stderr.ends_with(suffix), "{stderr}");
    assert!(stderr.contains("(worktree left at "), "{stderr}");
    let message = stderr["git-forge: ".len()..stderr.len() - 1].to_string();
    assert_ci_failure(
        &repo,
        &output,
        &Expected {
            message,
            hint: CLEANUP_HINT,
            reason: "cleanup_failed",
            tail: "locking\n".into(),
            plan: Some(".forge/ci.sh"),
        },
    );
    let leftovers = repo.ci_worktrees();
    assert_eq!(leftovers.len(), 1, "{leftovers:?}");
    for path in leftovers {
        repo.git(&["worktree", "remove", "-f", "-f", &path]);
    }
}

/// A plan-controlled multi-line lock reason with terminal controls reaches
/// the cleanup diagnostic through git's stderr. The human line and the JSON
/// `message` are one terminal-safe line; `data` and the stored Check keep
/// the exact reason and tail.
#[test]
fn hostile_worktree_lock_reason_is_sanitized_in_the_message() {
    let repo = Repo::new("hostile-lock");
    repo.pr_with(&ci_sh(
        "#!/bin/bash\necho locking\n\
         git worktree lock --reason $'held\\nforged line\\e[31mred\\x7f\\r' \"$PWD\" || exit 9\n\
         exit 0\n",
    ));
    let output = repo.forge(&["ci", "run", "1", "--json"], &[]);
    let stderr = String::from_utf8(output.stderr.clone()).unwrap();
    let line = stderr.strip_suffix('\n').unwrap_or(&stderr);
    let unsafe_chars: Vec<char> = line
        .chars()
        .filter(|c| matches!(*c as u32, 0..=0x1f | 0x7f..=0x9f))
        .collect();
    assert!(unsafe_chars.is_empty(), "{unsafe_chars:?} in {stderr:?}");
    // git renders the reason's other controls as `?`; its newlines and the
    // multi-line diagnostic itself are escaped by git-forge.
    assert!(
        line.contains("lock reason: held\\x0aforged line"),
        "{stderr}"
    );
    assert!(
        line.ends_with("; reason: cleanup_failed; output tail: locking\\x0a"),
        "{stderr}"
    );
    let message = line["git-forge: ".len()..].to_string();
    assert_ci_failure(
        &repo,
        &output,
        &Expected {
            message,
            hint: CLEANUP_HINT,
            reason: "cleanup_failed",
            tail: "locking\n".into(),
            plan: Some(".forge/ci.sh"),
        },
    );
    for path in repo.ci_worktrees() {
        repo.git(&["worktree", "unlock", &path]);
        repo.git(&["worktree", "remove", "-f", &path]);
    }
}
