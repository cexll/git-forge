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
cleanup outcome. It walks the raw index entries with a nonzero stage and
groups them by exact path bytes. libgit2's conflict iterator groups stages
with the index path comparison, which is case-insensitive under
`core.ignorecase` and would fold `A.txt` and `a.txt` into one path. The classification is unchanged: a failure is a conflict
exactly when the index has an unmerged entry.

`pr prepare` uses the shared formatter in `src/cli_error.rs` to build the
`E_PREPARE_CONFLICT` error from
structured parts: PR id, strategy, base ref and tip, source commit, up to 20
conflicted paths, the remedy, and the cleanup outcome. Each path is the
git-quoted path (`core.quotePath=false`): for example `"bad\200.txt"` for
the bytes `bad<0x80>.txt` and `"new\nline"` for a path with a newline. The
same quoted form is used in the message and in `data.paths`. Invalid UTF-8
bytes are octal escapes, so the encoding is injective; git itself would emit
them raw with `core.quotePath=false`. Paths and branch names in the message
also pass through `sanitize_terminal`, so no control byte reaches the
terminal. The JSON error adds
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
- Lossy UTF-8 (`U+FFFD`) or exact paths in `data.paths`. Lossy decoding
  maps distinct invalid-byte paths to one string; exact bytes cannot be
  represented in JSON text. The git-quoted form is injective and familiar
  from `git status` and `git diff`. Chosen by the orchestrator on #36.
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
and its messages are unchanged. `pr merge` uses the same diagnostics with
`E_MERGE_CONFLICT`; `../bug-fix/merge-strategy-conflict-errors.md` supersedes
this decision's prepare-only scope without changing the prepare contract.
