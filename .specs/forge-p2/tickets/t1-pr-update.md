## Issue: t1 — #1 pr.update: extend an open PR with new commits

Description: L1 PRs are one-shot immutable snapshots; commits added after creation never join the PR, and no pr.update event exists. Any review-fix loop needs follow-up commits on the same change: devflow fix rounds send a worker back into the SAME worktree/branch; with immutable snapshots every round must recreate the PR and re-collect approve + ci.check, so review history fragments across N PRs for one logical change. A pr.update event re-pins the PR to the branch tip as a NEW snapshot generation; existing immutability is kept per generation.
Type: AFK
Milestone: pr-update
Touches: src/pr_merge.rs, src/merge_state.rs, src/store.rs, src/event.rs, src/fold.rs, src/cli.rs, tests/t3_merge.rs, scripts/gf-dogfood.sh
Blocked by: None
User stories covered: forge-pipeline-p2

### What to build
Per issue #1 spec amendment (per-generation snapshot contract):

1) pr.update event body records old/new source_head, base_head, merge_base.
2) Each update pins a NEW immutable generation ref; layout is ADR-time detail (e.g. refs/forge/prs/<n>/source/g<k>), but generation-addressability is contractual: L1 snapshot refs stay generation 0 and immutable.
3) Fold exposes the latest generation as the PR's current snapshot; diff = merge_base..latest source_head.
4) Review events and CI Checks bind their generation; ONLY latest-generation approve and check satisfy the Merge Gate; pre-update approve/CI are stale and refused.
5) Tests, red-first: update re-pins; approve-before-update cannot merge post-update content; CI-green-before-update cannot merge post-update content.

### Acceptance criteria
- [ ] `pr update <n>` re-pins the PR to the branch tip; event body records old/new source_head/base_head/merge_base
- [ ] Each update is addressable as a new immutable generation; generation-0 L1 refs unchanged
- [ ] `pr show` exposes the latest generation; diff = merge_base..latest source_head
- [ ] Approve-before-update cannot merge post-update content (gate refuses pre-update approve/CI)
- [ ] After update, fresh approve + fresh CI green on the latest generation merges
- [ ] dogfood gains update-then-merge assertions (stale approve refused, fresh approve passes)

### Validation
Fulfills: VAL-205, VAL-206
- Verification: cargo test (exit zero) + dogfood assertions (exit zero)
- Command / scenario: create PR → approve → update the source branch → merge → refused (stale approve); update → fresh approve + CI → merge → passes
- Evidence expected: gate refusal + pass tests green; dogfood green

### Notes
- Reviewed in pipeline terms: a fix round appends commits to the SAME branch; the adapter's merge gate detects the snapshot moved and calls `pr update` before re-running CI/ceremony — NOT `pr abandon` + recreate (#10 currently supersedes; #1 lands, the adapter switches to update in the same batch or a follow-up adapter ticket).
- The adapter follows: once `pr update` exists, a moved source on an OPEN PR triggers update (new generation) instead of supersede-recreate; the recreate path stays for stale BASE (not stale source).