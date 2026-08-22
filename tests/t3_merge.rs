//! t3 merge integration tests. Run the built `git-forge` binary as a
//! subprocess inside an isolated temp git repository.

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

fn tmpdir(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!(
        "gf-t3-{tag}-{}-{:?}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .subsec_nanos()
    ));
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn git(dir: &Path, args: &[&str]) -> (i32, String, String) {
    let out = Command::new("git")
        .args(args)
        .current_dir(dir)
        .env("GIT_AUTHOR_NAME", "Test")
        .env("GIT_AUTHOR_EMAIL", "test@example.com")
        .env("GIT_COMMITTER_NAME", "Test")
        .env("GIT_COMMITTER_EMAIL", "test@example.com")
        .output()
        .unwrap();
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).trim().to_string(),
        String::from_utf8_lossy(&out.stderr).trim().to_string(),
    )
}

/// init_repo: set user identity in config so the CLI's `git commit`/`git
/// merge` (which do not inherit the test's env) find an identity.
fn init_repo(dir: &Path) {
    let (c, _, e) = git(dir, &["init", "-q"]);
    assert!(c == 0, "git init failed: {e}");
    let (c, _, e) = git(dir, &["config", "user.name", "Test"]);
    assert!(c == 0, "config user.name failed: {e}");
    let (c, _, e) = git(dir, &["config", "user.email", "test@example.com"]);
    assert!(c == 0, "config user.email failed: {e}");
    std::fs::write(dir.join("base.txt"), "base\n").unwrap();
    let (c, _, e) = git(dir, &["add", "base.txt"]);
    assert!(c == 0, "git add failed: {e}");
    let (c, _, e) = git(dir, &["commit", "-q", "-m", "base commit"]);
    assert!(c == 0, "base commit failed: {e}");
    let (c, _, e) = git(dir, &["branch", "-M", "main"]);
    assert!(c == 0, "branch -M failed: {e}");
}

fn forge(dir: &Path, args: &[&str]) -> (i32, String, String) {
    let bin = env!("CARGO_BIN_EXE_git-forge");
    let out = Command::new(bin)
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap();
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).trim().to_string(),
        String::from_utf8_lossy(&out.stderr).trim().to_string(),
    )
}

/// Repo for the email-only PR actor regression: setup (git commits) runs with
/// user.name present, then user.name is UNSET for the forge phase so only
/// user.email remains locally configured. Asserts the post-unset state so a
/// name that silently survives can never mask the regression.
fn init_email_only_test_repo(dir: &Path) {
    init_repo(dir);
    make_feature(dir, "feature", "feat\n");
    let (c, _, e) = git(dir, &["config", "--unset", "user.name"]);
    assert_eq!(c, 0, "unset user.name failed: {e}");
    let (c, _, e) = git(dir, &["config", "user.email", "practor@example.com"]);
    assert_eq!(c, 0, "set user.email failed: {e}");
    let (c, o, _) = git(dir, &["config", "--get", "--local", "user.name"]);
    assert_ne!(
        c, 0,
        "local user.name must be absent after unset (got {o:?})"
    );
    let (c, o, _) = git(dir, &["config", "--get", "--local", "user.email"]);
    assert_eq!(c, 0, "local user.email must remain: {o}");
    assert_eq!(o.trim(), "practor@example.com");
}

/// Run the git-forge binary in `dir` under a HERMETIC config env: HOME and
/// the XDG dirs point at a freshly-and-exclusively-created empty directory and
/// system config is disabled. Only the repo's own local config can supply git
/// identity, so no machine/user-level user.name can leak in and mask a
/// missing-name repo. Used only by the email-only PR actor regression: the
/// directory is created with `create_dir` (exclusive, unique per call) so
/// parallel tests never share or remove each other's enclave.
fn forge_hermetic(dir: &Path, args: &[&str]) -> (i32, String, String) {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let bin = env!("CARGO_BIN_EXE_git-forge");
    let mut clean = std::env::temp_dir().join(format!(
        "gf-t3-herm-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    let mut created = false;
    for _ in 0..16 {
        match std::fs::create_dir(&clean) {
            Ok(()) => {
                created = true;
                break;
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                clean = clean.with_extension(format!("r{}", NEXT.fetch_add(1, Ordering::Relaxed)));
            }
            Err(e) => panic!("cannot create hermetic dir {clean:?}: {e}"),
        }
    }
    assert!(
        created,
        "could not reserve a unique hermetic config dir after 16 attempts"
    );
    let out = Command::new(bin)
        .args(args)
        .current_dir(dir)
        .env("HOME", &clean)
        .env("XDG_CONFIG_HOME", &clean)
        .env("XDG_GLOBAL_CONFIG", &clean)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env_remove("GIT_CONFIG_GLOBAL")
        .env_remove("GIT_CONFIG_SYSTEM")
        .output()
        .unwrap();
    let _ = std::fs::remove_dir_all(&clean);
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).trim().to_string(),
        String::from_utf8_lossy(&out.stderr).trim().to_string(),
    )
}

/// Make a feature branch off main with one commit, then return to main.
fn make_feature(dir: &Path, branch: &str, content: &str) {
    let (c, _, e) = git(dir, &["checkout", "-q", "-b", branch]);
    assert!(c == 0, "checkout -b {branch} failed: {e}");
    std::fs::write(dir.join("feature.txt"), content).unwrap();
    let (c, _, e) = git(dir, &["add", "feature.txt"]);
    assert!(c == 0, "add failed: {e}");
    let (c, _, e) = git(dir, &["commit", "-q", "-m", &format!("{branch} commit")]);
    assert!(c == 0, "commit failed: {e}");
    let (c, _, e) = git(dir, &["checkout", "-q", "main"]);
    assert!(c == 0, "checkout main failed: {e}");
}

/// Checkout `branch` so a merge runs while the base (`main`) is NOT checked
/// out anywhere — required by the checked-out base guard.
fn checkout(dir: &Path, branch: &str) {
    let (c, _, e) = git(dir, &["checkout", "-q", branch]);
    assert!(c == 0, "checkout {branch} failed: {e}");
}

fn ref_oid(dir: &Path, r: &str) -> Option<String> {
    let (c, o, _) = git(dir, &["rev-parse", "--verify", "--quiet", r]);
    if c == 0 {
        Some(o)
    } else {
        None
    }
}

fn create_approved_pr(dir: &Path, branch: &str, title: &str) {
    make_feature(dir, branch, "feat\n");
    let (c, o, e) = forge(
        dir,
        &[
            "forge", "pr", "create", "--source", branch, "--base", "main", title,
        ],
    );
    assert!(c == 0, "pr create failed: {e} {o}");
    let (c, o, e) = forge(dir, &["forge", "pr", "review", "1", "--approve"]);
    assert!(c == 0, "pr review failed: {e} {o}");
}

#[test]
fn unapproved_pr_merge_rejected_without_touching_base() {
    let dir = tmpdir("unapproved");
    init_repo(&dir);
    make_feature(&dir, "feature", "feat\n");
    let (c, o, e) = forge(
        &dir,
        &[
            "forge", "pr", "create", "--source", "feature", "--base", "main", "PR1",
        ],
    );
    assert!(c == 0, "create failed: {e} {o}");
    let base_before = ref_oid(&dir, "refs/heads/main");
    let (c, _, e) = forge(&dir, &["forge", "pr", "merge", "1"]);
    assert_ne!(c, 0, "unapproved merge must fail");
    assert!(e.contains("not approved"), "stderr: {e}");
    assert_eq!(
        ref_oid(&dir, "refs/heads/main"),
        base_before,
        "base unchanged"
    );
    // no pr.merge event: show still says no review yet? chain has created + review
    let (cs, os, es) = forge(&dir, &["forge", "pr", "show", "1"]);
    assert!(cs == 0, "show failed: {es}");
    assert!(!os.contains("merged"), "show must not claim merged: {os}");
}

#[test]
fn approve_then_reject_blocks_merge() {
    let dir = tmpdir("approvereject");
    init_repo(&dir);
    make_feature(&dir, "feature", "feat\n");
    assert_eq!(
        forge(
            &dir,
            &["forge", "pr", "create", "--source", "feature", "--base", "main", "PR1"]
        )
        .0,
        0
    );
    assert_eq!(
        forge(&dir, &["forge", "pr", "review", "1", "--approve"]).0,
        0
    );
    assert_eq!(
        forge(&dir, &["forge", "pr", "review", "1", "--reject"]).0,
        0
    );
    let (c, _, e) = forge(&dir, &["forge", "pr", "merge", "1"]);
    assert_ne!(c, 0, "reject after approve must block merge");
    assert!(e.contains("not approved"), "stderr: {e}");
}

#[test]
fn default_merge_creates_merge_commit_and_events() {
    let dir = tmpdir("defaultmerge");
    init_repo(&dir);
    create_approved_pr(&dir, "feature", "PR title");
    checkout(&dir, "feature");
    let base_before = ref_oid(&dir, "refs/heads/main").unwrap();
    let (c, o, e) = forge(&dir, &["forge", "pr", "merge", "1"]);
    assert!(c == 0, "merge failed: {e} {o}");
    let base_after = ref_oid(&dir, "refs/heads/main").unwrap();
    assert_ne!(base_before, base_after, "base must advance");
    // default merge → a merge commit with two parents.
    let (_, parents, _) = git(
        &dir,
        &["rev-list", "--parents", "-n", "1", "refs/heads/main"],
    );
    let n_parents = parents.split_whitespace().count() - 1;
    assert_eq!(
        n_parents, 2,
        "default merge must be a 2-parent merge commit"
    );
    // merged content present
    let (ct, cot, _) = git(&dir, &["ls-tree", "refs/heads/main", "feature.txt"]);
    assert_eq!(ct, 0, "feature.txt missing after merge: {cot}");
    // pr.merge event appended with result_commit
    let (cs, os, es) = forge(&dir, &["forge", "pr", "show", "1"]);
    assert!(cs == 0, "show failed: {es}");
    assert!(os.contains("merged"), "show must reflect merge: {os}");
    let (c2, _, e2) = forge(&dir, &["forge", "pr", "merge", "1"]);
    assert_ne!(c2, 0, "double merge must fail");
    assert!(e2.contains("already merged"), "stderr: {e2}");
    // pending result ref cleaned up
    assert!(
        ref_oid(&dir, "refs/forge/prs/1/result").is_none(),
        "pending ref must be gone"
    );
    // temp worktree removed
    let (wl, out, _) = git(&dir, &["worktree", "list", "--porcelain"]);
    assert_eq!(wl, 0);
    assert!(
        !out.contains("git-forge-pr1-merge"),
        "temp worktree left behind: {out}"
    );
}

