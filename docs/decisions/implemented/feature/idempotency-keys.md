# Idempotency keys for create and comment commands

Status: active

## Problem

An agent that retries `issue new`, `pr create`, `issue comment` or
`pr comment` after an uncertain result (a timeout, a lost stdout, a killed
process) creates a second Issue, PR or comment. The commands had no way to
recognize a repeated request (spec #33, ticket #39).

## Decision

The four commands accept `--idempotency-key <k>`. The wire contract section
"Idempotency Keys (schema 1)" in `docs/architecture/git-forge.md` is
normative; this record gives the reasons.

**Storage.** One ref per command kind and key:
`refs/forge/idempotency/<command>/<hex>`. `<hex>` is the lowercase hex of the
key's bytes. Hex is injective, always a legal ref component, has no case
collisions on case-insensitive file systems, and cannot form a
directory/file conflict with another key. A key is any non-empty value of at
most 100 bytes, so the 200-character component stays under the 255-byte
file-name limit of the files ref backend. Arguments reach the binary as
UTF-8 (`std::env::args`), but the encoding works on bytes and needs no
character restriction.

The ref points at a root commit whose tree holds
`.forge/idempotency.json` with `v`, `command`, `key`, `inputs`, `id`,
`json` and `text`. `inputs` is the git blob id of the canonical inputs: a
JSON object of the normalized input fields in a fixed order, built with the
wire string escaper, so equal digests mean equal inputs. `json` and `text`
are the exact results the first call rendered. Re-rendering at replay time
would drift: `issue new --json` derives `children` and `blocked` from other
Issues' current state, and `pr create --json` reports the snapshot commits
of branches that may have moved since. The record is read back as untrusted
persisted state: every field must be present, typed and match the looked-up
command and key, or the command fails with `E_INVALID`.

**Inputs.** The fields that define "the same request" are the values the
command stores: title, description or body, labels in order, parent, the
blocking set, `--closes`, the target id and comment body, and for
`pr create` the source and base branch names with any `refs/heads/` prefix
removed. Branch names, not their commits, are inputs: a retry after the
source branch gained a commit is still the same request. Output mode
(`--json`) is not an input; a first call in text mode can be replayed with
`--json` and the reverse.

**Atomicity.** The record ref is created with expected absence (the zero
OID) in the same `git update-ref --stdin` start/prepare/commit transaction
that publishes the mutation: the counter and new entity refs for
`issue new` (`apply_issue_batch_claimed`) and `pr create`
(`create_pr_claimed`), and the entity head CAS for comments
(`append_event_claimed`). The record commit is written before the
transaction and is unreachable until its ref moves. A crash between "the
key is recorded" and "the mutation is applied" therefore cannot exist: both
are one transaction, with the same guarantee the existing counter-plus-entity
allocation relies on. A failed transaction publishes neither.

**Concurrency.** A keyed command looks its record up with a read-only store
before it resolves its identity, which is where the test-only write barrier
parks a command. Two concurrent first calls can both pass the lookup. Both
then build a transaction that expects the record ref to be absent; exactly
one commits. On any transaction failure the loser checks the record ref
before any retry classification. This order matters: the winner also moved
the counter or the entity head, which would otherwise read as a plain CAS
conflict and retry until exhaustion. The loser then answers from the
winner's record: the same result for the same inputs, `E_CONFLICT` for
different ones. A deterministic CLI test parks two processes at separate
write barriers, releases them in order, and checks one mutation, one
record, and identical output.

**Replay.** A replay reads only. It never resolves the identity, so it
writes nothing and does not validate `GIT_FORGE_ROLE`. A same-key loser
also validates state after its lookup (branches, `--closes`, parent,
blockers, the commented entity). When any step after the lookup fails, the
command re-reads the record (`idempotency::recheck`) and answers from it if
present, so a winner that committed and a later change to that state (a
deleted source branch, a deleted Issue ref) cannot turn the loser's replay
into `E_NOT_FOUND` or `E_INVALID`. This also covers identity resolution: a
loser with an invalid `GIT_FORGE_ROLE` replays the recorded result.

**Malformed records.** Only an absent ref means "no record". A symbolic
ref at a record path is malformed (`E_INVALID`): treating it as absent let
the claim line write through it to its target. A non-commit target or a
commit without a valid record blob is malformed too. It precedes domain
validation, so a replay succeeds after the Issue was closed, the PR
abandoned, or a branch moved or deleted. A key reused with different inputs
fails with `E_CONFLICT`, `retryable: false` (a retry with the same key cannot
succeed), the key as `input`, and `data` naming the command, the key and the
recorded id.

**Option values equal to `--json`.** Keys are opaque, so
`--idempotency-key --json` must keep the key `--json`. The shared split
`machine::json_requested` removed every `--json` token before any parser
ran, which lost such a value for every value-taking option (`--label`,
`--body`, `--reason` and others). The split now takes the command's
value-taking options (`json_requested_with`, a `const` beside each parser,
plus `--` for the two commands with end-of-options). It copies each such
option and its following token verbatim, so only a standalone `--json`
requests JSON: the rule #35 set for `-h`/`--help` values. The split records
its decision (`machine::json_mode`), and `main` uses it to decide whether a
failure prints a JSON error. The namespace dispatchers (`issue`, `pr`,
`ci`) split with `json_requested_by_dispatch`, which also treats a `--json`
in the subcommand slot as standalone, so `issue --json` is a JSON
`E_USAGE` (unknown subcommand). Only when no command grammar ran (top
level, unknown command, `forge` and `version` errors) does `main` fall back
to "any `--json` argument"; that fallback is never ORed into a grammar's
decision, which would bring back the option-value and after-`--` cases. The table in
`tests/json_option_values.rs` covers every value-taking option of every
`--json`-capable command, with and without a standalone `--json`; a new
value-taking option must be added to its command's list and to the table.

## Alternatives considered

Writing the key raw into the ref name was rejected: keys may hold `/`,
spaces, `..`, control characters or case variants that are illegal or
collide in ref names. A hash of the key as the ref name was rejected: hex
is injective without collision handling, and the 100-byte bound is ample
for UUIDs and ticket-derived keys. Recording only the result id and
re-rendering the output was rejected because of the drift above. Storing the
full canonical inputs instead of a digest would make the record larger and
add nothing that equality of the blob id does not give. A record check
inside the retry loop before each transaction was rejected as the only
guard: the transaction's expected-absence line is the guard by
construction, and the deterministic race test exercises it. Expiring keys
was rejected by the spec default (keys never expire). For `--json`
values, a `--json` arm inside each parser loop was rejected: it would move
the output-mode decision out of the shared split, change the help ordering
of every option-taking command, and leave `main` without a decision for
failures inside the loop; a second scanner of option values was rejected
for the reason #35 gave for help tokens.

## Consequences

Commands without the flag read and write no record; their output is
unchanged. Each keyed first call adds one ref and one small commit. Help
text for the four commands lists `[--idempotency-key <k>]`. Two structural
moves keep files under the size gate: `src/store.rs` unit tests moved to
`src/store/tests.rs`, and `pr create` moved from `src/cli.rs` to
`src/pr_create.rs`. Tests: `tests/idempotency.rs` (CLI case tables, races, usage errors)
and `src/store/idempotency/tests.rs` (encoding, record round trip,
all-or-nothing transactions, malformed records).
