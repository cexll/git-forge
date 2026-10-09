//! `pr update` moved-base refusal at the built CLI boundary: every way the
//! base branch can leave the snapshot base refuses with the gated remedy
//! (spec #33 default "Moved-base pr update message"), a complete
//! `E_BASE_MOVED` JSON error under `--json`, and no ref change.
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT_REPO: AtomicU64 = AtomicU64::new(0);

struct Repo(PathBuf);

impl Drop for Repo {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

impl Repo {
    fn new(tag: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "gf-moved-base-{tag}-{}-{}",
            std::process::id(),
            NEXT_REPO.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&path).unwrap();
        let repo = Self(path);
        repo.git(&["init", "-q", "-b", "main"]);
        repo.git(&["config", "user.name", "Test"]);
        repo.git(&["config", "user.email", "test@example.com"]);
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

    fn forge(&self, args: &[&str]) -> (i32, String, String) {
        let out = Command::new(env!("CARGO_BIN_EXE_git-forge"))
            .args(args)
            .current_dir(&self.0)
            .env_remove("GIT_FORGE_ROLE")
            .output()
            .unwrap();
        (
            out.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&out.stdout).trim().to_string(),
            String::from_utf8_lossy(&out.stderr).trim().to_string(),
        )
    }

    fn write(&self, path: &str, content: &str) {
        std::fs::write(self.0.join(path), content).unwrap();
    }

    fn commit_all(&self, message: &str) -> String {
        self.git(&["add", "-A"]);
        self.git(&["commit", "-q", "-m", message]);
        self.git(&["rev-parse", "HEAD"])
    }

    fn rev(&self, name: &str) -> String {
        self.git(&["rev-parse", name])
    }

    /// A new commit with `parent`'s tree and `parent` as its only parent.
    fn child_of(&self, parent: &str, message: &str) -> String {
        self.git(&[
            "commit-tree",
            &format!("{parent}^{{tree}}"),
            "-p",
            parent,
            "-m",
            message,
        ])
    }

    /// Every ref name and target, so a refusal can prove it wrote nothing.
    fn all_refs(&self) -> String {
        self.git(&["for-each-ref", "--format=%(refname) %(objectname)"])
    }
}

/// A repository with `ancestor` <- `base tip` on `<base>`, a `feature`
/// source branch with one commit on the base tip, and PR #1 from `feature`
/// into `<base>`. HEAD is detached so every branch can be moved or deleted.
struct Fixture {
    repo: Repo,
    base: String,
    ancestor: String,
}

fn fixture(tag: &str, base: &str) -> Fixture {
    let repo = Repo::new(tag);
    std::fs::create_dir_all(repo.0.join(".forge")).unwrap();
    repo.write(".forge/ci.sh", "#!/bin/sh\nexit 0\n");
    repo.write("shared.txt", "ancestor\n");
    let ancestor = repo.commit_all("ancestor");
    repo.write("shared.txt", "base\n");
    repo.commit_all("base tip");
    if base != "main" {
        repo.git(&["branch", base, "main"]);
    }
    repo.git(&["checkout", "-q", "-b", "feature"]);
    repo.write("source.txt", "source\n");
    repo.commit_all("source change");
    let (code, _, stderr) = repo.forge(&[
        "forge",
        "pr",
        "create",
        "--source",
        "feature",
        "--base",
        base,
        "Moved base",
    ]);
    assert_eq!(code, 0, "pr create: {stderr}");
    repo.git(&["checkout", "-q", "--detach"]);
    Fixture {
        repo,
        base: base.to_string(),
        ancestor,
    }
}

/// How the base branch (and possibly the source) change after PR creation.
#[derive(Clone, Copy, Debug)]
enum Movement {
    /// The base gains one commit on top of the snapshot base.
    Advance,
    /// The base tip is replaced by a commit that does not contain it.
    Rewrite,
    /// The base is reset back to an ancestor of the snapshot base.
    ResetBack,
    /// The source gains a commit and the base advances: the moved base
    /// still refuses the otherwise valid update.
    SourceAndBase,
    /// A successful update (generation 1) precedes the base advance.
    AfterUpdate,
}

fn apply(f: &Fixture, movement: Movement) {
    let repo = &f.repo;
    let base_ref = format!("refs/heads/{}", f.base);
    let snapshot = repo.rev(&base_ref);
    match movement {
        Movement::Advance => {
            let next = repo.child_of(&snapshot, "base advance");
            repo.git(&["update-ref", &base_ref, &next]);
        }
        Movement::Rewrite => {
            let rewritten = repo.child_of(&f.ancestor, "base rewritten");
            repo.git(&["update-ref", &base_ref, &rewritten]);
        }
        Movement::ResetBack => {
            repo.git(&["update-ref", &base_ref, &f.ancestor]);
        }
        Movement::SourceAndBase => {
            let source = repo.child_of(&repo.rev("refs/heads/feature"), "source advance");
            repo.git(&["update-ref", "refs/heads/feature", &source]);
            let next = repo.child_of(&snapshot, "base advance");
            repo.git(&["update-ref", &base_ref, &next]);
        }
        Movement::AfterUpdate => {
            let source = repo.child_of(&repo.rev("refs/heads/feature"), "source advance");
            repo.git(&["update-ref", "refs/heads/feature", &source]);
            let (code, _, stderr) = repo.forge(&["forge", "pr", "update", "1"]);
            assert_eq!(code, 0, "first pr update: {stderr}");
            let next = repo.child_of(&snapshot, "base advance");
            repo.git(&["update-ref", &base_ref, &next]);
        }
    }
}

