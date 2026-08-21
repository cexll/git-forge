//! t1b issue CLI integration tests. Run the built `git-forge` binary as a
//! subprocess inside an isolated temp git repository (the store opens ".").

use std::path::PathBuf;
use std::process::Command;

fn tmpdir(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!(
        "gf-t1b-{tag}-{}-{:?}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .subsec_nanos()
    ));
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn init_repo(dir: &PathBuf) {
    // git init needs identity
    Command::new("git")
        .args(["init", "-q"])
        .current_dir(dir)
        .status()
        .unwrap();
    Command::new("git")
        .args(["config", "user.email", "test@example.com"])
        .current_dir(dir)
        .status()
        .unwrap();
    Command::new("git")
        .args(["config", "user.name", "Test"])
        .current_dir(dir)
        .status()
        .unwrap();
}

/// Run the git-forge binary in `dir`. `args` are the full args after the binary.
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

#[test]
fn issue_new_list_show_roundtrip() {
    let dir = tmpdir("roundtrip");
    init_repo(&dir);
    let (c1, o1, e1) = forge(&dir, &["forge", "issue", "new", "My title"]);
    assert_eq!(c1, 0, "new failed: {e1}");
    assert!(o1.contains("#1"), "new output: {o1}");

    let (cl, ol, el) = forge(&dir, &["forge", "issue", "list"]);
    assert_eq!(cl, 0, "list failed: {el}");
    assert!(ol.contains("#1"), "list output: {ol}");

    let (cs, os, es) = forge(&dir, &["forge", "issue", "show", "1"]);
    assert_eq!(cs, 0, "show failed: {es}");
    assert!(os.contains("My title"), "show output: {os}");
    assert!(os.contains("open"), "issue should be open: {os}");
}

#[test]
fn issue_comments_and_close_reopen() {
    let dir = tmpdir("lifecycle");
    init_repo(&dir);
    assert_eq!(forge(&dir, &["forge", "issue", "new", "T"]).0, 0);
    assert_eq!(
        forge(&dir, &["forge", "issue", "comment", "1", "first comment"]).0,
        0
    );
    assert_eq!(
        forge(&dir, &["forge", "issue", "comment", "1", "second comment"]).0,
        0
    );
    assert_eq!(forge(&dir, &["forge", "issue", "close", "1"]).0, 0);
    assert_eq!(forge(&dir, &["forge", "issue", "reopen", "1"]).0, 0);

    let (c, os, _) = forge(&dir, &["forge", "issue", "show", "1"]);
    assert_eq!(c, 0);
    assert!(os.contains("open"), "after reopen should be open: {os}");
    assert!(os.contains("first comment"));
    assert!(os.contains("second comment"));
}

#[test]
fn invalid_entity_id_is_clean_error() {
    let dir = tmpdir("baddid");
    init_repo(&dir);
    let (c, _, es) = forge(&dir, &["forge", "issue", "show", "abc"]);
    assert_ne!(c, 0);
    assert!(
        es.contains("invalid entity id") || es.contains("usage"),
        "stderr: {es}"
    );
    // No panic (exit is a clean failure, not a crash signal).
    assert!(c == 1 || c == 2, "exit code {c}");
}

#[test]
fn empty_title_rejected() {
    let dir = tmpdir("emptytitle");
    init_repo(&dir);
    let (c, _, es) = forge(&dir, &["forge", "issue", "new", "   "]);
    assert_ne!(c, 0);
    assert!(es.contains("title"), "stderr: {es}");
}

