//! t2 PR integration tests. Run the built `git-forge` binary as a subprocess
//! inside an isolated temp git repository.

use std::path::{Path, PathBuf};
use std::process::Command;

fn tmpdir(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!(
        "gf-t2-{tag}-{}-{:?}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .subsec_nanos()
    ));
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn git(dir: &PathBuf, args: &[&str]) -> (i32, String, String) {
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

fn init_repo(dir: &PathBuf) {
    let (c, _, e) = git(dir, &["init", "-q"]);
    assert!(c == 0, "git init failed: {e}");
    std::fs::write(dir.join("base.txt"), "base\n").unwrap();
    let (c, _, e) = git(dir, &["add", "base.txt"]);
    assert!(c == 0, "git add failed: {e}");
    let (c, _, e) = git(dir, &["commit", "-q", "-m", "base commit"]);
    assert!(c == 0, "base commit failed: {e}");
    let (c, _, e) = git(dir, &["branch", "-M", "main"]);
    assert!(c == 0, "branch -M failed: {e}");
}

fn forge(dir: &PathBuf, args: &[&str]) -> (i32, String, String) {
    forge_with_env(dir, args, &[])
}

fn forge_with_env(dir: &PathBuf, args: &[&str], envs: &[(&str, &str)]) -> (i32, String, String) {
    let bin = env!("CARGO_BIN_EXE_git-forge");
    let mut cmd = Command::new(bin);
    cmd.args(args).current_dir(dir);
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

/// Make a feature branch off main with one commit.
fn make_feature(dir: &PathBuf, branch: &str, content: &str) {
    git(dir, &["checkout", "-q", "-b", branch]);
    std::fs::write(dir.join("feature.txt"), content).unwrap();
    git(dir, &["add", "feature.txt"]);
    git(dir, &["commit", "-q", "-m", &format!("{branch} commit")]);
    git(dir, &["checkout", "-q", "main"]);
}

fn ref_oid(dir: &PathBuf, r: &str) -> Option<String> {
    let (c, o, _) = git(dir, &["rev-parse", "--verify", r]);
    if c == 0 {
        Some(o)
    } else {
        None
    }
}

#[test]
#[allow(clippy::cognitive_complexity)]
fn pr_create_show_snapshot() {
    let dir = tmpdir("create");
    init_repo(&dir);
    make_feature(&dir, "feature", "feat\n");
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
            "Add feature",
        ],
    );
    assert_eq!(c, 0, "pr create failed: {e}");
    assert!(o.contains("PR #1"), "create output: {o}");

    // Snapshot refs pinned atomically.
    assert!(ref_oid(&dir, "refs/forge/prs/1/head").is_some());
    assert!(ref_oid(&dir, "refs/forge/prs/1/meta").is_some());
    let head_oid = ref_oid(&dir, "refs/forge/prs/1/head").unwrap();
    let meta_oid = ref_oid(&dir, "refs/forge/prs/1/meta").unwrap();
    // `meta` stays pinned at the pr.created snapshot; `pr create` appends the
    // pending ci.check on the head chain, so head advances one commit past it.
    assert_ne!(
        head_oid, meta_oid,
        "head must advance past the immutable pr.created snapshot (pending ci.check)"
    );
    let src = ref_oid(&dir, "refs/forge/prs/1/source").unwrap();
    let base = ref_oid(&dir, "refs/forge/prs/1/base").unwrap();
    assert_eq!(src, git(&dir, &["rev-parse", "refs/heads/feature"]).1);
    assert_eq!(base, git(&dir, &["rev-parse", "refs/heads/main"]).1);

    let cs = forge(&dir, &["forge", "pr", "show", "1"]);
    assert_eq!(cs.0, 0, "pr show failed: {}", cs.2);
    assert!(cs.1.contains("Add feature"), "show output: {}", cs.1);
    assert!(cs.1.contains("feature"), "show source: {}", cs.1);
    assert!(cs.1.contains("main"), "show base: {}", cs.1);
    assert!(cs.1.contains("no review yet"), "show decision: {}", cs.1);

    // The meta commit carries the real pr.created event (not a bare genesis
    // root): `.forge/event.json` is present and encodes kind=pr.created + title.
    let (ct, ot, et) = git(&dir, &["show", &format!("{meta_oid}:.forge/event.json")]);
    assert_eq!(ct, 0, "meta commit must carry .forge/event.json: {et}");
    assert!(
        ot.contains("\"pr.created\""),
        "event kind must be pr.created: {ot}"
    );
    assert!(
        ot.contains("Add feature"),
        "event body must carry the PR title: {ot}"
    );

    // The head chain tip is the pending ci.check marker appended by `pr create`.
    let (ht, hott, het) = git(&dir, &["show", &format!("{head_oid}:.forge/event.json")]);
    assert_eq!(ht, 0, "head commit must carry .forge/event.json: {het}");
    assert!(hott.contains("\"ci.check\""), "head event kind: {hott}");
    assert!(
        hott.contains("\"status\":\"pending\""),
        "head ci status: {hott}"
    );
}

#[test]
fn pr_create_collision_leaves_no_partial_state() {
    let dir = tmpdir("collision");
    init_repo(&dir);
    make_feature(&dir, "feature", "feat\n");
    let src_oid = git(&dir, &["rev-parse", "refs/heads/feature"]).1;
    // Pre-create the source ref PR #1 would target → forced collision.
    let (cu, _, eu) = git(&dir, &["update-ref", "refs/forge/prs/1/source", &src_oid]);
    assert_eq!(cu, 0, "pre-create update-ref failed: {eu}");

    let (c, _, e) = forge(
        &dir,
        &[
            "forge", "pr", "create", "--source", "feature", "--base", "main", "T",
        ],
    );
    assert_ne!(c, 0, "pr create must fail on ref collision");
    assert!(
        e.contains("already exists") || e.contains("exist"),
        "stderr: {e}"
    );

    // Counter untouched; head/meta/base absent; pre-existing source unchanged.
    assert!(
        ref_oid(&dir, "refs/forge/meta/counter").is_none(),
        "counter must not be created on a failed create"
    );
    assert!(ref_oid(&dir, "refs/forge/prs/1/head").is_none());
    assert!(ref_oid(&dir, "refs/forge/prs/1/meta").is_none());
    assert!(ref_oid(&dir, "refs/forge/prs/1/base").is_none());
    assert_eq!(
        ref_oid(&dir, "refs/forge/prs/1/source").unwrap(),
        src_oid,
        "pre-existing source ref must be untouched"
    );
}

#[test]
fn pr_create_requires_flags_and_title() {
    let dir = tmpdir("require");
    init_repo(&dir);
    make_feature(&dir, "feature", "feat\n");
    // missing --base
    let (c1, _, e1) = forge(&dir, &["forge", "pr", "create", "--source", "feature", "T"]);
    assert_ne!(c1, 0, "missing base must error");
    assert!(e1.contains("--base"), "stderr: {e1}");
    // missing --source
    let (c2, _, e2) = forge(&dir, &["forge", "pr", "create", "--base", "main", "T"]);
    assert_ne!(c2, 0);
    assert!(e2.contains("--source"), "stderr: {e2}");
    // empty title
    let (c3, _, e3) = forge(
        &dir,
        &[
            "forge", "pr", "create", "--source", "feature", "--base", "main", "   ",
        ],
    );
    assert_ne!(c3, 0);
    assert!(e3.contains("title"), "stderr: {e3}");
    // no PR refs created by any failure
    assert!(ref_oid(&dir, "refs/forge/prs/1/head").is_none());
}

/// `pr create --label` must match issue semantics: whitespace-only labels are
/// rejected, and surrounding whitespace is trimmed before persistence. `--body`
/// is likewise trimmed (matching the trimmed issue description).
#[test]
fn pr_create_label_trimmed_and_body_trimmed() {
    let dir = tmpdir("labeltrim");
    init_repo(&dir);
    make_feature(&dir, "feature", "feat\n");

    // whitespace-only label must be rejected (no PR created).
    let (c1, _, e1) = forge(
        &dir,
        &[
            "forge", "pr", "create", "--source", "feature", "--base", "main", "T", "--label", "   ",
        ],
    );
    assert_ne!(c1, 0, "whitespace-only label must error; stdout={e1}");
    assert!(e1.contains("label"), "stderr: {e1}");
    assert!(ref_oid(&dir, "refs/forge/prs/1/head").is_none());

    // a padded label is trimmed; a padded body is trimmed.
    let (c2, _, e2) = forge(
        &dir,
        &[
            "forge",
            "pr",
            "create",
            "--source",
            "feature",
            "--base",
            "main",
            "T",
            "--label",
            "  bug  ",
            "--body",
            "  hello world  ",
        ],
    );
    assert_eq!(c2, 0, "padded label/body must create: {e2}");
    let show2 = forge(&dir, &["forge", "pr", "show", "1"]);
    assert_eq!(show2.0, 0, "pr show failed: {}", show2.2);
    assert!(
        show2.1.contains("labels: bug"),
        "label must be trimmed: {}",
        show2.1
    );
    assert!(
        show2.1.contains("hello world"),
        "body must be trimmed: {}",
        show2.1
    );
}

/// `--` marks end-of-options, so a title that exactly equals a reserved flag
/// name is representable on both surfaces.
#[test]
fn end_of_options_lets_flag_names_be_titles() {
    // issue surface (fresh repo, issue always #1)
    let dir = tmpdir("endo-issue");
    init_repo(&dir);
    let (ci, _, ei) = forge(&dir, &["forge", "issue", "new", "--", "--label"]);
    assert_eq!(ci, 0, "issue create failed: {ei}");
    let s1 = forge(&dir, &["forge", "issue", "show", "1"]);
    assert_eq!(s1.0, 0, "issue show failed: {}", s1.2);
    assert!(
        s1.1.contains("--label"),
        "title should be --label: {}",
        s1.1
    );

    // pr surface (fresh repo, PR always #1)
    let dir2 = tmpdir("endo-pr");
    init_repo(&dir2);
    make_feature(&dir2, "feature", "feat\n");
    let (cp, _, ep) = forge(
        &dir2,
        &[
            "forge", "pr", "create", "--source", "feature", "--base", "main", "--", "--label",
        ],
    );
    assert_eq!(cp, 0, "pr create failed: {ep}");
    let s2 = forge(&dir2, &["forge", "pr", "show", "1"]);
    assert_eq!(s2.0, 0, "pr show failed: {}", s2.2);
    assert!(
        s2.1.contains("--label"),
        "PR title should be --label: {}",
        s2.1
    );
}

/// A third positional (`<title> [description] extra`) is rejected, not silently
/// discarded.
#[test]
fn issue_new_rejects_excess_positionals() {
    let dir = tmpdir("excess");
    init_repo(&dir);
    let (c, o, e) = forge(&dir, &["forge", "issue", "new", "T", "desc", "extra"]);
    assert_ne!(c, 0, "excess positional must error; stdout={o}");
    assert!(e.contains("too many positional"), "stderr: {e}");
    // No issue ref is created by the rejection.
    assert!(ref_oid(&dir, "refs/forge/issues/1").is_none());
}

/// Stored control characters (e.g. a label containing newline/ESC) are escaped
/// on render, so `show` cannot forge output or emit a terminal action.
#[test]
fn issue_show_escapes_control_chars_in_label() {
    let dir = tmpdir("ctrllabel");
    init_repo(&dir);
    // A label containing a backspace and an ESC sequence is stored (issue label
    // validation only rejects empty) but must render escaped.
    let (c, _, e) = forge(
        &dir,
        &[
            "forge",
            "issue",
            "new",
            "T",
            "--label",
            "a\u{8}b\u{1b}[31mred",
        ],
    );
    assert_eq!(c, 0, "issue create failed: {e}");
    let s = forge(&dir, &["forge", "issue", "show", "1"]);
    assert_eq!(s.0, 0, "issue show failed: {}", s.2);
    assert!(
        !s.1.contains('\u{8}'),
        "bare backspace must be escaped: {}",
        s.1
    );
    assert!(!s.1.contains('\u{1b}'), "bare ESC must be escaped: {}", s.1);
    assert!(
        s.1.contains("\\x08") && s.1.contains("\\x1b"),
        "escaped forms present: {}",
        s.1
    );
}

#[test]
fn pr_create_rejects_non_local_refs() {
    let dir = tmpdir("nonlocal");
    init_repo(&dir);
    make_feature(&dir, "feature", "feat\n");
    // tag
    git(&dir, &["tag", "v1"]);
    let (c1, _, e1) = forge(
        &dir,
        &[
            "forge", "pr", "create", "--source", "v1", "--base", "main", "T",
        ],
    );
    assert_ne!(c1, 0, "tag source must error");
    assert!(e1.contains("branch"), "stderr: {e1}");
    // remote-tracking ref
    git(
        &dir,
        &[
            "update-ref",
            "refs/remotes/origin/feature",
            "refs/heads/feature",
        ],
    );
    let (c2, _, e2) = forge(
        &dir,
        &[
            "forge",
            "pr",
            "create",
            "--source",
            "origin/feature",
            "--base",
            "main",
            "T",
        ],
    );
    assert_ne!(c2, 0, "remote-tracking source must error");
    assert!(
        e2.contains("branch") || e2.contains("local"),
        "stderr: {e2}"
    );
    // OID
    let oid = git(&dir, &["rev-parse", "refs/heads/feature"]).1;
    let (c3, _, e3) = forge(
        &dir,
        &[
            "forge", "pr", "create", "--source", &oid, "--base", "main", "T",
        ],
    );
    assert_ne!(c3, 0, "OID source must error");
    assert!(e3.contains("branch") || e3.contains("OID"), "stderr: {e3}");
    assert!(ref_oid(&dir, "refs/forge/prs/1/head").is_none());
}

#[test]
fn pr_create_accepts_slashed_local_branch() {
    let dir = tmpdir("slashbranch");
    init_repo(&dir);
    config_identity(&dir);
    // Hierarchical local branch name with a slash (refs/heads/feat/thing).
    // The PR is merged later, so it carries a passing CI plan.
    make_feature_with_ci(&dir, "feat/thing", "feat\n", 0);
    let (c, o, e) = forge(
        &dir,
        &[
            "forge",
            "pr",
            "create",
            "--source",
            "feat/thing",
            "--base",
            "main",
            "T",
        ],
    );
    assert_eq!(c, 0, "slashed local branch must be accepted: {e} {o}");
    let (cs, os, _) = forge(&dir, &["forge", "pr", "show", "1"]);
    assert_eq!(cs, 0);
    assert!(os.contains("feat/thing"), "source_ref shown: {os}");
    // A slashed name that is NOT a local branch is still rejected.
    let (c2, _, e2) = forge(
        &dir,
        &[
            "forge",
            "pr",
            "create",
            "--source",
            "nope/thing",
            "--base",
            "main",
            "T2",
        ],
    );
    assert_ne!(c2, 0, "nonexistent slashed branch must error: {e2}");
    assert!(
        e2.contains("branch") || e2.contains("local"),
        "stderr: {e2}"
    );
    assert!(ref_oid(&dir, "refs/forge/prs/2/head").is_none());

    // Full-form refs/heads/... must canonicalize to the bare name for BOTH
    // storage and the merge path (merge builds refs/heads/{base_ref}).
    let (c3, _, e3) = forge(
        &dir,
        &[
            "forge",
            "pr",
            "create",
            "--source",
            "refs/heads/feat/thing",
            "--base",
            "refs/heads/main",
            "T3",
        ],
    );
    assert_eq!(c3, 0, "full-form refs must be accepted: {e3}");
    // Green CI Check so the merge gate lets PR #2 proceed.
    let (cc, oc, ec) = forge(&dir, &["forge", "ci", "run", "2"]);
    assert_eq!(cc, 0, "ci run 2 must pass: {ec} {oc}");
    let (cs3, os3, _) = forge(&dir, &["forge", "pr", "show", "2"]);
    assert_eq!(cs3, 0);
    assert!(
        os3.contains("source: feat/thing"),
        "canonical bare source stored: {os3}"
    );
    assert!(
        os3.contains("base: main"),
        "canonical bare base stored: {os3}"
    );
    let (cr, _, er) = forge(&dir, &["forge", "pr", "review", "2", "--approve"]);
    assert_eq!(cr, 0, "review full-form PR: {er}");
    let (ch, _, eh) = git(&dir, &["checkout", "-q", "feat/thing"]);
    assert_eq!(ch, 0, "checkout feat/thing: {eh}");
    let (cm, _, em) = forge(&dir, &["forge", "pr", "merge", "2"]);
    assert_eq!(cm, 0, "merge full-form PR: {em}");
}