/// The independently stated message (spec #33 default "Moved-base pr update
/// message"), without the `git-forge: ` prefix. `<s>` is literal: `pr update`
/// has no strategy to name.
fn expected_message(shown_base: &str) -> String {
    format!(
        "base branch '{shown_base}' has moved since PR #1 was created; run git forge pr \
         prepare 1 --strategy <s> to merge the existing snapshot, or create a replacement PR \
         from the updated source with the same --closes and abandon PR #1"
    )
}

const HINT: &str = "Inspect the current base and PR snapshot before proceeding.";

/// `pr update 1` refuses with `E_BASE_MOVED`, with and without `--json`,
/// and changes no ref. `shown_base` is the base name as the terminal shows it.
fn assert_base_moved(f: &Fixture, shown_base: &str, snapshot_base: &str) {
    let repo = &f.repo;
    let current_base = repo.rev(&format!("refs/heads/{}", f.base));
    assert_ne!(current_base, snapshot_base, "fixture did not move the base");
    let message = expected_message(shown_base);
    let before = repo.all_refs();

    let (code, stdout, stderr) = repo.forge(&["forge", "pr", "update", "1"]);
    assert_eq!(code, 1, "{stderr}");
    assert_eq!(stdout, "");
    assert_eq!(stderr, format!("git-forge: {message}"));
    assert!(!stderr.contains("manual"), "{stderr}");

    let (code, stdout, stderr) = repo.forge(&["forge", "pr", "update", "1", "--json"]);
    assert_eq!(code, 1, "{stderr}");
    assert_eq!(stderr, format!("git-forge: {message}"));
    let actual: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(
        actual,
        serde_json::json!({"error": {
            "code": "E_BASE_MOVED",
            "message": message,
            "input": f.base,
            "hint": HINT,
            "retryable": false,
            "data": {
                "pr": 1,
                "base": f.base,
                "snapshot_base": snapshot_base,
                "current_base": current_base,
            }
        }})
    );
    assert_eq!(repo.all_refs(), before, "a refused update changed refs");
}

