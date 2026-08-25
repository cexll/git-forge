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

// Both the pr.created and the pending ci.check payloads are asserted directly,
// so this grew past the default cognitive-complexity threshold; consistent with
// the suite's other multi-assertion tests.
#[test]
#[allow(clippy::cognitive_complexity)]
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
        .create_pr(
            "PR title",
            "feature",
            "main",
            head,
            base,
            merge_base,
            "a@x",
            None,
            &[],
        )
        .unwrap();
    assert_eq!(id, 1, "first PR gets id 1");

    // /meta stays pinned at the immutable pr.created snapshot; /head advances
    // one commit past it to the pending ci.check child (F-006 publishes both in
    // the same atomic transaction, so a failed publication changes neither).
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
    assert_ne!(
        head_tip, meta_tip,
        "head must advance past the pr.created snapshot (pending ci.check)"
    );

    // /meta is the pr.created snapshot commit: sole parent = genesis root, and
    // its event payload carries the snapshot fields.
    let meta_commit = repo.find_commit(meta_tip).unwrap();
    assert_eq!(
        meta_commit.parent_ids().count(),
        1,
        "pr.created commit's parent is the genesis root"
    );
    let meta_body = git_forge::event::Event::from_json(
        std::str::from_utf8(
            meta_commit
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
    assert_eq!(meta_body.kind, EventKind::PrCreated);
    assert_eq!(meta_body.entity_id, 1);
    assert_eq!(
        meta_body.body.get("title").unwrap().as_str(),
        Some("PR title")
    );
    let source_head_s = head.to_string();
    let base_head_s = base.to_string();
    let merge_base_s = merge_base.to_string();
    assert_eq!(
        meta_body.body.get("source_head").unwrap().as_str(),
        Some(source_head_s.as_str())
    );
    assert_eq!(
        meta_body.body.get("base_head").unwrap().as_str(),
        Some(base_head_s.as_str())
    );
    assert_eq!(
        meta_body.body.get("merge_base").unwrap().as_str(),
        Some(merge_base_s.as_str())
    );

    // /head is the pending ci.check child: sole parent is the pr.created commit
    // (/meta), and its event payload records the pending CI Check marker.
    let head_commit = repo.find_commit(head_tip).unwrap();
    let head_parents: Vec<_> = head_commit.parent_ids().collect();
    assert_eq!(head_parents.len(), 1, "ci.check commit has one parent");
    assert_eq!(
        head_parents[0], meta_tip,
        "ci.check's parent is the pr.created commit"
    );
    let head_body = git_forge::event::Event::from_json(
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
    assert_eq!(head_body.kind, EventKind::CiCheck);
    assert_eq!(head_body.entity_id, 1);
    assert_eq!(
        head_body.body.get("status").unwrap().as_str(),
        Some("pending")
    );
    assert_eq!(head_body.actor, "a@x");
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
        .create_pr(
            "second",
            "feature",
            "main",
            head,
            base,
            merge_base,
            "a@x",
            None,
            &[],
        )
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
    let res = store.create_pr(
        "T",
        "feature",
        "main",
        head,
        base,
        merge_base,
        "a@x",
        None,
        &[],
    );
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

/// F-006 regression: PR publication (counter CAS + /head + /meta + /source +
/// /base + the pending ci.check child) is ONE atomic store transaction. Inject
/// a failure on a different ref than the head (the immutable /source ref PR #1
/// would target) and prove the whole batch rolls back: the counter is untouched
/// and no PR ref is left behind, so a failed `pr create` can never strand a
/// durable PR without its pending CI Check.
#[test]
fn create_pr_failed_publication_leaves_counter_and_pr_refs_unchanged() {
    let dir = tmpdir("prfail");
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
    // Inject a publication failure: pre-create a ref the transaction would try
    // to create, forcing the single update-ref batch (counter + all four PR
    // refs) to abort atomically.
    repo.reference(&pr_source_ref(1), head, false, "pre")
        .unwrap();
    let res = store.create_pr(
        "T",
        "feature",
        "main",
        head,
        base,
        merge_base,
        "a@x",
        None,
        &[],
    );
    assert!(res.is_err());
    assert!(matches!(res, Err(StoreError::RefExists(_))));
    // The whole publication rolled back: counter untouched, no PR ref created.
    assert!(
        repo.find_reference(COUNTER_REF).is_err(),
        "counter must be untouched by a failed publication"
    );
    assert!(
        repo.find_reference(&pr_head_ref(1)).is_err(),
        "no partial head ref after failed publication"
    );
    assert!(
        repo.find_reference(&pr_meta_ref(1)).is_err(),
        "no partial meta ref after failed publication"
    );
    assert!(
        repo.find_reference(&pr_base_ref(1)).is_err(),
        "no partial base ref after failed publication"
    );
    // The pre-existing immutable snapshot ref is exactly as it was.
    assert_eq!(
        repo.find_reference(&pr_source_ref(1)).unwrap().target(),
        Some(head),
        "the pre-existing source ref must be untouched"
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

#[test]
fn read_chain_on_absent_ref_is_empty() {
    // store.rs:299 — read_chain on a ref that does not exist yields an empty
    // chain (Ok), never an error: the "no events yet" state is normal.
    let dir = tmpdir("chainabsent");
    let store = bound(&dir);
    let chain = store
        .read_chain(&issue_ref(999))
        .expect("absent ref must read as an empty chain");
    assert!(chain.is_empty(), "absent ref chain must be empty");
}

#[test]
fn read_chain_on_non_commit_ref_is_store_error() {
    // store.rs:104-106 — a ref pointing at a NON-COMMIT object (here: a blob)
    // surfaces as StoreError::Git via the From<GitError> conversion, never a
    // panic and never a silent empty chain.
    let dir = tmpdir("chainblob");
    let store = bound(&dir);
    let repo = store.repo();
    let blob_oid = repo.blob(b"not a commit").expect("write a scratch blob");
    repo.reference(&issue_ref(1), blob_oid, false, "corrupt")
        .unwrap();
    let res = store.read_chain(&issue_ref(1));
    assert!(
        matches!(res, Err(StoreError::Git(_))),
        "a non-commit ref must be a StoreError::Git, got: {res:?}"
    );
}

#[test]
fn read_chain_rejects_event_json_that_is_not_a_blob() {
    // store.rs:490-492 — a tree entry at `.forge/event.json` that is a
    // SUBTREE (directory), not a blob, is an InvalidState, never a panic.
    let dir = tmpdir("eventtree");
    let store = bound(&dir);
    let repo = store.repo();
    let sig = test_signature();
    // Build: root commit with a directory at .forge/event.json.
    let mut root = repo.treebuilder(None).unwrap();
    let mut forge_tb = repo.treebuilder(None).unwrap();
    let event_tb = repo.treebuilder(None).unwrap();
    let sub_tree_oid = event_tb.write().unwrap();
    forge_tb
        .insert("event.json", sub_tree_oid, 0o040000)
        .unwrap();
    let forge_oid = forge_tb.write().unwrap();
    root.insert(".forge", forge_oid, 0o040000).unwrap();
    let tree_oid = root.write().unwrap();
    let tree = repo.find_tree(tree_oid).unwrap();
    let base = repo.commit(None, &sig, &sig, "base", &tree, &[]).unwrap();
    repo.reference(&issue_ref(1), base, false, "corrupt")
        .unwrap();
    let res = store.read_chain(&issue_ref(1));
    assert!(
        matches!(res, Err(StoreError::InvalidState(_))),
        "a subtree at event.json must be InvalidState, got: {res:?}"
    );
}

#[test]
fn read_chain_rejects_non_utf8_event_json() {
    // store.rs:484-485 — an event blob with invalid UTF-8 is an InvalidState,
    // never a panic and never a silently-skipped event.
    let dir = tmpdir("eventutf8");
    let store = bound(&dir);
    let repo = store.repo();
    let sig = test_signature();
    let bad = repo.blob(&[0xff, 0xfe, 0x00, 0x01]).unwrap();
    let mut root = repo.treebuilder(None).unwrap();
    let mut forge_tb = repo.treebuilder(None).unwrap();
    forge_tb.insert("event.json", bad, 0o100644).unwrap();
    let forge_oid = forge_tb.write().unwrap();
    root.insert(".forge", forge_oid, 0o040000).unwrap();
    let tree_oid = root.write().unwrap();
    let tree = repo.find_tree(tree_oid).unwrap();
    let base = repo.commit(None, &sig, &sig, "base", &tree, &[]).unwrap();
    repo.reference(&issue_ref(1), base, false, "corrupt")
        .unwrap();
    let res = store.read_chain(&issue_ref(1));
    assert!(
        matches!(res, Err(StoreError::InvalidState(_))),
        "a non-UTF8 event blob must be InvalidState, got: {res:?}"
    );
}

#[test]
fn allocate_id_refuses_when_entity_ref_preexists() {
    // store.rs:737-740 — a pre-existing entity ref at the id the counter would
    // allocate is the stale-collision case: RefExists, counter untouched.
    let dir = tmpdir("allocexist");
    let store = bound(&dir);
    let repo = store.repo();
    let sig = test_signature();
    let tree_oid = repo.treebuilder(None).unwrap().write().unwrap();
    let tree = repo.find_tree(tree_oid).unwrap();
    let base = repo.commit(None, &sig, &sig, "base", &tree, &[]).unwrap();
    repo.reference(&issue_ref(1), base, false, "pre").unwrap();
    let res = store.allocate_id();
    assert!(
        matches!(res, Err(StoreError::RefExists(_))),
        "a pre-existing entity ref must be RefExists, got: {res:?}"
    );
    assert!(
        repo.find_reference(COUNTER_REF).is_err(),
        "counter must be untouched by a refused allocation"
    );
}

#[test]
fn merge_squash_refuses_pr_created_with_empty_title() {
    // git.rs:472-477 — the squash path refuses a PR whose stored title is
    // empty (a direct-store PR bypasses the CLI's title validation): the
    // worktree is cleaned and no merge ref advances.
    let dir = tmpdir("emptytitle");
    {
        let store = bound(&dir);
        let repo = store.repo();
        let sig = test_signature();
        let tree_oid = repo.treebuilder(None).unwrap().write().unwrap();
        let tree = repo.find_tree(tree_oid).unwrap();
        let base = repo.commit(None, &sig, &sig, "base", &tree, &[]).unwrap();
        // Feature carries its own justfile so the CI fallback has a plan in the
        // PR snapshot (the justfile must predate create_pr to be snapshotted).
        let mut feat_tb = repo.treebuilder(None).unwrap();
        let just_blob = repo.blob(b"check:\n    echo ok\n").unwrap();
        feat_tb.insert("justfile", just_blob, 0o100644).unwrap();
        let feat_tree_oid = feat_tb.write().unwrap();
        let feat_tree = repo.find_tree(feat_tree_oid).unwrap();
        let head = repo
            .commit(
                None,
                &sig,
                &sig,
                "head",
                &feat_tree,
                &[&repo.find_commit(base).unwrap()],
            )
            .unwrap();
        repo.branch("main", &repo.find_commit(base).unwrap(), false)
            .unwrap();
        repo.branch("feature", &repo.find_commit(head).unwrap(), false)
            .unwrap();
        let merge_base = repo.merge_base(base, head).unwrap();
        let _id = {
            let id = store
                .create_pr(
                    "",
                    "feature",
                    "main",
                    head,
                    base,
                    merge_base,
                    "a@x",
                    None,
                    &[],
                )
                .expect("store-level create_pr accepts an empty title");
            assert_eq!(id, 1);
            id
        };
    }

    // Drive the merge through the real CLI: approve, run CI to green, then
    // squash — the empty title must refuse the squash.
    let bin = env!("CARGO_BIN_EXE_git-forge");
    let run = |args: &[&str]| {
        let out = Command::new(bin)
            .args(args)
            .current_dir(&dir)
            .output()
            .unwrap();
        (
            out.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&out.stdout).trim().to_string(),
            String::from_utf8_lossy(&out.stderr).trim().to_string(),
        )
    };
    assert_eq!(run(&["forge", "pr", "review", "1", "--approve"]).0, 0);
    let (cc, _oc, ec) = run(&["forge", "ci", "run", "1"]);
    assert_eq!(cc, 0, "ci run must succeed for the squash fixture: {ec}");
    let (cm, _om, em) = run(&["forge", "pr", "merge", "1", "--squash"]);
    assert_ne!(cm, 0, "squash of an empty-title PR must fail: {em}");
    assert!(
        em.contains("PR has no title"),
        "stderr must name the empty-title refusal: {em}"
    );
}

#[test]
fn validate_refuses_created_event_outside_meta_commit() {
    // store.rs:409 — a pr.created event appended at the chain TIP (not the
    // authoritative /meta commit) breaks the anchor: the scan refuses.
    let dir = tmpdir("forgedcreated");
    {
        let store = bound(&dir);
        let repo = store.repo();
        let sig = test_signature();
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
        let id = store
            .create_pr(
                "T",
                "feature",
                "main",
                head,
                base,
                merge_base,
                "a@x",
                None,
                &[],
            )
            .unwrap();
        assert_eq!(id, 1);
        // Forge: append a SECOND pr.created (id 1) at the tip — it is not at /meta.
        let dup = event(
            EventKind::PrCreated,
            "pr",
            1,
            "a@x",
            &[("title", JsonValue::String("dup".into()))],
        );
        store
            .append_event(&pr_head_ref(1), &dup)
            .expect("append the forged duplicate created");
    }

    let bin = env!("CARGO_BIN_EXE_git-forge");
    let out = Command::new(bin)
        .args(["forge", "ci", "run", "1"])
        .current_dir(&dir)
        .output()
        .unwrap();
    assert_ne!(
        out.status.code().unwrap_or(-1),
        0,
        "a duplicated created outside /meta must refuse the run"
    );
}

#[test]
fn validate_refuses_chain_with_no_created_event() {
    // store.rs:412 — a PR head chain carrying events but ZERO pr.created
    // events is not anchored: the scan refuses.
    let dir = tmpdir("nocreated");
    {
        let store = bound(&dir);
        let repo = store.repo();
        let sig = test_signature();
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
        let id = store
            .create_pr(
                "T",
                "feature",
                "main",
                head,
                base,
                merge_base,
                "a@x",
                None,
                &[],
            )
            .unwrap();
        assert_eq!(id, 1);
        // Forge: strip the chain down to events with NO pr.created — append two
        // comments, then repoint /meta away so the scan sees created != 1.
        let comment = event(
            EventKind::PrComment,
            "pr",
            1,
            "a@x",
            &[("body", JsonValue::String("c".into()))],
        );
        store
            .append_event(&pr_head_ref(1), &comment)
            .expect("append comment 1");
        store
            .append_event(&pr_head_ref(1), &comment)
            .expect("append comment 2");
        // Point /meta at a commit that is NOT the created commit: the scan's
        // created-at-meta check then never matches, leaving created == 0.
        let tip = repo
            .find_reference(&pr_head_ref(1))
            .unwrap()
            .target()
            .unwrap();
        repo.reference(&pr_meta_ref(1), tip, true, "forge-meta")
            .unwrap();
    }

    let bin = env!("CARGO_BIN_EXE_git-forge");
    let out = Command::new(bin)
        .args(["forge", "ci", "run", "1"])
        .current_dir(&dir)
        .output()
        .unwrap();
    assert_ne!(
        out.status.code().unwrap_or(-1),
        0,
        "a chain with no created event must refuse the run"
    );
}

#[test]
fn current_tip_surfaces_corrupt_ref_file_as_store_error() {
    // store.rs:250 — a ref FILE with unparseable content is a REAL git error
    // (not NotFound): current_tip must surface StoreError::Git, never treat it
    // as an absent entity.
    let dir = tmpdir("corruptref");
    {
        let store = bound(&dir);
        let repo = store.repo();
        // Create the ref properly first, then corrupt its on-disk file.
        let tree_oid = repo.treebuilder(None).unwrap().write().unwrap();
        let sig = test_signature();
        let tree = repo.find_tree(tree_oid).unwrap();
        let base = repo.commit(None, &sig, &sig, "base", &tree, &[]).unwrap();
        repo.reference(&issue_ref(1), base, false, "seed").unwrap();
    }
    let ref_path = dir.join(".git").join("refs/forge/issues/1");
    std::fs::create_dir_all(ref_path.parent().unwrap()).unwrap();
    std::fs::write(&ref_path, b"not-a-ref-value\n").unwrap();
    let store = EventStore::open(&dir).expect("reopen the repo");
    let res = store.read_chain(&issue_ref(1));
    assert!(
        matches!(res, Err(StoreError::Git(_))),
        "a corrupt ref file must surface StoreError::Git, got: {res:?}"
    );
}

#[test]
fn allocate_id_surfaces_batch_failure_when_entity_ref_is_absent() {
    // store.rs:740 — a ref update batch that fails for a reason OTHER than a
    // pre-existing entity ref (here: a stale .lock file) surfaces the raw git
    // error once the counter is proven unchanged and the entity ref absent.
    let dir = tmpdir("alloclock");
    let store = bound(&dir);
    let lock = dir.join(".git").join("refs/forge/issues/1.lock");
    std::fs::create_dir_all(lock.parent().unwrap()).unwrap();
    std::fs::write(&lock, b"stale").unwrap();
    let res = store.allocate_id();
    assert!(res.is_err(), "a locked entity ref must fail the allocation");
    match res {
        Err(StoreError::RefExists(_)) => {
            unreachable!("the entity ref does not exist; RefExists would be wrong");
        }
        Err(_) => {}
        Ok(_) => unreachable!("allocation must fail under a stale lock"),
    }
}

#[test]
fn read_chain_rejects_event_entry_whose_oid_is_not_a_blob() {
    // store.rs:496 — a `.forge/event.json` entry whose object id names a TREE
    // while carrying a blob file mode is malformed; reading the chain must
    // error (Git/InvalidState), never panic and never yield a phantom event.
    // Built via `update-index --cacheinfo`, which does not type-check the
    // object against the mode.
    let dir = tmpdir("eventtreeoid");
    let index = dir.join(".git").join("index");
    let sub_tree;
    {
        let store = bound(&dir);
        let repo = store.repo();
        let event_tb = repo.treebuilder(None).unwrap();
        sub_tree = event_tb.write().unwrap();
    }
    let ci = Command::new("git")
        .args([
            "update-index",
            "--add",
            "--cacheinfo",
            &format!("100644,{sub_tree},event.json"),
        ])
        .env("GIT_INDEX_FILE", &index)
        .current_dir(&dir)
        .output()
        .unwrap();
    assert_eq!(ci.status.code(), Some(0), "setup: stage malformed entry");
    let cw = Command::new("git")
        .args(["write-tree"])
        .env("GIT_INDEX_FILE", &index)
        .current_dir(&dir)
        .output()
        .unwrap();
    assert_eq!(cw.status.code(), Some(0), "setup: write forge tree");
    let forge_tree_oid =
        git2::Oid::from_str(String::from_utf8_lossy(&cw.stdout).trim()).expect("parse tree oid");

    let store = bound(&dir);
    let repo = store.repo();
    let sig = test_signature();
    let mut root_tb = repo.treebuilder(None).unwrap();
    root_tb.insert(".forge", forge_tree_oid, 0o040000).unwrap();
    let root_oid = root_tb.write().unwrap();
    let tree = repo.find_tree(root_oid).unwrap();
    let base = repo.commit(None, &sig, &sig, "base", &tree, &[]).unwrap();
    repo.reference(&issue_ref(1), base, false, "corrupt")
        .unwrap();
    let res = store.read_chain(&issue_ref(1));
    assert!(
        matches!(res, Err(StoreError::Git(_))) || matches!(res, Err(StoreError::InvalidState(_))),
        "a tree-oid event entry must be an error, got: {res:?}"
    );
}

#[test]
fn concurrent_create_pr_retries_and_yields_distinct_ids() {
    // store.rs:851 — two concurrent create_pr calls race on the SAME repo's
    // counter; the loser retries with a fresh id and both PRs publish with
    // distinct ids and intact chains.
    let dir = tmpdir("concpr");
    let (base, head, merge_base) = {
        let store = bound(&dir);
        let repo = store.repo();
        let sig = test_signature();
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
        (base, head, merge_base)
    };
    let d1 = dir.clone();
    let d2 = dir.clone();
    let h1 = std::thread::spawn(move || {
        let s = EventStore::open(&d1)
            .unwrap()
            .bind_signature(test_signature());
        s.create_pr(
            "A",
            "feature",
            "main",
            head,
            base,
            merge_base,
            "a@x",
            None,
            &[],
        )
    });
    let h2 = std::thread::spawn(move || {
        let s = EventStore::open(&d2)
            .unwrap()
            .bind_signature(test_signature());
        s.create_pr(
            "B",
            "feature",
            "main",
            head,
            base,
            merge_base,
            "a@x",
            None,
            &[],
        )
    });
    let r1 = h1.join().unwrap();
    let r2 = h2.join().unwrap();
    let (id1, id2) = match (r1, r2) {
        (Ok(a), Ok(b)) => (a, b),
        (r1, r2) => panic!("both concurrent create_pr must succeed: {r1:?} {r2:?}"),
    };
    assert_ne!(id1, id2, "distinct ids from a concurrent race");
}
