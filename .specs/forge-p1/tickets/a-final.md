## Issue: a-final — accept the whole P1 contract round

Description: exercise every contract assertion through its real surface and report per assertion
Type: ACCEPTANCE
Milestone: acceptance
Touches: None
Blocked by: None

### What to build
Not a build. The user's full contract, proven end to end: the P1 contract (VAL-101…VAL-118) exercised through its real surfaces — the forge CLI in a disposable clone with a real worktree landscape (main tree on the base, per-feature source trees) for VAL-101…VAL-112, and the implement-skill test matrix with the fake git-forge fixture for VAL-113…VAL-118 — plus the same surface re-checks the implementers' own dogfood run.

### Acceptance criteria
- [ ] Every contract assertion carries a verdict and at least one evidence artifact

### Validation
Proves: VAL-101, VAL-102, VAL-103, VAL-104, VAL-105, VAL-106, VAL-107, VAL-108, VAL-109, VAL-110, VAL-111, VAL-112, VAL-113, VAL-114, VAL-115, VAL-116, VAL-117, VAL-118
- Command / scenario: one end-to-end sweep: clean checked-out base merge with --ff (result==source head) → refusal matrix (non-ff, moved base, dirty base incl. untracked, refresh-failure injection) → recycle matrix (removed/branch-deleted, refusals, branch-kept, partial-leftover, base-worktree-untouched) → adapter matrix (propose --forge, filing idempotence, failure taxonomy, crash probes, dual-path parity, progress-log mirrors)
- Evidence expected: per-assertion report + evidence artifacts

### Notes
- Terminal milestone: the compiler derives dependsOn from every feature in the plan; no manual Blocked by.
- The sweep re-tests only unproven + newly claimable, but P1's matrix is small enough to run the whole set once.