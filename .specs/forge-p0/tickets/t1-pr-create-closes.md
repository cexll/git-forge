## Issue: t1 — pr create accepts --closes and surfaces it

Description: `git forge pr create` gains a single `--closes <issue-id>`; the closes relationship is recorded in `pr.created` and shown by `pr show`.
Type: AFK
Milestone: p0-core
Touches: src/cli.rs, src/store.rs, src/event.rs, src/fold.rs, src/issue.rs, tests/t2_pr.rs, scripts/gf-dogfood.sh
Blocked by: t0
User stories covered: s0

### What to build
Extend `pr create` (CLI surface in `cli.rs` → event in `store.rs` → fold in `event.rs`/`fold.rs`) with one optional `--closes <n>`:

- Parse `--closes <n>` — id must be an existing issue (unknown id → clean error, no PR created, no counter increment beyond the rejected attempt's own allocation semantics — the allocation must not happen for a rejected create).
- Write `pr.created` body with optional `"closes": <n>` when provided (`v` stays 1; old readers ignore it).
- `pr show <n>` prints `closes: #<n>` when present (and absent otherwise).
- `pr list` may surface it, but is not asserted.

### Acceptance criteria
- [ ] `pr create --closes <n>` succeeds and the PR shows the closes relationship
- [ ] `pr create --closes <n>` with a non-existent issue id exits non-zero, creates no PR, and consumes no sequence id (create is atomic)
- [ ] `pr create` without `--closes` behaves exactly as before (no closes field in the event/body)
- [ ] `pr show` output for a PR created before this ticket (no closes) renders exactly as before
- [ ] Existing suite stays green (no assertion weakened)

### Validation
Fulfills: VAL-001
- Verification: `cargo test` (exit zero)
- Command / scenario: manually: `git forge issue new "x"; git forge pr create --source main --base main "" `…; the grep-style checks in gf-dogfood.sh assert the output
- Evidence expected: dogfood assertion asserting `closes: #<n>` in `pr show` output; exit 0

### Notes
- The model decides: single `--closes`, not repeatable (1:1 ticket/issue projection; repeatable is deferred to the adapter's own batch semantics under #10).
- The `store` API must not leak a new public method if a narrow change to the existing `create_pr` signature suffices; the caller-facing signature change is a public contract — keep it minimal and update all tests.
- gf-dogfood.sh must gain a dogfood assertion (not just a unit test) — this is the oracle.
