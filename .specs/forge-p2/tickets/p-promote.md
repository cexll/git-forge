## Issue: p-promote — write the proven P2 flows into .agents/qa/

Description: turn the accepted P2 flows (machine-interface JSON; identity role attribution; pr.update generation gate; live-base result-bound merge) into install-e2e flow contracts under `.agents/qa/flows/`, reviewed and merged like any other change.
Type: AFK
Milestone: acceptance
Touches: .agents/qa/
Blocked by: a-final
User stories covered: forge-pipeline-p2

### What to build
Write the proven flows as flow contracts under `.agents/qa/flows/`:

1. `forge-machine-interface.md`: `--json` byte-stable schemas per command; `git forge version` semver + exit 0; unknown-flag exit-1; the adapter version-gates on semver and parses --json (no prose sed).
2. `forge-identity-actor.md`: per-worktree/environment identity overrides; three-role event chains distinguishable; wire contract role semantics.
3. `forge-pr-update.md`: `pr update` re-pins a new generation; only latest-generation approve + CI satisfy the gate; stale approve/CI are refused.
4. `forge-live-base-merge.md`: moved base is revalidated against the current tip (result-bound CI + review), never blanket-refused; finalize CASes the gate-validated base tip.

Each flow contract carries install-e2e's five required headings: `## Scope`, `## Driver contract for future runs`, `## Future run steps`, `## Blocking checks for future runs`, `## Evidence expected`.

### Acceptance criteria
- [ ] Four flow contract files exist under `.agents/qa/flows/` with the five required headings each
- [ ] Each flow names the CLI steps and the exit-code/JSON-schema/ref-state assertions the acceptance sweep observed
- [ ] The install-e2e parser accepts them (its own shape check passes)
- [ ] Nothing else changes — the flows are documentation-of-proof, not code

### Validation
Verification: `cargo test` (exit zero)
- Command / scenario: n/a (install-e2e shape check is the gate; the flows are contracts for future runs)
- Evidence expected: install-e2e parser accepts all four files; existing suite green

### Notes
- The promotion writes the learning into durable flow contracts so a future run can reproduce acceptability.
- It is Blocked by the acceptance ticket, so it can only include what the sweep actually proved.