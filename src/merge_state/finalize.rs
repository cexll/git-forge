//! Atomic merge completion retries and adopted-input validation.
use super::{
    build_merge_event, cleanup_pending_result, finalize_pr_merge, finish_merge_output,
    refresh_base_worktrees, stage_issue_close, validate_merge_gate_at, MergeFinalizeSpec,
};
#[cfg(debug_assertions)]
use super::{maybe_run_issue_stage_barrier, maybe_run_test_retry_barrier};
use crate::cli_error::CliError;
use crate::event::{Event, EventKind};
use crate::store::{issue_ref, pr_head_ref, BoundEventStore, StoreError};
use git2::Oid;
use std::collections::HashMap;
/// Build the `pr.merge` event ONCE (F-009) and run the atomic completion
/// transaction with CAS retry: the transaction deletes the pending ref, CASes
/// the base branch and CASes the PR head chain from the GATE-VALIDATED OID.
/// When the PR declares a `--closes` target, the same transaction also CASes
/// `refs/forge/issues/<n>` onto an `issue.close` event commit (t2) — staged
/// fresh per attempt, skipped when the issue is already closed, and absent if
/// the issue ref vanished (hard refusal, never a silent skip).
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
/// The one head-unchanged exception: the linked issue may have moved between
/// staging and the transaction — that is a retryable concurrent write, and
/// the loop rebuilds the issue stage (fresh fold + tip) and retries.
pub(crate) fn run_finalize(
    store: &BoundEventStore,
    spec: MergeFinalizeSpec<'_>,
    lock_handle: &mut Option<std::fs::File>,
    tmp: &std::path::Path,
) -> Result<String, CliError> {
    let merge_event = build_merge_event(&spec);
    // Single `issue.close` event constructed ONCE (F-009 analogue): every
    // finalize attempt re-parents the SAME event to the then-current issue
    // tip, so retries never regenerate the event identity, and its empty
    // body / actor come from the same identity resolution the whole merge
    // used (`spec.actor`) — no second identity path.
    let issue_event: Option<Event> = spec.closes_issue.map(|cid| {
        Event::new(
            EventKind::IssueClose,
            "issue",
            cid,
            spec.actor,
            HashMap::new(),
        )
    });
    const MAX_FINALIZE_RETRIES: u32 = 3;
    let mut head_expected = spec.gate_head;
    let mut attempts = 0u32;
    let head = pr_head_ref(spec.id);
    let (head_applied, applied_issue) = loop {
        // Stage the issue close for THIS attempt (fresh fold + fresh tip):
        // the CAS retry loop rebuilds the parent/expected tip every attempt
        // (t2), so an attempt that fails because the issue moved concurrently
        // retries against the new tip, and a retry after a head-move CAS
        // failure drops the previous attempt's stale staging entirely.
        let issue_stage: Option<(u64, Oid, Oid)> = match &issue_event {
            Some(ev) => stage_issue_close(store, ev).map_err(|cause| {
                format!(
                    "merge execution finished but {cause}; refs unchanged, \
                     pending result ref refs/forge/prs/{}/result left in place",
                    spec.id
                )
            })?,
            None => None,
        };
        let issue_expected = issue_stage.map(|(_, _, exp)| exp);
        // Test-only per-attempt issue-stage barrier (F-006): pauses after the
        // issue close is staged for THIS attempt (fresh fold + tip) and before
        // the update-ref batch, so a regression can move the linked issue
        // exactly between staging and the transaction — the window the CAS
        // retry loop exists to heal. The ready sentinel's content is the
        // attempt number, so multi-round tests never mistake a consumed
        // round-0 sentinel for the next round. Debug builds only; inert in
        // release and when the env var is unset.
        #[cfg(debug_assertions)]
        if issue_event.is_some() {
            maybe_run_issue_stage_barrier(store, &spec, attempts, lock_handle, tmp)?;
        }
        match finalize_pr_merge(store, &spec, &merge_event, head_expected, issue_stage) {
            Ok(()) => break (head_expected, issue_stage),
            Err(mut e) => {
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
                    // F-004: best-effort delete the pending result ref (CAS
                    // expected result_commit); report a leftover ONLY when the
                    // CAS deletion itself fails.
                    let pending = cleanup_pending_result(
                        store,
                        spec.id,
                        spec.result_commit,
                        lock_handle,
                        tmp,
                    );
                    e.message = format!(
                        "merge execution finished but final transaction failed \
                         ({e}); refs unchanged{pending}"
                    );
                    return Err(e);
                };
                // Head did not move. The failure can still be the linked-issue
                // CAS (t2): a concurrent issue write between this attempt's
                // staging and the transaction moved the issue tip, so the
                // whole batch failed and nothing applied. Retry with a
                // rebuilt issue stage (fresh fold + tip); otherwise the
                // failure is genuine (base / pending-ref / git error) and we
                // refuse with refs unchanged and the pending ref left in
                // place.
                if tip == head_expected {
                    let issue_moved = match (spec.closes_issue, issue_expected) {
                        (Some(cid), Some(expected)) => store
                            .store()
                            .current_tip(&issue_ref(cid))
                            .ok()
                            .flatten()
                            .map(|now| now != expected)
                            .unwrap_or(false),
                        _ => false,
                    };
                    if issue_moved {
                        attempts += 1;
                        if attempts > MAX_FINALIZE_RETRIES {
                            let pending = cleanup_pending_result(
                                store,
                                spec.id,
                                spec.result_commit,
                                lock_handle,
                                tmp,
                            );
                            return Err(finalize_exhausted_error(e, &pending));
                        }
                        continue;
                    }
                    // F-004: best-effort delete the pending result ref; report a
                    // leftover ONLY when the CAS deletion fails.
                    let pending = cleanup_pending_result(
                        store,
                        spec.id,
                        spec.result_commit,
                        lock_handle,
                        tmp,
                    );
                    e.message = format!(
                        "merge execution finished but final transaction failed \
                         ({e}); refs unchanged{pending}"
                    );
                    return Err(e);
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
                // also cannot reach `head_expected`; all refuse with refs
                // unchanged.
                if !store
                    .store()
                    .head_extends_pr_chain(spec.id, head_expected, tip)
                {
                    // F-004: best-effort delete the pending result ref; report
                    // a leftover ONLY when the CAS deletion fails.
                    let pending = cleanup_pending_result(
                        store,
                        spec.id,
                        spec.result_commit,
                        lock_handle,
                        tmp,
                    );
                    e.message = format!(
                        "merge execution finished but final transaction failed \
                         ({e}); refs unchanged{pending}"
                    );
                    return Err(e);
                }
                // The candidate is a valid extension; re-evaluate the merge
                // gates against THIS EXACT tip (F-007): a legitimate concurrent
                // append that carries a failed CI Check refuses and names the
                // gate; a legitimate green append is retried. The returned
                // folded state is also cross-checked for F-003 (snapshot
                // identity) and F-002 (candidate identity) below.
                validate_adopted_finalize_tip(store, &spec, tip, lock_handle, tmp)?;
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
                        return Err(format!("{b}{pending}").into());
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
                    return Err(finalize_exhausted_error(e, &pending));
                }
            }
        }
    };
    // ── #3 live-base refresh: after the base CAS, refresh every checked-out
    // base worktree to the result commit and roll the whole completion back
    // (never split) when a refresh fails. Detail in `refresh_base_worktrees`.
    refresh_base_worktrees(store, &spec, head_applied, applied_issue)?;
    finish_merge_output(store, &spec).map_err(CliError::from)
}

