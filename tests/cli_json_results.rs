//! JSON success results for commands that accept `--json`, at the built CLI
//! boundary. Each test parses stdout independently with `serde_json` and checks
//! the exact key set, so an extra, missing, or misnamed field fails.
use std::path::PathBuf;
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT_REPO: AtomicU64 = AtomicU64::new(0);

struct Repo(PathBuf);
impl Repo {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "gf-json-results-{}-{}",
            std::process::id(),
            NEXT_REPO.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&path).unwrap();
        let repo = Self(path);
        repo.git(&["init", "-q"]);
        repo.git(&["config", "user.name", "Test"]);
        repo.git(&["config", "user.email", "test@example.com"]);
        repo
    }
    /// A path beside the repository, so a linked worktree there leaves the
    /// base worktree clean.
    fn worktree_path(&self) -> PathBuf {
        PathBuf::from(format!("{}-wt", self.0.display()))
    }
    fn git(&self, args: &[&str]) -> String {
        let output = Command::new("git")
            .args(args)
            .current_dir(&self.0)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
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
        assert!(
            output.status.success(),
            "{args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).unwrap()
    }
    fn commit_file(&self, name: &str, content: &str) {
        let path = self.0.join(name);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, content).unwrap();
        self.git(&["add", name]);
        self.git(&["commit", "-q", "-m", name]);
    }
    fn rev(&self, name: &str) -> String {
        self.git(&["rev-parse", name])
    }
    /// One open Issue, #1.
    fn issue(&self) {
        self.ok(&["issue", "new", "an issue"]);
    }
    /// `main` with a base commit and `feature` with one commit plus a passing
    /// CI plan; PR #1 from `feature` into `main`.
    fn pr(&self) {
        self.commit_file("base.txt", "base\n");
        self.git(&["branch", "-M", "main"]);
        self.git(&["checkout", "-q", "-b", "feature"]);
        self.commit_file("feature.txt", "feature\n");
        self.commit_file(".forge/ci.sh", "#!/bin/sh\nexit 0\n");
        self.git(&["checkout", "-q", "main"]);
        self.ok(&[
            "pr", "create", "--source", "feature", "--base", "main", "a PR",
        ]);
    }
}
impl Drop for Repo {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
        let _ = std::fs::remove_dir_all(self.worktree_path());
    }
}

/// Exit 0, empty stderr, one stdout line that parses as a JSON object whose
/// keys are exactly `keys` (in sorted order, as `serde_json` maps them).
fn json_result(output: &Output, keys: &[&str]) -> serde_json::Map<String, serde_json::Value> {
    assert_eq!(
        output.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stderr.is_empty());
    let stdout = String::from_utf8(output.stdout.clone()).unwrap();
    assert_eq!(stdout.lines().count(), 1, "one JSON document: {stdout}");
    let document: serde_json::Value = serde_json::from_str(&stdout).expect("valid JSON");
    let object = document.as_object().expect("JSON object").clone();
    let mut expected: Vec<&str> = keys.to_vec();
    expected.sort_unstable();
    assert_eq!(
        object.keys().map(String::as_str).collect::<Vec<_>>(),
        expected
    );
    object
}

/// The tip commit of a forge ref: the event commit a mutation appended.
fn tip(repo: &Repo, reference: &str) -> serde_json::Value {
    serde_json::Value::String(repo.rev(reference))
}

#[test]
fn issue_close_and_reopen_print_id_state_and_event() {
    let repo = Repo::new();
    repo.issue();
    let closed = json_result(
        &repo.run(&["issue", "close", "1", "--json"]),
        &["id", "state", "event"],
    );
    assert_eq!(closed["id"], 1);
    assert_eq!(closed["state"], "closed");
    assert_eq!(closed["event"], tip(&repo, "refs/forge/issues/1"));
    let reopened = json_result(
        &repo.run(&["issue", "reopen", "--json", "1"]),
        &["id", "state", "event"],
    );
    assert_eq!(reopened["id"], 1);
    assert_eq!(reopened["state"], "open");
    assert_eq!(reopened["event"], tip(&repo, "refs/forge/issues/1"));
}

