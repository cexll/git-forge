# Identify Issue batch CAS rejection from the failed transaction

Status: active

## Problem

A failed Issue graph transaction can overlap another allocator. Reading moved
refs after an unrelated failure does not prove the transaction failed its
expected-old-value check. Retrying hides the original failure and can publish
an Issue after a command should have failed.

## Decision

Classify the failed `git update-ref --stdin` result in the Issue batch owner.
Run Git with `LC_ALL=C`. Recognize only its prepare-stage expected-absence
rejection or an actual OID versus expected OID rejection for a ref and expected
value in this transaction. Return `CasConflict` only for those diagnostics.
Preserve other errors without rereading refs or retrying.

This refines the retry contract in
`../feature/declarative-issue-plans-and-graph.md`; graph snapshots and atomic
publication remain unchanged.

## Alternatives considered

- Infer failure from moved refs after the command: rejected because movement
  can overlap any failure, including an injected process error.
- Treat every cannot-lock-ref message as CAS: rejected because lockfile and
  permission failures share that prefix.
- Retry unknown diagnostics: rejected because Git has no structured rejection
  code and unknown failures must remain visible.

## Consequences

`issue new` still retries recognized conflicts with a fresh complete snapshot.
A non-CAS failure with a concurrent counter move fails on the first attempted
transaction and leaves only the competitor's Issue published. Unknown Git
versions or diagnostics propagate their error instead of risking a false retry.
The real CLI regressions exercise both an injected non-CAS failure with counter
movement and a genuine competing-allocation CAS rejection.
