# CI Check in the Merge Gate

Status: active

## Problem

L1's Merge Gate blocks `git forge pr merge` only on an approved Review event
(`src/pr_merge.rs`). The project needs a git-native CI whose outcome gates merge
— the git-native analogue of GitLab's "merge when pipeline succeeds" — while
honouring the **zero resident processes / git-protocol-only** constraint: no
server, no daemon, nothing runs unless a developer invokes it.

## Decision

Implement a git-native CI run (all on-demand, no daemon), now shipped in L1:

- **Plan**: a per-repo shell script `.forge/ci.sh`; when absent, `git forge ci
  run` falls back to `just check`. This keeps any repo able to provide a plan,
  while git-forge itself is zero-config. A `.forge/ci.sh` that is a symlink or
  otherwise not a regular file is **refused up front** (F-001) — the plan is
  resolved before any temporary worktree is created or any Check is recorded, so
  CI never follows a tracked link to mutable bytes outside the snapshot.
- **Trigger**: on-demand only. `git forge pr create` publishes a `pending`
  `ci.check` marker (no plan executes at creation); `git forge ci run <pr>`
  explicitly executes the plan against the PR's immutable source snapshot. No
  git hook auto-runs CI.
- **Result**: a **CI Check** Forge Event (`status`: pending/success/failed, plus
  `plan`, timestamp, actor). A failing plan still records a `failed` Check
  before the command exits nonzero. The Merge Gate reads only the latest status;
  a re-run appends a new CI Check and fold keeps the latest.
- **Gate**: the Merge Gate (in L1) requires an approved Review **and** the latest
  CI Check == success. Direct `git merge` remains documented as bypassable, as in
  L1.

## Alternatives considered

- **Justfile-only plan** (`ci run` always runs `just check`) — rejected: not
  general for non-`just` repositories, and it couples CI to git-forge's own
  build surface.
- **Config-file plan** (`.forge/ci.yml` steps) — rejected: parallel to justfile
  and to a shell script; more format surface for no added capability.
- **Hook auto-trigger on PR create** — rejected: an auto-running hook violates the
  spirit of CI being purely on-demand and would run an uninvited process at PR
  creation. PR creation merely surfaces a pending Check; `git forge ci run` is
  the only trigger.
- **Explicit-only trigger with no pending marker** — rejected: a freshly created
  PR would show no Check at all, obscuring that CI is expected before merge.
- **Rich status event** (log tail, duration, run-id blob ref) — rejected:
  the Merge Gate needs only status; logs are bloat and need ref management.
- **CI as advisory** (record but never block) — rejected: does not gate.
- **Broaden Merge Gate into one undifferentiated term** — rejected: blurs
  approval vs CI; a distinct **CI Check** term keeps the two conditions crisp.

## Consequences

- `CONTEXT.md` gains **CI Check** and the **Merge Gate** definition broadens to
  "approved Review and latest CI Check green" (both shipped in L1).
- Event schema gains a CI Check kind; the fold maps it into PR state (latest
  status) so the gate predicate can read it.
- The Merge Gate predicate requires the latest CI Check == success (with a
  `pending` marker on create), so `git forge pr merge` refuses a
  pending/failed/absent Check and names `git forge ci run <pr>` as the remedy.
- Zero-daemon invariant preserved: CI runs only when the CLI invokes it.