#[test]
fn issue_comment_reports_the_resulting_state_of_the_issue() {
    let repo = Repo::new();
    repo.issue();
    repo.ok(&["issue", "close", "1"]);
    let result = json_result(
        &repo.run(&["issue", "comment", "1", "--json", "a", "note"]),
        &["id", "state", "event"],
    );
    assert_eq!(result["id"], 1);
    assert_eq!(result["state"], "closed");
    assert_eq!(result["event"], tip(&repo, "refs/forge/issues/1"));
    let shown: serde_json::Value =
        serde_json::from_str(&repo.ok(&["issue", "show", "1", "--json"])).unwrap();
    assert_eq!(shown["comments"], serde_json::json!(["a note"]));
}

#[test]
fn pr_comment_prints_id_state_and_event() {
    let repo = Repo::new();
    repo.pr();
    let result = json_result(
        &repo.run(&["pr", "comment", "1", "looks", "good", "--json"]),
        &["id", "state", "event"],
    );
    assert_eq!(result["id"], 1);
    assert_eq!(result["state"], "open");
    assert_eq!(result["event"], tip(&repo, "refs/forge/prs/1/head"));
}

#[test]
fn pr_abandon_prints_abandoned_state_reason_and_event() {
    let repo = Repo::new();
    repo.pr();
    let result = json_result(
        &repo.run(&["pr", "abandon", "--json", "1", "--reason", "superseded"]),
        &["id", "state", "reason", "event"],
    );
    assert_eq!(result["id"], 1);
    assert_eq!(result["state"], "abandoned");
    assert_eq!(result["reason"], "superseded");
    assert_eq!(result["event"], tip(&repo, "refs/forge/prs/1/head"));
}

#[test]
fn pr_review_prints_decision_generation_candidate_and_event() {
    let repo = Repo::new();
    repo.pr();
    let keys = [
        "id",
        "state",
        "decision",
        "generation",
        "candidate",
        "event",
    ];
    let rejected = json_result(
        &repo.run(&["pr", "review", "1", "--reject", "--json"]),
        &keys,
    );
    assert_eq!(rejected["id"], 1);
    assert_eq!(rejected["state"], "open");
    assert_eq!(rejected["decision"], "reject");
    assert_eq!(rejected["generation"], 0);
    assert_eq!(rejected["candidate"], serde_json::Value::Null);
    assert_eq!(rejected["event"], tip(&repo, "refs/forge/prs/1/head"));
    repo.ok(&["pr", "prepare", "1", "--strategy", "merge"]);
    let approved = json_result(
        &repo.run(&["pr", "review", "--json", "1", "--approve"]),
        &keys,
    );
    assert_eq!(approved["decision"], "approve");
    assert_eq!(approved["candidate"], tip(&repo, "refs/forge/prs/1/result"));
    assert_eq!(approved["event"], tip(&repo, "refs/forge/prs/1/head"));
}

#[test]
fn pr_prepare_prints_candidate_identity_and_event() {
    let repo = Repo::new();
    repo.pr();
    let result = json_result(
        &repo.run(&["pr", "prepare", "1", "--json", "--strategy", "squash"]),
        &[
            "id",
            "state",
            "strategy",
            "generation",
            "candidate",
            "result_tree",
            "base_head",
            "event",
        ],
    );
    assert_eq!(result["id"], 1);
    assert_eq!(result["state"], "open");
    assert_eq!(result["strategy"], "squash");
    assert_eq!(result["generation"], 0);
    let candidate = repo.rev("refs/forge/prs/1/result");
    assert_eq!(result["candidate"], candidate.as_str());
    assert_eq!(
        result["result_tree"],
        repo.rev(&format!("{candidate}^{{tree}}")).as_str()
    );
    assert_eq!(result["base_head"], tip(&repo, "main"));
    assert_eq!(result["event"], tip(&repo, "refs/forge/prs/1/head"));
}

