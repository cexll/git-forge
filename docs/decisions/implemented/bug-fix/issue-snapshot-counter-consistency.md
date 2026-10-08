# Keep Issue snapshot allocation reads consistent

Status: active

## Problem

Reading the counter before enumerating Issue refs can cross a concurrent atomic
allocation. The resulting snapshot contains Issue N while its counter still
selects N. Both `issue new` and `issue apply` then report a false collision before
publication CAS can detect the concurrent writer.

## Decision

Bracket Issue enumeration and folding with counter-tip reads in `issue_snapshot`.
Discard the complete snapshot when the counter moved. Use `MAX_ALLOC_RETRIES` to
bound the read loop and return `CasConflict` on exhaustion. Handle that result
inside `issue new`'s existing allocation retry loop. Apply still reports conflicts.

Keep stable-counter collisions fatal. Preserve the transaction diagnostic
classification in `issue-batch-cas-rejection.md`; this change adds an identified
read conflict, not broader retry classification for transaction failures.

## Alternatives considered

- Retry a collision in callers: rejected because a stable corrupt counter must
  not be treated as concurrency, and both callers need a consistent snapshot.
- Read the counter only after Issue refs: rejected because it can omit a newly
  allocated Issue while accepting the advanced counter.
- Retry snapshot reads without a bound: rejected because continuous allocations
  must produce a conflict rather than keep the command running indefinitely.
- Lock the whole graph during reads: rejected because publication already verifies
  observed Issue tips and the counter with one transaction.

## Consequences

Production changes are confined to snapshot stabilization and its caller error
handling. No ref publication, event schema, or Git diagnostic parsing changes.
Existing Issue tip changes still rely on publication CAS; reads do not lock refs.
The append-only counter chain distinguishes allocations by tip identity.

A thread-local unit-test hook runs between the two read phases and is absent
from non-test builds. Real competing allocations cover absent and existing
counters, bounded read conflicts, and caller retries. The caller test runs in an
isolated child test process to avoid changing the parent process directory.
Real Git diagnostics and CLI publication retries cover existing-counter CAS.
