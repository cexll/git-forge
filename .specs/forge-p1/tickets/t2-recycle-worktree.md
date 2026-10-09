## Issue: t2 — #4 source worktree/branch recycle after pr merge

Description: `git forge pr merge <n> --recycle-worktree <path>` removes the merged source worktree and conditionally deletes its branch, returning a structured outcome line for orchestrators. Runs after base refresh (#3) and finalization; never touches the base worktree; salvage renaming of unmerged branches is pipeline-side and happens BEFORE this call.
Type: AFK
Milestone: merge-core
Touches: src/pr_merge.rs, src/merge_state.rs, src/cli.rs, src/git.rs, docs/architecture/git-forge.md, tests/t3_merge.rs, scripts/gf-dogfood.sh
Blocked by: t1
User stories covered: forge-pipeline-p1

### What to build
Per issue #4's spec amendment (command shape and recycle ownership):

- Shape: `git forge pr merge <n> --recycle-worktree <path>`.
- Validation: path must be a REGISTERED worktree whose checked-out branch is the PR source_ref. Wrong/unregistered paths are refused with refs untouched.
- Dirty check: a dirty worktree is refused without `--force` (untracked counts, same `git status --porcelain` semantics as #3).
- Branch deletion ONLY when the source tip is an ancestor of the integrated base tip: `--ff`/`--merge` delete the branch; `--squash`/`--rebase` keep it and REPORT why.
- Structured outcome line for orchestrators, exactly this vocabulary: `removed | refused-dirty | refused-not-contained | partial-leftover(<path>) | branch-deleted | branch-kept(<reason>)`.
- Sequencing: the recycle runs after base refresh (#3) and finalization; it never touches the base worktree. Salvage renaming of an unmerged branch is adapter/pipeline-side and runs BEFORE this call — forge does not salvage.
- Docs: wire-contract row for the new flag + outcome vocabulary; the switchover announcement lands in this ticket's OLD-PATH integration commit message and Issue comment (one commit boundary per the switchover ADR).

### Acceptance criteria
- [ ] Clean registered source worktree, checked-out branch == PR source_ref, `--ff` merge: worktree removed (absent from `git worktree list`), branch deleted, outcome `removed` + `branch-deleted`
- [ ] Unregistered path or worktree not on the PR source_ref: exits non-zero, outcome `refused-not-contained`, refs unchanged, the referenced worktree is untouched
- [ ] Dirty source worktree without `--force`: exits non-zero, outcome `refused-dirty`, worktree and branch intact; with `--force`: proceeds
- [ ] `--squash`/`--rebase` merge with recycle: branch kept, outcome `branch-kept(<reason>)` naming why; `--ff`/`--merge` delete the branch
- [ ] Partial-removal injection: outcome `partial-leftover(<path>)` naming the path
- [ ] Switchover ordering contract held: #9/#3/#4 landed by the old path (no forge PR) and the announcement lives in this ticket's OLD-PATH integration commit message + Issue comment
- [ ] gf-dogfood.sh covers the outcome vocabulary (removed / refused-dirty / refused-not-contained / branch-deleted / branch-kept)

### Validation
Fulfills: VAL-109, VAL-110, VAL-111, VAL-112, VAL-118
- Verification: `cargo test` (exit zero)
- Command / scenario: dogfood: feature worktree + `pr merge --ff --recycle-worktree <path>`; unregistered/dirty/wrong-branch refusals; squash + recycle for branch-kept; partial-removal injection in tests/t3_merge.rs
- Evidence expected: dogfood assertions on outcome lines green; injection green; cargo test exit 0

### Notes
- The outcome line is the forge→adapter contract (adapter contract clause 8): the adapter consumes it verbatim and persists it before clearing feature.worktree/branch. Keep the vocabulary exact.
- This ticket lands by the OLD path (no forge PR) per the switchover ADR; its integration commit announces the switchover.
- Recycling is explicit per-merge, not hooked (hooks are L2, not built).