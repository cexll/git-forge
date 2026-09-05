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

/// Add a passing `.forge/ci.sh` (exit 0) to the CURRENT branch and commit it,
/// so the PR's own commits make `forge ci run` record a green latest CI Check.
/// Must be called while the feature branch is checked out.
fn add_passing_ci_plan(dir: &Path) {
    std::fs::create_dir_all(dir.join(".forge")).unwrap();
    std::fs::write(dir.join(".forge").join("ci.sh"), "#!/bin/bash\nexit 0\n").unwrap();
    let (c, _, e) = git(dir, &["add", ".forge/ci.sh"]);
    assert_eq!(c, 0, "add ci.sh failed: {e}");
    let (c, _, e) = git(dir, &["commit", "-q", "-m", "ci plan"]);
    assert_eq!(c, 0, "commit ci.sh failed: {e}");
}

/// Run `forge ci run 1`, asserting the latest CI Check is recorded as success.
/// Called after `pr create` so the merge gate (L2) sees a green Check.
fn ci_green(dir: &Path) {
    let (c, o, e) = forge(dir, &["forge", "ci", "run", "1"]);
    assert_eq!(c, 0, "ci run must pass: {e} {o}");
}

fn create_approved_pr(dir: &Path, branch: &str, title: &str) {
    make_feature(dir, branch, "feat\n");
    // The L2 merge gate requires the latest CI Check to be success, so give
    // the PR a passing plan and run it once before approving.
    let (c, _, e) = git(dir, &["checkout", "-q", branch]);
    assert_eq!(c, 0, "checkout {branch} failed: {e}");
    add_passing_ci_plan(dir);
    let (c, _, e) = git(dir, &["checkout", "-q", "main"]);
    assert_eq!(c, 0, "checkout main failed: {e}");
    let (c, o, e) = forge(
        dir,
        &[
            "forge", "pr", "create", "--source", branch, "--base", "main", title,
        ],
    );
    assert!(c == 0, "pr create failed: {e} {o}");
    ci_green(dir);
    let (c, o, e) = forge(dir, &["forge", "pr", "review", "1", "--approve"]);
    assert!(c == 0, "pr review failed: {e} {o}");
}
/// Like [`create_approved_pr`] but for a PR declaring `--closes <issue_id>`.
/// The shared counter means issue #1 already exists when the PR is created,
/// so `pr_id` names the PR's actual id (the helper never assumes).
fn create_approved_pr_closes(dir: &Path, branch: &str, title: &str, issue_id: u64, pr_id: u64) {
    make_feature(dir, branch, "feat\n");
    let (c, _, e) = git(dir, &["checkout", "-q", branch]);
    assert_eq!(c, 0, "checkout {branch} failed: {e}");
    add_passing_ci_plan(dir);
    let (c, _, e) = git(dir, &["checkout", "-q", "main"]);
    assert_eq!(c, 0, "checkout main failed: {e}");
    let (c, o, e) = forge(
        dir,
        &[
            "forge",
            "pr",
            "create",
            "--source",
            branch,
            "--base",
            "main",
            title,
            "--closes",
            &issue_id.to_string(),
        ],
    );
    assert!(c == 0, "pr create --closes failed: {e} {o}");
    let (c, o, e) = forge(dir, &["forge", "ci", "run", &pr_id.to_string()]);
    assert_eq!(c, 0, "ci run must pass: {e} {o}");
    let (c, o, e) = forge(
        dir,
        &["forge", "pr", "review", &pr_id.to_string(), "--approve"],
    );
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
    let (c, _, e) = git(&dir, &["checkout", "-q", "feature"]);
    assert_eq!(c, 0, "co feature: {e}");
    add_passing_ci_plan(&dir);
    let (c, _, e) = git(&dir, &["checkout", "-q", "main"]);
    assert_eq!(c, 0, "co main: {e}");
    assert_eq!(
        forge(
            &dir,
            &["forge", "pr", "create", "--source", "feature", "--base", "main", "PR1"]
        )
        .0,
        0
    );
    ci_green(&dir);
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
    add_passing_ci_plan(&dir);
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
    ci_green(&dir);
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
fn checked_out_dirty_base_refused_refs_unchanged() {
    // VAL-106: the guard relaxes only for a CLEAN checked-out base. A
    // modified tracked file makes the main worktree dirty; the merge must
    // still refuse before any side effect, naming the dirt, with refs
    // unchanged.
    let dir = tmpdir("checkedout-dirty");
    init_repo(&dir);
    create_approved_pr(&dir, "feature", "co dirty PR");
    // main is checked out (create_approved_pr leaves it there); make it dirty
    // by modifying a tracked file.
    std::fs::write(dir.join("base.txt"), "dirty\n").unwrap();
    let base_before = ref_oid(&dir, "refs/heads/main").unwrap();
    let (c, _, e) = forge(&dir, &["forge", "pr", "merge", "1"]);
    assert_ne!(c, 0, "dirty checked-out base must refuse");
    assert!(e.contains("dirty"), "stderr: {e}");
    assert_eq!(
        ref_oid(&dir, "refs/heads/main").unwrap(),
        base_before,
        "base unchanged"
    );
    let (cs, os, _) = forge(&dir, &["forge", "pr", "show", "1"]);
    assert!(cs == 0, "pr show must succeed after dirty refusal");
    assert!(!os.contains("merged"), "no pr.merge on dirty refusal: {os}");
}

#[test]
fn checked_out_untracked_base_refused_refs_unchanged() {
    // VAL-106: UNTRACKED files count as dirty — an untracked file in the
    // checked-out base worktree refuses the merge exactly like a tracked
    // modification.
    let dir = tmpdir("checkedout-untracked");
    init_repo(&dir);
    create_approved_pr(&dir, "feature", "co untracked PR");
    std::fs::write(dir.join("untracked.txt"), "u\n").unwrap();
    let base_before = ref_oid(&dir, "refs/heads/main").unwrap();
    let (c, _, e) = forge(&dir, &["forge", "pr", "merge", "1"]);
    assert_ne!(c, 0, "untracked file must count as dirty");
    assert!(e.contains("dirty"), "stderr: {e}");
    assert_eq!(
        ref_oid(&dir, "refs/heads/main").unwrap(),
        base_before,
        "base unchanged"
    );
}

#[test]
fn checked_out_clean_base_merges_and_worktree_refreshes() {
    // VAL-105: a CLEAN checked-out base (porcelain empty) merges, and after
    // the base CAS the base worktree is refreshed — its files match the
    // result commit and the tree stays clean (never a moved ref with a
    // stale tree).
    let dir = tmpdir("checkedout-clean");
    init_repo(&dir);
    create_approved_pr(&dir, "feature", "co clean PR");
    // main is checked out and clean (create_approved_pr leaves it there).
    let base_before = ref_oid(&dir, "refs/heads/main").unwrap();
    let (c, o, e) = forge(&dir, &["forge", "pr", "merge", "1"]);
    assert_eq!(c, 0, "clean checked-out base must merge: {e} {o}");
    let result = ref_oid(&dir, "refs/heads/main").unwrap();
    assert_ne!(result, base_before, "base advanced to the result commit");
    // The result commit is the merge of the source; its tree carries the
    // feature file with the feature's content.
    let (ch, head, _) = git(&dir, &["rev-parse", "HEAD"]);
    assert_eq!(ch, 0, "rev-parse HEAD failed");
    assert_eq!(head, result, "worktree HEAD agrees with the moved base ref");
    let content = std::fs::read_to_string(dir.join("feature.txt")).unwrap();
    assert_eq!(content, "feat\n", "worktree files match the result commit");
    let (_, status, _) = git(&dir, &["status", "--porcelain"]);
    assert_eq!(status, "", "worktree clean after refresh: {status:?}");
    let (cs, os, _) = forge(&dir, &["forge", "pr", "show", "1"]);
    assert!(cs == 0, "pr show must succeed");
    assert!(os.contains("merged:"), "pr.merge recorded: {os}");
}

#[test]
fn checked_out_ignored_only_base_merges() {
    // VAL-106 (ignored clause): ignored files do NOT count as dirty — an
    // ignored file in the checked-out base worktree must not refuse the
    // merge (porcelain output is empty for ignored-only trees). The
    // .gitignore is committed BEFORE the PR snapshot so the base ref never
    // moves after creation.
    let dir = tmpdir("checkedout-ignored");
    init_repo(&dir);
    std::fs::write(dir.join(".gitignore"), "ignored.txt\n").unwrap();
    let (ca, _, ea) = git(&dir, &["add", ".gitignore"]);
    assert_eq!(ca, 0, "add .gitignore failed: {ea}");
    let (cc, _, ec) = git(&dir, &["commit", "-qm", "gitignore"]);
    assert_eq!(cc, 0, "commit .gitignore failed: {ec}");
    create_approved_pr(&dir, "feature", "co ignored PR");
    std::fs::write(dir.join("ignored.txt"), "i\n").unwrap();
    let (_, porcelain, _) = git(&dir, &["status", "--porcelain"]);
    assert_eq!(porcelain, "", "ignored-only tree must be porcelain-clean");
    let (c, o, e) = forge(&dir, &["forge", "pr", "merge", "1"]);
    assert_eq!(c, 0, "ignored-only dirt must not refuse: {e} {o}");
}

#[test]
fn refresh_failure_rolls_refs_back_and_reports_leftover() {
    // VAL-107: with the refresh seam injected, the merge exits non-zero, the
    // base ref and the worktree HEAD agree (no partial move — the tree keeps
    // the pre-merge content the old tip checks out), and the output reports
    // the worktree as a leftover.
    let dir = tmpdir("refreshfail");
    init_repo(&dir);
    create_approved_pr(&dir, "feature", "refresh fail PR");
    let base_before = ref_oid(&dir, "refs/heads/main").unwrap();
    let head_before = ref_oid(&dir, "refs/forge/prs/1/head").unwrap();
    let bin = env!("CARGO_BIN_EXE_git-forge");
    let out = Command::new(bin)
        .args(["forge", "pr", "merge", "1"])
        .current_dir(&dir)
        .env("GIT_FORGE_TEST_FAIL_BASE_REFRESH", "1")
        .output()
        .unwrap();
    let code = out.status.code().unwrap_or(-1);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_ne!(code, 0, "refresh failure must fail the merge");
    assert!(
        stderr.contains("base worktree refresh failed"),
        "stderr must name the refresh failure: {stderr}"
    );
    assert!(
        stderr.contains("worktree left at"),
        "stderr must report the worktree as a leftover: {stderr}"
    );
    // Refs agree with the worktree: base moved nowhere, PR chain untouched,
    // and the worktree HEAD is the pre-merge base tip (no partial move).
    assert_eq!(
        ref_oid(&dir, "refs/heads/main").unwrap(),
        base_before,
        "base must roll back to the pre-merge tip"
    );
    assert_eq!(
        ref_oid(&dir, "refs/forge/prs/1/head").unwrap(),
        head_before,
        "PR head chain must be restored"
    );
    let (_, head, _) = git(&dir, &["rev-parse", "HEAD"]);
    assert_eq!(head, base_before, "worktree HEAD agrees with the base ref");
    let content = std::fs::read_to_string(dir.join("feature.txt"));
    assert!(
        content.is_err(),
        "worktree tree is unchanged (feature file absent)"
    );
    let (cs, os, _) = forge(&dir, &["forge", "pr", "show", "1"]);
    assert!(cs == 0, "pr show must succeed after refresh failure");
    assert!(!os.contains("merged:"), "no pr.merge recorded: {os}");
}

#[test]
fn refresh_failure_with_closes_rolls_all_refs_back() {
    // VAL-107 sibling (F-001): with a `--closes` PR, the refresh-failure
    // rollback must ALSO re-anchor the linked issue ref, CAS'd from the
    // issue.close commit the finalize transaction just applied (the ref's
    // ACTUAL current value) — never from `gate_head` (a PR-head OID the
    // issue ref never held; the wrong oldvalue aborts the whole
    // update-ref batch and leaves every ref advanced against a stale base
    // worktree, the never-split violation).
    let dir = tmpdir("refreshfail-closes");
    init_repo(&dir);
    let (ci, _, ei) = forge(&dir, &["forge", "issue", "new", "linked bug"]);
    assert_eq!(ci, 0, "issue new failed: {ei}");
    create_approved_pr_closes(&dir, "feature", "refresh fail closes PR", 1, 2);
    let base_before = ref_oid(&dir, "refs/heads/main").unwrap();
    let head_before = ref_oid(&dir, "refs/forge/prs/2/head").unwrap();
    let issue_before = ref_oid(&dir, "refs/forge/issues/1").unwrap();
    let bin = env!("CARGO_BIN_EXE_git-forge");
    let out = Command::new(bin)
        .args(["forge", "pr", "merge", "2"])
        .current_dir(&dir)
        .env("GIT_FORGE_TEST_FAIL_BASE_REFRESH", "1")
        .output()
        .unwrap();
    let code = out.status.code().unwrap_or(-1);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_ne!(code, 0, "refresh failure must fail the merge");
    assert!(
        stderr.contains("base worktree refresh failed"),
        "stderr must name the refresh failure: {stderr}"
    );
    assert!(
        stderr.contains("worktree left at"),
        "stderr must report the worktree as a leftover: {stderr}"
    );
    assert!(
        !stderr.contains("ROLLBACK FAILED"),
        "the atomic rollback must succeed (never-split): {stderr}"
    );
    assert_eq!(
        ref_oid(&dir, "refs/heads/main").unwrap(),
        base_before,
        "base must roll back to the pre-merge tip"
    );
    assert_eq!(
        ref_oid(&dir, "refs/forge/prs/2/head").unwrap(),
        head_before,
        "PR head chain must be restored"
    );
    assert_eq!(
        ref_oid(&dir, "refs/forge/issues/1").unwrap(),
        issue_before,
        "linked issue ref must be re-anchored to its pre-close tip"
    );
    let (cs, os, es) = forge(&dir, &["forge", "issue", "show", "1"]);
    assert_eq!(cs, 0, "issue show failed: {es}");
    assert!(
        os.contains("open"),
        "issue must fold open after rollback: {os}"
    );
    let (_, head, _) = git(&dir, &["rev-parse", "HEAD"]);
    assert_eq!(head, base_before, "worktree HEAD agrees with the base ref");
    let content = std::fs::read_to_string(dir.join("feature.txt"));
    assert!(
        content.is_err(),
        "worktree tree is unchanged (feature file absent)"
    );
    let (ps, pos, _) = forge(&dir, &["forge", "pr", "show", "2"]);
    assert!(ps == 0, "pr show must succeed after refresh failure");
    assert!(!pos.contains("merged:"), "no pr.merge recorded: {pos}");
    assert_no_temp_worktree_pr(&dir, 2);
}

/// CR-02 (code review): dirt appearing in a checked-out BASE worktree between
/// the pre-merge guard and the #3 refresh (the merge side effects run in
/// between) must NOT be silently destroyed by the refresh's bare
/// `reset --hard`. The refresh re-checks cleanliness and refuses; the whole
/// completion rolls back (never split) and the uncommitted work survives.
/// The shim appends an untracked file on the SECOND `git -C <dir> status
/// --porcelain` call: call #1 is the pre-merge guard, call #2 is the refresh
/// re-check — the dirt appears exactly in the guard→refresh window.
#[test]
fn refresh_refuses_dirt_that_appears_after_guard() {
    let dir = tmpdir("refreshdirty");
    init_repo(&dir);
    create_approved_pr(&dir, "feature", "refresh dirty PR");
    let base_before = ref_oid(&dir, "refs/heads/main").unwrap();
    let head_before = ref_oid(&dir, "refs/forge/prs/1/head").unwrap();
    // The refresh runs `git -C <worktree>` with the CANONICAL path (the
    // porcelain worktree list resolves symlinks, e.g. /private/var vs /var on
    // macOS), so the shim must match the canonical form — a raw `dir.to_str()`
    // could silently never trigger.
    let dirstr = std::fs::canonicalize(&dir)
        .unwrap()
        .to_str()
        .unwrap()
        .to_string();
    // The refresh's CR-02 re-check is `git -C <dir> diff --quiet <base_tip>`
    // (baseline the PRE-MERGE tip; the first such call IS the re-check — the
    // pre-merge guard uses `status --porcelain`, not diff). Inject the dirt on
    // that first diff call so it appears exactly in the guard→refresh window.
    let shim = make_git_shim(
        "refresh-dirty-shim",
        "if [ \"$1\" = \"-C\" ] && [ \"$2\" = \"$GF_TARGET\" ] && \
         [ \"$3\" = \"diff\" ] && [ \"$4\" = \"--quiet\" ]; then\n\
         n=$(cat \"$GF_ST_COUNT\" 2>/dev/null || echo 0)\n\
         n=$((n+1))\n\
         echo \"$n\" > \"$GF_ST_COUNT\"\n\
         if [ \"$n\" -ge 1 ]; then\n\
         echo dirt >> \"$GF_TARGET/dirt.txt\"\n\
         fi\n\
         fi",
    );
    let counter = shim.join("count");
    std::fs::write(&counter, "0").unwrap();
    let (cm, om, em) = run_forge(
        &dir,
        Some(&shim),
        &[
            ("GF_TARGET", dirstr.as_str()),
            ("GF_ST_COUNT", counter.to_str().unwrap()),
        ],
        &["forge", "pr", "merge", "1"],
    );
    assert_ne!(cm, 0, "dirty-at-refresh must fail the merge: {om} {em}");
    let both = format!("{om} {em}");
    assert!(
        both.contains("became dirty"),
        "stderr must name the dirty refusal: {both}"
    );
    assert!(
        !both.contains("ROLLBACK FAILED"),
        "the atomic rollback must succeed (never-split): {both}"
    );
    // Refs agree with the worktree: base rolled back, PR chain restored.
    assert_eq!(
        ref_oid(&dir, "refs/heads/main").unwrap(),
        base_before,
        "base must roll back to the pre-merge tip"
    );
    assert_eq!(
        ref_oid(&dir, "refs/forge/prs/1/head").unwrap(),
        head_before,
        "PR head chain must be restored"
    );
    // The injected uncommitted work SURVIVES (no reset --hard ran).
    assert!(
        dir.join("dirt.txt").exists(),
        "the uncommitted dirt must survive the refusal"
    );
    let (_, head, _) = git(&dir, &["rev-parse", "HEAD"]);
    assert_eq!(head, base_before, "worktree HEAD agrees with the base ref");
    let (cs, os, _) = forge(&dir, &["forge", "pr", "show", "1"]);
    assert!(cs == 0, "pr show must succeed after the refusal");
    assert!(!os.contains("merged:"), "no pr.merge recorded: {os}");
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
    add_passing_ci_plan(&dir);
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
    ci_green(&dir);
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
#[allow(clippy::cognitive_complexity)]
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
#[allow(clippy::cognitive_complexity)]
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
/// Stored wire-format event JSON of every event-bearing commit on a chain,
/// oldest→tip (genesis roots have no event blob and are skipped). The raw
/// JSON is what assertions read: the actor/kind/body live in the event JSON,
/// not in the folded state.
fn chain_event_jsons(dir: &Path, ref_name: &str) -> Vec<String> {
    let rev = git(dir, &["rev-list", ref_name]);
    assert_eq!(rev.0, 0, "rev-list {ref_name} failed: {}", rev.2);
    let mut out = Vec::new();
    for oid in rev.1.split_whitespace() {
        let show = git(dir, &["show", &format!("{oid}:.forge/event.json")]);
        if show.0 != 0 {
            continue; // genesis root has no event.json
        }
        out.push(show.1);
    }
    out.reverse();
    out
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
    let (c, _, e) = git(&dir, &["checkout", "-q", "feature"]);
    assert_eq!(c, 0, "co feature: {e}");
    add_passing_ci_plan(&dir);
    let (c, _, e) = git(&dir, &["checkout", "-q", "main"]);
    assert_eq!(c, 0, "co main: {e}");
    let (c, o, e) = forge(
        &dir,
        &[
            "forge", "pr", "create", "--source", "feature", "--base", "main", "PR",
        ],
    );
    assert_eq!(c, 0, "pr create failed: {e} {o}");
    ci_green(&dir);
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
        6,
        "six events: pr.created + ci.check + ci.check + pr.comment + pr.review + pr.merge"
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
    assert_eq!(
        actors.len(),
        4,
        "pr.created + ci.check + pr.comment + pr.review"
    );
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

/// Like [`assert_no_temp_worktree`] but for a SPECIFIC PR id — the merge
/// tests with a `--closes` target run PR #2 (the linked issue took id 1), so
/// the PR #1 assertion would be a vacuous pass.
fn assert_no_temp_worktree_pr(dir: &Path, pr: u64) {
    let (wl, out, _) = git(dir, &["worktree", "list", "--porcelain"]);
    assert_eq!(wl, 0, "worktree list must succeed");
    assert!(
        !out.contains(&format!("git-forge-pr{pr}-merge")),
        "temp worktree for PR #{pr} left behind: {out}"
    );
}

/// Wait until the per-attempt issue-stage barrier creates `<dir>/ready` whose
/// CONTENT is exactly the expected attempt number (the first round parks with
/// "0", the first retry with "1", ...). Content-matching makes multi-round
/// tests unambiguous: a round-0 ready that the merge has since consumed (and
/// whose round-1 ready is already up) cannot be mistaken for the next round.
fn wait_issue_barrier_round(dir: &Path, round: u32, deadline: Instant, tag: &str) {
    let expected = round.to_string();
    let mut seen = String::new();
    while seen != expected {
        assert!(
            Instant::now() < deadline,
            "{tag}: merge never reached the issue-stage barrier round {round}"
        );
        std::thread::sleep(Duration::from_millis(20));
        let p = dir.join("ready");
        if p.exists() {
            seen = std::fs::read_to_string(&p)
                .unwrap_or_default()
                .trim()
                .to_string();
        }
    }
}

/// Create the atomic `release` sentinel that unpauses one issue-stage barrier
/// round.
fn release_issue_barrier(dir: &Path) {
    let release_path = dir.join("release");
    use std::io::Write as _;
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&release_path)
        .expect("release sentinel must be created atomically");
    let _ = f.write_all(b"go\n");
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
    add_passing_ci_plan(&dir);
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
    ci_green(&dir);
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
    let (c, _, e) = git(&dir, &["checkout", "-q", "feature"]);
    assert_eq!(c, 0, "co feature: {e}");
    add_passing_ci_plan(&dir);
    let (c, _, e) = git(&dir, &["checkout", "-q", "main"]);
    assert_eq!(c, 0, "co main: {e}");
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
    ci_green(&dir);
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
    add_passing_ci_plan(&dir);
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
    ci_green(&dir);
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
    for s in [
        "--merge",
        "--squash",
        "--rebase",
        "--ff",
        "--recycle-worktree",
        "--force",
    ] {
        assert!(o.contains(s), "help must list {s}: {o}");
    }
    let (c2, o2, e2) = forge(&dir, &["forge", "pr", "merge", "-h"]);
    assert_eq!(c2, 0, "merge -h failed: {e2}");
    assert!(o2.contains("usage: git forge pr merge"), "-h: {o2}");
}

#[test]
fn merge_refuses_when_head_ref_disappears_during_barrier() {
    // pr_merge.rs:384-388 — if the PR head ref is DELETED while the merge is
    // parked at the pre-finalize barrier, the completion transaction fails and
    // the finalizer must refuse with refs unchanged and the pending result ref
    // left in place (never a silent retry into a vanished ref).
    let dir = tmpdir("barrier-vanish");
    init_repo(&dir);
    create_approved_pr(&dir, "feature", "vanish PR");
    checkout(&dir, "feature");
    let barrier = tmpdir("barrier-vanish-window");
    let base_before = ref_oid(&dir, "refs/heads/main").unwrap();
    let head_before = ref_oid(&dir, "refs/forge/prs/1/head").unwrap();

    let bin = env!("CARGO_BIN_EXE_git-forge");
    let child: Child = Command::new(bin)
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

    // Delete the head ref while the merge is parked at the barrier.
    let (cd, _, ed) = git(&dir, &["update-ref", "-d", "refs/forge/prs/1/head"]);
    assert_eq!(cd, 0, "setup: delete pr head ref: {ed}");

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
    let output = child.wait_with_output().unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !output.status.success(),
        "a vanished head ref must fail the merge: {stderr}"
    );
    assert!(
        stderr.contains("final transaction failed"),
        "stderr must name the finalization failure: {stderr}"
    );
    assert!(
        stderr.contains("pending result ref refs/forge/prs/1/result left in place"),
        "stderr must name the stranded pending ref: {stderr}"
    );
    assert_eq!(
        ref_oid(&dir, "refs/heads/main").unwrap(),
        base_before,
        "base ref must not advance"
    );
    assert!(
        ref_oid(&dir, "refs/forge/prs/1/head").is_none(),
        "head ref must stay deleted"
    );
    assert!(
        ref_oid(&dir, "refs/forge/prs/1/result").is_some(),
        "pending result ref must be left in place"
    );
    let _ = head_before;
}
/// VAL-002: a PR created with `--closes <n>` writes the `issue.close` event
/// IN THE SAME finalize transaction — after merging, the linked issue folds
/// closed and its chain carries exactly one `issue.close` event whose actor
/// is the single resolved identity (the close event is never a second
/// identity path) and whose body stays empty `{}`.
#[test]
fn merge_with_closes_writes_close_event_and_closes_issue() {
    let dir = tmpdir("mcloses");
    init_repo(&dir);
    // Distinct from the fixture identity so the assertion proves the close
    // event's actor comes from repo identity resolution, not a fallback.
    let (c, _, e) = git(&dir, &["config", "user.email", "someone@example.com"]);
    assert_eq!(c, 0, "config user.email failed: {e}");
    let (ci, _, ei) = forge(&dir, &["forge", "issue", "new", "linked bug"]);
    assert_eq!(ci, 0, "issue new failed: {ei}");
    create_approved_pr_closes(&dir, "feature", "closes PR", 1, 2);
    checkout(&dir, "feature");
    let base_before = ref_oid(&dir, "refs/heads/main").unwrap();

    let (cm, om, em) = forge(&dir, &["forge", "pr", "merge", "2"]);
    assert_eq!(cm, 0, "merge with --closes must succeed: {em} {om}");
    assert!(
        ref_oid(&dir, "refs/heads/main").unwrap() != base_before,
        "base branch must advance on a successful merge"
    );

    let (cs, os, es) = forge(&dir, &["forge", "issue", "show", "1"]);
    assert_eq!(cs, 0, "issue show failed: {es}");
    assert!(
        os.contains("closed"),
        "linked issue must fold closed after the merge: {os}"
    );

    let events = chain_event_jsons(&dir, "refs/forge/issues/1");
    let closes: Vec<&String> = events
        .iter()
        .filter(|j| j.contains("\"kind\":\"issue.close\""))
        .collect();
    assert_eq!(
        closes.len(),
        1,
        "exactly one issue.close event expected, got {}: {events:?}",
        closes.len()
    );
    let close_json = *closes.first().unwrap();
    assert!(
        close_json.contains("\"body\":{}"),
        "issue.close body must stay empty {{}}: {close_json}"
    );
    assert!(
        close_json.contains("\"actor\":\"someone@example.com\""),
        "issue.close actor must be the resolved repo identity: {close_json}"
    );
    assert!(
        close_json.contains("\"entity\":\"issue\"") && close_json.contains("\"entity_id\":1"),
        "issue.close must belong to the linked issue: {close_json}"
    );
    // The close commit is the chain tip: its ONLY parent is the pre-merge tip.
    assert!(events.last().unwrap().contains("\"kind\":\"issue.close\""));
}

/// VAL-003: an already-closed target must not get a second `issue.close` — the
/// merge succeeds with base+PR actions only, the issue chain length is
/// unchanged, and the folded state stays closed.
#[test]
fn merge_with_closes_on_already_closed_issue_is_noop() {
    let dir = tmpdir("mcloses-noop");
    init_repo(&dir);
    let (ci, _, ei) = forge(&dir, &["forge", "issue", "new", "linked bug"]);
    assert_eq!(ci, 0, "issue new failed: {ei}");
    let (cc, _, ec) = forge(&dir, &["forge", "issue", "close", "1"]);
    assert_eq!(cc, 0, "issue close failed: {ec}");
    let events_before = chain_event_jsons(&dir, "refs/forge/issues/1");
    let closes_before = events_before
        .iter()
        .filter(|j| j.contains("\"kind\":\"issue.close\""))
        .count();
    assert_eq!(closes_before, 1, "setup: one manual close event");

    create_approved_pr_closes(&dir, "feature", "closes closed PR", 1, 2);
    checkout(&dir, "feature");
    let base_before = ref_oid(&dir, "refs/heads/main").unwrap();
    let (cm, om, em) = forge(&dir, &["forge", "pr", "merge", "2"]);
    assert_eq!(
        cm, 0,
        "merge must still succeed on an already-closed target: {em} {om}"
    );
    assert!(
        ref_oid(&dir, "refs/heads/main").unwrap() != base_before,
        "base branch must advance on a successful merge"
    );

    let events_after = chain_event_jsons(&dir, "refs/forge/issues/1");
    assert_eq!(
        events_after.len(),
        events_before.len(),
        "issue chain length must be unchanged: {events_after:?}"
    );
    let closes_after = events_after
        .iter()
        .filter(|j| j.contains("\"kind\":\"issue.close\""))
        .count();
    assert_eq!(
        closes_after, closes_before,
        "no second issue.close event may be written: {events_after:?}"
    );
    let (cs, os, es) = forge(&dir, &["forge", "issue", "show", "1"]);
    assert_eq!(cs, 0, "issue show failed: {es}");
    assert!(
        os.contains("closed"),
        "issue must still fold closed (no reopen side effect): {os}"
    );
}

/// The finalize transaction is all-or-nothing: a CAS clash injected in the
/// barrier window (pending result ref moved) while a `--closes` target is
/// staged must leave the issue ref untouched — no partial `issue.close`
/// event, issue folds open, base and PR head unchanged.
#[test]
fn finalize_cas_failure_leaves_linked_issue_open() {
    let dir = tmpdir("mcloses-fail");
    init_repo(&dir);
    let (ci, _, ei) = forge(&dir, &["forge", "issue", "new", "linked bug"]);
    assert_eq!(ci, 0, "issue new failed: {ei}");
    create_approved_pr_closes(&dir, "feature", "closes PR", 1, 2);
    checkout(&dir, "feature");
    let barrier = tmpdir("mcloses-fail-window");
    let base_before = ref_oid(&dir, "refs/heads/main").unwrap();
    let head_before = ref_oid(&dir, "refs/forge/prs/2/head").unwrap();
    let issue_before = ref_oid(&dir, "refs/forge/issues/1").unwrap();

    let bin = env!("CARGO_BIN_EXE_git-forge");
    let child: Child = Command::new(bin)
        .args(["forge", "pr", "merge", "2"])
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
    assert!(
        ref_oid(&dir, "refs/forge/prs/2/result").is_some(),
        "pending result ref must exist in barrier window"
    );
    // Sabotage the completion transaction: move the pending ref away from the
    // expected result commit, then release the barrier.
    let (c, _, e) = git(
        &dir,
        &["update-ref", "refs/forge/prs/2/result", &base_before],
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
    let output = child.wait_with_output().unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !output.status.success(),
        "the sabotaged transaction must fail: {stderr}"
    );
    assert!(
        stderr.contains("final transaction failed"),
        "stderr must name the finalization failure: {stderr}"
    );
    assert_eq!(
        ref_oid(&dir, "refs/heads/main").unwrap(),
        base_before,
        "base must not advance on a failed transaction"
    );
    assert_eq!(
        ref_oid(&dir, "refs/forge/prs/2/head").unwrap(),
        head_before,
        "PR head chain must not advance on a failed transaction"
    );
    assert_eq!(
        ref_oid(&dir, "refs/forge/issues/1").unwrap(),
        issue_before,
        "issue ref must be untouched by a failed transaction"
    );
    let events = chain_event_jsons(&dir, "refs/forge/issues/1");
    assert!(
        events
            .iter()
            .all(|j| !j.contains("\"kind\":\"issue.close\"")),
        "no partial issue.close event may exist: {events:?}"
    );
    let (cs, os, es) = forge(&dir, &["forge", "issue", "show", "1"]);
    assert_eq!(cs, 0, "issue show failed: {es}");
    assert!(
        os.contains("open"),
        "issue must stay open after a failed transaction: {os}"
    );
    assert_no_temp_worktree(&dir);
}

/// The linked issue may move BETWEEN the per-attempt staging and the
/// update-ref batch — the exact window the finalize retry loop exists to
/// heal (F-006: the previous version parked at the PRE-finalize barrier, so
/// the move landed before the first staging and the first transaction CASed
/// the already-moved tip — no retry). GIT_FORGE_TEST_MERGE_ISSUE_BARRIER
/// (in run_finalize, right after per-attempt staging) parks the merge there;
/// the concurrent comment lands AFTER the close was staged at the old tip, so
/// the first transaction CASes a stale issue tip and fails, the loop
/// re-stages at the moved tip (same event, one close event in the final
/// chain) and the merge still succeeds.
#[test]
fn merge_retries_when_issue_moves_during_finalize() {
    let dir = tmpdir("mcloses-move");
    init_repo(&dir);
    let (ci, _, ei) = forge(&dir, &["forge", "issue", "new", "linked bug"]);
    assert_eq!(ci, 0, "issue new failed: {ei}");
    create_approved_pr_closes(&dir, "feature", "closes PR", 1, 2);
    checkout(&dir, "feature");

    let barrier = tmpdir("mcloses-move-window");
    let issue_before = ref_oid(&dir, "refs/forge/issues/1").unwrap();
    let bin = env!("CARGO_BIN_EXE_git-forge");
    let child: Child = Command::new(bin)
        .args(["forge", "pr", "merge", "2"])
        .current_dir(&dir)
        .env("GIT_FORGE_TEST_MERGE_ISSUE_BARRIER", &barrier)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();

    let deadline = Instant::now() + Duration::from_secs(30);
    // Round 0: the per-attempt close is staged at the pre-move tip; move the
    // issue NOW, so the first transaction CASes a stale issue tip and fails,
    // and the loop re-stages at the moved tip.
    wait_issue_barrier_round(&barrier, 0, deadline, "first staging");
    let (cm, _, em) = forge(&dir, &["forge", "issue", "comment", "1", "while parked"]);
    assert_eq!(cm, 0, "concurrent issue comment failed: {em}");
    release_issue_barrier(&barrier);

    // Round 1: the retry re-staged at the moved tip; release WITHOUT moving,
    // so the second transaction succeeds.
    wait_issue_barrier_round(&barrier, 1, deadline, "retry staging");
    release_issue_barrier(&barrier);

    let output = child.wait_with_output().unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "merge must retry and succeed after the issue moved: {stderr}"
    );

    let events = chain_event_jsons(&dir, "refs/forge/issues/1");
    let closes: Vec<&String> = events
        .iter()
        .filter(|j| j.contains("\"kind\":\"issue.close\""))
        .collect();
    assert_eq!(
        closes.len(),
        1,
        "exactly one issue.close event in the final chain: {events:?}"
    );
    assert!(
        events.last().unwrap().contains("\"kind\":\"issue.close\""),
        "the close event must be re-parented onto the moved tip: {events:?}"
    );
    assert!(
        events.iter().any(|j| j.contains("while parked")),
        "the concurrent comment must be in the chain: {events:?}"
    );
    let (cs, os, es) = forge(&dir, &["forge", "issue", "show", "1"]);
    assert_eq!(cs, 0, "issue show failed: {es}");
    assert!(os.contains("closed"), "issue must fold closed: {os}");

    assert_ne!(
        ref_oid(&dir, "refs/forge/issues/1").unwrap(),
        issue_before,
        "the issue ref must have moved (concurrent comment + re-parented close)"
    );
}
/// A vanished linked-issue ref is a hard refusal, never a silent skip: the
/// close cannot be anchored to a real chain, so the merge aborts with refs
/// unchanged and the pending result ref left in place.
#[test]
fn merge_refuses_when_linked_issue_ref_disappears_during_barrier() {
    let dir = tmpdir("mcloses-vanish");
    init_repo(&dir);
    let (ci, _, ei) = forge(&dir, &["forge", "issue", "new", "linked bug"]);
    assert_eq!(ci, 0, "issue new failed: {ei}");
    create_approved_pr_closes(&dir, "feature", "closes PR", 1, 2);
    checkout(&dir, "feature");
    let barrier = tmpdir("mcloses-vanish-window");
    let base_before = ref_oid(&dir, "refs/heads/main").unwrap();
    let issue_before = ref_oid(&dir, "refs/forge/issues/1").unwrap();

    let bin = env!("CARGO_BIN_EXE_git-forge");
    let child: Child = Command::new(bin)
        .args(["forge", "pr", "merge", "2"])
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
    let (cd, _, ed) = git(&dir, &["update-ref", "-d", "refs/forge/issues/1"]);
    assert_eq!(cd, 0, "setup: delete issue ref: {ed}");
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
    let output = child.wait_with_output().unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !output.status.success(),
        "a vanished issue ref must fail the merge: {stderr}"
    );
    assert!(
        stderr.contains("linked issue #1 ref is gone"),
        "stderr must name the missing issue ref: {stderr}"
    );
    assert!(
        stderr.contains("pending result ref refs/forge/prs/2/result left in place"),
        "stderr must name the stranded pending ref: {stderr}"
    );
    assert_eq!(
        ref_oid(&dir, "refs/heads/main").unwrap(),
        base_before,
        "base ref must not advance"
    );
    assert!(
        ref_oid(&dir, "refs/forge/prs/2/result").is_some(),
        "pending result ref must be left in place"
    );
    let _ = issue_before;
}
/// VAL-004 (t3): a merge whose gate is RED (missing approve) while the PR
/// declares `--closes` must never write a partial `issue.close`. The gate
/// read fails BOTH before the finalize transaction and before any temp
/// worktree is created: merge exits non-zero, `issue show` still reports
/// open, the issue chain carries no `issue.close` event, the PR head chain
/// carries no `pr.merge` event, and base/head/issue refs are unchanged.
#[test]
fn merge_gate_red_with_closes_keeps_issue_open() {
    let dir = tmpdir("mcloses-gatered");
    init_repo(&dir);
    let (ci, _, ei) = forge(&dir, &["forge", "issue", "new", "linked bug"]);
    assert_eq!(ci, 0, "issue new failed: {ei}");
    // Approved? NO. The PR declares the closes relationship but is never
    // reviewed, so the L1 gate (effective approve) is red. CI plan is present
    // so the gate failure is NOT the CI check (t3: gate red = missing approve).
    make_feature(&dir, "feature", "feat\n");
    let (c, _, e) = git(&dir, &["checkout", "-q", "feature"]);
    assert_eq!(c, 0, "checkout feature failed: {e}");
    add_passing_ci_plan(&dir);
    let (c, _, e) = git(&dir, &["checkout", "-q", "main"]);
    assert_eq!(c, 0, "checkout main failed: {e}");
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
            "gate red closes PR",
            "--closes",
            "1",
        ],
    );
    assert_eq!(c, 0, "pr create --closes failed: {e} {o}");
    let (c, _, e) = forge(&dir, &["forge", "ci", "run", "2"]);
    assert_eq!(c, 0, "ci run failed: {e}");
    checkout(&dir, "feature");

    let base_before = ref_oid(&dir, "refs/heads/main").unwrap();
    let head_before = ref_oid(&dir, "refs/forge/prs/2/head").unwrap();
    let issue_before = ref_oid(&dir, "refs/forge/issues/1").unwrap();
    let (cm, om, em) = forge(&dir, &["forge", "pr", "merge", "2"]);
    assert_ne!(cm, 0, "gate-red merge must fail: {om} {em}");
    assert!(em.contains("not approved"), "stderr: {em}");
    assert_eq!(
        ref_oid(&dir, "refs/heads/main").unwrap(),
        base_before,
        "base must not move on a gate-red failure"
    );
    assert_eq!(
        ref_oid(&dir, "refs/forge/prs/2/head").unwrap(),
        head_before,
        "PR head must not move on a gate-red failure"
    );
    assert_eq!(
        ref_oid(&dir, "refs/forge/issues/1").unwrap(),
        issue_before,
        "issue ref must not move on a gate-red failure"
    );
    let issue_events = chain_event_jsons(&dir, "refs/forge/issues/1");
    assert!(
        issue_events
            .iter()
            .all(|j| !j.contains("\"kind\":\"issue.close\"")),
        "no partial issue.close: {issue_events:?}"
    );
    let pr_events = chain_event_jsons(&dir, "refs/forge/prs/2/head");
    assert!(
        pr_events
            .iter()
            .all(|j| !j.contains("\"kind\":\"pr.merge\"")),
        "no pr.merge event on a gate-red failure: {pr_events:?}"
    );
    let (cs, os, es) = forge(&dir, &["forge", "issue", "show", "1"]);
    assert_eq!(cs, 0, "issue show failed: {es}");
    assert!(os.contains("open"), "issue must stay open: {os}");
    assert_no_temp_worktree_pr(&dir, 2);
}