#[test]
fn merge_succeeds_from_non_base_branch_with_dirty_main_worktree() {
    let dir = tmpdir("dirty");
    init_repo(&dir);
    make_feature(&dir, "feature", "feat\n");
    assert_eq!(
        forge(
            &dir,
            &["forge", "pr", "create", "--source", "feature", "--base", "main", "PR1"]
        )
        .0,
        0
    );
    assert_eq!(
        forge(&dir, &["forge", "pr", "review", "1", "--approve"]).0,
        0
    );
    // user is on a non-base branch (feature) with an uncommitted change (dirty
    // worktree); main is NOT checked out anywhere.
    checkout(&dir, "feature");
    std::fs::write(dir.join("feature.txt"), "feat\nuncommitted\n").unwrap();
    let (c, o, e) = forge(&dir, &["forge", "pr", "merge", "1"]);
    assert!(c == 0, "merge failed: {e} {o}");
    // user worktree untouched: still on feature, dirty file preserved.
    let (_, br, _) = git(&dir, &["branch", "--show-current"]);
    assert_eq!(br, "feature", "user branch must be untouched");
    assert!(
        std::fs::read_to_string(dir.join("feature.txt"))
            .unwrap()
            .contains("uncommitted"),
        "user dirty file must be untouched"
    );
    // base merged despite dirty other worktree.
    let (ct, cot, _) = git(&dir, &["ls-tree", "refs/heads/main", "feature.txt"]);
    assert_eq!(ct, 0, "feature.txt missing after merge: {cot}");
}

#[test]
fn squash_produces_single_commit() {
    let dir = tmpdir("squash");
    init_repo(&dir);
    create_approved_pr(&dir, "feature", "squashed title");
    checkout(&dir, "feature");
    let (c, o, e) = forge(&dir, &["forge", "pr", "merge", "1", "--squash"]);
    assert!(c == 0, "squash merge failed: {e} {o}");
    let (_, parents, _) = git(
        &dir,
        &["rev-list", "--parents", "-n", "1", "refs/heads/main"],
    );
    let n_parents = parents.split_whitespace().count() - 1;
    assert_eq!(n_parents, 1, "squash must produce a single-parent commit");
    let (_, msg, _) = git(&dir, &["log", "-1", "--format=%s", "refs/heads/main"]);
    assert_eq!(msg, "squashed title", "squash commit must carry PR title");
}

#[test]
fn rebase_replays_snapshot_onto_base() {
    let dir = tmpdir("rebase");
    init_repo(&dir);
    // base moves after PR creation but BEFORE merge? No — stale-base rule would
    // reject. Keep base fixed; feature has 2 commits.
    make_feature(&dir, "feature", "feat1\n");
    // second commit on feature
    let (c, _, e) = git(&dir, &["checkout", "-q", "feature"]);
    assert!(c == 0, "checkout feature: {e}");
    std::fs::write(dir.join("feat2.txt"), "feat2\n").unwrap();
    let (c, _, e) = git(&dir, &["add", "feat2.txt"]);
    assert!(c == 0, "add: {e}");
    let (c, _, e) = git(&dir, &["commit", "-q", "-m", "second"]);
    assert!(c == 0, "commit: {e}");
    let (c, _, e) = git(&dir, &["checkout", "-q", "main"]);
    assert!(c == 0, "checkout main: {e}");
    let (c, o, e) = forge(
        &dir,
        &[
            "forge",
            "pr",
            "create",
            "--source",
            "feature",
            "--base",
            "main",
            "rebase PR",
        ],
    );
    assert!(c == 0, "create: {e} {o}");
    assert_eq!(
        forge(&dir, &["forge", "pr", "review", "1", "--approve"]).0,
        0
    );
    checkout(&dir, "feature");

    let (cm, om, em) = forge(&dir, &["forge", "pr", "merge", "1", "--rebase"]);
    assert!(cm == 0, "rebase merge failed: {em} {om}");
    let (_, parents, _) = git(
        &dir,
        &["rev-list", "--parents", "-n", "1", "refs/heads/main"],
    );
    let n_parents = parents.split_whitespace().count() - 1;
    assert_eq!(n_parents, 1, "rebase must be linear (single-parent tip)");
    let (_cl, log, _) = git(&dir, &["log", "--format=%s", "refs/heads/main"]);
    assert!(log.contains("second"), "rebased commits present: {log}");
    assert!(
        log.contains("rebase PR") || log.contains("feature commit"),
        "rebase PR content: {log}"
    );
    // both feature commits' content merged
    let (ct1, cot1, _) = git(&dir, &["ls-tree", "refs/heads/main", "feature.txt"]);
    assert_eq!(ct1, 0, "feature.txt missing: {cot1}");
    let (ct2, cot2, _) = git(&dir, &["ls-tree", "refs/heads/main", "feat2.txt"]);
    assert_eq!(ct2, 0, "feat2.txt missing: {cot2}");
}

#[test]
fn stale_base_rejects_merge_without_ref_changes() {
    let dir = tmpdir("stale");
    init_repo(&dir);
    create_approved_pr(&dir, "feature", "stale PR");
    let base_before = ref_oid(&dir, "refs/heads/main").unwrap();
    // advance main after PR creation
    let (c, _, e) = git(&dir, &["checkout", "-q", "main"]);
    assert!(c == 0, "checkout main: {e}");
    std::fs::write(dir.join("later.txt"), "later\n").unwrap();
    let (c, _, e) = git(&dir, &["add", "later.txt"]);
    assert!(c == 0, "add: {e}");
    let (c, _, e) = git(&dir, &["commit", "-q", "-m", "later"]);
    assert!(c == 0, "commit: {e}");
    let new_base = ref_oid(&dir, "refs/heads/main").unwrap();
    assert_ne!(base_before, new_base);

    let (cm, _, em) = forge(&dir, &["forge", "pr", "merge", "1"]);
    assert_ne!(cm, 0, "stale base must reject merge");
    assert!(em.contains("moved") || em.contains("stale"), "stderr: {em}");
    assert_eq!(
        ref_oid(&dir, "refs/heads/main").unwrap(),
        new_base,
        "base unchanged"
    );
    let (_cs, os, _) = forge(&dir, &["forge", "pr", "show", "1"]);
    assert!(
        !os.contains("merged"),
        "no pr.merge on stale rejection: {os}"
    );
}

#[test]
fn checked_out_base_refused() {
    let dir = tmpdir("checkedout");
    init_repo(&dir);
    create_approved_pr(&dir, "feature", "co PR");
    // user is ON main (base checked out) → merge must refuse.
    let (c, _, e) = forge(&dir, &["forge", "pr", "merge", "1"]);
    assert_ne!(c, 0, "merge must refuse when base checked out");
    assert!(e.contains("checked out"), "stderr: {e}");
}

#[test]
fn conflict_aborts_cleans_and_leaves_refs_unchanged() {
    let dir = tmpdir("conflict");
    init_repo(&dir);
    // base.txt exists with "base\n"; feature modifies it differently.
    make_feature(&dir, "feature", "feat\n");
    // modify base.txt on feature (conflicting)
    let (c, _, e) = git(&dir, &["checkout", "-q", "feature"]);
    assert!(c == 0, "co: {e}");
    std::fs::write(dir.join("base.txt"), "feature-change\n").unwrap();
    let (c, _, e) = git(&dir, &["add", "base.txt"]);
    assert!(c == 0, "add: {e}");
    let (c, _, e) = git(&dir, &["commit", "-q", "-m", "conflict"]);
    assert!(c == 0, "commit: {e}");
    let (c, _, e) = git(&dir, &["checkout", "-q", "main"]);
    assert!(c == 0, "co main: {e}");
    // also change base.txt on main
    std::fs::write(dir.join("base.txt"), "main-change\n").unwrap();
    let (c, _, e) = git(&dir, &["add", "base.txt"]);
    assert!(c == 0, "add: {e}");
    let (c, _, e) = git(&dir, &["commit", "-q", "-m", "main change"]);
    assert!(c == 0, "commit: {e}");
    let (c, o, e) = forge(
        &dir,
        &[
            "forge",
            "pr",
            "create",
            "--source",
            "feature",
            "--base",
            "main",
            "conflict PR",
        ],
    );
    assert!(c == 0, "create: {e} {o}");
    assert_eq!(
        forge(&dir, &["forge", "pr", "review", "1", "--approve"]).0,
        0
    );
    checkout(&dir, "feature");

    let base_before = ref_oid(&dir, "refs/heads/main").unwrap();
    let (cm, _, em) = forge(&dir, &["forge", "pr", "merge", "1"]);
    assert_ne!(cm, 0, "conflict must fail: {em}");
    assert_eq!(
        ref_oid(&dir, "refs/heads/main").unwrap(),
        base_before,
        "base unchanged"
    );
    // temp worktree removed and no registration remains
    let (wl, out, _) = git(&dir, &["worktree", "list", "--porcelain"]);
    assert_eq!(wl, 0);
    assert!(!out.contains("git-forge-pr1-merge"), "worktree left: {out}");
    let (_cs, os, _) = forge(&dir, &["forge", "pr", "show", "1"]);
    assert!(!os.contains("merged"), "no pr.merge on conflict: {os}");
}

#[test]
fn failing_hook_aborts_merge_cleans_worktree() {
    let dir = tmpdir("hookfail");
    init_repo(&dir);
    create_approved_pr(&dir, "feature", "hook PR");
    checkout(&dir, "feature");
    // failing commit-msg hook (runs on the merge commit path)
    let hooks = dir.join(".git").join("hooks");
    std::fs::create_dir_all(&hooks).unwrap();
    let hook_path = hooks.join("commit-msg");
    std::fs::write(&hook_path, "#!/bin/sh\nexit 1\n").unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&hook_path, std::fs::Permissions::from_mode(0o755)).unwrap();
    let base_before = ref_oid(&dir, "refs/heads/main").unwrap();
    let (cm, _, em) = forge(&dir, &["forge", "pr", "merge", "1"]);
    assert_ne!(cm, 0, "failing hook must abort merge: {em}");
    assert_eq!(
        ref_oid(&dir, "refs/heads/main").unwrap(),
        base_before,
        "base unchanged"
    );
    let (wl, out, _) = git(&dir, &["worktree", "list", "--porcelain"]);
    assert_eq!(wl, 0);
    assert!(!out.contains("git-forge-pr1-merge"), "worktree left: {out}");
    let (_cs, os, _) = forge(&dir, &["forge", "pr", "show", "1"]);
    assert!(!os.contains("merged"), "no pr.merge on hook failure: {os}");
}

