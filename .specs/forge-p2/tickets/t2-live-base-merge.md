## Issue: t2 — #2 live-base merge: merge onto a moved base instead of refusing stale

Description: git forge pr merge refuses when refs/heads/<base> has moved since pr create; the only remedy is recreating the PR. Multiple in-flight PRs against the same base serialize — the moment one merges, every other PR is stale. Agent pipelines land feature branches one after another onto a moving integration branch, so recreate churn is per-merge, not exceptional. Live-base merge recomputes merge_base against the current base tip and binds review/CI to the merge result.
Type: AFK
Milestone: live-base
Touches: src/pr_merge.rs, src/merge_state.rs, src/event.rs, src/fold.rs, src/cli.rs, tests/t3_merge.rs, scripts/gf-dogfood.sh
Blocked by: None
User stories covered: forge-pipeline-p2

### What to build
Per issue #2 spec amendment (bind review and CI to the MERGE RESULT):

1) Algorithm: recompute merge_base against the CURRENT base tip; execute the strategy against the current base; materialize the result commit; identify what CI verified with the result (run the CI plan against the result tree, or prove result tree == the tree CI checked).
2) A review decision must be bound to the result being merged; finalize CASes the current base tip the gate validated — this redesigns the base_oid CAS and the F-007/F-008 gate_head binding; name both in the implementation.
3) Deterministic acceptance: two PRs on one base; first merges; second's base has moved: merge REFUSES unless the result is re-validated, then passes with a fresh CI check and fresh review.

### Acceptance criteria
- [ ] Two PRs on one base: first merges; second's base moved → merge refuses (no re-validation)
- [ ] After fresh CI run + fresh approve (result-bound), the second PR's live-base merge passes
- [ ] Finalize CASes the CURRENT base tip the gate validated (F-007/F-008 binding redesign named in code)
- [ ] CAS conflict injected (base moved between gate validation and finalize): merge fails, refs unchanged, no partial write
- [ ] dogfood gains two-PR live-base assertions (refuse without fresh gate, pass with)

### Validation
Fulfills: VAL-207, VAL-208
- Verification: cargo test (exit zero) + dogfood assertions (exit zero)
- Command / scenario: PR1 + PR2 on one base; merge PR1; try PR2 → refused (stale); fresh ci + approve → merge PR2 → passes; inject CAS conflict → refused no partial
- Evidence expected: refuse + pass tests green; CAS conflict no-partial green; dogfood green

### Notes
- The #2 contract REPLACES the pre-#2 `--ff` moved-base refusal (VAL-103): with live-base, moved base is revalidated, not refused; the exact CAS expectation changes from snapshot base_head to the gate-validated current base tip.
- The adapter's stale-base recreate path is superseded for BASE moves (still used for a base that genuinely diverges unmerged) — adapter follow-up ticket notes are per the spec.
