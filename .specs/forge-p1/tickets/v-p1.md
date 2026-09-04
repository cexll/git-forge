## Issue: v-p1 — prove the merge-core + adapter assertions

Description: exercise every merge-core and adapter ASSERTION through its real surface (forge CLI; adapter via the implement-skill test matrix / fake git-forge fixture) and report per assertion
Type: VALIDATION
Milestone: acceptance
Touches: None
Blocked by: t0, t1, t2, t3

### What to build
Not a build. Exercise the P1 assertions through their real surfaces and report per assertion: VAL-101…VAL-112 through the forge CLI in a disposable clone (including a worktree landscape: main tree on the base branch, per-feature source trees); VAL-113…VAL-118 through the implement-skill test matrix with the fake git-forge fixture (VAL-118 additionally against the switchover ADR's ordering facts).

### Acceptance criteria
- [ ] Every assigned assertion carries a verdict and at least one evidence artifact

### Validation
Proves: VAL-101, VAL-102, VAL-103, VAL-104, VAL-105, VAL-106, VAL-107, VAL-108, VAL-109, VAL-110, VAL-111, VAL-112, VAL-113, VAL-114, VAL-115, VAL-116, VAL-117, VAL-118
- Command / scenario: disposable clone → feature worktree → pr create --closes → review/ci → `pr merge --ff --recycle-worktree`; refusal matrix (non-ff, moved base, dirty base, dirty/unregistered/wrong-branch recycle, --squash branch-kept); refresh-failure and partial-removal injections; then adapter matrix: propose --forge, filing retry, failure classes, crash probes, dual-path parity
- Evidence expected: per-assertion report + evidence artifacts (stdout/exits captured; fixture call logs for adapter assertions)

### Notes
- The validator never fixes what it tests. A finding becomes a fix feature (amend), never a code edit by the validator.
- Adapter assertions are CLI-observable only through the fixture's recorded call log and devflow's own outputs; the report names the tool per assertion.