#[test]
fn locked_temp_worktree_on_hook_failure_is_preserved_and_reported() {
    let dir = tmpdir("lockedhook");
    init_repo(&dir);
    create_approved_pr(&dir, "feature", "locked hook PR");
    checkout(&dir, "feature");
    // failing pre-merge-commit hook that LOCKS its own (temp) worktree before
    // failing: `git worktree remove --force` cannot remove a locked worktree,
    // so the cleanup path must preserve the leftover and report it instead of
    // deleting the directory under a live registration (AC-005d).
    let hooks = dir.join(".git").join("hooks");
    std::fs::create_dir_all(&hooks).unwrap();
    let hook_path = hooks.join("pre-merge-commit");
    std::fs::write(
        &hook_path,
        "#!/usr/bin/env bash\ngit worktree lock --reason 'test' \"$(pwd)\"\nexit 1\n",
    )
    .unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&hook_path, std::fs::Permissions::from_mode(0o755)).unwrap();
    let base_before = ref_oid(&dir, "refs/heads/main").unwrap();
    let (cm, _, em) = forge(&dir, &["forge", "pr", "merge", "1"]);
    assert_ne!(cm, 0, "failing hook must abort merge: {em}");
    assert!(
        em.contains("worktree is left at"),
        "stderr must report the leftover worktree path: {em}"
    );
    assert_eq!(
        ref_oid(&dir, "refs/heads/main").unwrap(),
        base_before,
        "base unchanged"
    );
    // The locked temp worktree must still be REGISTERED with its directory
    // intact (never a dangling registration), and reported as leftover.
    let (wl, out, _) = git(&dir, &["worktree", "list", "--porcelain"]);
    assert_eq!(wl, 0);
    let mut leftover: Option<String> = None;
    for line in out.lines() {
        if let Some(p) = line.strip_prefix("worktree ") {
            if p.contains("git-forge-pr1-merge") {
                assert!(
                    std::path::Path::new(p).is_dir(),
                    "leftover worktree directory must still exist: {p}"
                );
                leftover = Some(p.to_string());
            }
        }
    }
    assert!(
        leftover.is_some(),
        "locked temp worktree must remain registered after cleanup: {out}"
    );
    // The stderr message must carry the EXACT leftover path, delimited by
    // quotes (actionable contract: an operator can unlock/remove it without
    // re-deriving it; quoting keeps paths with spaces parseable). macOS /var
    // is a symlink to /private/var; git canonicalizes the registration while
    // temp_dir() does not, so compare canonical forms.
    let reported = em
        .split("worktree is left at ")
        .nth(1)
        .and_then(|s| s.split('"').nth(1))
        .unwrap_or_default();
    let canon = |p: &str| std::fs::canonicalize(p).unwrap_or_else(|_| std::path::PathBuf::from(p));
    assert_eq!(
        canon(reported),
        canon(leftover.as_deref().unwrap_or("")),
        "reported leftover path must match the registered worktree: {em}"
    );
    // No pr.merge; hygiene: unlock + remove the leftover to leave no residue.
    let (_cs, os, _) = forge(&dir, &["forge", "pr", "show", "1"]);
    assert!(!os.contains("merged"), "no pr.merge on hook failure: {os}");
    let p = leftover.unwrap();
    let (u, _, eu) = git(&dir, &["worktree", "unlock", &p]);
    assert!(u == 0, "unlock failed: {eu}");
    let (r, _, er) = git(&dir, &["worktree", "remove", "--force", &p]);
    assert!(r == 0, "remove failed: {er}");
    let lock = std::path::PathBuf::from(format!("{p}.lock"));
    assert!(
        !lock.exists(),
        "sibling lock file must be released on cleanup failure: {}",
        lock.display()
    );
}

#[test]
fn squash_failing_hook_resets_worktree_no_refs() {
    let dir = tmpdir("squashhook");
    init_repo(&dir);
    create_approved_pr(&dir, "feature", "squash hook PR");
    checkout(&dir, "feature");
    // failing commit-msg hook (runs on `git commit -m` in the squash path)
    let hooks = dir.join(".git").join("hooks");
    std::fs::create_dir_all(&hooks).unwrap();
    let hook_path = hooks.join("commit-msg");
    std::fs::write(&hook_path, "#!/bin/sh\nexit 1\n").unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&hook_path, std::fs::Permissions::from_mode(0o755)).unwrap();
    let base_before = ref_oid(&dir, "refs/heads/main").unwrap();
    let (cm, _, em) = forge(&dir, &["forge", "pr", "merge", "1", "--squash"]);
    assert_ne!(cm, 0, "failing squash hook must abort: {em}");
    assert_eq!(
        ref_oid(&dir, "refs/heads/main").unwrap(),
        base_before,
        "base unchanged"
    );
    let (wl, out, _) = git(&dir, &["worktree", "list", "--porcelain"]);
    assert_eq!(wl, 0);
    assert!(!out.contains("git-forge-pr1-merge"), "worktree left: {out}");
    let (_cs, os, _) = forge(&dir, &["forge", "pr", "show", "1"]);
    assert!(
        !os.contains("merged"),
        "no pr.merge on squash hook failure: {os}"
    );
}

#[test]
fn merge_flag_parser_accepts_explicit_and_rejects_conflicts() {
    let dir = tmpdir("flags");
    init_repo(&dir);
    create_approved_pr(&dir, "feature", "flag PR");
    checkout(&dir, "feature");
    let base_before = ref_oid(&dir, "refs/heads/main").unwrap();
    // explicit --merge works
    let (cm, om, em) = forge(&dir, &["forge", "pr", "merge", "1", "--merge"]);
    assert!(cm == 0, "explicit --merge failed: {em} {om}");
    assert_ne!(ref_oid(&dir, "refs/heads/main").unwrap(), base_before);

    // conflicting strategy flags on a second (unmerged, fresh) PR
    let dir2 = tmpdir("flags-conflict");
    init_repo(&dir2);
    create_approved_pr(&dir2, "feature", "conflict flags PR");
    checkout(&dir2, "feature");
    let (cc, _, ec) = forge(
        &dir2,
        &["forge", "pr", "merge", "1", "--squash", "--rebase"],
    );
    assert_ne!(cc, 0, "conflicting flags must be rejected");
    assert!(
        ec.contains("cannot combine") || ec.contains("more than once"),
        "stderr: {ec}"
    );
    // duplicate id rejected
    let (cd, _, ed) = forge(&dir2, &["forge", "pr", "merge", "1", "1"]);
    assert_ne!(cd, 0, "duplicate id must be rejected");
    assert!(ed.contains("one PR id"), "stderr: {ed}");
    // repeated identical flag rejected
    let (cr, _, er) = forge(
        &dir2,
        &["forge", "pr", "merge", "1", "--squash", "--squash"],
    );
    assert_ne!(cr, 0, "repeated strategy flag must be rejected");
    assert!(
        er.contains("more than once") || er.contains("cannot"),
        "stderr: {er}"
    );
}

#[test]
fn worktree_removal_failure_reports_and_cleans_pending_ref() {
    let dir = tmpdir("rmfail");
    init_repo(&dir);
    create_approved_pr(&dir, "feature", "rm fail PR");
    checkout(&dir, "feature");
    let base_before = ref_oid(&dir, "refs/heads/main").unwrap();
    let bin = env!("CARGO_BIN_EXE_git-forge");
    // The seam locks the temp worktree so `worktree remove --force` fails.
    let out = Command::new(bin)
        .args(["forge", "pr", "merge", "1"])
        .current_dir(&dir)
        .env("GIT_FORGE_TEST_FAIL_WORKTREE_REMOVE", "1")
        .output()
        .unwrap();
    let code = out.status.code().unwrap_or(-1);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_ne!(code, 0, "merge must fail on removal failure");
    assert!(
        stderr.contains("removal failed")
            || stderr.contains("still registered")
            || stderr.contains("worktree"),
        "stderr must report the leftover: {stderr}"
    );
    // No ref changes: base and PR chain unchanged; result ref best-effort cleaned.
    assert_eq!(
        ref_oid(&dir, "refs/heads/main").unwrap(),
        base_before,
        "base unchanged"
    );
    let (cs, os, _) = forge(&dir, &["forge", "pr", "show", "1"]);
    assert!(cs == 0, "pr show must succeed after failed removal");
    assert!(
        !os.contains("merged"),
        "no pr.merge on removal failure: {os}"
    );
    // On removal failure: best-effort delete of pending /result (CAS expected
    // oid) succeeds, so the ref is gone; base/PR chain unchanged.
    assert!(
        ref_oid(&dir, "refs/forge/prs/1/result").is_none(),
        "pending result ref must be cleaned best-effort after removal failure"
    );
    // Hygiene: unlock + remove the intentionally locked temp worktree, then
    // verify the EXACT sibling lock file for that path was released.
    let (wl, out2, _) = git(&dir, &["worktree", "list", "--porcelain"]);
    assert_eq!(wl, 0);
    let mut removed_any = false;
    let mut temp_path: Option<std::path::PathBuf> = None;
    for line in out2.lines() {
        if let Some(p) = line.strip_prefix("worktree ") {
            if p.contains("git-forge-pr1-merge") {
                temp_path = Some(std::path::PathBuf::from(p));
                let (u, _, eu) = git(&dir, &["worktree", "unlock", p]);
                assert!(u == 0, "unlock failed: {eu}");
                let (r, _, er) = git(&dir, &["worktree", "remove", "--force", p]);
                assert!(r == 0, "remove failed: {er}");
                removed_any = true;
            }
        }
    }
    assert!(
        removed_any,
        "expected the locked temp worktree to be cleaned"
    );
    if let Some(tp) = temp_path {
        let lock = std::path::PathBuf::from(format!("{}.lock", tp.display()));
        assert!(
            !lock.exists(),
            "sibling lock file must be released on removal failure: {}",
            lock.display()
        );
    }
}

