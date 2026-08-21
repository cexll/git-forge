# Debug seam failure routes through shared merge cleanup (F-019)

Status: active

## Problem

`cmd_pr_merge` has a debug-only test seam (`GIT_FORGE_TEST_FAIL_WORKTREE_REMOVE=1`,
inert in release builds) that locks the disposable temp worktree so `git
worktree remove --force` fails deterministically, exercising the removal-failure
branch's leftover report + best-effort pending-ref cleanup. The seam's own
failure path was not covered by that cleanup: if `git worktree lock` itself
failed (the injected lock call), `cmd_pr_merge` returned the seam error
directly — after the pending-result ref (`refs/forge/prs/<n>/result`) and the
sibling path lock already existed — so a debug-injected failure could leak both.

## Decision

The seam's lock-failure branch now routes through the same
`cleanup_pending_result` used by the other pre-finalization failure branches
(worktree removal, verification, barrier deadline): release the sibling path
lock (drop handle + remove lock file) and best-effort delete the pending
result ref (CAS expected OID), then return the seam error with the same
"worktree left at ..." framing as its sibling. This does NOT change the
finalize-transaction failure path, which deliberately leaves
`refs/forge/prs/<n>/result` in place and reports it (the merge may have
committed; deleting the pending ref there would strand the result commit).

## Alternatives considered

- **Leave the path as-is**: it is debug-only and hard to trigger, but it is
  exactly the class of gap the code-review self-check exists to close; a
  leaked pending ref or path lock inside a merge is an irreversible-repo
  hazard even in a test build.
- **Re-issue the lock call inside the seam**: makes the injection less
  deterministic (depends on what failed) without removing the leak.

## Consequences

- The pre-finalization failure branches in `cmd_pr_merge` (worktree removal,
  verification, barrier deadline, seam lock failure) all release the sibling
  lock and best-effort remove the pending ref; the finalize-transaction path
  retains the ref by design (reported, not cleaned).
- A deterministic regression (`seam_lock_failure_releases_lock_and_cleans_pending_ref`)
  uses the same PATH-shim pattern as the F-008 verification test: the shim
  intercepts `git -C <repo> worktree lock <tmp>` and exits 128 with a
  distinctive stderr; the test asserts merge fails with that stderr, the
  pending `/result` ref is gone, and the leftover temp worktree has no sibling
  `.lock` file. The test was red before the fix (pending ref survived) and is
  green after.