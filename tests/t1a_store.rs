//! t1a store integration tests. Each test runs against an isolated temp repo.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use git_forge::event::{Event, EventKind, JsonValue};
use git_forge::store::{
    issue_ref, pr_base_ref, pr_head_ref, pr_meta_ref, pr_source_ref, EventStore, StoreError,
    COUNTER_REF,
};

// Process-local monotonic suffix: two tests in this binary may share a `tag`
// (e.g. "chain") and would collide on pid+nanos alone, racing .git/config.lock
// when both init the same temp dir in the same instant.
static NEXT_TMPDIR: AtomicU64 = AtomicU64::new(0);

fn candidate_name(tag: &str, pid: u32, seq: u64) -> PathBuf {
    std::env::temp_dir().join(format!("gf-t1a-{tag}-{pid}-{seq}"))
}

/// Create a fresh isolated temp dir, starting the candidate scan at
/// `start_seq`. Creation is exclusive (`create_dir`): a candidate that already
/// exists — a stale dir left in /tmp by a prior run after PID reuse, or a
/// parallel test's directory — is skipped, never reopened as the test repo.
fn make_tmpdir(tag: &str, pid: u32, start_seq: u64) -> PathBuf {
    let mut seq = start_seq;
    loop {
        let d = candidate_name(tag, pid, seq);
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
    make_tmpdir(tag, std::process::id(), start_seq)
}

#[test]
fn tmpdir_skips_stale_directory_from_prior_run() {
    // Cross-run PID-reuse regression: a prior run may leave
    // gf-t1a-<tag>-<pid>-<seq> in /tmp (this suite never cleans up). Start the
    // scan at seq 0 with candidate 0 pre-existing — make_tmpdir must skip it
    // and exclusively create candidate 1, never reopen the stale repo.
    struct TempDirGuard(PathBuf);
    impl Drop for TempDirGuard {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    let pid = std::process::id();
    let stale = candidate_name("stale", pid, 0);
    std::fs::create_dir(&stale).unwrap();
    let _stale_guard = TempDirGuard(stale.clone());
    let d = make_tmpdir("stale", pid, 0);
    let _d_guard = TempDirGuard(d.clone());
    assert_ne!(d, stale, "tmpdir must not reuse an existing directory");
    assert_eq!(
        d,
        candidate_name("stale", pid, 1),
        "must take the first free candidate"
    );
    std::fs::remove_dir(&stale).unwrap();
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

#[test]
fn lazy_first_allocation_is_atomic() {
    let dir = tmpdir("lazy");
    let store = EventStore::init(&dir).unwrap();
    let id = store.allocate_id().unwrap();
    assert_eq!(id, 1, "first allocation must be id 1");
    assert!(store.repo().find_reference(COUNTER_REF).is_ok());
    assert_eq!(counter_next(&store), 2);
    assert!(store.repo().find_reference(&issue_ref(1)).is_ok());
}

#[test]
fn sequential_ids_advance_counter() {
    let dir = tmpdir("seq");
    let store = EventStore::init(&dir).unwrap();
    assert_eq!(store.allocate_id().unwrap(), 1);
    assert_eq!(store.allocate_id().unwrap(), 2);
    assert_eq!(store.allocate_id().unwrap(), 3);
    assert_eq!(counter_next(&store), 4);
    for n in 1..=3 {
        assert!(store.repo().find_reference(&issue_ref(n)).is_ok());
    }
}

#[test]
fn counter_is_a_versioned_chain() {
    let dir = tmpdir("chain");
    let store = EventStore::init(&dir).unwrap();
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
    let store = EventStore::init(&dir).unwrap();
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
    let store = EventStore::init(&dir).unwrap();
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
        let s = EventStore::open(&dir_a).unwrap();
        s.append_event(&r_a, &a_clone).unwrap()
    });
    let h2 = std::thread::spawn(move || {
        let s = EventStore::open(&dir_b).unwrap();
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
    let store = EventStore::init(&dir).unwrap();
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
    let s2 = EventStore::open(&dir).unwrap();
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
    let h1 = std::thread::spawn(move || EventStore::open(&dir_a).unwrap().allocate_id().unwrap());
    let h2 = std::thread::spawn(move || EventStore::open(&dir_b).unwrap().allocate_id().unwrap());
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
    let store = EventStore::init(&dir).unwrap();
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
    let store = EventStore::init(&dir).unwrap();
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
        .create_pr("PR title", "feature", "main", head, base, merge_base)
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
        .create_pr("second", "feature", "main", head, base, merge_base)
        .unwrap();
    assert_eq!(id2, 2, "second PR gets id 2");
    assert!(repo.find_reference(&pr_head_ref(2)).is_ok());
}

#[test]
fn create_pr_rejects_preexisting_ref_without_touching_counter() {
    let dir = tmpdir("prreject");
    let store = EventStore::init(&dir).unwrap();
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
    let res = store.create_pr("T", "feature", "main", head, base, merge_base);
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
