# Forge-pipeline adapter contract: one embedded owner, labelled projections, mirrored events

Status: active

## Problem

The switchover (see `pipeline-switchover-at-guard-and-recycle`) hands every
subsequent integration to `git forge` commands, but nothing yet decides *who*
runs them, where PR identity lives, how Tickets project to Issues without
duplicating across re-proposals, or how a recreated PR stays attributable. Left
implicit, these drift into orchestrator improvisation — the exact failure class
the mission ledger exists to prevent.

## Decision

One contract, eight clauses:

1. **Owner.** The adapter is embedded in `devflow.cjs` behind
   `propose --forge`. Only the orchestrator/scheduler writes forge state; a
   worker's brief forbids touching `git forge`. PR id and merge outcomes live in
   `runtime.json` — one stateful resource, one lifecycle owner. PR creation
   happens at merge-gate time, when the feature has passed mission review and
   gate verification — not at worker receipt, which would guarantee stale-base
   refusals whenever a concurrent wave member merges first. A crash between a
   successful forge merge and the runtime/progress update is repaired by an
   idempotent resume probe keyed by the persisted prId (clause 7).
2. **Filing.** Issues are filed for active tickets at `propose`, for `amend`
   additions at amend time, and for injected validators at injection; a retry
   opens no new Issue. A failed/cancelled Ticket is closed with an
   abandon-reason comment (the Issue-level analog of #6). Proof (report-only)
   tickets close via a single issue comment (evidence pointer + overall status)
   plus close — the validator's Receipt carries the pointer, and the
   orchestrator-owned adapter posts the comment and the close; validators never
   run `git forge` write commands (see
   `report-only-work-closes-issue-without-pr`); code tickets close via
   `pr create --closes` + merge.
3. **Idempotence.** Before filing, the adapter looks for an open Issue carrying
   the `session:<s>` label whose first body line is `Ticket: <fid>`; found →
   reuse, never re-file. The ticket id lives in that stable body line, NOT in a
   label — the label vocabulary stays exactly `type:`/`milestone:`/`session:`.
   Seven historical mission sessions share one id counter; unguarded filing
   interleaves duplicates across them.
4. **Labels.** The vocabulary is exactly three prefixes — `type:`
   (`afk|hitl|validation|acceptance|prefactor`), `milestone:<name>`,
   `session:<spec-session>` — on Issues and PRs alike. Everything else lives in
   the body; labels never double as content.
5. **Attribution.** The progress log mirrors forge transitions
   (`forge_pr_created`, `forge_pr_superseded`, `forge_merged`) with old/new PR
   ids, so the recreate window before `pr.update` (#1) stays reconstructable
   after a crash. Pre-live-base stale PRs are recreated via `forge_pr_superseded`
   with old/new ids (see switchover record) — PR-abandon (#6) covers discarded
   attempts, superseded covers replaced snapshots. Worker git identity is NOT
   customized — role attribution waits for issue #8's first-class mechanism; no
   second path via `extensions.worktreeConfig`.
6. **Failure taxonomy.** Forge failures map to devflow pauses by class:
   ci-run red → existing `gate_failed` (fix/rebase round follows, budget as
   today); stale-base / superseded PR → adapter recreate path, never a pause,
   budget-neutral; `pr create` / prose-parse / bridge failure → new RECOVERABLE
   pause reason `forge_bridge_failed` (self-clears on re-run, mirrors
   gate_failed semantics, error text names the remedy); finalize CAS conflict
   persisting after forge's own retry → `forge_bridge_failed`; worktree recycle
   refused/partial → existing `worktree_partial_removal` RejectError with
   feature.worktree kept as the recovery record.
7. **Crash reconcile/resume probe.** After a successful forge merge the adapter
   runs an idempotent ordered probe keyed by the persisted prId: (1) PR chain
   merged but runtime still in_review → mark completed, mirror `forge_merged`,
   continue; (2) merged and issue still open → post the closing comment and
   close (pipeline-side op until #5's transaction lands); (3) merged but
   worktree still present → recycle per clause 8; (4) never re-invoke
   `pr merge` (the PR is terminal and forge refuses). Precedence: forge refs say
   what happened, the progress log says what the mission recorded, runtime.json
   is rebuilt from the probe — never hand-edited.
8. **Recycle ownership.** Forge-side removal (#4) is the sole remover
   post-switch and returns a structured outcome (removed / refused-dirty /
   refused-not-contained / partial-leftover(path) / branch-deleted /
   branch-kept(reason)). Salvage renaming of an unmerged branch stays
   adapter-owned and runs BEFORE the recycle call; the adapter persists the
   outcome before clearing feature.worktree/branch. Until #4 lands, devflow's
   existing recycleFeatureWorktree remains the implementer.

## Alternatives considered

- **Standalone wrapper script above devflow** — rejected: PR ids and merge
  results would live outside scheduler state, breaking the crash-recovery
  ledger and the "only devflow writes state" trust boundary.
- **Auto-detect git-forge with no flag** — rejected: merge semantics would fork
  invisibly at propose time; the plan digest should pin which one a mission uses.
- **File every planned ticket at propose** — rejected: cancelled or never-run
  tickets leave permanently open Issues.
- **Skip the event mirror, rely on the forge chain** — rejected: the forge chain
  never says which mission round a superseded PR answered; the window between
  switch and #1 is exactly when the adapter needs calibrating.
- **Set per-worktree git identity now** — rejected: duplicates #8's future
  mechanism before it exists; half the surface (forge actors) would stay shared
  anyway.

## Consequences

- `devflow.cjs` gains forge subcommand plumbing + `runtime.json` fields; the
  implement-skill test matrix needs a fake `git-forge` binary fixture.
- Until issue #7 (machine interface) lands, the adapter parses `issue #<n>
  created:` / `PR #<n> created:` prose and must treat parse failure as a pause,
  never a guess; failure classes follow clause 6 (never blanket `gate_failed`).
- Issues #1–#9 keep their pre-vocabulary labels: their ids are cited across the
  decision set and L1 has no relabel operation. The
  `type:`/`milestone:`/`session:` vocabulary binds adapter-filed projections
  from the switchover forward.
- The glossary terms **Forge Mode** and **Issue Projection** are now canonical
  (implement `CONTEXT.md`).
