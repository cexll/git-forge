# Abandoned PRs are terminal for merge, review and CI; no reopen in L1

Status: active

## Problem

The pipeline discards attempts: a feature cancelled after retries, a branch
kept for history, an attempt superseded by a recreate. L1 PR state has no
"this will never ship" terminal state, so discarded PRs stay open forever and
`pr list` accumulates dead entries with no recorded reason.

## Decision

`pr.abandoned` is an event, and abandoned is a terminal state for the entity:

- `pr merge`, `pr review` and `ci run` each REFUSE an abandoned PR, naming the
  abandon reason from the event.
- No reopen in L1: proposing the same work again means a NEW PR. (A recreated
  snapshot is a different entity; see `forge_pr_superseded` in the adapter
  contract for replaced-snapshot attribution.)
- Abandon refuses when the PR is already merged, and refuses a second abandon
  (idempotence via the event chain, never a state edit).
- Every refusal leaves refs unchanged.

## Alternatives considered

- **Allow reopen to restore eligibility** — rejected: reopen needs decision
  invalidation semantics (old approvals, old CI checks revivable?) that L1 has
  no machinery for; the entity chain cannot express "abandoned then revived"
  without a new state lattice. Revival trigger: a workflow that reuses a
  discarded PR identity by design.
- **No terminal state (current L1)** — rejected: dead PRs accumulate with no
  recorded reason; the pipeline's failed attempts are exactly the workload that
  surfaces this.

## Consequences

- PR lifecycle becomes open → merged | abandoned; both terminal.
- The pipeline maps cancelled features to abandon (#6), recreated snapshots to
  supersede (`forge_pr_superseded`, budget-neutral), keeping the two concepts
  distinct in the audit chain.
- Wire-schema encoding (event body fields, fold terminality rule) lands with
  issue #6's spec.
