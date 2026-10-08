# JSON results for mutating and diff commands

Status: active

## Problem

`issue comment|close|reopen`, `pr comment` and `pr diff` accepted `--json`
but printed prose. `pr review|update|prepare|merge|abandon` had no JSON
success result, so an agent had to parse prose to learn the new state, the
snapshot generation, the prepared candidate, or the merge commit.

## Decision

Each of these commands prints one JSON object under a bare `--json` token.
Every object opens with `id` and `state` (the entity state after the
command). Every mutation result except `pr merge` returns the appended
event commit as `event`. `pr merge` returns `merge_commit`, the new base tip,
and no `event`. Commands also return the ids they produce or bind:
`generation`, `candidate`, `result_tree`, `base_head`. `pr diff` returns the snapshot
`base` and `head` commits and the exact diff text.

Each shape has one hand-ordered template in `src/machine.rs`, like the
existing schema-1 shapes. Results use values the command captured at
mutation time, not a refold that a concurrent writer could change. `issue comment`
and `pr comment` fold the chain once after the append, because a comment does
not change the lifecycle state.

`pr merge` finalization returns a typed `MergeOutcome` instead of a prose
string. `MergeOutcome::prose` renders the unchanged human line. The JSON
result renders the same values. The merge transaction is unchanged.

A help request combined with `--json` fails with `E_USAGE` before any
command runs. This follows the spec default for a command without a JSON
result. `machine::reject_json_help` is the single check, and `main` calls it
before dispatch. A help request is a `-h` or `--help` token before any `--`
separator, or a `help` word in the subcommand position. An option value that
equals `-h` or `--help` is also refused under `--json`. After `--`, such a
token stays data.

## Alternatives considered

A common success envelope was rejected by the spec default: existing
machine consumers would break. Rendering merge JSON by refolding the PR after
success could not report the recycle outcome without parsing prose. Parsing
the prose line couples the machine surface to human text. A JSON help
result would add a schema that no consumer needs. A check inside each
command parser would need one change per help path and could miss one.

## Consequences

Existing JSON fixtures are unchanged. `pr review`, `pr update` and
`pr abandon` now accept `--json` instead of rejecting it as an unknown option.
A recycle failure after a final merge still exits 1 with an `E_INTERNAL`
error; it prints no success result. Help text for the touched commands lists
`[--json]`. CLI tests in `tests/cli_json_results.rs` parse each result with an
independent JSON parser and check the exact key set.
