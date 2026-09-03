# A CI-plan file appearing in a diff without its ticket claiming it is a blocking review finding

Status: active

## Problem

`git forge ci run` takes its plan from the PR's immutable source snapshot
(`.forge/ci.sh`, else the snapshot's justfile via `just check`). The implementer
therefore controls the bytes that decide its own green. The F-012 validator
blocks indirect escapes (`import`/`mod`/`set fallback`/`set dotenv-*`/symlinks),
but the direct path remains: a branch that edits `.forge/ci.sh` (or the check
recipe inside the justfile) can weaken what CI proves — an `exit 0` plan passes
any content.

## Decision

Fix it in the review layer, not in forge: a diff touching `.forge/ci.sh` or the
`check` recipe must have a ticket whose `Touches` names that path, or the
reviewer records a **blocking finding**. The Merge Gate's `approve` — a
mechanical gate (see *Review approve is an orchestration ceremony*, same zone) —
therefore becomes force-bearing: the only party that can weaken CI must pass the
gate, and the gate is held by the orchestrator's independent review.

## Alternatives considered

- **Forge-side pinning (CI plan taken from base, not source)** — rejected:
  breaks legitimate CI-evolution PRs (changing the checks is itself a feature),
  and moves the security boundary away from review onto a snapshot-source rule.
- **Leave it unaddressed** — rejected: a green CI is an explicit gate promise,
  and a promise with a known hole that nobody wrote down gets mistaken for
  coverage by downstream automation.

## Consequences

- Reviewer checklists carry the CI-plan-change item.
- CI provenance holds as much force as review holds discipline — in this
  single-user environment that force is orchestration discipline, not human
  attestation (same caveat as the ceremony record).
- When a hostile contributor appears, revisit forge-side base-pinned CI plans.
