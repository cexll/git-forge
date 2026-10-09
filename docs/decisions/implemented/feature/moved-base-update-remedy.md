# Moved-base pr update remedy

Status: active

## Problem

`pr update` refuses when the base branch tip differs from the snapshot base.
Its message said "recreate the PR or merge manually". A manual merge bypasses
the merge gate (approval and CI bound to the merged result), so an agent that
followed the message could merge unreviewed code. The JSON error also had no
data to show which commits differ.

## Decision

`pr update` (`src/pr_merge.rs`) keeps the refusal and its order: the source
and base branches must exist (`E_NOT_FOUND`), then a moved base refuses before
the "nothing to update" check and before any write. The refusal is
`E_BASE_MOVED` with the message from spec #33:

```text
base branch '<b>' has moved since PR #<n> was created; run git forge pr prepare <n> --strategy <s> to merge the existing snapshot, or create a replacement PR from the updated source with the same --closes and abandon PR #<n>
```

Both remedies go through the merge gate. `<s>` is printed literally, because
`pr update` has no strategy to name; this matches the placeholder style of
the git-forge skill. The branch name in the message passes through
`sanitize_terminal`. The JSON error keeps `input` (the base branch name),
`hint`, and `retryable: false` from the typed-error change and adds `data`
with `pr`, `base`, `snapshot_base`, and `current_base`. The exact contract is
in `docs/architecture/git-forge.md`, "Moved base".

A base branch already deleted at the check is not a moved base. It keeps
the existing `branch '<b>' does not exist` refusal with `E_NOT_FOUND`,
because the missing branch check runs first and the spec default keeps
uncovered behavior.

The update transaction verifies the base tip that the check read. A base
that moves or is deleted after the check fails that verify, and the refusal
is `E_BASE_MOVED` with the same message (user decision on #33,
2026-10-09: a base moved or deleted after the check is a moved base). A deleted base has no
live tip, so `data.current_base` is `null`; the key stays present so that
the `data` shape does not change.

## Alternatives considered

- Expand `<s>` to `merge|squash|rebase|ff`. The spec gives the message as
  exact text, and the skill already uses `<s>` as the placeholder. Rejected.
- Replace the hint with the remedy text. The spec does not specify a hint,
  and its default keeps current behavior for uncovered questions. The
  existing hint stays; the question is recorded on ticket #37.
- Accept a moved base in `pr update`. Out of scope for spec #33.

## Consequences

`tests/moved_base_update.rs` drives the built CLI through every reachable
moved-base path: the base advanced, rewritten, or reset to an ancestor; the
source and base both moved; the base moved after an earlier update; and a
base name with a C1 control character. Each row asserts exit 1, the exact
stderr, the complete JSON error, and unchanged refs. A characterization test
pins the deleted-base `E_NOT_FOUND` refusal. `store.update_pr` verifies the
base branch ref in its transaction; two race tests move or delete the base
after the check and assert `E_BASE_MOVED` (with `current_base` null for the
deleted base) and no published generation. The "recreate the PR" texts in `pr merge` and
`pr prepare` are other commands and are unchanged.
