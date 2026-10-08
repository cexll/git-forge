# CI failure diagnostics

Status: active

## Problem

A failed `ci run` printed only `CI run for PR #<n> failed (<plan>)` and at
most an exit status. The plan's stdout and stderr went to the null device,
and a plan refused before selection surfaced as `E_INTERNAL` without a Check.
An agent could not tell a broken plan from a slow one or a failing test, so
it reran CI blind. Spec #33 requires a typed reason and the last 4 KiB of
combined output with the CI result.

## Decision

`src/ci.rs` returns a `CiRun` whose `failure` is `None` only for a green run.
A failure carries exactly one `CiReason` and the human detail. The reason is
the first cause in run order:

| Failure | Reason |
|---|---|
| No plan, symlink or non-regular plan (`snapshot_ci_plan`) | `plan_invalid` |
| `just check` justfile fails `validate_justfile_closure` | `plan_invalid` |
| Plan materialization I/O, no trusted `just`, output pipe, spawn | `spawn_failed` |
| Deadline hit (whatever the exit status) | `timeout` |
| Non-zero exit with the process group proven empty | `nonzero_exit` |
| Leader terminated by a signal with the group proven empty | `nonzero_exit` |
| Background process survived, group unprovable, failed wait | `cleanup_failed` |
| Worktree removal failed after an otherwise green run (F-025) | `cleanup_failed` |

When the plan failed and the worktree removal also failed, the plan's reason
wins and the message keeps the leftover text. A plan refused before
selection now appends a `failed` Check without `plan`, so the latest Check
always explains the latest run. Failures that are not a CI result keep their
codes and write no Check: a missing or abandoned PR, a missing snapshot ref,
a temporary worktree that cannot be created, and a failed Check append.

The plan's stdout and stderr share one pipe. A reader thread
(`src/ci/output.rs`) drains it and keeps at most 4097 bytes; the extra byte
records that the output was cut. The tail is the last 4096 bytes; a cut
inside a UTF-8 character drops the partial character; invalid bytes become
U+FFFD; the text is trimmed again so the stored tail is at most 4096 bytes.
After the run, `ci run` waits at most about one second for end of file.

`record_and_report_ci_check` (`src/pr_prepare.rs`) stores `reason` and `tail`
in the `ci.check` body of a failed Check. `E_CI_FAILED` adds `data` with
`pr`, `reason`, and `tail`; the message appends `; reason: <r>` and, when the
tail is not empty, `; output tail: <tail>` through `sanitize_terminal`. The
fold exposes `ci_reason` and `ci_tail`; `pr create|show --json` render them
in the `ci` member as strings or `null`. The merge gate is unchanged: only a
`success` Check bound to the current generation and candidate passes. The
exact wire text is in `docs/architecture/git-forge.md`, "CI failure".

## Alternatives considered

- Write the output to a temporary file and read its end. Memory stays
  bounded, but disk use does not, and an escaped descendant can keep writing
  after cleanup. Rejected.
- Git-quoted or `\xNN` escapes for invalid bytes, as #36 chose for paths.
  Path identity needs an injective encoding; a diagnostic tail does not, and
  escapes would make ordinary text harder to read and grow up to fourfold.
  U+FFFD keeps the text readable and the JSON valid. Chosen: lossy decoding,
  recorded as a Known Limitation.
- A sixth reason for a signal kill or for a surviving background process.
  The spec fixes five reasons. A signal is a non-success exit of the plan
  itself; a surviving process is a failure to clean up the plan's processes.
  The gate refuses every non-success Check, so the label never changes a gate
  decision. Rejected.
- Omit `reason` and `tail` from `pr show --json` when absent. The machine
  surface fixes the key set per template; `null` keeps it fixed. Rejected.

## Consequences

`tests/ci_diagnostics.rs` covers every reason at the CLI boundary: exit code,
exact stderr, the complete JSON error, the stored Check, `pr show --json`, and
the merge gate refusing the failed Check. Module tests in `src/ci/tests.rs`
pin the byte bound, UTF-8 cut, U+FFFD bound, exit-status mapping, terminal
escaping, and the in-memory bound of the capture (F-016).

The F-016 regression `ci_run_detects_unbounded_output_capture` asserted that
the plan's stdout and stderr were not pipes, as a proxy for unbounded
`Command::output()` buffering. Capturing the tail makes them a pipe, so that
oracle now checks a 32 MiB plan: the run drains the pipe (`nonzero_exit`, not
`timeout`) and stores exactly the last 4096 bytes. The heap bound moved to the
`Capture` module test, which fails if the kept buffer grows past 4097 bytes.

`cli_errors.rs::failed_ci_has_json_error_and_records_failed_check` and the
`pr-create.json` and `pr-show.json` fixtures changed for the new message,
`data`, and `ci` keys.
