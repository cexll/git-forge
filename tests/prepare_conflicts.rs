//! `pr prepare` conflict diagnostics at the built CLI boundary: every path
//! that leaves an unmerged index reports the conflicted paths, the remedy,
//! and a complete `E_PREPARE_CONFLICT` JSON error, with no ref change and no
//! temporary worktree left behind.
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
            "gf-prepare-conflict-{tag}-{}-{}",
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

    fn commit_all(&self, message: &str) {
        self.git(&["add", "-A"]);
        self.git(&["commit", "-q", "-m", message]);
    }

    fn ref_oid(&self, name: &str) -> Option<String> {
        let out = Command::new("git")
            .args(["rev-parse", "--verify", "-q", name])
            .current_dir(&self.0)
            .output()
            .unwrap();
        out.status
            .success()
            .then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
    }

    fn install_hook(&self, name: &str, body: &str) {
        use std::os::unix::fs::PermissionsExt;
        let hook = self.0.join(".git/hooks").join(name);
        std::fs::write(&hook, body).unwrap();
        std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    fn worktree_count(&self) -> usize {
        self.git(&["worktree", "list", "--porcelain"])
            .matches("worktree ")
            .count()
    }
}

/// How the source branch and the base branch diverge.
#[derive(Clone, Copy, Debug)]
enum Divergence {
    /// Both sides edit `shared.txt` differently.
    Content,
    /// The source deletes `shared.txt`; the base modifies it.
    ModifyDelete,
    /// The source renames `shared.txt` to `renamed.txt`; the base deletes it.
    RenameDelete,
    /// Both sides add `added.txt` with different content.
    AddAdd,
    /// Both sides edit 21 files differently.
    ManyPaths,
    /// The sides do not conflict; a commit hook writes unmerged entries.
    CleanWithHook,
}

fn many_paths() -> Vec<String> {
    (0..21).map(|i| format!("many/f{i:02}.txt")).collect()
}

/// Build the repository: a common ancestor, a `feature` source branch, a
/// diverged `main`, CI plan, and PR #1 from `feature` into `main`.
fn conflict_fixture(divergence: Divergence) -> Repo {
    let repo = Repo::new(&format!("{divergence:?}").to_lowercase());
    std::fs::create_dir_all(repo.0.join(".forge")).unwrap();
    std::fs::create_dir_all(repo.0.join("many")).unwrap();
    repo.write(".forge/ci.sh", "#!/bin/sh\nexit 0\n");
    repo.write("shared.txt", "ancestor\n");
    for path in many_paths() {
        repo.write(&path, "ancestor\n");
    }
    repo.commit_all("ancestor");
    repo.git(&["checkout", "-q", "-b", "feature"]);
    match divergence {
        Divergence::Content => repo.write("shared.txt", "source\n"),
        Divergence::ModifyDelete => {
            repo.git(&["rm", "-q", "shared.txt"]);
        }
        Divergence::RenameDelete => {
            repo.git(&["mv", "shared.txt", "renamed.txt"]);
        }
        Divergence::AddAdd => repo.write("added.txt", "source\n"),
        Divergence::ManyPaths => many_paths()
            .iter()
            .for_each(|path| repo.write(path, "source\n")),
        Divergence::CleanWithHook => repo.write("source-only.txt", "source\n"),
    }
    repo.commit_all("source change");
    repo.git(&["checkout", "-q", "main"]);
    match divergence {
        Divergence::Content | Divergence::ModifyDelete => repo.write("shared.txt", "base\n"),
        Divergence::RenameDelete => {
            repo.git(&["rm", "-q", "shared.txt"]);
        }
        Divergence::AddAdd => repo.write("added.txt", "base\n"),
        Divergence::ManyPaths => many_paths()
            .iter()
            .for_each(|path| repo.write(path, "base\n")),
        Divergence::CleanWithHook => repo.write("base-only.txt", "base\n"),
    }
    repo.commit_all("base change");
    let (code, _, stderr) = repo.forge(&[
        "forge",
        "pr",
        "create",
        "--source",
        "feature",
        "--base",
        "main",
        "Conflicting",
    ]);
    assert_eq!(code, 0, "pr create: {stderr}");
    repo
}

/// A hook that replaces `shared.txt`'s index entry with three unmerged
/// stages and then fails, as a conflicting hook would.
fn unmerged_index_hook(repo: &Repo) -> String {
    let blob = repo.git(&["rev-parse", "main:shared.txt"]);
    format!(
        "#!/bin/sh\nprintf '0 0000000000000000000000000000000000000000\\tshared.txt\\n100644 {blob} 1\\tshared.txt\\n100644 {blob} 2\\tshared.txt\\n100644 {blob} 3\\tshared.txt\\n' | git update-index --index-info || exit 99\necho 'hook failed' >&2\nexit 1\n"
    )
}