/// VAL-004 (t3): a stale-base merge with `--closes` must refuse BEFORE
/// touching anything — the stale check runs after the gate read but before
/// the worktree/finalize side effects, so the linked issue folds open, the
/// issue chain has no `issue.close`, the PR head chain has no `pr.merge`,
/// and base/head/issue refs stay put.
#[test]
fn merge_stale_base_with_closes_keeps_issue_open() {
    let dir = tmpdir("mcloses-stale");
    init_repo(&dir);
    let (ci, _, ei) = forge(&dir, &["forge", "issue", "new", "linked bug"]);
    assert_eq!(ci, 0, "issue new failed: {ei}");
    create_approved_pr_closes(&dir, "feature", "stale closes PR", 1, 2);
    let base_snapshot = ref_oid(&dir, "refs/forge/prs/2/base").unwrap();
    // Advance main AFTER the PR snapshot, so the merge gate passes but the
    // stale-base check rejects.
    let (c, _, e) = git(&dir, &["checkout", "-q", "main"]);
    assert_eq!(c, 0, "checkout main failed: {e}");
    std::fs::write(dir.join("later.txt"), "later\n").unwrap();
    let (c, _, e) = git(&dir, &["add", "later.txt"]);
    assert_eq!(c, 0, "add later.txt failed: {e}");
    let (c, _, e) = git(&dir, &["commit", "-q", "-m", "advance base"]);
    assert_eq!(c, 0, "commit advance failed: {e}");
    let base_current = ref_oid(&dir, "refs/heads/main").unwrap();
    assert_ne!(base_snapshot, base_current, "setup: base must have moved");
    checkout(&dir, "feature");

    let head_before = ref_oid(&dir, "refs/forge/prs/2/head").unwrap();
    let issue_before = ref_oid(&dir, "refs/forge/issues/1").unwrap();
    let (cm, om, em) = forge(&dir, &["forge", "pr", "merge", "2"]);
    assert_ne!(cm, 0, "stale-base merge must fail: {om} {em}");
    assert!(
        em.contains("has moved"),
        "stderr must name the stale base: {em}"
    );
    assert_eq!(
        ref_oid(&dir, "refs/heads/main").unwrap(),
        base_current,
        "base must not change on a stale rejection"
    );
    assert_eq!(
        ref_oid(&dir, "refs/forge/prs/2/head").unwrap(),
        head_before,
        "PR head must not move on a stale rejection"
    );
    assert_eq!(
        ref_oid(&dir, "refs/forge/issues/1").unwrap(),
        issue_before,
        "issue ref must not move on a stale rejection"
    );
    let issue_events = chain_event_jsons(&dir, "refs/forge/issues/1");
    assert!(
        issue_events
            .iter()
            .all(|j| !j.contains("\"kind\":\"issue.close\"")),
        "no partial issue.close: {issue_events:?}"
    );
    let pr_events = chain_event_jsons(&dir, "refs/forge/prs/2/head");
    assert!(
        pr_events
            .iter()
            .all(|j| !j.contains("\"kind\":\"pr.merge\"")),
        "no pr.merge event on a stale rejection: {pr_events:?}"
    );
    let (cs, os, es) = forge(&dir, &["forge", "issue", "show", "1"]);
    assert_eq!(cs, 0, "issue show failed: {es}");
    assert!(os.contains("open"), "issue must stay open: {os}");
    assert_no_temp_worktree_pr(&dir, 2);
}

