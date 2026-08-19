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