fn finalize_exhausted_error(mut error: CliError, pending: &str) -> CliError {
    let message = format!(
        "merge execution finished but the merge inputs kept moving during \
         finalization ({error}); refs unchanged{pending}"
    );
    if error.code == "E_CONFLICT" {
        error = CliError::from(StoreError::Exhausted);
    }
    error.message = message;
    error
}

/// F-007/F-003/F-002 finalize retry validation, extracted from `run_finalize`
/// (clippy too_many_lines): re-evaluate the merge gate against the moved tip
/// `tip`, then require the folded state's snapshot identity (generation +
/// source_head) AND prepared-candidate identity to match the spec the result
/// was built from. Refuses (with cleanup) on any mismatch — the retry never
/// finalizes the OLD result under a NEW generation/candidate's head.
fn validate_adopted_finalize_tip(
    store: &BoundEventStore,
    spec: &MergeFinalizeSpec<'_>,
    tip: Oid,
    lock_handle: &mut Option<std::fs::File>,
    tmp: &std::path::Path,
) -> Result<(), CliError> {
    let revalidated = match validate_merge_gate_at(store.store(), spec.id, tip) {
        Ok(state) => state,
        Err(mut gate_err) => {
            let pending =
                cleanup_pending_result(store, spec.id, spec.result_commit, lock_handle, tmp);
            gate_err.message = format!(
                "{gate_err} (merge aborted: the PR head changed during finalization; \
                 refs unchanged{pending})"
            );
            return Err(gate_err);
        }
    };
    // F-003: the revalidated tip may carry a NEW snapshot generation (a
    // `pr update` landed during the pending window). The merge result was
    // built from the OLD snapshot only; a gate-validated-but-different
    // generation/source means the retry would finalize the OLD result under
    // the NEW generation's head, falsely marking updated content merged.
    // Refuse (cleanup + no rematerialization), naming the change.
    if revalidated.generation != spec.generation
        || revalidated.source_head.as_deref() != Some(spec.source_head)
    {
        let pending = cleanup_pending_result(store, spec.id, spec.result_commit, lock_handle, tmp);
        return Err(CliError::new(
            "E_GATE",
            format!(
                "merge execution finished but the PR snapshot changed during finalization \
                 (generation {} -> {}, source {} -> {}); refs unchanged{pending}",
                spec.generation,
                revalidated.generation,
                spec.source_head,
                revalidated.source_head.as_deref().unwrap_or("<none>")
            ),
            spec.id.to_string(),
            "Inspect PR review and CI state and satisfy the reported merge gate.",
            false,
        ));
    }
    // F-002 candidate identity: a re-prepare in the pending window replaces
    // the candidate WITHOUT bumping the generation (so the generation/source
    // check above cannot see it). The merge is bound to the OLD candidate's
    // result; a fold whose candidate now differs must REFUSE (cleanup +
    // re-prepare remedy), never finalize under a head that approves a
    // different candidate. Applies on both paths: the candidate path pins
    // `spec.prepared_result`; a legacy merge that gains a candidate
    // mid-window also refuses (the candidate pin would collide with the
    // pending-ref lifecycle).
    let candidate_changed = match (spec.prepared_result, &revalidated.prepared) {
        (None, None) => false,
        (None, Some(_)) | (Some(_), None) => true,
        (Some(expected), Some(current)) => current.result_commit != expected,
    };
    if candidate_changed {
        let pending = cleanup_pending_result(store, spec.id, spec.result_commit, lock_handle, tmp);
        return Err(CliError::new(
            "E_GATE",
            format!(
                "merge execution finished but the PR candidate changed during finalization; \
                 re-run `git forge pr prepare {}`, then `git forge ci run {}` and \
                 `git forge pr review {} --approve`, then merge; refs unchanged{pending}",
                spec.id, spec.id, spec.id
            ),
            spec.id.to_string(),
            "Inspect PR review and CI state and satisfy the reported merge gate.",
            false,
        ));
    }
    Ok(())
}