#[test]
fn pr_update_prints_new_generation_snapshot_and_event() {
    let repo = Repo::new();
    repo.pr();
    repo.git(&["checkout", "-q", "feature"]);
    repo.commit_file("more.txt", "more\n");
    repo.git(&["checkout", "-q", "main"]);
    let result = json_result(
        &repo.run(&["pr", "update", "--json", "1"]),
        &[
            "id",
            "state",
            "generation",
            "source_head",
            "base_head",
            "merge_base",
            "event",
        ],
    );
    assert_eq!(result["id"], 1);
    assert_eq!(result["state"], "open");
    assert_eq!(result["generation"], 1);
    assert_eq!(result["source_head"], tip(&repo, "feature"));
    assert_eq!(result["base_head"], tip(&repo, "main"));
    assert_eq!(result["merge_base"], tip(&repo, "main"));
    assert_eq!(result["event"], tip(&repo, "refs/forge/prs/1/head"));
}

#[test]
fn pr_diff_prints_base_head_and_diff_text() {
    let repo = Repo::new();
    repo.pr();
    let plain = repo.ok(&["pr", "diff", "1"]);
    let result = json_result(
        &repo.run(&["pr", "diff", "--json", "1"]),
        &["id", "state", "generation", "base", "head", "diff"],
    );
    assert_eq!(result["id"], 1);
    assert_eq!(result["state"], "open");
    assert_eq!(result["generation"], 0);
    assert_eq!(result["base"], tip(&repo, "main"));
    assert_eq!(result["head"], tip(&repo, "feature"));
    let diff = result["diff"].as_str().unwrap();
    assert!(diff.contains("+++ b/feature.txt"), "{diff}");
    assert_eq!(format!("{diff}\n"), plain);
}

const MERGE_KEYS: [&str; 7] = [
    "id",
    "state",
    "strategy",
    "base",
    "generation",
    "merge_commit",
    "recycle",
];

#[test]
fn pr_merge_prints_merged_state_and_merge_commit() {
    let repo = Repo::new();
    repo.pr();
    repo.ok(&["ci", "run", "1"]);
    repo.ok(&["pr", "review", "1", "--approve"]);
    let result = json_result(&repo.run(&["pr", "merge", "1", "--json"]), &MERGE_KEYS);
    assert_eq!(result["id"], 1);
    assert_eq!(result["state"], "merged");
    assert_eq!(result["strategy"], "merge");
    assert_eq!(result["base"], "main");
    assert_eq!(result["generation"], 0);
    assert_eq!(result["merge_commit"], tip(&repo, "main"));
    assert_eq!(result["recycle"], serde_json::Value::Null);
    let shown: serde_json::Value =
        serde_json::from_str(&repo.ok(&["pr", "show", "1", "--json"])).unwrap();
    assert_eq!(shown["merged"], result["merge_commit"]);
}

#[test]
fn pr_merge_of_a_prepared_candidate_reports_the_recycle_outcome() {
    let repo = Repo::new();
    repo.pr();
    repo.ok(&["pr", "prepare", "1", "--strategy", "squash"]);
    let candidate = repo.rev("refs/forge/prs/1/result");
    repo.ok(&["ci", "run", "1"]);
    repo.ok(&["pr", "review", "1", "--approve"]);
    let worktree = repo.worktree_path();
    repo.git(&[
        "worktree",
        "add",
        "-q",
        worktree.to_str().unwrap(),
        "feature",
    ]);
    let result = json_result(
        &repo.run(&[
            "pr",
            "merge",
            "1",
            "--squash",
            "--recycle-worktree",
            worktree.to_str().unwrap(),
            "--json",
        ]),
        &MERGE_KEYS,
    );
    assert_eq!(result["state"], "merged");
    assert_eq!(result["strategy"], "squash");
    assert_eq!(result["merge_commit"], candidate.as_str());
    assert_eq!(result["merge_commit"], tip(&repo, "main"));
    let recycle = result["recycle"].as_str().unwrap();
    assert!(
        recycle.starts_with("recycle outcome: removed; branch-kept(feature"),
        "{recycle}"
    );
    assert!(!worktree.exists());
}

