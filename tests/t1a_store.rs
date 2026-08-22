//! t1a store integration tests. Each test runs against an isolated temp repo.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

use git2::Signature;

use git_forge::event::{Event, EventKind, JsonValue};
use git_forge::store::{
    issue_ref, pr_base_ref, pr_head_ref, pr_meta_ref, pr_source_ref, BoundEventStore, EventStore,
    StoreError, COUNTER_REF,
};

// Process-local monotonic suffix: two tests in this binary may share a `tag`
// (e.g. "chain") and would collide on pid+nanos alone, racing .git/config.lock
// when both init the same temp dir in the same instant.
static NEXT_TMPDIR: AtomicU64 = AtomicU64::new(0);

fn candidate_name(root: &Path, tag: &str, pid: u32, seq: u64) -> PathBuf {
    root.join(format!("gf-t1a-{tag}-{pid}-{seq}"))
}

/// Create a fresh isolated temp dir under `root`, starting the candidate scan
/// at `start_seq`. Creation is exclusive (`create_dir`): a candidate that
/// already exists — a stale dir left in /tmp by a prior run after PID reuse,
/// or a parallel test's directory — is skipped, never reopened as the test
/// repo.
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
    // Cross-run PID-reuse regression: a prior run may leave stale candidate
    // dirs in /tmp (this suite never cleans up). make_tmpdir must skip an
    // existing candidate and exclusively create a later one, never reopen the
    // stale repo.
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

fn event(kind: EventKind, entity: &str, id: u64, actor: &str, body: &[(&str, JsonValue)]) -> Event {
    let map: HashMap<String, JsonValue> = body
        .iter()
        .map(|(k, v)| (k.to_string(), v.clone()))
        .collect();
    let numeric = entity
        .bytes()
        .fold(0u64, |acc, b| acc.wrapping_mul(31).wrapping_add(b as u64));
    Event::new_with_id(
        &format!("22222222-2222-4222-8222-{:012x}", (numeric << 8) ^ id),
        kind,
        entity,
        id,
        actor,
        map,
    )
    .expect("fixture id is uuid v4")
}

/// Read the counter's `next` value straight from the counter commit tree.
fn counter_next(store: &EventStore) -> u64 {
    let repo = store.repo();
    let tip = repo.find_reference(COUNTER_REF).unwrap().target().unwrap();
    let commit = repo.find_commit(tip).unwrap();
    let tree = repo.find_tree(commit.tree_id()).unwrap();
    let entry = tree
        .get_path(std::path::Path::new(".forge/counter.json"))
        .unwrap();
    let obj = entry.to_object(repo).unwrap();
    let blob = obj.as_blob().unwrap();
    let content = std::str::from_utf8(blob.content()).unwrap();
    let key = "\"next\":";
    let idx = content.find(key).unwrap() + key.len();
    let digits: String = content[idx..]
        .chars()
        .take_while(|c| c.is_ascii_digit())
        .collect();
    digits.parse().unwrap()
}

/// A deterministic test committer identity for bound stores. Binding is the
/// ONLY route to forge writes; the store never reads libgit2 config for
/// commits, so the test supplies the signature explicitly.
fn test_signature() -> Signature<'static> {
    git2::Signature::now("test", "test@example.com").expect("test signature")
}

/// Open (or init, for a fresh dir) the repo at `dir` and bind a committer
/// identity, yielding a write-capable [`BoundEventStore`]. The open-first
/// fallback lets one helper serve both the primary store (fresh dir → init)
/// and concurrently-opened secondary stores (existing repo → open).
fn bound(dir: &Path) -> BoundEventStore {
    let store = EventStore::open(dir)
        .or_else(|_| EventStore::init(dir))
        .expect("bound store on fresh or existing repo");
    store.bind_signature(test_signature())
}

#[test]
fn lazy_first_allocation_is_atomic() {
    let dir = tmpdir("lazy");
    let store = bound(&dir);
    let id = store.allocate_id().unwrap();
    assert_eq!(id, 1, "first allocation must be id 1");
    assert!(store.repo().find_reference(COUNTER_REF).is_ok());
    assert_eq!(counter_next(store.store()), 2);
    assert!(store.repo().find_reference(&issue_ref(1)).is_ok());
}

#[test]
fn sequential_ids_advance_counter() {
    let dir = tmpdir("seq");
    let store = bound(&dir);
    assert_eq!(store.allocate_id().unwrap(), 1);
    assert_eq!(store.allocate_id().unwrap(), 2);
    assert_eq!(store.allocate_id().unwrap(), 3);
    assert_eq!(counter_next(store.store()), 4);
    for n in 1..=3 {
        assert!(store.repo().find_reference(&issue_ref(n)).is_ok());
    }
}

