## Issue: t2 — finalize transaction closes the linked issue

Description: When a PR created with `--closes <n>` merges, the `issue.close` is written in the SAME `update-ref` transaction as base CAS + PR-head CAS + pending-result delete.
Type: AFK
Milestone: p0-core
Touches: src/pr_merge.rs, src/store.rs, scripts/gf-dogfood.sh
Blocked by: t1
User stories covered: s0

### What to build
In `store.finalize_pr_merge` (and its caller in `pr_merge`), when the merged PR has a `closes` target:

- Construct an `issue.close` event commit whose parent is the issue chain tip (tree = `.forge/event.json`, message `forge:issue.close:<n>`), and CAS `refs/forge/issues/<n>` from its expected tip — in the SAME `update-ref --stdin` batch as: delete pending-result ref, CAS `refs/heads/<base>`, CAS PR `/head`. One transaction, all-or-nothing.
- On the existing finalize CAS retry loop: rebuild the issue-head parent/expected tip on retry; reuse the single event UUID pair across retries (no second pair of events).
- If the target issue is already closed when merge reaches finalize: do NOT append a second `issue.close`; the merge proceeds with base+PR actions only (idempotent no-op).
- If any part of the transaction fails: nothing applies — issue stays open, no `pr.merge` event, base untouched (existing failure semantics extended to the issue ref).

### Acceptance criteria
- [ ] Merge with `--closes` leaves the linked issue closed on success
- [ ] Already-closed target: merge still succeeds, no second close event, fold state unchanged (still `open: true`→ no — still closed, i.e. `open: false`; the fold's issue chain length is unchanged)
- [ ] Failed transaction (test-injected CAS clash with GIT_FORGE_TEST_MERGE_BARRIER or a forced base-move) leaves issue open, no partial event
- [ ] The empty-body `issue.close` event's actor/ts are resolved through the same single identity resolution as every other write (no second path)
- [ ] Dogfood oracle gains a merge-closes assertion (green in the ordinary merge path)

### Validation
Fulfills: VAL-002, VAL-003
- Verification: `cargo test` (exit zero)
- Command / scenario: manual three-script: create issue, PR with --closes, resolve gate, merge → `issue show` closed; with issue pre-closed → merge succeeds; with gate red → merge fails & issue stays open
- Evidence expected: gf-dogfood.sh merged-issue-closed assertion green

### Notes
- This is the store.rs white-box human-review surface. The unit test must prove transaction atomicity with an injected failure (not only the happy path).
- The closure in the transaction uses the EXISTING empty body `{}` — no schema change here; only the writer changes (finalize writes it, not `git issue close`).
