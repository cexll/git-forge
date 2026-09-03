//! PR merge-gate state machine and merge-completion transaction construction
//! (pure extraction; behavior preserved).
//!
//! Split from `pr_merge.rs` / `store.rs` so the CLI merge orchestration and
//! the store layer stay under the size gate when P0 extends the merge flow
//! (issue-close in the finalize transaction, abandoned-PR terminal state).
//! The merge gate binds its decision to the EXACT PR-head OID it validated
//! (F-007); the completion transaction CASes the head chain from that OID.

use git2::Oid;
use std::collections::HashMap;

use crate::event::{Event, EventKind, JsonValue};
use crate::store::{pr_head_ref, pr_result_ref, BoundEventStore, EventStore, StoreError, ZERO_OID};

/// Read the PR head ref tip and validate the merge gate against that EXACT tip
/// (F-007, F-008): the folded [`crate::event::PrState`] is returned together
/// with the head OID the gates were evaluated against, so the completion
/// transaction can CAS from that OID — never re-read and accept a tip that
/// moved after the gate.
pub(crate) fn read_merge_gate(
    store: &EventStore,
    id: u64,
    head_ref: &str,
) -> Result<(crate::event::PrState, Oid), String> {
    let tip = store
        .repo()
        .find_reference(head_ref)
        .and_then(|r| r.target().ok_or(git2::Error::from_str("no target")))
        .map_err(|_| format!("PR #{id} does not exist"))?;
    let state = validate_merge_gate_at(store, id, tip)?;
    Ok((state, tip))
}

/// Validate PR #id's merge gate against a SPECIFIC head tip (F-007, F-008):
/// the whole chain folded at `tip` must be anchored to PR #id, the effective
/// Review must be approve, the PR must not already be merged, and the latest
/// CI Check must be success. Returns the folded [`crate::event::PrState`].
/// The exact tip validated is the tip the caller folds and CASes from.
pub(crate) fn validate_merge_gate_at(
    store: &EventStore,
    id: u64,
    tip: Oid,
) -> Result<crate::event::PrState, String> {
    // Chain-anchor / identity validation (F-008): the WHOLE chain folded at
    // `tip` must be anchored by PR #id's authoritative `pr.created` event and
    // every event-bearing commit must belong to entity `pr` / `entity_id` id.
    // The previous `fold(...).pr.id == id` check was unsound because `fold`
    // overwrites `pr.id` for every PR event, so a mixed/foreign chain carrying
    // the target id on a later event could pass the anchor while retaining a
    // foreign snapshot, approval, and CI state.
    // Single chain walk (F-008): read the chain at `tip` and validate it is
    // anchored to PR `id` in one pass, so the fold below reuses the same
    // events instead of re-walking the whole chain a second time.
    let (anchored, chain) = store
        .read_pr_chain_anchored(id, tip)
        .map_err(|e| e.to_string())?;
    if !anchored {
        return Err(format!(
            "refs/forge/prs/{id}/head points at a tip whose chain is not anchored \
             to PR #{id}"
        ));
    }
    let state = crate::event::fold(&chain).pr;
    // Belt-and-suspenders: now that `pr_chain_anchored_to` guarantees identity,
    // this is only a cross-check against a malformed fold.
    if state.id != id {
        return Err(format!(
            "refs/forge/prs/{id}/head points at a tip whose chain is anchored to \
             PR #{}, not PR #{id}",
            state.id
        ));
    }

    // Gate: effective approve required (last reachable pr.review).
    if state.effective_decision.as_deref() != Some("approve") {
        return Err(format!(
            "PR #{id} is not approved (effective decision: {})",
            state.effective_decision.as_deref().unwrap_or("none")
        ));
    }
    // Already merged: a pr.merge event exists → no double merge.
    if state.merge_result.is_some() {
        return Err(format!("PR #{id} is already merged"));
    }
    // Gate (L2): the latest CI Check must be success (the fold keeps the most
    // recently appended ci.check). A pending/absent/failed Check refuses the
    // merge BEFORE any worktree/ref side effect, and names the `ci run` step
    // as the remedy. This is purely additive to the L1 approval gate.
    match state.ci_status.as_deref() {
        Some("success") => {}
        Some(status) => {
            return Err(format!(
                "PR #{id} is not mergeable: latest CI Check is `{status}` \
                 (expected `success`); run `git forge ci run {id}`"
            ));
        }
        None => {
            return Err(format!(
                "PR #{id} is not mergeable: no CI Check recorded; run `git forge ci run {id}`"
            ));
        }
    }
    Ok(state)
}

