//! Store unit tests (moved from `store.rs` to keep it under the size gate).
use super::*;
use std::error::Error;

#[test]
fn map_repo_open_err_not_found_becomes_not_a_git_repo() {
    // A path that is not a repository yields ErrorCode::NotFound, which
    // must map to the user-facing NotAGitRepository.
    let e = match git2::Repository::open("/tmp/gf-no-such-repo-xyz") {
        Ok(_) => panic!("open must fail on a nonexistent path"),
        Err(e) => e,
    };
    assert!(matches!(
        map_repo_open_err(e),
        StoreError::NotAGitRepository
    ));
}

#[test]
fn store_error_display_constracts_user_facing_text() {
    assert_eq!(
        StoreError::NotAGitRepository.to_string(),
        "not a git repository (or any of the parent directories)"
    );
    assert_eq!(
        StoreError::CasConflict.to_string(),
        "ref CAS conflict — ref moved, retry"
    );
    assert_eq!(
        StoreError::Exhausted.to_string(),
        "bounded CAS retries exhausted; refs unchanged"
    );
    assert_eq!(
        StoreError::MissingEvent.to_string(),
        "entity chain has no event.json payload"
    );
    assert_eq!(
        StoreError::MissingRef.to_string(),
        "required ref does not exist"
    );
    assert_eq!(
        StoreError::RefExists("refs/forge/issues/1".into()).to_string(),
        "entity ref already exists: refs/forge/issues/1"
    );
    assert_eq!(
        StoreError::Command("boom".into()).to_string(),
        "git command failed: boom"
    );
    assert_eq!(
        StoreError::InvalidState("whatev".into()).to_string(),
        "invalid store state: whatev"
    );
}

#[test]
fn store_error_source_exposes_git_but_not_other_variants() {
    // Only the Git variant surfaces a std source; the user-facing variants
    // must report None (they are terminal, not wrapped errors).
    let ge = git2::Error::from_str("boom");
    assert!(StoreError::Git(ge).source().is_some());
    assert!(StoreError::NotAGitRepository.source().is_none());
    assert!(StoreError::CasConflict.source().is_none());
    assert!(StoreError::Exhausted.source().is_none());
    assert!(StoreError::MissingEvent.source().is_none());
    assert!(StoreError::MissingRef.source().is_none());
    assert!(StoreError::RefExists("x".into()).source().is_none());
    assert!(StoreError::Command("y".into()).source().is_none());
    assert!(StoreError::InvalidState("z".into()).source().is_none());
    // Display for the Git wrapper keeps the "git error:" prefix.
    let ge2 = git2::Error::from_str("boom");
    assert!(StoreError::Git(ge2).to_string().starts_with("git error:"));
}

#[test]
fn map_repo_open_err_non_not_found_stays_git() {
    // A git2 error whose code is not NotFound must pass through as a
    // StoreError::Git (keeping its diagnostic), not be coerced to the
    // user-facing "not a git repository" message.
    let ge = git2::Error::from_str("boom");
    assert!(matches!(map_repo_open_err(ge), StoreError::Git(_)));
}