/// `--json` with a help request is a usage error, never prose on stdout: help
/// has no JSON result. Covers every command that accepts `--json`, with the
/// help flag before and after `--json`, plus the namespace help words.
#[test]
fn json_with_help_is_a_usage_error_for_every_json_command() {
    let repo = Repo::new();
    let commands: &[&[&str]] = &[
        &["issue", "new"],
        &["issue", "apply"],
        &["issue", "list"],
        &["issue", "show", "1"],
        &["issue", "comment", "1"],
        &["issue", "close", "1"],
        &["issue", "reopen", "1"],
        &["pr", "create"],
        &["pr", "list"],
        &["pr", "show", "1"],
        &["pr", "comment", "1"],
        &["pr", "review", "1"],
        &["pr", "update", "1"],
        &["pr", "prepare", "1"],
        &["pr", "diff", "1"],
        &["pr", "merge"],
        &["pr", "merge", "1"],
        &["pr", "abandon", "1"],
        &["ci", "run", "1"],
    ];
    let mut cases: Vec<Vec<&str>> = Vec::new();
    for command in commands {
        for flag in ["--help", "-h"] {
            for tail in [["--json", flag], [flag, "--json"]] {
                cases.push(command.iter().copied().chain(tail).collect());
                // Help directly after the subcommand takes the dispatcher's
                // early help path.
                let mut early = command[..2].to_vec();
                early.extend(tail);
                cases.push(early);
            }
        }
    }
    for namespace_help in [
        vec!["help", "--json"],
        vec!["--json", "--help"],
        vec!["forge", "help", "--json"],
        vec!["issue", "help", "--json"],
        vec!["pr", "--help", "--json"],
        vec!["ci", "help", "--json"],
    ] {
        cases.push(namespace_help);
    }
    for args in cases {
        let output = repo.run(&args);
        assert_eq!(output.status.code(), Some(1), "{args:?}");
        let stdout = String::from_utf8(output.stdout).unwrap();
        assert_eq!(stdout.lines().count(), 1, "{args:?}: {stdout}");
        let document: serde_json::Value =
            serde_json::from_str(&stdout).unwrap_or_else(|e| panic!("{args:?}: {e}: {stdout}"));
        assert_eq!(document["error"]["code"], "E_USAGE", "{args:?}");
        assert_eq!(
            String::from_utf8_lossy(&output.stderr),
            format!(
                "git-forge: {}\n",
                document["error"]["message"].as_str().unwrap()
            )
        );
    }
    // Nothing was written: no forge refs exist.
    assert_eq!(repo.git(&["for-each-ref", "refs/forge"]), "");
    // A literal `--help` after `--` is data, not a help request.
    let created = json_result(
        &repo.run(&["issue", "new", "--json", "--", "--help"]),
        &[
            "id",
            "title",
            "description",
            "labels",
            "open",
            "comments",
            "key",
            "parent",
            "blocked_by",
            "children",
            "blocked",
        ],
    );
    assert_eq!(created["title"], "--help");
}

/// Names of every ref under `refs/forge` plus the branch heads, so two
/// repositories with the same history compare equal regardless of commit ids.
fn ref_names(repo: &Repo) -> String {
    repo.git(&[
        "for-each-ref",
        "--format=%(refname)",
        "refs/forge",
        "refs/heads",
    ])
}