#[test]
fn seam_lock_failure_releases_lock_and_cleans_pending_ref() {
    // F-019 regression: when the debug seam's own `git worktree lock` call
    // fails (shim forces it), the merge must route through the shared cleanup
    // — release the sibling path lock and best-effort delete the pending
    // result ref — instead of returning with both leaked. This is the
    // deterministic counterpart to the (harder-to-trigger) in-process seam
    // path; the shim intercepts `git -C <repo> worktree lock <tmp>` and exits
    // 128 with a distinctive stderr, forwarding every other git call.
    let dir = tmpdir("seamlockfail");
    init_repo(&dir);
    create_approved_pr(&dir, "feature", "seam lock fail PR");
    checkout(&dir, "feature");
    let base_before = ref_oid(&dir, "refs/heads/main").unwrap();

    let shim = tmpdir("seamlockfail-shim");
    let which = Command::new("sh")
        .arg("-c")
        .arg("command -v git")
        .output()
        .unwrap();
    let real_git = String::from_utf8_lossy(&which.stdout).trim().to_string();
    assert!(!real_git.is_empty(), "could not resolve real git path");
    let shim_script = format!(
        "#!/bin/sh\n\
         if [ \"$1\" = \"-C\" ] && [ \"$3\" = \"worktree\" ] && [ \"$4\" = \"lock\" ]; then\n\
         \x20 echo \"fatal: worktree lock disabled by test shim\" >&2\n\
         \x20 exit 128\n\
         fi\n\
         exec \"{real_git}\" \"$@\"\n"
    );
    std::fs::write(shim.join("git"), shim_script).unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(shim.join("git"), std::fs::Permissions::from_mode(0o755)).unwrap();

    let mut path = shim.display().to_string();
    if let Ok(p) = std::env::var("PATH") {
        path.push(':');
        path.push_str(&p);
    }
    let bin = env!("CARGO_BIN_EXE_git-forge");
    let out = Command::new(bin)
        .args(["forge", "pr", "merge", "1"])
        .current_dir(&dir)
        .env("PATH", &path)
        .env("GIT_FORGE_TEST_FAIL_WORKTREE_REMOVE", "1")
        .output()
        .unwrap();
    let code = out.status.code().unwrap_or(-1);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_ne!(code, 0, "merge must fail on seam lock failure");
    assert!(
        stderr.contains("worktree lock disabled by test shim"),
        "stderr must surface the injected worktree lock failure: {stderr}"
    );
    // Base unchanged, PR chain untouched, no merge committed.
    assert_eq!(
        ref_oid(&dir, "refs/heads/main").unwrap(),
        base_before,
        "base unchanged"
    );
    let (cs, os, _) = forge(&dir, &["forge", "pr", "show", "1"]);
    assert!(cs == 0, "pr show must succeed after seam lock failure");
    assert!(!os.contains("merged"), "no pr.merge: {os}");
    // The shared cleanup ran: the pending result ref is gone (CAS expected
    // oid, best-effort) and no sibling lock file survives for the path.
    assert!(
        ref_oid(&dir, "refs/forge/prs/1/result").is_none(),
        "pending result ref must be cleaned best-effort on seam lock failure"
    );
    let (wl, out2, _) = git(&dir, &["worktree", "list", "--porcelain"]);
    assert_eq!(wl, 0);
    let mut temp_path: Option<std::path::PathBuf> = None;
    for line in out2.lines() {
        if let Some(p) = line.strip_prefix("worktree ") {
            if p.contains("git-forge-pr1-merge") {
                temp_path = Some(std::path::PathBuf::from(p));
            }
        }
    }
    // The leftover temp worktree MUST exist (the seam failed before removing
    // it) and its sibling lock MUST already be released by the shared cleanup.
    assert!(
        temp_path.is_some(),
        "expected a leftover temp worktree (git-forge-pr1-merge) after seam lock failure"
    );
    if let Some(tp) = temp_path {
        let lock = std::path::PathBuf::from(format!("{}.lock", tp.display()));
        assert!(
            !lock.exists(),
            "sibling lock file must be released on seam lock failure: {}",
            lock.display()
        );
        // Hygiene: remove the leftover worktree so the repo is clean for any
        // later assertion. It was never locked (the seam failed before
        // locking), so only a plain remove is needed.
        let (r, _, er) = git(
            &dir,
            &["worktree", "remove", "--force", tp.to_str().unwrap()],
        );
        assert!(r == 0, "remove failed: {er}");
    }
}

#[test]
fn worktree_verification_failure_surfaces_raw_git_stderr() {
    let dir = tmpdir("verifyfail");
    init_repo(&dir);
    create_approved_pr(&dir, "feature", "verify fail PR");
    checkout(&dir, "feature");
    let base_before = ref_oid(&dir, "refs/heads/main").unwrap();

    // Deterministic seam (F-008): a PATH shim around `git` that fails the
    // SECOND `git worktree list --porcelain` invocation with a distinctive
    // raw stderr. In `cmd_pr_merge` the worktree-list call order is fixed:
    // #1 = checked-out base guard (must succeed), #2 = post-removal
    // verification (must fail, surfacing the raw stderr). Every other git
    // invocation execs the real binary unchanged.
    let shim = tmpdir("verifyfail-shim");
    let counter = shim.join("counter");
    let which = Command::new("sh")
        .arg("-c")
        .arg("command -v git")
        .output()
        .unwrap();
    assert!(which.status.success(), "command -v git failed");
    let real_git = String::from_utf8_lossy(&which.stdout).trim().to_string();
    assert!(!real_git.is_empty(), "could not resolve real git path");
    let shim_script = format!(
        "#!/bin/sh\n\
         if [ \"$1\" = \"-C\" ] && [ \"$3\" = \"worktree\" ] && [ \"$4\" = \"list\" ]; then\n\
         \x20 n=0\n\
         \x20 [ -f \"$COUNTER\" ] && n=$(cat \"$COUNTER\")\n\
         \x20 n=$((n+1))\n\
         \x20 echo \"$n\" > \"$COUNTER\"\n\
         \x20 if [ \"$n\" -ge 2 ]; then\n\
         \x20   echo \"fatal: worktree list disabled by test shim\" >&2\n\
         \x20   exit 128\n\
         \x20 fi\n\
         fi\n\
         exec \"{real_git}\" \"$@\"\n"
    );
    std::fs::write(shim.join("git"), shim_script).unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(shim.join("git"), std::fs::Permissions::from_mode(0o755)).unwrap();

    let mut path = shim.display().to_string();
    if let Ok(p) = std::env::var("PATH") {
        path.push(':');
        path.push_str(&p);
    }
    let bin = env!("CARGO_BIN_EXE_git-forge");
    let out = Command::new(bin)
        .args(["forge", "pr", "merge", "1"])
        .current_dir(&dir)
        .env("PATH", &path)
        .env("COUNTER", &counter)
        .output()
        .unwrap();
    let code = out.status.code().unwrap_or(-1);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_ne!(
        code, 0,
        "merge must fail when post-removal verification cannot run"
    );
    assert!(
        stderr.contains(
            "merge succeeded but worktree verification failed (git worktree list: fatal: worktree list disabled by test shim)"
        ),
        "verification-path error must surface RAW git stderr: {stderr}"
    );
    // No ref changes: base and PR chain unchanged; result ref best-effort cleaned.
    assert_eq!(
        ref_oid(&dir, "refs/heads/main").unwrap(),
        base_before,
        "base unchanged"
    );
    assert!(
        ref_oid(&dir, "refs/forge/prs/1/result").is_none(),
        "pending result ref must be cleaned best-effort after verification failure"
    );
    let (cs, os, _) = forge(&dir, &["forge", "pr", "show", "1"]);
    assert!(cs == 0, "pr show must succeed after verification failure");
    assert!(
        !os.contains("merged"),
        "no pr.merge on verification failure: {os}"
    );
}

#[test]
fn nonexistent_pr_merge_is_clean_error() {
    let dir = tmpdir("nopr");
    init_repo(&dir);
    let (c, _, e) = forge(&dir, &["forge", "pr", "merge", "42"]);
    assert_ne!(c, 0);
    assert!(e.contains("#42"), "stderr: {e}");
}

/// Every CLI-written PR event (create/comment/review/merge) must carry the
/// invoking repo's configured `user.email` as its actor (wire contract:
/// `"actor": "<user.email>"`). The actor lives in the event JSON, so the
/// assertion reads the raw chain JSON, not the folded state.
fn pr_chain_actors(dir: &Path, ref_name: &str) -> Vec<String> {
    let rev = git(dir, &["rev-list", ref_name]);
    assert_eq!(rev.0, 0, "rev-list {ref_name} failed: {}", rev.2);
    let mut actors = Vec::new();
    for oid in rev.1.split_whitespace() {
        let show = git(dir, &["show", &format!("{oid}:.forge/event.json")]);
        if show.0 != 0 {
            continue; // genesis root has no event.json
        }
        let json = show.1;
        let key = "\"actor\":";
        let idx = json.find(key).expect("event json must carry actor") + key.len();
        let value: String = json[idx..]
            .chars()
            .skip_while(|c| *c == ' ' || *c == '"')
            .take_while(|c| *c != '"')
            .collect();
        actors.push(value);
    }
    actors
}

#[test]
fn pr_chain_events_carry_configured_user_email_as_actor() {
    let dir = tmpdir("practor");
    init_repo(&dir);
    // Distinct from the fixture identity so the assertion proves the actor
    // comes from the repo config, not from a hardcoded fallback.
    let (c, _, e) = git(&dir, &["config", "user.email", "someone@example.com"]);
    assert_eq!(c, 0, "config user.email failed: {e}");
    make_feature(&dir, "feature", "feat\n");
    let (c, o, e) = forge(
        &dir,
        &[
            "forge", "pr", "create", "--source", "feature", "--base", "main", "PR",
        ],
    );
    assert_eq!(c, 0, "pr create failed: {e} {o}");
    assert_eq!(forge(&dir, &["forge", "pr", "comment", "1", "hello"]).0, 0);
    assert_eq!(
        forge(&dir, &["forge", "pr", "review", "1", "--approve"]).0,
        0
    );
    checkout(&dir, "feature");
    let (cm, om, em) = forge(&dir, &["forge", "pr", "merge", "1"]);
    assert_eq!(cm, 0, "pr merge failed: {em} {om}");

    let actors = pr_chain_actors(&dir, "refs/forge/prs/1/head");
    assert_eq!(
        actors.len(),
        4,
        "four events: pr.created + pr.comment + pr.review + pr.merge"
    );
    assert!(
        actors.iter().all(|a| a == "someone@example.com"),
        "every PR event actor must equal configured user.email, got: {actors:?}"
    );
}

#[test]
fn pr_events_carry_user_email_when_only_email_is_configured() {
    // F-014 regression on the PR side: with ONLY user.email configured (no
    // user.name), PR create/comment/review events must still carry that email
    // as actor. Setup (branch + commits) runs while user.name is set (git
    // commits need an identity); immediately before the forge phase we unset
    // user.name so the store's actor path — which is required to read the
    // email via repo.config(), never via git2 Repository::signature() — is
    // actually exercised in the name-less state. forge_hermetic keeps any
    // machine/user-level identity from leaking back in.
    let dir = tmpdir("practor-email-only");
    init_email_only_test_repo(&dir);
    // PR merge is intentionally NOT part of this test: the git-level merge
    // itself needs a committer identity (user.name), which is out of scope for
    // the store's event-actor contract and covered by the name-present suite.
    let (c, o, e) = forge_hermetic(
        &dir,
        &[
            "forge", "pr", "create", "--source", "feature", "--base", "main", "PR",
        ],
    );
    assert_eq!(c, 0, "email-only pr create failed: {e} {o}");
    assert_eq!(
        forge_hermetic(&dir, &["forge", "pr", "comment", "1", "hello"]).0,
        0
    );
    assert_eq!(
        forge_hermetic(&dir, &["forge", "pr", "review", "1", "--approve"]).0,
        0
    );

    let actors = pr_chain_actors(&dir, "refs/forge/prs/1/head");
    assert_eq!(actors.len(), 3, "pr.created + pr.comment + pr.review");
    assert!(
        actors.iter().all(|a| a == "practor@example.com"),
        "email-only PR events must carry the configured email, got: {actors:?}"
    );
}