/// VAL-004 (t3): a merge conflict inside the disposable worktree with
/// `--closes` must leave the linked issue open — the failure happens in
/// execute_strategy, BEFORE the finalize transaction, so no `issue.close`,
/// no `pr.merge`, no ref move, and the temp worktree is cleaned (existing
/// invariant preserved under the closes target).
#[test]
#[allow(clippy::cognitive_complexity)]
fn merge_conflict_with_closes_keeps_issue_open() {
    let dir = tmpdir("mcloses-conflict");
    init_repo(&dir);
    let (ci, _, ei) = forge(&dir, &["forge", "issue", "new", "linked bug"]);
    assert_eq!(ci, 0, "issue new failed: {ei}");
    // Feature and base both change base.txt differently after the fork.
    make_feature(&dir, "feature", "feat\n");
    let (c, _, e) = git(&dir, &["checkout", "-q", "feature"]);
    assert_eq!(c, 0, "checkout feature failed: {e}");
    std::fs::write(dir.join("base.txt"), "feature-change\n").unwrap();
    let (c, _, e) = git(&dir, &["add", "base.txt"]);
    assert_eq!(c, 0, "add base.txt failed: {e}");
    let (c, _, e) = git(&dir, &["commit", "-q", "-m", "feature conflict"]);
    assert_eq!(c, 0, "commit conflict failed: {e}");
    add_passing_ci_plan(&dir);
    let (c, _, e) = git(&dir, &["checkout", "-q", "main"]);
    assert_eq!(c, 0, "checkout main failed: {e}");
    std::fs::write(dir.join("base.txt"), "main-change\n").unwrap();
    let (c, _, e) = git(&dir, &["add", "base.txt"]);
    assert_eq!(c, 0, "add base.txt failed: {e}");
    let (c, _, e) = git(&dir, &["commit", "-q", "-m", "main conflict"]);
    assert_eq!(c, 0, "commit conflict failed: {e}");
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
            "conflict closes PR",
            "--closes",
            "1",
        ],
    );
    assert_eq!(c, 0, "pr create --closes failed: {e} {o}");
    let (c, o, e) = forge(&dir, &["forge", "ci", "run", "2"]);
    assert_eq!(c, 0, "ci run failed: {e} {o}");
    let (c, o, e) = forge(&dir, &["forge", "pr", "review", "2", "--approve"]);
    assert_eq!(c, 0, "review approve failed: {e} {o}");
    checkout(&dir, "feature");

    let base_before = ref_oid(&dir, "refs/heads/main").unwrap();
    let head_before = ref_oid(&dir, "refs/forge/prs/2/head").unwrap();
    let issue_before = ref_oid(&dir, "refs/forge/issues/1").unwrap();
    let (cm, om, em) = forge(&dir, &["forge", "pr", "merge", "2"]);
    assert_ne!(cm, 0, "conflict merge must fail: {om} {em}");
    assert_eq!(
        ref_oid(&dir, "refs/heads/main").unwrap(),
        base_before,
        "base must not move on a conflict"
    );
    assert_eq!(
        ref_oid(&dir, "refs/forge/prs/2/head").unwrap(),
        head_before,
        "PR head must not move on a conflict"
    );
    assert_eq!(
        ref_oid(&dir, "refs/forge/issues/1").unwrap(),
        issue_before,
        "issue ref must not move on a conflict"
    );
    let issue_events = chain_event_jsons(&dir, "refs/forge/issues/1");
    assert!(
        issue_events
            .iter()
            .all(|j| !j.contains("\"kind\":\"issue.close\"")),
        "no partial issue.close: {issue_events:?}"
    );
    let pr_events = chain_event_jsons(&dir, "refs/forge/prs/2/head");
    assert!(
        pr_events
            .iter()
            .all(|j| !j.contains("\"kind\":\"pr.merge\"")),
        "no pr.merge event on a conflict: {pr_events:?}"
    );
    let (cs, os, es) = forge(&dir, &["forge", "issue", "show", "1"]);
    assert_eq!(cs, 0, "issue show failed: {es}");
    assert!(os.contains("open"), "issue must stay open: {os}");
    assert_no_temp_worktree_pr(&dir, 2);
}

