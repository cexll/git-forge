## Issue: t1 — #3 post-merge base refresh: allow merge while base is checked out

Description: Relax the checked-out base guard: `pr merge` succeeds while base_ref is checked out in a non-temporary worktree, provided that worktree is clean; after the base CAS, the base worktree is refreshed to the result commit. Refresh failure exits non-zero with refs unchanged (no partial move) and reports the worktree as a leftover.
Type: AFK
Milestone: merge-core
Touches: src/pr_merge.rs, src/merge_state.rs, src/store.rs, src/git.rs, docs/architecture/git-forge.md, tests/t3_merge.rs, scripts/gf-dogfood.sh
Blocked by: t0
User stories covered: forge-pipeline-p1

### What to build
Per issue #3's spec amendment (clean-worktree refresh semantics):

- clean = `git status --porcelain` (default flags) empty for the base worktree. UNTRACKED files count as dirty; ignored files do NOT. Sparse-checkout and submodules are out of scope for L1 — say so in docs (`docs/architecture/git-forge.md` Known Limitations).
- The checked-out base guard stops refusing a clean base worktree; it still refuses a dirty one.
- Refresh after base CAS: advance the base worktree's checked-out files to the result commit. On refresh failure: exit non-zero, refs UNCHANGED (chosen posture: no partial move), reporting the worktree as a leftover.
- The refresh runs inside the same consistent post-state as finalize — refs+tree must never be split (never a moved ref with a stale tree, nor a refreshed tree with a moved-back ref).
- Tests per the amendment: dirty base refused with refs untouched; clean base merged and worktree files match the result commit; refresh-failure injection leaves refs+tree consistent — never split.
- Guard-assertion reversal: existing dogfood assertions that expect refusal on a checked-out base flip in the same change (this is the reversal window P0's spec named for the #3/#9 batch).

### Acceptance criteria
- [ ] Clean main-tree base (base_ref checked out, `git status --porcelain` empty): merge exits zero; after merge the worktree files match the result commit and the tree is clean
- [ ] Dirty base (modified tracked file AND untracked file cases): merge exits non-zero, base ref value unchanged; untracked counts as dirty
- [ ] Ignored-only dirt: merge proceeds (ignored does NOT count as dirty)
- [ ] Refresh-failure injection: merge exits non-zero, base ref and worktree HEAD agree (no partial move), output reports the worktree as a leftover
- [ ] docs/architecture/git-forge.md: guard section updated; Known Limitations names sparse-checkout and submodules as out of L1 scope
- [ ] gf-dogfood.sh: refusal-on-checked-out-base assertions reversed; clean-merge/dirty-refusal assertions added

### Validation
Fulfills: VAL-105, VAL-106, VAL-107, VAL-108
- Verification: `cargo test` (exit zero)
- Command / scenario: dogfood with base checked out in main tree: clean merge + file comparison; dirty (tracked/untracked) refusal; ignored-only pass; refresh-failure injection in tests/t3_merge.rs
- Evidence expected: dogfood assertions green; refresh-failure injection green; cargo test exit 0

### Notes
- The refresh is the unblock for the switchover's target command surface (`pr merge --ff --recycle-worktree` from the normal integration setup) — devflow's repoRoot always holds the integration branch, so every post-switch merge goes through this path.
- `--force` does NOT apply here: no flag unlocks a dirty base tree (the dirty refusal is the invariant; #4's `--force` concerns the source worktree only).