#[test]
fn barrier_holds_pending_ref_through_gc() {
    let dir = tmpdir("barrier");
    init_repo(&dir);
    create_approved_pr(&dir, "feature", "barrier PR");
    checkout(&dir, "feature");
    let barrier = tmpdir("barrier-window");
    let base_before = ref_oid(&dir, "refs/heads/main").unwrap();

    // Spawn the merge with the barrier env; it blocks after pending ref exists.
    let bin = env!("CARGO_BIN_EXE_git-forge");
    let mut child: Child = Command::new(bin)
        .args(["forge", "pr", "merge", "1"])
        .current_dir(&dir)
        .env("GIT_FORGE_TEST_MERGE_BARRIER", &barrier)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();

    // Wait for ready sentinel (bounded).
    let deadline = Instant::now() + Duration::from_secs(30);
    while !barrier.join("ready").exists() {
        assert!(
            Instant::now() < deadline,
            "merge never reached the barrier window"
        );
        std::thread::sleep(Duration::from_millis(20));
    }

    // While pending: base/PR head unchanged, pending result ref exists and
    // survives a hard gc.
    assert_eq!(
        ref_oid(&dir, "refs/heads/main").unwrap(),
        base_before,
        "base must be unchanged while merge is pending"
    );
    let pending = ref_oid(&dir, "refs/forge/prs/1/result");
    assert!(
        pending.is_some(),
        "pending result ref must exist in barrier window"
    );
    let pending_oid = pending.unwrap();
    let (gc, _, gce) = git(&dir, &["gc", "--prune=now", "--quiet"]);
    assert_eq!(gc, 0, "gc failed: {gce}");
    let (ce, _, cee) = git(
        &dir,
        &["cat-file", "-e", &format!("{pending_oid}^{{commit}}")],
    );
    assert_eq!(ce, 0, "pending result commit pruned by gc: {cee}");
    assert_eq!(
        ref_oid(&dir, "refs/heads/main").unwrap(),
        base_before,
        "base unchanged across gc while pending"
    );

    // Release the barrier atomically (O_CREAT|O_EXCL to match the seam's
    // sentinel protocol); merge completes.
    let release_path = barrier.join("release");
    {
        use std::io::Write as _;
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&release_path)
            .expect("release sentinel must be created atomically");
        let _ = f.write_all(b"go\n");
    }
    let status = child.wait().unwrap();
    assert!(status.success(), "merge must succeed after release");
    let after = ref_oid(&dir, "refs/heads/main").unwrap();
    assert_ne!(after, base_before, "base advances after released barrier");
    assert!(
        ref_oid(&dir, "refs/forge/prs/1/result").is_none(),
        "pending ref cleaned after merge"
    );
    let (_cs, os, _) = forge(&dir, &["forge", "pr", "show", "1"]);
    assert!(os.contains("merged"), "pr.merge recorded: {os}");
}

// ── MERGE-path error/cleanup coverage (uncovered branches) ────────────────
//
// The tests below exercise the remaining uncovered merge-execution, worktree
// and cleanup branches in src/pr_merge.rs and src/git.rs, each via a real
// `git forge pr merge` invocation in a temp repo. Deterministic failure is
// injected with either real git behavior (conflicts, hooks), a debug-only env
// seam, or a PATH shim that selectively fails/records specific `git` calls.

/// Write an executable `git` shim into a fresh temp dir and return the dir.
/// The script defines `REAL_GIT` (resolved via PATH), runs `body`, then
/// falls through to `exec "$REAL_GIT" "$@"`. `body` may reference `$1..$9`
/// (git's argv) and `$REAL_GIT` for pass-through subprocesses.
fn make_git_shim(tag: &str, body: &str) -> PathBuf {
    let dir = tmpdir(tag);
    let which = Command::new("sh")
        .arg("-c")
        .arg("command -v git")
        .output()
        .unwrap();
    let real_git = String::from_utf8_lossy(&which.stdout).trim().to_string();
    assert!(!real_git.is_empty(), "could not resolve real git path");
    let script = format!("#!/bin/sh\nREAL_GIT=\"{real_git}\"\n{body}\nexec \"$REAL_GIT\" \"$@\"\n");
    std::fs::write(dir.join("git"), script).unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(dir.join("git"), std::fs::Permissions::from_mode(0o755)).unwrap();
    dir
}

/// Run the git-forge binary with an optional PATH shim (its dir prepended to
/// PATH) and extra env vars, in `dir`.
fn run_forge(
    dir: &Path,
    shim: Option<&Path>,
    envs: &[(&str, &str)],
    args: &[&str],
) -> (i32, String, String) {
    let bin = env!("CARGO_BIN_EXE_git-forge");
    let mut cmd = Command::new(bin);
    cmd.args(args).current_dir(dir);
    if let Some(s) = shim {
        let mut path = s.display().to_string();
        if let Ok(p) = std::env::var("PATH") {
            path.push(':');
            path.push_str(&p);
        }
        cmd.env("PATH", &path);
    }
    for (k, v) in envs {
        cmd.env(k, v);
    }
    let out = cmd.output().unwrap();
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).trim().to_string(),
        String::from_utf8_lossy(&out.stderr).trim().to_string(),
    )
}

/// Assert no temp worktree remains registered (hygiene contract shared by the
/// cleanup-path tests): `git worktree list --porcelain` must not contain the
/// disposable `git-forge-pr1-merge` path.
fn assert_no_temp_worktree(dir: &Path) {
    let (wl, out, _) = git(dir, &["worktree", "list", "--porcelain"]);
    assert_eq!(wl, 0, "worktree list must succeed");
    assert!(
        !out.contains("git-forge-pr1-merge"),
        "temp worktree left behind: {out}"
    );
}

#[test]
fn merge_explicit_merge_flag_conflict_rejected() {
    // pr_merge.rs:24 — the `--merge` arm's duplicate-flag check. The existing
    // flag test only covers conflicts whose FIRST strategy flag is --squash/
    // --rebase; this covers --merge followed by a second strategy flag.
    let dir = tmpdir("mergeflag");
    init_repo(&dir);
    create_approved_pr(&dir, "feature", "merge flag PR");
    checkout(&dir, "feature");
    let base_before = ref_oid(&dir, "refs/heads/main").unwrap();
    // `--merge` as the FIRST strategy flag followed by a second one hits the
    // `--merge` arm's duplicate check (pr_merge.rs:24); `--squash --merge`
    // would hit the later arms, already covered by the existing flag test.
    let (c, _, e) = forge(&dir, &["forge", "pr", "merge", "1", "--merge", "--merge"]);
    assert_ne!(c, 0, "conflicting strategy flags must be rejected");
    assert!(
        e.contains("merge strategy flag specified more than once"),
        "stderr: {e}"
    );
    assert_eq!(
        ref_oid(&dir, "refs/heads/main").unwrap(),
        base_before,
        "base unchanged"
    );
}

#[test]
fn merge_missing_snapshot_source_ref_rejected() {
    // pr_merge.rs:78-82 — `PR #N snapshot source ref missing` (the immutable
    // snapshot ref is deleted, e.g. by an external cleanup).
    let dir = tmpdir("srcmiss");
    init_repo(&dir);
    create_approved_pr(&dir, "feature", "src miss PR");
    checkout(&dir, "feature");
    let base_before = ref_oid(&dir, "refs/heads/main").unwrap();
    let (c, _, e) = git(&dir, &["update-ref", "-d", "refs/forge/prs/1/source"]);
    assert!(c == 0, "delete source ref: {e}");
    let (cm, _, em) = forge(&dir, &["forge", "pr", "merge", "1"]);
    assert_ne!(cm, 0, "missing snapshot source ref must fail the merge");
    assert!(
        em.contains("PR #1 snapshot source ref missing"),
        "stderr: {em}"
    );
    assert_eq!(
        ref_oid(&dir, "refs/heads/main").unwrap(),
        base_before,
        "base unchanged"
    );
    let (_cs, os, _) = forge(&dir, &["forge", "pr", "show", "1"]);
    assert!(!os.contains("merged"), "no pr.merge: {os}");
}

#[test]
fn merge_missing_snapshot_base_ref_rejected() {
    // pr_merge.rs:83-87 — `PR #N snapshot base ref missing`.
    let dir = tmpdir("basemiss");
    init_repo(&dir);
    create_approved_pr(&dir, "feature", "base miss PR");
    checkout(&dir, "feature");
    let base_before = ref_oid(&dir, "refs/heads/main").unwrap();
    let (c, _, e) = git(&dir, &["update-ref", "-d", "refs/forge/prs/1/base"]);
    assert!(c == 0, "delete base ref: {e}");
    let (cm, _, em) = forge(&dir, &["forge", "pr", "merge", "1"]);
    assert_ne!(cm, 0, "missing snapshot base ref must fail the merge");
    assert!(
        em.contains("PR #1 snapshot base ref missing"),
        "stderr: {em}"
    );
    assert_eq!(
        ref_oid(&dir, "refs/heads/main").unwrap(),
        base_before,
        "base unchanged"
    );
}

#[test]
fn merge_unwritable_tmpdir_reports_reservation_failure() {
    // pr_merge.rs:182-192 (lock retry) + 196-200 (exhausted reservation).
    // TMPDIR points at a regular FILE, so every sibling-lock `create_new`
    // fails with ENOTDIR and all 16 reservation attempts fail. A file (not a
    // chmod-000 dir) keeps the test immune to running as root.
    let dir = tmpdir("tmpdirfail");
    init_repo(&dir);
    create_approved_pr(&dir, "feature", "tmpdir PR");
    checkout(&dir, "feature");
    let base_before = ref_oid(&dir, "refs/heads/main").unwrap();
    let file = dir.join("not-a-dir");
    std::fs::write(&file, "x").unwrap();
    let bin = env!("CARGO_BIN_EXE_git-forge");
    let out = Command::new(bin)
        .args(["forge", "pr", "merge", "1"])
        .current_dir(&dir)
        .env("TMPDIR", &file)
        .output()
        .unwrap();
    let code = out.status.code().unwrap_or(-1);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_ne!(code, 0, "merge must fail when no temp path can be reserved");
    assert!(
        stderr.contains("failed to create temporary worktree")
            && stderr.contains("could not reserve a unique temp worktree path after 16 attempts"),
        "stderr: {stderr}"
    );
    assert_eq!(
        ref_oid(&dir, "refs/heads/main").unwrap(),
        base_before,
        "base unchanged"
    );
    assert_no_temp_worktree(&dir);
}

