# Prepare conflict diagnostics

Status: active

## Problem

A conflicting `pr prepare` printed `git merge failed: ;`. `git merge` writes
its conflict report to stdout, and the command kept only stderr. The
failure named no conflicted file and no remedy, so an agent could not
resolve the conflict on the ticket branch without rerunning git by hand.

## Decision

The strategy adapter (`src/git/strategy.rs`) reads the unmerged index
entries of the temporary worktree before cleanup and returns them with the
cleanup outcome. The classification is unchanged: a failure is a conflict
exactly when the index has an unmerged entry.

`pr prepare` (`src/pr_prepare.rs`) builds the `E_PREPARE_CONFLICT` error from
structured parts: PR id, strategy, base ref and tip, source commit, up to 20
conflicted paths, the remedy, and the cleanup outcome. The JSON error adds
`data` with `pr`, `strategy`, `base`, `source`, and every conflicted path.
`CliError.data` is an ordered field list, so the keys render in a fixed
order. The exact text is in the wire contract (`docs/architecture/git-forge.md`,
"Prepare conflict").

The remedy follows spec #33: merge the base into the source branch, resolve,
then run `pr update` or create a replacement PR.

## Alternatives considered

- Forward git's stdout and stderr. Rebase stderr tells the user to run
  `git rebase --continue` in a worktree that cleanup has already removed. The
  report is also unbounded for many paths. Rejected.
- Parse `CONFLICT (...)` lines from git's output. The wording varies by git
  version and a hook can write unmerged entries without such a line. The
  index is the authoritative source. Rejected.
- Limit `data.paths` to 20 as well. The spec limits only the message; a
  machine consumer needs the complete list. Rejected.

## Consequences

Every prepare conflict path (merge, squash, and rebase; content, add/add,
modify/delete, rename/delete; a pre-merge-commit or pre-commit hook that
writes unmerged entries) has a CLI test in `tests/prepare_conflicts.rs`
asserting exit code, stderr, the complete JSON error, unchanged refs, and no
leftover worktree. `ff` has no conflict path; its precondition refusal is
pinned unchanged. Failures without unmerged entries keep their
`git <s> failed: <cause>` message and `E_INTERNAL`. Temporary worktree cleanup
and its messages are unchanged. `pr merge` strategy execution still reports a
conflict as `E_INTERNAL` with git's stderr; that path is outside this
decision.