#[test]
fn parse_counter_next_reads_and_rejects() {
    assert_eq!(parse_counter_next(r#"{"v":1,"next":7}"#).unwrap(), 7);
    assert_eq!(parse_counter_next("  {\"v\":1,\"next\":42}  ").unwrap(), 42);
    // missing "next" key
    assert!(matches!(
        parse_counter_next(r#"{"v":1}"#),
        Err(StoreError::InvalidState(_))
    ));
    // non-numeric next
    assert!(matches!(
        parse_counter_next(r#"{"v":1,"next":abc}"#),
        Err(StoreError::InvalidState(_))
    ));
}

#[test]
fn current_tip_absent_entity_is_none_not_error() {
    // An entity ref that does not exist reads as None (fresh entity), not
    // an error — this is how lazy first allocation starts.
    let d = std::env::temp_dir().join(format!(
        "gf-store-tip-{}-{:?}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .subsec_nanos()
    ));
    let _ = std::fs::remove_dir_all(&d);
    let repo = git2::Repository::init(&d).unwrap();
    let store = EventStore::from_repo(repo).unwrap();
    let tip = store.current_tip("refs/forge/issues/999").unwrap();
    assert_eq!(tip, None);
    let _ = std::fs::remove_dir_all(&d);
}

/// A bare store whose `reference-transaction` hook writes 2 MiB to stderr
/// in the `prepared` and `aborted` phases: more than any pipe buffer.
fn flooding_store(tag: &str) -> (std::path::PathBuf, EventStore) {
    let path = std::env::temp_dir().join(format!("gf-store-flood-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&path);
    EventStore::init(&path).unwrap();
    let store = EventStore::open(&path).unwrap();
    let hooks = store.repo.path().join("hooks");
    std::fs::create_dir_all(&hooks).unwrap();
    let hook = hooks.join("reference-transaction");
    std::fs::write(
        &hook,
        "#!/bin/sh\ncat >/dev/null\ncase \"$1\" in prepared|aborted) head -c 2097152 /dev/zero >&2 ;; esac\nexit 0\n",
    )
    .unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).unwrap();
    (path, store)
}

/// Run `run_update_ref_transaction` on a fresh handle in a thread, failing
/// the test instead of hanging when it does not return within 60 seconds.
fn transaction_within_deadline(
    path: &Path,
    lines: Vec<String>,
    absent: Vec<String>,
) -> Result<(), StoreError> {
    let (sender, receiver) = std::sync::mpsc::channel();
    let path = path.to_path_buf();
    std::thread::spawn(move || {
        let store = EventStore::open(&path).unwrap();
        let _ = sender.send(store.run_update_ref_transaction(&lines, &absent));
    });
    receiver
        .recv_timeout(std::time::Duration::from_secs(60))
        .expect("update-ref transaction did not finish: deadlocked on a pipe")
}

/// Every `*.lock` file under the git dir.
fn lock_files(dir: &Path) -> Vec<std::path::PathBuf> {
    let mut found = Vec::new();
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            found.extend(lock_files(&path));
        } else if path.extension().is_some_and(|e| e == "lock") {
            found.push(path);
        }
    }
    found
}

fn empty_commit(store: &EventStore) -> Oid {
    let tree = store.repo.treebuilder(None).unwrap().write().unwrap();
    let tree = store.repo.find_tree(tree).unwrap();
    let sig = Signature::now("Test", "test@example.com").unwrap();
    store
        .repo
        .commit(None, &sig, &sig, "m", &tree, &[])
        .unwrap()
}

/// A hook that floods stderr while the transaction is prepared must not
/// deadlock the step-by-step protocol: the transaction commits, and no
/// lock file remains.
#[test]
fn a_transaction_commits_when_a_hook_floods_stderr_while_prepared() {
    let (path, store) = flooding_store("commit");
    let commit = empty_commit(&store);
    let lines = vec![format!("update refs/forge/a {commit} {ZERO_OID}")];
    let outcome = transaction_within_deadline(&path, lines, vec!["refs/forge/b".into()]);
    assert!(outcome.is_ok(), "{outcome:?}");
    assert_eq!(store.current_tip("refs/forge/a").unwrap(), Some(commit));
    assert_eq!(lock_files(&path), Vec::<std::path::PathBuf>::new());
    let _ = std::fs::remove_dir_all(&path);
}

/// Error paths of the protocol under the same flooding hook: a failed
/// `prepare` (expected-value mismatch) and a failed absence check after
/// `prepare` (a symbolic ref at an expected-absent ref). Each returns an
/// error, writes no ref, leaves the symbolic ref as it was, and leaves no
/// lock file. A failed `prepare` keeps git's stderr text.
#[test]
fn a_failed_transaction_aborts_writes_nothing_and_leaves_no_lock() {
    let (path, store) = flooding_store("abort");
    let commit = empty_commit(&store);
    store
        .repo
        .reference_symbolic("refs/forge/record", "refs/forge/target", true, "m")
        .unwrap();
    let after_prepare = transaction_within_deadline(
        &path,
        vec![
            format!("update refs/forge/a {commit} {ZERO_OID}"),
            "option no-deref".into(),
            format!("update refs/forge/record {commit} {ZERO_OID}"),
        ],
        vec!["refs/forge/record".into()],
    );
    assert!(
        matches!(&after_prepare, Err(StoreError::Command(m)) if m.contains("refs/forge/record exists")),
        "{after_prepare:?}"
    );
    let failed_prepare = transaction_within_deadline(
        &path,
        vec![
            format!("update refs/forge/a {commit} {ZERO_OID}"),
            format!("update refs/forge/c {commit} {commit}"),
        ],
        vec![],
    );
    assert!(
        matches!(&failed_prepare, Err(StoreError::Command(m)) if m.contains("cannot lock ref 'refs/forge/c'")),
        "{:?}",
        failed_prepare.as_ref().map_err(|e| e
            .to_string()
            .chars()
            .rev()
            .take(200)
            .collect::<String>())
    );
    for name in ["refs/forge/a", "refs/forge/c", "refs/forge/target"] {
        assert_eq!(store.current_tip(name).unwrap(), None, "{name}");
    }
    assert_eq!(
        store
            .repo
            .find_reference("refs/forge/record")
            .unwrap()
            .symbolic_target(),
        Some("refs/forge/target")
    );
    assert_eq!(lock_files(&path), Vec::<std::path::PathBuf>::new());
    let _ = std::fs::remove_dir_all(&path);
}
