## Issue: a-final — accept the whole contract round

Description: exercise every contract assertion through its real surface and report per assertion
Type: ACCEPTANCE
Milestone: acceptance
Touches: None
Blocked by: None

### What to build
Not a build. The user's full contract, proven end to end: the P0 contract (VAL-001…VAL-010) exercised through the real forge CLI in a disposable clone, plus the same surface re-checks that the implementers' own dogfood run.

### Acceptance criteria
- [ ] Every contract assertion carries a verdict and at least one evidence artifact

### Validation
Proves: VAL-001, VAL-002, VAL-003, VAL-004, VAL-005, VAL-006, VAL-007, VAL-008, VAL-009, VAL-010
- Command / scenario: fresh clone → issue new; pr create --closes; review --approve; ci run; merge; issue show closed; pre-closed no-op; gate-red keeps open; abandon + refusals; double abandon; merged-then-abandon — one end-to-end sweep
- Evidence expected: per-assertion report + evidence artifacts

### Notes
- Terminal milestone: the compiler derives dependsOn from every feature in the plan; no manual Blocked by.
- The sweep re-tests only unproven + newly claimable, but P0 is small enough to run the whole matrix once.