#[test]
fn worktree_add_failure_reports_and_leaves_refs_unchanged() {
    // pr_merge.rs:179 + 198-200 and git.rs:108 — `git worktree add` fails
    // (shim): the sibling lock is released, no worktree is registered, and
    // the merge fails with git's diagnostic.
    let dir = tmpdir("wtaddfail");
    init_repo(&dir);
    create_approved_pr(&dir, "feature", "wt add fail PR");
    checkout(&dir, "feature");
    let base_before = ref_oid(&dir, "refs/heads/main").unwrap();
    let shim = make_git_shim(
        "wtaddfail-shim",
        "if [ \"$1\" = \"-C\" ] && [ \"$3\" = \"worktree\" ] && [ \"$4\" = \"add\" ]; then\n\
         \x20 echo \"fatal: worktree add disabled by test shim\" >&2\n\
         \x20 exit 128\n\
         fi",
    );
    let (c, _, e) = run_forge(&dir, Some(&shim), &[], &["forge", "pr", "merge", "1"]);
    assert_ne!(c, 0, "worktree add failure must fail the merge");
    assert!(
        e.contains("failed to create temporary worktree")
            && e.contains("worktree add disabled by test shim"),
        "stderr must surface git's worktree-add diagnostic: {e}"
    );
    assert_eq!(
        ref_oid(&dir, "refs/heads/main").unwrap(),
        base_before,
        "base unchanged"
    );
    assert_no_temp_worktree(&dir);
}

#[test]
fn rebase_conflict_aborts_cleans_and_leaves_refs_unchanged() {
    // git.rs:216 + 283-286 (rebase failure → abort when state exists) and
    // 62-85 (`rebase_in_progress` true path). Feature and main diverge on
    // shared.txt after the fork point, so `git rebase --onto` genuinely
    // conflicts and creates rebase state that must be aborted.
    let dir = tmpdir("rebconf");
    init_repo(&dir);
    std::fs::write(dir.join("shared.txt"), "v1\n").unwrap();
    let (c, _, e) = git(&dir, &["add", "shared.txt"]);
    assert!(c == 0, "add shared: {e}");
    let (c, _, e) = git(&dir, &["commit", "-q", "-m", "shared v1"]);
    assert!(c == 0, "commit shared: {e}");
    let (c, _, e) = git(&dir, &["checkout", "-q", "-b", "feature"]);
    assert!(c == 0, "co feature: {e}");
    std::fs::write(dir.join("shared.txt"), "v2\n").unwrap();
    let (c, _, e) = git(&dir, &["add", "shared.txt"]);
    assert!(c == 0, "add v2: {e}");
    let (c, _, e) = git(&dir, &["commit", "-q", "-m", "feature v2"]);
    assert!(c == 0, "commit v2: {e}");
    let (c, _, e) = git(&dir, &["checkout", "-q", "main"]);
    assert!(c == 0, "co main: {e}");
    std::fs::write(dir.join("shared.txt"), "v3\n").unwrap();
    let (c, _, e) = git(&dir, &["add", "shared.txt"]);
    assert!(c == 0, "add v3: {e}");
    let (c, _, e) = git(&dir, &["commit", "-q", "-m", "main v3"]);
    assert!(c == 0, "commit v3: {e}");
    let (c, o, e) = forge(
        &dir,
        &[
            "forge",
            "pr",
            "create",
            "--source",
            "feature",
            "--base",
            "main",
            "rebase conflict PR",
        ],
    );
    assert!(c == 0, "create: {e} {o}");
    assert_eq!(
        forge(&dir, &["forge", "pr", "review", "1", "--approve"]).0,
        0
    );
    checkout(&dir, "feature");
    let base_before = ref_oid(&dir, "refs/heads/main").unwrap();
    let (cm, _, em) = forge(&dir, &["forge", "pr", "merge", "1", "--rebase"]);
    assert_ne!(cm, 0, "conflicting rebase must fail the merge");
    assert!(
        em.contains("git rebase failed") && em.contains("worktree cleaned up, no ref changes made"),
        "stderr: {em}"
    );
    assert_eq!(
        ref_oid(&dir, "refs/heads/main").unwrap(),
        base_before,
        "base unchanged"
    );
    assert_no_temp_worktree(&dir);
    let (_cs, os, _) = forge(&dir, &["forge", "pr", "show", "1"]);
    assert!(
        !os.contains("merged"),
        "no pr.merge on rebase conflict: {os}"
    );
}

#[test]
fn rebase_refused_by_pre_rebase_hook_cleans_worktree() {
    // git.rs:216 + 283-286 and 62-85 (`rebase_in_progress` false path: the
    // hook refuses BEFORE rebase state is created, so no `rebase --abort`
    // runs). Worktrees share the main repo's hooks dir. main must advance past
    // the fork point before PR creation so `git rebase --onto` actually
    // replays commits — a fast-forward rebase skips the pre-rebase hook.
    let dir = tmpdir("prerebase");
    init_repo(&dir);
    make_feature(&dir, "feature", "feat\n"); // returns to main
                                             // advance main after the fork point (different file → no rebase conflict)
    std::fs::write(dir.join("later.txt"), "later\n").unwrap();
    let (c, _, e) = git(&dir, &["add", "later.txt"]);
    assert!(c == 0, "add later: {e}");
    let (c, _, e) = git(&dir, &["commit", "-q", "-m", "main later"]);
    assert!(c == 0, "commit later: {e}");
    let (c, o, e) = forge(
        &dir,
        &[
            "forge",
            "pr",
            "create",
            "--source",
            "feature",
            "--base",
            "main",
            "pre-rebase PR",
        ],
    );
    assert!(c == 0, "create: {e} {o}");
    assert_eq!(
        forge(&dir, &["forge", "pr", "review", "1", "--approve"]).0,
        0
    );
    checkout(&dir, "feature");
    let hooks = dir.join(".git").join("hooks");
    std::fs::create_dir_all(&hooks).unwrap();
    let hook_path = hooks.join("pre-rebase");
    std::fs::write(
        &hook_path,
        "#!/bin/sh\necho \"pre-rebase refused\" >&2\nexit 1\n",
    )
    .unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&hook_path, std::fs::Permissions::from_mode(0o755)).unwrap();
    let base_before = ref_oid(&dir, "refs/heads/main").unwrap();
    let (cm, _, em) = forge(&dir, &["forge", "pr", "merge", "1", "--rebase"]);
    assert_ne!(cm, 0, "pre-rebase refusal must abort the merge");
    assert!(
        em.contains("git rebase failed") && em.contains("worktree cleaned up, no ref changes made"),
        "stderr: {em}"
    );
    assert_eq!(
        ref_oid(&dir, "refs/heads/main").unwrap(),
        base_before,
        "base unchanged"
    );
    assert_no_temp_worktree(&dir);
}

#[test]
fn squash_conflict_aborts_cleans_and_leaves_refs_unchanged() {
    // git.rs:226 — `git merge --squash` itself fails (content conflict), a
    // different failure site than the existing commit-msg-hook squash test
    // (which covers the `git commit -m` failure at git.rs:238). The squash
    // cleanup resets the detached worktree back to HEAD and removes it.
    let dir = tmpdir("sqconf");
    init_repo(&dir);
    let (c, _, e) = git(&dir, &["checkout", "-q", "-b", "feature"]);
    assert!(c == 0, "co feature: {e}");
    std::fs::write(dir.join("base.txt"), "feature-change\n").unwrap();
    let (c, _, e) = git(&dir, &["add", "base.txt"]);
    assert!(c == 0, "add: {e}");
    let (c, _, e) = git(&dir, &["commit", "-q", "-m", "feature change"]);
    assert!(c == 0, "commit: {e}");
    let (c, _, e) = git(&dir, &["checkout", "-q", "main"]);
    assert!(c == 0, "co main: {e}");
    std::fs::write(dir.join("base.txt"), "main-change\n").unwrap();
    let (c, _, e) = git(&dir, &["add", "base.txt"]);
    assert!(c == 0, "add: {e}");
    let (c, _, e) = git(&dir, &["commit", "-q", "-m", "main change"]);
    assert!(c == 0, "commit: {e}");
    let (c, o, e) = forge(
        &dir,
        &[
            "forge",
            "pr",
            "create",
            "--source",
            "feature",
            "--base",
            "main",
            "squash conflict PR",
        ],
    );
    assert!(c == 0, "create: {e} {o}");
    assert_eq!(
        forge(&dir, &["forge", "pr", "review", "1", "--approve"]).0,
        0
    );
    checkout(&dir, "feature");
    let base_before = ref_oid(&dir, "refs/heads/main").unwrap();
    let (cm, _, em) = forge(&dir, &["forge", "pr", "merge", "1", "--squash"]);
    assert_ne!(cm, 0, "conflicting squash must fail the merge");
    assert!(
        em.contains("git squash failed") && em.contains("worktree cleaned up, no ref changes made"),
        "stderr: {em}"
    );
    assert_eq!(
        ref_oid(&dir, "refs/heads/main").unwrap(),
        base_before,
        "base unchanged"
    );
    assert_no_temp_worktree(&dir);
    let (_cs, os, _) = forge(&dir, &["forge", "pr", "show", "1"]);
    assert!(
        !os.contains("merged"),
        "no pr.merge on squash conflict: {os}"
    );
}

#[test]
fn invalid_result_commit_cleans_worktree_and_leaves_refs_unchanged() {
    // pr_merge.rs:227-234 — the strategy executed (result commit exists in the
    // temp worktree) but `rev-parse HEAD` returns a non-OID (shim), so the
    // result-commit parse fails and cleanup removes the worktree.
    let dir = tmpdir("badoid");
    init_repo(&dir);
    create_approved_pr(&dir, "feature", "bad oid PR");
    checkout(&dir, "feature");
    let base_before = ref_oid(&dir, "refs/heads/main").unwrap();
    let shim = make_git_shim(
        "badoid-shim",
        "if [ \"$1\" = \"-C\" ] && [ \"$3\" = \"rev-parse\" ] && [ \"$4\" = \"HEAD\" ]; then\n\
         \x20 echo \"not-an-oid\"\n\
         \x20 exit 0\n\
         fi",
    );
    let (c, _, e) = run_forge(&dir, Some(&shim), &[], &["forge", "pr", "merge", "1"]);
    assert_ne!(c, 0, "unparseable result commit must fail the merge");
    assert!(
        e.contains("git merge failed: invalid result commit")
            && e.contains("worktree cleaned up, no ref changes made"),
        "stderr: {e}"
    );
    assert_eq!(
        ref_oid(&dir, "refs/heads/main").unwrap(),
        base_before,
        "base unchanged"
    );
    assert_no_temp_worktree(&dir);
    assert!(
        ref_oid(&dir, "refs/forge/prs/1/result").is_none(),
        "pending ref must not exist (cleanup ran before it was created)"
    );
}