/// Shared best-effort pending-result cleanup, used by the finalize early-return
/// sites in `cmd_pr_merge` (seam worktree-lock failure / removal failed / dir
/// still exists / list failed / still registered / barrier deadline) and by
/// `run_finalize`: release the temp-path sibling lock, CAS-delete
/// `refs/forge/prs/<id>/result` (expected `result_commit`), and return the
/// user-facing "pending result ref left in place" suffix (empty when the ref
/// was deleted or already absent). Each call site keeps its own distinct error
/// message string.
pub(crate) fn cleanup_pending_result(
    store: &BoundEventStore,
    id: u64,
    result_commit: Oid,
    lock_handle: &mut Option<std::fs::File>,
    tmp: &std::path::Path,
) -> String {
    // Release the path lock (drop handle + remove file) so a concurrent
    // same-repo merge can reuse the path. Idempotent: runs even at sites
    // where the lock is already released.
    *lock_handle = None;
    let _ = std::fs::remove_file(tmp.with_extension("lock"));
    let left = match delete_pending_result_ref(store, id, result_commit) {
        Ok(true) | Ok(false) => false,
        Err(_) => true,
    };
    if left {
        format!("; pending result ref refs/forge/prs/{id}/result left in place")
    } else {
        String::new()
    }
}

/// Atomically create the pending result ref `refs/forge/prs/<n>/result` →
/// `result_commit` (expected absence). It keeps the freshly-built merge
/// result reachable through `git gc` until the final completion
/// transaction (wire contract § Merge completion).
pub(crate) fn create_pending_result_ref(
    store: &BoundEventStore,
    pr_id: u64,
    result_commit: Oid,
) -> Result<(), StoreError> {
    let r = pr_result_ref(pr_id);
    store
        .store()
        .run_update_ref_stdin(&[format!("update {r} {result_commit} {ZERO_OID}")])
}

/// Best-effort deletion of the pending result ref (CAS: it must hold
/// `result_commit`). Ok(true) deleted; Ok(false) ref was already absent
/// (nothing to clean); Err when the ref still exists but CAS failed.
pub(crate) fn delete_pending_result_ref(
    store: &BoundEventStore,
    pr_id: u64,
    result_commit: Oid,
) -> Result<bool, StoreError> {
    let r = pr_result_ref(pr_id);
    let line = format!("update {r} {ZERO_OID} {result_commit}");
    match store.store().run_update_ref_stdin(&[line]) {
        Ok(()) => Ok(true),
        Err(_) => {
            if store.store().current_tip(&r)?.is_none() {
                Ok(false) // already gone
            } else {
                Err(StoreError::RefExists(r))
            }
        }
    }
}

/// Write an unappended `pr.merge` event commit (dangling object, parent =
/// `head_tip`) so the PR chain can be CAS-moved to it in the same
/// transaction as the base branch and the pending-ref deletion. The event
/// is constructed ONCE by the caller and reused across CAS retries, so a
/// retry after a legitimate green append reparents the SAME `pr.merge`
/// event (retained UUID + timestamp) instead of regenerating its identity
/// (F-009).
pub(crate) fn write_unappended_pr_merge_commit(
    store: &BoundEventStore,
    event: &Event,
    head_tip: Oid,
) -> Result<Oid, StoreError> {
    let message = format!("forge:{}:{}", event.kind.as_str(), event.entity_id);
    store.write_event_commit(event, &[head_tip], &message)
}

/// Atomic merge completion transaction construction (wire contract § Merge
/// completion): write the `pr.merge` event commit (dangling, parented to
/// `head_expected`), then ONE `git update-ref --stdin` transaction that
/// (1) deletes the pending `/result` ref, (2) CAS-updates
/// `refs/heads/<base_ref>` from the PR's snapshot `base_head` to
/// `result_commit`, and (3) CAS-moves the PR `/head` chain to the `pr.merge`
/// event commit.
///
/// `head_expected` is the PR-head OID the caller's gate validated — the
/// CAS refuses to advance the head from a tip that is no longer current.
/// F-007: the caller must NOT re-read and accept an arbitrary current tip
/// here, because a concurrent append (e.g. a `ci run` recording a failed
/// Check) could move the head to a tip whose gate no longer holds. On
/// failure nothing moved; the caller reports the leftover pending ref.
/// `merge_event` is the single `pr.merge` event built once by the caller
/// (F-009); it is re-parented to `head_expected` on each retry. `actor`
/// and `result_commit` are carried inside it, per the wire contract
/// `"actor": "<user.email>"` and the `result_commit` body field.
pub(crate) fn finalize_pr_merge(
    store: &BoundEventStore,
    pr_id: u64,
    base_ref: &str,
    base_expected: Oid,
    result_commit: Oid,
    merge_event: &Event,
    head_expected: Oid,
) -> Result<(), StoreError> {
    let head_ref = pr_head_ref(pr_id);
    let head_new = write_unappended_pr_merge_commit(store, merge_event, head_expected)?;
    let result = pr_result_ref(pr_id);
    let base_branch_ref = format!("refs/heads/{base_ref}");
    let lines = vec![
        // delete pending result ref (expected presence).
        format!("update {result} {ZERO_OID} {result_commit}"),
        // CAS base branch from snapshot base_head to result_commit.
        format!("update {base_branch_ref} {result_commit} {base_expected}"),
        // CAS PR head chain from the gate-validated tip to pr.merge commit.
        format!("update {head_ref} {head_new} {head_expected}"),
    ];
    store.store().run_update_ref_stdin(&lines)
}

