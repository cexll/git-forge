## Issue: p-promote — write the proven p0 flows into .agents/qa/

Description: turn the accepted P0 flows (issue closed via merge transaction; abandon terminal state) into install-e2e flow contracts under `.agents/qa/flows/`, reviewed and merged like any other change.
Type: AFK
Milestone: acceptance
Touches: .agents/qa/
Blocked by: a-final
User stories covered: s0

### What to build
Write the two proven flows as flow contracts under `.agents/qa/flows/`:

1. `forge-PR-closes-issue.md`: PR created with --closes merges → linked issue is closed in the same transaction; failed merge keeps it open.
2. `forge-PR-abandon.md`: abandon is terminal — merge/review/ci refuse; no reopen; double-abandon idempotent.

Each flow contract carries install-e2e's five required headings: `## Scope`, `## Driver contract for future runs`, `## Future run steps`, `## Blocking checks for future runs`, `## Evidence expected`.

### Acceptance criteria
- [ ] Two flow contract files exist under `.agents/qa/flows/` with the five required headings each
- [ ] Each flow names the CLI steps and the exit-code/ref-state assertions the acceptance sweep observed
- [ ] The install-e2e parser accepts them (its own shape check passes)
- [ ] Nothing else changes — the flows are documentation-of-proof, not code

### Validation
Verification: `cargo test` (exit zero)
- Command / scenario: n/a (install-e2e shape check is the gate; the flows are contracts for future runs)
- Evidence expected: install-e2e parser accepts both files; existing suite green

### Notes
- The promotion writes the learning into durable flow contracts so a future run can reproduce acceptability.
- It is Blocked by the acceptance ticket, so it can only include what the sweep actually proved.
