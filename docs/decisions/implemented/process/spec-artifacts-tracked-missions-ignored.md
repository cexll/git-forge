# Spec artifacts are tracked; only the mission runtime stays gitignored

Status: active

## Problem

devflow's `propose` refuses a contract or plan the repository does not track
(`contract_not_tracked` / `features_source_missing` guards): a contract inside
the repository that git does not track has no history to compare its digest
against and is deleted by `git clean -fdx`. This repository declared the
opposite policy: `.gitignore` carried `/.specs/` and AGENTS.md's Directory Map
said `.specs/` is "untracked — gitignored, never committed". The two rules
cannot both hold, so no new devflow mission could legitimately be proposed in
this repository — the existing `.specs/*/missions/` directories predate the
tracking guards.

## Decision

Flip the repository policy to the combination both tools already assume:

- `.specs/<session>/` spec artifacts (`spec.md`, `tickets/`, `features.json`,
  `validation-contract.json`) are **committed** — they are the plan of record,
  digest-pinned by every mission, and belong to the reviewable history.
- `.specs/*/missions/` (state.json, progress log, handoffs, reviews, briefs) and
  `.specs/*/evidence/` stay **gitignored** — they are execution churn the scheduler
  owns and validator by-products; committing them would dirty the integration tree
  every round. The durable outcome of a proof round is the assertion ledger (derived
  from the progress log) plus flow contracts promoted into `.agents/qa/flows/`, not
  the raw screenshots and stdout that produced it.

to-spec is designed to write a `.specs/.gitignore` containing `*/missions/` for
exactly this split; it is absent from this repository (the existing sessions
predate the rule), so the top-level patterns now encode both `missions/` and
`evidence/` for them.

## Alternatives considered

- **Keep specs untracked, place missions outside the repository**
  (`--mission-dir` external) — rejected: the spec is the contract the mission
  verifies against; kept outside git it cannot be diff-reviewed, digest-pinned
  against history, or survive `git clean`, and contradicts the handoff
  discipline to-spec was written around.
- **Track everything including `missions/`** — rejected: worker rounds write
  runtime state continuously; a tracked runtime dirties the integration tree
  between spawns and self-locks the pipeline on `dirty_worktree`.

## Consequences

- `.gitignore` pattern `/.specs/` → `/.specs/*/missions/`; AGENTS.md Directory
  Map updated in the same change.
- Seven session directories exist under `.specs/`; one (`spec-align`) removed
  its directory on 2026-08-28 per its own lifecycle record. Any mission whose
  tracked plan is gone is an orphan (no retroactive recovery is claimed for the
  others) — see `spec-align-orphan-mission` for the same class of gap.
- Any future tooling that re-cleans `.specs/` must respect the split: removing
  an ignored-but-tracked-adjacent spec directory is now a git-visible change.