#[test]
fn preexisting_pending_result_ref_cleans_worktree_and_leaves_refs_unchanged() {
    // pr_merge.rs:241-242 — a stale pending `/result` ref (expected absence)
    // makes `create_pending_result_ref` fail AFTER the strategy ran, so the
    // merge is cleaned up with no ref changes and the stale ref is untouched.
    let dir = tmpdir("pendingexists");
    init_repo(&dir);
    create_approved_pr(&dir, "feature", "pending exists PR");
    checkout(&dir, "feature");
    let base_before = ref_oid(&dir, "refs/heads/main").unwrap();
    let (c, _, e) = git(
        &dir,
        &["update-ref", "refs/forge/prs/1/result", &base_before],
    );
    assert!(c == 0, "pre-create stale pending ref: {e}");
    let (cm, _, em) = forge(&dir, &["forge", "pr", "merge", "1"]);
    assert_ne!(cm, 0, "preexisting pending ref must fail the merge");
    assert!(
        em.contains("git merge failed") && em.contains("worktree cleaned up, no ref changes made"),
        "stderr: {em}"
    );
    assert_eq!(
        ref_oid(&dir, "refs/heads/main").unwrap(),
        base_before,
        "base unchanged"
    );
    assert_no_temp_worktree(&dir);
    assert_eq!(
        ref_oid(&dir, "refs/forge/prs/1/result").unwrap(),
        base_before,
        "stale pending ref untouched"
    );
}

#[test]
fn worktree_remove_noop_leaves_dir_and_reports_leftover() {
    // pr_merge.rs:282-288 — `git worktree remove` reports success but the
    // directory survives (shim makes it a no-op), so the merge reports the
    // leftover directory and cleans the pending ref best-effort.
    let dir = tmpdir("dirleft");
    init_repo(&dir);
    create_approved_pr(&dir, "feature", "dir left PR");
    checkout(&dir, "feature");
    let base_before = ref_oid(&dir, "refs/heads/main").unwrap();
    let shim = make_git_shim(
        "dirleft-shim",
        "if [ \"$1\" = \"-C\" ] && [ \"$3\" = \"worktree\" ] && [ \"$4\" = \"remove\" ]; then\n\
         \x20 exit 0\n\
         fi",
    );
    let (c, _, e) = run_forge(&dir, Some(&shim), &[], &["forge", "pr", "merge", "1"]);
    assert_ne!(c, 0, "leftover directory must fail the merge");
    assert!(
        e.contains("temp worktree directory still exists at"),
        "stderr: {e}"
    );
    assert_eq!(
        ref_oid(&dir, "refs/heads/main").unwrap(),
        base_before,
        "base unchanged"
    );
    assert!(
        ref_oid(&dir, "refs/forge/prs/1/result").is_none(),
        "pending ref cleaned best-effort"
    );
    // The leftover worktree is still REGISTERED with its directory intact;
    // remove it for hygiene.
    let (wl, wout, _) = git(&dir, &["worktree", "list", "--porcelain"]);
    assert_eq!(wl, 0);
    let mut removed = false;
    for line in wout.lines() {
        if let Some(p) = line.strip_prefix("worktree ") {
            if p.contains("git-forge-pr1-merge") {
                assert!(
                    std::path::Path::new(p).is_dir(),
                    "leftover worktree directory must still exist: {p}"
                );
                let (r, _, er) = git(&dir, &["worktree", "remove", "--force", p]);
                assert!(r == 0, "hygiene remove failed: {er}");
                removed = true;
            }
        }
    }
    assert!(removed, "expected the leftover worktree to be cleaned");
}

#[test]
fn worktree_still_registered_after_removal_reported() {
    // pr_merge.rs:298-305 — the directory is gone but `git worktree list`
    // (shim-injected on the post-removal verification call) still lists the
    // temp path, so the merge reports the stale registration and cleans the
    // pending ref.
    let dir = tmpdir("stilreg");
    init_repo(&dir);
    create_approved_pr(&dir, "feature", "still reg PR");
    checkout(&dir, "feature");
    let base_before = ref_oid(&dir, "refs/heads/main").unwrap();
    let counter = dir.join("counter");
    let record = dir.join("record");
    let shim = make_git_shim(
        "stilreg-shim",
        "COUNTER=\"$GF_COUNTER\"\nRECORD=\"$GF_RECORD\"\n\
             if [ \"$1\" = \"-C\" ] && [ \"$3\" = \"worktree\" ]; then\n\
             \x20 case \"$4\" in\n\
             \x20   add)\n\
             \x20     printf '%s\\n' \"$6\" > \"$RECORD\"\n\
             \x20     exec \"$REAL_GIT\" \"$@\"\n\
             \x20     ;;\n\
             \x20   list)\n\
             \x20     n=0\n\
             \x20     [ -f \"$COUNTER\" ] && n=$(cat \"$COUNTER\")\n\
             \x20     n=$((n+1))\n\
             \x20     printf '%s\\n' \"$n\" > \"$COUNTER\"\n\
             \x20     if [ \"$n\" -ge 2 ]; then\n\
             \x20       out=$(\"$REAL_GIT\" \"$@\")\n\
             \x20       tmp=$(cat \"$RECORD\" 2>/dev/null)\n\
             \x20       printf '%s\\nworktree %s\\n' \"$out\" \"$tmp\"\n\
             \x20       exit 0\n\
             \x20     fi\n\
             \x20     exec \"$REAL_GIT\" \"$@\"\n\
             \x20     ;;\n\
             \x20 esac\n\
             fi",
    );
    let (c, _, e) = run_forge(
        &dir,
        Some(&shim),
        &[
            ("GF_COUNTER", counter.to_str().unwrap()),
            ("GF_RECORD", record.to_str().unwrap()),
        ],
        &["forge", "pr", "merge", "1"],
    );
    assert_ne!(c, 0, "stale registration must fail the merge");
    assert!(
        e.contains("temp worktree still registered at"),
        "stderr: {e}"
    );
    assert_eq!(
        ref_oid(&dir, "refs/heads/main").unwrap(),
        base_before,
        "base unchanged"
    );
    assert!(
        ref_oid(&dir, "refs/forge/prs/1/result").is_none(),
        "pending ref cleaned best-effort"
    );
    // The real worktree registration was actually removed (only the shim
    // claimed otherwise).
    assert_no_temp_worktree(&dir);
}

#[test]
fn pending_result_ref_left_in_place_reported_on_cleanup_failure() {
    // pr_merge.rs:362 + 365 — the cleanup's CAS delete of the pending ref
    // fails (the shim moved the ref to the base oid while also locking the
    // temp worktree, so `worktree remove` fails), producing the "left in
    // place" suffix on the removal-failure report.
    let dir = tmpdir("leftinplace");
    init_repo(&dir);
    create_approved_pr(&dir, "feature", "left in place PR");
    checkout(&dir, "feature");
    let base_before = ref_oid(&dir, "refs/heads/main").unwrap();
    let shim = make_git_shim(
        "leftinplace-shim",
        "if [ \"$1\" = \"-C\" ] && [ \"$3\" = \"worktree\" ] && [ \"$4\" = \"lock\" ]; then\n\
         \x20 \"$REAL_GIT\" \"$@\" || exit $?\n\
         \x20 base_oid=$(\"$REAL_GIT\" -C \"$2\" rev-parse refs/heads/main) || exit $?\n\
         \x20 \"$REAL_GIT\" -C \"$2\" update-ref refs/forge/prs/1/result \"$base_oid\" || exit $?\n\
         \x20 exit 0\n\
         fi",
    );
    let (c, _, e) = run_forge(
        &dir,
        Some(&shim),
        &[("GIT_FORGE_TEST_FAIL_WORKTREE_REMOVE", "1")],
        &["forge", "pr", "merge", "1"],
    );
    assert_ne!(c, 0, "cleanup CAS failure must fail the merge");
    assert!(
        e.contains("pending result ref refs/forge/prs/1/result left in place"),
        "stderr must report the leftover pending ref: {e}"
    );
    assert_eq!(
        ref_oid(&dir, "refs/heads/main").unwrap(),
        base_before,
        "base unchanged"
    );
    assert_eq!(
        ref_oid(&dir, "refs/forge/prs/1/result").unwrap(),
        base_before,
        "moved pending ref left in place"
    );
    // Hygiene: unlock + remove the locked temp worktree, drop the moved ref.
    let (wl, wout, _) = git(&dir, &["worktree", "list", "--porcelain"]);
    assert_eq!(wl, 0);
    let mut temp_path = None;
    for line in wout.lines() {
        if let Some(p) = line.strip_prefix("worktree ") {
            if p.contains("git-forge-pr1-merge") {
                temp_path = Some(p.to_string());
                let (u, _, eu) = git(&dir, &["worktree", "unlock", p]);
                assert!(u == 0, "unlock failed: {eu}");
                let (r, _, er) = git(&dir, &["worktree", "remove", "--force", p]);
                assert!(r == 0, "remove failed: {er}");
            }
        }
    }
    assert!(
        temp_path.is_some(),
        "expected a leftover locked temp worktree"
    );
    let (dr, _, der) = git(&dir, &["update-ref", "-d", "refs/forge/prs/1/result"]);
    assert!(dr == 0, "drop moved pending ref: {der}");
}