/// Run each row in its own repository and report every failing row.
fn run_rows(rows: Vec<(&str, &str, &str, Movement)>) {
    let mut failures = Vec::new();
    for (tag, base, shown_base, movement) in rows {
        let outcome = std::panic::catch_unwind(|| {
            let f = fixture(tag, base);
            let snapshot_base = f.repo.rev(&format!("refs/heads/{}", f.base));
            apply(&f, movement);
            assert_base_moved(&f, shown_base, &snapshot_base);
        });
        if let Err(error) = outcome {
            let detail = error
                .downcast_ref::<String>()
                .map(String::as_str)
                .or_else(|| error.downcast_ref::<&str>().copied())
                .unwrap_or("non-string panic");
            failures.push(format!("{tag}/{movement:?}: {detail}"));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn pr_update_moved_base_matrix() {
    run_rows(vec![
        ("advance", "main", "main", Movement::Advance),
        ("rewrite", "main", "main", Movement::Rewrite),
        ("reset-back", "main", "main", Movement::ResetBack),
        ("source-and-base", "main", "main", Movement::SourceAndBase),
        ("after-update", "main", "main", Movement::AfterUpdate),
        // Git refnames allow C1 controls; the terminal message escapes them,
        // while `input` and `data.base` keep the exact name.
        ("c1-name", "base\u{9b}2J", "base\\x9b2J", Movement::Advance),
    ]);
}

/// Characterization: a deleted base is not a moved base. `pr update` keeps
/// its existing missing-branch refusal (`E_NOT_FOUND`), which precedes the
/// moved-base check, and changes no ref.
#[test]
fn pr_update_deleted_base_keeps_not_found() {
    let f = fixture("deleted", "main");
    let repo = &f.repo;
    repo.git(&["update-ref", "-d", "refs/heads/main"]);
    let before = repo.all_refs();
    let message = "branch 'main' does not exist";

    let (code, stdout, stderr) = repo.forge(&["forge", "pr", "update", "1"]);
    assert_eq!(code, 1, "{stderr}");
    assert_eq!(stdout, "");
    assert_eq!(stderr, format!("git-forge: {message}"));

    let (code, stdout, stderr) = repo.forge(&["forge", "pr", "update", "1", "--json"]);
    assert_eq!(code, 1, "{stderr}");
    assert_eq!(stderr, format!("git-forge: {message}"));
    let actual: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(
        actual,
        serde_json::json!({"error": {
            "code": "E_NOT_FOUND",
            "message": message,
            "input": "main",
            "hint": "List the existing entities or branches and use an existing identifier.",
            "retryable": false,
        }})
    );
    assert_eq!(repo.all_refs(), before, "a refused update changed refs");
}

/// Run `pr update 1 --json` parked at the test-only write barrier (after its
/// moved-base check, before its transaction), run `between`, then release it.
fn update_racing(f: &Fixture, between: impl Fn(&Repo)) -> (i32, String, String) {
    let repo = &f.repo;
    let barrier = repo.0.with_extension("barrier");
    let mut child = Command::new(env!("CARGO_BIN_EXE_git-forge"))
        .args(["forge", "pr", "update", "1", "--json"])
        .current_dir(&repo.0)
        .env_remove("GIT_FORGE_ROLE")
        .env("GIT_FORGE_TEST_WRITE_BARRIER", &barrier)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    while !barrier.join("ready").exists() {
        assert!(
            child.try_wait().unwrap().is_none(),
            "exited before the barrier"
        );
        assert!(std::time::Instant::now() < deadline, "never parked");
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    between(repo);
    std::fs::write(barrier.join("release"), "release").unwrap();
    let out = child.wait_with_output().unwrap();
    let _ = std::fs::remove_dir_all(&barrier);
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).trim().to_string(),
        String::from_utf8_lossy(&out.stderr).trim().to_string(),
    )
}

/// A base that moves after `pr update` checked it and before its transaction
/// publishes still refuses with `E_BASE_MOVED` (the transaction verifies the
/// observed base tip), naming the moved tip, and publishes no generation.
#[test]
fn pr_update_refuses_a_base_moved_after_its_check() {
    let f = fixture("race", "main");
    let repo = &f.repo;
    let snapshot_base = repo.rev("refs/heads/main");
    let source = repo.child_of(&repo.rev("refs/heads/feature"), "source advance");
    repo.git(&["update-ref", "refs/heads/feature", &source]);
    let (code, stdout, stderr) = update_racing(&f, |repo| {
        let next = repo.child_of(&repo.rev("refs/heads/main"), "base advance");
        repo.git(&["update-ref", "refs/heads/main", &next]);
    });
    let current_base = repo.rev("refs/heads/main");
    let message = expected_message("main");
    assert_eq!(code, 1, "{stdout} {stderr}");
    assert_eq!(stderr, format!("git-forge: {message}"));
    let actual: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(
        actual,
        serde_json::json!({"error": {
            "code": "E_BASE_MOVED",
            "message": message,
            "input": "main",
            "hint": HINT,
            "retryable": false,
            "data": {
                "pr": 1,
                "base": "main",
                "snapshot_base": snapshot_base,
                "current_base": current_base,
            }
        }})
    );
    let forge_refs = repo.git(&["for-each-ref", "--format=%(refname)", "refs/forge/prs/1/"]);
    assert!(
        !forge_refs.contains("/gen/1/"),
        "a generation was published: {forge_refs}"
    );
    let (_, shown, _) = repo.forge(&["forge", "pr", "show", "1", "--json"]);
    let shown: serde_json::Value = serde_json::from_str(&shown).unwrap();
    assert_eq!(shown["generation"], 0, "{shown}");
}

/// A base deleted in the same window is a moved base: `E_BASE_MOVED` with
/// `current_base` null, and no generation is published.
#[test]
fn pr_update_refuses_a_base_deleted_after_its_check() {
    let f = fixture("race-deleted", "main");
    let repo = &f.repo;
    let snapshot_base = repo.rev("refs/heads/main");
    let source = repo.child_of(&repo.rev("refs/heads/feature"), "source advance");
    repo.git(&["update-ref", "refs/heads/feature", &source]);
    let (code, stdout, stderr) = update_racing(&f, |repo| {
        repo.git(&["update-ref", "-d", "refs/heads/main"]);
    });
    let message = expected_message("main");
    assert_eq!(code, 1, "{stdout} {stderr}");
    assert_eq!(stderr, format!("git-forge: {message}"));
    let actual: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(
        actual,
        serde_json::json!({"error": {
            "code": "E_BASE_MOVED",
            "message": message,
            "input": "main",
            "hint": HINT,
            "retryable": false,
            "data": {
                "pr": 1,
                "base": "main",
                "snapshot_base": snapshot_base,
                "current_base": null,
            }
        }})
    );
    let forge_refs = repo.git(&["for-each-ref", "--format=%(refname)", "refs/forge/prs/1/"]);
    assert!(
        !forge_refs.contains("/gen/1/"),
        "a generation was published: {forge_refs}"
    );
}
