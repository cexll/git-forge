//! `--idempotency-key` on `issue new`, `pr create`, `issue comment` and
//! `pr comment`, at the built CLI boundary. Every case asserts the exit code,
//! stdout, stderr, the complete JSON document, and the exact set of forge refs
//! after the command.
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT_REPO: AtomicU64 = AtomicU64::new(0);

struct Repo(PathBuf);
impl Repo {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "gf-idempotency-{}-{}",
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
    fn command(&self, args: &[&str]) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_git-forge"));
        command
            .args(args)
            .current_dir(&self.0)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env_remove("GIT_FORGE_ROLE")
            .env_remove("GIT_FORGE_TEST_WRITE_BARRIER");
        command
    }
    fn run(&self, args: &[&str]) -> Output {
        self.command(args).output().unwrap()
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
        std::fs::write(self.0.join(name), content).unwrap();
        self.git(&["add", name]);
        self.git(&["commit", "-q", "-m", name]);
    }
    /// Every ref under `refs/forge/` with its target, in name order.
    fn forge_refs(&self) -> Vec<(String, String)> {
        self.git(&[
            "for-each-ref",
            "--format=%(refname) %(objectname)",
            "refs/forge/",
        ])
        .lines()
        .map(|line| {
            let (name, oid) = line.split_once(' ').unwrap();
            (name.to_string(), oid.to_string())
        })
        .collect()
    }
    fn forge_ref_names(&self) -> Vec<String> {
        self.forge_refs()
            .into_iter()
            .map(|(name, _)| name)
            .collect()
    }
    /// `main` with one commit and `feature` (plus `other`) one commit ahead.
    fn branches(&self) {
        self.commit_file("base.txt", "base\n");
        self.git(&["branch", "-M", "main"]);
        self.git(&["checkout", "-q", "-b", "feature"]);
        self.commit_file("feature.txt", "feature\n");
        self.git(&["checkout", "-q", "-b", "other", "main"]);
        self.commit_file("other.txt", "other\n");
        self.git(&["checkout", "-q", "main"]);
    }
}
impl Drop for Repo {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn stdout(output: &Output) -> String {
    String::from_utf8(output.stdout.clone()).unwrap()
}
fn stderr(output: &Output) -> String {
    String::from_utf8(output.stderr.clone()).unwrap()
}

/// Exit 0 and empty stderr; returns stdout.
fn success(output: &Output) -> String {
    assert_eq!(output.status.code(), Some(0), "stderr: {}", stderr(output));
    assert_eq!(stderr(output), "");
    stdout(output)
}

/// The lowercase hex of the key bytes: the key's ref-name component.
fn hex(key: &str) -> String {
    key.bytes().map(|b| format!("{b:02x}")).collect()
}

fn record_ref(command: &str, key: &str) -> String {
    format!("refs/forge/idempotency/{command}/{}", hex(key))
}

fn names(list: &[&str]) -> Vec<String> {
    let mut out: Vec<String> = list.iter().map(|s| s.to_string()).collect();
    out.sort();
    out
}

fn json(text: &str) -> serde_json::Value {
    serde_json::from_str(text.trim_end()).expect("valid JSON")
}

// ───────────────────────────── issue new ─────────────────────────────

#[test]
fn issue_new_replay_returns_the_first_result_and_writes_nothing() {
    let repo = Repo::new();
    let first = success(&repo.run(&["issue", "new", "t", "--idempotency-key", "k1", "--json"]));
    let expected = json(&first);
    assert_eq!(expected["id"], 1);
    assert_eq!(expected["title"], "t");
    let refs = repo.forge_refs();
    assert_eq!(
        repo.forge_ref_names(),
        names(&[
            "refs/forge/issues/1",
            "refs/forge/meta/counter",
            &record_ref("issue-new", "k1"),
        ])
    );
    let replay = success(&repo.run(&["issue", "new", "t", "--idempotency-key", "k1", "--json"]));
    assert_eq!(replay, first);
    assert_eq!(repo.forge_refs(), refs);
    let list = json(&repo.ok(&["issue", "list", "--json"]));
    assert_eq!(list.as_array().unwrap().len(), 1);
}

// ──────────────────────── pr create and comments ────────────────────────

#[test]
fn pr_create_replay_returns_the_first_result_and_writes_nothing() {
    let repo = Repo::new();
    repo.branches();
    let args = [
        "pr",
        "create",
        "--source",
        "feature",
        "--base",
        "main",
        "a PR",
        "--idempotency-key",
        "p1",
    ];
    let first = success(&repo.run(&[&args[..], &["--json"]].concat()));
    assert_eq!(json(&first)["id"], 1);
    let refs = repo.forge_refs();
    assert_eq!(
        repo.forge_ref_names(),
        names(&[
            "refs/forge/meta/counter",
            "refs/forge/prs/1/base",
            "refs/forge/prs/1/head",
            "refs/forge/prs/1/meta",
            "refs/forge/prs/1/source",
            &record_ref("pr-create", "p1"),
        ])
    );
    assert_eq!(
        success(&repo.run(&[&args[..], &["--json"]].concat())),
        first
    );
    assert_eq!(
        success(&repo.run(&args)),
        "PR #1 created: a PR (feature -> main)\n"
    );
    assert_eq!(repo.forge_refs(), refs);
}

#[test]
fn comment_replay_returns_the_first_result_and_writes_one_comment() {
    let repo = Repo::new();
    repo.branches();
    repo.ok(&["issue", "new", "an issue"]);
    repo.ok(&[
        "pr", "create", "--source", "feature", "--base", "main", "a PR",
    ]);
    for (entity, head, kind) in [
        ("issue", "refs/forge/issues/1", "issue-comment"),
        ("pr", "refs/forge/prs/2/head", "pr-comment"),
    ] {
        let id = if entity == "issue" { "1" } else { "2" };
        let args = [
            entity,
            "comment",
            id,
            "hello",
            "--idempotency-key",
            "c1",
            "--json",
        ];
        let first = success(&repo.run(&args));
        let event = repo.git(&["rev-parse", head]);
        assert_eq!(
            json(&first),
            serde_json::json!({"id": id.parse::<u64>().unwrap(), "state": "open", "event": event})
        );
        let refs = repo.forge_refs();
        assert!(refs.iter().any(|(name, _)| *name == record_ref(kind, "c1")));
        assert_eq!(success(&repo.run(&args)), first);
        assert_eq!(repo.forge_refs(), refs);
        let shown = repo.ok(&[entity, "show", id]);
        assert_eq!(shown.matches("hello").count(), 1, "{shown}");
    }
}

// ───────────────────────────── case tables ─────────────────────────────

const HINT: &str =
    "Repeat the original inputs to read the recorded result, or use a new idempotency key.";

/// The exact `E_CONFLICT` document for a key whose record names `entity #id`.
fn conflict_json(key: &str, command: &str, entity: &str, id: u64) -> serde_json::Value {
    serde_json::json!({"error": {
        "code": "E_CONFLICT",
        "message": format!("idempotency key '{key}' was already used by {command} with different inputs ({entity} #{id})"),
        "input": key,
        "hint": HINT,
        "retryable": false,
        "data": {"command": command, "key": key, "id": id},
    }})
}

#[derive(Clone, Copy)]
enum Outcome {
    /// Exit 0 with the first result in both output modes.
    Replay,
    /// Exit 1 with `E_CONFLICT` naming the recorded `entity #id`.
    Conflict,
}

/// Run `first` once with `--json`, then `second` with and without `--json`,
/// and assert `outcome` plus an unchanged forge ref set after `second`.
fn check_case(
    repo: &Repo,
    case: &str,
    (first, second): (&[&str], &[&str]),
    (command, entity, text): (&str, &str, &str),
    outcome: Outcome,
) {
    let first_json = success(&repo.run(&[first, &["--json"]].concat()));
    let id = json(&first_json)["id"].as_u64().unwrap();
    let refs = repo.forge_refs();
    let with_json = repo.run(&[second, &["--json"]].concat());
    let without = repo.run(second);
    match outcome {
        Outcome::Replay => {
            assert_eq!(success(&with_json), first_json, "{case}");
            assert_eq!(success(&without), format!("{text}\n"), "{case}");
        }
        Outcome::Conflict => {
            let expected = conflict_json("k", command, entity, id);
            let message = expected["error"]["message"].as_str().unwrap().to_string();
            for (output, printed) in [(&with_json, true), (&without, false)] {
                assert_eq!(output.status.code(), Some(1), "{case}");
                assert_eq!(stderr(output), format!("git-forge: {message}\n"), "{case}");
                if printed {
                    assert_eq!(json(&stdout(output)), expected, "{case}");
                    assert_eq!(stdout(output).lines().count(), 1, "{case}");
                } else {
                    assert_eq!(stdout(output), "", "{case}");
                }
            }
        }
    }
    assert_eq!(
        repo.forge_refs(),
        refs,
        "{case}: the second call wrote refs"
    );
}

/// `base` with every token equal to a `from` replaced by its `to`.
fn swap(base: &[&'static str], pairs: &[(&str, &'static str)]) -> Vec<&'static str> {
    base.iter()
        .map(|arg| {
            pairs
                .iter()
                .find(|(from, _)| from == arg)
                .map_or(*arg, |(_, to)| *to)
        })
        .collect()
}

