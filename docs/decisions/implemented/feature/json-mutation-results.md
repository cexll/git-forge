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
mutation time, not a refold that a concurrent writer could change. Every
mutation result computes `state` by folding the chain up to the event commit
the command wrote (`read_chain_at`; for `pr merge`, the applied `pr.merge`
commit). An append can re-parent onto a newer tip, so the state read before
the append can be stale; the fold at the written event includes a concurrent
terminal event that landed first, and a later append cannot change it.
`generation` and `candidate` stay the values captured before the append.

`pr merge` finalization returns a typed `MergeOutcome` instead of a prose
string. `MergeOutcome::prose` renders the unchanged human line. The JSON
result renders the same values. The merge transaction is unchanged.

A help request combined with `--json` fails with `E_USAGE` and writes
nothing. This follows the spec default for a command without a JSON result.
Each parser decides what is a help request at its own help arm, and
`cli::help_result` returns the prose usage or, under `--json`, the error.
The subcommand dispatchers test the first argument after removing `--json`,
so `--json --help` and `--help --json` behave the same. Read commands parse
their arguments before they open the repository, so the decision never
depends on being inside a repository.
An option value that equals `-h` or `--help` stays data, as before. Without
`--json`, `pr prepare` still prints help for any help token. Under `--json`,
its parser reads `--strategy --help` as a strategy value and rejects it.

## Alternatives considered

A common success envelope was rejected by the spec default: existing
machine consumers would break. Rendering merge JSON by refolding the PR after
success could not report the recycle outcome without parsing prose. Parsing
the prose line couples the machine surface to human text. A JSON help
result would add a schema that no consumer needs. A separate scanner over
all tokens duplicated the grammar and misread option values such as
`--label --help` as help requests.

## Consequences

Existing JSON fixtures are unchanged. `pr review`, `pr update` and
`pr abandon` now accept `--json` instead of rejecting it as an unknown option.
A recycle failure after a final merge still exits 1 with an `E_INTERNAL`
error; it prints no success result. Help text for the touched commands lists
`[--json]`. CLI tests in `tests/cli_json_results.rs` parse each result with an
independent JSON parser and check the exact key set.
