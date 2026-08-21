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
