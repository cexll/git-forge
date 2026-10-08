//! Store unit tests for idempotency records: encoding, record round trip,
//! and the claim transaction's all-or-nothing outcomes.
use super::*;
use std::cell::Cell;
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT: AtomicU64 = AtomicU64::new(0);

struct Temp(std::path::PathBuf);
impl Drop for Temp {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn bound(path: &Path) -> BoundEventStore {
    EventStore::open(path)
        .unwrap()
        .bind_signature(Signature::now("Test", "test@example.com").unwrap())
}

fn store() -> (Temp, BoundEventStore) {
    let path = std::env::temp_dir().join(format!(
        "gf-idempotency-unit-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&path);
    EventStore::init(&path).unwrap();
    let store = bound(&path);
    (Temp(path), store)
}

fn request(command: IdempotentCommand, key: &str, inputs: &str) -> IdempotencyRequest {
    IdempotencyRequest::new(command, key, inputs).unwrap()
}

fn result(id: u64, event: Oid) -> RecordedResult {
    RecordedResult {
        id,
        json: format!("{{\"id\":{id},\"event\":\"{event}\"}}"),
        text: format!("did #{id}"),
    }
}

fn render(id: u64, event: Oid) -> Result<RecordedResult, StoreError> {
    Ok(result(id, event))
}

fn comment(id: u64, body: &str) -> Event {
    Event::new(
        EventKind::IssueComment,
        "issue",
        id,
        "test@example.com",
        HashMap::from([("body".into(), JsonValue::String(body.into()))]),
    )
}

fn created(id: u64) -> Event {
    Event::new(
        EventKind::IssueCreated,
        "issue",
        id,
        "test@example.com",
        HashMap::from([("title".into(), JsonValue::String(format!("Issue {id}")))]),
    )
}

/// Issue #1 with its `issue.created` event.
fn issue(store: &BoundEventStore) -> Oid {
    let id = store.allocate_id().unwrap();
    store.append_event(&issue_ref(id), &created(id)).unwrap()
}

#[test]
fn ref_name_hex_encodes_the_key_bytes_per_command() {
    let cases = [
        (IdempotentCommand::IssueNew, "k1", "issue-new/6b31"),
        (IdempotentCommand::PrCreate, "a/b", "pr-create/612f62"),
        (
            IdempotentCommand::IssueComment,
            "ü ",
            "issue-comment/c3bc20",
        ),
        (IdempotentCommand::PrComment, "..", "pr-comment/2e2e"),
    ];
    for (command, key, suffix) in cases {
        let name = request(command, key, "{}").ref_name();
        assert_eq!(name, format!("refs/forge/idempotency/{suffix}"));
        assert!(git2::Reference::is_valid_name(&name), "{name}");
    }
    let long = "\u{1}".repeat(MAX_KEY_BYTES);
    assert!(git2::Reference::is_valid_name(
        &request(IdempotentCommand::IssueNew, &long, "{}").ref_name()
    ));
}

#[test]
fn the_digest_is_the_git_blob_id_of_the_canonical_inputs() {
    let a = request(IdempotentCommand::IssueNew, "k", r#"{"title":"t"}"#);
    let b = request(IdempotentCommand::IssueNew, "k", r#"{"title":"u"}"#);
    assert_ne!(a.inputs, b.inputs);
    let (_temp, store) = store();
    let blob = store.repo().blob(br#"{"title":"t"}"#).unwrap();
    assert_eq!(a.inputs, blob);
}

#[test]
fn claimed_append_publishes_the_event_and_the_record_together() {
    let (_temp, store) = store();
    let before = issue(&store);
    let req = request(IdempotentCommand::IssueComment, "k", "{}");
    assert_eq!(store.store().idempotency_record(&req).unwrap(), None);
    let claim = Claim {
        request: &req,
        render: &render,
    };
    let Claimed::Written(event) = store
        .append_event_claimed(&issue_ref(1), &comment(1, "c"), &claim)
        .unwrap()
    else {
        panic!("first claim must write");
    };
    assert_eq!(
        store.store().current_tip(&issue_ref(1)).unwrap(),
        Some(event)
    );
    assert_eq!(
        store
            .repo()
            .find_commit(event)
            .unwrap()
            .parent_id(0)
            .unwrap(),
        before
    );
    let record = store.store().idempotency_record(&req).unwrap().unwrap();
    assert_eq!(
        record,
        IdempotencyRecord {
            inputs: req.inputs,
            result: result(1, event),
        }
    );
    // A record commit is a root commit holding only the record blob.
    let tip = store.store().current_tip(&req.ref_name()).unwrap().unwrap();
    let commit = store.repo().find_commit(tip).unwrap();
    assert_eq!(commit.parent_count(), 0);
    assert_eq!(commit.message(), Some("forge:idempotency:issue-comment"));
}

#[test]
fn an_existing_record_wins_and_the_entity_is_unchanged() {
    let (_temp, store) = store();
    issue(&store);
    let req = request(IdempotentCommand::IssueComment, "k", "{}");
    let claim = Claim {
        request: &req,
        render: &render,
    };
    let Claimed::Written(first) = store
        .append_event_claimed(&issue_ref(1), &comment(1, "c"), &claim)
        .unwrap()
    else {
        panic!("first claim must write");
    };
    let other = request(IdempotentCommand::IssueComment, "k", r#"{"x":1}"#);
    let claim = Claim {
        request: &other,
        render: &render,
    };
    let Claimed::Existing(record) = store
        .append_event_claimed(&issue_ref(1), &comment(1, "d"), &claim)
        .unwrap()
    else {
        panic!("second claim must meet the record");
    };
    assert_eq!(record.inputs, req.inputs);
    assert_eq!(record.result, result(1, first));
    assert_eq!(
        store.store().current_tip(&issue_ref(1)).unwrap(),
        Some(first)
    );
}

#[test]
fn a_concurrent_append_without_a_record_retries_and_writes_both() {
    let (temp, store) = store();
    issue(&store);
    let competitor = bound(&temp.0);
    let req = request(IdempotentCommand::IssueComment, "k", "{}");
    let calls = Cell::new(0);
    let competing = Cell::new(None);
    let racing = |id: u64, event: Oid| {
        if calls.replace(calls.get() + 1) == 0 {
            let tip = competitor
                .append_event(&issue_ref(1), &comment(1, "competitor"))
                .unwrap();
            competing.set(Some(tip));
        }
        render(id, event)
    };
    let claim = Claim {
        request: &req,
        render: &racing,
    };
    let Claimed::Written(event) = store
        .append_event_claimed(&issue_ref(1), &comment(1, "mine"), &claim)
        .unwrap()
    else {
        panic!("claim must write after the retry");
    };
    assert_eq!(calls.get(), 2);
    let commit = store.repo().find_commit(event).unwrap();
    assert_eq!(Some(commit.parent_id(0).unwrap()), competing.get());
    let record = store.store().idempotency_record(&req).unwrap().unwrap();
    assert_eq!(record.result, result(1, event));
}

#[test]
fn a_concurrent_same_key_create_returns_its_record_without_allocating() {
    let (temp, store) = store();
    let repo = store.repo();
    let tree = repo.treebuilder(None).unwrap().write().unwrap();
    let tree = repo.find_tree(tree).unwrap();
    let sig = Signature::now("Test", "test@example.com").unwrap();
    let base = repo.commit(None, &sig, &sig, "base", &tree, &[]).unwrap();
    let parent = repo.find_commit(base).unwrap();
    let source = repo
        .commit(None, &sig, &sig, "source", &tree, &[&parent])
        .unwrap();
    let req = request(IdempotentCommand::PrCreate, "k", "{}");
    let competitor = bound(&temp.0);
    let won = Cell::new(None);
    let racing = |id: u64, event: Oid| {
        if won.get().is_none() {
            let claim = Claim {
                request: &req,
                render: &render,
            };
            let id = competitor
                .create_pr_claimed(
                    "t",
                    "s",
                    "b",
                    source,
                    base,
                    base,
                    "a",
                    None,
                    &[],
                    None,
                    Some(&claim),
                )
                .unwrap();
            let Claimed::Written(id) = id else {
                panic!("competitor must write");
            };
            won.set(Some(id));
        }
        render(id, event)
    };
    let claim = Claim {
        request: &req,
        render: &racing,
    };
    let counter = || store.store().current_tip(COUNTER_REF).unwrap();
    let outcome = store
        .create_pr_claimed(
            "t",
            "s",
            "b",
            source,
            base,
            base,
            "a",
            None,
            &[],
            None,
            Some(&claim),
        )
        .unwrap();
    let Claimed::Existing(record) = outcome else {
        panic!("the loser must meet the winner's record");
    };
    assert_eq!(won.get(), Some(1));
    assert_eq!(record.result.id, 1);
    let after = counter();
    assert_eq!(store.counter_next().unwrap(), 2);
    assert_eq!(store.store().current_tip(&pr_head_ref(2)).unwrap(), None);
    assert_eq!(counter(), after);
}

#[test]
fn a_failed_batch_publishes_no_record_and_a_recorded_key_blocks_the_batch() {
    let (_temp, store) = store();
    let req = request(IdempotentCommand::IssueNew, "k", "{}");
    let claim = Claim {
        request: &req,
        render: &render,
    };
    // A stale snapshot fails the whole transaction: no issue, no record.
    let stale = store.store().issue_snapshot().unwrap();
    issue(&store);
    let changes = BTreeMap::from([(1, vec![created(1)])]);
    assert!(matches!(
        store.apply_issue_batch_claimed(&stale, 2, &changes, Some((1, &claim))),
        Err(StoreError::CasConflict)
    ));
    assert_eq!(store.store().idempotency_record(&req).unwrap(), None);
    // A fresh snapshot commits the issue and the record together.
    let snapshot = store.store().issue_snapshot().unwrap();
    let changes = BTreeMap::from([(2, vec![created(2)])]);
    assert!(matches!(
        store.apply_issue_batch_claimed(&snapshot, 3, &changes, Some((2, &claim))),
        Ok(Claimed::Written(()))
    ));
    let tip = store.store().current_tip(&issue_ref(2)).unwrap().unwrap();
    assert_eq!(
        store
            .store()
            .idempotency_record(&req)
            .unwrap()
            .unwrap()
            .result,
        result(2, tip)
    );
    // The same key again: the record wins; counter and issues are untouched.
    let snapshot = store.store().issue_snapshot().unwrap();
    let changes = BTreeMap::from([(3, vec![created(3)])]);
    let Ok(Claimed::Existing(record)) =
        store.apply_issue_batch_claimed(&snapshot, 4, &changes, Some((3, &claim)))
    else {
        panic!("a recorded key must block the batch");
    };
    assert_eq!(record.result, result(2, tip));
    assert_eq!(
        store.store().current_tip(COUNTER_REF).unwrap(),
        snapshot.counter_tip
    );
    assert_eq!(store.store().current_tip(&issue_ref(3)).unwrap(), None);
    // A claim for an issue outside the batch is refused before any write.
    assert!(matches!(
        store.apply_issue_batch_claimed(&snapshot, 4, &changes, Some((9, &claim))),
        Err(StoreError::InvalidState(_))
    ));
}

#[test]
fn a_malformed_record_is_an_error() {
    let (_temp, store) = store();
    let req = request(IdempotentCommand::IssueNew, "k", "{}");
    let name = req.ref_name();
    let good = record_blob(&req, &result(1, Oid::zero()));
    let blobs = [
        good.replace("\"v\":1", "\"v\":2"),
        good.replace("\"key\":\"k\"", "\"key\":\"j\""),
        good.replace("\"command\":\"issue new\"", "\"command\":\"pr create\""),
        good.replace("\"id\":1", "\"id\":0"),
        good.replace(
            &format!("\"inputs\":\"{}\"", req.inputs),
            "\"inputs\":\"x\"",
        ),
        good.replace(",\"text\":\"did #1\"", ""),
        good.replace("}", ",\"extra\":1}"),
        "not json".to_string(),
    ];
    for blob in blobs {
        let commit = store
            .write_forge_commit("idempotency.json", blob.as_bytes(), &[], "m")
            .unwrap();
        store
            .store()
            .run_update_ref_stdin(&[format!("update {name} {commit}")])
            .unwrap();
        assert!(
            matches!(
                store.store().idempotency_record(&req),
                Err(StoreError::InvalidState(ref m)) if *m == format!("malformed idempotency record {name}")
            ),
            "{blob}"
        );
    }
    // A record ref at a commit without the record blob.
    let commit = store
        .write_forge_commit("event.json", b"{}", &[], "m")
        .unwrap();
    store
        .store()
        .run_update_ref_stdin(&[format!("update {name} {commit}")])
        .unwrap();
    assert!(matches!(
        store.store().idempotency_record(&req),
        Err(StoreError::InvalidState(_))
    ));
    // A record ref at a non-commit object.
    let blob = store.repo().blob(good.as_bytes()).unwrap();
    store
        .store()
        .run_update_ref_stdin(&[format!("update {name} {blob}")])
        .unwrap();
    assert!(matches!(
        store.store().idempotency_record(&req),
        Err(StoreError::InvalidState(_))
    ));
    // A symbolic record ref, dangling or pointing at a record commit.
    let good_commit = store
        .write_forge_commit("idempotency.json", good.as_bytes(), &[], "m")
        .unwrap();
    store
        .store()
        .run_update_ref_stdin(&[format!("update refs/forge/other {good_commit}")])
        .unwrap();
    for target in ["refs/forge/missing", "refs/forge/other"] {
        store
            .store()
            .run_update_ref_stdin(&[format!("delete {name}")])
            .unwrap();
        store
            .repo()
            .reference_symbolic(&name, target, true, "m")
            .unwrap();
        assert!(
            matches!(
                store.store().idempotency_record(&req),
                Err(StoreError::InvalidState(_))
            ),
            "{target}"
        );
    }
    store
        .repo()
        .find_reference(&name)
        .unwrap()
        .delete()
        .unwrap();
    // The well-formed record parses back.
    let commit = store
        .write_forge_commit("idempotency.json", good.as_bytes(), &[], "m")
        .unwrap();
    store
        .store()
        .run_update_ref_stdin(&[format!("update {name} {commit}")])
        .unwrap();
    assert_eq!(
        store
            .store()
            .idempotency_record(&req)
            .unwrap()
            .unwrap()
            .result,
        result(1, Oid::zero())
    );
}

#[test]
fn unclaimed_mutations_never_return_a_record() {
    assert!(matches!(Claimed::Written(7).written(), Ok(7)));
    let record = IdempotencyRecord {
        inputs: Oid::zero(),
        result: result(1, Oid::zero()),
    };
    assert!(matches!(
        Claimed::<u64>::Existing(record).written(),
        Err(StoreError::InvalidState(_))
    ));
}
