## Issue: t6 — abandon idempotence and boundary matrix

Description: The abandon terminality is enforced on all boundaries: second abandon refuses without writing; merged PR refuses; refs stay unchanged on every refusal.
Type: AFK
Milestone: p0-core
Touches: src/cli.rs, src/fold.rs, scripts/gf-dogfood.sh
Blocked by: t5
User stories covered: s0

### What to build
Close the terminal-state matrix (fold + CLI):

- Second `abandon` on an already-abandoned PR: exit non-zero, message names the ORIGINAL reason, no second `pr.abandoned` event.
- Already-merged PR: abandon refuses, no event.
- The fold exposes `abandoned: Option<String>`; a folded PR whose chain has one abandoned event is terminal, and no later event can be appended after abandonment from the CLI (all mutators check).
- `pr show` on an abandoned PR renders the same single reason; list renders without duplicating.
- Refusal paths are all no-op: no counter consumption beyond the entity; no ref mutation.

### Acceptance criteria
- [ ] Second abandon: non-zero exit, chain still one `pr.abandoned`, show displays original reason
- [ ] Merged-then-abandon: non-zero exit, no event
- [ ] All mutators (merge/review/ci/abandon) refuse an abandoned PR with the same reason text
- [ ] gf-dogfood.sh covers the double-abandon and merged-abandon refusals

### Validation
Fulfills: VAL-009, VAL-010
- Verification: `cargo test` (exit zero)
- Command / scenario: manual: abandon twice; merge then abandon
- Evidence expected: dogfood assertions; non-zero exits; single reason shown

### Notes
- This ticket completes the fold on the "terminal" contract — the whole matrix is one state machine, no separate "state" exit paths.
- The reason shown on second abandon must be the ORIGINAL reason (not a generic message) — verifying that distinguishes the idempotent refusal from a broken read of the first event.
