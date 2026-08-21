# Git Adapter Extraction (merge-execution shell-outs to src/git.rs)

Status: active

## Problem

`src/cli.rs` had grown to 1243 lines and owned every git-binary shell-out
behind `git forge pr merge`: running git in a directory, temporary-worktree
add/remove/list, strategy execution (`merge --no-ff --no-edit`, `merge --squash`
+ `commit -m`, `rebase --onto`), and failure cleanup. Five early-return sites in
`cmd_pr_merge` each duplicated the same release-lock + delete-pending-result-ref
block. AGENTS.md's module map described a `gate.rs`/`sync.rs`/`hooks.rs` layout
that no longer matched the real source tree, so the documented ownership
boundaries drifted from the code.

## Decision

Extract every merge-execution git shell-out into a new `git_forge::git` module
(`src/git.rs`), which becomes the single home for shell-outs to the git binary:
`git_in` (run git in a directory), worktree add/remove/list plus the debug-only
lock test seam, strategy execution (`execute_strategy`), failure cleanup
(`cleanup_failed_worktree`), and `require_single_merge_base` (+ the
zero-merge-base error helper). The repeated pending-result cleanup in
`cmd_pr_merge` is folded into one shared `cleanup_pending_result` helper while
each call site keeps its own distinct error string. `cmd_pr_diff`'s `git diff`
shell-out and the merge-gate predicate remain in `cli.rs` (orchestration layer).
AGENTS.md and `docs/architecture/git-forge.md` are synced to the real src
layout in the same change.

## Alternatives considered

- **Full `gate.rs` split**: move the merge-gate predicate and its command
  parsing into a separate module. Rejected: the gate reads PR state from the
  store and shares orchestration context with `cmd_pr_merge`; splitting it
  would scatter one cohesive flow across modules without removing a single
  shell-out, and the module map had already drifted once — a smaller, real
  boundary is easier to keep truthful.
- **Keeping everything in `cli.rs`**: no new module, just the shared
  `cleanup_pending_result` helper. Rejected: cli.rs stays at ~1200+ lines, the
  merge-execution plumbing remains entangled with argument parsing and
  user-facing output, and there is still no single owner for git shell-outs to
  audit (L3 review surface).
- **Per-command shell-out wrappers**: a thin helper per git invocation (e.g.
  `git_worktree_add`, `git_merge`) with no shared strategy/cleanup logic.
  Rejected: it adds a wrapper per call site but keeps the duplicated cleanup
  and abort/reset ordering problems; a single adapter with strategy execution
  and cleanup co-located is what actually removes duplication.

## Consequences

- `src/git.rs` is the single home for merge-execution git shell-outs; reviews
  of irreversible operations (ref writes, merges, worktree mutation) now audit
  one small module plus the orchestration in `cmd_pr_merge`.
- Behavior preservation is a hard contract (VAL-101): extraction must not
  change error strings, exit codes, or cleanup semantics. In particular the
  squash path keeps its pre-extraction ordering — `git merge --squash` stages
  first, THEN the title is checked, so a missing title produces the cleanup
  error after staging (`reset --hard HEAD` + `git clean -fd` + worktree
  removal). F-001 (restoring that ordering after the extraction moved the
  title check before staging) is fixed in the same feature round and is not
  waived by this record.
- Docs must be kept in sync with the real layout: AGENTS.md's module map and
  `docs/architecture/git-forge.md` name `src/git.rs` as the git adapter and
  no longer mention absent modules (`gate.rs`, `sync.rs`, `hooks.rs`); any
  future module change updates both docs in the same change.