#[test]
fn pr_create_rejects_self_and_same_commit() {
    let dir = tmpdir("selfpr");
    init_repo(&dir);
    make_feature(&dir, "feature", "feat\n");
    // same branch
    let (c1, _, e1) = forge(
        &dir,
        &[
            "forge", "pr", "create", "--source", "feature", "--base", "feature", "T",
        ],
    );
    assert_ne!(c1, 0, "same branch must error");
    assert!(
        e1.contains("self-PR") || e1.contains("differ"),
        "stderr: {e1}"
    );
    // distinct branches resolving to same commit: branch2 == main-ancestor
    git(&dir, &["branch", "empty", "main"]);
    let (c2, _, e2) = forge(
        &dir,
        &[
            "forge", "pr", "create", "--source", "empty", "--base", "main", "T",
        ],
    );
    assert_ne!(c2, 0, "same commit must error");
    assert!(e2.contains("same commit"), "stderr: {e2}");
    assert!(ref_oid(&dir, "refs/forge/prs/1/head").is_none());
}

#[test]
fn pr_create_zero_merge_bases_asks_to_deepen_or_marks_unrelated() {
    let dir = tmpdir("zerobase");
    init_repo(&dir);
    // Two truly unrelated roots (orphan histories, no common ancestor).
    let (c0, _, e0) = git(&dir, &["checkout", "--orphan", "z1"]);
    assert!(c0 == 0, "orphan z1: {e0}");
    let _ = std::fs::remove_file(dir.join("base.txt"));
    std::fs::write(dir.join("a.txt"), "a\n").unwrap();
    let (c, _, e) = git(&dir, &["add", "a.txt"]);
    assert!(c == 0, "add: {e}");
    let (c, _, e) = git(&dir, &["commit", "-q", "-m", "z1"]);
    assert!(c == 0, "commit z1: {e}");
    let (c0, _, e0) = git(&dir, &["checkout", "--orphan", "z2"]);
    assert!(c0 == 0, "orphan z2: {e0}");
    let _ = std::fs::remove_file(dir.join("a.txt"));
    std::fs::write(dir.join("b.txt"), "b\n").unwrap();
    let (c, _, e) = git(&dir, &["add", "b.txt"]);
    assert!(c == 0, "add: {e}");
    let (c, _, e) = git(&dir, &["commit", "-q", "-m", "z2"]);
    assert!(c == 0, "commit z2: {e}");

    let (c, _, e) = forge(
        &dir,
        &[
            "forge", "pr", "create", "--source", "z1", "--base", "z2", "T",
        ],
    );
    assert_ne!(c, 0, "zero merge-base must be rejected");
    assert!(
        e.contains("deepen") || e.contains("unrelated"),
        "stderr must guide the user: {e}"
    );
    assert!(ref_oid(&dir, "refs/forge/prs/1/head").is_none());
}

#[test]
fn pr_create_rejects_multiple_merge_bases() {
    let dir = tmpdir("crisscross");
    init_repo(&dir);
    // Genuine criss-cross with TWO merge bases. Capture each branch's original
    // tip (A on x, B on y) BEFORE any merge, then:
    //   x2 = merge(A, B)  — parents (A, B)
    //   y2 = merge(B, A)  — parents (B, A)
    // x2 and y2 share exactly A and B as maximal common ancestors → 2 merge
    // bases. (Merging branch names after the first merge would make its tip an
    // ancestor and collapse to 1 base — the very trap this test guards.)
    git(&dir, &["checkout", "-q", "-b", "x"]);
    std::fs::write(dir.join("x.txt"), "x\n").unwrap();
    git(&dir, &["add", "x.txt"]);
    git(&dir, &["commit", "-q", "-m", "x"]);
    let a = git(&dir, &["rev-parse", "refs/heads/x"]).1;

    git(&dir, &["checkout", "-q", "main"]);
    git(&dir, &["checkout", "-q", "-b", "y"]);
    std::fs::write(dir.join("y.txt"), "y\n").unwrap();
    git(&dir, &["add", "y.txt"]);
    git(&dir, &["commit", "-q", "-m", "y"]);
    let b = git(&dir, &["rev-parse", "refs/heads/y"]).1;

    // x2 = merge(A, B)
    git(&dir, &["checkout", "-q", "x"]);
    git(&dir, &["merge", "-q", "--no-edit", "--no-ff", &b]);
    // y2 = merge(B, A) — merge the ORIGINAL A oid, not x's merged tip.
    git(&dir, &["checkout", "-q", "y"]);
    git(&dir, &["merge", "-q", "--no-edit", "--no-ff", &a]);

    // Confirm two merge bases from the CLI's own guard.
    let (cm, om, em) = git(
        &dir,
        &["merge-base", "--all", "refs/heads/x", "refs/heads/y"],
    );
    assert_eq!(cm, 0, "merge-base failed: {em}");
    let count = om.split_whitespace().filter(|l| !l.is_empty()).count();
    assert!(count >= 2, "expected >=2 merge bases, got {count} ({om})");

    let (c, _, e) = forge(
        &dir,
        &["forge", "pr", "create", "--source", "x", "--base", "y", "T"],
    );
    assert_ne!(c, 0, "criss-cross must be rejected");
    assert!(
        e.contains("merge base") || e.contains("criss-cross"),
        "stderr: {e}"
    );
    assert!(ref_oid(&dir, "refs/forge/prs/1/head").is_none());
}

#[test]
fn pr_review_comment_diff_roundtrip() {
    let dir = tmpdir("review");
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
        forge(&dir, &["forge", "pr", "comment", "1", "looks good"]).0,
        0
    );
    assert_eq!(
        forge(&dir, &["forge", "pr", "review", "1", "--approve"]).0,
        0
    );

    let (cs, os, _) = forge(&dir, &["forge", "pr", "show", "1"]);
    assert_eq!(cs, 0);
    assert!(os.contains("decision: approve"), "show decision: {os}");
    assert!(os.contains("looks good"), "show comment: {os}");

    // diff: base...source contains the feature change.
    let (cd, od, ed) = forge(&dir, &["forge", "pr", "diff", "1"]);
    assert_eq!(cd, 0, "pr diff failed: {ed}");
    assert!(od.contains("feature.txt"), "diff output: {od}");
}

#[test]
fn review_decisions_transition() {
    let dir = tmpdir("decision");
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
    let (_, os1, _) = forge(&dir, &["forge", "pr", "show", "1"]);
    assert!(os1.contains("decision: approve"));
    // approve → reject → effective reject
    assert_eq!(
        forge(&dir, &["forge", "pr", "review", "1", "--reject"]).0,
        0
    );
    let (_, os2, _) = forge(&dir, &["forge", "pr", "show", "1"]);
    assert!(os2.contains("decision: reject"), "approve→reject: {os2}");
    // reject → approve → effective approve
    assert_eq!(
        forge(&dir, &["forge", "pr", "review", "1", "--approve"]).0,
        0
    );
    let (_, os3, _) = forge(&dir, &["forge", "pr", "show", "1"]);
    assert!(os3.contains("decision: approve"), "reject→approve: {os3}");
}

#[test]
fn inline_review_anchors_commit() {
    let dir = tmpdir("inline");
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
    let feat_commit = git(&dir, &["rev-parse", "refs/heads/feature"]).1;
    let c = forge(
        &dir,
        &[
            "forge",
            "pr",
            "review",
            "1",
            "--approve",
            "--file",
            "feature.txt",
            "--line",
            "1",
            "--commit",
            &feat_commit,
        ],
    );
    assert_eq!(c.0, 0, "inline review failed: {}", c.2);
    let (cd, od, ed) = forge(&dir, &["forge", "pr", "diff", "1"]);
    assert_eq!(cd, 0, "diff failed: {ed}");
    assert!(od.contains("+feat"), "diff shows +feat: {od}");
    // The event chain stores the anchored fields (readable via refs).
    let chain = git(
        &dir,
        &[
            "for-each-ref",
            "--format=%(refname)",
            "refs/forge/prs/1/head",
        ],
    )
    .1;
    assert!(chain.contains("refs/forge/prs/1/head"));
}

#[test]
fn inline_review_requires_commit_anchor() {
    let dir = tmpdir("inlinereq");
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
    let before = git(&dir, &["rev-parse", "refs/forge/prs/1/head"]).1;
    // FR-005: file/line without a commit anchor must be rejected, and no
    // pr.review event may be appended (head ref unchanged).
    let (c, _, e) = forge(
        &dir,
        &[
            "forge",
            "pr",
            "review",
            "1",
            "--approve",
            "--file",
            "feature.txt",
            "--line",
            "1",
        ],
    );
    assert_ne!(c, 0, "unanchored inline review must be rejected");
    assert!(
        e.contains("--commit"),
        "error must ask for --commit anchor: {e}"
    );
    let after = git(&dir, &["rev-parse", "refs/forge/prs/1/head"]).1;
    assert_eq!(before, after, "no pr.review appended without anchor");
    // FR-005: the anchor must resolve to a real commit object — a bogus hash
    // or non-commit text is rejected, and no pr.review is appended.
    for bogus in ["deadbeef", "not-a-commit"] {
        let (cb, _, eb) = forge(
            &dir,
            &[
                "forge",
                "pr",
                "review",
                "1",
                "--approve",
                "--file",
                "feature.txt",
                "--line",
                "1",
                "--commit",
                bogus,
            ],
        );
        assert_ne!(cb, 0, "bogus anchor '{bogus}' must be rejected");
        assert!(
            eb.contains("does not resolve to a commit"),
            "error must explain the bad anchor '{bogus}': {eb}"
        );
        assert_eq!(
            git(&dir, &["rev-parse", "refs/forge/prs/1/head"]).1,
            after,
            "no pr.review appended for bogus anchor '{bogus}'"
        );
    }
    // FR-005 immutability: a ref name like `main` is accepted as an anchor
    // input but the EVENT must store the peeled commit OID, never the mutable
    // ref name.
    let main_oid = git(&dir, &["rev-parse", "refs/heads/main"]).1;
    let (cr, _, er) = forge(
        &dir,
        &[
            "forge",
            "pr",
            "review",
            "1",
            "--approve",
            "--file",
            "feature.txt",
            "--line",
            "1",
            "--commit",
            "main",
        ],
    );
    assert_eq!(cr, 0, "ref-name anchor must be accepted: {er}");
    let head_tip = git(&dir, &["rev-parse", "refs/forge/prs/1/head"]).1;
    let event = git(&dir, &["show", &format!("{head_tip}:.forge/event.json")]).1;
    assert!(
        event.contains(&main_oid),
        "event must store the peeled commit OID: {event}"
    );
    assert!(
        !event.contains("\"main\""),
        "event must not store the mutable ref name: {event}"
    );
    // Plain approve (no inline options) still works.
    let (c2, _, e2) = forge(&dir, &["forge", "pr", "review", "1", "--approve"]);
    assert_eq!(c2, 0, "plain approve must still work: {e2}");
}

#[test]
fn review_precedence_nonexistent_pr_before_invalid_anchor() {
    let dir = tmpdir("reviewprec");
    init_repo(&dir);
    make_feature(&dir, "feature", "feat\n");
    // Nonexistent PR + bogus anchor: the entity error wins (established
    // behavior for every other pr subcommand); never resolve/validate the
    // anchor for a PR that does not exist.
    let (c, _, e) = forge(
        &dir,
        &[
            "forge",
            "pr",
            "review",
            "99",
            "--approve",
            "--file",
            "feature.txt",
            "--line",
            "1",
            "--commit",
            "not-a-commit",
        ],
    );
    assert_ne!(c, 0, "nonexistent PR must be rejected");
    assert!(
        e.contains("PR #99 does not exist"),
        "entity error must precede anchor validation: {e}"
    );
    assert!(
        !e.contains("does not resolve to a commit"),
        "anchor must not be resolved for a nonexistent PR: {e}"
    );
}

#[test]
fn snapshot_survives_branch_deletion_and_gc() {
    let dir = tmpdir("gc");
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
    // delete the source branch and gc
    git(&dir, &["branch", "-D", "feature"]);
    git(&dir, &["gc", "--prune=now", "--quiet"]);
    // diff still works from immutable snapshot refs.
    let (cd, od, ed) = forge(&dir, &["forge", "pr", "diff", "1"]);
    assert_eq!(cd, 0, "diff after delete+gc failed: {ed}");
    assert!(od.contains("feature.txt"), "diff output: {od}");
    // show still works
    let (cs, os, es) = forge(&dir, &["forge", "pr", "show", "1"]);
    assert_eq!(cs, 0, "show after delete+gc failed: {es}");
    assert!(os.contains("PR #1"), "show: {os}");
}