/// CAS-exhaustion (t3): the linked issue keeps moving BETWEEN every
/// per-attempt staging and its update-ref batch, so the finalize CAS retry
/// loop exhausts its budget (initial attempt + MAX_FINALIZE_RETRIES) and
/// refuses with no ref updates: the issue folds open, the issue chain has no
/// issue.close event, the PR head chain has no pr.merge, base/head refs are
/// unchanged, the pending result ref is cleaned up, and the temp worktree
/// was already gone before the finalize phase.
#[test]
#[allow(clippy::cognitive_complexity)]
fn merge_issue_cas_exhaustion_keeps_issue_open() {
    let dir = tmpdir("mcloses-cas-exhaust");
    init_repo(&dir);
    let (ci, _, ei) = forge(&dir, &["forge", "issue", "new", "linked bug"]);
    assert_eq!(ci, 0, "issue new failed: {ei}");
    create_approved_pr_closes(&dir, "feature", "closes PR", 1, 2);
    checkout(&dir, "feature");
    let barrier = tmpdir("mcloses-cas-exhaust-window");
    let base_before = ref_oid(&dir, "refs/heads/main").unwrap();
    let head_before = ref_oid(&dir, "refs/forge/prs/2/head").unwrap();
    let issue_before = ref_oid(&dir, "refs/forge/issues/1").unwrap();

    let bin = env!("CARGO_BIN_EXE_git-forge");
    let child: Child = Command::new(bin)
        .args(["forge", "pr", "merge", "2"])
        .current_dir(&dir)
        .env("GIT_FORGE_TEST_MERGE_ISSUE_BARRIER", &barrier)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();

    // Initial attempt + 3 retries: move the issue inside EVERY window so the
    // first-transaction CAS loses each time and the retry budget exhausts.
    let deadline = Instant::now() + Duration::from_secs(30);
    for round in 0..4u32 {
        wait_issue_barrier_round(&barrier, round, deadline, "cas-exhaust");
        let (cm, _, em) = forge(
            &dir,
            &["forge", "issue", "comment", "1", &format!("move {round}")],
        );
        assert_eq!(cm, 0, "concurrent issue comment failed: {em}");
        release_issue_barrier(&barrier);
    }
    let output = child.wait_with_output().unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !output.status.success(),
        "CAS exhaustion must fail the merge: {stderr}"
    );
    assert!(
        stderr.contains("kept moving during finalization"),
        "stderr must name the exhausted retries: {stderr}"
    );
    assert!(stderr.contains("refs unchanged"), "stderr: {stderr}");
    assert_eq!(
        ref_oid(&dir, "refs/heads/main").unwrap(),
        base_before,
        "base must not move after CAS exhaustion"
    );
    assert_eq!(
        ref_oid(&dir, "refs/forge/prs/2/head").unwrap(),
        head_before,
        "PR head must not move after CAS exhaustion"
    );

    // The test's own comments moved the issue chain; the merge must not have
    // appended anything on top of the LAST comment tip (no close event).
    let tip_after_moves = ref_oid(&dir, "refs/forge/issues/1").unwrap();
    assert_ne!(
        tip_after_moves, issue_before,
        "setup: comments must move the issue"
    );
    assert!(
        ref_oid(&dir, "refs/forge/prs/2/result").is_none(),
        "pending result ref must be cleaned up on exhaustion"
    );
    let issue_events = chain_event_jsons(&dir, "refs/forge/issues/1");
    assert!(
        issue_events
            .iter()
            .all(|j| !j.contains("\"kind\":\"issue.close\"")),
        "no partial issue.close: {issue_events:?}"
    );
    let pr_events = chain_event_jsons(&dir, "refs/forge/prs/2/head");
    assert!(
        pr_events
            .iter()
            .all(|j| !j.contains("\"kind\":\"pr.merge\"")),
        "no pr.merge event after CAS exhaustion: {pr_events:?}"
    );
    let (cs, os, es) = forge(&dir, &["forge", "issue", "show", "1"]);
    assert_eq!(cs, 0, "issue show failed: {es}");
    assert!(os.contains("open"), "issue must stay open: {os}");
    assert_no_temp_worktree_pr(&dir, 2);
}

