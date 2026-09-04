## Issue: t0 — #9 pr merge --ff: fast-forward strategy

Description: `git forge pr merge --ff` executes a fast-forward: result_commit == source_head (no new commit, single-parent chain preserved); precondition merge_base == base_head; before #2 lands, a moved base refuses naming recreate-or-rebase-and-recheck; after #2, the ff gate revalidates against the live-base result.
Type: AFK
Milestone: merge-core
Touches: src/pr_merge.rs, src/merge_state.rs, src/store.rs, src/cli.rs, src/git.rs, tests/t3_merge.rs, scripts/gf-dogfood.sh
Blocked by: None
User stories covered: forge-pipeline-p1

### What to build
Add the `--ff` strategy to `pr merge` per issue #9's spec amendment (two-phase fast-forward contract):

- Strategy semantics: execute the equivalent of `git merge --ff-only <source-oid>` in the disposable (or, post-#3, the clean checked-out base) worktree. `result_commit == source_head` — no new commit is constructed; the single-parent chain of source_head is preserved.
- Precondition: `merge_base == base_head` (the snapshot source sits directly on top of the snapshot base). A non-ffable PR fails with a distinct error naming the rebase remedy.
- Phase gate (pre-#2): current base != snapshot base_head → refuse, naming recreate-or-rebase-and-recheck.
- Post-#2 compatibility clause: once #2 ships, the ff gate is revalidated against the live-base result per #2's contract (result-bound CI + result-bound approve). Write the clause now, gated on #2's landing.
- Finalize flows through `merge_state.rs`'s existing transaction with exact-base CAS: the CAS expectation is the PR snapshot's base_head.

### Acceptance criteria
- [ ] `pr merge --ff` on a strictly-on-top source exits zero; `pr show` displays result_commit == source_head; the integrated base tip's parent is the prior base tip (no merge commit)
- [ ] Non-ffable PR + `--ff`: exits non-zero with a distinct error naming the rebase remedy; no ref change; PR remains unmerged
- [ ] Moved base (current base != snapshot base_head, pre-#2): refuses naming recreate-or-rebase-and-recheck; base/PR refs unchanged
- [ ] CAS clash injected (base advanced between expectation check and finalize): merge fails, no partial write (base/PR/issue refs all unchanged) — exact-base CAS holds
- [ ] Existing merge strategies unchanged: merge-commit/squash/rebase paths byte-identical in behavior
- [ ] gf-dogfood.sh gains --ff assertions (success + non-ff refusal)

### Validation
Fulfills: VAL-101, VAL-102, VAL-103, VAL-104
- Verification: `cargo test` (exit zero)
- Command / scenario: dogfood: issue → pr → review --approve → ci run → `pr merge --ff`; verify result_commit==source_head via pr show + branch tip; advance base and repeat for refusals; CAS-clash injection in tests/t3_merge.rs
- Evidence expected: dogfood assertions green; t3_merge.rs CAS injection green; cargo test exit 0

### Notes
- The wire body stays `{strategy, result_commit}` with `strategy: "ff"`; `v: 1` unchanged, no new event kind.
- Reuse the P0 finalize transaction (merge_state.rs) — this ticket changes the strategy predicate and the constructed result, not the transaction machinery.
- The error text for moved-base MUST contain the exact remedy phrase `recreate-or-rebase-and-recheck` (asserted).