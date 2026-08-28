# devflow has no lifecycle for an orphaned initializing mission without its features source

Status: active

## Problem

`.specs/git-forge-spec-align/missions/state.json` holds a mission stuck in
`state: "initializing"` with `featuresSourcePath` pointing at
`.specs/git-forge-spec-align/features.json`, a file that does not exist. The
mission never produced a milestone, artifact, or committed feature; it is an
orphan left on disk by a scheduler defect (the spec was superseded by
`git-forge-e2e-gate` per `ddbfb19`, and its plan source was then removed).

Once the features source is gone, **no devflow subcommand can operate on it**:

- `cancel` and `next-feature` fail with `features_source_missing` because
  `loadMission` refuses every command when `featuresSourcePath` is missing.
- `reconcile` additionally requires `state.state === "completed"`, which a
  stuck-initializing mission can never reach.
- There is no mission-level cancel/drop/abandon command — cancel is
  feature-scoped (`cmdCancel` takes `--feature`, and needs `featuresDoc`
  present to look the feature up).

So the directory is unreachable except by hand-editing scheduler state, which
the agent trust boundary forbids.

VAL-202 is the only assertion recorded against this orphan's
validation-state.json and it sits `pending`. That is a **false negative**: the
assertion is functionally owned by the `git-forge-e2e-gate` mission, where it
is `passed` with three evidence artifacts
(`.specs/git-forge-e2e-gate/evidence/assertions-e2e-gate/VAL-202-{e2e,grep,check}.txt`).
The e2e gate it describes is real and enforced (`just e2e` → `dogfood-all`).

## Impact

- None on build, tests, or functionality: the orphan directory is entirely
  gitignored (`.specs/git-forge-spec-align/` is not tracked by git, and
  `missions/` is in `.gitignore`), so it carries no versioned content and is
  invisible to `just check`/`just test`/`just e2e`.
- Cosmetic only in a spec ledger sweep: it shows as one lingering `pending`
  assertion that is actually proven elsewhere.

## Decision

Treat this as a **tool lifecycle defect, not a project feature gap**, and
record it as **unrecoverable via the current devflow interface**:

- Do NOT hand-edit or delete the orphaned `missions/` files (scheduler-owned,
  and deletion is a destructive action on code this repo did not author — it
  needs explicit maintainer approval).
- The assertion (VAL-202) is already proven by its rightful owner
  (`git-forge-e2e-gate`); the orphan's `pending` copy is stale and carries no
  functional weight.
- Recovery is a devflow.cjs enhancement: add a mission-level
  abandon/drop command that can retire an `initializing`/`features_source_missing`
  orphan (removing or marking the record) without requiring a plan file. Until
  then the directory is left in place and documented here.

## Alternatives considered

- **Hand-edit `state.json` to `completed`/`cancelled`** — rejected: violates the
  trust boundary (scheduler state is written only through devflow subcommands),
  and would fabricate a lifecycle the tool never actually ran.
- **Delete `.specs/git-forge-spec-align/` outright** — rejected for now: it is
  a destructive action on scheduler-generated data; without a devflow command the
  deletion is not traceable in the mission log and could mask the tool defect we
  want to steer future work away from. Keep it, document it.
- **Force-record the orphan VAL-202 as passed via `record-verdicts`** — rejected:
  that command targets a mission's own validation-contract, and the orphan has no
  valid contract/features to record against; it would also duplicate the real
  e2e-gate provenance.

## Consequences

- Spec ledger interpretation: treat the single `pending` (spec-align's VAL-202)
  as a stale cosmetic record owned by a superseded mission; the functional e2e
  gate is proven under `git-forge-e2e-gate`.
- Future devflow work that touches mission lifecycle (add/abandon/orphan
  recovery) should re-read this record before changing the plan-file
  requirement — a mission whose source is gone is currently a dead end.
- The durable lesson is the devflow lifecycle gap itself: a mission whose plan
  source is gone is a dead end for every subcommand, so mission-level
  abandon/drop remains the wanted enhancement.

## Update (2026-08-28): orphan directory no longer present

As of 2026-08-28 `.specs/git-forge-spec-align/` is no longer on disk. It was
gitignored, so its removal is not recorded in git history and must have
happened outside both git and devflow. Consequences:

- The stale `pending` copy of VAL-202 is gone with the directory; the
  assertion remains proven under `git-forge-e2e-gate` (evidence artifacts
  listed above). No reconciliation is needed.
- The tool defect this record exists to document is UNCHANGED: devflow still
  has no mission-level abandon/drop command. If another orphaned
  `initializing`/`features_source_missing` mission appears, this record's
  analysis still applies — recover via a devflow enhancement, not hand-edits.
- This record stays in `implemented/` as the standing analysis of that gap;
  the "keep the directory in place" posture above is now moot, not reversed.
