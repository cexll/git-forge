## Issue: v-p0 — prove the p0-core assertions

Description: exercise every p0-core ASSERTION through its real surface (CLI) and report per assertion
Type: VALIDATION
Milestone: p0-core
Touches: None
Blocked by: t0, t1, t2, t3, t4, t5, t6

### What to build
Not a build. Exercise the p0-core assertions through the real CLI (git forge pr/issue in a disposable clone) and report per assertion.

### Acceptance criteria
- [ ] Every assigned assertion carries a verdict and at least one evidence artifact

### Validation
Proves: VAL-001, VAL-002, VAL-003, VAL-004, VAL-005, VAL-006, VAL-007, VAL-008, VAL-009, VAL-010
- Command / scenario: fresh clone → issue new → pr create --closes → review --approve → ci run → merge; issue show closed; repeat with pre-closed issue; repeat with gate red (no approve) → issue stays open; abandon path: pr abandon --reason then merge/review/ci refusals; double abandon; merged-then-abandon
- Evidence expected: per-assertion report + evidence artifacts (stdout/exits captured)

### Notes
- The validator never fixes what it tests. A finding becomes a fix feature (amend), never a code edit by the validator.
- Assertions are CLI-observable only; the report's tool is the CLI itself.
