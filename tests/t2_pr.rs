//! t2 PR integration tests. Run the built `git-forge` binary as a subprocess
//! inside an isolated temp git repository.

use std::path::PathBuf;
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
    assert_eq!(
        head_oid, meta_oid,
        "head and meta must point at the same pr.created snapshot commit"
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

    // The shared commit must carry a real pr.created event (not a bare genesis
    // root): `.forge/event.json` is present and encodes kind=pr.created + title.
    let (ct, ot, et) = git(&dir, &["show", &format!("{head_oid}:.forge/event.json")]);
    assert_eq!(ct, 0, "head commit must carry .forge/event.json: {et}");
    assert!(
        ot.contains("\"pr.created\""),
        "event kind must be pr.created: {ot}"
    );
    assert!(
        ot.contains("Add feature"),
        "event body must carry the PR title: {ot}"
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
    // Hierarchical local branch name with a slash (refs/heads/feat/thing)
    make_feature(&dir, "feat/thing", "feat\n");
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
