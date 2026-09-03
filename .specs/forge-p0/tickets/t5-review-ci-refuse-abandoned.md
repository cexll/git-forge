## Issue: t5 — review and CI refuse an abandoned PR

Description: `git forge pr review` and `git forge ci run` refuse an abandoned PR, appending no event (no new review, no new ci.check).
Type: AFK
Milestone: p0-core
Touches: src/cli.rs, src/fold.rs, scripts/gf-dogfood.sh
Blocked by: t4
User stories covered: s0

### What to build
Extend the armed-against-abandoned checks (t4's merge check) to review and CI:

- `pr review <n> --approve|--reject` on an abandoned PR: non-zero exit, message names the abandonment reason, no `pr.review` event appended (review chain unchanged).
- `ci run <n>` on an abandoned PR: non-zero exit, message names the reason, NO `ci.check` appended (existing check state — typically pending from create — is unchanged), and the plan is not materialized/executed (no worktree side effects).
- Both checks run before any write; no identity resolution for a refused write is needed beyond existing command plumbing (keep single-identity invariant, but a refusal that writes nothing must not resolve the actor).
- Existing behavior for non-abandoned PRs is unchanged (review/CI on live PRs unaffected).

### Acceptance criteria
- [ ] `pr review --approve` on abandoned: exit non-zero, review chain unchanged (pr show shows no new decision)
- [ ] `ci run` on abandoned: exit non-zero, no new ci.check (pr show still shows the pending check from create)
- [ ] Both messages name the abandon reason
- [ ] Non-abandoned PR: existing review/CI behavior byte-identical

### Validation
Fulfills: VAL-007, VAL-008
- Verification: `cargo test` (exit zero)
- Command / scenario: manual: abandon a PR; attempt review and ci run
- Evidence expected: dogfood assertions; non-zero exits

### Notes
- ci refusal mirrors the `.forge/ci.sh`-not-a-regular-file refusal semantics (refuse before execution, before any event).
- The pending `ci.check` from create is NOT deleted or modified — it stays, exactly as today, though merge also refuses.