#[test]
fn counter_next_entry_reads_chain_tip() {
    let dir = tmpdir("cntnext");
    let store = bound(&dir);
    // Absent counter: best-effort bound, first id is 1 (no error).
    assert_eq!(store.counter_next().unwrap(), 1);
    // After each allocation the single public entry returns the chain-tip
    // value, matching what the raw counter walk sees.
    assert_eq!(store.allocate_id().unwrap(), 1);
    assert_eq!(store.counter_next().unwrap(), 2);
    assert_eq!(counter_next(store.store()), store.counter_next().unwrap());
    assert_eq!(store.allocate_id().unwrap(), 2);
    assert_eq!(store.counter_next().unwrap(), 3);
    assert_eq!(counter_next(store.store()), store.counter_next().unwrap());
}

#[test]
fn counter_is_a_versioned_chain() {
    let dir = tmpdir("chain");
    let store = bound(&dir);
    assert_eq!(store.allocate_id().unwrap(), 1); // counter commit {v1,next:2} (root)
    assert_eq!(store.allocate_id().unwrap(), 2); // counter commit {v1,next:3}
    assert_eq!(store.allocate_id().unwrap(), 3); // counter commit {v1,next:4}

    // Walk the counter chain from tip: each commit's sole parent is the
    // previous counter commit; the newest has next=4 and the root has next=2.
    let repo = store.repo();
    let mut oid = repo.find_reference(COUNTER_REF).unwrap().target().unwrap();
    let mut nexts = Vec::new();
    loop {
        let commit = repo.find_commit(oid).unwrap();
        let tree = repo.find_tree(commit.tree_id()).unwrap();
        let entry = tree
            .get_path(std::path::Path::new(".forge/counter.json"))
            .unwrap();
        let obj = entry.to_object(repo).unwrap();
        let blob = obj.as_blob().unwrap();
        let content = std::str::from_utf8(blob.content()).unwrap();
        let key = "\"next\":";
        let idx = content.find(key).unwrap() + key.len();
        let digits: String = content[idx..]
            .chars()
            .take_while(|c| c.is_ascii_digit())
            .collect();
        let next: u64 = digits.parse().unwrap();
        nexts.push(next);

        // Wire contract: versioned counter chain. The root (next=2) has ZERO
        // parents; every successor has EXACTLY ONE — the previous counter
        // commit. Requiring these exact counts per next-value rejects a
        // disconnected chain outright.
        let parents: Vec<_> = commit.parent_ids().collect();
        let expect = if next == 2 { 0 } else { 1 };
        assert_eq!(
            parents.len(),
            expect,
            "counter commit next={next} must have {expect} parent(s), got {}",
            parents.len()
        );

        match parents.first() {
            Some(p) => oid = *p,
            None => break,
        }
    }
    assert_eq!(
        nexts,
        [4, 3, 2],
        "counter is a chain oldest→tip: next values"
    );
    assert_eq!(nexts.len(), 3, "exactly three counter commits in the chain");
}

#[test]
fn append_and_read_chain_roundtrip() {
    let dir = tmpdir("chain");
    let store = bound(&dir);
    let id = store.allocate_id().unwrap();
    let r = issue_ref(id);
    let created = event(
        EventKind::IssueCreated,
        "issue",
        id,
        "a@x",
        &[("title", JsonValue::String("T".into()))],
    );
    let comment = event(
        EventKind::IssueComment,
        "issue",
        id,
        "b@x",
        &[("body", JsonValue::String("hello".into()))],
    );
    store.append_event(&r, &created).unwrap();
    store.append_event(&r, &comment).unwrap();
    let chain = store.read_chain(&r).unwrap();
    assert_eq!(chain.len(), 2);
    assert_eq!(chain[0].kind, EventKind::IssueCreated);
    assert_eq!(chain[1].kind, EventKind::IssueComment);
    assert_eq!(
        chain[1].body.get("body").and_then(JsonValue::as_str),
        Some("hello")
    );
    assert_eq!(chain[0].id, created.id);
    assert_eq!(chain[1].id, comment.id);
}

