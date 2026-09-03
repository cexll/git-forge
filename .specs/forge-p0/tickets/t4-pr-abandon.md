## Issue: t4 — pr abandon command: terminal state + reason

Description: `git forge pr abandon <n> --reason <text>` writes a `pr.abandoned` event; the PR becomes terminal, and `pr show` displays abandoned + reason. Merge refuses abandoned PRs.
Type: AFK
Milestone: p0-core
Touches: src/cli.rs, src/store.rs, src/event.rs, src/fold.rs, src/pr_merge.rs, tests/t2_pr.rs, tests/t3_merge.rs, scripts/gf-dogfood.sh
Blocked by: t2
User stories covered: s0

### What to build
New command surface `git forge pr abandon <n> --reason <text>`:

- New EventKind `pr.abandoned`, entity pr, body `{"reason": "<text>"}` (kind+fold in event.rs/fold.rs).
- `abandon` appends the event to the PR `/head` chain (single append, reuse CAS retry loop, single UUID retained).
- `abandon` refuses an already-merged PR and a second abandon (idempotent), and refuses an unknown PR; refs unchanged on refusal.
- `pr show` displays `abandoned: <reason>` for an abandoned PR.
- `pr merge` refuses an abandoned PR: non-zero exit, message names the reason, refs unchanged; the check precedes the merge-gate predicate.
- reason is required: empty/whitespace-only `--reason` is rejected before any write.

### Acceptance criteria
- [ ] `pr abandon <n> --reason <text>` exits zero; `pr show <n>` shows the reason
- [ ] `pr merge` on an abandoned PR exits non-zero, names the reason, no ref change (no `pr.merge` event, base untouched)
- [ ] Already-merged PR: `abandon` exits non-zero, no event appended
- [ ] Unknown PR id: exit non-zero, no counter consumption
- [ ] Missing/empty reason: exit non-zero, no event

### Validation
Fulfills: VAL-005, VAL-006
- Verification: `cargo test` (exit zero)
- Command / scenario: manual: create PR, abandon with a reason, pr show, attempt merge
- Evidence expected: dogfood assertion on abandoned merge refusal

### Notes
- `drop`/`cancel` naming: command is `abandon` (matches decision record `pr.abandoned`). No reopen — a reopened PR is a new PR (different entity).
- The `--reason` is positional vs flag is decided by reviewer at review time; spec names flag because the existing `review`/`comment` grammar keeps payloads in flags/positionals consistently — precedence: `pr create` has positional title, `review` has `--approve` flags; `abandon` gets `--reason` flag.
- `pr list` display is NOT asserted (non-goal in spec Risks); the show path is asserted.
