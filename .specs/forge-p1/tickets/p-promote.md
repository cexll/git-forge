## Issue: p-promote — write the proven P1 flows into .agents/qa/

Description: turn the accepted P1 flows (ff integration with checked-out base; worktree/branch recycle; forge-adapter pipeline) into install-e2e flow contracts under `.agents/qa/flows/`, reviewed and merged like any other change.
Type: AFK
Milestone: acceptance
Touches: .agents/qa/
Blocked by: a-final
User stories covered: forge-pipeline-p1

### What to build
Write the proven flows as flow contracts under `.agents/qa/flows/`:

1. `forge-PR-ff-integration.md`: `pr merge --ff` from a clean checked-out base merges exactly the verified source_head; non-ffable/moved-base/dirty-base refusals leave refs untouched; refresh failure never splits refs+tree.
2. `forge-PR-recycle-worktree.md`: `pr merge <n> --recycle-worktree <path>` removes the registered source worktree, deletes the branch exactly when the source tip is contained in the integrated base, reports the structured outcome vocabulary, never touches the base worktree.
3. `forge-adapter-pipeline.md`: `propose --forge` files idempotently, creates the PR at merge-gate time, closes proof tickets by comment+close, maps failure classes to their named pauses, reconciles crash states by prId without re-invoking `pr merge`, and mirrors forge transitions in the progress log.

Each flow contract carries install-e2e's five required headings: `## Scope`, `## Driver contract for future runs`, `## Future run steps`, `## Blocking checks for future runs`, `## Evidence expected`.

### Acceptance criteria
- [ ] Three flow contract files exist under `.agents/qa/flows/` with the five required headings each
- [ ] Each flow names the CLI steps and the exit-code/outcome-line/ref-state assertions the acceptance sweep observed (including the `removed | refused-* | branch-* | partial-leftover` outcome vocabulary)
- [ ] The install-e2e parser accepts them (its own shape check passes)
- [ ] Nothing else changes — the flows are documentation-of-proof, not code

### Validation
Verification: `cargo test` (exit zero)
- Command / scenario: n/a (install-e2e shape check is the gate; the flows are contracts for future runs)
- Evidence expected: install-e2e parser accepts all three files; existing suite green

### Notes
- The promotion writes the learning into durable flow contracts so a future run can reproduce acceptability.
- It is Blocked by the acceptance ticket, so it can only include what the sweep actually proved.