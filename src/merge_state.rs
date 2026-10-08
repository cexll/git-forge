//! PR merge-gate state machine and merge-completion transaction construction
//! (pure extraction; behavior preserved).
//!
//! Split from `pr_merge.rs` / `store.rs` so the CLI merge orchestration and
//! the store layer stay under the size gate when P0 extends the merge flow
//! (issue-close in the finalize transaction, abandoned-PR terminal state).
//! The merge gate binds its decision to the EXACT PR-head OID it validated
//! (F-007); the completion transaction CASes the head chain from that OID.

use crate::cli_error::CliError;
use git2::Oid;
use std::collections::HashMap;

use crate::event::{Event, EventKind, JsonValue};
use crate::store::{
    issue_ref, pr_head_ref, pr_result_ref, BoundEventStore, EventStore, StoreError, ZERO_OID,
};

mod finalize;
pub(crate) use finalize::run_finalize;

/// Read the PR head ref tip and validate the merge gate against that EXACT tip
/// (F-007, F-008): the folded [`crate::event::PrState`] is returned together
/// with the head OID the gates were evaluated against, so the completion
/// transaction can CAS from that OID — never re-read and accept a tip that
/// moved after the gate.
pub(crate) fn read_merge_gate(
    store: &EventStore,
    id: u64,
    head_ref: &str,
) -> Result<(crate::event::PrState, Oid), CliError> {
    let tip = store
        .repo()
        .find_reference(head_ref)
        .and_then(|r| r.target().ok_or(git2::Error::from_str("no target")))
        .map_err(|_| CliError::not_found(format!("PR #{id} does not exist"), id.to_string()))?;
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
) -> Result<crate::event::PrState, CliError> {
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
        .map_err(CliError::from)?;
    if !anchored {
        return Err(CliError::invalid(format!(
            "refs/forge/prs/{id}/head points at a tip whose chain is not anchored \
             to PR #{id}"
        )));
    }
    let state = crate::event::fold(&chain).pr;
    // Belt-and-suspenders: now that `pr_chain_anchored_to` guarantees identity,
    // this is only a cross-check against a malformed fold.
    if state.id != id {
        return Err(CliError::invalid(format!(
            "refs/forge/prs/{id}/head points at a tip whose chain is anchored to \
             PR #{}, not PR #{id}",
            state.id
        )));
    }

    // Abandoned PRs are terminal (pr.abandoned): refuse BEFORE the approve/CI
    // gates and name the abandon reason, so an abandoned PR never falls
    // through to a gate diagnostic. The exact tip folded here is the tip the
    // completion transaction CASes from (F-007), so an abandon appended after
    // this gate refuses the finalize via the CAS retry re-validation.
    if let Some(reason) = &state.abandoned {
        return Err(CliError::invalid(format!(
            "PR #{id} is abandoned: {reason}"
        )));
    }
    // Gate: effective approve required (last reachable pr.review).
    if state.effective_decision.as_deref() != Some("approve") {
        return Err(CliError::new(
            "E_GATE",
            format!(
                "PR #{id} is not approved (effective decision: {})",
                state.effective_decision.as_deref().unwrap_or("none")
            ),
            id.to_string(),
            "Inspect PR review and CI state and satisfy the reported merge gate.",
            false,
        ));
    }
    // Gate (t1 generation binding): a decision from an OLDER snapshot
    // generation is stale — it authorized content the PR no longer holds.
    // Only a latest-generation approve satisfies the gate.
    if state.decision_generation != state.generation {
        return Err(CliError::new(
            "E_GATE",
            format!(
                "PR #{id} approve is stale (decision on generation {}, current \
             snapshot generation {}); re-review after `pr update`",
                state.decision_generation, state.generation
            ),
            id.to_string(),
            "Inspect PR review and CI state and satisfy the reported merge gate.",
            false,
        ));
    }
    // Gate (t8 #8): an approve from the SAME role that created the PR does
    // not satisfy "someone other than the implementer approved" — the
    // distinguishability the roles exist to provide. Runs only when BOTH the
    // pr.created and the effective approve record roles: without roles the
    // actors are indistinguishable, which is NOT self-approval — the
    // single-user default (one identity, no role) keeps its pre-t8 gate.
    // The refusal is policy-documented, not an event error: re-review by a
    // different role (or an orchestrator-driven approve) is the remedy.
    if let (Some(created_role), Some(decision_role)) = (&state.creator_role, &state.decision_role) {
        if created_role == decision_role && state.effective_decision.as_deref() == Some("approve") {
            return Err(CliError::new(
                "E_GATE",
                format!(
                    "PR #{id} is self-approved: the effective approve was recorded by the \
                 same role ('{decision_role}') that created the PR; the merge gate requires \
                 an approval from a different role when roles are configured"
                ),
                id.to_string(),
                "Inspect PR review and CI state and satisfy the reported merge gate.",
                false,
            ));
        }
    }
    // Already merged: a pr.merge event exists → no double merge.
    if state.merge_result.is_some() {
        return Err(CliError::invalid(format!("PR #{id} is already merged")));
    }
    // Gate (L2): the latest CI Check must be success (the fold keeps the most
    // recently appended ci.check). A pending/absent/failed Check refuses the
    // merge BEFORE any worktree/ref side effect, and names the `ci run` step
    // as the remedy. This is purely additive to the L1 approval gate.
    match state.ci_status.as_deref() {
        Some("success") if state.ci_generation == state.generation => {}
        // A green Check from an OLDER generation validated content the PR no
        // longer holds: stale, never satisfies the gate (t1 generation
        // binding).
        Some("success") => {
            return Err(CliError::new(
                "E_GATE",
                format!(
                    "PR #{id} is not mergeable: latest CI Check is stale (run on \
                 generation {}, current snapshot generation {}); \
                 run `git forge ci run {id}`",
                    state.ci_generation, state.generation
                ),
                id.to_string(),
                "Inspect PR review and CI state and satisfy the reported merge gate.",
                false,
            ));
        }
        Some(status) => {
            return Err(CliError::new(
                "E_GATE",
                format!(
                    "PR #{id} is not mergeable: latest CI Check is `{status}` \
                 (expected `success`); run `git forge ci run {id}`"
                ),
                id.to_string(),
                "Inspect PR review and CI state and satisfy the reported merge gate.",
                false,
            ));
        }
        None => {
            return Err(CliError::new(
                "E_GATE",
                format!(
                    "PR #{id} is not mergeable: no CI Check recorded; run `git forge ci run {id}`"
                ),
                id.to_string(),
                "Inspect PR review and CI state and satisfy the reported merge gate.",
                false,
            ));
        }
    }
    Ok(state)
}

