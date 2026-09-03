## Issue: t0 — split merge-gate state out of pr_merge.rs (pure refactor)

Description: Extract merge-gate state machine + finalize transaction construction into their own module, byte-for-byte behavior-preserving, so store.rs/pr_merge.rs stay under the 800-code-line size gate when P0 adds keys/fields/checks.
Type: AFK
Milestone: p0-core
Touches: src/pr_merge.rs, src/store.rs, src/event.rs, src/fold.rs, src/lib.rs
Blocked by: None
User stories covered: split-for-0

### What to build
A pure move: move the merge-gate predicate and finalize-transaction construction logic out of `pr_merge.rs` into a new module (e.g. `src/pr_merge/meta.rs` or a flat sibling `src/merge_state.rs`), and move fold-state helpers out of `store.rs` into their natural seam (fold). No behavior change. This is the prefactor the P0 size-gate work depends on: P0 adds a pr.abandoned kind, a closes field, and a fourth ref move in the finalize transaction, and those additions must land where there is still line budget.

### Acceptance criteria
- [ ] Behavior byte-for-byte identical: cargo test (45/45) green before AND after, no test changed
- [ ] Lint/cargo gates green: fmt, clippy -D warnings -D too_many_lines, no new size-gate violations
- [ ] Every file that P0 will touch has room under the 800-code-line gate (report final line counts)
- [ ] No public API renaming that would ripple into tests that P0 does not own

### Validation
- Verification: cargo test (exit zero)
- Command / scenario: run the full suite; the prefactor is accepted by "existing tests still pass unchanged"
- Evidence expected: cargo test exit 0, 45/45

### Notes
- This ticket claims NO assertion (prefactor exception, not TDD-deleted). Its acceptance is the unchanged green suite — the "unchanged" is the whole safety argument: if it needed test rewrites it wasn't a prefactor.
- gril-decision `size-gate: src/*.rs every-file ≤ 800` — store.rs current 769, event.rs 679, pr_merge.rs callers keep the too_many_lines carve-out for CMD (L24-26).
- Keep the diff mechanical: no reformatting of moved code, no doc rewrites; the move is the change.