// ── t4 / VAL-006: merge refuses an abandoned PR, naming the reason ──

/// An abandoned PR whose gate would otherwise refuse with "not approved" must
/// be refused with the ABANDON reason instead: the abandoned check precedes
/// the merge-gate predicate. No ref moves, no `pr.merge` event, no worktree.
#[test]
fn merge_refuses_abandoned_pr_naming_reason_before_gate() {
    let dir = tmpdir("abandon-gate-precedence");
    init_repo(&dir);
    make_feature(&dir, "feature", "feat\n");
    assert_eq!(
        forge(
            &dir,
            &[
                "forge",
                "pr",
                "create",
                "--source",
                "feature",
                "--base",
                "main",
                "abandoned PR"
            ],
        )
        .0,
        0
    );
    // No approve, no ci run: the gate alone would refuse "not approved". The
    // abandonment check must fire FIRST and name the abandon reason.
    assert_eq!(
        forge(
            &dir,
            &[
                "forge",
                "pr",
                "abandon",
                "1",
                "--reason",
                "dropped by owner"
            ]
        )
        .0,
        0
    );
    checkout(&dir, "feature");
    let base_before = ref_oid(&dir, "refs/heads/main").unwrap();
    let head_before = ref_oid(&dir, "refs/forge/prs/1/head").unwrap();
    let (c, _o, e) = forge(&dir, &["forge", "pr", "merge", "1"]);
    assert_ne!(c, 0, "merge on an abandoned PR must refuse: {e}");
    assert!(
        e.contains("dropped by owner"),
        "stderr must name the abandon reason: {e}"
    );
    assert!(
        !e.contains("not approved"),
        "the abandoned check must precede the gate predicate: {e}"
    );
    assert_eq!(
        ref_oid(&dir, "refs/heads/main").unwrap(),
        base_before,
        "base must not move"
    );
    assert_eq!(
        ref_oid(&dir, "refs/forge/prs/1/head").unwrap(),
        head_before,
        "PR head must not move"
    );
    let events = chain_event_jsons(&dir, "refs/forge/prs/1/head");
    assert!(
        events.iter().all(|j| !j.contains("\"kind\":\"pr.merge\"")),
        "no pr.merge event: {events:?}"
    );
    assert_no_temp_worktree_pr(&dir, 1);
}

/// An approved + green-CI PR that is then abandoned must refuse the merge with
/// the reason, even though the gate itself would have let it through.
#[test]
fn merge_refuses_abandoned_approved_pr_without_ref_changes() {
    let dir = tmpdir("abandon-approved");
    init_repo(&dir);
    create_approved_pr(&dir, "feature", "approved then abandoned");
    assert_eq!(
        forge(
            &dir,
            &[
                "forge",
                "pr",
                "abandon",
                "1",
                "--reason",
                "owner changed mind"
            ],
        )
        .0,
        0
    );
    checkout(&dir, "feature");
    let base_before = ref_oid(&dir, "refs/heads/main").unwrap();
    let head_before = ref_oid(&dir, "refs/forge/prs/1/head").unwrap();
    let (c, _o, e) = forge(&dir, &["forge", "pr", "merge", "1"]);
    assert_ne!(c, 0, "merge on an abandoned PR must refuse: {e}");
    assert!(
        e.contains("owner changed mind"),
        "stderr must name the abandon reason: {e}"
    );
    assert_eq!(
        ref_oid(&dir, "refs/heads/main").unwrap(),
        base_before,
        "base must not move"
    );
    assert_eq!(
        ref_oid(&dir, "refs/forge/prs/1/head").unwrap(),
        head_before,
        "PR head must not move"
    );
    let events = chain_event_jsons(&dir, "refs/forge/prs/1/head");
    assert!(
        events.iter().all(|j| !j.contains("\"kind\":\"pr.merge\"")),
        "no pr.merge event: {events:?}"
    );
    assert_no_temp_worktree_pr(&dir, 1);
}
// ── t6 / VAL-009+VAL-010: the abandon boundary matrix, one scenario ──

/// The whole terminal-state matrix against ONE abandoned PR: every mutator —
/// merge, review, ci run and a second abandon — refuses with a non-zero exit
/// and names the SAME (original) reason text; each refusal writes nothing
/// (PR head chain and base ref unchanged, exactly one pr.abandoned event),
/// and `pr show` keeps displaying the original reason.
#[test]
fn all_mutators_refuse_abandoned_pr_with_same_reason_and_no_refs_move() {
    let dir = tmpdir("abandon-matrix");
    init_repo(&dir);
    create_approved_pr(&dir, "feature", "matrix PR");
    let reason = "matrix: dropped by owner";
    let (c, _o, e) = forge(&dir, &["forge", "pr", "abandon", "1", "--reason", reason]);
    assert_eq!(c, 0, "abandon must succeed: {e}");
    checkout(&dir, "feature");
    let head_before = ref_oid(&dir, "refs/forge/prs/1/head").unwrap();
    let base_before = ref_oid(&dir, "refs/heads/main").unwrap();
    // merge: same wording as the other mutators, names the reason.
    let (c, _o, e) = forge(&dir, &["forge", "pr", "merge", "1"]);
    assert_ne!(c, 0, "merge must refuse an abandoned PR: {e}");
    assert!(
        e.contains("PR #1 is abandoned: matrix: dropped by owner"),
        "merge refusal must use the shared wording: {e}"
    );
    // review: same wording.
    let (c, _o, e) = forge(&dir, &["forge", "pr", "review", "1", "--approve"]);
    assert_ne!(c, 0, "review must refuse an abandoned PR: {e}");
    assert!(
        e.contains("PR #1 is abandoned: matrix: dropped by owner"),
        "review refusal must use the shared wording: {e}"
    );
    // ci run: same wording.
    let (c, _o, e) = forge(&dir, &["forge", "ci", "run", "1"]);
    assert_ne!(c, 0, "ci run must refuse an abandoned PR: {e}");
    assert!(
        e.contains("PR #1 is abandoned: matrix: dropped by owner"),
        "ci run refusal must use the shared wording: {e}"
    );
    // second abandon: names the ORIGINAL reason, never the new one.
    let (c, _o, e) = forge(
        &dir,
        &[
            "forge",
            "pr",
            "abandon",
            "1",
            "--reason",
            "different reason",
        ],
    );
    assert_ne!(c, 0, "second abandon must refuse: {e}");
    assert!(
        e.contains("PR #1 is already abandoned: matrix: dropped by owner")
            && !e.contains("different reason"),
        "second abandon must name the ORIGINAL reason: {e}"
    );
    // No refusal moved anything.
    assert_eq!(
        ref_oid(&dir, "refs/forge/prs/1/head").unwrap(),
        head_before,
        "no refusal may append an event to the PR chain"
    );
    assert_eq!(
        ref_oid(&dir, "refs/heads/main").unwrap(),
        base_before,
        "no refusal may move the base ref"
    );
    let events = chain_event_jsons(&dir, "refs/forge/prs/1/head");
    assert_eq!(
        events
            .iter()
            .filter(|j| j.contains("\"kind\":\"pr.abandoned\""))
            .count(),
        1,
        "exactly one pr.abandoned event: {events:?}"
    );
    // show displays the original reason only.
    let (_cs, os, _es) = forge(&dir, &["forge", "pr", "show", "1"]);
    assert!(
        os.contains("abandoned: matrix: dropped by owner"),
        "show: {os}"
    );
    assert!(!os.contains("different reason"), "show: {os}");
}

// ── t0 / VAL-101..104: `pr merge --ff` fast-forward strategy ──────────────

#[test]
fn ff_merge_fast_forwards_to_source_head() {
    // VAL-101: `pr merge --ff` on a strictly-on-top source exits 0; the
    // integrated base tip IS the verified source_head (no new commit,
    // single-parent chain preserved); `pr show` displays result_commit ==
    // source_head; the base tip's parent is the prior base tip.
    let dir = tmpdir("ffok");
    init_repo(&dir);
    // Single-commit source strictly on top of base (feature + CI plan in ONE
    // commit), so the integrated tip's sole parent is exactly the prior base
    // tip. The PR then gets a green CI Check and an approval (merge gate).
    let (c, _, e) = git(&dir, &["checkout", "-q", "-b", "feature"]);
    assert_eq!(c, 0, "checkout -b feature: {e}");
    std::fs::write(dir.join("feature.txt"), "feat\n").unwrap();
    add_passing_ci_plan(&dir);
    let (c, _, e) = git(&dir, &["checkout", "-q", "main"]);
    assert_eq!(c, 0, "checkout main: {e}");
    let (c, o, e) = forge(
        &dir,
        &[
            "forge", "pr", "create", "--source", "feature", "--base", "main", "ff PR",
        ],
    );
    assert_eq!(c, 0, "pr create failed: {e} {o}");
    ci_green(&dir);
    let (c, o, e) = forge(&dir, &["forge", "pr", "review", "1", "--approve"]);
    assert_eq!(c, 0, "approve failed: {e} {o}");
    let base_before = ref_oid(&dir, "refs/heads/main").unwrap();
    let source_before = ref_oid(&dir, "refs/heads/feature").unwrap();
    checkout(&dir, "feature");
    let (c, o, e) = forge(&dir, &["forge", "pr", "merge", "1", "--ff"]);
    assert!(c == 0, "ff merge failed: {e} {o}");
    // result_commit == source_head: base tip is exactly the source head.
    let base_after = ref_oid(&dir, "refs/heads/main").unwrap();
    assert_eq!(base_after, source_before, "base tip must equal source head");
    // No new commit: the source tip's sole parent is the PRIOR base tip.
    let (_, parents, _) = git(&dir, &["rev-list", "--parents", "-n", "1", &base_after]);
    let p: Vec<&str> = parents.split_whitespace().collect();
    assert_eq!(p.len(), 2, "source tip must keep a single parent");
    assert_eq!(p[1], base_before, "parent must be the prior base tip");
    // pr show displays result_commit == source_head.
    let (cs, os, es) = forge(&dir, &["forge", "pr", "show", "1"]);
    assert_eq!(cs, 0, "pr show failed: {es}");
    assert!(os.contains("merged:"), "pr show must display merged: {os}");
    assert!(
        os.contains(&source_before),
        "pr show must display result_commit {source_before}: {os}"
    );
    // The source branch is untouched by the merge.
    assert_eq!(
        ref_oid(&dir, "refs/heads/feature").unwrap(),
        source_before,
        "source branch unchanged"
    );
}