/// The gate-validated inputs of one merge completion, captured at the gate
/// read (`read_merge_gate`) and bound to the head OID it validated (F-007).
pub(crate) struct MergeFinalizeSpec<'a> {
    pub id: u64,
    pub base_ref: &'a str,
    pub base_oid: Oid,
    pub result_commit: Oid,
    pub actor: &'a str,
    pub strategy: &'a str,
    pub gate_head: Oid,
}

/// Build the `pr.merge` event ONCE (F-009) and run the atomic completion
/// transaction with CAS retry: the transaction deletes the pending ref, CASes
/// the base branch and CASes the PR head chain from the GATE-VALIDATED OID.
/// The head ref is CAS'd from `head_expected`, never re-read and accepted: if
/// a concurrent `ci run` moved the head during the pending window, the head
/// CAS fails and nothing moves. We then re-read/refold and retry ONLY when the
/// new tip is a valid first-parent extension of the CURRENT validated head on
/// PR #id's chain (F-008; `head_expected` as it advances through each adopted
/// candidate — NOT the immutable initial `gate_head`, F-010) AND the
/// effective Review is still approve AND the latest CI Check is still
/// success — the revalidated tip becomes the new expected head. Otherwise
/// (a rewrite / non-append sibling move / cross-PR / cross-entity tip, a
/// tip whose gate no longer holds, or the head did not move and the
/// failure is genuine) we refuse without advancing the base, preserving the
/// pending result ref for genuine/sabotage failures (F-007, F-008, F-010).
pub(crate) fn run_finalize(
    store: &BoundEventStore,
    spec: MergeFinalizeSpec<'_>,
    lock_handle: &mut Option<std::fs::File>,
    tmp: &std::path::Path,
) -> Result<String, String> {
    let mut merge_body = HashMap::new();
    merge_body.insert(
        "strategy".into(),
        JsonValue::String(spec.strategy.to_string()),
    );
    merge_body.insert(
        "result_commit".into(),
        JsonValue::String(spec.result_commit.to_string()),
    );
    let merge_event = Event::new(EventKind::PrMerge, "pr", spec.id, spec.actor, merge_body);
    let head = pr_head_ref(spec.id);
    let mut head_expected = spec.gate_head;
    let mut attempts = 0u32;
    const MAX_FINALIZE_RETRIES: u32 = 3;
    loop {
        match finalize_pr_merge(
            store,
            spec.id,
            spec.base_ref,
            spec.base_oid,
            spec.result_commit,
            &merge_event,
            head_expected,
        ) {
            Ok(()) => break,
            Err(e) => {
                // F-008: read the EXACT current head tip ONCE. The candidate we
                // validate, fold, and CAS is the SAME OID — never a re-read tip
                // adopted without proof (the old loop re-read the ref at the top
                // of the next iteration, which could swap `head_expected` for an
                // unproven tip).
                let head_now = store
                    .repo()
                    .find_reference(&head)
                    .ok()
                    .and_then(|r| r.target());
                // Disappeared head ref: a genuine transaction failure. Refuse
                // with refs unchanged and the pending result ref left in place.
                let Some(tip) = head_now else {
                    return Err(format!(
                        "merge execution finished but final transaction failed \
                         ({e}); refs unchanged, pending result ref \
                         refs/forge/prs/{}/result left in place",
                        spec.id
                    ));
                };
                // Head did not move: the failure is genuine (base / pending-ref
                // / git error). Refuse with refs unchanged and the pending ref
                // left in place.
                if tip == head_expected {
                    return Err(format!(
                        "merge execution finished but final transaction failed \
                         ({e}); refs unchanged, pending result ref \
                         refs/forge/prs/{}/result left in place",
                        spec.id
                    ));
                }
                // Canonical chain validator: the moved tip must be a valid
                // first-parent extension of the CURRENT validated tip on PR
                // #id's chain (F-008, F-010) — `head_expected`, not the
                // immutable initial `gate_head`. The initial `gate_head` is
                // only correct on the FIRST retry; once the loop has validated
                // and adopted a newer tip (e.g. a legitimate green append
                // H→A), a subsequent candidate must extend THAT tip. A sibling
                // rewrite A→B that descends from H but never from A is a
                // non-append move and must be refused (F-010). Rewrites,
                // mixed/foreign chains, cross-PR tips, or disappeared refs
                // also cannot reach `head_expected`, so all refuse with refs
                // unchanged and the pending ref left in place.
                if !store
                    .store()
                    .head_extends_pr_chain(spec.id, head_expected, tip)
                {
                    return Err(format!(
                        "merge execution finished but final transaction failed \
                         ({e}); refs unchanged, pending result ref \
                         refs/forge/prs/{}/result left in place",
                        spec.id
                    ));
                }
                // The candidate is a valid extension; re-evaluate the merge
                // gates against THIS EXACT tip (F-007): a legitimate concurrent
                // append that carries a failed CI Check refuses and names the
                // gate; a legitimate green append is retried.
                if let Err(gate_err) = validate_merge_gate_at(store.store(), spec.id, tip) {
                    let pending = cleanup_pending_result(
                        store,
                        spec.id,
                        spec.result_commit,
                        lock_handle,
                        tmp,
                    );
                    return Err(format!(
                        "{gate_err} (merge aborted: the PR head changed during \
                         finalization; refs unchanged{pending})"
                    ));
                }
                head_expected = tip;
                attempts += 1;
                // Test-only retry-window barrier (F-010): pause between the
                // adoption of a revalidated head tip and the retry transaction,
                // so a regression can force-rewrite the head (A→B) at exactly
                // that point. Fires only on the FIRST adoption; debug builds
                // only, inert in release.
                #[cfg(debug_assertions)]
                if attempts == 1 {
                    if let Err(b) = maybe_run_test_retry_barrier() {
                        let pending = cleanup_pending_result(
                            store,
                            spec.id,
                            spec.result_commit,
                            lock_handle,
                            tmp,
                        );
                        return Err(format!("{b}{pending}"));
                    }
                }
                if attempts > MAX_FINALIZE_RETRIES {
                    let pending = cleanup_pending_result(
                        store,
                        spec.id,
                        spec.result_commit,
                        lock_handle,
                        tmp,
                    );
                    return Err(format!(
                        "merge execution finished but the PR head kept moving \
                         during finalization ({e}); refs unchanged{pending}"
                    ));
                }
            }
        }
    }
    Ok(format!(
        "merged PR #{} into {} ({})",
        spec.id, spec.base_ref, spec.result_commit
    ))
}