#[test]
fn append_cas_retry_retains_uuid() {
    let dir = tmpdir("cas");
    let store = bound(&dir);
    let id = store.allocate_id().unwrap();
    let r = issue_ref(id);

    let a = event(
        EventKind::IssueComment,
        "issue",
        id,
        "a@x",
        &[("body", JsonValue::String("first".into()))],
    );
    let b = event(
        EventKind::IssueComment,
        "issue",
        id,
        "b@x",
        &[("body", JsonValue::String("second".into()))],
    );

    let dir_a = dir.clone();
    let dir_b = dir.clone();
    let a_clone = a.clone();
    let b_clone = b.clone();
    let r_a = r.clone();
    let r_b = r.clone();
    let h1 = std::thread::spawn(move || {
        let s = EventStore::open(&dir_a)
            .unwrap()
            .bind_signature(test_signature());
        s.append_event(&r_a, &a_clone).unwrap()
    });
    let h2 = std::thread::spawn(move || {
        let s = EventStore::open(&dir_b)
            .unwrap()
            .bind_signature(test_signature());
        s.append_event(&r_b, &b_clone).unwrap()
    });
    let _ = h1.join().unwrap();
    let _ = h2.join().unwrap();

    let chain = store.read_chain(&r).unwrap();
    assert_eq!(
        chain.len(),
        2,
        "both comments must be present after CAS retry"
    );
    let ids: Vec<&str> = chain.iter().map(|e| e.id.as_str()).collect();
    assert!(ids.contains(&a.id.as_str()), "uuid of a retained: {ids:?}");
    assert!(ids.contains(&b.id.as_str()), "uuid of b retained: {ids:?}");
}

#[test]
fn stale_tip_rejected_without_corruption() {
    let dir = tmpdir("stale");
    let store = bound(&dir);
    let id = store.allocate_id().unwrap();
    let r = issue_ref(id);
    let first = event(
        EventKind::IssueComment,
        "issue",
        id,
        "a@x",
        &[("body", JsonValue::String("one".into()))],
    );
    let tip1 = store.append_event(&r, &first).unwrap();

    let second = event(
        EventKind::IssueComment,
        "issue",
        id,
        "b@x",
        &[("body", JsonValue::String("two".into()))],
    );
    let s2 = EventStore::open(&dir)
        .unwrap()
        .bind_signature(test_signature());
    let comp = event(
        EventKind::IssueComment,
        "issue",
        id,
        "c@x",
        &[("body", JsonValue::String("competing".into()))],
    );
    let comp_tip = s2.append_event(&r, &comp).unwrap();
    assert_ne!(comp_tip, tip1);

    let final_tip = store.append_event(&r, &second).unwrap();
    assert_ne!(final_tip, comp_tip, "append must move the ref forward");
    let chain = store.read_chain(&r).unwrap();
    assert_eq!(chain.len(), 3);
    let bodies: Vec<&str> = chain
        .iter()
        .filter_map(|e| e.body.get("body").and_then(JsonValue::as_str))
        .collect();
    assert!(bodies.contains(&"one"));
    assert!(bodies.contains(&"two"));
    assert!(bodies.contains(&"competing"));
}

#[test]
fn concurrent_allocation_returns_distinct_ids() {
    let dir = tmpdir("concalloc");
    EventStore::init(&dir).unwrap();
    let dir_a = dir.clone();
    let dir_b = dir.clone();
    let h1 = std::thread::spawn(move || {
        EventStore::open(&dir_a)
            .unwrap()
            .bind_signature(test_signature())
            .allocate_id()
            .unwrap()
    });
    let h2 = std::thread::spawn(move || {
        EventStore::open(&dir_b)
            .unwrap()
            .bind_signature(test_signature())
            .allocate_id()
            .unwrap()
    });
    let a = h1.join().unwrap();
    let b = h2.join().unwrap();
    assert_ne!(a, b, "concurrent allocations must be distinct");
    let mut both = [a, b];
    both.sort();
    assert_eq!(both, [1, 2], "distinct sequential ids from absent counter");
    let store = EventStore::open(&dir).unwrap();
    assert_eq!(counter_next(&store), 3);
    for n in [a, b] {
        assert!(store.repo().find_reference(&issue_ref(n)).is_ok());
    }
}

#[test]
fn counter_collision_aborts_without_partial_state() {
    let dir = tmpdir("collide");
    let store = bound(&dir);
    let genesis = {
        let repo = store.repo();
        let empty = repo.treebuilder(None).unwrap().write().unwrap();
        let empty_tree = repo.find_tree(empty).unwrap();
        let sig = repo.signature().unwrap();
        repo.commit(None, &sig, &sig, "forge:genesis", &empty_tree, &[])
            .unwrap()
    };
    let r = issue_ref(1);
    store
        .repo()
        .reference(&r, genesis, false, "pre-create")
        .unwrap();
    let res = store.allocate_id();
    assert!(
        res.is_err(),
        "allocation must fail when entity ref pre-exists"
    );
    assert!(
        store.repo().find_reference(COUNTER_REF).is_err(),
        "counter must not be created on a failed transaction"
    );
}

