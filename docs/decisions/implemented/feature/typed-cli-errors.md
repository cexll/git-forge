# Typed CLI failures

Status: active

## Problem

Missing PR positionals caused slice panics. Error strings did not expose a
stable class to CLI consumers. Stringifying storage failures discarded CAS
retry information.

## Decision

Command handlers return `CliError`. Argument parsers assign `E_USAGE` where
input fails. Entity checks assign `E_NOT_FOUND`. Storage errors convert at the
CLI boundary, retaining `E_CONFLICT` and `retryable` for CAS failures.
`main` renders one JSON failure object when the invocation contains a bare
`--json` token. `Display` retains the human stderr message and exit 1.
The existing event serializer owns string escaping. CLI tests use the
dev-only `serde_json` parser to check complete error documents independently
of that serializer. Production dependencies are unchanged.
The parser is available under MIT. Reusing the production parser would weaken
oracle independence; extending it to parse arbitrary CLI documents is not needed.

Branch syntax is validated before lookup: valid absent local branches return
`E_NOT_FOUND`; noncanonical inputs remain `E_INVALID`. Finalization retains
typed CAS classes while adding human context. Git's recognized expected-value
rejection identifies a CAS failure; concurrent movement alone does not.
The transaction adapter exposes stderr rather than structured Git rejection
codes. An exact missing-reference diagnostic for an update with a nonzero
expected OID identifies deletion of an expected-present ref. Unknown process
failures remain internal errors. Finalization has no expected-absent operations;
the separate pending-result publication owns that creation expectation.
Candidate currentness, strategy binding, and gates precede merge-base and
strategy preconditions on all four merge strategies. Candidate staleness and
gate refusals return `E_GATE` with the existing human remedy. Preparation returns
`E_PREPARE_CONFLICT` only when the failed strategy leaves an unmerged index,
checked before cleanup. A failing hook that leaves unmerged entries receives
the same code; hook and process failures without such entries remain `E_INTERNAL`.
Conflicted-path diagnostics and conflict-specific remedies belong to the
separate preparation-diagnostics contract.

## Alternatives considered

Classifying messages in `main` would couple machine classes to mutable prose.
Changing storage errors to depend on the CLI would reverse module ownership.
A common success envelope would break existing machine consumers and is not
part of this change.

## Consequences

Command-specific failures can attach stable codes without altering stderr.
The wire contract defines required fields and permits additive diagnostic
`data`. Success results and merge/storage transaction behavior are unchanged.
Binary-boundary tests cover every no-argument subcommand, missing entities,
unknown options, and a competing real allocation that causes an atomic apply
CAS conflict without publishing requested Issues.
The error oracle checks exact keys, types, values, and equality with independently
expected stderr. Mutation qualification rejects malformed JSON, changed messages,
extra keys, and wrong types. Literal PR diff usage has a dedicated boundary check.
Merge and prepare normalize `--json` without changing their human success output.
Strategy execution and pre-cleanup classification live in `src/git/strategy.rs`.
Atomic retry orchestration and adopted-input validation live in
`src/merge_state/finalize.rs`. Their parent modules retain the existing API.
These private modules keep both owners below the mandatory per-file size gate.
Table-driven CLI regressions cover every merge strategy against candidate
staleness and binding failures. A second matrix covers moved and deleted
expected-present finalization refs, including linked-Issue retry exhaustion,
complete JSON failures, and atomic refusal with pending-result cleanup.
