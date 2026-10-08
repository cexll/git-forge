//! Snapshot-bound issue graph transactions; no ref changes before final CAS.
use super::*;
use crate::event::{fold, IssueState};
use std::collections::BTreeMap;

pub(crate) struct IssueSnapshot {
    pub counter_tip: Option<Oid>,
    pub next: u64,
    pub tips: BTreeMap<u64, Oid>,
    pub states: BTreeMap<u64, IssueState>,
}
impl EventStore {
    pub(crate) fn issue_snapshot(&self) -> Result<IssueSnapshot, StoreError> {
        for _ in 0..MAX_ALLOC_RETRIES {
            let counter_tip = self.current_tip(COUNTER_REF)?;
            let next = match counter_tip {
                Some(tip) => self.read_counter_next(tip)?,
                None => 1,
            };
            #[cfg(test)]
            tests::after_counter_read(self);
            let mut tips = BTreeMap::new();
            let mut states = BTreeMap::new();
            for reference in self.repo.references_glob("refs/forge/issues/*")? {
                let reference = reference?;
                let name = reference
                    .name()
                    .ok_or_else(|| StoreError::InvalidState("non-UTF-8 issue ref".into()))?;
                let id: u64 = name
                    .strip_prefix("refs/forge/issues/")
                    .and_then(|s| s.parse().ok())
                    .filter(|id| *id > 0)
                    .ok_or_else(|| StoreError::InvalidState(format!("invalid issue ref {name}")))?;
                let tip = reference.target().ok_or_else(|| {
                    StoreError::InvalidState(format!("symbolic issue ref {name}"))
                })?;
                tips.insert(id, tip);
                let events = self.chain_events_from_tip(tip)?;
                if !events.is_empty() {
                    states.insert(id, fold(&events).issue);
                }
            }
            // Allocations publish the counter and new Issue together. Reject a
            // graph read that crossed an allocation before checking id collisions.
            if self.current_tip(COUNTER_REF)? != counter_tip {
                continue;
            }
            return Ok(IssueSnapshot {
                counter_tip,
                next,
                tips,
                states,
            });
        }
        Err(StoreError::CasConflict)
    }
}
impl BoundEventStore {
    pub(crate) fn apply_issue_batch(
        &self,
        snapshot: &IssueSnapshot,
        next: u64,
        changes: &BTreeMap<u64, Vec<Event>>,
    ) -> Result<(), StoreError> {
        let mut lines = Vec::new();
        let old_counter = snapshot
            .counter_tip
            .map_or_else(|| ZERO_OID.into(), |tip| tip.to_string());
        if next != snapshot.next {
            let counter = self.write_counter_commit(next, snapshot.counter_tip)?;
            lines.push(format!("update {COUNTER_REF} {counter} {old_counter}"));
        } else {
            lines.push(format!("verify {COUNTER_REF} {old_counter}"));
        }
        for (id, tip) in &snapshot.tips {
            if !changes.contains_key(id) {
                lines.push(format!("verify {} {tip}", issue_ref(*id)));
            }
        }
        for (id, events) in changes {
            let expected = snapshot.tips.get(id).copied();
            let mut parent = expected;
            for event in events {
                let parents: Vec<_> = parent.into_iter().collect();
                parent = Some(self.write_event_commit(
                    event,
                    &parents,
                    &format!("forge:{}:{id}", event.kind.as_str()),
                )?);
            }
            let tip =
                parent.ok_or_else(|| StoreError::InvalidState("empty issue event batch".into()))?;
            let old = expected.map_or_else(|| ZERO_OID.into(), |tip| tip.to_string());
            lines.push(format!("update {} {tip} {old}", issue_ref(*id)));
        }
        match self.inner.run_update_ref_stdin(&lines) {
            Err(error) if expected_value_rejected(&error, &lines) => Err(StoreError::CasConflict),
            result => result,
        }
    }
}