/// ── #2 identify what CI verified with the result ── `source_oid`'s tree is
/// what the latest Check validated; `result_commit`'s tree is what the
/// merge lands. Equal trees prove the Check already validated the result
/// ("prove result tree == the tree CI checked"). Differing trees require the
/// CI plan resolved from the RESULT tree to run against the result worktree
/// (`tmp`, already at the result commit) and be green; the caller routes a
/// failure through the strategy cleanup (worktree removed, no ref changes)
/// and reports the plan verdict.
pub(crate) fn verify_result_ci(
    repo: &git2::Repository,
    tmp: &std::path::Path,
    source_oid: Oid,
    result_commit: Oid,
) -> Result<(), String> {
    let source_tree = repo
        .find_commit(source_oid)
        .and_then(|c| c.tree())
        .map(|t| t.id())
        .map_err(|_| "cannot read the PR source snapshot tree".to_string())?;
    let result_tree = repo
        .find_commit(result_commit)
        .and_then(|c| c.tree())
        .map(|t| t.id())
        .map_err(|_| "cannot read the merge result tree".to_string())?;
    if source_tree == result_tree {
        return Ok(());
    }
    let plan = crate::ci::snapshot_ci_plan(repo, result_commit)?;
    let plan_label = plan.label();
    let run = crate::ci::run_ci_plan(
        repo,
        result_commit,
        tmp,
        &plan,
        crate::ci::ci_timeout(std::env::var("GIT_FORGE_CI_TIMEOUT").ok().as_deref()),
    );
    if run.status == Some(0) && !run.timed_out {
        Ok(())
    } else {
        Err(format!(
            "result CI ({plan_label}) failed against the live-base merge result: the \
             source-side Check validated a different tree; fix or re-run, then merge"
        ))
    }
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

/// Write an unappended event commit (dangling object, sole parent `parent_tip`)
/// so an entity ref can be CAS-moved to it in a later ref transaction — used
/// for both the `pr.merge` event (parent = PR head tip) and the linked-issue
/// `issue.close` event (parent = issue chain tip). The event is constructed
/// ONCE by the caller and reused across CAS retries, so a retry reparents the
/// SAME event (retained UUID + timestamp) instead of regenerating its
/// identity (F-009).
pub(crate) fn write_unappended_event_commit(
    store: &BoundEventStore,
    event: &Event,
    parent_tip: Oid,
) -> Result<Oid, StoreError> {
    let message = format!("forge:{}:{}", event.kind.as_str(), event.entity_id);
    store.write_event_commit(event, &[parent_tip], &message)
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
///
/// When the PR declares a `--closes` target, `issue_close` carries
/// `(issue_id, close_commit, issue_expected)` — the freshly-written
/// `issue.close` event commit and the issue-chain tip it was parented to —
/// and the ATOMIC batch gains a fourth CAS: `refs/forge/issues/<n>` from
/// `issue_expected` to `close_commit`. One transaction, all-or-nothing: if
/// any CAS fails, nothing applies (issue, base and PR refs all unchanged).
pub(crate) fn finalize_pr_merge(
    store: &BoundEventStore,
    spec: &MergeFinalizeSpec<'_>,
    merge_event: &Event,
    head_expected: Oid,
    issue_close: Option<(u64, Oid, Oid)>,
) -> Result<Oid, CliError> {
    let head_ref = pr_head_ref(spec.id);
    let head_new = write_unappended_event_commit(store, merge_event, head_expected)?;
    let result = pr_result_ref(spec.id);
    let base_branch_ref = format!("refs/heads/{}", spec.base_ref);
    let mut lines = vec![
        // delete pending result ref (expected presence).
        format!("update {result} {ZERO_OID} {}", spec.result_commit),
        // F-007/F-008 gate_head binding redesign (base side): CAS the base
        // branch from the GATE-VALIDATED CURRENT base tip (`gate_base_oid`,
        // #2 live-base) to result_commit — a base move after gate validation
        // fails this CAS and nothing applies.
        format!(
            "update {base_branch_ref} {} {}",
            spec.result_commit, spec.gate_base_oid
        ),
        // CAS PR head chain from the gate-validated tip to pr.merge commit.
        format!("update {head_ref} {head_new} {head_expected}"),
    ];
    if let Some((issue_id, close_oid, issue_expected)) = issue_close {
        lines.push(format!(
            "update {} {close_oid} {issue_expected}",
            issue_ref(issue_id)
        ));
    }
    store
        .store()
        .run_update_ref_stdin(&lines)
        .map(|()| head_new)
        .map_err(|error| {
            let message = error.to_string();
            let mut classified = if crate::store::expected_value_rejected(&error, &lines) {
                CliError::from(StoreError::CasConflict)
            } else {
                CliError::from(error)
            };
            classified.message = message;
            classified
        })
}

/// The gate-validated inputs of one merge completion, captured at the gate
/// read (`read_merge_gate`) and bound to the head OID it validated (F-007).
pub(crate) struct MergeFinalizeSpec<'a> {
    pub id: u64,
    pub base_ref: &'a str,
    /// The base tip the gate validated (F-007/F-008 gate_head binding redesign,
    /// base side): the finalize CAS moves `refs/heads/<base_ref>` FROM this
    /// tip — the CURRENT branch tip at gate-read time (#2 live-base), which
    /// equals the immutable snapshot base_head only when the base has not
    /// moved. A base move between gate validation and finalize fails this CAS
    /// and the whole transaction (no partial write), exactly as a head move
    /// fails the head CAS from `gate_head`.
    pub gate_base_oid: Oid,
    pub result_commit: Oid,
    pub actor: &'a str,
    pub strategy: &'a str,
    /// The head OID the merge gate validated (F-007): the finalize CASes the
    /// PR head chain FROM this tip.
    pub gate_head: Oid,
    pub closes_issue: Option<u64>,
    pub base_worktrees: &'a [std::path::PathBuf],
    /// #4 recycle: `Some((target_path, force))` when `pr merge` was given
    /// `--recycle-worktree <path>` (with its optional `--force`). `None`
    /// keeps the merge recycle-free (explicit per-merge, never hooked).
    pub recycle: Option<(&'a str, bool)>,
    /// The PR's source branch NAME (from the `pr.created` snapshot), needed
    /// by the recycle's containment check and branch deletion. `None` only
    /// for PRs without a source branch, which refuse the recycle.
    pub source_ref: Option<&'a str>,
    /// The PR's snapshot source head OID (hex), the recycle's branch-content
    /// identity anchor: the branch is deleted only when its live tip equals
    /// this (the work the PR was created from, not a moved branch) AND that
    /// tip is an ancestor of the integrated result commit. Also the F-003
    /// identity anchor: the finalize retry requires the revalidated head's
    /// source_head to equal this.
    pub source_head: &'a str,
    /// The PR's snapshot generation the merge result was built from (F-003):
    /// the finalize retry re-validates a concurrently moved head tip against
    /// THIS generation, so an in-window `pr update` that moves the PR to a
    /// new snapshot REFUSES (cleanup + no rematerialization) instead of
    /// finalizing the OLD result under the NEW generation's head.
    pub generation: u64,
    /// The prepared candidate's result commit this merge binds, when the
    /// merge went through the F-002 candidate path (`None` for the legacy
    /// path). The finalize retry cross-checks the revalidated fold's
    /// candidate identity (F-002 re-prepare): an in-window re-prepare that
    /// replaces the candidate (same generation — re-prepare does not bump it)
    /// REFUSES instead of silently finalizing under a head that approves a
    /// different candidate.
    pub prepared_result: Option<&'a str>,
}
/// Stage the linked-issue close for ONE finalize attempt (t2): fold the
/// issue chain at its CURRENT tip and decide whether the transaction needs
/// an `issue.close` at all. Returns `Some((issue_id, close_commit,
/// expected_tip))` when the issue is open — the close commit is written NOW
/// (dangling, sole parent = the expected tip, so the transaction's CAS can
/// pin it) — or `None` when the issue is already closed (idempotent no-op:
/// no second close event; the merge proceeds with base+PR actions only).
/// An absent issue ref is a hard refusal: the close cannot be anchored to a
/// real chain, and silently skipping an owed close would mask tampering.
fn stage_issue_close(
    store: &BoundEventStore,
    issue_event: &Event,
) -> Result<Option<(u64, Oid, Oid)>, String> {
    let eref = issue_ref(issue_event.entity_id);
    let tip = store
        .store()
        .current_tip(&eref)
        .map_err(|e| format!("linked issue ref read failed: {e}"))?
        .ok_or_else(|| format!("linked issue #{} ref is gone", issue_event.entity_id))?;
    let chain = store
        .read_chain(&eref)
        .map_err(|e| format!("linked issue chain read failed: {e}"))?;
    if !crate::event::fold(&chain).issue.open {
        // Already closed: the fold's `open: false` is the single source of
        // truth (last close/reopen on the chain), so no second close event.
        return Ok(None);
    }
    let close_oid =
        write_unappended_event_commit(store, issue_event, tip).map_err(|e| e.to_string())?;
    Ok(Some((issue_event.entity_id, close_oid, tip)))
}

/// ── #3 live-base refresh: after the base CAS, refresh every checked-out
/// base worktree to the result commit. The transaction moved the branch
/// ref; each worktree's HEAD is attached to it, so a bare `git reset
/// --hard` materializes the result tree without moving any ref.
/// NEVER-SPLIT POSTURE (issue #3, VAL-107): a refresh failure rolls the
/// whole completion back in ONE update-ref batch — the base branch is
/// CAS'd back to the snapshot base_head, the PR head chain is restored to
/// the tip the transaction advanced FROM, the linked issue (if closed in
/// the same batch) is re-anchored to its pre-close tip, and the pending
/// result ref is cleaned best-effort — so no ref shows a moved state with
/// a stale tree (never split), the merge reports unchanged, and the
/// failing worktree is reported as a leftover. A failed rollback is
/// reported loudly with the exact partial state, never silently.
fn refresh_base_worktrees(
    store: &BoundEventStore,
    spec: &MergeFinalizeSpec<'_>,
    head_applied: Oid,
    applied_issue: Option<(u64, Oid, Oid)>,
) -> Result<(), String> {
    let mut refreshed: Vec<&std::path::Path> = Vec::new();
    for wt in spec.base_worktrees {
        if let Err(rerr) = crate::git::refresh_base_worktree(
            wt,
            &spec.result_commit.to_string(),
            &spec.gate_base_oid.to_string(),
        ) {
            let base_branch_ref = format!("refs/heads/{}", spec.base_ref);
            let mut rollback = vec![
                // Revert the base branch CAS (result_commit → base_head).
                format!(
                    "update {base_branch_ref} {} {}",
                    spec.gate_base_oid, spec.result_commit
                ),
            ];
            // Restore the PR head chain from the tip the transaction left it
            // at (the pr.merge event commit), read fresh: the CAS refuses to
            // clobber a concurrently moved chain.
            let head_ref = pr_head_ref(spec.id);
            let head_tip = store
                .store()
                .current_tip(&head_ref)
                .map_err(|e| format!("head ref unreadable during rollback: {e}"))?
                .ok_or_else(|| "head ref vanished during rollback".to_string())?;
            rollback.push(format!("update {head_ref} {head_applied} {head_tip}"));
            if let Some((issue_id, close_oid, issue_expected)) = applied_issue {
                // F-001: the transaction moved the issue ref ONTO close_oid
                // (`update {issue_ref} {close_oid} {issue_expected}`), so the
                // re-anchor must CAS FROM close_oid — the ref's actual current
                // value — back to the pre-close tip. The old `spec.gate_head`
                // oldvalue is a PR-head OID the issue ref never held: the CAS
                // refused, the whole batch aborted mid-way, and every
                // `--closes` merge left refs advanced against a stale base
                // worktree (never-split violation).
                rollback.push(format!(
                    "update {} {} {}",
                    issue_ref(issue_id),
                    issue_expected,
                    close_oid
                ));
            }
            let rolled = store.store().run_update_ref_stdin(&rollback);
            // CR-03 (code review): earlier-refreshed worktrees sit on the result
            // commit's tree. If the refs roll back but those trees stay, the base
            // ref returns to base_oid while a checked-out worktree holds the
            // result tree — the exact ref/tree split never-split forbids, and the
            // next merge refuses a dirty base worktree forever. Restore them to
            // the pre-merge tip (bare reset: their HEAD now resolves base_oid).
            // Best-effort and named, never silent. Only after a SUCCESSFUL ref
            // rollback: with refs still advanced the worktree HEAD still resolves
            // the result commit and tree/ref agree.
            let mut stale_trees: Vec<String> = Vec::new();
            if rolled.is_ok() {
                for done in &refreshed {
                    let (okh, tip, _) = crate::git::git_in(done, &["rev-parse", "HEAD"]);
                    if !okh || tip != spec.gate_base_oid.to_string() {
                        stale_trees.push(format!("{} (HEAD not on the base tip)", done.display()));
                        continue;
                    }
                    let (ok, _, err) = crate::git::git_in(done, &["reset", "--hard"]);
                    if !ok {
                        stale_trees
                            .push(format!("{} (reset --hard failed: {err})", done.display()));
                    }
                }
            }
            let stale_note = if stale_trees.is_empty() {
                String::new()
            } else {
                format!(
                    "; additionally stale trees left at: {}",
                    stale_trees.join("; ")
                )
            };
            let pending_note = match rolled {
                Ok(()) => stale_note,
                Err(berr) => format!(
                    " ROLLBACK FAILED ({berr}): base ref is at {} — a partial \
                     state exists and must be reconciled manually{stale_note}",
                    spec.result_commit
                ),
            };
            return Err(format!(
                "merge completed but base worktree refresh failed: {rerr} \
                 (worktree left at {}){pending_note}",
                wt.display()
            ));
        }
        refreshed.push(wt);
    }
    Ok(())
}

/// The single `pr.merge` event (F-009): strategy + result_commit in the
/// body, actor from the merge's one identity resolution. Reused across CAS
/// retries so the event identity never regenerates.
fn build_merge_event(spec: &MergeFinalizeSpec<'_>) -> Event {
    let mut merge_body = HashMap::new();
    merge_body.insert(
        "strategy".into(),
        JsonValue::String(spec.strategy.to_string()),
    );
    merge_body.insert(
        "result_commit".into(),
        JsonValue::String(spec.result_commit.to_string()),
    );
    Event::new(EventKind::PrMerge, "pr", spec.id, spec.actor, merge_body)
}

/// A finalized merge: the PR is terminal and `merge_commit` is the new base
/// tip. `state` is the PR lifecycle state folded at the applied `pr.merge`
/// event. `recycle` holds the `--recycle-worktree` outcome when one was run.
#[derive(Debug, PartialEq)]
pub(crate) struct MergeOutcome {
    pub id: u64,
    pub state: &'static str,
    pub base_ref: String,
    pub strategy: String,
    pub generation: u64,
    pub merge_commit: String,
    pub recycle: Option<String>,
}

impl MergeOutcome {
    fn merged_line(&self) -> String {
        format!(
            "merged PR #{} into {} ({})",
            self.id, self.base_ref, self.merge_commit
        )
    }

    /// The human success line: the merged line, then the recycle outcome.
    pub(crate) fn prose(&self) -> String {
        match &self.recycle {
            Some(recycle) => format!("{}; {recycle}", self.merged_line()),
            None => self.merged_line(),
        }
    }
}

/// The #4 recycle epilogue, extracted from `run_finalize` (clippy
/// too_many_lines): after base refresh (#3) and finalization, run the
/// source-worktree recycle when `--recycle-worktree` was passed (recycle is
/// explicit per-merge, never hooked) and shape the merged-PR output line. A
/// refused/partial recycle does NOT undo the merge: the PR is terminal, the
/// outcome names what stayed.
fn finish_merge_output(
    store: &BoundEventStore,
    spec: &MergeFinalizeSpec<'_>,
    merge_event: Oid,
) -> Result<MergeOutcome, CliError> {
    let mut outcome = MergeOutcome {
        id: spec.id,
        state: crate::machine::pr_state_at(store.store(), merge_event)?,
        base_ref: spec.base_ref.to_string(),
        strategy: spec.strategy.to_string(),
        generation: spec.generation,
        merge_commit: spec.result_commit.to_string(),
        recycle: None,
    };
    let merged = outcome.merged_line();
    let Some((target, force)) = spec.recycle else {
        return Ok(outcome);
    };
    let source_ref = spec
        .source_ref
        .ok_or_else(|| "--recycle-worktree requires a PR with a source branch".to_string())?;
    let repo_dir = store
        .repo()
        .workdir()
        .map(|p| p.to_path_buf())
        .ok_or_else(|| "cannot resolve repository working directory".to_string())?;
    match recycle_source_worktree(
        &repo_dir,
        source_ref,
        spec.source_head,
        &spec.result_commit.to_string(),
        target,
        spec.strategy,
        force,
    ) {
        Ok(recycle) => {
            outcome.recycle = Some(recycle);
            Ok(outcome)
        }
        // CR-01 (code review): a recycle epilogue failure (phase-2 refused-dirty,
        // partial-leftover) does NOT undo the merge — the PR is terminal. Keep the
        // merged line in the error so the adapter can classify the outcome (its
        // contract: the merged line always precedes the recycle outcome, success
        // or failure); a bare Err would send a terminal merge into the adapter's
        // bridge-failure catch-all with the wrong remedy.
        Err(recycle_err) => Err(format!("{merged}; {recycle_err}").into()),
    }
}

/// ── #4 source-worktree recycle, phase 1: PRE-MERGE validation. A recycle
/// refusal must leave the merge unexecuted (PR unmerged, base ref
/// unchanged), so containment and dirtiness are checked BEFORE any merge
/// side effect — right after the checked-out base guard. Containment: the
/// path must be a REGISTERED worktree whose checked-out branch is the PR's
/// source_ref (refused-not-contained otherwise). Dirty check (untracked
/// counts, same porcelain semantics as #3): --force is the ONLY unlock
/// (refused-dirty otherwise). `force` never unlocks containment.
pub(crate) fn validate_recycle_target(
    repo_dir: &std::path::Path,
    source_ref: &str,
    target: &str,
    force: bool,
) -> Result<(), String> {
    let target_path = std::path::Path::new(target);
    let (ok_list, porcelain, list_err) = crate::git::worktree_list_raw(repo_dir);
    if !ok_list {
        return Err(format!(
            "recycle refused (refused-not-contained): git worktree list failed: {list_err}"
        ));
    }
    let head_ref = format!("refs/heads/{source_ref}");
    let on_source = crate::git::worktree_branch_of(&porcelain, target_path)
        .map(|b| b == head_ref)
        .unwrap_or(false);
    if !on_source {
        return Err(format!(
            "recycle refused (refused-not-contained): '{target}' is not a registered \
             worktree checked out on the PR source branch '{source_ref}'; \
             the worktree is untouched, the PR is not merged"
        ));
    }
    if crate::git::worktree_dirty(target_path)? && !force {
        return Err(format!(
            "recycle refused (refused-dirty): worktree '{target}' has uncommitted \
             changes; commit, stash or clean it, or pass --force to recycle anyway"
        ));
    }
    Ok(())
}

/// ── #4 source-worktree recycle, phase 2: post-finalize execution, after
/// base refresh (#3) and finalization. Removes the validated source
/// worktree and conditionally deletes its branch; never touches the base
/// worktree; salvage renaming of an unmerged branch is pipeline-side and
/// happens BEFORE this call (adapter contract clause 8). The outcome line
/// is the forge→adapter contract; the adapter consumes it verbatim and
/// persists it before clearing feature.worktree/branch. Vocabulary,
/// exactly: `removed | refused-dirty | partial-leftover(<path>) |
/// branch-deleted | branch-kept(<reason>)` — `refused-dirty` here is the
/// phase-2 TOCTOU re-check (F-003): dirt that appeared between the phase-1
/// validation and this removal refuses the removal (the merge stays
/// terminal) unless --force was passed. `refused-not-contained` remains
/// phase-1 only.
///
/// Branch deletion (VAL-111) needs BOTH: (a) the live branch tip still
/// points at the PR's snapshot source_head — a moved branch holds unmerged
/// work and is kept — AND (b) that tip is an ancestor of the integrated
/// result commit (`git merge-base --is-ancestor`), AND (c) the strategy
/// preserves the source tip in the result: `--ff`/`--merge` do (their
/// result contains the source tip), `--squash`/`--rebase` synthesize a NEW
/// commit and always keep the branch with `branch-kept(<reason>)` naming
/// the strategy. A branch that no longer exists has nothing to delete.
pub(crate) fn recycle_source_worktree(
    repo_dir: &std::path::Path,
    source_ref: &str,
    source_head: &str,
    result_commit: &str,
    target: &str,
    strategy: &str,
    force: bool,
) -> Result<String, String> {
    let target_path = std::path::Path::new(target);
    // F-003: re-run the dirty check immediately before the destructive
    // removal. Phase-1 validated a clean tree, but dirt may appear in the
    // validate→remove window (TOCTOU) — an unconditional `--force` remove
    // would silently destroy uncommitted work. Refuse (refused-dirty)
    // unless the caller passed --force; a refusal does NOT undo the merge
    // (the PR is terminal), the outcome names what stayed.
    if crate::git::worktree_dirty(target_path)? && !force {
        return Err(format!(
            "recycle refused (refused-dirty): worktree '{target}' has uncommitted \
             changes (dirt appeared after the pre-merge validation); the worktree \
             is untouched, commit/stash/clean it or pass --force to recycle anyway"
        ));
    }
    // Debug-only test seam (VAL-112): GIT_FORGE_TEST_FAIL_RECYCLE_REMOVE=<path>
    // locks THAT worktree so `git worktree remove --force` fails
    // deterministically, exercising the partial-leftover outcome. Inert in
    // release builds and when the env var is unset.
    #[cfg(debug_assertions)]
    if std::env::var("GIT_FORGE_TEST_FAIL_RECYCLE_REMOVE").as_deref()
        == Ok(target_path.to_string_lossy().as_ref())
    {
        if let Err(lock_err) = crate::git::lock_source_worktree_for_recycle(repo_dir, target_path) {
            return Err(format!(
                "recycle outcome: partial-leftover({target}): test seam failed to lock the \
                 worktree for removal-failure injection: {lock_err}"
            ));
        }
    }
    // Capture the live branch tip BEFORE removing the worktree (a
    // checked-out branch cannot be deleted while a worktree holds it).
    let live_tip = crate::git::branch_tip(repo_dir, source_ref);
    // Removal. A failure that leaves the directory OR the registration in
    // place is reported as partial-leftover(<path>) — partial removal
    // injected (VAL-112) or genuine.
    if let Err(rm_err) = crate::git::worktree_remove(repo_dir, target_path) {
        if target_path.exists() || worktree_registered_path(repo_dir, target_path) {
            return Err(format!(
                "recycle outcome: partial-leftover({target}): worktree removal failed: {rm_err}"
            ));
        }
    }
    if target_path.exists() || worktree_registered_path(repo_dir, target_path) {
        return Err(format!(
            "recycle outcome: partial-leftover({target}): worktree still present \
             after removal"
        ));
    }
    // Branch deletion (VAL-111). --squash/--rebase synthesize a NEW commit:
    // the branch is ALWAYS kept, with branch-kept(<reason>) naming the
    // strategy. --ff/--merge keep the source tip in the result, so the
    // branch is deleted only when its live tip still equals the PR's
    // snapshot source_head (a moved branch holds unmerged work) AND that
    // tip is an ancestor of the integrated result commit
    // (`git merge-base --is-ancestor`). A branch that no longer exists has
    // nothing to delete.
    if strategy == "squash" || strategy == "rebase" {
        return Ok(format!(
            "recycle outcome: removed; branch-kept({source_ref}: the {strategy} merge \
             synthesized a new commit, so the branch tip is not an ancestor of the \
             integrated result commit — the branch holds the original commits; \
             salvage renaming is pipeline-side)"
        ));
    }
    let Some(tip) = live_tip else {
        return Ok(format!(
            "recycle outcome: removed (branch {source_ref} absent)"
        ));
    };
    if tip == source_head
        && crate::git::merge_base_is_ancestor(repo_dir, source_head, result_commit)
    {
        match crate::git::branch_force_delete(repo_dir, source_ref) {
            Ok(()) => Ok(format!(
                "recycle outcome: removed; branch-deleted ({source_ref})"
            )),
            Err(del_err) => Err(format!(
                "recycle outcome: partial-leftover({target}): worktree removed but branch \
                 deletion failed: {del_err}"
            )),
        }
    } else {
        Ok(format!(
            "recycle outcome: removed; branch-kept({source_ref}: its tip is not an \
             ancestor of the integrated result commit — the branch holds unmerged work; \
             salvage renaming is pipeline-side)"
        ))
    }
}

/// Registered-worktree check helper for the partial-leftover verdict: true
/// when `git worktree list --porcelain` still registers `target`.
fn worktree_registered_path(repo_dir: &std::path::Path, target: &std::path::Path) -> bool {
    let (ok, porcelain, _) = crate::git::worktree_list_raw(repo_dir);
    ok && crate::git::worktree_registered_path(&porcelain, target)
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
pub(crate) fn run_test_barrier(env_key: &str) -> Result<(), String> {
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

/// Test-only per-attempt issue-stage barrier wrapper: park the merge between
/// ONE finalize attempt's issue staging and its update-ref batch (F-006). On
/// barrier failure (deadline) or on the pending-ref cleanup failing, the
/// error is returned with the pending result ref best-effort cleaned.
#[cfg(debug_assertions)]
fn maybe_run_issue_stage_barrier(
    store: &BoundEventStore,
    spec: &MergeFinalizeSpec<'_>,
    attempts: u32,
    lock_handle: &mut Option<std::fs::File>,
    tmp: &std::path::Path,
) -> Result<(), String> {
    if let Err(b) = maybe_run_test_issue_barrier(attempts) {
        let pending = cleanup_pending_result(store, spec.id, spec.result_commit, lock_handle, tmp);
        return Err(format!("{b}{pending}"));
    }
    Ok(())
}

/// Per-attempt issue-stage barrier (F-006): pauses right after the linked-issue
/// close is staged for one finalize attempt and before the update-ref batch —
/// the exact window where a concurrent issue write makes the batch's issue CAS
/// fail and the retry loop re-stage. The ready sentinel's CONTENT is the
/// attempt number (0 = first staging, 1 = first retry, ...), so a multi-round
/// regression never mistakes a consumed round-0 sentinel for the next round.
/// Debug builds only; inert in release and when the env var is unset.
#[cfg(debug_assertions)]
fn maybe_run_test_issue_barrier(attempt: u32) -> Result<(), String> {
    use std::io::Write;
    let Ok(dir) = std::env::var("GIT_FORGE_TEST_MERGE_ISSUE_BARRIER") else {
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
    writeln!(f, "{attempt}").ok();
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

/// Retry-window barrier (F-010): pauses right after the merge has adopted a
/// revalidated head tip (the first CAS retry) and before the retry transaction.
/// Lets a deterministic regression force-rewrite the head between the adoption
/// and the retry. Debug builds only; inert in release and when the env var is
/// unset.
#[cfg(debug_assertions)]
fn maybe_run_test_retry_barrier() -> Result<(), String> {
    run_test_barrier("GIT_FORGE_TEST_MERGE_RETRY_BARRIER")
}

#[cfg(test)]
mod tests {
    use super::MergeOutcome;

    /// The human success line stays byte-identical to the pre-JSON output:
    /// the merged line, then `; <recycle outcome>` when a recycle ran.
    #[test]
    fn merge_outcome_prose_keeps_the_human_success_line() {
        let mut outcome = MergeOutcome {
            id: 7,
            state: "merged",
            base_ref: "main".into(),
            strategy: "squash".into(),
            generation: 2,
            merge_commit: "0123abcd".into(),
            recycle: None,
        };
        assert_eq!(outcome.prose(), "merged PR #7 into main (0123abcd)");
        outcome.recycle = Some("recycle outcome: removed".into());
        assert_eq!(
            outcome.prose(),
            "merged PR #7 into main (0123abcd); recycle outcome: removed"
        );
    }
}