#[test]
fn create_pr_creates_head_meta_source_base_atomically() {
    let dir = tmpdir("pralloc");
    let store = bound(&dir);
    let repo = store.repo();
    let sig = repo.signature().unwrap();
    let tree_oid = repo.treebuilder(None).unwrap().write().unwrap();
    let tree = repo.find_tree(tree_oid).unwrap();
    let base = repo.commit(None, &sig, &sig, "base", &tree, &[]).unwrap();
    let head = repo
        .commit(
            None,
            &sig,
            &sig,
            "head",
            &tree,
            &[&repo.find_commit(base).unwrap()],
        )
        .unwrap();
    repo.branch("main", &repo.find_commit(base).unwrap(), false)
        .unwrap();
    repo.branch("feature", &repo.find_commit(head).unwrap(), false)
        .unwrap();
    let merge_base = repo.merge_base(base, head).unwrap();
    assert_eq!(merge_base, base, "head descends from base; one merge base");

    let id = store
        .create_pr("PR title", "feature", "main", head, base, merge_base, "a@x")
        .unwrap();
    assert_eq!(id, 1, "first PR gets id 1");

    // head and meta point at the SAME pr.created event commit; source/base pin
    // the immutable snapshot OIDs.
    let head_tip = repo
        .find_reference(&pr_head_ref(1))
        .unwrap()
        .target()
        .unwrap();
    let meta_tip = repo
        .find_reference(&pr_meta_ref(1))
        .unwrap()
        .target()
        .unwrap();
    assert_eq!(
        head_tip, meta_tip,
        "head must equal meta (same snapshot commit)"
    );
    let head_commit = repo.find_commit(head_tip).unwrap();
    assert_eq!(
        head_commit.parent_ids().count(),
        1,
        "pr.created commit's parent is the genesis root"
    );
    // The event payload carries the snapshot fields.
    let body = git_forge::event::Event::from_json(
        std::str::from_utf8(
            head_commit
                .tree()
                .unwrap()
                .get_path(std::path::Path::new(".forge/event.json"))
                .unwrap()
                .to_object(repo)
                .unwrap()
                .as_blob()
                .unwrap()
                .content(),
        )
        .unwrap(),
    )
    .unwrap();
    assert_eq!(body.kind, EventKind::PrCreated);
    assert_eq!(body.entity_id, 1);
    assert_eq!(body.body.get("title").unwrap().as_str(), Some("PR title"));
    let source_head_s = head.to_string();
    let base_head_s = base.to_string();
    let merge_base_s = merge_base.to_string();
    assert_eq!(
        body.body.get("source_head").unwrap().as_str(),
        Some(source_head_s.as_str())
    );
    assert_eq!(
        body.body.get("base_head").unwrap().as_str(),
        Some(base_head_s.as_str())
    );
    assert_eq!(
        body.body.get("merge_base").unwrap().as_str(),
        Some(merge_base_s.as_str())
    );
    assert_eq!(
        repo.find_reference(&pr_source_ref(1)).unwrap().target(),
        Some(head)
    );
    assert_eq!(
        repo.find_reference(&pr_base_ref(1)).unwrap().target(),
        Some(base)
    );

    // Next allocation takes id 2; the counter advanced to {next:2}.
    let id2 = store
        .create_pr("second", "feature", "main", head, base, merge_base, "a@x")
        .unwrap();
    assert_eq!(id2, 2, "second PR gets id 2");
    assert!(repo.find_reference(&pr_head_ref(2)).is_ok());
}

