//! t1b issue CLI integration tests. Run the built `git-forge` binary as a
//! subprocess inside an isolated temp git repository (the store opens ".").

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

// Process-local monotonic suffix: two tests in this binary may share a `tag`
// and would collide on pid+nanos alone. Same convention as t1a_store.rs.
static NEXT_TMPDIR: AtomicU64 = AtomicU64::new(0);

fn candidate_name(root: &Path, tag: &str, pid: u32, seq: u64) -> PathBuf {
    root.join(format!("gf-t1b-{tag}-{pid}-{seq}"))
}

/// Create a fresh isolated temp dir under `root`, starting the candidate scan
/// at `start_seq`. Creation is exclusive (`create_dir`): a candidate that
/// already exists — a stale dir left in /tmp by a prior run after PID reuse,
/// or a parallel test's directory — is skipped, never reopened as the test
/// repo. Mirrors t1a_store.rs's make_tmpdir convention.
fn make_tmpdir(root: &Path, tag: &str, pid: u32, start_seq: u64) -> PathBuf {
    let mut seq = start_seq;
    loop {
        let d = candidate_name(root, tag, pid, seq);
        match std::fs::create_dir(&d) {
            Ok(()) => return d,
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => seq += 1,
            Err(e) => panic!("cannot create temp dir {d:?}: {e}"),
        }
    }
}

fn tmpdir(tag: &str) -> PathBuf {
    // fetch_add gives each test a distinct candidate range within this process;
    // exclusivity across processes (and across runs) comes from create_dir.
    let start_seq = NEXT_TMPDIR.fetch_add(1, Ordering::Relaxed);
    make_tmpdir(&std::env::temp_dir(), tag, std::process::id(), start_seq)
}