/// A hook that adds three unmerged stages for each of `paths` (exact bytes,
/// NUL-terminated records) and then fails.
fn unmerged_paths_hook(repo: &Repo, paths: &[&[u8]]) -> String {
    let blob = repo.git(&["rev-parse", "main:shared.txt"]);
    let mut records = Vec::new();
    for path in paths {
        for stage in 1..=3 {
            records.extend_from_slice(format!("100644 {blob} {stage}\t").as_bytes());
            records.extend_from_slice(path);
            records.push(0);
        }
    }
    let file = repo.0.join(".git/unmerged-records");
    std::fs::write(&file, records).unwrap();
    format!(
        "#!/bin/sh\ngit update-index -z --index-info < '{}' || exit 99\necho 'hook failed' >&2\nexit 1\n",
        file.display()
    )
}

/// The independently stated prepare-conflict contract (spec #33 default
/// "Prepare conflict output").
fn expected_message(strategy: &str, base: &str, source: &str, paths: &[String]) -> String {
    let shown = paths
        .iter()
        .take(20)
        .cloned()
        .collect::<Vec<_>>()
        .join(", ");
    let more = if paths.len() > 20 {
        format!(" (and {} more)", paths.len() - 20)
    } else {
        String::new()
    };
    let count = if paths.len() == 1 {
        "1 path".to_string()
    } else {
        format!("{} paths", paths.len())
    };
    format!(
        "PR #1 cannot be prepared with strategy {strategy}: source {source} conflicts with \
         base 'main' at {base} in {count}: {shown}{more}; merge 'main' into the source branch \
         'feature', resolve the conflicts, then run `git forge pr update 1` or create a \
         replacement PR; worktree cleaned up, no ref changes made"
    )
}

const HINT: &str = "Merge the base branch into the source branch, resolve the conflicts, \
                    then run `git forge pr update <n>` or create a replacement PR.";

/// `paths` are the expected git-quoted paths (core.quotePath=false); the
/// message and `data.paths` both carry them.
fn assert_prepare_conflict(repo: &Repo, strategy: &str, paths: &[String]) {
    let base = repo.ref_oid("refs/heads/main").unwrap();
    let source = repo.ref_oid("refs/heads/feature").unwrap();
    let protected = [
        "refs/heads/main",
        "refs/heads/feature",
        "refs/forge/prs/1/head",
        "refs/forge/prs/1/result",
    ];
    let before = protected.map(|name| repo.ref_oid(name));
    let message = expected_message(strategy, &base, &source, paths);
    let args = ["forge", "pr", "prepare", "1", "--strategy", strategy];

    let (code, stdout, stderr) = repo.forge(&args);
    assert_eq!(code, 1, "{strategy}: {stderr}");
    assert_eq!(stdout, "");
    assert_eq!(stderr, format!("git-forge: {message}"));
    assert!(!stderr.contains("failed: ;"), "{stderr}");

    let mut json_args = args.to_vec();
    json_args.push("--json");
    let (code, stdout, stderr) = repo.forge(&json_args);
    assert_eq!(code, 1);
    assert_eq!(stderr, format!("git-forge: {message}"));
    let actual: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(
        actual,
        serde_json::json!({"error": {
            "code": "E_PREPARE_CONFLICT",
            "message": message,
            "input": "1",
            "hint": HINT.replace("<n>", "1"),
            "retryable": false,
            "data": {
                "pr": 1,
                "strategy": strategy,
                "base": base,
                "source": source,
                "paths": paths,
            }
        }})
    );
    assert_eq!(protected.map(|name| repo.ref_oid(name)), before);
    assert_eq!(repo.worktree_count(), 1, "temporary worktree left behind");
}

fn strings(paths: &[&str]) -> Vec<String> {
    paths.iter().map(|p| p.to_string()).collect()
}