#[test]
fn create_pr_rejects_preexisting_ref_without_touching_counter() {
    let dir = tmpdir("prreject");
    let store = bound(&dir);
    let repo = store.repo();
    let sig = repo.signature().unwrap();
    let tree_oid = repo.treebuilder(None).unwrap().write().unwrap();
    let tree = repo.find_tree(tree_oid).unwrap();
    let base = repo.commit(None, &sig, &sig, "base", &tree, &[]).unwrap();
    let head = repo
        .commit(
            None,
            &sig,
            &sig,
            "head",
            &tree,
            &[&repo.find_commit(base).unwrap()],
        )
        .unwrap();
    let merge_base = repo.merge_base(base, head).unwrap();
    // Pre-create the ref the fresh counter would target (#1) → forced collision.
    repo.reference(&pr_head_ref(1), head, false, "pre").unwrap();
    let res = store.create_pr("T", "feature", "main", head, base, merge_base, "a@x");
    assert!(res.is_err());
    assert!(matches!(res, Err(StoreError::RefExists(_))));
    assert!(
        repo.find_reference(COUNTER_REF).is_err(),
        "counter must be untouched by a failed create"
    );
    assert!(
        repo.find_reference(&pr_meta_ref(1)).is_err(),
        "no partial meta ref after failed create"
    );
    assert!(
        repo.find_reference(&pr_source_ref(1)).is_err(),
        "no partial source ref after failed create"
    );
    assert!(
        repo.find_reference(&pr_base_ref(1)).is_err(),
        "no partial base ref after failed create"
    );
    // The pre-created head ref is untouched.
    assert_eq!(
        repo.find_reference(&pr_head_ref(1)).unwrap().target(),
        Some(head)
    );
}

// ── VAL-115 regression: config corrupted AFTER open (isolated child tests) ──
//
// libgit2 1.9.6 SIGSEGVs (exit 139) when `.git/config` is replaced with
// malformed content after `Repository::open` and before a lazy `repo.config()`
// read. The fix removes every libgit2 config read from write paths: identity
// is resolved via the git binary and bound as an explicit signature BEFORE
// writes, so a post-open corruption cannot page-fault the write.
//
// These tests run in ISOLATED child processes (parent spawns
// `current_exe --ignored --exact <name>`): a pre-fix child SIGSEGVs the child
// only, never the suite. The parent asserts the child completed (exit 0) —
// exit 139 would fail the parent.

/// Child: open valid store, bind identity, CORRUPT .git/config, then write —
/// must not SIGSEGV (pre-bound signature means the FORGE commit path never
/// re-reads libgit2 config). The write may fail with a CLEAN git error (the
/// `git update-ref` subprocess itself parses the corrupt config and rejects
/// it — that is git's clean error, not a libgit2 crash), but the process must
/// never exit 139.
#[test]
#[ignore]
fn val115_child_postopen_corrupt_write_completes() {
    let dir = std::env::var("VAL115_REPO").expect("VAL115_REPO");
    let store = EventStore::open(&dir).unwrap();
    // Bind identity (the fix): explicit signature, no libgit2 config read.
    let store = store.bind_signature(test_signature());
    // Corrupt the config now — after open, before any forge write.
    std::fs::write(store.repo().path().join("config"), "[user\nemail = broken").unwrap();
    // The forge commit path (tree/blob building + commit with the pre-bound
    // signature) must not crash. The ref transaction shells out to git
    // update-ref, which itself reads config and may fail cleanly — that is
    // acceptable (clean error, no SIGSEGV). Both outcomes are fine; reaching
    // the end of this function (or returning an Err) proves no crash.
    match store.allocate_id() {
        Ok(id) => {
            assert_eq!(id, 1);
            let r = issue_ref(id);
            let ev = Event::new(EventKind::IssueCreated, "issue", 1, "a@x", HashMap::new());
            let _ = store.append_event(&r, &ev);
        }
        Err(_) => {
            // git update-ref rejected the corrupt config — a clean CLI error,
            // exactly the non-SIGSEGV outcome the regression requires.
        }
    }
}

/// Parent: run the child in its own process and require it to EXIT (not
/// SIGSEGV — exit 139). Pre-fix this failed with status 139 (signal 11).
#[test]
fn val115_postopen_corrupt_write_does_not_segv() {
    let dir = tmpdir("val115");
    let clean = tmpdir("val115-herm");
    Command::new("git")
        .args(["init", "-q", "-b", "master"])
        .current_dir(&dir)
        .status()
        .unwrap();
    Command::new("git")
        .args(["config", "user.email", "test@example.com"])
        .current_dir(&dir)
        .status()
        .unwrap();
    Command::new("git")
        .args(["config", "user.name", "Test"])
        .current_dir(&dir)
        .status()
        .unwrap();
    let exe = std::env::current_exe().unwrap();
    let out = Command::new(&exe)
        .arg("--ignored")
        .arg("--exact")
        .arg("val115_child_postopen_corrupt_write_completes")
        .env("VAL115_REPO", &dir)
        .env("HOME", &clean)
        .output()
        .unwrap();
    // Child must have COMPLETED. Exit 139 (SIGSEGV) fails this assertion.
    assert!(
        out.status.success(),
        "child must complete without SIGSEGV; status={:?} stderr={}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr)
    );
}
