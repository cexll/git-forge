# Report-only work closes its Issue with an evidence comment and opens no PR

Status: active

## Problem

A mission's validation and acceptance work — the `reportOnly` features in devflow
— exercises the contract through real surfaces, commits nothing, and therefore
owns no branch and no worktree. A Pull Request requires a source branch with
commits, so this category of work has no forge representation. Leaving it
unrecorded would make `issue close` mean "someone closed it" with no trace of
what proved it, and would halve the value of the `--closes` linkage (#5).

## Decision

Forge a distinction between the two kinds of work:

- **Code work** closes through a PR: `pr create --closes <issue>` → CI → review →
  merge, and once #5 lands, its finalization transaction flips the Issue.
- **Proof work** closes through its Issue: one `git issue comment` naming the
  ledger/report path plus an overall status, then `git issue close`.

The mission's assertion ledger stays the authority for *proven*; the Issue
comment is a pointer with a summary, never a second copy of verdicts.
Ownership: the validator's Receipt carries the evidence pointer and summary,
and the orchestrator-owned adapter posts the comment and the close — validators
and the orchestrator never run `git forge` write commands themselves.

## Alternatives considered

- **PRs with an empty/OID source for proof work** — rejected: it fights the
  canonical-local-branch rule (`resolve_local_branch`) at the exact boundary that
  keeps PR snapshots gc-safe, inventing a PR whose diff is always empty.
- **No forge trace at all; close issues by hand** — rejected: `closed` with no
  reason is the same class of unanswerable question #5 was filed to remove.
- **Comment the full per-assertion report into the Issue** — rejected: it
  duplicates ledger state in prose and drifts on the first re-run.

## Consequences

- The validator's Receipt gains the evidence pointer and summary; the devflow
  adapter posts the pointer comment and closes the Issue.
- Issue chains accumulate comment-only closes for proof work; an "is this
  resolved by code or by observation?" query is a future need — the trigger for
  labels or a `resolution` field, deliberately not built now.