#[test]
fn ff_non_ffable_pr_rejected_naming_rebase_remedy() {
    // VAL-102: a PR whose source does NOT sit directly on top of the snapshot
    // base (merge_base != base_head) refuses `--ff` with a distinct error
    // naming the rebase remedy; refs unchanged; PR remains unmerged.
    let dir = tmpdir("ffnoff");
    init_repo(&dir);
    make_feature(&dir, "feature", "feat\n");
    // Advance main AFTER the feature forked, then create the PR: the snapshot
    // base_head (current main) is no longer the source's merge_base.
    let (c, _, e) = git(&dir, &["checkout", "-q", "feature"]);
    assert_eq!(c, 0, "co feature: {e}");
    add_passing_ci_plan(&dir);
    let (c, _, e) = git(&dir, &["checkout", "-q", "main"]);
    assert_eq!(c, 0, "co main: {e}");
    std::fs::write(dir.join("diverge.txt"), "diverge\n").unwrap();
    let (c, _, e) = git(&dir, &["add", "diverge.txt"]);
    assert_eq!(c, 0, "add: {e}");
    let (c, _, e) = git(&dir, &["commit", "-q", "-m", "diverge on main"]);
    assert_eq!(c, 0, "commit: {e}");
    let (c, o, e) = forge(
        &dir,
        &[
            "forge", "pr", "create", "--source", "feature", "--base", "main", "noff PR",
        ],
    );
    assert_eq!(c, 0, "pr create failed: {e} {o}");
    ci_green(&dir);
    let (c, o, e) = forge(&dir, &["forge", "pr", "review", "1", "--approve"]);
    assert_eq!(c, 0, "approve failed: {e} {o}");
    checkout(&dir, "feature");
    let base_before = ref_oid(&dir, "refs/heads/main").unwrap();
    let head_before = ref_oid(&dir, "refs/forge/prs/1/head").unwrap();
    let (cm, _, em) = forge(&dir, &["forge", "pr", "merge", "1", "--ff"]);
    assert_ne!(cm, 0, "non-ffable PR must refuse --ff");
    assert!(
        em.contains("rebase") || em.contains("recreate"),
        "refusal must name the rebase/recreate remedy: {em}"
    );
    assert_eq!(
        ref_oid(&dir, "refs/heads/main").unwrap(),
        base_before,
        "base unchanged on ff refusal"
    );
    assert_eq!(
        ref_oid(&dir, "refs/forge/prs/1/head").unwrap(),
        head_before,
        "PR head unchanged on ff refusal"
    );
    let (_cs, os, _) = forge(&dir, &["forge", "pr", "show", "1"]);
    assert!(!os.contains("merged:"), "PR must remain unmerged: {os}");
}

#[test]
fn ff_moved_base_refuses_naming_recreate_or_rebase_and_recheck() {
    // VAL-103: after the base ref advanced past the snapshot base_head, `--ff`
    // refuses with the exact remedy phrase recreate-or-rebase-and-recheck;
    // base/PR refs unchanged.
    let dir = tmpdir("ffmoved");
    init_repo(&dir);
    create_approved_pr(&dir, "feature", "ff moved PR");
    let base_before = ref_oid(&dir, "refs/heads/main").unwrap();
    let head_before = ref_oid(&dir, "refs/forge/prs/1/head").unwrap();
    // Advance main after PR creation.
    let (c, _, e) = git(&dir, &["checkout", "-q", "main"]);
    assert_eq!(c, 0, "co main: {e}");
    std::fs::write(dir.join("later.txt"), "later\n").unwrap();
    let (c, _, e) = git(&dir, &["add", "later.txt"]);
    assert_eq!(c, 0, "add: {e}");
    let (c, _, e) = git(&dir, &["commit", "-q", "-m", "later"]);
    assert_eq!(c, 0, "commit: {e}");
    let moved_base = ref_oid(&dir, "refs/heads/main").unwrap();
    assert_ne!(base_before, moved_base);
    checkout(&dir, "feature");
    let (cm, _, em) = forge(&dir, &["forge", "pr", "merge", "1", "--ff"]);
    assert_ne!(cm, 0, "moved base must refuse --ff");
    assert!(
        em.contains("recreate-or-rebase-and-recheck"),
        "refusal must name recreate-or-rebase-and-recheck: {em}"
    );
    assert_eq!(
        ref_oid(&dir, "refs/heads/main").unwrap(),
        moved_base,
        "base unchanged"
    );
    assert_eq!(
        ref_oid(&dir, "refs/forge/prs/1/head").unwrap(),
        head_before,
        "PR head unchanged"
    );
    let (_cs, os, _) = forge(&dir, &["forge", "pr", "show", "1"]);
    assert!(!os.contains("merged:"), "PR must remain unmerged: {os}");
}

#[test]
#[allow(clippy::cognitive_complexity)]
fn ff_finalize_base_cas_clash_leaves_no_partial_write() {
    // VAL-104: exact-base CAS — the base advances between the expectation
    // check and finalize (injected in the GIT_FORGE_TEST_MERGE_BARRIER
    // window: pending result ref exists, temp worktree already gone). The
    // completion transaction's base CAS then fails → merge exits non-zero
    // with base/PR/issue refs all unchanged (no partial write).
    let dir = tmpdir("ffcas");
    init_repo(&dir);
    let (ci, _, ei) = forge(&dir, &["forge", "issue", "new", "ff cas bug"]);
    assert_eq!(ci, 0, "issue new failed: {ei}");
    create_approved_pr_closes(&dir, "feature", "ff closes PR", 1, 2);
    checkout(&dir, "feature");
    let barrier = tmpdir("ffcas-window");
    let base_before = ref_oid(&dir, "refs/heads/main").unwrap();
    let head_before = ref_oid(&dir, "refs/forge/prs/2/head").unwrap();
    let issue_before = ref_oid(&dir, "refs/forge/issues/1").unwrap();

    let bin = env!("CARGO_BIN_EXE_git-forge");
    let child: Child = Command::new(bin)
        .args(["forge", "pr", "merge", "2", "--ff"])
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
            "ff merge never reached the barrier window"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    // Inject the CAS clash: advance the base ref while the merge is parked
    // between the expectation check and the finalize transaction. The clash
    // commit is built with plumbing (commit-tree of the current base tree,
    // empty delta) so no checkout of main is needed.
    let tree_before = {
        let (_, o, _) = git(&dir, &["rev-parse", &format!("{base_before}^{{tree}}")]);
        o.trim().to_string()
    };
    let (c, o3, e3) = git(
        &dir,
        &["commit-tree", &tree_before, "-m", "concurrent base advance"],
    );
    assert_eq!(c, 0, "commit-tree failed: {e3} {o3}");
    let new_base = o3.trim().to_string();
    let (c, _, e) = git(
        &dir,
        &["update-ref", "refs/heads/main", &new_base, &base_before],
    );
    assert_eq!(c, 0, "advance base ref: {e}");
    let moved = ref_oid(&dir, "refs/heads/main").unwrap();
    assert_eq!(moved, new_base, "base ref must have moved");
    assert_ne!(moved, base_before);

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
    let output = child.wait_with_output().unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !output.status.success(),
        "the CAS-clashed ff merge must fail: {stderr}"
    );
    assert_eq!(
        ref_oid(&dir, "refs/heads/main").unwrap(),
        new_base,
        "base must hold the concurrent value (merge wrote nothing)"
    );
    assert_eq!(
        ref_oid(&dir, "refs/forge/prs/2/head").unwrap(),
        head_before,
        "PR head chain must be unchanged"
    );
    assert_eq!(
        ref_oid(&dir, "refs/forge/issues/1").unwrap(),
        issue_before,
        "issue ref must be unchanged"
    );
    let events = chain_event_jsons(&dir, "refs/forge/issues/1");
    assert!(
        events
            .iter()
            .all(|j| !j.contains("\"kind\":\"issue.close\"")),
        "no partial issue.close event may exist: {events:?}"
    );
    assert_no_temp_worktree_pr(&dir, 2);
}

// ── t2 / VAL-109..112: `pr merge --recycle-worktree` (#4 source recycle) ──

/// Register a linked worktree on `branch` in a unique temp path OUTSIDE the
/// repo dir — the real pipeline layout (devflow worktrees never live inside
/// the integration tree, where they would register as untracked dirt and
/// trip the checked-out base guard). The main worktree stays wherever it is
/// (typically the clean base branch), so the checked-out base guard and the
/// #3 refresh see a clean base while the source worktree is the recycle
/// target.
fn add_source_worktree(dir: &Path, name: &str, branch: &str) -> PathBuf {
    let wt = std::env::temp_dir().join(format!(
        "gf-t3-wt-{name}-{}-{:?}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .subsec_nanos()
    ));
    let (c, _, e) = git(
        dir,
        &["worktree", "add", "-q", wt.to_str().unwrap(), branch],
    );
    assert_eq!(c, 0, "worktree add {name} on {branch} failed: {e}");
    wt
}

#[test]
fn recycle_ff_removes_source_worktree_and_deletes_branch() {
    // VAL-109: a registered clean source worktree whose checked-out branch
    // is the PR source_ref, recycled through `pr merge --ff
    // --recycle-worktree`: exit 0, outcome lines `removed` +
    // `branch-deleted`, the worktree directory is gone and unregistered,
    // and `git rev-parse <source_ref>` fails (branch deleted). The base
    // advanced to the result commit (== the source head for --ff).
    let dir = tmpdir("recycleff");
    init_repo(&dir);
    make_feature(&dir, "feature", "feat\n");
    checkout(&dir, "feature");
    add_passing_ci_plan(&dir);
    checkout(&dir, "main");
    let wt = add_source_worktree(&dir, "wt-src", "feature");
    let wts = wt.to_str().unwrap();
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
            "recycle PR",
        ],
    );
    assert_eq!(c, 0, "pr create failed: {e} {o}");
    ci_green(&dir);
    let (c, o, e) = forge(&dir, &["forge", "pr", "review", "1", "--approve"]);
    assert_eq!(c, 0, "approve failed: {e} {o}");
    let source_before = ref_oid(&dir, "refs/heads/feature").unwrap();
    let (cm, om, em) = forge(
        &dir,
        &[
            "forge",
            "pr",
            "merge",
            "1",
            "--ff",
            "--recycle-worktree",
            wts,
        ],
    );
    assert_eq!(cm, 0, "ff merge with recycle failed: {em} {om}");
    assert!(om.contains("removed"), "outcome must contain removed: {om}");
    assert!(
        om.contains("branch-deleted"),
        "outcome must contain branch-deleted: {om}"
    );
    assert!(!wt.exists(), "worktree directory must be gone");
    let (_, list, _) = git(&dir, &["worktree", "list", "--porcelain"]);
    assert!(!list.contains(wts), "worktree must be unregistered: {list}");
    assert_eq!(
        ref_oid(&dir, "refs/heads/feature"),
        None,
        "source branch must be deleted"
    );
    // --ff result commit IS the source head; the base advanced onto it.
    assert_eq!(
        ref_oid(&dir, "refs/heads/main").unwrap(),
        source_before,
        "base tip must be the merged source head"
    );
}

/// Setup shared by the recycle tests: repo, approved green PR #1 on branch
/// `feature`, a registered source worktree on `feature`, main worktree on
/// the clean base. Returns (dir, worktree path).
fn recycle_setup(tag: &str) -> (PathBuf, PathBuf) {
    let dir = tmpdir(tag);
    init_repo(&dir);
    make_feature(&dir, "feature", "feat\n");
    checkout(&dir, "feature");
    add_passing_ci_plan(&dir);
    checkout(&dir, "main");
    let wt = add_source_worktree(&dir, "wt-src", "feature");
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
            "recycle PR",
        ],
    );
    assert_eq!(c, 0, "pr create failed: {e} {o}");
    ci_green(&dir);
    let (c, o, e) = forge(&dir, &["forge", "pr", "review", "1", "--approve"]);
    assert_eq!(c, 0, "approve failed: {e} {o}");
    (dir, wt)
}

