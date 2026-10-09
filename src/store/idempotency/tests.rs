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

/// How a failure-mode row prepares the record ref (table on ticket #39).
enum Setup {
    /// A record commit holding these blob bytes, behind a loose ref.
    Content(Vec<u8>),
    /// A record commit without the record entry.
    NoEntry,
    /// A record commit whose record entry is a tree.
    EntryIsTree,
    /// A well-formed record commit with this object deleted afterwards.
    Deleted(fn(&git2::Commit<'_>) -> Oid),
    /// The loose ref file holds these bytes.
    LooseFile(&'static str),
    /// A direct ref, packed when `true`, to the object this picks given a
    /// well-formed record commit.
    Direct(bool, fn(&BoundEventStore, Oid) -> Oid),
    /// A symbolic ref to this target; the target holds a well-formed record
    /// commit when `true` and is absent otherwise.
    Symbolic(&'static str, bool),
}

fn record_commit(store: &BoundEventStore, blob: &[u8]) -> Oid {
    store
        .write_forge_commit("idempotency.json", blob, &[], "m")
        .unwrap()
}

/// A commit over `good`'s tree reshaped by `reshape` (root tree builder,
/// `.forge` tree builder), with `good` as its parent when `child`.
fn reshaped(
    store: &BoundEventStore,
    good: Oid,
    child: bool,
    reshape: fn(&Repository, &mut TreeBuilder<'_>, &mut TreeBuilder<'_>),
) -> Oid {
    let repo = store.repo();
    let good = repo.find_commit(good).unwrap();
    let tree = good.tree().unwrap();
    let forge_id = tree.get_name(".forge").unwrap().id();
    let mut forge = repo
        .treebuilder(Some(&repo.find_tree(forge_id).unwrap()))
        .unwrap();
    let mut root = repo.treebuilder(Some(&tree)).unwrap();
    reshape(repo, &mut root, &mut forge);
    root.insert(".forge", forge.write().unwrap(), 0o040000)
        .unwrap();
    let tree = repo.find_tree(root.write().unwrap()).unwrap();
    let sig = Signature::now("Test", "test@example.com").unwrap();
    let parents: Vec<&git2::Commit<'_>> = if child { vec![&good] } else { vec![] };
    repo.commit(None, &sig, &sig, "m", &tree, &parents).unwrap()
}

fn loose_object(store: &BoundEventStore, oid: Oid) -> std::path::PathBuf {
    let hex = oid.to_string();
    store
        .repo()
        .path()
        .join("objects")
        .join(&hex[..2])
        .join(&hex[2..])
}

/// Apply `setup` to the record ref `name`, where `good` is a well-formed
/// record blob for it.
fn prepare(store: &BoundEventStore, name: &str, good: &str, setup: &Setup) {
    let repo = store.repo();
    let point = |oid: Oid| {
        repo.reference(name, oid, true, "m").unwrap();
    };
    match setup {
        Setup::Content(blob) => point(record_commit(store, blob)),
        Setup::NoEntry => point(
            store
                .write_forge_commit("event.json", b"{}", &[], "m")
                .unwrap(),
        ),
        Setup::EntryIsTree => {
            let blob = repo.blob(good.as_bytes()).unwrap();
            let mut inner = repo.treebuilder(None).unwrap();
            inner.insert("x", blob, 0o100644).unwrap();
            let inner = inner.write().unwrap();
            let mut forge = repo.treebuilder(None).unwrap();
            forge.insert("idempotency.json", inner, 0o040000).unwrap();
            let forge = forge.write().unwrap();
            let mut root = repo.treebuilder(None).unwrap();
            root.insert(".forge", forge, 0o040000).unwrap();
            let tree = repo.find_tree(root.write().unwrap()).unwrap();
            let sig = Signature::now("Test", "test@example.com").unwrap();
            point(repo.commit(None, &sig, &sig, "m", &tree, &[]).unwrap());
        }
        Setup::Deleted(object) => {
            let oid = record_commit(store, good.as_bytes());
            let doomed = object(&repo.find_commit(oid).unwrap());
            point(oid);
            std::fs::remove_file(loose_object(store, doomed)).unwrap();
        }
        Setup::LooseFile(content) => {
            let path = repo.path().join(name);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, content).unwrap();
        }
        Setup::Direct(false, target) => {
            point(target(store, record_commit(store, good.as_bytes())));
        }
        Setup::Direct(true, target) => {
            let oid = target(store, record_commit(store, good.as_bytes()));
            std::fs::write(
                repo.path().join("packed-refs"),
                format!("# pack-refs with: peeled fully-peeled sorted \n{oid} {name}\n"),
            )
            .unwrap();
        }
        Setup::Symbolic(target, exists) => {
            if *exists {
                let record = record_commit(store, good.as_bytes());
                repo.reference(target, record, true, "m").unwrap();
            }
            repo.reference_symbolic(name, target, true, "m").unwrap();
        }
    }
}

/// The ref-level rows and the content rows about the record's objects
/// (commit, tree shape, blob presence) of the ticket #39 table.
fn object_rows() -> Vec<(&'static str, Setup)> {
    vec![
        // Ref level.
        (
            "R3 symbolic, dangling",
            Setup::Symbolic("refs/forge/missing", false),
        ),
        (
            "R3 symbolic to a record commit",
            Setup::Symbolic("refs/forge/other", true),
        ),
        (
            "R4 loose ref to a blob",
            Setup::Direct(false, |s, _| s.repo().blob(b"x").unwrap()),
        ),
        (
            "R4 packed ref to a blob",
            Setup::Direct(true, |s, _| s.repo().blob(b"x").unwrap()),
        ),
        (
            "R4 loose ref to a tree",
            Setup::Direct(false, |s, c| s.repo().find_commit(c).unwrap().tree_id()),
        ),
        (
            "R4 packed ref to a tree",
            Setup::Direct(true, |s, c| s.repo().find_commit(c).unwrap().tree_id()),
        ),
        (
            "R5 loose ref to a missing object",
            Setup::LooseFile("1234567890123456789012345678901234567890\n"),
        ),
        (
            "R5 packed ref to a missing object",
            Setup::Direct(true, |_, _| {
                Oid::from_str("1234567890123456789012345678901234567890").unwrap()
            }),
        ),
        ("R6 corrupt loose ref file", Setup::LooseFile("garbage\n")),
        (
            "R6 symbolic ref without a target",
            Setup::LooseFile("ref: \n"),
        ),
        // Content level.
        ("C1 tree object missing", Setup::Deleted(|c| c.tree_id())),
        ("C2 no record entry", Setup::NoEntry),
        ("C3 entry is a tree", Setup::EntryIsTree),
        (
            "C4 entry blob missing",
            Setup::Deleted(|c| {
                c.tree()
                    .unwrap()
                    .get_path(Path::new(RECORD_PATH))
                    .unwrap()
                    .id()
            }),
        ),
        (
            "C12 record commit has a parent (same tree)",
            Setup::Direct(false, |s, c| reshaped(s, c, true, |_, _, _| {})),
        ),
        (
            "C13 extra root tree entry",
            Setup::Direct(false, |s, c| {
                reshaped(s, c, false, |repo, root, _| {
                    root.insert("x", repo.blob(b"x").unwrap(), 0o100644)
                        .unwrap();
                })
            }),
        ),
        (
            "C13 extra .forge tree entry",
            Setup::Direct(false, |s, c| {
                reshaped(s, c, false, |repo, _, forge| {
                    forge
                        .insert("event.json", repo.blob(b"{}").unwrap(), 0o100644)
                        .unwrap();
                })
            }),
        ),
        (
            "C13 record entry executable",
            Setup::Direct(false, |s, c| {
                reshaped(s, c, false, |_, _, forge| {
                    let id = forge.get("idempotency.json").unwrap().unwrap().id();
                    forge.insert("idempotency.json", id, 0o100755).unwrap();
                })
            }),
        ),
    ]
}

/// Every record failure mode in the ticket #39 table: each present but
/// malformed record ref is `InvalidState("malformed idempotency record
/// <ref>")`, never an absent key, a conflict, or an internal error.
#[test]
fn every_malformed_record_failure_mode_is_invalid_state() {
    let req = request(IdempotentCommand::IssueNew, "k", "{}");
    let name = req.ref_name();
    let good = record_blob(&req, &result(1, Oid::zero()));
    let digest = req.inputs.to_string();
    let edit = |from: &str, to: &str| {
        assert!(good.contains(from), "{from}");
        Setup::Content(good.replacen(from, to, 1).into_bytes())
    };
    let inputs = |value: &str| {
        edit(
            &format!("\"inputs\":\"{digest}\""),
            &format!("\"inputs\":{value}"),
        )
    };
    let mut rows = object_rows();
    rows.extend([
        // Content level: the blob.
        ("C5 not UTF-8", Setup::Content(b"{\"v\":1,\xff}".to_vec())),
        ("C5 not JSON", Setup::Content(b"not json".to_vec())),
        ("C5 not an object", Setup::Content(b"[1]".to_vec())),
        ("C6 v is 2", edit("\"v\":1", "\"v\":2")),
        ("C6 v is a string", edit("\"v\":1", "\"v\":\"1\"")),
        ("C6 v missing", edit("\"v\":1,", "")),
        ("C6 v with a leading zero", edit("\"v\":1", "\"v\":01")),
        ("C7 extra field", edit("}", ",\"extra\":1}")),
        ("C7 field renamed", edit("\"text\":", "\"texts\":")),
        (
            "C7 command not a string",
            edit("\"command\":\"issue new\"", "\"command\":1"),
        ),
        (
            "C8 command mismatch",
            edit("\"command\":\"issue new\"", "\"command\":\"pr create\""),
        ),
        ("C8 key mismatch", edit("\"key\":\"k\"", "\"key\":\"j\"")),
        (
            "C9 inputs short hex",
            inputs(&format!("\"{}\"", &digest[..1])),
        ),
        (
            "C9 inputs 39 hex",
            inputs(&format!("\"{}\"", &digest[..39])),
        ),
        ("C9 inputs 41 hex", inputs(&format!("\"{digest}0\""))),
        (
            "C9 inputs uppercase",
            inputs(&format!("\"{}\"", digest.to_uppercase())),
        ),
        (
            "C9 inputs non-hex",
            inputs(&format!("\"{}g\"", &digest[..39])),
        ),
        ("C9 inputs a number", inputs("1")),
        ("C10 id 0", edit("\"id\":1", "\"id\":0")),
        ("C10 id negative", edit("\"id\":1", "\"id\":-1")),
        ("C10 id a string", edit("\"id\":1", "\"id\":\"1\"")),
        ("C10 id missing", edit("\"id\":1,", "")),
        ("C10 id with a leading zero", edit("\"id\":1", "\"id\":01")),
        (
            "C10 id with leading zeroes",
            edit("\"id\":1", "\"id\":0001"),
        ),
        ("C11 json missing", edit("\"json\":", "\"jsonx\":")),
        (
            "C11 json not a string",
            Setup::Content(
                good.replacen("\"json\":\"", "\"json\":[\"", 1)
                    .replacen("\",\"text\"", "\"],\"text\"", 1)
                    .into_bytes(),
            ),
        ),
        ("C11 text missing", edit(",\"text\":\"did #1\"", "")),
        (
            "C11 text null",
            edit("\"text\":\"did #1\"", "\"text\":null"),
        ),
    ]);
    let expected = format!("malformed idempotency record {name}");
    let mut failures = Vec::new();
    for (row, setup) in &rows {
        let (temp, store) = store();
        prepare(&store, &name, &good, setup);
        // A fresh handle: no object cached by `prepare` hides a deletion.
        match bound(&temp.0).store().idempotency_record(&req) {
            Err(StoreError::InvalidState(message)) if message == expected => {}
            other => failures.push(format!("{row}: {other:?}")),
        }
    }
    assert!(failures.is_empty(), "{failures:#?}");
}

/// R2: a well-formed record reads back, loose or packed.
#[test]
fn a_well_formed_record_reads_back_loose_or_packed() {
    let req = request(IdempotentCommand::IssueNew, "k", "{}");
    let good = record_blob(&req, &result(1, Oid::zero()));
    for setup in [
        Setup::Content(good.clone().into_bytes()),
        Setup::Direct(false, |_, commit| commit),
        Setup::Direct(true, |_, commit| commit),
    ] {
        let (temp, store) = store();
        prepare(&store, &req.ref_name(), &good, &setup);
        assert_eq!(
            bound(&temp.0).store().idempotency_record(&req).unwrap(),
            Some(IdempotencyRecord {
                inputs: req.inputs,
                result: result(1, Oid::zero()),
            })
        );
    }
}

/// Every ref with its direct target or `-> <symbolic target>`.
fn all_refs(store: &BoundEventStore) -> Vec<(String, String)> {
    let mut refs: Vec<_> = store
        .repo()
        .references()
        .unwrap()
        .map(|r| {
            let r = r.unwrap();
            let target = match r.symbolic_target() {
                Some(t) => format!("-> {t}"),
                None => r.target().unwrap().to_string(),
            };
            (r.name().unwrap().to_string(), target)
        })
        .collect();
    refs.sort();
    refs
}

/// R7: a symbolic record ref that appears after the record lookup (here:
/// while the result renders, just before the transaction) fails the whole
/// transaction at every claim site, dangling or not: no mutation, no write
/// through the symbolic ref, and the failure reads as the malformed record.
#[test]
fn a_symbolic_record_ref_inserted_before_publication_fails_every_claim_site() {
    type Site = fn(&BoundEventStore, &Claim<'_>) -> Result<(), StoreError>;
    let sites: [(&str, IdempotentCommand, Site); 3] = [
        ("append", IdempotentCommand::IssueComment, |store, claim| {
            store
                .append_event_claimed(&issue_ref(1), &comment(1, "c"), claim)
                .map(|_| ())
        }),
        (
            "issue batch",
            IdempotentCommand::IssueNew,
            |store, claim| {
                let snapshot = store.store().issue_snapshot().unwrap();
                let changes = BTreeMap::from([(2, vec![created(2)])]);
                store
                    .apply_issue_batch_claimed(&snapshot, 3, &changes, Some((2, claim)))
                    .map(|_| ())
            },
        ),
        ("pr create", IdempotentCommand::PrCreate, |store, claim| {
            let head = store.store().current_tip(&issue_ref(1)).unwrap().unwrap();
            store
                .create_pr_claimed(
                    "t",
                    "s",
                    "b",
                    head,
                    head,
                    head,
                    "a",
                    None,
                    &[],
                    None,
                    Some(claim),
                )
                .map(|_| ())
        }),
    ];
    let mut failures = Vec::new();
    for (site, command, run) in sites {
        for existing in [false, true] {
            let (_temp, store) = store();
            issue(&store);
            let target = "refs/forge/race-target";
            if existing {
                let tip = store.store().current_tip(&issue_ref(1)).unwrap().unwrap();
                store.repo().reference(target, tip, true, "m").unwrap();
            }
            let req = request(command, "k", "{}");
            let name = req.ref_name();
            let before = all_refs(&store);
            let racing = |id: u64, event: Oid| {
                store
                    .repo()
                    .reference_symbolic(&name, target, true, "m")
                    .unwrap();
                render(id, event)
            };
            let claim = Claim {
                request: &req,
                render: &racing,
            };
            let outcome = run(&store, &claim);
            let mut expected = before.clone();
            expected.push((name.clone(), format!("-> {target}")));
            expected.sort();
            let after = all_refs(&store);
            let ok = matches!(
                &outcome,
                Err(StoreError::InvalidState(m)) if *m == format!("malformed idempotency record {name}")
            );
            if !ok || after != expected {
                failures.push(format!(
                    "{site}, target exists {existing}: {outcome:?}; refs {after:?}"
                ));
            }
        }
    }
    assert!(failures.is_empty(), "{failures:#?}");
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
