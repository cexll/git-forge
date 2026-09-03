# Initial CI plan is the `just check` fallback; coverage/e2e promotion deferred

Status: active

## Problem

`git forge ci run <pr>` resolves its plan from the PR snapshot's `.forge/ci.sh`,
else the snapshot justfile's `check` recipe. The pipeline chooses today's plan
without adding a new plan file or silently upgrading which checks the Merge Gate
enforces.

## Decision

- No `.forge/ci.sh` is added. CI plan = the justfile `check` fallback, which is
  verified self-contained for F-012 (no `import`/`mod`/`set fallback`/`set
  dotenv-*`; only `set shell`).
- Coverage-94% promotion (constraints.yaml upgrade trigger "a CI runner wired to
  run `just coverage` as a blocking aggregate gate") is DEFERRED to a separate
  future decision — the trigger condition is now technically met, but the
  cost-per-ticket change is deliberate, not accidental.
- e2e (45/45 dogfood, both default branches) is DEFERRED for CI: it depends on an
  external `GDOGFOOD_SRC` repo and would make every PR's gate depend on an
  unrelated checkout.

## Alternatives considered

- **Add `.forge/ci.sh` = `just check && just coverage`** — rejected now:
  promotes coverage to a hard per-PR gate in the same change that adopts the
  pipeline; revive when a mission's assertions require it.
- **Add `.forge/ci.sh` = `just check && just e2e`** — rejected: `GDOGFOOD_SRC`
  coupling; revive when the forge gates a repo whose e2e is self-contained.

## Consequences

- Every PR's Merge Gate runs `just check`; green check == fast checks green.
- `constraints.yaml`'s coverage trigger stays documented but un-triggered; this
  record is the standing "decided, deferred" marker.
- The CI-provenance review policy (separate record, same zone) applies to changes
  of the justfile `check` recipe as the de-facto plan file.
