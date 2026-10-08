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
The existing event serializer owns string escaping. No dependency is added.

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
