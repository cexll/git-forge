## Issue: v-p2 — prove the machine-interface / identity / pr-update / live-base assertions

Description: exercise every P2 ASSERTION through its real surface (forge CLI; adapter via the implement-skill test matrix with the fake git-forge fixture) and report per assertion
Type: VALIDATION
Milestone: acceptance
Touches: None
Blocked by: t7, t8, t1, t2

### What to build
Not a build. Exercise the P2 assertions through their real surfaces and report per assertion: VAL-201…VAL-204 through the forge CLI (--json byte-stable fixtures, version probe, unknown-flag quirk, role-attributed event chains) and the adapter matrix (--json parsing + version-gate); VAL-205…VAL-208 through the forge CLI in a disposable clone (pr update re-pin + generation gate; two-PR live-base refuse/pass + CAS conflict); VAL-209 against the self-bootstrap record (each P2 feature's PR merged + issue closed via the new flow).

### Acceptance criteria
- [ ] Every assigned assertion carries a verdict and at least one evidence artifact
### Validation
Proves: VAL-201, VAL-202, VAL-203, VAL-204, VAL-205, VAL-206, VAL-207, VAL-208, VAL-209
- Command / scenario: disposable clone → --json on each command (byte-compare schema fixtures); version; unknown flag; three-role event chain; pr update re-pin + stale-approve refusal + fresh approve pass; two-PR live-base refuse/pass; CAS conflict; adapter matrix with --json fixture
- Evidence expected: per-assertion report + evidence artifacts (stdout/exits captured; schema fixture byte-compares; adapter matrix outputs)

### Notes
- The validator never fixes what it tests. A finding becomes a fix feature (amend), never a code edit by the validator.
- VAL-209's evidence is the mission's own PR/issue records (self-bootstrap proof).