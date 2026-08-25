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
fn cli_events_fallback_to_store_default_when_user_email_is_empty() {
    // F-027 regression: `git config user.email ""` leaves the key present-but-
    // empty in .git/config, and libgit2's get_string returns that empty string
    // as-is (NOT NotFound). The old actor() recorded an empty-string actor,
    // violating the wire contract `"actor": "<user.email>"`. An empty/whitespace
    // value is semantically "no email": every CLI-written event must carry the
    // store default forge@localhost and commands must still succeed. The hermetic
    // forge() env guarantees no machine/user-level identity leaks in to mask it.
    let dir = tmpdir("actor-empty-email");
    Command::new("git")
        .args(["init", "-q"])
        .current_dir(&dir)
        .status()
        .unwrap();
    Command::new("git")
        .args(["config", "user.email", ""])
        .current_dir(&dir)
        .status()
        .unwrap();
    assert_eq!(
        forge(&dir, &["forge", "issue", "new", "T", "desc"]).0,
        0,
        "empty user.email repo: issue new must still succeed"
    );
    assert_eq!(forge(&dir, &["forge", "issue", "comment", "1", "hi"]).0, 0);
    assert_eq!(forge(&dir, &["forge", "issue", "close", "1"]).0, 0);
    assert_eq!(forge(&dir, &["forge", "issue", "reopen", "1"]).0, 0);
    let actors = event_chain_actors(&dir);
    assert_eq!(actors.len(), 4);
    assert!(
        actors.iter().all(|a| a == "forge@localhost"),
        "empty user.email: every event actor must fall back to the store default, got: {actors:?}"
    );
}

#[test]
fn cli_events_fallback_to_store_default_when_user_email_is_whitespace() {
    // F-005 regression: `git config user.email "   "` (whitespace-only) leaves
    // the key present-but-whitespace in .git/config, and libgit2's get_string
    // returns that whitespace string as-is (NOT NotFound). An all-whitespace
    // value is semantically "no email" exactly like an empty one: actor()
    // trims before the emptiness check, so every CLI-written event must carry
    // the store default forge@localhost and commands must still succeed. The
    // hermetic forge() env guarantees no machine/user-level identity leaks in
    // to mask it.
    let dir = tmpdir("actor-whitespace-email");
    Command::new("git")
        .args(["init", "-q"])
        .current_dir(&dir)
        .status()
        .unwrap();
    Command::new("git")
        .args(["config", "user.email", "   "])
        .current_dir(&dir)
        .status()
        .unwrap();
    assert_eq!(
        forge(&dir, &["forge", "issue", "new", "T", "desc"]).0,
        0,
        "whitespace user.email repo: issue new must still succeed"
    );
    assert_eq!(forge(&dir, &["forge", "issue", "comment", "1", "hi"]).0, 0);
    assert_eq!(forge(&dir, &["forge", "issue", "close", "1"]).0, 0);
    assert_eq!(forge(&dir, &["forge", "issue", "reopen", "1"]).0, 0);
    let actors = event_chain_actors(&dir);
    assert_eq!(actors.len(), 4);
    assert!(
        actors.iter().all(|a| a == "forge@localhost"),
        "whitespace user.email: every event actor must fall back to the store default, got: {actors:?}"
    );
}

