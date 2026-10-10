# Merge strategy conflict errors

Status: active

## Problem

The unprepared `pr merge` strategy path converted `StrategyFailure.message`
to `CliError`, discarding conflicted paths and returning `E_INTERNAL`.
Merge and squash conflicts could print `git <strategy> failed: ;` because
Git writes the conflict report to stdout. Rebase diagnostics referred to a
temporary worktree already removed by cleanup.

## Decision

Return `E_MERGE_CONFLICT` when failed merge, squash, or rebase execution
leaves unmerged index entries. Keep `E_PREPARE_CONFLICT` restricted to
`pr prepare`. The architecture code table defines it as candidate preparation;
`../feature/prepare-conflict-diagnostics.md` explicitly excluded merge.
This decision supersedes only that prepare-only scope, not its diagnostics.

Both commands use the conflict formatter in `src/cli_error.rs` with an
explicit `ConflictOperation`. Codes and operation wording differ; paths,
terminal escaping, the 20-path message cap, complete ordered `data`, remedy,
and cleanup clause are shared. `src/git/strategy.rs` remains the sole owner
of index classification, git quoting, and worktree cleanup. Strategy cleanup
unlinks the sibling path lock once; the merge caller's handle drops on error
return without unlinking a path another invocation might have reacquired.
Failures without conflict paths keep their operational `E_INTERNAL` error.
Hooks leaving unmerged entries receive the conflict code; this existing
classification does not distinguish hooks from tree conflicts.

The failure occurs before pending-result publication and finalization.
It publishes no merge event and changes no refs or existing user worktrees.
Cleanup failures retain the adapter's leftover report.

## Alternatives considered

- Reuse `E_PREPARE_CONFLICT`. Rejected: the documented code is command-specific,
  and #36 explicitly excluded merge. A distinct code preserves that boundary.
- Duplicate the formatter in `pr_merge`. Rejected: quoting, truncation,
  sanitization, and remediation would have two owners and could drift.
- Forward Git's output. Rejected: rebase continuation instructions refer to
  the removed worktree, and output is not a stable structured conflict list.

## Consequences

CLI tests in `tests/prepare_conflicts.rs` exercise merge, squash, and rebase
without a candidate and with an unmoved base. Each asserts the full JSON
error and exact stderr in JSON and human modes, all refs, worktree
registrations, user HEAD/index/status/content, and no temporary directory or
sibling lock for the command process. Conflicts include an ordinary path
and a filename containing newline and quote bytes.

`ff` can reach execution only when the base tip is an ancestor of the source.
It fast-forwards without merging trees, so it cannot create an unmerged index.
The divergent-history CLI test pins its existing precondition error, complete
JSON, stderr, and unchanged state. Prepared-candidate merge does not execute
a strategy and cannot raise `E_MERGE_CONFLICT`.

The external skill reference `~/.agents/skills/git-forge/references/wire-contract.md`
needs the CLI failure envelope and `E_MERGE_CONFLICT` payload documented.
That reference is read-only for this ticket; the repository architecture
contract owns the implemented behavior.
