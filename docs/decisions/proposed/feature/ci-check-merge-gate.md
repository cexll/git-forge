# CI Check in the Merge Gate (L2)

Status: proposed

## Problem

L1's Merge Gate blocks `git forge pr merge` only on an approved Review event
(`src/pr_merge.rs`). There is no CI: no executable plan, no status result event,
no gate integration. CI execution is explicitly a **Must-not-have (L1)** in the
architecture doc, which plans L2 as "CI plan format (shell script in repo vs
config file) and status event schema". The open question "does PR creation
auto-trigger CI" currently answers **no by design**.

L2 needs a git-native CI whose outcome gates merge — the git-native analogue of
GitLab's "merge when pipeline succeeds" — while honouring the project's
**zero resident processes / git-protocol-only** constraint.

## Decision

Add a git-native CI run (all on-demand, no daemon):

- **Plan**: a per-repo shell script `.forge/ci.sh`; when absent, `git forge ci
  run` falls back to `just check`. This keeps any repo able to provide a plan,
  while git-forge itself is zero-config.
- **Trigger**: a git hook auto-runs the plan when a PR is created (committing
  its Forge Event); `git forge ci run <pr>` re-runs it explicitly. Both are
  process-per-invocation — no resident process.
- **Result**: a **CI Check** Forge Event (`status`: pending/success/failed,
  plus plan, timestamp, actor). The Merge Gate reads only the status; a checkout
  may re-run and append a new CI Check (fold keeps the latest).
- **Gate**: in L2, the Merge Gate requires an approved Review **and** the latest
  CI Check == success. Direct `git merge` remains documented as bypassable, as
  in L1.

## Alternatives considered

- **Justfile-only plan** (`ci run` always runs `just check`) — rejected: not
  general for non-`just` repositories, and it couples CI to git-forge's own
  build surface.
- **Config-file plan** (`.forge/ci.yml` steps) — rejected: parallel to justfile
  and to a shell script; more format surface for no added capability.
- **Explicit-only trigger** (no hook) — rejected: fails the "PR creation
  auto-triggers CI" requirement; hook plus explicit re-run is the on-demand
  compromise.
- **Rich status event** (log tail, duration, run-id blob ref) — rejected:
  the Merge Gate needs only status; logs are bloat and need ref management.
- **CI as advisory** (record but never block) — rejected: does not gate.
- **Broaden Merge Gate into one undifferentiated term** — rejected: blurs
  approval vs CI; a distinct **CI Check** term keeps the two conditions crisp.

## Consequences

- `CONTEXT.md` gains **CI Check** and the **Merge Gate** definition broadens to
  "approved Review and (L2) latest CI Check green".
- Event schema gains a CI Check kind; the fold maps it into PR state (latest
  status) so the gate predicate can read it.
- The Merge Gate predicate in `src/pr_merge.rs` is extended (L2) past the L1
  approval-only check.
- Zero-daemon invariant preserved: hooks and CLI are the only triggers.