#[test]
fn cli_events_fallback_to_store_default_when_user_email_value_is_unreadable() {
    // F-028 STAGE B regression: a user.email VALUE that cannot be read as a
    // string — here non-UTF-8 bytes, which libgit2 returns raw and git2's
    // get_string rejects with "configuration value is not valid utf8" (a
    // non-NotFound error) — is an unusable identity, not an environment
    // fault. Policy: commit commands still succeed and every event carries
    // the store default forge@localhost (the value-lookup failure falls back,
    // unlike a config-open failure which propagates). The old actor()
    // propagated this error, so `forge issue new` hard-failed. The hermetic
    // forge() env keeps any machine/user-level identity from masking the
    // fallback.
    let dir = tmpdir("actor-unreadable-email");
    Command::new("git")
        .args(["init", "-q"])
        .current_dir(&dir)
        .status()
        .unwrap();
    // A raw 0xFF byte in an unquoted value parses fine as git config (values
    // are byte strings) but can never convert to a Rust String, so git2's
    // get_string returns a non-NotFound error for the existing key.
    std::fs::write(
        dir.join(".git/config"),
        b"[user]\n\temail = bad\xff@example.com\n",
    )
    .unwrap();
    assert_eq!(
        forge(&dir, &["forge", "issue", "new", "T", "desc"]).0,
        0,
        "unreadable user.email value: issue new must still succeed"
    );
    assert_eq!(forge(&dir, &["forge", "issue", "comment", "1", "hi"]).0, 0);
    assert_eq!(forge(&dir, &["forge", "issue", "close", "1"]).0, 0);
    assert_eq!(forge(&dir, &["forge", "issue", "reopen", "1"]).0, 0);
    let actors = event_chain_actors(&dir);
    assert_eq!(actors.len(), 4);
    assert!(
        actors.iter().all(|a| a == "forge@localhost"),
        "unreadable user.email value: every event actor must fall back to the store default, got: {actors:?}"
    );
}