// Git has no structured update-ref error code. Only recognized expected-value
// diagnostics for this transaction permit retry; unknown failures stay fatal.
pub(crate) fn expected_value_rejected(error: &StoreError, lines: &[String]) -> bool {
    let StoreError::Command(stderr) = error else {
        return false;
    };
    lines.iter().any(|line| {
        let mut fields = line.split_whitespace();
        let operation = fields.next();
        let Some(reference) = fields.next() else {
            return false;
        };
        let expected = match operation {
            Some("update") => fields.nth(1),
            Some("verify") => fields.next(),
            _ => None,
        };
        let Some(expected) = expected else {
            return false;
        };
        let prefix = format!("fatal: prepare: cannot lock ref '{reference}': ");
        let Some(reason) = stderr.strip_prefix(&prefix) else {
            return false;
        };
        if expected == ZERO_OID {
            return reason == "reference already exists";
        }
        reason
            .strip_prefix("is at ")
            .and_then(|s| s.split_once(" but expected "))
            .is_some_and(|(actual, rejected)| {
                rejected == expected && actual != expected && Oid::from_str(actual).is_ok()
            })
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(0);
    type SnapshotHook = Box<dyn FnMut(&EventStore)>;
    thread_local! {
        static SNAPSHOT_HOOK: std::cell::RefCell<Option<SnapshotHook>> = const { std::cell::RefCell::new(None) };
    }
    pub(super) fn after_counter_read(store: &EventStore) {
        SNAPSHOT_HOOK.with(|hook| {
            if let Some(hook) = hook.borrow_mut().as_mut() {
                hook(store);
            }
        });
    }
    struct Hook;
    impl Hook {
        fn install(hook: impl FnMut(&EventStore) + 'static) -> Self {
            SNAPSHOT_HOOK.with(|slot| *slot.borrow_mut() = Some(Box::new(hook)));
            Self
        }
    }
    impl Drop for Hook {
        fn drop(&mut self) {
            SNAPSHOT_HOOK.with(|slot| *slot.borrow_mut() = None);
        }
    }
    struct Temp(std::path::PathBuf);
    impl Drop for Temp {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    fn store() -> (Temp, BoundEventStore) {
        let path = loop {
            let path = std::env::temp_dir().join(format!(
                "gf-issue-cas-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            match std::fs::create_dir(&path) {
                Ok(()) => break path,
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(e) => panic!("{e}"),
            }
        };
        let store = EventStore::init(&path)
            .unwrap()
            .bind_signature(Signature::now("Test", "test@example.com").unwrap());
        (Temp(path), store)
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
    #[test]
    fn snapshot_restarts_after_allocation_between_counter_and_issue_reads() {
        for initialized in [false, true] {
            let (temp, store) = store();
            if initialized {
                store.allocate_id().unwrap();
            }
            let competitor = EventStore::open(&temp.0)
                .unwrap()
                .bind_signature(Signature::now("Competitor", "competitor@example.com").unwrap());
            let mut competed = false;
            let _hook = Hook::install(move |_| {
                if !competed {
                    competitor.allocate_id().unwrap();
                    competed = true;
                }
            });
            let snapshot = store.store().issue_snapshot().unwrap();
            let allocated = if initialized { 2 } else { 1 };
            assert_eq!(snapshot.next, allocated + 1);
            assert_eq!(
                snapshot.counter_tip,
                store.store().current_tip(COUNTER_REF).unwrap()
            );
            assert!(snapshot.tips.contains_key(&allocated));
            assert!(!snapshot.tips.contains_key(&snapshot.next));
            store
                .apply_issue_batch(
                    &snapshot,
                    snapshot.next + 1,
                    &BTreeMap::from([(snapshot.next, vec![created(snapshot.next)])]),
                )
                .unwrap();
            assert_eq!(store.counter_next().unwrap(), allocated + 2);
        }
    }
    #[test]
    fn snapshot_returns_cas_conflict_when_counter_never_stabilizes() {
        let (temp, store) = store();
        let competitor = EventStore::open(&temp.0)
            .unwrap()
            .bind_signature(Signature::now("Test", "test@example.com").unwrap());
        let _hook = Hook::install(move |_| {
            competitor.allocate_id().unwrap();
        });
        assert!(matches!(
            store.store().issue_snapshot(),
            Err(StoreError::CasConflict)
        ));
        assert_eq!(
            store.counter_next().unwrap(),
            u64::from(MAX_ALLOC_RETRIES) + 1
        );
        assert_eq!(
            store
                .repo()
                .references_glob("refs/forge/issues/*")
                .unwrap()
                .count(),
            MAX_ALLOC_RETRIES as usize
        );
    }
    #[test]
    fn new_retries_after_snapshot_counter_conflicts() {
        const CHILD: &str = "GIT_FORGE_SNAPSHOT_RETRY_CHILD";
        if std::env::var_os(CHILD).is_some() {
            let competitor = EventStore::open(".")
                .unwrap()
                .bind_signature(Signature::now("Test", "test@example.com").unwrap());
            let mut attempts = 0;
            let _hook = Hook::install(move |_| {
                if attempts < MAX_ALLOC_RETRIES {
                    let id = competitor.allocate_id().unwrap();
                    competitor
                        .append_event(&issue_ref(id), &created(id))
                        .unwrap();
                    attempts += 1;
                }
            });
            let output = crate::issue_apply::new_issue(
                "Requested".into(),
                None,
                vec![],
                None,
                vec![],
                false,
            )
            .unwrap();
            assert_eq!(
                output,
                format!("issue #{} created: Requested", MAX_ALLOC_RETRIES + 1)
            );
            return;
        }
        let (temp, store) = store();
        let output = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "store::issue_batch::tests::new_retries_after_snapshot_counter_conflicts",
                "--nocapture",
            ])
            .current_dir(&temp.0)
            .env(CHILD, "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env_remove("GIT_FORGE_ROLE")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        let id = u64::from(MAX_ALLOC_RETRIES) + 1;
        assert_eq!(store.counter_next().unwrap(), id + 1);
        for competitor in 1..id {
            assert_eq!(
                fold(&store.read_chain(&issue_ref(competitor)).unwrap())
                    .issue
                    .title,
                Some(format!("Issue {competitor}"))
            );
        }
        assert_eq!(
            fold(&store.read_chain(&issue_ref(id)).unwrap())
                .issue
                .title
                .as_deref(),
            Some("Requested")
        );
    }
    #[test]
    fn moved_existing_counter_is_classified_as_cas_conflict() {
        let (_temp, store) = store();
        store.allocate_id().unwrap();
        let snapshot = store.store().issue_snapshot().unwrap();
        let old = snapshot.counter_tip.unwrap();
        store.allocate_id().unwrap();
        let actual = store.store().current_tip(COUNTER_REF).unwrap().unwrap();
        let error = store
            .store()
            .run_update_ref_stdin(&[format!("verify {COUNTER_REF} {old}")])
            .unwrap_err();
        let StoreError::Command(stderr) = error else {
            panic!("unexpected error: {error:?}");
        };
        assert_eq!(stderr, format!("fatal: prepare: cannot lock ref '{COUNTER_REF}': is at {actual} but expected {old}"));
        let counter = store.counter_next().unwrap();
        assert!(matches!(
            store.apply_issue_batch(
                &snapshot,
                snapshot.next + 1,
                &BTreeMap::from([(snapshot.next, vec![created(snapshot.next)])])
            ),
            Err(StoreError::CasConflict)
        ));
        assert_eq!(store.counter_next().unwrap(), counter);
        assert_eq!(
            store.store().current_tip(COUNTER_REF).unwrap(),
            Some(actual)
        );
        assert!(store
            .read_chain(&issue_ref(snapshot.next))
            .unwrap()
            .is_empty());
    }
    #[test]
    fn stale_counter_aborts_all_issue_creations_and_preserves_competing_allocation() {
        let (_temp, store) = store();
        let snapshot = store.store().issue_snapshot().unwrap();
        let competitor = store.allocate_id().unwrap();
        assert_eq!(competitor, 1);
        let counter = store.store().current_tip(COUNTER_REF).unwrap();
        let tip = store.store().current_tip(&issue_ref(1)).unwrap();
        let changes = BTreeMap::from([(1, vec![created(1)]), (2, vec![created(2)])]);
        assert!(matches!(
            store.apply_issue_batch(&snapshot, 3, &changes),
            Err(StoreError::CasConflict)
        ));
        assert_eq!(store.store().current_tip(COUNTER_REF).unwrap(), counter);
        assert_eq!(store.store().current_tip(&issue_ref(1)).unwrap(), tip);
        assert_eq!(store.store().current_tip(&issue_ref(2)).unwrap(), None);
    }
    #[test]
    fn stale_read_dependency_aborts_updates_creations_and_counter_together() {
        let (_temp, store) = store();
        let empty = store.store().issue_snapshot().unwrap();
        store
            .apply_issue_batch(
                &empty,
                3,
                &BTreeMap::from([(1, vec![created(1)]), (2, vec![created(2)])]),
            )
            .unwrap();
        let snapshot = store.store().issue_snapshot().unwrap();
        let comment = Event::new(
            EventKind::IssueComment,
            "issue",
            1,
            "test@example.com",
            HashMap::from([("body".into(), JsonValue::String("competing comment".into()))]),
        );
        let competing_tip = store.append_event(&issue_ref(1), &comment).unwrap();
        let changes = BTreeMap::from([
            (
                2,
                vec![Event::new(
                    EventKind::IssueClose,
                    "issue",
                    2,
                    "test@example.com",
                    HashMap::new(),
                )],
            ),
            (3, vec![created(3)]),
        ]);
        assert!(matches!(
            store.apply_issue_batch(&snapshot, 4, &changes),
            Err(StoreError::CasConflict)
        ));
        assert_eq!(
            store.store().current_tip(&issue_ref(1)).unwrap(),
            Some(competing_tip)
        );
        assert_eq!(
            store.store().current_tip(&issue_ref(2)).unwrap(),
            Some(snapshot.tips[&2])
        );
        assert_eq!(store.store().current_tip(&issue_ref(3)).unwrap(), None);
        assert_eq!(
            store.store().current_tip(COUNTER_REF).unwrap(),
            snapshot.counter_tip
        );
    }
    #[test]
    fn unchanged_plan_refuses_stale_snapshot_without_mutating_competitor() {
        let (_temp, store) = store();
        let empty = store.store().issue_snapshot().unwrap();
        store
            .apply_issue_batch(&empty, 2, &BTreeMap::from([(1, vec![created(1)])]))
            .unwrap();
        let snapshot = store.store().issue_snapshot().unwrap();
        let event = Event::new(
            EventKind::IssueClose,
            "issue",
            1,
            "test@example.com",
            HashMap::new(),
        );
        let competing_tip = store.append_event(&issue_ref(1), &event).unwrap();
        assert!(matches!(
            store.apply_issue_batch(&snapshot, snapshot.next, &BTreeMap::new()),
            Err(StoreError::CasConflict)
        ));
        assert_eq!(
            store.store().current_tip(&issue_ref(1)).unwrap(),
            Some(competing_tip)
        );
        assert_eq!(
            store.store().current_tip(COUNTER_REF).unwrap(),
            snapshot.counter_tip
        );
    }
}