#[test]
fn prepare_conflict_paths_matrix() {
    let shared = strings(&["shared.txt"]);
    let rows: Vec<(&str, Divergence, Vec<String>)> = vec![
        ("merge", Divergence::Content, shared.clone()),
        ("merge", Divergence::ModifyDelete, shared.clone()),
        ("merge", Divergence::RenameDelete, strings(&["renamed.txt"])),
        ("merge", Divergence::AddAdd, strings(&["added.txt"])),
        ("squash", Divergence::Content, shared.clone()),
        ("squash", Divergence::ModifyDelete, shared.clone()),
        (
            "squash",
            Divergence::RenameDelete,
            strings(&["renamed.txt"]),
        ),
        ("squash", Divergence::AddAdd, strings(&["added.txt"])),
        ("rebase", Divergence::Content, shared.clone()),
        ("rebase", Divergence::ModifyDelete, shared.clone()),
        (
            "rebase",
            Divergence::RenameDelete,
            strings(&["renamed.txt"]),
        ),
        ("rebase", Divergence::AddAdd, strings(&["added.txt"])),
        ("merge", Divergence::ManyPaths, many_paths()),
    ];
    let mut failures = Vec::new();
    for (strategy, divergence, paths) in rows {
        let repo = conflict_fixture(divergence);
        let outcome = std::panic::catch_unwind(|| assert_prepare_conflict(&repo, strategy, &paths));
        if let Err(error) = outcome {
            let detail = error
                .downcast_ref::<String>()
                .map(String::as_str)
                .or_else(|| error.downcast_ref::<&str>().copied())
                .unwrap_or("non-string panic");
            failures.push(format!("{strategy}/{divergence:?}: {detail}"));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn prepare_conflict_from_merge_hook_unmerged_index() {
    let repo = conflict_fixture(Divergence::CleanWithHook);
    repo.install_hook("pre-merge-commit", &unmerged_index_hook(&repo));
    assert_prepare_conflict(&repo, "merge", &strings(&["shared.txt"]));
}

#[test]
fn prepare_conflict_from_squash_hook_unmerged_index() {
    let repo = conflict_fixture(Divergence::CleanWithHook);
    repo.install_hook("pre-commit", &unmerged_index_hook(&repo));
    assert_prepare_conflict(&repo, "squash", &strings(&["shared.txt"]));
}

/// Under `core.ignorecase=true` the index compares paths case-insensitively;
/// unmerged `A.txt` and `a.txt` are still two conflicted paths.
#[test]
fn prepare_conflict_keeps_case_distinct_paths() {
    let repo = conflict_fixture(Divergence::CleanWithHook);
    repo.git(&["config", "core.ignorecase", "true"]);
    repo.install_hook(
        "pre-commit",
        &unmerged_paths_hook(&repo, &[b"A.txt", b"a.txt"]),
    );
    assert_prepare_conflict(&repo, "squash", &strings(&["A.txt", "a.txt"]));
}

/// A path with control bytes is reported in git's C-style quoted form, in
/// both the message and `data.paths`, so no raw control byte reaches stderr.
#[test]
fn prepare_conflict_escapes_control_bytes_in_message() {
    let repo = conflict_fixture(Divergence::CleanWithHook);
    repo.install_hook(
        "pre-commit",
        &unmerged_paths_hook(&repo, &[b"evil\nline\x08.txt"]),
    );
    assert_prepare_conflict(&repo, "squash", &strings(&["\"evil\\nline\\b.txt\""]));
}

/// Distinct non-UTF-8 paths stay distinct: each invalid byte is an octal
/// escape inside a quoted path.
#[test]
fn prepare_conflict_keeps_distinct_invalid_utf8_paths() {
    let repo = conflict_fixture(Divergence::CleanWithHook);
    repo.install_hook(
        "pre-commit",
        &unmerged_paths_hook(&repo, &[b"bad\x80.txt", b"bad\x81.txt"]),
    );
    assert_prepare_conflict(
        &repo,
        "squash",
        &strings(&["\"bad\\200.txt\"", "\"bad\\201.txt\""]),
    );
}

/// ff has no conflict path: a diverged base fails the fast-forward
/// precondition before any worktree exists, and that refusal is unchanged.
#[test]
fn prepare_ff_divergence_keeps_precondition_refusal() {
    let repo = conflict_fixture(Divergence::Content);
    let base = repo.ref_oid("refs/heads/main").unwrap();
    let merge_base = repo.git(&["merge-base", "main", "feature"]);
    let (code, stdout, stderr) = repo.forge(&["forge", "pr", "prepare", "1", "--strategy", "ff"]);
    assert_eq!(code, 1);
    assert_eq!(stdout, "");
    assert_eq!(
        stderr,
        format!(
            "git-forge: PR #1 cannot fast-forward: the source does not sit directly on top of \
             the current base tip of 'main' (merge_base {merge_base} != base tip {base}); use \
             --strategy rebase to replay the source or recreate the PR"
        )
    );
    assert_eq!(repo.ref_oid("refs/forge/prs/1/result"), None);
    assert_eq!(repo.worktree_count(), 1);
}