/// Test-only pending-window barrier (wire contract § Test-only pending-window
/// barrier). Debug builds only; inert in release. When `env_key=<dir>` is set,
/// the merge pauses (between the two named merge phases) until a `release`
/// sentinel appears:
///   1. atomically create `<dir>/ready` (O_CREAT|O_EXCL);
///   2. poll for `<dir>/release` (bounded 30s deadline);
///   3. on success, delete `<dir>/release` and continue;
///   4. on deadline, remove both sentinels best-effort and fail the merge with
///      no ref updates.
#[cfg(debug_assertions)]
fn run_test_barrier(env_key: &str) -> Result<(), String> {
    use std::io::Write;
    let Ok(dir) = std::env::var(env_key) else {
        return Ok(());
    };
    let dir = std::path::PathBuf::from(dir);
    std::fs::create_dir_all(&dir).map_err(|e| format!("barrier dir: {e}"))?;
    let ready = dir.join("ready");
    let release = dir.join("release");
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&ready)
        .map_err(|e| format!("barrier ready sentinel: {e}"))?;
    writeln!(f, "ready").ok();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    while !release.exists() {
        if std::time::Instant::now() > deadline {
            let _ = std::fs::remove_file(&ready);
            let _ = std::fs::remove_file(&release);
            return Err("test merge barrier deadline exceeded; no ref updates made".into());
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    // Observed release: delete it and the ready sentinel, then continue.
    let _ = std::fs::remove_file(&release);
    let _ = std::fs::remove_file(&ready);
    Ok(())
}

/// Pre-final-transaction barrier: pauses after the pending `/result` ref exists
/// (and the temp worktree is gone) but before the first completion transaction.
#[cfg(debug_assertions)]
pub(crate) fn maybe_run_test_barrier() -> Result<(), String> {
    run_test_barrier("GIT_FORGE_TEST_MERGE_BARRIER")
}

/// Retry-window barrier (F-010): pauses right after the merge has adopted a
/// revalidated head tip (the first CAS retry) and before the retry transaction.
/// Lets a deterministic regression force-rewrite the head between the adoption
/// and the retry. Debug builds only; inert in release and when the env var is
/// unset.
#[cfg(debug_assertions)]
fn maybe_run_test_retry_barrier() -> Result<(), String> {
    run_test_barrier("GIT_FORGE_TEST_MERGE_RETRY_BARRIER")
}