#[test]
fn finalize_failure_after_barrier_leaves_refs_unchanged() {
    // pr_merge.rs:333 + 337 — while the merge is parked in the test barrier
    // window (pending ref exists, temp worktree already gone), the pending
    // ref is moved to a different oid. On release the atomic completion
    // transaction's CAS delete fails → nothing moves and the leftover pending
    // ref is reported.
    let dir = tmpdir("finfail");
    init_repo(&dir);
    create_approved_pr(&dir, "feature", "finalize fail PR");
    checkout(&dir, "feature");
    let barrier = tmpdir("finfail-window");
    let base_before = ref_oid(&dir, "refs/heads/main").unwrap();
    let head_before = ref_oid(&dir, "refs/forge/prs/1/head").unwrap();

    let bin = env!("CARGO_BIN_EXE_git-forge");
    let mut child: Child = Command::new(bin)
        .args(["forge", "pr", "merge", "1"])
        .current_dir(&dir)
        .env("GIT_FORGE_TEST_MERGE_BARRIER", &barrier)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();

    let deadline = Instant::now() + Duration::from_secs(30);
    while !barrier.join("ready").exists() {
        assert!(
            Instant::now() < deadline,
            "merge never reached the barrier window"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    // While parked: base/head unchanged, pending ref exists.
    assert_eq!(
        ref_oid(&dir, "refs/heads/main").unwrap(),
        base_before,
        "base must be unchanged while merge is pending"
    );
    assert!(
        ref_oid(&dir, "refs/forge/prs/1/result").is_some(),
        "pending result ref must exist in barrier window"
    );
    // Sabotage the completion transaction: move the pending ref away from the
    // expected result commit, then release the barrier.
    let (c, _, e) = git(
        &dir,
        &["update-ref", "refs/forge/prs/1/result", &base_before],
    );
    assert!(c == 0, "move pending ref: {e}");
    let release_path = barrier.join("release");
    {
        use std::io::Write as _;
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&release_path)
            .expect("release sentinel must be created atomically");
        let _ = f.write_all(b"go\n");
    }
    let status = child.wait().unwrap();
    assert!(
        !status.success(),
        "merge must fail when the final transaction cannot complete"
    );
    let mut stderr = String::new();
    {
        use std::io::Read as _;
        if let Some(mut e) = child.stderr.take() {
            let _ = e.read_to_string(&mut stderr);
        }
    }
    assert!(
        stderr.contains("merge execution finished but final transaction failed"),
        "stderr: {stderr}"
    );
    assert!(stderr.contains("refs unchanged"), "stderr: {stderr}");
    assert!(
        stderr.contains("pending result ref refs/forge/prs/1/result left in place"),
        "stderr: {stderr}"
    );
    assert_eq!(
        ref_oid(&dir, "refs/heads/main").unwrap(),
        base_before,
        "base unchanged"
    );
    assert_eq!(
        ref_oid(&dir, "refs/forge/prs/1/head").unwrap(),
        head_before,
        "PR head chain unchanged"
    );
    assert_eq!(
        ref_oid(&dir, "refs/forge/prs/1/result").unwrap(),
        base_before,
        "pending ref left at the moved oid"
    );
    assert_no_temp_worktree(&dir);
}

#[test]
fn merge_git_spawn_failure_is_clean_error() {
    // git.rs:44-49 (spawn-failure branch) + 151-152 — the first git call in
    // the merge flow is the checked-out-base guard's `git worktree list`; with
    // no git on PATH it cannot spawn and the error is user-facing, no panic.
    let dir = tmpdir("nogit");
    init_repo(&dir);
    create_approved_pr(&dir, "feature", "no git PR");
    checkout(&dir, "feature");
    let base_before = ref_oid(&dir, "refs/heads/main").unwrap();
    let empty = tmpdir("nogit-bin");
    let bin = env!("CARGO_BIN_EXE_git-forge");
    let out = Command::new(bin)
        .args(["forge", "pr", "merge", "1"])
        .current_dir(&dir)
        .env("PATH", &empty)
        .output()
        .unwrap();
    let code = out.status.code().unwrap_or(-1);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_ne!(code, 0, "merge must fail when git cannot be spawned");
    assert!(
        stderr.contains("git worktree list failed"),
        "stderr: {stderr}"
    );
    assert_eq!(
        ref_oid(&dir, "refs/heads/main").unwrap(),
        base_before,
        "base unchanged"
    );
}

#[test]
fn worktree_list_failure_is_clean_error() {
    // git.rs:155 — a nonzero `git worktree list` exit in the checked-out-base
    // guard surfaces the BARE "git worktree list failed" message (stderr never
    // appended), via `base_checked_out_elsewhere`'s Err propagation.
    let dir = tmpdir("listfail");
    init_repo(&dir);
    create_approved_pr(&dir, "feature", "list fail PR");
    checkout(&dir, "feature");
    let base_before = ref_oid(&dir, "refs/heads/main").unwrap();
    let shim = make_git_shim(
        "listfail-shim",
        "if [ \"$1\" = \"-C\" ] && [ \"$3\" = \"worktree\" ] && [ \"$4\" = \"list\" ]; then\n\
         \x20 echo \"fatal: worktree list disabled by test shim\" >&2\n\
         \x20 exit 128\n\
         fi",
    );
    let (c, _, e) = run_forge(&dir, Some(&shim), &[], &["forge", "pr", "merge", "1"]);
    assert_ne!(c, 0, "worktree list failure must fail the merge");
    assert!(
        e.trim().ends_with("git worktree list failed"),
        "bare message expected: {e}"
    );
    assert!(
        !e.contains("disabled by test shim"),
        "stderr must never be appended on nonzero exit: {e}"
    );
    assert_eq!(
        ref_oid(&dir, "refs/heads/main").unwrap(),
        base_before,
        "base unchanged"
    );
}

#[test]
fn merge_barrier_deadline_cleans_pending_ref_and_leaves_refs_unchanged() {
    // pr_merge.rs:400-402 + 324 — the test-only barrier's 30s deadline with
    // the natural (clean) cleanup: no release sentinel ever appears, so the
    // merge removes both sentinels, deletes the pending ref, and fails with
    // the bare deadline error and no ref updates. Deterministic; ~30s runtime.
    let dir = tmpdir("deadline-clean");
    init_repo(&dir);
    create_approved_pr(&dir, "feature", "deadline clean PR");
    checkout(&dir, "feature");
    let barrier = tmpdir("deadline-clean-window");
    let base_before = ref_oid(&dir, "refs/heads/main").unwrap();
    let bin = env!("CARGO_BIN_EXE_git-forge");
    let mut child: Child = Command::new(bin)
        .args(["forge", "pr", "merge", "1"])
        .current_dir(&dir)
        .env("GIT_FORGE_TEST_MERGE_BARRIER", &barrier)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    // Wait for the ready sentinel (bounded), then never release.
    let deadline = Instant::now() + Duration::from_secs(30);
    while !barrier.join("ready").exists() {
        assert!(
            Instant::now() < deadline,
            "merge never reached the barrier window"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    let status = child.wait().unwrap();
    assert!(!status.success(), "barrier deadline must fail the merge");
    let mut stderr = String::new();
    {
        use std::io::Read as _;
        if let Some(mut e) = child.stderr.take() {
            let _ = e.read_to_string(&mut stderr);
        }
    }
    assert!(
        stderr.contains("test merge barrier deadline exceeded; no ref updates made"),
        "stderr: {stderr}"
    );
    assert!(
        !stderr.contains("left in place"),
        "clean deadline cleanup must not report a leftover ref: {stderr}"
    );
    assert_eq!(
        ref_oid(&dir, "refs/heads/main").unwrap(),
        base_before,
        "base unchanged"
    );
    assert!(
        ref_oid(&dir, "refs/forge/prs/1/result").is_none(),
        "pending ref cleaned on deadline"
    );
    assert_no_temp_worktree(&dir);
}

#[test]
fn merge_barrier_deadline_reports_leftover_pending_ref() {
    // pr_merge.rs:400-402 + 320-322 — the test-only barrier's 30s deadline:
    // no release sentinel ever appears, so the merge removes both sentinels
    // and fails with no ref updates. While parked, the pending ref is moved to
    // a different oid so the deadline cleanup's CAS delete fails and the
    // "left in place" suffix is reported. Deterministic; ~30s runtime.
    let dir = tmpdir("deadline");
    init_repo(&dir);
    create_approved_pr(&dir, "feature", "deadline PR");
    checkout(&dir, "feature");
    let barrier = tmpdir("deadline-window");
    let base_before = ref_oid(&dir, "refs/heads/main").unwrap();
    let bin = env!("CARGO_BIN_EXE_git-forge");
    let mut child: Child = Command::new(bin)
        .args(["forge", "pr", "merge", "1"])
        .current_dir(&dir)
        .env("GIT_FORGE_TEST_MERGE_BARRIER", &barrier)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    // Wait for the ready sentinel (bounded), then never release.
    let deadline = Instant::now() + Duration::from_secs(30);
    while !barrier.join("ready").exists() {
        assert!(
            Instant::now() < deadline,
            "merge never reached the barrier window"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    // Sabotage the deadline cleanup: move the pending ref away from the
    // expected result commit so its CAS delete cannot succeed.
    let (c, _, e) = git(
        &dir,
        &["update-ref", "refs/forge/prs/1/result", &base_before],
    );
    assert!(c == 0, "move pending ref: {e}");
    let status = child.wait().unwrap();
    assert!(!status.success(), "barrier deadline must fail the merge");
    let mut stderr = String::new();
    {
        use std::io::Read as _;
        if let Some(mut e) = child.stderr.take() {
            let _ = e.read_to_string(&mut stderr);
        }
    }
    assert!(
        stderr.contains("test merge barrier deadline exceeded; no ref updates made"),
        "stderr: {stderr}"
    );
    assert!(
        stderr.contains("pending result ref refs/forge/prs/1/result left in place"),
        "stderr: {stderr}"
    );
    assert_eq!(
        ref_oid(&dir, "refs/heads/main").unwrap(),
        base_before,
        "base unchanged"
    );
    assert_eq!(
        ref_oid(&dir, "refs/forge/prs/1/result").unwrap(),
        base_before,
        "moved pending ref left in place"
    );
    assert_no_temp_worktree(&dir);
    // Hygiene: drop the moved pending ref.
    let (dr, _, der) = git(&dir, &["update-ref", "-d", "refs/forge/prs/1/result"]);
    assert!(dr == 0, "drop moved pending ref: {der}");
}

#[test]
fn result_commit_resolution_failure_cleans_worktree_and_leaves_refs_unchanged() {
    // git.rs:252-254 — the strategy ran, then `git rev-parse HEAD` (result
    // resolution) exits nonzero (shim), so execute_strategy cleans up with
    // the strategy kind and no ref changes.
    let dir = tmpdir("revparsefail");
    init_repo(&dir);
    create_approved_pr(&dir, "feature", "rev-parse fail PR");
    checkout(&dir, "feature");
    let base_before = ref_oid(&dir, "refs/heads/main").unwrap();
    let shim = make_git_shim(
        "revparsefail-shim",
        "if [ \"$1\" = \"-C\" ] && [ \"$3\" = \"rev-parse\" ] && [ \"$4\" = \"HEAD\" ]; then\n\
         \x20 echo \"fatal: rev-parse disabled by test shim\" >&2\n\
         \x20 exit 128\n\
         fi",
    );
    let (c, _, e) = run_forge(&dir, Some(&shim), &[], &["forge", "pr", "merge", "1"]);
    assert_ne!(c, 0, "result-commit resolution failure must fail the merge");
    assert!(
        e.contains("git merge failed") && e.contains("worktree cleaned up, no ref changes made"),
        "stderr: {e}"
    );
    assert_eq!(
        ref_oid(&dir, "refs/heads/main").unwrap(),
        base_before,
        "base unchanged"
    );
    assert_no_temp_worktree(&dir);
    assert!(
        ref_oid(&dir, "refs/forge/prs/1/result").is_none(),
        "pending ref must not exist (cleanup ran before it was created)"
    );
}

#[test]
fn merge_help_usage_and_strategies() {
    // pr_merge.rs:412-418 — `pr merge --help` / `-h` (the only pr_merge.rs
    // block no existing test reached; parent `pr --help` does not dispatch
    // into cmd_pr_merge).
    let dir = tmpdir("mhelp");
    init_repo(&dir);
    let (c, o, e) = forge(&dir, &["forge", "pr", "merge", "--help"]);
    assert_eq!(c, 0, "merge --help failed: {e}");
    assert!(o.contains("usage: git forge pr merge"), "usage head: {o}");
    for s in ["--merge", "--squash", "--rebase"] {
        assert!(o.contains(s), "help must list {s}: {o}");
    }
    let (c2, o2, e2) = forge(&dir, &["forge", "pr", "merge", "-h"]);
    assert_eq!(c2, 0, "merge -h failed: {e2}");
    assert!(o2.contains("usage: git forge pr merge"), "-h: {o2}");
}