#[test]
fn git_issue_wrapper_matches_forge() {
    let dir = tmpdir("wrapper");
    init_repo(&dir);
    // Real `git issue` dispatch: git finds `git-issue` on PATH. Make a bin dir
    // holding a copy of our binary named `git-issue`, so argv[0] = git-issue
    // and the wrapper path fires.
    let bindir = dir.join(".gitalias");
    std::fs::create_dir_all(&bindir).unwrap();
    let bin = env!("CARGO_BIN_EXE_git-forge");
    std::fs::copy(bin, bindir.join("git-issue")).unwrap();
    let path = format!(
        "{}:{}",
        bindir.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let out = Command::new("git")
        .args(["issue", "new", "via wrapper"])
        .current_dir(&dir)
        .env("PATH", &path)
        .output()
        .unwrap();
    let code = out.status.code().unwrap_or(-1);
    assert_eq!(
        code,
        0,
        "git issue new failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let (cl, ol, _) = forge(&dir, &["forge", "issue", "list"]);
    assert_eq!(cl, 0);
    assert!(
        ol.contains("via wrapper"),
        "wrapper-created issue appears in list: {ol}"
    );
}

#[test]
fn git_forge_discovery_on_path() {
    let dir = tmpdir("gforge");
    init_repo(&dir);
    // `git forge issue ...`: git runs `git-forge` found on PATH, passing
    // argv = ["issue", ...]. This is the primary user surface.
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
        .args(["forge", "issue", "new", "discovered"])
        .current_dir(&dir)
        .env("PATH", &path)
        .output()
        .unwrap();
    let code = out.status.code().unwrap_or(-1);
    assert_eq!(
        code,
        0,
        "git forge issue new failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let (c, os, es) = forge(&dir, &["forge", "issue", "show", "1"]);
    assert_eq!(c, 0, "show failed: {es}");
    assert!(os.contains("discovered"), "git-forge-created issue: {os}");
}

#[test]
fn concurrent_comments_both_succeed() {
    let dir = tmpdir("concurrent");
    init_repo(&dir);
    assert_eq!(forge(&dir, &["forge", "issue", "new", "T"]).0, 0);
    // Two concurrent comment processes on issue #1 — CAS retry keeps both.
    let bin = env!("CARGO_BIN_EXE_git-forge");
    let d1 = dir.clone();
    let d2 = dir.clone();
    let h1 = std::thread::spawn(move || {
        Command::new(bin)
            .args(["forge", "issue", "comment", "1", "from A"])
            .current_dir(&d1)
            .output()
            .unwrap()
    });
    let d2b = d2.clone();
    let h2 = std::thread::spawn(move || {
        Command::new(bin)
            .args(["forge", "issue", "comment", "1", "from B"])
            .current_dir(&d2b)
            .output()
            .unwrap()
    });
    let o1 = h1.join().unwrap();
    let o2 = h2.join().unwrap();
    assert_eq!(o1.status.code(), Some(0), "A comment failed");
    assert_eq!(o2.status.code(), Some(0), "B comment failed");
    let (c, os, _) = forge(&dir, &["forge", "issue", "show", "1"]);
    assert_eq!(c, 0);
    assert!(os.contains("from A"), "A comment folded: {os}");
    assert!(os.contains("from B"), "B comment folded: {os}");
}

#[test]
fn top_level_help_exits_zero_with_usage() {
    // AGENTS verification matrix: `just verify-cli` runs the built binary with
    // `--help` and requires exit 0 with usage output.
    let dir = tmpdir("help");
    let (c, o, e) = forge(&dir, &["--help"]);
    assert_eq!(c, 0, "top-level --help failed: {e}");
    assert!(o.contains("usage: git forge"), "help output: {o}");
}

#[test]
fn bare_invocation_and_help_share_usage() {
    // The usage line must not be duplicated across separate code paths.
    let dir = tmpdir("usage");
    init_repo(&dir);
    let (c0, o0, e0) = forge(&dir, &[]);
    let (c1, o1, e1) = forge(&dir, &["--help"]);
    assert_eq!(c0, 0, "bare invocation failed: {e0}");
    assert_eq!(c1, 0, "--help failed: {e1}");
    assert_eq!(o0, o1, "bare invocation usage must equal --help usage");
}