#[test]
fn cli_unreadable_config_fails_cleanly_without_mutation() {
    // Observable CLI guarantee: when the repo's own .git/config is unreadable
    // (here: chmod 000), a forge command must fail with a CLEAN error — a
    // non-zero exit, not a panic — and must not mutate forge state: no
    // refs/forge may be written. This test pins ONLY that subprocess-level
    // contract; it does NOT exercise actor()'s STAGE A branch (the
    // repo.config() open-failure path), which is an unexercised defensive
    // error path under pinned libgit2 1.9.6 (see the VAL-117 probe record:
    // `.specs/git-forge-contract-fix/evidence/assertions-contract-fix/
    // VAL-117-STAGE-A-probe.txt`). No claim is made that this fixture reaches
    // that branch. The hermetic forge() env keeps any machine/user-level
    // identity out of the picture.
    use std::os::unix::fs::PermissionsExt;
    let dir = tmpdir("cli-config-unreadable");
    init_repo(&dir);
    let config = dir.join(".git/config");
    let original = std::fs::metadata(&config).unwrap().permissions();
    let mut blocked = original.clone();
    blocked.set_mode(0o000);
    std::fs::set_permissions(&config, blocked).unwrap();
    let (c, _o, es) = forge(&dir, &["forge", "issue", "new", "T"]);
    // Restore so the temp dir is never left with a permission-blocked config.
    std::fs::set_permissions(&config, original).unwrap();
    assert_eq!(
        c, 1,
        "unreadable config must surface as a clean command error (1), not a panic; exit was {c}, stderr: {es}"
    );
    assert!(
        es.contains("config"),
        "the clean error should point at the config problem, stderr: {es}"
    );
    // The environment fault must not mutate forge state: no refs may be written.
    assert!(
        !dir.join(".git/refs/forge").exists(),
        "no refs/forge may be written when the config is unreadable"
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

#[test]
fn non_git_dir_reports_friendly_error_not_libgit2_internals() {
    // A bare temp dir (never `git init`) is not inside any repository. Every
    // forge command must surface a user-facing "not a git repository" error
    // (exit 1) instead of leaking libgit2's internal error string
    // ("could not find repository at '.'", "class=Repository (6)"). This is
    // the store-NotFound -> StoreError::NotAGitRepository mapping.
    let dir = tmpdir("nongit");
    for args in [
        &["forge", "issue", "list"][..],
        &["forge", "issue", "new", "T"][..],
        &["forge", "pr", "list"][..],
        &["forge", "pr", "merge", "1"][..],
    ] {
        let (c, _o, e) = forge(&dir, args);
        assert_eq!(c, 1, "{args:?} must fail with exit 1, got stderr: {e}");
        assert!(
            e.contains("not a git repository"),
            "{args:?} should report 'not a git repository', got: {e}"
        );
        assert!(
            !e.contains("class=Repository") && !e.contains("could not find repository"),
            "{args:?} must not leak libgit2 internals, got: {e}"
        );
    }
}

#[test]
fn issue_help_lists_every_subcommand() {
    // `git forge issue --help` (and a bare `issue`) must print usage naming all
    // subcommands. Covers the issue_help() block; a single call covers many lines.
    let dir = tmpdir("ihelp");
    let (c, o, e) = forge(&dir, &["forge", "issue", "--help"]);
    assert_eq!(c, 0, "issue --help failed: {e}");
    for sub in ["new", "list", "show", "comment", "close", "reopen"] {
        assert!(o.contains(sub), "issue help missing '{sub}': {o}");
    }
    assert!(o.contains("usage: git forge issue"), "help head: {o}");
    // bare `issue` (no subcommand) also prints the help, same as --help
    let (c2, o2, e2) = forge(&dir, &["forge", "issue"]);
    assert_eq!(c2, 0, "bare issue failed: {e2}");
    assert!(
        o2.contains("usage: git forge issue"),
        "bare issue help: {o2}"
    );
}

#[test]
fn top_level_dispatch_unknown_and_forge_direct() {
    // main.rs dispatch: unknown top-level command errors; the direct
    // `forge forge` form dispatches help and rejects unknown forge subcommands.
    let dir = tmpdir("dispatch");
    let (c, _o, e) = forge(&dir, &["bogus"]);
    assert_eq!(c, 1, "unknown top-level must fail");
    assert!(e.contains("unknown command 'bogus'"), "{e}");

    let (c2, o2, e2) = forge(&dir, &["forge", "--help"]);
    assert_eq!(c2, 0, "forge --help failed: {e2}");
    assert!(o2.contains("usage: git forge"), "forge help: {o2}");

    let (c3, _o3, e3) = forge(&dir, &["forge", "bogus"]);
    assert_eq!(c3, 1, "unknown forge subcommand must fail");
    assert!(e3.contains("unknown forge subcommand 'bogus'"), "{e3}");
}

#[test]
fn issue_list_empty_and_closed_states() {
    // Empty repo list -> "(no issues)"; after create+close the folded state
    // reports "(closed)". Covers the issue list bounds and found/state arms.
    let dir = tmpdir("ilist");
    init_repo(&dir);
    let (c, o, e) = forge(&dir, &["forge", "issue", "list"]);
    assert_eq!(c, 0, "empty list failed: {e}");
    assert!(o.contains("(no issues)"), "empty list output: {o}");

    assert_eq!(forge(&dir, &["forge", "issue", "new", "T"]).0, 0);
    assert_eq!(forge(&dir, &["forge", "issue", "close", "1"]).0, 0);
    let (c2, o2, e2) = forge(&dir, &["forge", "issue", "list"]);
    assert_eq!(c2, 0, "closed list failed: {e2}");
    assert!(o2.contains("closed"), "closed list output: {o2}");
    // show prints the closed state and the no-comments section
    let (c3, o3, e3) = forge(&dir, &["forge", "issue", "show", "1"]);
    assert_eq!(c3, 0, "closed show failed: {e3}");
    assert!(o3.contains("closed"), "closed show state: {o3}");
}

#[test]
fn issue_new_supports_body_and_labels_shown_in_show() {
    // `issue new <title> [description] --label <x>...` stores both the body
    // and labels, and `issue show` renders them.
    let dir = tmpdir("ilabel");
    init_repo(&dir);
    let (c, _o, e) = forge(
        &dir,
        &[
            "forge", "issue", "new", "T", "a body", "--label", "bug", "--label", "urgent",
        ],
    );
    assert_eq!(c, 0, "issue new with labels failed: {e}");
    let (cs, os, es) = forge(&dir, &["forge", "issue", "show", "1"]);
    assert_eq!(cs, 0, "issue show failed: {es}");
    assert!(os.contains("description: a body"), "body shown: {os}");
    assert!(os.contains("labels: bug, urgent"), "labels shown: {os}");
}
#[test]
fn issue_label_flag_value_validation() {
    // cli.rs:97 --label with no value; cli.rs:101 --label with empty value.
    let dir = tmpdir("labelval");
    init_repo(&dir);
    let (c, _o, e) = forge(&dir, &["forge", "issue", "new", "T", "--label"]);
    assert_ne!(c, 0, "--label with no value must fail");
    assert!(
        e.contains("--label requires a value"),
        "stderr should name the missing label value: {e}"
    );
    let (c2, _o2, e2) = forge(&dir, &["forge", "issue", "new", "T", "--label", ""]);
    assert_ne!(c2, 0, "--label with empty value must fail");
    assert!(
        e2.contains("--label requires a non-empty value"),
        "stderr should name the empty label value: {e2}"
    );
}

#[test]
fn issue_unknown_subcommand_is_clean_error() {
    // cli.rs:290 unknown issue subcommand.
    let dir = tmpdir("unksub");
    init_repo(&dir);
    let (c, _o, e) = forge(&dir, &["forge", "issue", "bogus"]);
    assert_ne!(c, 0, "unknown issue subcommand must fail");
    assert!(
        e.contains("unknown issue subcommand 'bogus'"),
        "stderr should name the unknown subcommand: {e}"
    );
}

#[test]
fn issue_nonexistent_entity_and_empty_body_are_clean_errors() {
    // store.rs:185 issue show nonexistent; store.rs:225 empty comment body;
    // store.rs:230 comment on nonexistent; store.rs:248 close/reopen nonexistent.
    let dir = tmpdir("missing");
    init_repo(&dir);
    let (c1, _o1, e1) = forge(&dir, &["forge", "issue", "show", "999"]);
    assert_ne!(c1, 0, "show nonexistent must fail");
    assert!(e1.contains("issue #999 does not exist"), "show: {e1}");

    let (c2, _o2, e2) = forge(&dir, &["forge", "issue", "comment", "1", "   "]);
    assert_ne!(c2, 0, "whitespace-only comment body must fail");
    assert!(
        e2.contains("comment body must be non-empty"),
        "comment: {e2}"
    );

    let (c3, _o3, e3) = forge(&dir, &["forge", "issue", "comment", "999", "x"]);
    assert_ne!(c3, 0, "comment on nonexistent must fail");
    assert!(
        e3.contains("issue #999 does not exist"),
        "comment 999: {e3}"
    );

    let (c4, _o4, e4) = forge(&dir, &["forge", "issue", "close", "999"]);
    assert_ne!(c4, 0, "close nonexistent must fail");
    assert!(e4.contains("issue #999 does not exist"), "close 999: {e4}");

    let (c5, _o5, e5) = forge(&dir, &["forge", "issue", "reopen", "999"]);
    assert_ne!(c5, 0, "reopen nonexistent must fail");
    assert!(e5.contains("issue #999 does not exist"), "reopen 999: {e5}");
}
#[test]
fn git_pr_wrapper_dispatches_to_pr() {
    // main.rs:77 run_wrapper: argv[0] basename `git-pr` must route to the PR
    // command surface (run_pr), not the issue surface.
    let dir = tmpdir("wrappr");
    init_repo(&dir);
    let bindir = dir.join(".gpralias");
    std::fs::create_dir_all(&bindir).unwrap();
    let bin = env!("CARGO_BIN_EXE_git-forge");
    std::fs::copy(bin, bindir.join("git-pr")).unwrap();
    let path = format!(
        "{}:{}",
        bindir.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    // `git pr list` in an empty repo: the PR surface's "(no pull requests)".
    let out = Command::new("git")
        .args(["pr", "list"])
        .current_dir(&dir)
        .env("PATH", &path)
        .output()
        .unwrap();
    let code = out.status.code().unwrap_or(-1);
    assert_eq!(
        code,
        0,
        "git pr list failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("no pull requests"),
        "git-pr wrapper must route to the PR surface, got: {stdout}"
    );
}
