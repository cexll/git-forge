## Issue: a-final — accept the whole P2 contract round

Description: exercise every contract assertion through its real surface and report per assertion
Type: ACCEPTANCE
Milestone: acceptance
Touches: None
Blocked by: None

### What to build
Not a build. The user's full contract, proven end to end: the P2 contract (VAL-201…VAL-209) exercised through its real surfaces — the forge CLI in a disposable clone for VAL-201/VAL-202/VAL-204/VAL-205/VAL-206/VAL-207/VAL-208, the implement-skill test matrix with the fake git-forge fixture for VAL-203, and the self-bootstrap record for VAL-209 — plus the same surface re-checks the implementers' own dogfood run.

### Acceptance criteria
- [ ] Every contract assertion carries a verdict and at least one evidence artifact

### Validation
Proves: VAL-201, VAL-202, VAL-203, VAL-204, VAL-205, VAL-206, VAL-207, VAL-208, VAL-209
- Command / scenario: one end-to-end sweep: --json byte-stable fixtures per command + version probe + unknown-flag quirk → three-role event chain → pr update re-pin + stale-approve refusal + fresh approve pass → two-PR live-base refuse/pass + CAS conflict → adapter matrix (--json parsing, version-gate) → self-bootstrap PR/issue records
- Evidence expected: per-assertion report + evidence artifacts

### Notes
- Terminal milestone: the compiler derives dependsOn from every feature in the plan; no manual Blocked by.
- The sweep re-tests only unproven + newly claimable, but P2's matrix is small enough to run the whole set once.