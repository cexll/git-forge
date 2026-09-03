## Issue: t3 — failed merge leaves the issue open

Description: A merge that fails its gate (missing approve, stale base, conflict) must never write a partial `issue.close` — the issue stays open and the PR stays unmerged.
Type: AFK
Milestone: p0-core
Touches: src/pr_merge.rs, src/store.rs, tests/t3_merge.rs, scripts/gf-dogfood.sh
Blocked by: t2
User stories covered: s0

### What to build
Extend the existing merge-failure coverage to the `--closes` path: for a PR declaring a closes target, each pre-existing merge-failure class (gate red / stale base / checked-out base / merge conflict) leaves the linked issue open and refs unchanged. Concretely:

- The failure classes already short-circuit BEFORE the finalize transaction; verify none of them regresses when the closes target is present (they must still run — the closes target must not short-circuit into an early close or an early partial write).
- A conflict inside the disposable worktree (execute_strategy failure) never touches the issue ref (it happens before finalize).
- The finalize CAS retry exhaustion leaves the issue open (no partial `issue.close`).
- The test suite asserts each class: gate red, stale base, conflict, CAS-exhaustion (forced).

### Acceptance criteria
- [ ] Gate red (missing approve) with `--closes` → merge exits non-zero, issue open, no `pr.merge` event
- [ ] Stale base with `--closes` → merge refuses, issue open, no ref change
- [ ] Conflict on merge with `--closes` → issue open, temp worktree cleaned (existing invariant)
- [ ] CAS-exhaustion (via test injection) → issue open
- [ ] Pre-existing failure-class tests unchanged and green (no weakened assertions)

### Validation
Fulfills: VAL-004
- Verification: `cargo test` (exit zero)
- Command / scenario: manual merge with gate red then issue show
- Evidence expected: issue stays open; non-zero exit; test names the class

### Notes
- The store-level test must prove atomicity — the issue ref is not written even when the base/PR refs changed in the same batch (batch is all-or-nothing).
- The failure classes' EXISTING tests stay byte-unchanged where possible; this ticket adds assertions, it does not rewrite them.
