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
fn nonexistent_pr_merge_is_clean_error() {
    let dir = tmpdir("nopr");
    init_repo(&dir);
    let (c, _, e) = forge(&dir, &["forge", "pr", "merge", "42"]);
    assert_ne!(c, 0);
    assert!(e.contains("#42"), "stderr: {e}");
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