/// `base` without the `count` tokens that start at the token `from`.
fn cut(base: &[&'static str], from: &str, count: usize) -> Vec<&'static str> {
    let at = base.iter().position(|arg| *arg == from).unwrap();
    [&base[..at], &base[at + count..]].concat()
}

#[test]
fn issue_new_same_key_replays_identical_inputs_and_rejects_each_changed_field() {
    let base = [
        "issue",
        "new",
        "t",
        "d",
        "--label",
        "x",
        "--label",
        "y",
        "--blocked-by",
        "1",
        "--blocked-by",
        "2",
        "--idempotency-key",
        "k",
    ];
    let cases: Vec<(&str, Vec<&str>, Outcome)> = vec![
        ("identical", base.to_vec(), Outcome::Replay),
        (
            "blocked-by in another order",
            swap(&base, &[("1", "2"), ("2", "1")]),
            Outcome::Replay,
        ),
        ("title", swap(&base, &[("t", "u")]), Outcome::Conflict),
        ("description", swap(&base, &[("d", "e")]), Outcome::Conflict),
        ("description removed", cut(&base, "d", 1), Outcome::Conflict),
        ("labels", cut(&base, "x", 2), Outcome::Conflict),
        (
            "label order",
            swap(&base, &[("x", "y"), ("y", "x")]),
            Outcome::Conflict,
        ),
        (
            "parent",
            [&base[..], &["--parent", "1"]].concat(),
            Outcome::Conflict,
        ),
        ("blocked-by", cut(&base, "1", 2), Outcome::Conflict),
    ];
    for (case, second, outcome) in &cases {
        let repo = Repo::new();
        repo.ok(&["issue", "new", "one"]);
        repo.ok(&["issue", "new", "two"]);
        check_case(
            &repo,
            case,
            (&base, second),
            ("issue new", "issue", "issue #3 created: t"),
            *outcome,
        );
        let list = json(&repo.ok(&["issue", "list", "--json"]));
        assert_eq!(list.as_array().unwrap().len(), 3, "{case}");
    }
}

#[test]
fn pr_create_same_key_replays_identical_inputs_and_rejects_each_changed_field() {
    let base = [
        "pr",
        "create",
        "--source",
        "feature",
        "--base",
        "main",
        "a PR",
        "--body",
        "b",
        "--label",
        "x",
        "--closes",
        "1",
        "--idempotency-key",
        "k",
    ];
    let with = |pairs: &[(&str, &'static str)]| swap(&base, pairs);
    let without = |flag: &str| cut(&base, flag, 2);
    let cases: Vec<(&str, Vec<&str>, Outcome)> = vec![
        ("identical", base.to_vec(), Outcome::Replay),
        (
            "full branch refs",
            with(&[
                ("feature", "refs/heads/feature"),
                ("main", "refs/heads/main"),
            ]),
            Outcome::Replay,
        ),
        ("title", with(&[("a PR", "b PR")]), Outcome::Conflict),
        ("source", with(&[("feature", "other")]), Outcome::Conflict),
        ("base", with(&[("main", "other")]), Outcome::Conflict),
        ("body", with(&[("b", "c")]), Outcome::Conflict),
        ("body removed", without("--body"), Outcome::Conflict),
        ("labels", with(&[("x", "z")]), Outcome::Conflict),
        ("labels removed", without("--label"), Outcome::Conflict),
        ("closes", with(&[("1", "2")]), Outcome::Conflict),
        ("closes removed", without("--closes"), Outcome::Conflict),
    ];
    for (case, second, outcome) in &cases {
        let repo = Repo::new();
        repo.branches();
        repo.ok(&["issue", "new", "one"]);
        repo.ok(&["issue", "new", "two"]);
        check_case(
            &repo,
            case,
            (&base, second),
            ("pr create", "PR", "PR #3 created: a PR (feature -> main)"),
            *outcome,
        );
        let list = json(&repo.ok(&["pr", "list", "--json"]));
        assert_eq!(list.as_array().unwrap().len(), 1, "{case}");
    }
}

#[test]
fn comments_with_the_same_key_replay_identical_inputs_and_reject_changed_ones() {
    for (entity, command, label) in [
        ("issue", "issue comment", "issue"),
        ("pr", "pr comment", "PR"),
    ] {
        let base = [
            entity,
            "comment",
            "3",
            "hello world",
            "--idempotency-key",
            "k",
        ];
        let cases: &[(&str, Vec<&str>, Outcome)] = &[
            ("identical", base.to_vec(), Outcome::Replay),
            (
                "body as several words",
                vec![
                    entity,
                    "comment",
                    "3",
                    "hello",
                    "world",
                    "--idempotency-key",
                    "k",
                ],
                Outcome::Replay,
            ),
            (
                "body",
                vec![entity, "comment", "3", "bye", "--idempotency-key", "k"],
                Outcome::Conflict,
            ),
            (
                "target",
                vec![
                    entity,
                    "comment",
                    "4",
                    "hello world",
                    "--idempotency-key",
                    "k",
                ],
                Outcome::Conflict,
            ),
            (
                "key first",
                vec![
                    entity,
                    "comment",
                    "--idempotency-key",
                    "k",
                    "3",
                    "hello world",
                ],
                Outcome::Replay,
            ),
        ];
        for (case, second, outcome) in cases {
            let repo = Repo::new();
            repo.branches();
            // Issues #1-#4 and PRs #5-#6 would shift ids; keep the targets
            // at #3 and #4 for both entity kinds.
            if entity == "issue" {
                for title in ["a", "b", "c", "d"] {
                    repo.ok(&["issue", "new", title]);
                }
            } else {
                repo.ok(&["issue", "new", "a"]);
                repo.ok(&["issue", "new", "b"]);
                repo.ok(&["pr", "create", "--source", "feature", "--base", "main", "p"]);
                repo.ok(&["pr", "create", "--source", "other", "--base", "main", "q"]);
            }
            let case = format!("{command}: {case}");
            check_case(
                &repo,
                &case,
                (&base, second),
                (command, label, &format!("comment added to {label} #3")),
                *outcome,
            );
            let shown = repo.ok(&[entity, "show", "3"]);
            assert_eq!(shown.matches("hello").count(), 1, "{case}: {shown}");
        }
    }
}

#[test]
fn a_key_is_scoped_to_its_command_kind() {
    let repo = Repo::new();
    repo.branches();
    let issue = success(&repo.run(&["issue", "new", "t", "--idempotency-key", "k", "--json"]));
    let pr = success(&repo.run(&[
        "pr",
        "create",
        "--source",
        "feature",
        "--base",
        "main",
        "p",
        "--idempotency-key",
        "k",
        "--json",
    ]));
    assert_eq!(json(&issue)["id"], 1);
    assert_eq!(json(&pr)["id"], 2);
    assert_eq!(
        success(&repo.run(&["issue", "comment", "1", "c", "--idempotency-key", "k"])),
        "comment added to issue #1\n"
    );
    assert_eq!(
        success(&repo.run(&["pr", "comment", "2", "c", "--idempotency-key", "k"])),
        "comment added to PR #2\n"
    );
    let mut expected = vec![
        "refs/forge/issues/1".to_string(),
        "refs/forge/meta/counter".to_string(),
        "refs/forge/prs/2/base".to_string(),
        "refs/forge/prs/2/head".to_string(),
        "refs/forge/prs/2/meta".to_string(),
        "refs/forge/prs/2/source".to_string(),
    ];
    for kind in ["issue-new", "pr-create", "issue-comment", "pr-comment"] {
        expected.push(record_ref(kind, "k"));
    }
    expected.sort();
    assert_eq!(repo.forge_ref_names(), expected);
}

#[test]
fn replay_after_the_entity_changed_returns_the_original_result() {
    let repo = Repo::new();
    repo.branches();
    // issue new, first call in text mode: the recorded JSON is the issue as
    // created, so a later child does not appear in the replayed result.
    let created = ["issue", "new", "t", "--idempotency-key", "n"];
    assert_eq!(success(&repo.run(&created)), "issue #1 created: t\n");
    let as_created = repo.ok(&["issue", "show", "1", "--json"]);
    let comment = [
        "issue",
        "comment",
        "1",
        "c",
        "--idempotency-key",
        "c",
        "--json",
    ];
    let commented = success(&repo.run(&comment));
    let pr = [
        "pr",
        "create",
        "--source",
        "feature",
        "--base",
        "main",
        "p",
        "--idempotency-key",
        "p",
        "--json",
    ];
    let opened = success(&repo.run(&pr));
    assert_eq!(opened, repo.ok(&["pr", "show", "2", "--json"]));
    let pr_comment = [
        "pr",
        "comment",
        "2",
        "c",
        "--idempotency-key",
        "c",
        "--json",
    ];
    let pr_commented = success(&repo.run(&pr_comment));

    repo.ok(&["issue", "new", "child", "--parent", "1"]);
    repo.ok(&["issue", "close", "1"]);
    repo.git(&["checkout", "-q", "feature"]);
    repo.commit_file("later.txt", "later\n");
    repo.git(&["checkout", "-q", "main"]);
    repo.ok(&["pr", "abandon", "2", "--reason", "replaced"]);
    let refs = repo.forge_refs();

    assert_eq!(
        success(&repo.run(&[&created[..], &["--json"]].concat())),
        as_created
    );
    assert_eq!(json(&as_created)["children"], serde_json::json!([]));
    assert_eq!(json(&as_created)["open"], true);
    assert_eq!(success(&repo.run(&comment)), commented);
    assert_eq!(json(&commented)["state"], "open");
    assert_eq!(success(&repo.run(&pr)), opened);
    assert_eq!(success(&repo.run(&pr_comment)), pr_commented);
    assert_eq!(json(&pr_commented)["state"], "open");
    assert_eq!(repo.forge_refs(), refs);
}

// ───────────────────────────── concurrency ─────────────────────────────

/// Start `args` with a write barrier at `barrier` and wait until it parks
/// there: its record lookup has run and it has written nothing yet.
fn spawn_parked(repo: &Repo, args: &[&str], barrier: &Path) -> std::process::Child {
    let mut child = repo
        .command(args)
        .env("GIT_FORGE_TEST_WRITE_BARRIER", barrier)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    while !barrier.join("ready").exists() {
        assert!(
            child.try_wait().unwrap().is_none(),
            "{args:?} exited before the write barrier"
        );
        assert!(
            std::time::Instant::now() < deadline,
            "{args:?} never parked"
        );
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    child
}

/// Release a parked command and wait for it to exit.
fn release(child: std::process::Child, barrier: &Path) -> Output {
    std::fs::write(barrier.join("release"), "release").unwrap();
    child.wait_with_output().unwrap()
}

/// Two invocations with the same key both pass the record lookup before
/// either writes (both park at the write barrier). The first one released
/// commits; the second one's transaction fails on the record ref it expected
/// to be absent, and it prints the first one's result.
fn race(repo: &Repo, args: &[&str]) -> (String, String) {
    let (first, second) = race_then(repo, args, |_| {});
    (success(&first), success(&second))
}

/// [`race`] that runs `between` after the first call committed and before
/// the second one, parked past its record lookup, resumes.
fn race_then(repo: &Repo, args: &[&str], between: impl Fn(&Repo)) -> (Output, Output) {
    let a = repo.0.with_extension("barrier-a");
    let b = repo.0.with_extension("barrier-b");
    let first = spawn_parked(repo, args, &a);
    let second = spawn_parked(repo, args, &b);
    let first = release(first, &a);
    between(repo);
    let second = release(second, &b);
    let _ = std::fs::remove_dir_all(&a);
    let _ = std::fs::remove_dir_all(&b);
    (first, second)
}

#[test]
fn concurrent_same_key_calls_create_one_record_and_return_one_result() {
    let repo = Repo::new();
    repo.branches();
    let (first, second) = race(
        &repo,
        &["issue", "new", "t", "--idempotency-key", "k", "--json"],
    );
    assert_eq!(second, first);
    assert_eq!(
        json(&first),
        json(&repo.ok(&["issue", "show", "1", "--json"]))
    );
    assert_eq!(
        repo.forge_ref_names(),
        names(&[
            "refs/forge/issues/1",
            "refs/forge/meta/counter",
            &record_ref("issue-new", "k"),
        ])
    );

    let pr = [
        "pr",
        "create",
        "--source",
        "feature",
        "--base",
        "main",
        "p",
        "--idempotency-key",
        "k",
        "--json",
    ];
    let (first, second) = race(&repo, &pr);
    assert_eq!(second, first);
    assert_eq!(first, repo.ok(&["pr", "show", "2", "--json"]));

    for (entity, id, head, kind) in [
        ("issue", "1", "refs/forge/issues/1", "issue-comment"),
        ("pr", "2", "refs/forge/prs/2/head", "pr-comment"),
    ] {
        let before = repo.git(&["rev-parse", head]);
        let args = [
            entity,
            "comment",
            id,
            "once",
            "--idempotency-key",
            "k",
            "--json",
        ];
        let (first, second) = race(&repo, &args);
        assert_eq!(second, first);
        let event = repo.git(&["rev-parse", head]);
        assert_eq!(repo.git(&["rev-parse", &format!("{event}^")]), before);
        assert_eq!(json(&first)["event"], event.as_str());
        assert_eq!(repo.ok(&[entity, "show", id]).matches("once").count(), 1);
        assert!(repo.forge_ref_names().contains(&record_ref(kind, "k")));
    }
    let mut expected = vec![
        "refs/forge/issues/1".to_string(),
        "refs/forge/meta/counter".to_string(),
        "refs/forge/prs/2/base".to_string(),
        "refs/forge/prs/2/head".to_string(),
        "refs/forge/prs/2/meta".to_string(),
        "refs/forge/prs/2/source".to_string(),
    ];
    for kind in ["issue-new", "pr-create", "issue-comment", "pr-comment"] {
        expected.push(record_ref(kind, "k"));
    }
    expected.sort();
    assert_eq!(repo.forge_ref_names(), expected);
    assert_eq!(repo.ok(&["forge", "issue", "list"]), "#1 t (open)\n");
}

/// Two concurrent calls with the same key and DIFFERENT inputs: the first
/// released wins and the second fails with E_CONFLICT naming its entity.
#[test]
fn concurrent_same_key_calls_with_different_inputs_conflict() {
    let repo = Repo::new();
    let a = repo.0.with_extension("barrier-a");
    let b = repo.0.with_extension("barrier-b");
    let first = spawn_parked(&repo, &["issue", "new", "t", "--idempotency-key", "k"], &a);
    let second = spawn_parked(
        &repo,
        &["issue", "new", "u", "--idempotency-key", "k", "--json"],
        &b,
    );
    let first = release(first, &a);
    let second = release(second, &b);
    let _ = std::fs::remove_dir_all(&a);
    let _ = std::fs::remove_dir_all(&b);
    assert_eq!(success(&first), "issue #1 created: t\n");
    assert_eq!(second.status.code(), Some(1));
    let expected = conflict_json("k", "issue new", "issue", 1);
    assert_eq!(json(&stdout(&second)), expected);
    assert_eq!(
        stderr(&second),
        format!(
            "git-forge: {}\n",
            expected["error"]["message"].as_str().unwrap()
        )
    );
    assert_eq!(
        repo.forge_ref_names(),
        names(&[
            "refs/forge/issues/1",
            "refs/forge/meta/counter",
            &record_ref("issue-new", "k"),
        ])
    );
}

// ─────────────────────────── keys and usage ───────────────────────────

#[test]
fn keys_are_hex_encoded_into_the_record_ref_name() {
    let repo = Repo::new();
    let long = "x".repeat(100);
    for key in ["a/b c", "ünï", "tab\there", "..", "-dash", long.as_str()] {
        let first = success(&repo.run(&["issue", "new", "t", "--idempotency-key", key, "--json"]));
        let refs = repo.forge_refs();
        assert!(
            refs.iter()
                .any(|(name, _)| *name == record_ref("issue-new", key)),
            "{key:?}"
        );
        assert_eq!(
            success(&repo.run(&["issue", "new", "t", "--idempotency-key", key, "--json"])),
            first
        );
        assert_eq!(repo.forge_refs(), refs);
    }
    // Six distinct keys, six issues.
    let list = json(&repo.ok(&["issue", "list", "--json"]));
    assert_eq!(list.as_array().unwrap().len(), 6);
}

#[test]
fn invalid_key_arguments_are_usage_errors_that_write_nothing() {
    let repo = Repo::new();
    repo.branches();
    repo.ok(&["issue", "new", "one"]);
    repo.ok(&["pr", "create", "--source", "feature", "--base", "main", "p"]);
    let refs = repo.forge_refs();
    let long = "x".repeat(101);
    let commands: [&[&str]; 4] = [
        &["issue", "new", "t"],
        &["pr", "create", "--source", "feature", "--base", "main", "q"],
        &["issue", "comment", "1", "c"],
        &["pr", "comment", "2", "c"],
    ];
    let usage = "Run the command with --help and correct its arguments.";
    for command in commands {
        let cases: [(&[&str], &str, &str); 4] = [
            (
                &["--idempotency-key"],
                "--idempotency-key requires a value",
                "--idempotency-key",
            ),
            (
                &["--idempotency-key", ""],
                "--idempotency-key requires a non-empty value",
                "",
            ),
            (
                &["--idempotency-key", "a", "--idempotency-key", "a"],
                "--idempotency-key may only be specified once",
                "--idempotency-key",
            ),
            (
                &["--idempotency-key", &long],
                "--idempotency-key must be at most 100 bytes",
                &long,
            ),
        ];
        for (extra, message, input) in cases {
            // The standalone `--json` precedes the key flag: after it,
            // `--json` would be the key's value.
            let args = [command, &["--json"], extra].concat();
            let output = repo.run(&args);
            assert_eq!(output.status.code(), Some(1), "{args:?}");
            assert_eq!(
                stderr(&output),
                format!("git-forge: {message}\n"),
                "{args:?}"
            );
            assert_eq!(
                json(&stdout(&output)),
                serde_json::json!({"error": {
                    "code": "E_USAGE", "message": message, "input": input,
                    "hint": usage, "retryable": false,
                }}),
                "{args:?}"
            );
        }
    }
    assert_eq!(repo.forge_refs(), refs);
}

#[test]
fn commands_without_a_key_write_no_record_and_repeat_the_mutation() {
    let repo = Repo::new();
    repo.branches();
    assert_eq!(repo.ok(&["issue", "new", "t"]), "issue #1 created: t\n");
    assert_eq!(repo.ok(&["issue", "new", "t"]), "issue #2 created: t\n");
    repo.ok(&["issue", "comment", "1", "c"]);
    repo.ok(&["issue", "comment", "1", "c"]);
    repo.ok(&["pr", "create", "--source", "feature", "--base", "main", "p"]);
    repo.ok(&["pr", "create", "--source", "feature", "--base", "main", "p"]);
    repo.ok(&["pr", "comment", "3", "c"]);
    repo.ok(&["pr", "comment", "3", "c"]);
    assert_eq!(repo.ok(&["issue", "show", "1"]).matches("  - c").count(), 2);
    assert_eq!(
        repo.ok(&["pr", "show", "3"]).matches("comment: c").count(),
        2
    );
    assert!(repo
        .forge_ref_names()
        .iter()
        .all(|name| !name.starts_with("refs/forge/idempotency/")));
}

// ─────────────────────────── malformed records ───────────────────────────

/// A record ref that exists but is not a direct ref to a record commit is
/// `E_INVALID`, never an absent key: a symbolic ref, a non-commit target,
/// and a commit with a malformed record blob.
#[test]
fn a_present_but_malformed_record_ref_is_invalid() {
    let record = record_ref("issue-new", "k");
    let message = format!("invalid store state: malformed idempotency record {record}");
    type Corrupt = fn(&Repo, &str);
    let corrupt: [(&str, Corrupt); 3] = [
        ("symbolic ref", |repo, record| {
            repo.git(&["symbolic-ref", record, "refs/forge/issues/1"]);
        }),
        ("blob target", |repo, record| {
            let blob = repo.git(&["rev-parse", "HEAD:base.txt"]);
            repo.git(&["update-ref", record, &blob]);
        }),
        ("malformed blob", |repo, record| {
            std::fs::create_dir_all(repo.0.join(".forge")).unwrap();
            std::fs::write(repo.0.join(".forge/idempotency.json"), "{}").unwrap();
            repo.git(&["add", ".forge/idempotency.json"]);
            let tree = repo.git(&["write-tree"]);
            let commit = repo.git(&["commit-tree", &tree, "-m", "bad"]);
            repo.git(&["rm", "-q", "--cached", ".forge/idempotency.json"]);
            repo.git(&["update-ref", record, &commit]);
        }),
    ];
    for (case, corrupt) in corrupt {
        let repo = Repo::new();
        repo.branches();
        repo.ok(&["issue", "new", "one"]);
        corrupt(&repo, &record);
        let refs = repo.forge_refs();
        for args in [
            &["issue", "new", "t", "--idempotency-key", "k", "--json"][..],
            &["issue", "new", "t", "--idempotency-key", "k"][..],
        ] {
            let output = repo.run(args);
            assert_eq!(output.status.code(), Some(1), "{case}");
            assert_eq!(stderr(&output), format!("git-forge: {message}\n"), "{case}");
            if args.contains(&"--json") {
                assert_eq!(
                    json(&stdout(&output)),
                    serde_json::json!({"error": {
                        "code": "E_INVALID", "message": message, "input": "",
                        "hint": "Correct the repository state before retrying.",
                        "retryable": false,
                    }}),
                    "{case}"
                );
            } else {
                assert_eq!(stdout(&output), "", "{case}");
            }
            assert_eq!(repo.forge_refs(), refs, "{case}");
        }
    }
}

/// A same-key loser that resumes after the winner committed must answer from
/// the winner's record even when the state its own validation reads changed
/// in between (a deleted branch, Issue, PR, parent or blocker).
#[test]
fn a_same_key_loser_replays_even_when_its_validation_inputs_changed() {
    type Change = fn(&Repo);
    let cases: [(&str, &[&str], Change); 7] = [
        (
            "pr create, source branch deleted",
            &[
                "pr",
                "create",
                "--source",
                "feature",
                "--base",
                "main",
                "p",
                "--closes",
                "1",
                "--idempotency-key",
                "k",
                "--json",
            ],
            |repo| {
                repo.git(&["branch", "-D", "feature"]);
            },
        ),
        (
            "pr create, base branch moved onto the source",
            &[
                "pr",
                "create",
                "--source",
                "feature",
                "--base",
                "other",
                "p",
                "--idempotency-key",
                "k",
                "--json",
            ],
            |repo| {
                repo.git(&["update-ref", "refs/heads/other", "refs/heads/feature"]);
            },
        ),
        (
            "pr create, closed issue deleted",
            &[
                "pr",
                "create",
                "--source",
                "feature",
                "--base",
                "main",
                "p",
                "--closes",
                "1",
                "--idempotency-key",
                "k",
                "--json",
            ],
            |repo| {
                repo.git(&["update-ref", "-d", "refs/forge/issues/1"]);
            },
        ),
        (
            "issue new, parent deleted",
            &[
                "issue",
                "new",
                "t",
                "--parent",
                "1",
                "--idempotency-key",
                "k",
                "--json",
            ],
            |repo| {
                repo.git(&["update-ref", "-d", "refs/forge/issues/1"]);
            },
        ),
        (
            "issue new, blocker deleted",
            &[
                "issue",
                "new",
                "t",
                "--blocked-by",
                "1",
                "--idempotency-key",
                "k",
                "--json",
            ],
            |repo| {
                repo.git(&["update-ref", "-d", "refs/forge/issues/1"]);
            },
        ),
        (
            "issue comment, issue deleted",
            &[
                "issue",
                "comment",
                "1",
                "c",
                "--idempotency-key",
                "k",
                "--json",
            ],
            |repo| {
                repo.git(&["update-ref", "-d", "refs/forge/issues/1"]);
            },
        ),
        (
            "pr comment, PR deleted",
            &[
                "pr",
                "comment",
                "2",
                "c",
                "--idempotency-key",
                "k",
                "--json",
            ],
            |repo| {
                repo.git(&["update-ref", "-d", "refs/forge/prs/2/head"]);
            },
        ),
    ];
    let mut failures = Vec::new();
    for (case, args, change) in cases {
        let repo = Repo::new();
        repo.branches();
        repo.ok(&["issue", "new", "one"]);
        repo.ok(&["pr", "create", "--source", "other", "--base", "main", "q"]);
        let (first, second) = race_then(&repo, args, change);
        let first = success(&first);
        assert!(json(&first)["id"].as_u64().unwrap() > 0, "{case}");
        if second.status.code() != Some(0) || stdout(&second) != first || !second.stderr.is_empty()
        {
            failures.push(format!(
                "{case}: {:?} {}",
                second.status.code(),
                stderr(&second)
            ));
        }
    }
    assert!(failures.is_empty(), "{failures:#?}");
}