#[test]
fn recycle_refuses_unregistered_and_wrong_branch_paths() {
    // VAL-110 (containment): an UNREGISTERED path and a worktree whose
    // checked-out branch is NOT the PR source_ref both refuse with exit
    // non-zero, outcome refused-not-contained, refs unchanged, PR unmerged,
    // and the referenced worktree untouched (the wrong-branch worktree must
    // still exist and stay registered afterwards).
    let (dir, wt) = recycle_setup("recyclenc");
    let wts = wt.to_str().unwrap();
    let base_before = ref_oid(&dir, "refs/heads/main").unwrap();
    let head_before = ref_oid(&dir, "refs/forge/prs/1/head").unwrap();
    // A registered worktree on a branch that is NOT the PR source_ref
    // (a branch cannot be checked out in two worktrees, so this is its own).
    let (_c, _o, _eb) = git(&dir, &["branch", "other", "main"]);
    let wrong = add_source_worktree(&dir, "wt-wrong", "other");
    let wrongs = wrong.to_str().unwrap();

    let (_c1, o1, e1) = forge(
        &dir,
        &[
            "forge",
            "pr",
            "merge",
            "1",
            "--ff",
            "--recycle-worktree",
            "/tmp/gf-not-registered",
        ],
    );
    let both1 = format!("{o1} {e1}");
    assert!(
        both1.contains("refused-not-contained"),
        "unregistered refusal must say refused-not-contained: {both1}"
    );

    let (c2, o2, e2) = forge(
        &dir,
        &[
            "forge",
            "pr",
            "merge",
            "1",
            "--ff",
            "--recycle-worktree",
            wrongs,
        ],
    );
    assert_ne!(c2, 0, "wrong-branch worktree must refuse");
    let both2 = format!("{o2} {e2}");
    assert!(
        both2.contains("refused-not-contained"),
        "wrong-branch refusal must say refused-not-contained: {both2}"
    );
    assert_eq!(ref_oid(&dir, "refs/heads/main").unwrap(), base_before);
    assert_eq!(
        ref_oid(&dir, "refs/forge/prs/1/head").unwrap(),
        head_before,
        "PR head must be unchanged"
    );
    let (_cs, os, _) = forge(&dir, &["forge", "pr", "show", "1"]);
    assert!(!os.contains("merged:"), "PR must remain unmerged: {os}");
    assert!(
        wt.exists() && wrong.exists(),
        "referenced worktrees must be untouched"
    );
    let (_, list, _) = git(&dir, &["worktree", "list", "--porcelain"]);
    assert!(
        list.contains(wts) && list.contains(wrongs),
        "registrations intact: {list}"
    );
}

#[test]
fn recycle_refuses_dirty_without_force_proceeds_with_force() {
    // VAL-110 (dirty): a dirty source worktree (tracked modification AND an
    // untracked file both count) refuses without --force with outcome
    // refused-dirty, worktree and branch intact, merge refused; with
    // --force the merge + recycle proceed.
    let (dir, wt) = recycle_setup("recycledirty");
    let wts = wt.to_str().unwrap();
    std::fs::write(wt.join("feature.txt"), "dirty\n").unwrap();
    let base_before = ref_oid(&dir, "refs/heads/main").unwrap();
    let head_before = ref_oid(&dir, "refs/forge/prs/1/head").unwrap();
    let (_cd, od, ed) = forge(
        &dir,
        &[
            "forge",
            "pr",
            "merge",
            "1",
            "--ff",
            "--recycle-worktree",
            wts,
        ],
    );
    let bothd = format!("{od} {ed}");
    assert!(
        bothd.contains("refused-dirty"),
        "dirty refusal must say refused-dirty: {bothd}"
    );
    assert!(wt.exists(), "refused-dirty keeps the worktree");
    assert!(
        ref_oid(&dir, "refs/heads/feature").is_some(),
        "refused-dirty keeps the branch"
    );
    assert_eq!(ref_oid(&dir, "refs/heads/main").unwrap(), base_before);
    assert_eq!(ref_oid(&dir, "refs/forge/prs/1/head").unwrap(), head_before);

    let (cf, of, ef) = forge(
        &dir,
        &[
            "forge",
            "pr",
            "merge",
            "1",
            "--ff",
            "--force",
            "--recycle-worktree",
            wts,
        ],
    );
    assert_eq!(cf, 0, "--force must proceed: {ef} {of}");
    assert!(of.contains("removed"), "--force recycle must remove: {of}");
    assert!(!wt.exists(), "--force recycle removes the worktree");
    assert!(
        ref_oid(&dir, "refs/heads/feature").is_none(),
        "branch deleted after --force recycle"
    );
}

#[test]
fn recycle_branch_deletion_follows_strategy_ancestry() {
    // VAL-111: branch deletion ONLY when the source tip is an ancestor of
    // the integrated base tip. --ff and --merge delete the branch (their
    // result commit contains the source tip); --squash and --rebase KEEP it
    // and report branch-kept(<reason>) naming why (squash/rebase synthesize
    // a NEW commit; the original branch tip is not an ancestor of the
    // result).
    for (strategy, want_deleted) in [
        ("--ff", true),
        ("--merge", true),
        ("--squash", false),
        ("--rebase", false),
    ] {
        let dir = tmpdir("recyclestrat");
        init_repo(&dir);
        make_feature(&dir, "feature", "feat\n");
        checkout(&dir, "feature");
        add_passing_ci_plan(&dir);
        checkout(&dir, "main");
        let wt = add_source_worktree(&dir, "wt-src", "feature");
        let wts = wt.to_str().unwrap();
        let (c, o, e) = forge(
            &dir,
            &[
                "forge", "pr", "create", "--source", "feature", "--base", "main", "strat PR",
            ],
        );
        assert_eq!(c, 0, "pr create failed: {e} {o}");
        ci_green(&dir);
        let (c, o, e) = forge(&dir, &["forge", "pr", "review", "1", "--approve"]);
        assert_eq!(c, 0, "approve failed: {e} {o}");
        let (cm, om, em) = forge(
            &dir,
            &[
                "forge",
                "pr",
                "merge",
                "1",
                strategy,
                "--recycle-worktree",
                wts,
            ],
        );
        assert_eq!(cm, 0, "{strategy} merge with recycle failed: {em} {om}");
        assert!(
            om.contains("removed"),
            "{strategy}: worktree must be removed: {om}"
        );
        assert!(!wt.exists(), "{strategy}: worktree dir must be gone");
        if want_deleted {
            assert!(
                om.contains("branch-deleted"),
                "{strategy}: outcome must contain branch-deleted: {om}"
            );
            assert_eq!(
                ref_oid(&dir, "refs/heads/feature"),
                None,
                "{strategy}: branch must be deleted"
            );
        } else {
            let both = om.clone();
            assert!(
                both.contains("branch-kept("),
                "{strategy}: outcome must contain branch-kept(<reason>): {both}"
            );
            assert!(
                ref_oid(&dir, "refs/heads/feature").is_some(),
                "{strategy}: branch must be kept"
            );
        }
    }
}

#[test]
fn recycle_partial_leftover_on_removal_failure_base_untouched() {
    // VAL-112: partial-removal injection — a locked source worktree makes
    // `git worktree remove --force` fail deterministically, so the recycle
    // reports partial-leftover(<path>) NAMING THE PATH, the worktree stays
    // registered, and the merge itself is intact (the PR is merged, the
    // branch kept). The base worktree is NEVER touched by the recycle:
    // after a merge with recycle, the base worktree is clean and its HEAD
    // is the result commit (the #3 refresh, which recycle runs after).
    let (dir, wt) = recycle_setup("recyclepl");
    let wts = wt.to_str().unwrap();
    let source_before = ref_oid(&dir, "refs/heads/feature").unwrap();
    // Inject the partial-removal: lock the source worktree (debug seam) so
    // the recycle's `git worktree remove --force` fails.
    let (_cm, om, em) = run_forge(
        &dir,
        None,
        &[("GIT_FORGE_TEST_FAIL_RECYCLE_REMOVE", wts)],
        &[
            "forge",
            "pr",
            "merge",
            "1",
            "--ff",
            "--recycle-worktree",
            wts,
        ],
    );

    let both = format!("{om} {em}");
    assert!(
        both.contains(&format!("partial-leftover({wts})")),
        "outcome must name partial-leftover(<path>) with the exact path: {both}"
    );
    // CR-01 (code review): a recycle epilogue failure does NOT undo the merge —
    // the PR is terminal — so the error output must still carry the merged line
    // (the adapter's contract: the merged line always precedes the recycle
    // outcome). A bare `recycle outcome: partial-leftover(...)` would send a
    // terminal merge into the adapter's bridge-failure catch-all.
    assert!(
        both.contains("merged PR #1 into main"),
        "the merged line must survive the recycle failure: {both}"
    );
    // The merge itself is intact: PR merged onto the source head.
    assert_eq!(
        ref_oid(&dir, "refs/heads/main").unwrap(),
        source_before,
        "base advanced to the merged source head"
    );
    // The worktree stays registered (partial removal).
    assert!(wt.exists(), "leftover worktree directory stays");
    let (_, list, _) = git(&dir, &["worktree", "list", "--porcelain"]);
    assert!(
        list.contains(wts),
        "leftover worktree stays registered: {list}"
    );
    // Base worktree NEVER touched by the recycle: clean, HEAD == result.
    let (_, status, _) = git(&dir, &["status", "--porcelain"]);
    assert_eq!(status, "", "base worktree must be clean: {status}");
    let (_, head, _) = git(&dir, &["rev-parse", "HEAD"]);
    assert_eq!(
        head, source_before,
        "base worktree HEAD must be the result commit"
    );
}

/// Phase-2 TOCTOU injection: a shim that appends real dirt to the source
/// worktree on the SECOND `git -C <target> status --porcelain` call. Call #1
/// is the phase-1 pre-merge validation (sees the clean tree); call #2 is the
/// phase-2 removal re-check (F-003) — the dirt appears exactly in the
/// validate→remove window the re-check exists to close.
fn make_dirty_on_second_status_shim() -> PathBuf {
    make_git_shim(
        "p2dirty-shim",
        "if [ \"$1\" = \"-C\" ] && [ \"$2\" = \"$GF_TARGET\" ] && \
         [ \"$3\" = \"status\" ] && [ \"$4\" = \"--porcelain\" ]; then\n\
         n=$(cat \"$GF_ST_COUNT\" 2>/dev/null || echo 0)\n\
         n=$((n+1))\n\
         echo \"$n\" > \"$GF_ST_COUNT\"\n\
         if [ \"$n\" -ge 2 ]; then\n\
         echo dirt >> \"$GF_TARGET/feature.txt\"\n\
         fi\n\
         fi",
    )
}

#[test]
fn recycle_phase2_dirty_refuses_without_force() {
    // F-003: dirt appearing between the phase-1 validation and the phase-2
    // removal must NOT be silently destroyed by the unconditional
    // `worktree remove --force`. The phase-2 re-check refuses with
    // refused-dirty, the worktree and its branch stay intact with the dirt
    // preserved, and the already-terminal merge is not undone.
    let (dir, wt) = recycle_setup("recyclep2dirty");
    let wts = wt.to_str().unwrap();
    let source_before = ref_oid(&dir, "refs/heads/feature").unwrap();
    let shim = make_dirty_on_second_status_shim();
    let counter = shim.join("count");
    std::fs::write(&counter, "0").unwrap();
    let (cm, om, em) = run_forge(
        &dir,
        Some(&shim),
        &[
            ("GF_TARGET", wts),
            ("GF_ST_COUNT", counter.to_str().unwrap()),
        ],
        &[
            "forge",
            "pr",
            "merge",
            "1",
            "--ff",
            "--recycle-worktree",
            wts,
        ],
    );
    assert_ne!(cm, 0, "phase-2 dirty recycle must exit non-zero: {om} {em}");
    let both = format!("{om} {em}");
    assert!(
        both.contains("refused-dirty") && both.contains(wts),
        "outcome must be refused-dirty naming the worktree path: {both}"
    );
    // CR-01: phase-2 refused-dirty is a TERMINAL merge with a recycle refusal —
    // the error must carry the merged line so the adapter classifies it as
    // worktree_partial_removal, never as "PR NOT merged" (the phase-1 class).
    assert!(
        both.contains("merged PR #1 into main"),
        "the merged line must survive the phase-2 refusal: {both}"
    );
    // The merge itself is terminal (recycle refusal never undoes it).
    assert_eq!(
        ref_oid(&dir, "refs/heads/main").unwrap(),
        source_before,
        "base advanced to the merged source head"
    );
    // The worktree survives, still registered, with the uncommitted dirt.
    assert!(wt.exists(), "refused recycle leaves the worktree in place");
    let (_, list, _) = git(&dir, &["worktree", "list", "--porcelain"]);
    assert!(list.contains(wts), "worktree stays registered: {list}");
    let content = std::fs::read_to_string(wt.join("feature.txt")).unwrap();
    assert!(
        content.contains("dirt"),
        "the uncommitted changes must survive the refusal: {content:?}"
    );
    assert!(
        ref_oid(&dir, "refs/heads/feature").is_some(),
        "the source branch is kept"
    );
}