#[test]
fn tmpdir_skips_stale_directory_from_prior_run() {
    // Cross-run PID-reuse regression (mirrors t1a_store.rs): a prior run may
    // leave stale candidate dirs in /tmp (this suite never cleans up).
    // make_tmpdir must skip an existing candidate and exclusively create a
    // later one, never reopen the stale repo.
    struct TempDirGuard(PathBuf);
    impl Drop for TempDirGuard {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    // Owned, exclusively-created root: the test only ever manipulates paths
    // it created itself, never global /tmp entries.
    let root = make_tmpdir(&std::env::temp_dir(), "stale-root", std::process::id(), 0);
    let _root_guard = TempDirGuard(root.clone());
    let stale = candidate_name(&root, "stale", 1, 0);
    std::fs::create_dir(&stale).unwrap();
    let d = make_tmpdir(&root, "stale", 1, 0);
    assert_ne!(
        d, stale,
        "tmpdir must not reuse an existing directory: {d:?}"
    );
    // An interrupted run may also have left later candidates (seq >= 1); the
    // invariant is "never reuse an existing directory", not "exactly seq 1".
    assert!(d.file_name().is_some());
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

/// Initialize a temp repo with ONLY user.email configured (no user.name).
/// Used to prove the actor comes from user.email alone — git2's
/// `Repository::signature()` (which the store must NOT rely on for the actor)
/// errors when user.name is absent, and the old implementation fell back to
/// forge@localhost, silently violating the wire contract.
fn init_email_only(dir: &PathBuf) {
    Command::new("git")
        .args(["init", "-q"])
        .current_dir(dir)
        .status()
        .unwrap();
    Command::new("git")
        .args(["config", "user.email", "email-only@example.com"])
        .current_dir(dir)
        .status()
        .unwrap();
}

/// Run the git-forge binary in `dir`. `args` are the full args after the binary.
///
/// The child runs under a HERMETIC config env: HOME/XDG dirs point at a fresh
/// empty directory and system config is disabled, so the only git identity the
/// process can see is the repo's own local config (this is what makes the
/// email-only and no-identity actor tests meaningful — a machine-level
/// user.name/user.email can never mask them). libgit2 may still consult the
/// account home, so HOME is SET to the clean dir, not removed.
fn forge(dir: &PathBuf, args: &[&str]) -> (i32, String, String) {
    let bin = env!("CARGO_BIN_EXE_git-forge");
    // Exclusive, owned hermetic config dir (tmpdir create_dir never reopens a
    // stale dir; each call returns its actually-created path).
    let clean = tmpdir("hermetic");
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

/// Every CLI-written event (new/comment/close) must carry the invoking repo's
/// configured `user.email` as its actor (wire contract: `"actor": "<user.email>"`).
fn event_chain_actors(dir: &PathBuf) -> Vec<String> {
    // Walk the issue #1 chain (oldest→tip) and collect each event's actor from
    // the stored wire-format JSON — the actor field lives in the event JSON, not
    // in the folded state.
    let rev = Command::new("git")
        .args(["rev-list", "refs/forge/issues/1"])
        .current_dir(dir)
        .output()
        .unwrap();
    assert_eq!(rev.status.code(), Some(0), "rev-list failed");
    let mut actors = Vec::new();
    for oid in String::from_utf8_lossy(&rev.stdout).split_whitespace() {
        let show = Command::new("git")
            .args(["show", &format!("{oid}:.forge/event.json")])
            .current_dir(dir)
            .output()
            .unwrap();
        if show.status.code() != Some(0) {
            continue; // genesis root has no event.json
        }
        let json = String::from_utf8_lossy(&show.stdout).to_string();
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
fn cli_events_carry_configured_user_email_as_actor() {
    let dir = tmpdir("actor");
    init_repo(&dir);
    // Distinct from the default fixture identity so the assertion proves the
    // actor comes from the repo config, not from a hardcoded fallback.
    let cfg = Command::new("git")
        .args(["config", "user.email", "someone@example.com"])
        .current_dir(&dir)
        .output()
        .unwrap();
    assert_eq!(cfg.status.code(), Some(0), "config user.email failed");
    assert_eq!(forge(&dir, &["forge", "issue", "new", "T"]).0, 0);
    assert_eq!(
        forge(&dir, &["forge", "issue", "comment", "1", "hello"]).0,
        0
    );
    assert_eq!(forge(&dir, &["forge", "issue", "close", "1"]).0, 0);
    assert_eq!(forge(&dir, &["forge", "issue", "reopen", "1"]).0, 0);

    let actors = event_chain_actors(&dir);
    assert_eq!(
        actors.len(),
        4,
        "four events: created + comment + close + reopen"
    );
    assert!(
        actors.iter().all(|a| a == "someone@example.com"),
        "every event actor must equal configured user.email, got: {actors:?}"
    );
}

#[test]
fn cli_events_carry_user_email_when_only_email_is_configured() {
    // F-014 regression: with ONLY user.email configured (no user.name), the
    // actor must still be that email. The old event-store actor() routed
    // through git2 Repository::signature(), which requires user.name AND
    // user.email; a name-less repo silently fell back to forge@localhost.
    // The hermetic forge() env plus init_email_only make this meaningful: no
    // machine/user-level user.name can leak in to mask the regression.
    let dir = tmpdir("actor-email-only");
    init_email_only(&dir);
    assert_eq!(
        forge(&dir, &["forge", "issue", "new", "T", "desc"]).0,
        0,
        "email-only repo: issue new must succeed"
    );
    assert_eq!(forge(&dir, &["forge", "issue", "comment", "1", "hi"]).0, 0);
    assert_eq!(forge(&dir, &["forge", "issue", "close", "1"]).0, 0);
    assert_eq!(forge(&dir, &["forge", "issue", "reopen", "1"]).0, 0);
    let actors = event_chain_actors(&dir);
    assert_eq!(actors.len(), 4);
    assert!(
        actors.iter().all(|a| a == "email-only@example.com"),
        "email-only repo: every actor must equal the config email, got: {actors:?}"
    );
}

#[test]
fn cli_events_fallback_to_store_default_when_no_identity_configured() {
    // A repo with NO user.email (and no user.name, and no leaked machine
    // identity because forge() is hermetic) must still succeed, recording the
    // documented store fallback actor. The fallback fires only when the
    // user.email key is ABSENT — the actor is the email, never user.name.
    let dir = tmpdir("actor-none");
    Command::new("git")
        .args(["init", "-q"])
        .current_dir(&dir)
        .status()
        .unwrap();
    assert_eq!(
        forge(&dir, &["forge", "issue", "new", "T"]).0,
        0,
        "no-identity repo: issue new must still succeed"
    );
    let actors = event_chain_actors(&dir);
    assert_eq!(actors.len(), 1);
    assert_eq!(
        actors[0], "forge@localhost",
        "no identity: fallback actor must be the store default"
    );
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