#[test]
fn git_forge_pr_discovery_on_path() {
    let dir = tmpdir("gforgepr");
    init_repo(&dir);
    make_feature(&dir, "feature", "feat\n");
    let bindir = dir.join(".gforgepath");
    std::fs::create_dir_all(&bindir).unwrap();
    let bin = env!("CARGO_BIN_EXE_git-forge");
    std::fs::copy(bin, bindir.join("git-forge")).unwrap();
    let path = format!(
        "{}:{}",
        bindir.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let out = Command::new("git")
        .args([
            "forge",
            "pr",
            "create",
            "--source",
            "feature",
            "--base",
            "main",
            "discovered PR",
        ])
        .current_dir(&dir)
        .env("PATH", &path)
        .output()
        .unwrap();
    let code = out.status.code().unwrap_or(-1);
    assert_eq!(
        code,
        0,
        "git forge pr create failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    // `git forge pr list` via the same PATH.
    let out2 = Command::new("git")
        .args(["forge", "pr", "list"])
        .current_dir(&dir)
        .env("PATH", &path)
        .output()
        .unwrap();
    let code2 = out2.status.code().unwrap_or(-1);
    assert_eq!(code2, 0, "git forge pr list failed");
    let list = String::from_utf8_lossy(&out2.stdout);
    assert!(list.contains("discovered PR"), "list output: {list}");
}

#[test]
fn nonexistent_pr_is_clean_error() {
    let dir = tmpdir("nopr");
    init_repo(&dir);
    let cases: [&[&str]; 3] = [
        &["forge", "pr", "show", "99"],
        &["forge", "pr", "diff", "99"],
        &["forge", "pr", "review", "99", "--approve"],
    ];
    for cmd in cases {
        let (c, _, e) = forge(&dir, cmd);
        assert_ne!(c, 0, "cmd {cmd:?} must fail");
        assert!(e.contains("#99"), "stderr for {cmd:?}: {e}");
    }
}

#[test]
fn pr_help_lists_every_subcommand() {
    // `git forge pr --help` (and a bare `pr`) must print usage naming all
    // subcommands. Covers the pr_help() block.
    let dir = tmpdir("phelp");
    let (c, o, e) = forge(&dir, &["forge", "pr", "--help"]);
    assert_eq!(c, 0, "pr --help failed: {e}");
    for sub in [
        "create", "list", "show", "comment", "review", "diff", "merge",
    ] {
        assert!(o.contains(sub), "pr help missing '{sub}': {o}");
    }
    assert!(o.contains("usage: git forge pr"), "help head: {o}");
    let (c2, o2, e2) = forge(&dir, &["forge", "pr"]);
    assert_eq!(c2, 0, "bare pr failed: {e2}");
    assert!(o2.contains("usage: git forge pr"), "bare pr help: {o2}");
}

#[test]
fn pr_create_supports_body_and_labels() {
    // `pr create --source <b> --base <b> <title> --body <text> --label <x>...`
    // stores a description and labels; `pr show` renders both.
    let dir = tmpdir("prlabel");
    init_repo(&dir);
    make_feature(&dir, "feature", "x\n");
    let (c, _o, e) = forge(
        &dir,
        &[
            "forge",
            "pr",
            "create",
            "--source",
            "feature",
            "--base",
            "main",
            "T",
            "--body",
            "the body",
            "--label",
            "enhancement",
            "--label",
            "docs",
        ],
    );
    assert_eq!(c, 0, "pr create with body/labels failed: {e}");
    let (cs, os, es) = forge(&dir, &["forge", "pr", "show", "1"]);
    assert_eq!(cs, 0, "pr show failed: {es}");
    assert!(os.contains("description: the body"), "body shown: {os}");
    assert!(
        os.contains("labels: enhancement, docs"),
        "labels shown: {os}"
    );
}

#[test]
fn cli_arg_validation_errors_are_user_facing() {
    // One pass covering the many CLI arg-validation branches: each misuse must
    // surface a clean exit-1 message, never a panic or libgit2 internal leak.
    let dir = tmpdir("argerr");
    init_repo(&dir);
    // The inline-anchor case needs an existing PR (the store-existence check
    // runs before the inline requires-commit check); create PR #1 first.
    make_feature(&dir, "feature", "feat\n");
    let (cc, _oc, ec) = forge(
        &dir,
        &[
            "forge", "pr", "create", "--source", "feature", "--base", "main", "t",
        ],
    );
    assert_eq!(cc, 0, "pr create failed: {ec}");
    let cases: [(&[&str], &str); 11] = [
        (&["forge", "issue", "new"], "usage: git forge issue new"),
        (
            &["forge", "issue", "comment", "1"],
            "usage: git forge issue comment",
        ),
        (
            &["forge", "issue", "show", "0"],
            "entity id must be positive",
        ),
        (&["forge", "pr", "create"], "usage: git forge pr create"),
        (
            &["forge", "pr", "create", "--source", "feature"],
            "usage: git forge pr create",
        ),
        (
            &["forge", "pr", "review", "1"],
            "usage: git forge pr review",
        ),
        (
            &["forge", "pr", "comment", "1"],
            "usage: git forge pr comment",
        ),
        (&["forge", "pr", "merge", "--squash"], "empty entity id"),
        (
            &["forge", "pr", "merge", "1", "--squash", "--rebase"],
            "merge strategy flag specified more than once",
        ),
        (
            &[
                "forge",
                "pr",
                "review",
                "1",
                "--approve",
                "--file",
                "x",
                "--line",
                "1",
            ],
            "inline review requires --commit",
        ),
        (
            &["forge", "pr", "merge", "1", "--bogus-flag"],
            "unknown option",
        ),
    ];
    for (cmd, needle) in &cases {
        let (c, _o, e) = forge(&dir, cmd);
        assert_ne!(c, 0, "cmd {cmd:?} must fail: {e}");
        assert!(
            e.contains(needle),
            "cmd {cmd:?} stderr should contain '{needle}': {e}"
        );
        assert!(
            !e.contains("class=Repository") && !e.contains("could not find repository"),
            "cmd {cmd:?} must not leak libgit2 internals: {e}"
        );
    }
}

// ─────────────────────────── CI run (t0) ───────────────────────────

/// Read the `.forge/event.json` payload of the given commit (PR head chain tip).
fn head_event(dir: &PathBuf, pr: u64) -> String {
    let head = git(dir, &["rev-parse", &format!("refs/forge/prs/{pr}/head")]).1;
    let (c, event, e) = git(dir, &["show", &format!("{head}:.forge/event.json")]);
    assert_eq!(c, 0, "read {head}:.forge/event.json failed: {e}");
    event
}

/// Parse `YYYY-MM-DDTHH:MM:SSZ` (RFC3339 UTC) into seconds since the Unix
/// epoch — the independent test oracle for the CI Check run-time timestamp.
fn parse_rfc3339_utc(s: &str) -> Option<i64> {
    let b = s.as_bytes();
    if b.len() != 20
        || b[4] != b'-'
        || b[7] != b'-'
        || b[10] != b'T'
        || b[13] != b':'
        || b[16] != b':'
        || b[19] != b'Z'
    {
        return None;
    }
    let num = |r: std::ops::Range<usize>| -> Option<i64> {
        let mut n = 0i64;
        for &c in &b[r] {
            if !c.is_ascii_digit() {
                return None;
            }
            n = n * 10 + (c - b'0') as i64;
        }
        Some(n)
    };
    let year = num(0..4)?;
    let month = num(5..7)?;
    let day = num(8..10)?;
    let hour = num(11..13)?;
    let min = num(14..16)?;
    let sec = num(17..19)?;
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) || hour > 23 || min > 59 || sec > 60 {
        return None;
    }
    let y = if month <= 2 { year - 1 } else { year };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = if month > 2 { month - 3 } else { month + 9 };
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146097 + doe - 719468;
    Some(days * 86400 + hour * 3600 + min * 60 + sec)
}

/// Write an executable `git` shim into a fresh temp dir and return the dir.
/// The script defines `REAL_GIT`, runs `body`, then falls through to
/// `exec "$REAL_GIT" "$@"`. Used to deterministically fail/record specific
/// `git` calls during a `ci run` (F-002/F-003).
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

/// Assert no CI temp worktree remains registered: `git worktree list
/// --porcelain` must not contain the disposable `git-forge-pr1-ci` path.
fn assert_no_ci_worktree(dir: &Path) {
    let (wl, out, _) = git(&dir.to_path_buf(), &["worktree", "list", "--porcelain"]);
    assert_eq!(wl, 0, "worktree list must succeed");
    assert!(
        !out.contains("git-forge-pr1-ci"),
        "CI temp worktree left behind: {out}"
    );
}

/// Best-effort hygiene for the F-002/F-003 regression tests: remove the
/// leftover CI worktree (registered under the `git-forge-pr1-ci` pattern)
/// using the REAL git — the run that produced it ran under a shim, so this
/// must not go through that shim.
fn remove_leftover_ci_worktree(dir: &Path) {
    let (wl, out, _) = git(&dir.to_path_buf(), &["worktree", "list", "--porcelain"]);
    assert_eq!(wl, 0, "worktree list must succeed");
    for line in out.lines() {
        if let Some(p) = line.strip_prefix("worktree ") {
            if p.contains("git-forge-pr1-ci") {
                let (r, _, er) = git(&dir.to_path_buf(), &["worktree", "remove", "--force", p]);
                assert!(r == 0, "hygiene worktree remove failed: {er}");
                return;
            }
        }
    }
    panic!("expected a leftover CI worktree to clean; list: {out}");
}

/// True if the process with the given pid still exists (running OR a not-yet-
/// reaped zombie); false when it is gone (ESRCH). Used by the F-015
/// background-descendant regression to poll for the group reaping.
fn process_alive(pid: u32) -> bool {
    let out = Command::new("kill")
        .arg("-0")
        .arg(pid.to_string())
        .output()
        .unwrap();
    out.status.code() == Some(0)
}

/// VAL-001: a passing plan records a CI Check `success`, the command exits 0,
/// and the developer's working tree + current branch are unchanged.
#[test]
fn ci_run_success_records_success_and_leaves_worktree_unchanged() {
    let dir = tmpdir("ci-success");
    init_repo(&dir);
    git(&dir, &["config", "user.email", "ci@example.com"]);
    // The CI plan is committed in the PR's OWN commits (the source branch), so
    // running it from a detached temp worktree validates exactly the PR.
    git(&dir, &["checkout", "-q", "-b", "feature"]);
    std::fs::create_dir_all(dir.join(".forge")).unwrap();
    std::fs::write(dir.join(".forge").join("ci.sh"), "#!/bin/bash\nexit 0\n").unwrap();
    std::fs::write(dir.join("feature.txt"), "feat\n").unwrap();
    git(&dir, &["add", "."]);
    git(&dir, &["commit", "-q", "-m", "feature + passing ci plan"]);
    git(&dir, &["checkout", "-q", "main"]);

    assert_eq!(
        forge(
            &dir,
            &["forge", "pr", "create", "--source", "feature", "--base", "main", "PR1"]
        )
        .0,
        0
    );
    let before_branch = git(&dir, &["rev-parse", "--abbrev-ref", "HEAD"]).1;
    let before_main = git(&dir, &["rev-parse", "refs/heads/main"]).1;

    let t0 = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    let (c, o, e) = forge(&dir, &["forge", "ci", "run", "1"]);
    let t1 = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    assert_eq!(c, 0, "ci run must exit 0 on a passing plan: {e}");
    assert!(o.contains("passed"), "ci run output: {o}");

    let event = head_event(&dir, 1);
    assert!(event.contains("\"ci.check\""), "event kind: {event}");
    assert!(event.contains("\"status\":\"success\""), "status: {event}");
    assert!(event.contains("\"plan\":\".forge/ci.sh\""), "plan: {event}");
    assert!(
        event.contains("\"actor\":\"ci@example.com\""),
        "actor: {event}"
    );
    // F-001: the persisted CI Check ts must be the actual RFC3339 UTC run
    // time, never the hard-coded 1970 epoch placeholder.
    let ts = event
        .split("\"ts\":\"")
        .nth(1)
        .and_then(|s| s.split('"').next())
        .expect("persisted event must carry a ts field");
    assert_ne!(
        ts, "1970-01-01T00:00:00Z",
        "persisted CI Check ts must not be epoch: {event}"
    );
    let ts_secs = parse_rfc3339_utc(ts).expect("persisted CI Check ts must be RFC3339 UTC");
    assert!(
        ts_secs >= t0 && ts_secs <= t1,
        "persisted CI Check ts {ts} must fall in the run window [{t0},{t1}]"
    );

    // The fold exposes the latest CI status on the PR chain (readable via show).
    let (cs, os, es) = forge(&dir, &["forge", "pr", "show", "1"]);
    assert_eq!(cs, 0, "pr show after ci run failed: {es}");
    assert!(os.contains("ci: success"), "show ci status: {os}");

    // Developer's working tree and current branch are unchanged.
    assert_eq!(
        git(&dir, &["rev-parse", "--abbrev-ref", "HEAD"]).1,
        before_branch,
        "current branch must be unchanged"
    );
    assert_eq!(
        git(&dir, &["rev-parse", "refs/heads/main"]).1,
        before_main,
        "branch tip must be unchanged"
    );
    let (_, status, _) = git(&dir, &["status", "--porcelain"]);
    assert_eq!(status.trim(), "", "working tree must be clean: {status}");
}

/// VAL-002: a failing plan records a CI Check `failure`, the command exits
/// nonzero, and the working tree stays clean.
#[test]
fn ci_run_failure_records_failure_and_exits_nonzero() {
    let dir = tmpdir("ci-failure");
    init_repo(&dir);
    git(&dir, &["config", "user.email", "ci@example.com"]);
    git(&dir, &["checkout", "-q", "-b", "feature"]);
    std::fs::create_dir_all(dir.join(".forge")).unwrap();
    std::fs::write(dir.join(".forge").join("ci.sh"), "#!/bin/bash\nexit 1\n").unwrap();
    std::fs::write(dir.join("feature.txt"), "feat\n").unwrap();
    git(&dir, &["add", "."]);
    git(&dir, &["commit", "-q", "-m", "feature + failing ci plan"]);
    git(&dir, &["checkout", "-q", "main"]);

    assert_eq!(
        forge(
            &dir,
            &["forge", "pr", "create", "--source", "feature", "--base", "main", "PR1"]
        )
        .0,
        0
    );

    let (c, _o, e) = forge(&dir, &["forge", "ci", "run", "1"]);
    assert_ne!(c, 0, "ci run must exit nonzero on a failing plan: {e}");

    let event = head_event(&dir, 1);
    assert!(event.contains("\"ci.check\""), "event kind: {event}");
    assert!(event.contains("\"status\":\"failed\""), "status: {event}");
    assert!(event.contains("\"plan\":\".forge/ci.sh\""), "plan: {event}");

    let (_, status, _) = git(&dir, &["status", "--porcelain"]);
    assert_eq!(status.trim(), "", "working tree must be clean: {status}");
}

/// F-002 regression: a CI plan that emits a high-volume output stream must not
/// be buffered into git-forge's memory without bound. `run_ci_plan` discards
/// the plan's stdout/stderr (only the exit status matters), so a noisy plan
/// cannot OOM git-forge before it records the failed Check and cleans the temp
/// worktree. The stream here is large (well past the 64 KiB OS pipe buffer on
/// both stdout and stderr) but terminating, so a run that accumulated it would
/// grow the process buffer; the run must still record a `failure` Check, clean
/// the temp worktree, and leave the developer's working tree untouched.
#[test]
fn ci_run_high_volume_plan_records_failure_and_cleans_worktree() {
    let dir = tmpdir("ci-high-volume");
    init_repo(&dir);
    git(&dir, &["config", "user.email", "ci@example.com"]);
    git(&dir, &["checkout", "-q", "-b", "feature"]);
    std::fs::create_dir_all(dir.join(".forge")).unwrap();
    // 32 MiB to stdout and 32 MiB to stderr (64 MiB total), then fail. Under the
    // offending `Command::output()` the entire stream would be accumulated in
    // git-forge's heap; the fix redirects it to the null device and keeps only
    // the exit status, so memory stays bounded.
    std::fs::write(
        dir.join(".forge").join("ci.sh"),
        "#!/bin/bash\n\
         yes \"{0000000000000000000000000000000000000000000000000000000000000000}\" | head -c 33554432\n\
         yes \"{1111111111111111111111111111111111111111111111111111111111111111}\" | head -c 33554432 >&2\n\
         exit 1\n",
    )
    .unwrap();
    std::fs::write(dir.join("feature.txt"), "feat\n").unwrap();
    git(&dir, &["add", "."]);
    git(
        &dir,
        &["commit", "-q", "-m", "feature + noisy failing ci plan"],
    );
    git(&dir, &["checkout", "-q", "main"]);

    assert_eq!(
        forge(
            &dir,
            &["forge", "pr", "create", "--source", "feature", "--base", "main", "PR1"]
        )
        .0,
        0
    );

    let (c, _o, e) = forge(&dir, &["forge", "ci", "run", "1"]);
    assert_ne!(c, 0, "noisy failing plan must exit nonzero: {e}");
    assert!(
        e.contains("exited with status 1"),
        "the retained exit status must be reported: {e}"
    );

    let event = head_event(&dir, 1);
    assert!(event.contains("\"ci.check\""), "event kind: {event}");
    assert!(event.contains("\"status\":\"failed\""), "status: {event}");
    assert!(event.contains("\"plan\":\".forge/ci.sh\""), "plan: {event}");

    // The temp CI worktree must be removed (no leftover), and the developer's
    // working tree / current branch are unchanged.
    assert_no_ci_worktree(&dir);
    let (_, status, _) = git(&dir, &["status", "--porcelain"]);
    assert_eq!(status.trim(), "", "working tree must be clean: {status}");
    assert_eq!(git(&dir, &["rev-parse", "--abbrev-ref", "HEAD"]).1, "main");
}

/// F-002 regression: a NONTERMINATING CI plan (e.g. `#!/bin/sh\nexec yes`) must
/// not hang `ci run` forever. The wait is bounded by `GIT_FORGE_CI_TIMEOUT`;
/// on expiry the plan process is killed and reaped, a `failure` CI Check is
/// recorded, the temp worktree is removed, and the developer's working tree /
/// branch are untouched (F-002 round 2).
#[test]
fn ci_run_nonterminating_plan_times_out_records_failure_and_cleans() {
    let dir = tmpdir("ci-nonterm");
    init_repo(&dir);
    git(&dir, &["config", "user.email", "ci@example.com"]);
    git(&dir, &["checkout", "-q", "-b", "feature"]);
    std::fs::create_dir_all(dir.join(".forge")).unwrap();
    // `exec yes` replaces the shell with a never-exiting process; without a
    // bounded deadline the run would hang and never reach the failed Check
    // append or the temp-worktree cleanup (F-002).
    std::fs::write(dir.join(".forge").join("ci.sh"), "#!/bin/sh\nexec yes\n").unwrap();
    std::fs::write(dir.join("feature.txt"), "feat\n").unwrap();
    git(&dir, &["add", "."]);
    git(
        &dir,
        &["commit", "-q", "-m", "feature + nonterminating ci plan"],
    );
    git(&dir, &["checkout", "-q", "main"]);

    assert_eq!(
        forge(
            &dir,
            &["forge", "pr", "create", "--source", "feature", "--base", "main", "PR1"]
        )
        .0,
        0
    );

    // Bound the wait to 1s: a hung run would never return here (stalling the
    // gate), while a fixed run returns in ~1s and records a failed Check.
    let t0 = std::time::SystemTime::now();
    let (c, _o, e) = forge_with_env(
        &dir,
        &["forge", "ci", "run", "1"],
        &[("GIT_FORGE_CI_TIMEOUT", "1")],
    );
    let elapsed = t0.elapsed().unwrap().as_secs();
    assert_ne!(c, 0, "nonterminating plan must exit nonzero: {e}");
    assert!(
        elapsed < 30,
        "ci run must return within the bounded deadline (took {elapsed}s)"
    );

    let event = head_event(&dir, 1);
    assert!(event.contains("\"ci.check\""), "event kind: {event}");
    assert!(event.contains("\"status\":\"failed\""), "status: {event}");
    assert!(event.contains("\"plan\":\".forge/ci.sh\""), "plan: {event}");

    // The temp CI worktree must be removed (no leftover), and the developer's
    // working tree / current branch are unchanged.
    assert_no_ci_worktree(&dir);
    let (_, status, _) = git(&dir, &["status", "--porcelain"]);
    assert_eq!(status.trim(), "", "working tree must be clean: {status}");
    assert_eq!(git(&dir, &["rev-parse", "--abbrev-ref", "HEAD"]).1, "main");
}

/// VAL-003: a repo with no `.forge/ci.sh` runs the `just check` fallback and
/// succeeds (exit 0), recording a `success` CI Check with plan `just check`.
#[test]
fn ci_run_fallback_just_check_succeeds_without_ci_sh() {
    let dir = tmpdir("ci-fallback");
    init_repo(&dir);
    git(&dir, &["config", "user.email", "ci@example.com"]);
    git(&dir, &["checkout", "-q", "-b", "feature"]);
    // No `.forge/ci.sh`; a justfile with a `check` recipe is the fallback.
    std::fs::write(
        dir.join("justfile"),
        "check:\n    echo \"just check passed\"\n",
    )
    .unwrap();
    std::fs::write(dir.join("feature.txt"), "feat\n").unwrap();
    git(&dir, &["add", "."]);
    git(
        &dir,
        &["commit", "-q", "-m", "feature + justfile, no .forge/ci.sh"],
    );
    git(&dir, &["checkout", "-q", "main"]);

    assert_eq!(
        forge(
            &dir,
            &["forge", "pr", "create", "--source", "feature", "--base", "main", "PR1"]
        )
        .0,
        0
    );
    // Confirm the source tree really has no `.forge/ci.sh`.
    assert!(
        !dir.join(".forge").join("ci.sh").exists(),
        "test setup must have no .forge/ci.sh"
    );

    let (c, o, e) = forge(&dir, &["forge", "ci", "run", "1"]);
    assert_eq!(c, 0, "fallback just check must succeed: {e}");
    assert!(o.contains("passed"), "ci run output: {o}");

    let event = head_event(&dir, 1);
    assert!(event.contains("\"status\":\"success\""), "status: {event}");
    assert!(event.contains("\"plan\":\"just check\""), "plan: {event}");

    // Current branch and working tree unchanged.
    assert_eq!(git(&dir, &["rev-parse", "--abbrev-ref", "HEAD"]).1, "main");
    let (_, status, _) = git(&dir, &["status", "--porcelain"]);
    assert_eq!(status.trim(), "", "working tree must be clean: {status}");
}

/// F-001 regression: a `.forge/ci.sh` that is a tracked symlink redirecting CI
/// to mutable bytes OUTSIDE the PR's immutable source snapshot must be refused —
/// clean error, exit nonzero, and no green Check. The external target changes
/// AFTER the PR snapshot is taken, so a run that followed the link would
/// execute external mutable bytes instead of the frozen snapshot and record a
/// green Check from them.
#[test]
fn ci_run_refuses_symlink_escape_after_target_changes() {
    let dir = tmpdir("ci-symlink-escape");
    init_repo(&dir);
    git(&dir, &["config", "user.email", "ci@example.com"]);

    // An external, mutable CI script that lives OUTSIDE the repo (never
    // committed). It fails initially, then is flipped to pass AFTER PR
    // creation — a run that follows the symlink would read the changed
    // external bytes.
    let ext = tmpdir("ci-symlink-escape-ext");
    let ext_script = ext.join("evil.sh");
    std::fs::write(&ext_script, "#!/bin/bash\nexit 1\n").unwrap();

    // Commit `.forge/ci.sh` as a symlink pointing at the external script.
    git(&dir, &["checkout", "-q", "-b", "feature"]);
    std::fs::create_dir_all(dir.join(".forge")).unwrap();
    std::os::unix::fs::symlink(&ext_script, dir.join(".forge").join("ci.sh")).unwrap();
    std::fs::write(dir.join("feature.txt"), "feat\n").unwrap();
    git(&dir, &["add", "."]);
    git(&dir, &["commit", "-q", "-m", "feature + symlinked ci plan"]);
    git(&dir, &["checkout", "-q", "main"]);

    let (c, o, e) = forge(
        &dir,
        &[
            "forge", "pr", "create", "--source", "feature", "--base", "main", "PR1",
        ],
    );
    assert_eq!(c, 0, "pr create failed: {e} {o}");

    // The external symlink target CHANGES to a passing script after the PR's
    // immutable snapshot was taken.
    std::fs::write(&ext_script, "#!/bin/bash\nexit 0\n").unwrap();

    // The hardened selector must refuse to follow the symlink out of the
    // snapshot: clean error, exit nonzero, and no green CI Check recorded.
    let (cr, or_, er) = forge(&dir, &["forge", "ci", "run", "1"]);
    assert_ne!(
        cr, 0,
        "ci run must refuse a symlinked .forge/ci.sh: {or_} {er}"
    );
    assert!(
        er.contains("symlink"),
        "stderr must name the symlink refusal: {er}"
    );

    let event = head_event(&dir, 1);
    assert!(
        !event.contains("\"status\":\"success\""),
        "must not record a green Check from external bytes: {event}"
    );

    // Working tree + current branch unchanged, no temp worktree left behind.
    assert_eq!(git(&dir, &["rev-parse", "--abbrev-ref", "HEAD"]).1, "main");
    let (_, status, _) = git(&dir, &["status", "--porcelain"]);
    assert_eq!(status.trim(), "", "working tree must be clean: {status}");
    assert_no_ci_worktree(&dir);
}

/// F-012 regression: a `justfile` (the `just check` fallback plan) that is a
/// tracked symlink redirecting CI to mutable bytes OUTSIDE the PR's immutable
/// source snapshot must be refused — clean error, exit nonzero, and no green
/// Check. The external target changes AFTER the PR snapshot is taken, so a run
/// that followed the link (or that searched an ancestor/global justfile) would
/// execute external mutable bytes instead of the frozen snapshot.
#[test]
fn ci_run_refuses_symlink_justfile_escape_after_target_changes() {
    let dir = tmpdir("ci-justfile-symlink");
    init_repo(&dir);
    git(&dir, &["config", "user.email", "ci@example.com"]);

    // An external, mutable justfile that lives OUTSIDE the repo (never
    // committed). It fails initially, then is flipped to pass AFTER PR
    // creation.
    let ext = tmpdir("ci-justfile-symlink-ext");
    let ext_justfile = ext.join("justfile");
    std::fs::write(&ext_justfile, "check:\n    exit 1\n").unwrap();

    // Commit `justfile` as a symlink pointing at the external justfile.
    git(&dir, &["checkout", "-q", "-b", "feature"]);
    std::os::unix::fs::symlink(&ext_justfile, dir.join("justfile")).unwrap();
    std::fs::write(dir.join("feature.txt"), "feat\n").unwrap();
    git(&dir, &["add", "."]);
    git(
        &dir,
        &["commit", "-q", "-m", "feature + symlinked justfile"],
    );
    git(&dir, &["checkout", "-q", "main"]);

    let (c, o, e) = forge(
        &dir,
        &[
            "forge", "pr", "create", "--source", "feature", "--base", "main", "PR1",
        ],
    );
    assert_eq!(c, 0, "pr create failed: {e} {o}");

    // The external symlink target CHANGES to a passing justfile after the PR's
    // immutable snapshot was taken.
    std::fs::write(&ext_justfile, "check:\n    echo ok\n").unwrap();

    // The hardened selector must refuse to follow the justfile symlink out of
    // the snapshot: clean error, exit nonzero, and no green CI Check recorded.
    let (cr, or_, er) = forge(&dir, &["forge", "ci", "run", "1"]);
    assert_ne!(cr, 0, "ci run must refuse a symlinked justfile: {or_} {er}");
    assert!(
        er.contains("symlink"),
        "stderr must name the justfile symlink refusal: {er}"
    );

    let event = head_event(&dir, 1);
    assert!(
        !event.contains("\"status\":\"success\""),
        "must not record a green Check from external bytes: {event}"
    );

    // Working tree + current branch unchanged, no temp worktree left behind.
    assert_eq!(git(&dir, &["rev-parse", "--abbrev-ref", "HEAD"]).1, "main");
    let (_, status, _) = git(&dir, &["status", "--porcelain"]);
    assert_eq!(status.trim(), "", "working tree must be clean: {status}");
    assert_no_ci_worktree(&dir);
}

/// F-012 fallback edge: a snapshot with no `.forge/ci.sh` AND no justfile must
/// refuse the `just check` fallback up front, so an ancestor/global justfile
/// cannot supply a green Check. The refusal is a clean error with no green CI
/// Check recorded and no temp worktree created.
#[test]
fn ci_run_fallback_refuses_when_snapshot_has_no_justfile() {
    let dir = tmpdir("ci-nojustfile");
    init_repo(&dir);
    git(&dir, &["config", "user.email", "ci@example.com"]);
    git(&dir, &["checkout", "-q", "-b", "feature"]);
    std::fs::write(dir.join("feature.txt"), "feat\n").unwrap();
    git(&dir, &["add", "."]);
    git(&dir, &["commit", "-q", "-m", "feature without a CI plan"]);
    git(&dir, &["checkout", "-q", "main"]);

    assert_eq!(
        forge(
            &dir,
            &["forge", "pr", "create", "--source", "feature", "--base", "main", "PR1"]
        )
        .0,
        0
    );
    // The source tree must carry neither a ci.sh nor a justfile.
    assert!(!dir.join(".forge").join("ci.sh").exists());
    assert!(!dir.join("justfile").exists());

    let (c, _o, e) = forge(&dir, &["forge", "ci", "run", "1"]);
    assert_ne!(
        c, 0,
        "a snapshot with no .forge/ci.sh and no justfile must refuse the fallback: {e}"
    );
    assert!(
        e.contains("no justfile"),
        "stderr must name the missing justfile: {e}"
    );

    // No green Check and no temp worktree left behind.
    let event = head_event(&dir, 1);
    assert!(
        !event.contains("\"status\":\"success\""),
        "must not record a green Check: {event}"
    );
    assert_eq!(git(&dir, &["rev-parse", "--abbrev-ref", "HEAD"]).1, "main");
    let (_, status, _) = git(&dir, &["status", "--porcelain"]);
    assert_eq!(status.trim(), "", "working tree must be clean: {status}");
    assert_no_ci_worktree(&dir);
}

/// F-013 regression: a checkout smudge/attribute filter must NOT replace the CI
/// plan that runs. A `.gitattributes` filter rewrites `.forge/ci.sh` so that
/// the checked-out file is `exit 0` while the immutable blob is `exit 1`; a run
/// that executed the smudged file would record a green Check. The F-013 fix
/// materializes the exact immutable blob bytes, so the failing plan runs.
#[test]
fn ci_run_smudge_filter_cannot_replace_plan() {
    let dir = tmpdir("ci-smudge");
    init_repo(&dir);
    git(&dir, &["config", "user.email", "ci@example.com"]);
    // A smudge/attribute filter that rewrites `exit 1` -> `exit 0` on checkout.
    git(&dir, &["config", "filter.evil.clean", "cat"]);
    git(
        &dir,
        &["config", "filter.evil.smudge", "sed 's/exit 1/exit 0/'"],
    );
    git(&dir, &["checkout", "-q", "-b", "feature"]);
    std::fs::create_dir_all(dir.join(".forge")).unwrap();
    std::fs::write(dir.join(".gitattributes"), "*.sh filter=evil\n").unwrap();
    std::fs::write(dir.join(".forge").join("ci.sh"), "#!/bin/bash\nexit 1\n").unwrap();
    std::fs::write(dir.join("feature.txt"), "feat\n").unwrap();
    git(&dir, &["add", "."]);
    git(
        &dir,
        &["commit", "-q", "-m", "feature + smudge-rewritten ci plan"],
    );
    git(&dir, &["checkout", "-q", "main"]);

    assert_eq!(
        forge(
            &dir,
            &["forge", "pr", "create", "--source", "feature", "--base", "main", "PR1"]
        )
        .0,
        0
    );

    // The blob is `exit 1`, but the checkout smudge rewrites it to `exit 0`.
    // A run that executed the smudged file would record a green Check; the
    // F-013 fix materializes the exact blob bytes so the FAILING plan runs.
    let (c, o, e) = forge(&dir, &["forge", "ci", "run", "1"]);
    assert_ne!(
        c, 0,
        "the immutable plan (exit 1) must fail, not the smudged one: {e} {o}"
    );
    let event = head_event(&dir, 1);
    assert!(event.contains("\"status\":\"failed\""), "status: {event}");
    assert!(event.contains("\"plan\":\".forge/ci.sh\""), "plan: {event}");

    // The temp CI worktree must be removed, and the developer's working tree is
    // clean.
    assert_no_ci_worktree(&dir);
    let (_, status, _) = git(&dir, &["status", "--porcelain"]);
    assert_eq!(status.trim(), "", "working tree must be clean: {status}");
}

/// F-014 regression: the CI temp worktree must be created WITHOUT running
/// repository-relative hooks (post-checkout) that a snapshot's environment
/// could supply, so CI cannot hang or accumulate output before the deadline.
/// A post-checkout hook that writes a marker proves whether it ran; the F-014
/// fix (`core.hooksPath=/dev/null` on the CI worktree add) keeps it from
/// running at all.
#[test]
fn ci_run_does_not_run_post_checkout_hook() {
    let dir = tmpdir("ci-hook");
    init_repo(&dir);
    git(&dir, &["config", "user.email", "ci@example.com"]);

    git(&dir, &["checkout", "-q", "-b", "feature"]);
    std::fs::create_dir_all(dir.join(".forge")).unwrap();
    std::fs::write(dir.join(".forge").join("ci.sh"), "#!/bin/bash\nexit 0\n").unwrap();
    std::fs::write(dir.join("feature.txt"), "feat\n").unwrap();
    git(&dir, &["add", "."]);
    git(&dir, &["commit", "-q", "-m", "feature + passing ci plan"]);
    git(&dir, &["checkout", "-q", "main"]);

    // A post-checkout hook (in the repository's git dir) that a hostile
    // snapshot's environment could arrange to hang or spam output. It writes a
    // marker to prove whether git ran it during the CI worktree checkout. It is
    // installed AFTER the test's own branch checkouts, so the only checkout left
    // that could invoke it is the CI temp worktree add.
    std::fs::create_dir_all(dir.join(".git").join("hooks")).unwrap();
    let marker = dir.join("hook-ran.txt");
    std::fs::write(
        dir.join(".git").join("hooks").join("post-checkout"),
        format!("#!/bin/sh\necho ran > '{}'\n", marker.display()),
    )
    .unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(
        dir.join(".git").join("hooks").join("post-checkout"),
        std::fs::Permissions::from_mode(0o755),
    )
    .unwrap();

    assert_eq!(
        forge(
            &dir,
            &["forge", "pr", "create", "--source", "feature", "--base", "main", "PR1"]
        )
        .0,
        0
    );

    // The CI worktree add must not run the post-checkout hook (F-014).
    let (c, o, e) = forge(&dir, &["forge", "ci", "run", "1"]);
    assert_eq!(c, 0, "ci run must succeed: {e} {o}");
    assert!(o.contains("passed"), "ci run output: {o}");
    assert!(
        !marker.exists(),
        "post-checkout hook must NOT run during CI worktree creation"
    );

    assert_no_ci_worktree(&dir);
    let (_, status, _) = git(&dir, &["status", "--porcelain"]);
    assert_eq!(status.trim(), "", "working tree must be clean: {status}");
}

/// F-015 regression: on timeout the CI plan must terminate the COMPLETE process
/// group, so a background descendant whose leader exits cannot survive the
/// bounded deadline. The plan spawns `sleep 60` in the background and records
/// its pid; F-015 kills+reaps the whole group so the descendant is gone after
/// `ci run` returns, and the run records a failed Check + cleans the worktree.
#[test]
fn ci_run_kills_background_descendant_on_timeout() {
    let dir = tmpdir("ci-desc");
    init_repo(&dir);
    git(&dir, &["config", "user.email", "ci@example.com"]);
    git(&dir, &["checkout", "-q", "-b", "feature"]);
    std::fs::create_dir_all(dir.join(".forge")).unwrap();
    // The descendant pid is recorded OUTSIDE the repo so it does not dirty the
    // developer's working tree after the run.
    let pid_dir = tmpdir("ci-desc-pid");
    let desc_file = pid_dir.join("desc.pid");
    std::fs::write(
        dir.join(".forge").join("ci.sh"),
        format!(
            "#!/bin/bash\nsh -c 'sleep 60 & echo $! > \"{}\"; wait'\n",
            desc_file.display()
        ),
    )
    .unwrap();
    std::fs::write(dir.join("feature.txt"), "feat\n").unwrap();
    git(&dir, &["add", "."]);
    git(
        &dir,
        &["commit", "-q", "-m", "feature + background-descendant plan"],
    );
    git(&dir, &["checkout", "-q", "main"]);

    assert_eq!(
        forge(
            &dir,
            &["forge", "pr", "create", "--source", "feature", "--base", "main", "PR1"]
        )
        .0,
        0
    );

    let t0 = std::time::SystemTime::now();
    let (c, _o, e) = forge_with_env(
        &dir,
        &["forge", "ci", "run", "1"],
        &[("GIT_FORGE_CI_TIMEOUT", "1")],
    );
    let elapsed = t0.elapsed().unwrap().as_secs();
    assert_ne!(
        c, 0,
        "nonterminating descendant plan must exit nonzero: {e}"
    );
    assert!(
        elapsed < 30,
        "ci run must return within the bounded deadline (took {elapsed}s)"
    );

    let pid: u32 = std::fs::read_to_string(&desc_file)
        .unwrap()
        .trim()
        .parse()
        .expect("descendant pid must have been written by the plan");
    // The descendant must be reaped within a short window after the group kill.
    let mut gone = false;
    for _ in 0..100 {
        if !process_alive(pid) {
            gone = true;
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    assert!(
        gone,
        "background descendant pid {pid} survived the CI deadline (F-015)"
    );

    let event = head_event(&dir, 1);
    assert!(event.contains("\"status\":\"failed\""), "status: {event}");

    assert_no_ci_worktree(&dir);
    let (_, status, _) = git(&dir, &["status", "--porcelain"]);
    assert_eq!(status.trim(), "", "working tree must be clean: {status}");
}

/// F-011 regression: an extreme `GIT_FORGE_CI_TIMEOUT` (e.g. `u64::MAX`) must
/// not overflow the deadline computation and panic after the child/worktree are
/// created (which would skip the kill/reap, the failed-Check append and the
/// cleanup). The configured deadline is bounded/clamped, so a passing plan
/// still records a success Check and the temp worktree is cleaned.
#[test]
fn ci_run_extreme_timeout_does_not_panic() {
    let dir = tmpdir("ci-extreme-timeout");
    init_repo(&dir);
    git(&dir, &["config", "user.email", "ci@example.com"]);
    git(&dir, &["checkout", "-q", "-b", "feature"]);
    std::fs::create_dir_all(dir.join(".forge")).unwrap();
    std::fs::write(dir.join(".forge").join("ci.sh"), "#!/bin/bash\nexit 0\n").unwrap();
    std::fs::write(dir.join("feature.txt"), "feat\n").unwrap();
    git(&dir, &["add", "."]);
    git(&dir, &["commit", "-q", "-m", "feature + passing ci plan"]);
    git(&dir, &["checkout", "-q", "main"]);

    assert_eq!(
        forge(
            &dir,
            &["forge", "pr", "create", "--source", "feature", "--base", "main", "PR1"]
        )
        .0,
        0
    );

    // u64::MAX as the configured timeout would overflow the deadline
    // computation in the naive `Instant::now() + Duration` and panic after the
    // child/worktree are created, skipping the Check append and cleanup (F-011).
    let (c, o, e) = forge_with_env(
        &dir,
        &["forge", "ci", "run", "1"],
        &[("GIT_FORGE_CI_TIMEOUT", "18446744073709551615")],
    );
    assert_eq!(
        c, 0,
        "extreme timeout must not panic and must still succeed: {e} {o}"
    );
    assert!(o.contains("passed"), "ci run output: {o}");

    let event = head_event(&dir, 1);
    assert!(event.contains("\"status\":\"success\""), "status: {event}");

    assert_no_ci_worktree(&dir);
    let (_, status, _) = git(&dir, &["status", "--porcelain"]);
    assert_eq!(status.trim(), "", "working tree must be clean: {status}");
}

/// F-016 regression: unbounded output capture must not be silently restored.
/// A plan writes a deterministic marker whenever its stdout/stderr are a PIPE
/// (i.e. being captured, as with `Command::output()`) rather than redirected to
/// `/dev/null` (the bounded impl). Under the bounded impl the marker is never
/// written; if `Command::output()` unbounded capture is restored, the marker is
/// written and this test fails — deterministically, without needing an OOM.
#[test]
fn ci_run_detects_unbounded_output_capture() {
    let dir = tmpdir("ci-capture");
    init_repo(&dir);
    git(&dir, &["config", "user.email", "ci@example.com"]);
    git(&dir, &["checkout", "-q", "-b", "feature"]);
    std::fs::create_dir_all(dir.join(".forge")).unwrap();
    let marker = dir.join("unbounded-capture-marker.txt");
    std::fs::write(
        dir.join(".forge").join("ci.sh"),
        format!(
            "#!/bin/bash\n\
             if [ -p /dev/stdout ] || [ -p /dev/stderr ]; then\n\
             \x20 echo 'unbounded output capture detected' > \"{marker}\"\n\
             fi\n\
             echo 'noise on stdout'\n\
             echo 'noise on stderr' >&2\n\
             exit 1\n",
            marker = marker.display()
        ),
    )
    .unwrap();
    std::fs::write(dir.join("feature.txt"), "feat\n").unwrap();
    git(&dir, &["add", "."]);
    git(
        &dir,
        &["commit", "-q", "-m", "feature + capture detector ci plan"],
    );
    git(&dir, &["checkout", "-q", "main"]);

    assert_eq!(
        forge(
            &dir,
            &["forge", "pr", "create", "--source", "feature", "--base", "main", "PR1"]
        )
        .0,
        0
    );
    assert!(!marker.exists(), "marker must not exist before ci run");

    let (c, _o, e) = forge(&dir, &["forge", "ci", "run", "1"]);
    assert_ne!(c, 0, "failing capture-detector plan must exit nonzero: {e}");
    assert!(
        !marker.exists(),
        "the bounded impl must redirect plan output to /dev/null; a marker here \
         proves unbounded Command::output() capture was restored (F-016)"
    );

    let event = head_event(&dir, 1);
    assert!(event.contains("\"status\":\"failed\""), "status: {event}");

    assert_no_ci_worktree(&dir);
    let (_, status, _) = git(&dir, &["status", "--porcelain"]);
    assert_eq!(status.trim(), "", "working tree must be clean: {status}");
}

/// `ci run` on a nonexistent PR is a clean error (no worktree side effects).
#[test]
fn ci_run_nonexistent_pr_is_clean_error() {
    let dir = tmpdir("ci-nopr");
    init_repo(&dir);
    let (c, _o, e) = forge(&dir, &["forge", "ci", "run", "99"]);
    assert_ne!(c, 0, "ci run on a missing PR must fail");
    assert!(e.contains("PR #99 does not exist"), "stderr: {e}");
}

/// `git forge ci` dispatch surface: help lists the `run` subcommand.
#[test]
fn ci_help_lists_run_subcommand() {
    let dir = tmpdir("ci-help");
    let (c, o, e) = forge(&dir, &["forge", "ci", "--help"]);
    assert_eq!(c, 0, "ci --help failed: {e}");
    assert!(o.contains("usage: git forge ci"), "help head: {o}");
    assert!(o.contains("run"), "help missing 'run': {o}");
}

/// VAL-004: `pr create` records a pending CI Check marker (fast, non-destructive,
/// no plan executed), and a subsequent `ci run` executes the plan so the fold
/// reflects the latest status — without changing the developer's working tree
/// or current branch.
#[test]
#[allow(clippy::cognitive_complexity)]
fn pr_create_records_pending_ci_then_ci_run_reflects_latest() {
    let dir = tmpdir("ci-pending");
    init_repo(&dir);
    git(&dir, &["config", "user.email", "ci@example.com"]);
    git(&dir, &["checkout", "-q", "-b", "feature"]);
    // A PASSING plan committed in the PR's own commits. Because it exits 0,
    // `pr create` could only record `pending` (not `success`) if it did NOT
    // execute the plan.
    std::fs::create_dir_all(dir.join(".forge")).unwrap();
    std::fs::write(dir.join(".forge").join("ci.sh"), "#!/bin/bash\nexit 0\n").unwrap();
    std::fs::write(dir.join("feature.txt"), "feat\n").unwrap();
    git(&dir, &["add", "."]);
    git(&dir, &["commit", "-q", "-m", "feature + passing ci plan"]);
    git(&dir, &["checkout", "-q", "main"]);

    let before_branch = git(&dir, &["rev-parse", "--abbrev-ref", "HEAD"]).1;
    let before_main = git(&dir, &["rev-parse", "refs/heads/main"]).1;

    // `pr create` must be fast + non-destructive: it records pending only.
    let (c, o, e) = forge(
        &dir,
        &[
            "forge", "pr", "create", "--source", "feature", "--base", "main", "PR1",
        ],
    );
    assert_eq!(c, 0, "pr create must succeed: {e}");
    assert!(o.contains("PR #1 created"), "create output: {o}");

    // The PR chain now carries the pending ci.check as its LATEST event.
    let event = head_event(&dir, 1);
    assert!(event.contains("\"ci.check\""), "event kind: {event}");
    assert!(event.contains("\"status\":\"pending\""), "status: {event}");
    assert!(
        event.contains("\"actor\":\"ci@example.com\""),
        "actor: {event}"
    );

    // No plan ran: no CI temp worktree, and the working tree + current branch
    // are unchanged.
    let (wl, wl_out, _) = git(&dir, &["worktree", "list", "--porcelain"]);
    assert_eq!(wl, 0, "worktree list must succeed");
    assert!(
        !wl_out.contains("git-forge-pr1-ci"),
        "create must not touch a CI worktree: {wl_out}"
    );
    assert_eq!(
        git(&dir, &["rev-parse", "--abbrev-ref", "HEAD"]).1,
        before_branch,
        "current branch must be unchanged"
    );
    assert_eq!(
        git(&dir, &["rev-parse", "refs/heads/main"]).1,
        before_main,
        "branch tip must be unchanged"
    );
    let (_, status, _) = git(&dir, &["status", "--porcelain"]);
    assert_eq!(status.trim(), "", "working tree must be clean: {status}");

    // The fold surfaces the pending marker right after create.
    let (cs, os, es) = forge(&dir, &["forge", "pr", "show", "1"]);
    assert_eq!(cs, 0, "pr show after create failed: {es}");
    assert!(
        os.contains("ci: pending"),
        "show ci status after create: {os}"
    );

    // Now `ci run` executes the plan in the PR's own commits; the fold keeps
    // the LATEST status (success overwrites the pending marker).
    let (cr, or_, er) = forge(&dir, &["forge", "ci", "run", "1"]);
    assert_eq!(cr, 0, "ci run on a passing plan must pass: {er}");
    assert!(or_.contains("passed"), "ci run output: {or_}");
    let event = head_event(&dir, 1);
    assert!(
        event.contains("\"status\":\"success\""),
        "status after run: {event}"
    );
    let (cs, os, es) = forge(&dir, &["forge", "pr", "show", "1"]);
    assert_eq!(cs, 0, "pr show after ci run failed: {es}");
    assert!(os.contains("ci: success"), "show ci status after run: {os}");

    // Still no working-tree / current-branch change after the run.
    assert_eq!(
        git(&dir, &["rev-parse", "--abbrev-ref", "HEAD"]).1,
        before_branch,
        "current branch must be unchanged"
    );
    assert_eq!(
        git(&dir, &["rev-parse", "refs/heads/main"]).1,
        before_main,
        "branch tip must be unchanged"
    );
    let (_, status, _) = git(&dir, &["status", "--porcelain"]);
    assert_eq!(status.trim(), "", "working tree must be clean: {status}");
}

/// Make a PR with a passing `.forge/ci.sh` plan and a `feature` branch.
fn make_passing_ci_pr(dir: &PathBuf) {
    init_repo(dir);
    git(dir, &["config", "user.email", "ci@example.com"]);
    git(dir, &["checkout", "-q", "-b", "feature"]);
    std::fs::create_dir_all(dir.join(".forge")).unwrap();
    std::fs::write(dir.join(".forge").join("ci.sh"), "#!/bin/bash\nexit 0\n").unwrap();
    std::fs::write(dir.join("feature.txt"), "feat\n").unwrap();
    git(dir, &["add", "."]);
    git(dir, &["commit", "-q", "-m", "feature + passing ci plan"]);
    git(dir, &["checkout", "-q", "main"]);
    let (c, o, e) = forge(
        dir,
        &[
            "forge", "pr", "create", "--source", "feature", "--base", "main", "PR1",
        ],
    );
    assert_eq!(c, 0, "pr create failed: {e} {o}");
}

/// F-003 regression: a zero exit from `git worktree remove --force` must NOT
/// be taken as proof of cleanup when the directory survives (deterministic
/// no-op-removal). `ci run` must fail and report the leftover, never print
/// `passed`.
#[test]
fn ci_run_reports_leftover_when_worktree_remove_is_noop() {
    let dir = tmpdir("ci-noremove");
    make_passing_ci_pr(&dir);
    let shim = make_git_shim(
        "ci-noremove-shim",
        "if [ \"$1\" = \"-C\" ] && [ \"$3\" = \"worktree\" ] && [ \"$4\" = \"remove\" ]; then\n\
         \x20 exit 0\n\
         fi",
    );
    let (c, o, e) = run_forge(&dir, Some(&shim), &[], &["forge", "ci", "run", "1"]);
    assert_ne!(
        c, 0,
        "leftover worktree must fail ci run (got stdout {o:?})"
    );
    assert!(
        e.contains("temp worktree directory still exists"),
        "stderr must report the leftover directory: {e}"
    );
    assert!(
        e.contains("worktree left at"),
        "stderr must name the path: {e}"
    );
    assert!(!o.contains("passed"), "must not claim success: {o}");
    remove_leftover_ci_worktree(&dir);
    assert_no_ci_worktree(&dir);
}

/// F-002 regression: when recording the CI Check fails, the cleanup must not
/// silently discard a concurrent `worktree remove` failure — both the append
/// error AND the removal leftover path must be surfaced.
#[test]
fn ci_run_append_failure_reports_removal_failure() {
    let dir = tmpdir("ci-appendfail");
    make_passing_ci_pr(&dir);
    let shim = make_git_shim(
        "ci-appendfail-shim",
        "if [ \"$1\" = \"--git-dir\" ] && [ \"$3\" = \"update-ref\" ]; then\n\
         \x20 echo \"fatal: append disabled by test shim\" >&2\n\
         \x20 exit 1\n\
         fi\n\
         if [ \"$1\" = \"-C\" ] && [ \"$3\" = \"worktree\" ] && [ \"$4\" = \"remove\" ]; then\n\
         \x20 echo \"fatal: worktree remove disabled by test shim\" >&2\n\
         \x20 exit 1\n\
         fi",
    );
    let (c, _o, e) = run_forge(&dir, Some(&shim), &[], &["forge", "ci", "run", "1"]);
    assert_ne!(c, 0, "append failure must fail ci run");
    assert!(
        e.contains("recording the CI Check failed"),
        "append error must be reported: {e}"
    );
    assert!(
        e.contains("temp worktree removal failed"),
        "removal failure must NOT be discarded: {e}"
    );
    assert!(
        e.contains("worktree left at"),
        "removal failure must name the leftover path: {e}"
    );
    remove_leftover_ci_worktree(&dir);
    assert_no_ci_worktree(&dir);
}

/// F-004 regression: a passing `ci run` must not false-fail when an unrelated
/// registered worktree shares the owned temp-path prefix. A `.forge/ci.sh`
/// that creates a sibling `${PWD}-other` worktree and exits 0 leaves that
/// sibling registered after CI removes its own `$PWD` worktree; the
/// post-removal guard must compare each registered path EXACTLY with the owned
/// temp path, so the distinct sibling is never mistaken for the owned path (the
/// old substring `contains(tmp)` check falsely reported a leftover here).
#[test]
fn ci_run_passes_when_sibling_worktree_shares_temp_prefix() {
    let dir = tmpdir("ci-prefix-sibling");
    init_repo(&dir);
    git(&dir, &["config", "user.email", "ci@example.com"]);
    git(&dir, &["checkout", "-q", "-b", "feature"]);
    std::fs::create_dir_all(dir.join(".forge")).unwrap();
    std::fs::write(
        dir.join(".forge").join("ci.sh"),
        "#!/bin/bash\n\
         git worktree add --detach \"${PWD}-other\" HEAD\n\
         exit 0\n",
    )
    .unwrap();
    std::fs::write(dir.join("feature.txt"), "feat\n").unwrap();
    git(&dir, &["add", "."]);
    git(
        &dir,
        &["commit", "-q", "-m", "feature + prefix-sibling ci plan"],
    );
    git(&dir, &["checkout", "-q", "main"]);

    let (c, _, e) = forge(
        &dir,
        &[
            "forge", "pr", "create", "--source", "feature", "--base", "main", "PR1",
        ],
    );
    assert_eq!(c, 0, "pr create failed: {e}");

    // The sibling `${PWD}-other` shares the owned temp path as a prefix and is
    // still registered after the owned temp worktree is removed, so the guard
    // must NOT report a leftover and the passing run must exit 0.
    let (c, o, e) = forge(&dir, &["forge", "ci", "run", "1"]);
    assert_eq!(
        c, 0,
        "prefix-sibling worktree must not false-fail ci run: {e} {o}"
    );
    assert!(o.contains("passed"), "ci run output: {o}");

    // Hygiene: remove the sibling the CI plan left registered.
    remove_leftover_ci_worktree(&dir);
    assert_no_ci_worktree(&dir);
}

/// Set a repo identity in the local config so `pr merge`'s git subprocesses
/// (worktree add / merge / commit, which do not inherit the test's env) find a
/// committer — mirroring the t3_merge `init_repo` setup.
fn config_identity(dir: &PathBuf) {
    let (c, _, e) = git(dir, &["config", "user.name", "Test"]);
    assert_eq!(c, 0, "config user.name failed: {e}");
    let (c, _, e) = git(dir, &["config", "user.email", "test@example.com"]);
    assert_eq!(c, 0, "config user.email failed: {e}");
}

/// Make a feature branch off main with one commit carrying a `.forge/ci.sh`
/// plan that exits `ci_exit` (0 = passing, nonzero = failing) plus `content`,
/// then return to main. The plan is committed in the PR's OWN commits so a
/// later `ci run` validates exactly the PR.
fn make_feature_with_ci(dir: &PathBuf, branch: &str, content: &str, ci_exit: u16) {
    git(dir, &["checkout", "-q", "-b", branch]);
    std::fs::create_dir_all(dir.join(".forge")).unwrap();
    std::fs::write(
        dir.join(".forge").join("ci.sh"),
        format!("#!/bin/bash\nexit {ci_exit}\n"),
    )
    .unwrap();
    std::fs::write(dir.join("feature.txt"), content).unwrap();
    git(dir, &["add", "."]);
    git(dir, &["commit", "-q", "-m", &format!("{branch} commit")]);
    git(dir, &["checkout", "-q", "main"]);
}

/// VAL-005: an approved PR whose latest CI Check is `failed` must refuse the
/// merge (exit nonzero, no ref change) and the error must name the `ci run`
/// step.
#[test]
fn merge_refused_when_ci_check_failed() {
    let dir = tmpdir("merge-ci-failed");
    init_repo(&dir);
    config_identity(&dir);
    // A failing CI plan: `ci run` records status=failed.
    make_feature_with_ci(&dir, "feature", "feat\n", 1);
    assert_eq!(
        forge(
            &dir,
            &["forge", "pr", "create", "--source", "feature", "--base", "main", "PR1"]
        )
        .0,
        0
    );
    let (c, _o, _e) = forge(&dir, &["forge", "ci", "run", "1"]);
    assert_ne!(c, 0, "ci run on a failing plan must exit nonzero");
    assert_eq!(
        forge(&dir, &["forge", "pr", "review", "1", "--approve"]).0,
        0
    );
    // Uncheckout the base (main) so the merge reaches the CI gate.
    git(&dir, &["checkout", "-q", "feature"]);
    let base_before = ref_oid(&dir, "refs/heads/main").unwrap();
    let head_before = ref_oid(&dir, "refs/forge/prs/1/head").unwrap();
    let (c, _o, e) = forge(&dir, &["forge", "pr", "merge", "1"]);
    assert_ne!(c, 0, "approved + failed CI must refuse merge (got {e})");
    assert!(e.contains("git forge ci run 1"), "error names ci run: {e}");
    assert_eq!(
        ref_oid(&dir, "refs/heads/main").unwrap(),
        base_before,
        "base ref must be unchanged on a refused merge"
    );
    assert_eq!(
        ref_oid(&dir, "refs/forge/prs/1/head").unwrap(),
        head_before,
        "head ref must be unchanged on a refused merge"
    );
}

/// VAL-005: an approved PR whose latest CI Check is `pending` (the marker
/// `pr create` records, never overwritten by a `ci run`) must refuse the
/// merge (exit nonzero, no ref change) and the error must name the `ci run`
/// step.
#[test]
fn merge_refused_when_ci_check_pending() {
    let dir = tmpdir("merge-ci-pending");
    init_repo(&dir);
    config_identity(&dir);
    make_feature(&dir, "feature", "feat\n");
    assert_eq!(
        forge(
            &dir,
            &["forge", "pr", "create", "--source", "feature", "--base", "main", "PR1"]
        )
        .0,
        0
    );
    // No `ci run`: the chain carries the pending marker from pr create, which
    // is not success.
    assert_eq!(
        forge(&dir, &["forge", "pr", "review", "1", "--approve"]).0,
        0
    );
    git(&dir, &["checkout", "-q", "feature"]);
    let base_before = ref_oid(&dir, "refs/heads/main").unwrap();
    let head_before = ref_oid(&dir, "refs/forge/prs/1/head").unwrap();
    let (c, _o, e) = forge(&dir, &["forge", "pr", "merge", "1"]);
    assert_ne!(c, 0, "approved + pending CI must refuse merge (got {e})");
    assert!(e.contains("git forge ci run 1"), "error names ci run: {e}");
    assert_eq!(
        ref_oid(&dir, "refs/heads/main").unwrap(),
        base_before,
        "base ref must be unchanged on a refused merge"
    );
    assert_eq!(
        ref_oid(&dir, "refs/forge/prs/1/head").unwrap(),
        head_before,
        "head ref must be unchanged on a refused merge"
    );
}

/// VAL-005: an approved PR whose latest CI Check is `success` must proceed —
/// the merge command exits 0 and the base ref advances.
#[test]
fn merge_proceeds_when_ci_check_success() {
    let dir = tmpdir("merge-ci-success");
    init_repo(&dir);
    config_identity(&dir);
    make_feature_with_ci(&dir, "feature", "feat\n", 0);
    assert_eq!(
        forge(
            &dir,
            &["forge", "pr", "create", "--source", "feature", "--base", "main", "PR1"]
        )
        .0,
        0
    );
    let (c, o, e) = forge(&dir, &["forge", "ci", "run", "1"]);
    assert_eq!(c, 0, "ci run on a passing plan must pass: {e} {o}");
    assert_eq!(
        forge(&dir, &["forge", "pr", "review", "1", "--approve"]).0,
        0
    );
    git(&dir, &["checkout", "-q", "feature"]);
    let base_before = ref_oid(&dir, "refs/heads/main").unwrap();
    let (c, o, e) = forge(&dir, &["forge", "pr", "merge", "1"]);
    assert_eq!(c, 0, "approved + CI success must merge: {e} {o}");
    assert_ne!(
        ref_oid(&dir, "refs/heads/main").unwrap(),
        base_before,
        "base ref must advance on a green merge"
    );
}

/// L1 regression preserved: with no approval, the merge refuses with the
/// approval-only error even though the PR carries a pending CI Check.
#[test]
fn merge_refused_when_not_approved() {
    let dir = tmpdir("merge-not-approved");
    init_repo(&dir);
    config_identity(&dir);
    make_feature(&dir, "feature", "feat\n");
    assert_eq!(
        forge(
            &dir,
            &["forge", "pr", "create", "--source", "feature", "--base", "main", "PR1"]
        )
        .0,
        0
    );
    // No review (no approval) → the existing approval-only gate refuses.
    git(&dir, &["checkout", "-q", "feature"]);
    let base_before = ref_oid(&dir, "refs/heads/main").unwrap();
    let (c, _o, e) = forge(&dir, &["forge", "pr", "merge", "1"]);
    assert_ne!(c, 0, "unapproved merge must refuse (got {e})");
    assert!(e.contains("not approved"), "error names approval: {e}");
    assert_eq!(
        ref_oid(&dir, "refs/heads/main").unwrap(),
        base_before,
        "base ref must be unchanged on a refused merge"
    );
}

/// F-007 regression: the CI gate is checked against the PR head at command
/// start, but merge finalization used to re-read and accept whatever head was
/// current — so a concurrent `ci run` that appends a FAILED Check during the
/// pending-window barrier moved the head, and the merge would parent the
/// `pr.merge` commit to that failed-Check tip and advance the base. This holds
/// the merge in the barrier window, appends a failed Check, releases, and
/// asserts the base/head do NOT receive a merge and the CI gate is named.
#[test]
fn merge_refused_when_head_moves_to_failed_ci_during_finalize() {
    let dir = tmpdir("merge-ci-race");
    init_repo(&dir);
    config_identity(&dir);

    // A plan whose outcome is controlled by an env var: the FIRST `ci run`
    // (no env) succeeds; the CONCURRENT second `ci run` (env set) fails. The
    // plan lives in the PR's immutable source snapshot so `ci run` validates
    // exactly the PR tree.
    git(&dir, &["checkout", "-q", "-b", "feature"]);
    std::fs::create_dir_all(dir.join(".forge")).unwrap();
    std::fs::write(
        dir.join(".forge").join("ci.sh"),
        "#!/bin/bash\nif [ \"${GIT_FORGE_TEST_CI_FAIL:=0}\" = \"1\" ]; then exit 1; fi\nexit 0\n",
    )
    .unwrap();
    std::fs::write(dir.join("feature.txt"), "feat\n").unwrap();
    git(&dir, &["add", "."]);
    git(&dir, &["commit", "-q", "-m", "feature commit"]);
    git(&dir, &["checkout", "-q", "main"]);

    assert_eq!(
        forge(
            &dir,
            &["forge", "pr", "create", "--source", "feature", "--base", "main", "PR race"]
        )
        .0,
        0
    );
    let (c, o, e) = forge(&dir, &["forge", "ci", "run", "1"]);
    assert_eq!(c, 0, "first ci run must pass: {e} {o}");
    assert_eq!(
        forge(&dir, &["forge", "pr", "review", "1", "--approve"]).0,
        0
    );
    // Uncheckout the base (main) so the merge reaches the CI gate.
    git(&dir, &["checkout", "-q", "feature"]);

    let barrier = tmpdir("merge-ci-race-window");
    let base_before = ref_oid(&dir, "refs/heads/main").unwrap();

    // Spawn the merge parked in the pending-window barrier (debug only).
    let bin = env!("CARGO_BIN_EXE_git-forge");
    let mut child: std::process::Child = std::process::Command::new(bin)
        .args(["forge", "pr", "merge", "1"])
        .current_dir(&dir)
        .env("GIT_FORGE_TEST_MERGE_BARRIER", &barrier)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();

    // Wait for the ready sentinel (bounded).
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    while !barrier.join("ready").exists() {
        assert!(
            std::time::Instant::now() < deadline,
            "merge never reached the barrier window"
        );
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    assert!(
        ref_oid(&dir, "refs/forge/prs/1/result").is_some(),
        "pending result ref must exist in the barrier window"
    );

    // Concurrently append a FAILED CI Check by re-running `ci run 1` with the
    // failure env var set. This moves the PR head chain past the gate-validated
    // tip to a tip whose latest CI Check is failed.
    let out = std::process::Command::new(bin)
        .args(["forge", "ci", "run", "1"])
        .current_dir(&dir)
        .env("GIT_FORGE_TEST_CI_FAIL", "1")
        .output()
        .unwrap();
    assert_ne!(
        out.status.code().unwrap_or(-1),
        0,
        "failing ci run must exit nonzero"
    );
    let head_after = ref_oid(&dir, "refs/forge/prs/1/head").unwrap();

    // Release the barrier; the merge resumes into finalization.
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
    let mut stderr = String::new();
    {
        use std::io::Read as _;
        if let Some(mut e) = child.stderr.take() {
            let _ = e.read_to_string(&mut stderr);
        }
    }
    assert!(
        !status.success(),
        "merge must refuse when the head moved to a failed CI Check (stderr: {stderr})"
    );
    assert!(
        stderr.contains("git forge ci run 1"),
        "merge must name the CI gate: {stderr}"
    );
    assert_eq!(
        ref_oid(&dir, "refs/heads/main").unwrap(),
        base_before,
        "base ref must not receive a merge"
    );
    assert_eq!(
        ref_oid(&dir, "refs/forge/prs/1/head").unwrap(),
        head_after,
        "head ref must not receive a pr.merge commit"
    );
    assert!(
        ref_oid(&dir, "refs/forge/prs/1/result").is_none(),
        "pending result ref must be cleaned on a refused merge"
    );
}

/// Extract the `"id"` field value from a serialized event JSON string.
fn event_uuid(json: &str) -> Option<String> {
    let key = "\"id\":\"";
    let start = json.find(key)? + key.len();
    let rest = &json[start..];
    let end = rest.find('"')?;
    Some(rest[..end].to_string())
}

/// OIDs of every unreachable/dangling commit reported by `git fsck`, with the
/// locale forced to C so the diagnostic lines are stable to parse.
fn unreachable_commits(dir: &PathBuf) -> Vec<String> {
    let out = Command::new("git")
        .args(["fsck", "--unreachable", "--no-reflogs"])
        .current_dir(dir)
        .env("LC_ALL", "C")
        .env("LANG", "C")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git fsck --unreachable failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|l| {
            l.strip_prefix("unreachable commit ")
                .or_else(|| l.strip_prefix("dangling commit "))
        })
        .map(|s| s.trim().to_string())
        .collect()
}

/// The UUID of the single unreachable `pr.merge` event commit for PR `pr`.
/// Returns None when no such dangling commit exists (the finalize retry did not
/// write an unreachable first-attempt commit).
fn dangling_pr_merge_uuid(dir: &PathBuf, pr: u64) -> Option<String> {
    for oid in unreachable_commits(dir) {
        let (c, ev, _) = git(dir, &["show", &format!("{oid}:.forge/event.json")]);
        if c != 0 {
            continue;
        }
        if ev.contains("\"kind\":\"pr.merge\"") && ev.contains(&format!("\"entity_id\":{pr}")) {
            return event_uuid(&ev);
        }
    }
    None
}

/// F-008 regression: a green/approved PR #1 merge parked at the pending-window
/// barrier has its `/head` ref moved onto an INDEPENDENTLY green/approved PR #2
/// tip. The old retry loop treated ANY head move as a retryable concurrent
/// append: it re-folded PR #2's chain (approved + CI success), adopted it as
/// PR #1's new expected head, and parented PR #1's `pr.merge` onto PR #2's
/// chain while advancing PR #1's base and deleting its pending ref. The fix
/// proves the new tip is a valid first-parent extension of the gate head on PR
/// #1's chain before retrying; a cross-PR tip is a genuine transaction failure
/// → refs unchanged, pending result ref left in place.
#[test]
#[allow(clippy::cognitive_complexity)]
fn merge_refuses_when_head_moved_to_foreign_green_pr_during_finalize() {
    let dir = tmpdir("merge-cross-pr-sabotage");
    init_repo(&dir);
    config_identity(&dir);

    // PR #1: independently green + approved.
    make_feature_with_ci(&dir, "feature", "feat\n", 0);
    assert_eq!(
        forge(
            &dir,
            &["forge", "pr", "create", "--source", "feature", "--base", "main", "PR1"]
        )
        .0,
        0
    );
    assert_eq!(forge(&dir, &["forge", "ci", "run", "1"]).0, 0);
    assert_eq!(
        forge(&dir, &["forge", "pr", "review", "1", "--approve"]).0,
        0
    );

    // PR #2: independently green + approved (the foreign tip).
    make_feature_with_ci(&dir, "feature2", "feat2\n", 0);
    assert_eq!(
        forge(
            &dir,
            &["forge", "pr", "create", "--source", "feature2", "--base", "main", "PR2"]
        )
        .0,
        0
    );
    assert_eq!(forge(&dir, &["forge", "ci", "run", "2"]).0, 0);
    assert_eq!(
        forge(&dir, &["forge", "pr", "review", "2", "--approve"]).0,
        0
    );

    // Uncheckout the base (main) so the merge reaches the CI gate.
    git(&dir, &["checkout", "-q", "feature"]);

    let barrier = tmpdir("merge-cross-pr-window");
    let base_before = ref_oid(&dir, "refs/heads/main").unwrap();
    let head2 = ref_oid(&dir, "refs/forge/prs/2/head").unwrap();

    let bin = env!("CARGO_BIN_EXE_git-forge");
    let mut child = std::process::Command::new(bin)
        .args(["forge", "pr", "merge", "1"])
        .current_dir(&dir)
        .env("GIT_FORGE_TEST_MERGE_BARRIER", &barrier)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    while !barrier.join("ready").exists() {
        assert!(
            std::time::Instant::now() < deadline,
            "merge never reached the barrier window"
        );
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    assert!(
        ref_oid(&dir, "refs/forge/prs/1/result").is_some(),
        "pending result ref must exist in the barrier window"
    );

    // Sabotage: move PR #1's head to PR #2's green/approved tip.
    let (c, _, e) = git(&dir, &["update-ref", "refs/forge/prs/1/head", &head2]);
    assert_eq!(c, 0, "move PR #1 head to PR #2 tip: {e}");
    assert_eq!(
        ref_oid(&dir, "refs/forge/prs/1/head").unwrap(),
        head2,
        "PR #1 head must point at the foreign PR #2 tip"
    );

    // Release the barrier; the merge resumes into finalization.
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
    let mut stderr = String::new();
    {
        use std::io::Read as _;
        if let Some(mut e) = child.stderr.take() {
            let _ = e.read_to_string(&mut stderr);
        }
    }
    assert!(
        !status.success(),
        "merge must refuse a cross-PR head move (stderr: {stderr})"
    );
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
        "base ref must not be advanced"
    );
    assert_eq!(
        ref_oid(&dir, "refs/forge/prs/1/head").unwrap(),
        head2,
        "PR #1 head must stay at the foreign tip, not receive a pr.merge"
    );
    assert_eq!(
        ref_oid(&dir, "refs/forge/prs/2/head").unwrap(),
        head2,
        "PR #2 chain must be untouched"
    );
    assert!(
        ref_oid(&dir, "refs/forge/prs/1/result").is_some(),
        "pending result ref must be left in place on a sabotage refusal"
    );
}

/// F-009 regression: a legitimate green append moves the PR head during the
/// pending window. The first final transaction loses its head CAS; the retry
/// succeeds against the new green tip. The retry must re-parent the SAME
/// `pr.merge` event (retained UUID), not publish a freshly generated identity.
/// We locate the dangling first-attempt `pr.merge` commit (unreachable after
/// the failed CAS) and the reachable final `pr.merge` commit, and assert their
/// event UUIDs match.
#[test]
#[allow(clippy::cognitive_complexity)]
fn merge_retry_after_green_append_keeps_event_uuid() {
    let dir = tmpdir("merge-retry-uuid");
    init_repo(&dir);
    config_identity(&dir);
    make_feature_with_ci(&dir, "feature", "feat\n", 0);
    assert_eq!(
        forge(
            &dir,
            &["forge", "pr", "create", "--source", "feature", "--base", "main", "PR retry"]
        )
        .0,
        0
    );
    assert_eq!(forge(&dir, &["forge", "ci", "run", "1"]).0, 0);
    assert_eq!(
        forge(&dir, &["forge", "pr", "review", "1", "--approve"]).0,
        0
    );
    git(&dir, &["checkout", "-q", "feature"]);

    let barrier = tmpdir("merge-retry-uuid-window");
    let base_before = ref_oid(&dir, "refs/heads/main").unwrap();

    let bin = env!("CARGO_BIN_EXE_git-forge");
    let mut child = std::process::Command::new(bin)
        .args(["forge", "pr", "merge", "1"])
        .current_dir(&dir)
        .env("GIT_FORGE_TEST_MERGE_BARRIER", &barrier)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    while !barrier.join("ready").exists() {
        assert!(
            std::time::Instant::now() < deadline,
            "merge never reached the barrier window"
        );
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    assert!(
        ref_oid(&dir, "refs/forge/prs/1/result").is_some(),
        "pending result ref must exist in the barrier window"
    );

    // Legitimate green append: re-run `ci run 1` (the plan passes), which
    // appends a `ci.check` success event and moves the head past the
    // gate-validated tip as a valid first-parent extension.
    let (c, o, e) = forge(&dir, &["forge", "ci", "run", "1"]);
    assert_eq!(c, 0, "green ci run must pass: {e} {o}");
    let head_after = ref_oid(&dir, "refs/forge/prs/1/head").unwrap();

    // Release the barrier; the merge resumes into finalization and retries.
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
    let mut stderr = String::new();
    {
        use std::io::Read as _;
        if let Some(mut e) = child.stderr.take() {
            let _ = e.read_to_string(&mut stderr);
        }
    }
    assert!(
        status.success(),
        "merge must succeed after a green append (stderr: {stderr})"
    );
    assert_ne!(
        ref_oid(&dir, "refs/heads/main").unwrap(),
        base_before,
        "base ref must advance on the retried green merge"
    );
    // The successful merge advances the PR head to the `pr.merge` commit,
    // parented on the appended green tip — not left at `head_after`.
    let head_now = ref_oid(&dir, "refs/forge/prs/1/head").unwrap();
    assert_ne!(
        head_now, head_after,
        "head must advance to the pr.merge commit, not sit at the appended tip"
    );
    assert!(
        head_event(&dir, 1).contains("\"kind\":\"pr.merge\""),
        "head must carry a pr.merge event after the retried merge"
    );
    assert!(
        ref_oid(&dir, "refs/forge/prs/1/result").is_none(),
        "pending result ref must be cleaned on a successful merge"
    );

    let final_uuid =
        event_uuid(&head_event(&dir, 1)).expect("final PR #1 head must carry a pr.merge event");
    let dangling_uuid = dangling_pr_merge_uuid(&dir, 1)
        .expect("a dangling first-attempt pr.merge commit must exist");
    assert_eq!(
        dangling_uuid, final_uuid,
        "the retried pr.merge must re-parent the same event (retained UUID)"
    );
}

/// F-010 (multi-retry race): a green/approved PR #1 merge parked at the
/// pending-window barrier has a LEGITIMATE green append move its head H→A so
/// the first final transaction loses its CAS and the loop validates/adopts A.
/// Before the retry transaction, the head is force-rewritten A→B to a DIFFERENT
/// green/approved sibling under H (a non-append rewrite: B descends from H,
/// never from A). The old retry loop validated the moved candidate against the
/// IMMUTABLE initial `gate_head` (H), so it accepted B — because B reaches H —
/// advanced PR #1's base, deleted its pending ref, and parented `pr.merge` on
/// B. The fix validates each moved candidate against the CURRENT validated tip
/// (`head_expected`, = A after the first adoption); B does not extend A, so it
/// is a genuine transaction failure → base unchanged, pending result ref left
/// in place, exit nonzero.
#[test]
#[allow(clippy::cognitive_complexity)]
fn merge_refuses_sibling_rewrite_after_green_append_during_finalize() {
    let dir = tmpdir("merge-sibling-rewrite");
    init_repo(&dir);
    config_identity(&dir);
    make_feature_with_ci(&dir, "feature", "feat\n", 0);
    assert_eq!(
        forge(
            &dir,
            &["forge", "pr", "create", "--source", "feature", "--base", "main", "PR1"]
        )
        .0,
        0
    );
    assert_eq!(forge(&dir, &["forge", "ci", "run", "1"]).0, 0);
    assert_eq!(
        forge(&dir, &["forge", "pr", "review", "1", "--approve"]).0,
        0
    );
    // Uncheckout the base so the merge reaches the CI gate and parks.
    git(&dir, &["checkout", "-q", "feature"]);

    let barrier = tmpdir("merge-sibling-rewrite-window");
    let retry_barrier = tmpdir("merge-sibling-rewrite-retry");
    let base_before = ref_oid(&dir, "refs/heads/main").unwrap();
    let gate_head = ref_oid(&dir, "refs/forge/prs/1/head").unwrap();

    let bin = env!("CARGO_BIN_EXE_git-forge");
    let mut child = std::process::Command::new(bin)
        .args(["forge", "pr", "merge", "1"])
        .current_dir(&dir)
        .env("GIT_FORGE_TEST_MERGE_BARRIER", &barrier)
        .env("GIT_FORGE_TEST_MERGE_RETRY_BARRIER", &retry_barrier)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    while !barrier.join("ready").exists() {
        assert!(
            std::time::Instant::now() < deadline,
            "merge never reached the barrier window"
        );
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    assert!(
        ref_oid(&dir, "refs/forge/prs/1/result").is_some(),
        "pending result ref must exist in the barrier window"
    );

    // First loss: a LEGITIMATE green append (a passing `ci run` appends a
    // ci.check success commit) moves the head H→A.
    let (c, o, e) = forge(&dir, &["forge", "ci", "run", "1"]);
    assert_eq!(c, 0, "green ci run must pass: {e} {o}");
    let head_a = ref_oid(&dir, "refs/forge/prs/1/head").unwrap();
    assert_ne!(
        head_a, gate_head,
        "the green append must move the head to A"
    );

    // Release the first barrier; the merge loses its CAS against H, validates
    // and adopts A, then parks at the retry barrier.
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
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    while !retry_barrier.join("ready").exists() {
        assert!(
            std::time::Instant::now() < deadline,
            "merge never reached the retry window (did not adopt A)"
        );
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    assert_eq!(
        ref_oid(&dir, "refs/forge/prs/1/head").unwrap(),
        head_a,
        "the head must still be A when the merge is parked at the retry window"
    );

    // Second loss: force-rewrite A→B to a DIFFERENT green/approved sibling
    // under H. B descends from H (via a raw ci.check success + an approving
    // pr.review) and is anchored to PR #1, but it never descends from A.
    let b_ci = append_event_commit(&dir, &gate_head, &forged_ci_success_json(1));
    let b = append_event_commit(&dir, &b_ci, &forged_review_approve_json(1));
    let (c, _, e) = git(&dir, &["update-ref", "refs/forge/prs/1/head", &b]);
    assert_eq!(c, 0, "rewrite PR #1 head A->B: {e}");
    assert_eq!(
        ref_oid(&dir, "refs/forge/prs/1/head").unwrap(),
        b,
        "PR #1 head must point at the sibling rewrite B"
    );

    // Release the retry barrier; the merge retries against A, loses again to B,
    // and must refuse the non-append rewrite.
    let release_path = retry_barrier.join("release");
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
    let mut stderr = String::new();
    {
        use std::io::Read as _;
        if let Some(mut e) = child.stderr.take() {
            let _ = e.read_to_string(&mut stderr);
        }
    }
    assert!(
        !status.success(),
        "merge must refuse the A->B sibling rewrite (stderr: {stderr})"
    );
    assert!(
        stderr.contains("final transaction failed"),
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
        "base ref must not advance"
    );
    assert_eq!(
        ref_oid(&dir, "refs/forge/prs/1/head").unwrap(),
        b,
        "PR #1 head must stay at B, not receive a pr.merge"
    );
    assert!(
        ref_oid(&dir, "refs/forge/prs/1/result").is_some(),
        "pending result ref must be left in place on a non-append rewrite refusal"
    );
}

/// Run a `git` subprocess whose stdin is piped with `stdin` (may be None),
/// returning (code, stdout, stderr). Used to craft raw event commits for the
/// F-008 masquerade regressions (plumbing needs stdin for `hash-object --stdin`
/// and `mktree`).
fn run_git_in(dir: &PathBuf, args: &[&str], stdin: Option<&str>) -> (i32, String, String) {
    use std::io::Write as _;
    let mut cmd = Command::new("git");
    cmd.args(args)
        .current_dir(dir)
        .env("GIT_AUTHOR_NAME", "Test")
        .env("GIT_AUTHOR_EMAIL", "test@example.com")
        .env("GIT_COMMITTER_NAME", "Test")
        .env("GIT_COMMITTER_EMAIL", "test@example.com")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    let mut child = cmd.spawn().unwrap();
    match stdin {
        Some(input) => {
            let mut sink = child.stdin.take().unwrap();
            sink.write_all(input.as_bytes()).unwrap();
            // Drop `sink` to close stdin so the child sees EOF.
        }
        None => drop(child.stdin.take()),
    }
    let out = child.wait_with_output().unwrap();
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).trim().to_string(),
        String::from_utf8_lossy(&out.stderr).trim().to_string(),
    )
}

/// Append a raw `.forge/event.json` commit as a first-parent child of `parent`,
/// returning its OID. The event JSON is caller-supplied so a regression can
/// build a mixed/foreign masquerade chain (e.g. a PR #2 chain topped with an
/// event carrying `entity_id = 1`).
fn append_event_commit(dir: &PathBuf, parent: &str, json: &str) -> String {
    let (c, blob, e) = run_git_in(dir, &["hash-object", "-w", "--stdin"], Some(json));
    assert_eq!(c, 0, "hash-object failed: {e}");
    // `git mktree` builds one tree level; the `.forge/event.json` path needs a
    // nested tree: first the `.forge` subtree, then the root tree containing it.
    let forge_input = format!("100644 blob {blob}\tevent.json\n");
    let (c, forge_tree, e) = run_git_in(dir, &["mktree"], Some(&forge_input));
    assert_eq!(c, 0, "mktree (.forge) failed: {e}");
    let root_input = format!("040000 tree {forge_tree}\t.forge\n");
    let (c, tree, e) = run_git_in(dir, &["mktree"], Some(&root_input));
    assert_eq!(c, 0, "mktree (root) failed: {e}");
    let (c, commit, e) = run_git_in(
        dir,
        &["commit-tree", &tree, "-p", parent],
        Some("forge:event:masquerade"),
    );
    assert_eq!(c, 0, "commit-tree failed: {e}");
    commit
}

/// A valid UUID-v4-shaped id for a hand-built masquerade event.
const MASQUERADE_UUID: &str = "11111111-1111-4111-8111-111111111111";

/// Build the JSON for a synthetic `pr.comment` event on entity `pr` / id 1,
/// used to top a foreign chain so it folds to PR #1's id while retaining a PR
/// #2 snapshot / approval / CI state (the F-008 mixed-chain masquerade).
fn masquerade_event_json() -> String {
    format!(
        "{{\"v\":1,\"id\":\"{MASQUERADE_UUID}\",\"kind\":\"pr.comment\",\
         \"entity\":\"pr\",\"entity_id\":1,\"ts\":\"2026-01-01T00:00:00Z\",\
         \"actor\":\"attacker@example.com\",\"body\":{{\"body\":\"masquerade\"}}}}"
    )
}

/// JSON for a forged `ci.check` success event on entity `pr` / id `pr`, used
/// to top a forged same-id chain so it folds to a green latest CI Check.
fn forged_ci_success_json(pr: u64) -> String {
    format!(
        "{{\"v\":1,\"id\":\"22222222-2222-4222-8222-222222222222\",\
         \"kind\":\"ci.check\",\"entity\":\"pr\",\"entity_id\":{pr},\
         \"ts\":\"2026-01-01T00:00:01Z\",\"actor\":\"attacker@example.com\",\
         \"body\":{{\"status\":\"success\",\"plan\":\".forge/ci.sh\"}}}}"
    )
}

/// JSON for a forged approving `pr.review` event on entity `pr` / id `pr`.
fn forged_review_approve_json(pr: u64) -> String {
    format!(
        "{{\"v\":1,\"id\":\"33333333-3333-4333-8333-333333333333\",\
         \"kind\":\"pr.review\",\"entity\":\"pr\",\"entity_id\":{pr},\
         \"ts\":\"2026-01-01T00:00:02Z\",\"actor\":\"attacker@example.com\",\
         \"body\":{{\"decision\":\"approve\"}}}}"
    )
}

/// Build a synthetic all-PR-`pr` chain rooted at `parent` whose `pr.created`
/// is a FORGED re-commit — a verbatim copy of PR `pr`'s genuine creation event
/// published at a NEW commit, NOT the immutable `/meta` anchor. The chain is
/// topped with a status=success `ci.check` and an approving `pr.review`, so it
/// folds to a fully mergeable PR `pr` (approve + latest CI success) under the
/// old `saw_created` anchor. Returns the chain tip OID. The commit-aware anchor
/// must refuse it because its `pr.created` is not at the `/meta` commit.
fn build_forged_same_id_chain(dir: &PathBuf, parent: &str, pr: u64) -> String {
    let meta = ref_oid(dir, &format!("refs/forge/prs/{pr}/meta")).expect("PR meta ref must exist");
    let (c, genuine_created, e) = git(dir, &["show", &format!("{meta}:.forge/event.json")]);
    assert_eq!(c, 0, "read genuine pr.created event failed: {e}");
    let forged_created = append_event_commit(dir, parent, &genuine_created);
    let forged_ci = append_event_commit(dir, &forged_created, &forged_ci_success_json(pr));
    append_event_commit(dir, &forged_ci, &forged_review_approve_json(pr))
}

/// F-008 (mixed/foreign masquerade at the gate): PR #1's `/head` is rewritten
/// to a tip that folds to PR #1's id (a PR #2 chain topped with an event
/// carrying `entity_id = 1`) but is actually a foreign, independently
/// green/approved chain. The old `fold(...).pr.id == id` anchor accepted it
/// (fold overwrites `pr.id` per event), so the merge would adopt the foreign
/// snapshot and advance PR #1's base. The strengthened chain anchor must refuse
/// at the gate: base unchanged, no pending result ref, exit nonzero.
#[test]
#[allow(clippy::cognitive_complexity)]
fn merge_refuses_masquerading_foreign_chain_at_entry() {
    let dir = tmpdir("merge-masquerade-entry");
    init_repo(&dir);
    config_identity(&dir);

    // PR #1: independently green + approved.
    make_feature_with_ci(&dir, "feature", "feat\n", 0);
    assert_eq!(
        forge(
            &dir,
            &["forge", "pr", "create", "--source", "feature", "--base", "main", "PR1"]
        )
        .0,
        0
    );
    assert_eq!(forge(&dir, &["forge", "ci", "run", "1"]).0, 0);
    assert_eq!(
        forge(&dir, &["forge", "pr", "review", "1", "--approve"]).0,
        0
    );

    // PR #2: independently green + approved (the foreign snapshot source).
    make_feature_with_ci(&dir, "feature2", "feat2\n", 0);
    assert_eq!(
        forge(
            &dir,
            &["forge", "pr", "create", "--source", "feature2", "--base", "main", "PR2"]
        )
        .0,
        0
    );
    assert_eq!(forge(&dir, &["forge", "ci", "run", "2"]).0, 0);
    assert_eq!(
        forge(&dir, &["forge", "pr", "review", "2", "--approve"]).0,
        0
    );

    // Masquerade: top PR #2's green/approved chain with an event carrying
    // entity_id = 1 so the fold reports pr.id == 1 (the old anchor passes)
    // while the snapshot / approval / CI underneath stay PR #2's.
    let head2 = ref_oid(&dir, "refs/forge/prs/2/head").unwrap();
    let masq = append_event_commit(&dir, &head2, &masquerade_event_json());
    let (c, _, e) = git(&dir, &["update-ref", "refs/forge/prs/1/head", &masq]);
    assert_eq!(c, 0, "rewrite PR #1 head to masquerade: {e}");
    assert_eq!(
        ref_oid(&dir, "refs/forge/prs/1/head").unwrap(),
        masq,
        "PR #1 head must point at the masquerade tip"
    );

    // Uncheckout the base so the merge would reach the gate and merge.
    git(&dir, &["checkout", "-q", "feature"]);
    let base_before = ref_oid(&dir, "refs/heads/main").unwrap();

    let (c, _o, e) = forge(&dir, &["forge", "pr", "merge", "1"]);
    assert_ne!(
        c, 0,
        "merge must refuse a masquerading foreign chain (stderr: {e})"
    );
    assert!(
        e.contains("not anchored to PR #1"),
        "error must name the chain-anchor refusal: {e}"
    );
    assert_eq!(
        ref_oid(&dir, "refs/heads/main").unwrap(),
        base_before,
        "base ref must not advance"
    );
    assert_eq!(
        ref_oid(&dir, "refs/forge/prs/1/head").unwrap(),
        masq,
        "PR #1 head must stay at the masquerade tip"
    );
    assert!(
        ref_oid(&dir, "refs/forge/prs/1/result").is_none(),
        "no pending result ref may be created before the gate"
    );
}

/// F-008 (rewrite between the finalize read and CAS): a green/approved PR #1
/// merge parked at the pending-window barrier has its `/head` ref REWRITTEN to
/// a masquerading foreign tip (a PR #2 chain topped with an event carrying
/// `entity_id = 1`) that folds to PR #1's id. The retry must prove the moved
/// tip is a valid first-parent extension of the GATE-VALIDATED tip on PR #1's
/// chain before retrying (F-008); a rewrite is a genuine transaction failure →
/// base unchanged, pending result ref left in place, exit nonzero.
#[test]
#[allow(clippy::cognitive_complexity)]
fn merge_refuses_rewrite_to_unproven_tip_between_read_and_cas() {
    let dir = tmpdir("merge-masquerade-retry");
    init_repo(&dir);
    config_identity(&dir);

    // PR #1: independently green + approved (the merge's gate-validated head).
    make_feature_with_ci(&dir, "feature", "feat\n", 0);
    assert_eq!(
        forge(
            &dir,
            &["forge", "pr", "create", "--source", "feature", "--base", "main", "PR1"]
        )
        .0,
        0
    );
    assert_eq!(forge(&dir, &["forge", "ci", "run", "1"]).0, 0);
    assert_eq!(
        forge(&dir, &["forge", "pr", "review", "1", "--approve"]).0,
        0
    );

    // PR #2: independently green + approved (the foreign masquerade source).
    make_feature_with_ci(&dir, "feature2", "feat2\n", 0);
    assert_eq!(
        forge(
            &dir,
            &["forge", "pr", "create", "--source", "feature2", "--base", "main", "PR2"]
        )
        .0,
        0
    );
    assert_eq!(forge(&dir, &["forge", "ci", "run", "2"]).0, 0);
    assert_eq!(
        forge(&dir, &["forge", "pr", "review", "2", "--approve"]).0,
        0
    );

    // Uncheckout the base so the merge reaches the CI gate and parks.
    git(&dir, &["checkout", "-q", "feature"]);

    let barrier = tmpdir("merge-masquerade-retry-window");
    let base_before = ref_oid(&dir, "refs/heads/main").unwrap();
    let head2 = ref_oid(&dir, "refs/forge/prs/2/head").unwrap();
    let masq = append_event_commit(&dir, &head2, &masquerade_event_json());

    let bin = env!("CARGO_BIN_EXE_git-forge");
    let mut child = std::process::Command::new(bin)
        .args(["forge", "pr", "merge", "1"])
        .current_dir(&dir)
        .env("GIT_FORGE_TEST_MERGE_BARRIER", &barrier)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    while !barrier.join("ready").exists() {
        assert!(
            std::time::Instant::now() < deadline,
            "merge never reached the barrier window"
        );
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    assert!(
        ref_oid(&dir, "refs/forge/prs/1/result").is_some(),
        "pending result ref must exist in the barrier window"
    );

    // Rewrite PR #1's head to the masquerading foreign tip — an unproven tip,
    // not an append of the gate-validated PR #1 head.
    let (c, _, e) = git(&dir, &["update-ref", "refs/forge/prs/1/head", &masq]);
    assert_eq!(c, 0, "rewrite PR #1 head to masquerade: {e}");
    assert_eq!(
        ref_oid(&dir, "refs/forge/prs/1/head").unwrap(),
        masq,
        "PR #1 head must point at the masquerade tip"
    );

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
    let mut stderr = String::new();
    {
        use std::io::Read as _;
        if let Some(mut e) = child.stderr.take() {
            let _ = e.read_to_string(&mut stderr);
        }
    }
    assert!(
        !status.success(),
        "merge must refuse a rewrite to an unproven tip (stderr: {stderr})"
    );
    assert!(
        stderr.contains("final transaction failed"),
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
        "base ref must not advance"
    );
    assert_eq!(
        ref_oid(&dir, "refs/forge/prs/1/head").unwrap(),
        masq,
        "PR #1 head must stay at the masquerade tip, not receive a pr.merge"
    );
    assert_eq!(
        ref_oid(&dir, "refs/forge/prs/2/head").unwrap(),
        head2,
        "PR #2 chain must be untouched"
    );
    assert!(
        ref_oid(&dir, "refs/forge/prs/1/result").is_some(),
        "pending result ref must be left in place on a rewrite refusal"
    );
}

/// F-008 (forged same-id creation chain at the gate): PR #1's `/head` is
/// rewritten to a synthetic all-PR-#1 chain containing a FORGED `pr.created`
/// (a re-commit, not the `/meta` anchor), a `ci.check` success, and an
/// approving `pr.review`, while the genuine immutable `/source` and `/base`
/// refs remain. The old anchor accepted ANY `pr.created` with matching
/// entity/id (`saw_created`), so the gate folded the forged approval + green
/// Check and executed the genuine snapshot merge. The commit-aware anchor must
/// refuse: base unchanged, no pending result ref, exit nonzero.
#[test]
#[allow(clippy::cognitive_complexity)]
fn merge_refuses_forged_same_id_creation_chain_at_entry() {
    let dir = tmpdir("merge-forged-sameid-entry");
    init_repo(&dir);
    config_identity(&dir);

    // PR #1: an independently green + approved PR (the genuine snapshot /meta /
    // source / base refs).
    make_feature_with_ci(&dir, "feature", "feat\n", 0);
    assert_eq!(
        forge(
            &dir,
            &["forge", "pr", "create", "--source", "feature", "--base", "main", "PR1"]
        )
        .0,
        0
    );
    assert_eq!(forge(&dir, &["forge", "ci", "run", "1"]).0, 0);
    assert_eq!(
        forge(&dir, &["forge", "pr", "review", "1", "--approve"]).0,
        0
    );

    // Forge an all-PR-#1 chain rooted at PR #1's genesis (a sibling of the real
    // pr.created): re-commit the genuine creation event at a NEW commit and top
    // it with a green ci.check + approving pr.review. The genuine /source and
    // /base refs stay untouched.
    let meta = ref_oid(&dir, "refs/forge/prs/1/meta").unwrap();
    let genesis = git(&dir, &["rev-parse", &format!("{meta}^")]).1;
    let forged_tip = build_forged_same_id_chain(&dir, &genesis, 1);
    let (c, _, e) = git(&dir, &["update-ref", "refs/forge/prs/1/head", &forged_tip]);
    assert_eq!(c, 0, "rewrite PR #1 head to forged same-id chain: {e}");
    assert_eq!(
        ref_oid(&dir, "refs/forge/prs/1/head").unwrap(),
        forged_tip,
        "PR #1 head must point at the forged same-id tip"
    );

    // Uncheckout the base so the forged chain would (under the old anchor)
    // reach the merge execution and advance the base.
    git(&dir, &["checkout", "-q", "feature"]);
    let base_before = ref_oid(&dir, "refs/heads/main").unwrap();

    let (c, _o, e) = forge(&dir, &["forge", "pr", "merge", "1"]);
    assert_ne!(
        c, 0,
        "merge must refuse a forged same-id creation chain (stderr: {e})"
    );
    assert!(
        e.contains("not anchored to PR #1"),
        "error must name the chain-anchor refusal: {e}"
    );
    assert_eq!(
        ref_oid(&dir, "refs/heads/main").unwrap(),
        base_before,
        "base ref must not advance"
    );
    assert_eq!(
        ref_oid(&dir, "refs/forge/prs/1/head").unwrap(),
        forged_tip,
        "PR #1 head must stay at the forged tip"
    );
    assert!(
        ref_oid(&dir, "refs/forge/prs/1/result").is_none(),
        "no pending result ref may be created before the gate"
    );
}

/// F-008 (forged same-id creation chain rewritten during finalization): a
/// green/approved PR #1 merge parked at the pending-window barrier has its
/// `/head` ref REWRITTEN to a forged same-id chain appended ONTO the
/// gate-validated head (a forged `pr.created` re-commit plus a green
/// `ci.check` and approving `pr.review`) — a valid first-parent extension that
/// the old anchor accepted. The retry must prove the moved tip is anchored by
/// PR #1's authoritative `pr.created` before retrying (F-008); a forged /
/// replacement `pr.created` is a genuine transaction failure → base unchanged,
/// pending result ref left in place, exit nonzero.
#[test]
#[allow(clippy::cognitive_complexity)]
fn merge_refuses_forged_same_id_creation_chain_during_finalize() {
    let dir = tmpdir("merge-forged-sameid-retry");
    init_repo(&dir);
    config_identity(&dir);

    // PR #1: an independently green + approved PR (the merge's gate-validated
    // head).
    make_feature_with_ci(&dir, "feature", "feat\n", 0);
    assert_eq!(
        forge(
            &dir,
            &["forge", "pr", "create", "--source", "feature", "--base", "main", "PR1"]
        )
        .0,
        0
    );
    assert_eq!(forge(&dir, &["forge", "ci", "run", "1"]).0, 0);
    assert_eq!(
        forge(&dir, &["forge", "pr", "review", "1", "--approve"]).0,
        0
    );

    // Uncheckout the base so the merge reaches the CI gate and parks.
    git(&dir, &["checkout", "-q", "feature"]);

    let barrier = tmpdir("merge-forged-sameid-retry-window");
    let base_before = ref_oid(&dir, "refs/heads/main").unwrap();
    let gate_head = ref_oid(&dir, "refs/forge/prs/1/head").unwrap();

    let bin = env!("CARGO_BIN_EXE_git-forge");
    let mut child = std::process::Command::new(bin)
        .args(["forge", "pr", "merge", "1"])
        .current_dir(&dir)
        .env("GIT_FORGE_TEST_MERGE_BARRIER", &barrier)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    while !barrier.join("ready").exists() {
        assert!(
            std::time::Instant::now() < deadline,
            "merge never reached the barrier window"
        );
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    assert!(
        ref_oid(&dir, "refs/forge/prs/1/result").is_some(),
        "pending result ref must exist in the barrier window"
    );

    // Rewrite PR #1's head to a forged same-id chain appended ONTO the
    // gate-validated head: the forged pr.created descends from gate_head (a
    // valid first-parent extension) but is a re-commit of the genuine creation
    // event at a NON-authoritative commit.
    let forged_tip = build_forged_same_id_chain(&dir, &gate_head, 1);
    let (c, _, e) = git(&dir, &["update-ref", "refs/forge/prs/1/head", &forged_tip]);
    assert_eq!(c, 0, "rewrite PR #1 head to forged same-id chain: {e}");
    assert_eq!(
        ref_oid(&dir, "refs/forge/prs/1/head").unwrap(),
        forged_tip,
        "PR #1 head must point at the forged same-id tip"
    );

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
    let mut stderr = String::new();
    {
        use std::io::Read as _;
        if let Some(mut e) = child.stderr.take() {
            let _ = e.read_to_string(&mut stderr);
        }
    }
    assert!(
        !status.success(),
        "merge must refuse a forged same-id creation chain during finalization \
         (stderr: {stderr})"
    );
    assert!(
        stderr.contains("final transaction failed"),
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
        "base ref must not advance"
    );
    assert_eq!(
        ref_oid(&dir, "refs/forge/prs/1/head").unwrap(),
        forged_tip,
        "PR #1 head must stay at the forged tip, not receive a pr.merge"
    );
    assert!(
        ref_oid(&dir, "refs/forge/prs/1/result").is_some(),
        "pending result ref must be left in place on a forged-chain refusal"
    );
}
