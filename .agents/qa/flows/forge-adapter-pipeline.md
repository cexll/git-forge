# Flow: forge-adapter-pipeline

## Scope
- Actor: the devflow adapter (devflow.cjs `propose --forge` / `reconcile-forge` / `close-forge-ticket`) driven by an orchestrator; workers and validators NEVER invoke forge write commands.
- Surface: cli — the embedded adapter pipeline against a FAKE `git-forge` binary fixture (call-log recording every forge write with caller attribution), not a scratch forge clone.
- App: git-forge adapter contract — propose → filing (idempotent) → PR at merge-gate → gate/merge/recycle → proof close, with failure taxonomy, crash reconcile, and progress-log mirror events.
- Entrypoint: `devflow.cjs propose ... --forge` (mission-scoped flag), `devflow.cjs reconcile-forge --mission-dir <missions>`, `devflow.cjs close-forge-ticket --evidence <report> --status <status>`.
- Related path patterns:
  - `scripts/devflow.cjs` — `--forge` plumbing, runtime.json prId/issueId fields, prose parsing, failure-class mapping, reconcile probe.
  - `tests/devflow/forge-adapter.test.cjs` + `fake-git-forge.cjs` — the fake forge fixture and the adapter test matrix.
  - `docs/decisions/implemented/architecture/forge-pipeline-adapter-contract.md` — the eight clauses (owner/filing/idempotence/labels/attribution/failure-taxonomy/crash-probe/recycle-ownership).
  - `docs/decisions/implemented/process/pipeline-switchover-at-guard-and-recycle.md` — switchover ordering (VAL-118, outside this flow's VAL set).
- Proven by: a-final acceptance sweep, VAL-113..VAL-117, all `pass` (evidence: `.specs/forge-p1/forge-p1/missions/evidence/a-final/val-113/` .. `val-117/`, `forge-adapter.test-matrix.txt` 24/24, `dual-path-parity.test.txt` 6/6), adapter at the t5-fixed snapshot (`09a97d29`).

## Driver contract for future runs
- Start/health:
  - Run against the implement-skill devflow harness with the fake `git-forge` fixture on PATH (the fixture records a call log: every write attributed to its caller). A scratch working directory with `.specs/forge-matrix/` (features.json + validation-contract.json) seeded per scenario; `mktemp -d`, trap cleanup.
  - The adapter reads `issue #<n> created:` / `PR #<n> created:` prose from the forge output — parse failures are RECOVERABLE pauses (`forge_bridge_failed`), never guessed ids.
- Seed/auth:
  - Mission-scoped `--forge` flag decides the forge path; missions WITHOUT the flag keep legacy-path semantics to completion (dual-path parity depends on this).
  - Filing identity: issues carry labels `type:<t>`, `milestone:<m>`, `session:<s>` (three prefixes only; the ticket id is NEVER a label) and body first line `Ticket: <fid>`.
- Primary action: `propose --forge` files issues idempotently and records prId/issueId in runtime.json; driving a feature to the merge gate creates and merges the PR; failures are classified per clause 6; `reconcile-forge` converges crash states; `close-forge-ticket` closes proof tickets.
- State oracle:
  - `runtime.json` per feature: `prId`, `issueId`, `status` (completed/in_review/paused).
  - Fake-forge state: issue/PR records (`labels`, body, `open`, `merged`, review/CI fields) observed as JSON after each step.
  - Progress log events: `forge_issue_filed`, `forge_pr_created`, `forge_stale_base`, `forge_pr_superseded`, `forge_merged`, `forge_recycle_outcome`, `forge_ci_failed`.
  - state.json: `pauseReason` (e.g. `worktree_partial_removal`) lives HERE and in `merge_blocked.code` — NOT as a progress-log event field; grepping events for it finds nothing.
  - Call log: caller attribution on every forge write (`caller: adapter`).
- Evidence sinks: per-step stdout transcripts (`step-NN-<what>.txt`), forge-state JSON snapshots (`obs-NN-*.json`), the progress-log event list, the call log, and the node test-matrix outputs (`forge-adapter.test-matrix.txt`, `dual-path-parity.test.txt`).
- Cleanup/isolation: scratch dirs removed on exit; the fake forge is the only forge writer; a real `git forge` binary is NEVER invoked in this flow.
- Forbidden boundary: never hand-edit runtime.json (the reconcile probe rebuilds it from forge refs, never manual); never re-invoke `pr merge` on an already-merged PR (crash probe converges without it); validators/workers never run forge write commands (single-owner adapter); no `git forge` outside the fixture.

## Future run steps
1. Seed the matrix working dir (features t1/t2 + validation contract); run `devflow.cjs propose --session <s> --working-directory <dir> --features <features.json> --contract <validation-contract.json> --mission-dir <missions> --forge` → exit 0; missions dir created at `<dir>/.specs/forge-matrix/<s>/missions`.
2. VAL-113 (filing idempotence + merge-gate PR): observe forge state — issues filed with labels `[type:afk, milestone:<m>, session:<s>]` and body first line `Ticket: t1`/`Ticket: t2`; runtime.json carries `prId`/`issueId`. Re-run `propose` (reconcile path) → exit 0, forge issue count UNCHANGED (reuse by `session:<s>` label + `Ticket:` body match, not re-filed). Then drive feature t1 to the merge gate: PR created AT gate time, merged with `closes` linking, and the closes-issue closes (issue `open: false`).
3. VAL-114 (failure taxonomy, clause 6), one scenario per class against the fixture:
   a. ci red → feature pauses `gate_failed`; `forge_ci_failed` mirrored; ZERO `forge_bridge_failed` anywhere.
   b. stale/superseded base → adapter recreates without pausing: `forge_pr_created(old)` → `forge_stale_base` → `forge_pr_created(new)` → `forge_pr_superseded(oldPrId, newPrId, reason "stale base")`; old PR abandoned `superseded: base moved`; feature completes on the new PR.
   c. pr-create bridge failure → pause `forge_bridge_failed`; a retry self-clears and merges (exit 0, PR merged).
   d. prose-parse garbage (forge output unparseable) → pause `forge_bridge_failed`; NO duplicate filing.
   e. finalize CAS persistent conflict → pause `forge_bridge_failed`; PR unmerged.
   f. recycle refusal (`refused-dirty`, F-002 FIXED) → state.json `pauseReason: worktree_partial_removal`, `merge_blocked.code: worktree_partial_removal`, one `worktree_recycle_refused` event, feature `in_review` with worktree KEPT, ZERO `forge_bridge_failed` (never the catch-all).
4. VAL-115 (proof close + attribution): for a report-only validation feature, `devflow.cjs close-forge-ticket --evidence reports/<fid>.json --status success` → the `Ticket: <fid>` issue closes with EXACTLY ONE comment (`forge-proof-close: <fid>` / `status: success; evidence: reports/<fid>.json` — the report pointer) before the close; read the full-run call log: every forge write (issue new/comment/close, pr create/merge/review, ci run) carries `caller: adapter`; exactly one comment call and one close call for the proof ticket.
5. VAL-116 (crash reconcile, three injected states after a REAL merge, each probed with `devflow.cjs reconcile-forge --mission-dir <missions>`; record `pr merge` call count before/after):
   a. merged-not-recorded (runtime rewound to `in_review`, log truncated before `forge_merged`) → probe converges t1 to `completed`, `forge_merged` mirrored with the prId; merge count unchanged.
   b. merged + issue left open → probe posts the closing comment and closes the issue; merge count unchanged.
   c. merged + leftover worktree → probe recycles it (directory `exists: false`, worktree record null after); merge count unchanged. The probe reads the forge prId as authority and NEVER re-invokes `pr merge`.
6. VAL-117 (dual-path parity + mirror events): drive the same two-feature plan twice to completion — legacy (no `--forge`) and forge (`--forge`) — both exit 0 with identical feature statuses; then a third forge arm through a stale-base recreate window. Assert the progress log reconstructs the window in order: `forge_issue_filed(t1)` → `forge_issue_filed(t2)` → `mission_proposed` → `forge_pr_created(t1, prId 1)` → `forge_stale_base(t1, prId 1)` → `forge_pr_created(t1, prId 2)` → `forge_pr_superseded(old 1, new 2, "stale base")` → `forge_merged(t1, prId 2, commitId <sha>)` → `forge_recycle_outcome(t1, prId 2, "removed; branch-deleted (<branch>)")`. Run the standing node matrices: forge-adapter matrix and dual-path-parity matrix both green.

## Blocking checks for future runs
- Filing idempotence: a second `propose --forge` reuses the existing issues (count unchanged) keyed by the `session:<s>` label + `Ticket: <fid>` body line; no duplicate issues, no duplicate PRs.
- PR creation happens at merge-gate time (not at propose), the PR carries the `closes` link, and the gate closes both PR and linkee.
- Failure classes map to their named pauses and to nothing else: ci red → `gate_failed` (never blanket `forge_bridge_failed`); stale → recreate + `forge_pr_superseded` with NO pause; bridge/parse/finalize-CAS → `forge_bridge_failed`; recycle refusal → `worktree_partial_removal` (feature `in_review`, worktree kept, `merge_blocked.code` set). Assert by state.json `pauseReason` + `merge_blocked.code`, NOT by grepping progress-log events for `pauseReason`.
- Proof close: exactly one comment (`forge-proof-close: <fid>`) then close; the call log shows every forge write attributed to `caller: adapter` and zero writes from workers/validators.
- Crash probe: all three injected states converge from the forge prId authority; `pr merge` call count never increases; runtime.json is rebuilt by the probe, never hand-edited.
- Parity: legacy and forge paths end with identical feature statuses and exit 0; the forge mirror events (`forge_pr_created`/`forge_pr_superseded`/`forge_merged`, plus `forge_recycle_outcome`) reconstruct the recreate window in order; both node matrices green (proven: 24/24 and 6/6).
- Ids are never hardcoded: parse `issue #<n> created:` / `PR #<n> created:` from forge output; a parse failure is a RECOVERABLE `forge_bridge_failed` pause, not a guess.

## Negative / boundary checks
- `propose` without `--forge` takes the legacy path for its whole lifetime (the flag is mission-scoped; parity only holds because the legacy arm never touches the forge).
- A `forge_bridge_failed` pause is RECOVERABLE: the retry re-proposes, reuses the filed issues (idempotence holds across the failure), and completes.
- The recycle-refusal path must never fall into the `forge_bridge_failed` catch-all (F-002): assert `bridgeFailedPresent: false` in the verdict snapshot while `statePauseReason: worktree_partial_removal`.
- A parse-garbage forge output must not produce a guessed issue/PR id and must not double-file: the issue count is unchanged after the failed propose retries.
- The crash probe on an ALREADY converged mission is a no-op (`filed 0`); it never re-merges or re-comments an already-closed issue.
- Mirror-event vocabulary is fixed: any event name outside the listed `forge_*` set in the progress log is a contract break.

## Evidence expected
- Per-step transcripts with the full `devflow.cjs` command line and exit code (`step-01-propose-val113.txt` shape), one per scenario (VAL-113..VAL-117).
- Forge-state JSON snapshots after each key step (`obs-NN-forge-*.json`): issue labels/body, PR merged state, closes linkage, recycle outcome.
- The progress-log event list for the recreate window, in order, with `forge_pr_superseded(old 1, new 2, reason "stale base")` and `forge_merged(commitId <sha>)`.
- The call log with caller attribution (`obs-10-calllog.json` shape) and the write-attribution verdict (every write `caller: adapter`; exactly one comment + one close for the proof ticket).
- Crash-probe verdicts: runtime/worktree snapshots before/after each of the three injected states plus the `pr merge` call count (unchanged).
- Node matrix outputs: `forge-adapter.test-matrix.txt` (24/24 pass) and `dual-path-parity.test.txt` (6/6 pass); a run-log ending line `ALL <k> ASSERTIONS EXERCISED OK`.