#[test]
fn recycle_phase2_dirty_force_proceeds() {
    // F-003 companion: with --force the phase-2 re-check does not refuse and
    // the recycle removes the (now dirty) worktree and deletes the branch.
    let (dir, wt) = recycle_setup("recyclep2force");
    let wts = wt.to_str().unwrap();
    let shim = make_dirty_on_second_status_shim();
    let counter = shim.join("count");
    std::fs::write(&counter, "0").unwrap();
    let (cm, om, em) = run_forge(
        &dir,
        Some(&shim),
        &[
            ("GF_TARGET", wts),
            ("GF_ST_COUNT", counter.to_str().unwrap()),
        ],
        &[
            "forge",
            "pr",
            "merge",
            "1",
            "--ff",
            "--force",
            "--recycle-worktree",
            wts,
        ],
    );
    assert_eq!(cm, 0, "--force must unlock the phase-2 re-check: {em} {om}");
    assert!(om.contains("removed"), "--force recycle must remove: {om}");
    assert!(
        om.contains("branch-deleted"),
        "ff recycle deletes the merged branch: {om}"
    );
    assert!(!wt.exists(), "--force recycle removes the dirty worktree");
    let (_, list, _) = git(&dir, &["worktree", "list", "--porcelain"]);
    assert!(!list.contains(wts), "worktree unregistered: {list}");
    assert!(
        ref_oid(&dir, "refs/heads/feature").is_none(),
        "branch deleted after --force recycle"
    );
}

// ── #1 per-generation snapshots (t1, VAL-205/206) ──────────────────────────

/// Append a commit to the PR's source branch (the fix-round shape), then
/// re-pin with `pr update`. Must be called while the source branch is
/// checked out; returns the new branch tip.
fn fix_round_commit(dir: &Path, branch: &str, file: &str, content: &str) -> String {
    checkout(dir, branch);
    std::fs::write(dir.join(file), content).unwrap();
    let (c, _, e) = git(dir, &["add", file]);
    assert_eq!(c, 0, "add failed: {e}");
    let (c, _, e) = git(dir, &["commit", "-q", "-m", "fix round"]);
    assert_eq!(c, 0, "commit failed: {e}");
    ref_oid(dir, &format!("refs/heads/{branch}")).unwrap()
}

/// Read the `.forge/event.json` payload of the commit a PR ref points at.
fn head_event_json(dir: &Path, pr: u64) -> String {
    let r = format!("refs/forge/prs/{pr}/head");
    let (c, o, e) = git(dir, &["show", &format!("{r}:.forge/event.json")]);
    assert_eq!(c, 0, "read {r} event failed: {e}");
    o
}

/// The `pr.update` event body must record old/new source_head, base_head,
/// merge_base (the ticket's 4 field groups, 6 OIDs) and the generation.
fn assert_update_event_body(ev: &str, old_source: &str, new_source: &str, base: &str) {
    assert!(ev.contains("\"kind\":\"pr.update\""), "event kind: {ev}");
    for (field, oid) in [
        ("old_source_head", old_source),
        ("new_source_head", new_source),
        ("old_base_head", base),
        ("new_base_head", base),
        ("old_merge_base", base),
        ("new_merge_base", base),
    ] {
        assert!(
            ev.contains(&format!("\"{field}\":\"{oid}\"")),
            "{field}: {ev}"
        );
    }
}

/// VAL-205: `pr update <n>` re-pins the PR to the source branch tip as a NEW
/// immutable generation; the event body records old/new source_head,
/// base_head, merge_base; generation-0 refs stay untouched; `pr show` exposes
/// the latest generation and its snapshot; `pr diff` shows the new content.
#[test]
fn pr_update_re_pins_snapshot_as_new_generation() {
    let dir = tmpdir("prupdate");
    init_repo(&dir);
    create_approved_pr(&dir, "feature", "update PR");
    let gen0_source = ref_oid(&dir, "refs/forge/prs/1/source").unwrap();
    let gen0_base = ref_oid(&dir, "refs/forge/prs/1/base").unwrap();

    let new_tip = fix_round_commit(&dir, "feature", "feature.txt", "feat\nfix\n");
    checkout(&dir, "main");

    let (c, o, e) = forge(&dir, &["forge", "pr", "update", "1"]);
    assert_eq!(c, 0, "pr update failed: {e} {o}");
    assert!(
        o.contains("generation 1"),
        "update output names the generation: {o}"
    );

    // Generation-addressable ref pins the branch tip; the generation-0 L1
    // snapshot ref stays exactly where pr.created pinned it.
    assert_eq!(
        ref_oid(&dir, "refs/forge/prs/1/gen/1/source").as_deref(),
        Some(new_tip.as_str()),
        "generation-1 ref must pin the new branch tip"
    );
    assert_eq!(
        ref_oid(&dir, "refs/forge/prs/1/source").as_deref(),
        Some(gen0_source.as_str()),
        "generation-0 /source ref must stay immutable"
    );
    assert_eq!(
        ref_oid(&dir, "refs/forge/prs/1/base").as_deref(),
        Some(gen0_base.as_str()),
        "generation-0 /base ref must stay immutable"
    );

    // pr.show exposes the latest generation and the new snapshot source.
    let (_, o, _) = forge(&dir, &["forge", "pr", "show", "1"]);
    assert!(
        o.contains("generation: 1"),
        "show must expose the generation: {o}"
    );
    assert!(
        o.contains(&new_tip),
        "show must expose the new source_head: {o}"
    );

    // The pr.update event body records old/new source_head, base_head,
    // merge_base (the 4 contract field groups, 6 OIDs).
    let ev = head_event_json(&dir, 1);
    assert_update_event_body(&ev, &gen0_source, &new_tip, &gen0_base);

    // diff = merge_base..latest source_head: the fix-round change shows.
    let (c, o, e) = forge(&dir, &["forge", "pr", "diff", "1"]);
    assert_eq!(c, 0, "pr diff failed: {e}");
    assert!(o.contains("fix"), "diff must show the appended fix: {o}");
}

/// Each update is a new generation: after two updates both generation refs
/// exist with their own pinned tips, the first stays immutable, and `pr
/// show` exposes generation 2.
#[test]
fn pr_update_twice_pins_each_generation() {
    let dir = tmpdir("prupdate2");
    init_repo(&dir);
    create_approved_pr(&dir, "feature", "update twice PR");
    let g1_tip = fix_round_commit(&dir, "feature", "feature.txt", "feat\nfix1\n");
    checkout(&dir, "main");
    let (c, _, e) = forge(&dir, &["forge", "pr", "update", "1"]);
    assert_eq!(c, 0, "first update failed: {e}");

    let g2_tip = fix_round_commit(&dir, "feature", "feature.txt", "feat\nfix2\n");
    checkout(&dir, "main");
    let (c, o, e) = forge(&dir, &["forge", "pr", "update", "1"]);
    assert_eq!(c, 0, "second update failed: {e}");
    assert!(o.contains("generation 2"), "second update output: {o}");
    assert_eq!(
        ref_oid(&dir, "refs/forge/prs/1/gen/1/source").as_deref(),
        Some(g1_tip.as_str()),
        "generation-1 must keep its pinned tip"
    );
    assert_eq!(
        ref_oid(&dir, "refs/forge/prs/1/gen/2/source").as_deref(),
        Some(g2_tip.as_str()),
        "generation-2 must pin the latest tip"
    );
    let (_, o, _) = forge(&dir, &["forge", "pr", "show", "1"]);
    assert!(
        o.contains("generation: 2"),
        "show must expose generation 2: {o}"
    );
    assert!(
        o.contains(&g2_tip),
        "show must expose the latest snapshot: {o}"
    );
    assert!(
        !o.contains(&g1_tip),
        "show must not expose a stale snapshot: {o}"
    );
}

/// `pr update` refusals: unknown PR, already-at-branch-tip (nothing to
/// update), abandoned PR (terminal).
#[test]
fn pr_update_refusals() {
    let dir = tmpdir("prupdateref");
    init_repo(&dir);
    create_approved_pr(&dir, "feature", "update refusals PR");

    let (c, _, e) = forge(&dir, &["forge", "pr", "update", "99"]);
    assert_ne!(c, 0, "unknown PR must refuse");
    assert!(e.contains("PR #99 does not exist"), "unknown PR error: {e}");

    // No new commits on the branch: nothing to update.
    let (c, o, e) = forge(&dir, &["forge", "pr", "update", "1"]);
    assert_ne!(c, 0, "no-op update must refuse: {o} {e}");
    assert!(e.contains("nothing to update"), "no-op error: {e}");

    // Terminal states refuse.
    let (c, _, e) = forge(&dir, &["forge", "pr", "abandon", "1", "--reason", "gone"]);
    assert_eq!(c, 0, "abandon failed: {e}");
    let (c, _, e) = forge(&dir, &["forge", "pr", "update", "1"]);
    assert_ne!(c, 0, "abandoned PR update must refuse");
    assert!(e.contains("gone"), "abandon refusal names the reason: {e}");
}

/// VAL-206: an approve recorded BEFORE `pr update` cannot merge post-update
/// content — the merge refuses with refs unchanged; after a fresh approve and
/// a fresh CI Check on the latest generation, the merge succeeds and the base
/// contains the post-update commit.
#[test]
fn approve_before_update_cannot_merge_post_update_content() {
    let dir = tmpdir("prupdatestale");
    init_repo(&dir);
    create_approved_pr(&dir, "feature", "stale approve PR");
    let base_before = ref_oid(&dir, "main").unwrap();

    let new_tip = fix_round_commit(&dir, "feature", "feature.txt", "feat\nfix\n");
    checkout(&dir, "main");
    let (c, _, e) = forge(&dir, &["forge", "pr", "update", "1"]);
    assert_eq!(c, 0, "pr update failed: {e}");

    checkout(&dir, "feature");
    let (c, _, e) = forge(&dir, &["forge", "pr", "merge", "1"]);
    assert_ne!(
        c, 0,
        "pre-update approve must not merge post-update content: {e}"
    );
    assert!(
        e.contains("approve is stale"),
        "refusal must name the stale gate input: {e}"
    );
    assert_eq!(
        ref_oid(&dir, "main").as_deref(),
        Some(base_before.as_str()),
        "a refused merge must leave the base untouched"
    );
    let (_, o, _) = forge(&dir, &["forge", "pr", "show", "1"]);
    assert!(!o.contains("merged:"), "PR must stay unmerged: {o}");

    // Fresh approve + fresh CI on the latest generation merge.
    let (c, _, e) = forge(&dir, &["forge", "ci", "run", "1"]);
    assert_eq!(c, 0, "post-update ci run failed: {e}");
    let (c, _, e) = forge(&dir, &["forge", "pr", "review", "1", "--approve"]);
    assert_eq!(c, 0, "post-update approve failed: {e}");
    let (c, o, e) = forge(&dir, &["forge", "pr", "merge", "1"]);
    assert_eq!(c, 0, "fresh approve + fresh CI must merge: {e} {o}");
    assert!(
        git(&dir, &["merge-base", "--is-ancestor", &new_tip, "main"]).0 == 0,
        "the merged base must contain the post-update commit"
    );
}

/// VAL-206 (CI leg): a green CI Check recorded BEFORE `pr update` is stale —
/// a fresh approve alone must not satisfy the gate; the merge refuses naming
/// the missing Check, and a fresh CI run on the latest generation completes
/// the merge.
#[test]
fn ci_green_before_update_cannot_merge_post_update_content() {
    let dir = tmpdir("prupdatestaleci");
    init_repo(&dir);
    create_approved_pr(&dir, "feature", "stale ci PR");

    fix_round_commit(&dir, "feature", "feature.txt", "feat\nfix\n");
    checkout(&dir, "main");
    let (c, _, e) = forge(&dir, &["forge", "pr", "update", "1"]);
    assert_eq!(c, 0, "pr update failed: {e}");

    checkout(&dir, "feature");
    // Fresh approve is bound to the latest generation, but the only CI Check
    // on the chain is the pre-update green one — stale, so the gate refuses.
    let (c, _, e) = forge(&dir, &["forge", "pr", "review", "1", "--approve"]);
    assert_eq!(c, 0, "fresh approve failed: {e}");
    let (c, _, e) = forge(&dir, &["forge", "pr", "merge", "1"]);
    assert_ne!(c, 0, "pre-update green CI must not satisfy the gate: {e}");
    assert!(
        e.contains("CI Check is stale"),
        "refusal must name the stale Check: {e}"
    );

    // Fresh CI on the latest generation completes the merge.
    let (c, _, e) = forge(&dir, &["forge", "ci", "run", "1"]);
    assert_eq!(c, 0, "post-update ci run failed: {e}");
    let (c, _, e) = forge(&dir, &["forge", "pr", "merge", "1"]);
    assert_eq!(c, 0, "fresh CI must complete the merge: {e}");
}