/// A `-h`/`--help` token consumed as the value of a value-taking option is
/// data, not a help request: under `--json` the command behaves exactly as
/// without it (same exit status, same failure message, same refs written).
/// The run without `--json` keeps the pre-JSON behavior, so it is the oracle.
#[test]
fn help_tokens_consumed_as_option_values_are_data_under_json() {
    type Setup = fn(&Repo);
    let none: Setup = |_| {};
    let pr: Setup = |repo| repo.pr();
    let cases: &[(Setup, &[&str])] = &[
        (none, &["issue", "new", "t", "--label", "--help"]),
        (none, &["issue", "new", "t", "-l", "-h"]),
        (none, &["issue", "new", "t", "--parent", "--help"]),
        (none, &["issue", "new", "t", "--blocked-by", "-h"]),
        (none, &["issue", "list", "--parent", "--help"]),
        (none, &["issue", "list", "--state", "-h"]),
        (
            pr,
            &[
                "pr", "create", "--source", "feature", "--base", "main", "t", "--body", "--help",
            ],
        ),
        (
            pr,
            &[
                "pr", "create", "--source", "feature", "--base", "main", "t", "--label", "-h",
            ],
        ),
        (
            pr,
            &[
                "pr", "create", "--source", "feature", "--base", "main", "t", "--closes", "--help",
            ],
        ),
        (
            pr,
            &["pr", "create", "--source", "--help", "--base", "main", "t"],
        ),
        (
            pr,
            &["pr", "create", "--source", "feature", "--base", "-h", "t"],
        ),
        (
            pr,
            &[
                "pr",
                "review",
                "1",
                "--approve",
                "--file",
                "--help",
                "--line",
                "1",
                "--commit",
                "main",
            ],
        ),
        (
            pr,
            &[
                "pr",
                "review",
                "1",
                "--approve",
                "--file",
                "f",
                "--line",
                "-h",
                "--commit",
                "main",
            ],
        ),
        (
            pr,
            &[
                "pr",
                "review",
                "1",
                "--approve",
                "--file",
                "f",
                "--line",
                "1",
                "--commit",
                "--help",
            ],
        ),
        (pr, &["pr", "abandon", "1", "--reason", "--help"]),
        (pr, &["pr", "abandon", "1", "--reason", "-h"]),
        (pr, &["pr", "merge", "1", "--recycle-worktree", "--help"]),
    ];
    for (setup, args) in cases {
        let plain_repo = Repo::new();
        setup(&plain_repo);
        let plain = plain_repo.run(args);
        let json_repo = Repo::new();
        setup(&json_repo);
        let mut json_args = args[..2].to_vec();
        json_args.push("--json");
        json_args.extend(&args[2..]);
        let json = json_repo.run(&json_args);
        assert_eq!(json.status.code(), plain.status.code(), "{json_args:?}");
        let stdout = String::from_utf8(json.stdout).unwrap();
        let document: serde_json::Value = serde_json::from_str(&stdout)
            .unwrap_or_else(|e| panic!("{json_args:?}: {e}: {stdout}"));
        if plain.status.success() {
            assert!(document.get("error").is_none(), "{json_args:?}: {stdout}");
        } else {
            assert_eq!(json.stderr, plain.stderr, "{json_args:?}");
            let message = String::from_utf8_lossy(&plain.stderr);
            assert_eq!(
                document["error"]["message"]
                    .as_str()
                    .map(|m| format!("git-forge: {m}\n")),
                Some(message.into_owned()),
                "{json_args:?}"
            );
        }
        assert_eq!(
            ref_names(&json_repo),
            ref_names(&plain_repo),
            "{json_args:?}"
        );
    }
    // The reported regression: the label value `--help` is recorded.
    let repo = Repo::new();
    let created: serde_json::Value =
        serde_json::from_str(&repo.ok(&["issue", "new", "title", "--json", "--label", "--help"]))
            .unwrap();
    assert_eq!(created["labels"], serde_json::json!(["--help"]));
    // `pr prepare` treats any help token as help without `--json`; under
    // `--json`, `--strategy --help` is a strategy value and is rejected as one.
    let repo = Repo::new();
    repo.pr();
    let before = ref_names(&repo);
    let output = repo.run(&["pr", "prepare", "1", "--json", "--strategy", "--help"]);
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(
        String::from_utf8_lossy(&output.stderr),
        "git-forge: unknown strategy '--help'; use --strategy merge|squash|rebase|ff\n"
    );
    assert_eq!(ref_names(&repo), before);
}
