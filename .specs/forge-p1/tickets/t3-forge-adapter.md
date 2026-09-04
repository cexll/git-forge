## Issue: t3 — #10 forge adapter: embedded devflow.cjs integration (propose --forge)

Description: Implement the eight-clause adapter contract inside `devflow.cjs` behind `propose --forge`: PR creation at merge-gate time, idempotent issue projection, proof-ticket closure, failure taxonomy, crash reconcile/resume probe, recycle ownership consuming #4's structured outcome, progress-log mirroring, and a fake git-forge test-matrix fixture with dual-path parity.
Type: AFK
Milestone: adapter
Touches: ~/.agents/skills/implement/scripts/devflow.cjs, ~/.agents/skills/implement/tests/, ~/.agents/skills/implement/fixtures/
Blocked by: t2
User stories covered: forge-pipeline-p1

### What to build
The adapter contract (`docs/decisions/implemented/architecture/forge-pipeline-adapter-contract.md`), clause by clause:

1. **Owner**: adapter embedded in `devflow.cjs` behind `propose --forge`; only the orchestrator/scheduler writes forge state (worker briefs forbid `git forge`); PR id and merge outcomes live in `runtime.json`; PR creation happens at merge-gate time (after mission review passes and gate verification), not at worker receipt.
2. **Filing**: issues filed for active tickets at propose, amend additions at amend time, injected validators at injection; a failed/cancelled ticket closes with an abandon-reason comment; proof (report-only) tickets close via a single issue comment (evidence pointer + overall status) plus close — validators never run `git forge` write commands; code tickets close via `pr create --closes` + merge.
3. **Idempotence**: before filing, look for an open Issue carrying the `session:<s>` label whose first body line is `Ticket: <fid>`; found → reuse, never re-file. Ticket id lives in that body line, NOT in a label; the label vocabulary stays exactly `type:`/`milestone:`/`session:`.
4. **Attribution**: progress log mirrors forge transitions (`forge_pr_created`, `forge_pr_superseded`, `forge_merged`) with old/new PR ids; stale-base recreate via `forge_pr_superseded` with a new snapshot; no per-worktree git identity customization (#8's future mechanism).
5. **Failure taxonomy**: ci-run red → existing `gate_failed`; stale-base / superseded PR → adapter recreate path, never a pause, budget-neutral; `pr create` / prose-parse / bridge failure → new RECOVERABLE pause reason `forge_bridge_failed` (self-clears on re-run, error text names the remedy); finalize CAS conflict persisting after forge's own retry → `forge_bridge_failed`; worktree recycle refused/partial → existing `worktree_partial_removal` RejectError with feature.worktree kept as the recovery record. Parse failure (until #7) is a pause, never a guess.
6. **Crash reconcile/resume probe**: after a successful forge merge, an idempotent ordered probe keyed by the persisted prId: (1) PR chain merged but runtime still in_review → mark completed, mirror `forge_merged`, continue; (2) merged and issue still open → post the closing comment and close; (3) merged but worktree still present → recycle per clause 8; (4) NEVER re-invoke `pr merge`. Precedence: forge refs say what happened, the progress log says what the mission recorded, runtime.json is rebuilt from the probe — never hand-edited.
7. **Recycle ownership**: forge-side removal (#4) is the sole remover post-switch and returns the structured outcome; salvage renaming of an unmerged branch stays adapter-owned and runs BEFORE the recycle call; the adapter persists the outcome before clearing feature.worktree/branch.
8. **Test matrix**: fake `git-forge` binary fixture in the implement-skill test matrix; dual-path parity (same feature through the old path and through `--forge`).

Switchover ordering (per `docs/decisions/implemented/process/pipeline-switchover-at-guard-and-recycle.md`): #9→#3→#4 have landed the old way (no forge PR); this adapter ships; then #1/#2 go through the new flow. `--forge` is mission-scoped: missions proposed without it keep old-path merge semantics to completion and never gain a prId.

### Acceptance criteria
- [ ] `propose --forge` creates PRs at merge-gate time; `runtime.json` gains prId/issueId; progress log records `forge_pr_created`
- [ ] Filing is idempotent: a retry reuses the open Issue matched by `session:<s>` label + first body line `Ticket: <fid>` — forge-side issue count does not grow across retries
- [ ] Proof tickets close via exactly one issue comment (evidence pointer + status) + close; all forge write calls originate from the adapter, never validators
- [ ] Failure taxonomy maps each class to its named pause/recreate path: ci red→`gate_failed`; stale/superseded→recreate (`forge_pr_superseded`, no pause); bridge/parse + persistent CAS→RECOVERABLE `forge_bridge_failed`; recycle refused/partial→`worktree_partial_removal`
- [ ] Crash reconcile probe converges all three injected中断态 (merged-not-recorded / merged-open-issue / merged-remaining-worktree) and never re-invokes `pr merge`
- [ ] Recycle ownership: adapter consumes #4's structured outcome, runs salvage BEFORE recycle, persists the outcome before clearing feature.worktree/branch
- [ ] Fake git-forge fixture + dual-path parity green in the implement-skill test matrix; progress-log mirror events reconstruct the recreate window

### Validation
Fulfills: VAL-113, VAL-114, VAL-115, VAL-116, VAL-117
- Verification: `cargo test` (exit zero)
- Command / scenario: implement-skill test matrix with the fake git-forge fixture: propose --forge → merge-gate PR creation → filing retry idempotence → ci-red/stale/bridge failure classes → crash-state probes → recycle outcome consumption; dual-path parity run
- Evidence expected: test matrix green; fixture call-log assertions green; cargo test exit 0 (git-forge suite untouched and green)

### Notes
- Until #7 (machine interface) the adapter parses `issue #<n> created:` / `PR #<n> created:` prose; parse failure is a RECOVERABLE pause naming the remedy — never a guess, never a duplicate filing.
- The adapter is a cross-repo change: the deliverable lives in the implement skill; git-forge's own suite must stay untouched-and-green (that is what `cargo test` verifies here).
- No per-worktree git identity (clause 5 rejected alternative): role attribution waits for #8.