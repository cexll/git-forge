# Standalone six-state recycle outcome line

Status: active

## Problem

Issue #50 identifies recycle prose that cannot be consumed verbatim as the
six-state outcome required by forge-p1 VAL-109..112 and the adapter contract,
clause 8. The acceptance records remain unchanged.

## Decision

Preserve the existing `recycle outcome: ` discovery prefix. Emit one separate
line. Compose successful worktree removal and branch disposition with `; `,
as VAL-109 requires `removed` plus `branch-deleted`. Do not append a branch
name to `branch-deleted`. Keep retention explanations inside `branch-kept(...)`.

An absent source branch under any strategy maps to `removed` alone. The git adapter
already defines absence as nothing to delete. This does not invent a seventh
state or claim a deletion that did not occur.

Success stdout places the merged message before the outcome line. Failure
stderr places diagnostic detail before the outcome line. Post-finalize failures
also retain the merged message because the merge is terminal. Non-JSON failure
stdout remains empty, as required by the CLI Failure Contract.

The existing JSON success `recycle` string contains the exact line. Additive
JSON error `data.recycle` contains the same line, using the existing `CliError`
structured-data mechanism. Control characters in outcome paths use the existing
terminal sanitizer so the result remains one line; ordinary paths stay exact.

Do not change containment, dirty rechecks, branch-deletion predicates, merge
finalization, or base refresh ordering. Salvage remains adapter-owned.

The bundled adapter consumes refusal states from the standalone line, not
legacy human prose. It persists the exact refusal line and keeps worktree
recovery data in both phases. Only a post-finalize refusal records a terminal
merge. Its fixture emits the same current wire format.

## Alternatives considered

- Keep outcome text embedded in merged prose: requires adapter normalization
  and violates the ticket's standalone structured-line requirement.
- Add branch-absent as a state: violates the closed six-state vocabulary.
- Report branch-deleted for an absent branch: claims an operation that did not
  happen.
- Print prose on failure stdout: violates the existing CLI failure contract.
- Change acceptance records: hides the implementation defect.

## Consequences

CLI regressions exercise all six states in human and JSON modes, including the
actual path on injected partial removal and base-worktree state. Existing
strategy, force, wrong-branch, and phase-2 dirty coverage remains in place.
Repository wire documentation and the QA driver use the exact line format.
The installed user-level skill contract is read-only and requires a separate
owner update, recorded on Issue #50.

The repository's `.forge/ci.sh` runs the bundled adapter matrix after `cargo test`.
Node.js is required. The plan adds Homebrew's bin and Node install directories
to `PATH` so adapter fixture subprocesses can also resolve `node`. A missing
Node executable fails the plan with a diagnostic instead of skipping the matrix.
