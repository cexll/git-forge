# CI Process-Group Termination via Direct kill(2) Syscall (Review-Round Hardening)

Status: active

## Problem

A re-review of the CI gate (`src/ci.rs` / `src/cli.rs` / `src/ci/validate.rs`)
found the group-termination primitive and several adjacent defects:

- `signal_pg`/`pgid_alive` terminated and probed the plan's process group by
  SPAWNING `/bin/kill`. The fork+exec round-trip widened the post-reap
  pid-reuse window (a pgid recycled into an unrelated group between reap and
  signal could be hit), added a spawn-failure mode (`/bin/kill` itself cannot
  spawn after the user's process quota is exhausted — exactly the failure the
  deadline path must survive), and spawned with an inheritable `PATH` that
  `tighten_kill_env` existed only to defend (a shim `kill` would defeat the
  containment).
- The descendant-survival failure path returned `timed_out: true`, so a plan
  that exited cleanly while leaking a background process was reported as
  "timed out" (a wrong failure mode). The run outcome carried no detail
  channel.
- `cmd_ci_run` leaked the temp worktree when `worktree_add_ci` failed AFTER
  `git worktree add` had registered it: the `!ok_wt` path released the lock and
  returned without removing the worktree.
- The Just quote-close rule existed in three places and had DRIFTED:
  `split_top_level_commas` treated a `\` before a closing `'` as escaping it,
  but Just single-quoted strings are RAW — `[confirm('x\'), script]` split the
  comma as top-level and refused a benign attribute.
- The `resolve_just` inline comment claimed "ONLY system install locations …
  cannot influence through HOME" directly above a body that searches
  `$HOME/.cargo/bin` / `$HOME/.local/bin` (contradictory).
- `CiPlan.label` was a stringly-typed field that could drift from the variant.

## Decision (implemented)

1. **Terminate and probe the group with a direct `kill(2)` syscall** via
   `libc::kill(-pgid, sig)` / `libc::kill(-pgid, 0)` (`signal_pg`,
   `pgid_alive`). Errors are errno-typed: `ESRCH` → group empty (`Ok(false)`
   for the probe), `EPERM` → fail-closed `Err` (a live group we cannot signal),
   anything else → `Err`. This removes the PATH-shim vector and the
   spawn-failure mode, and narrows the pid-reuse window to the syscall boundary
   (reap→signal is now one instruction; the window is minimized, not eliminated
   — the OS may still recycle a pgid, a documented containment residual).
   `tighten_kill_env` is DELETED (nothing is spawned); `sanitize_loader_env` is
   RETAINED because loader preloads (`DYLD_INSERT_LIBRARIES`, …) still apply to
   the `bash`/`just`/CI `git` spawns.
2. **Add `CiRun.detail`** (`Option<&'static str>`). The descendant-survival
   path reports `detail: Some("left a background process running")` with
   `timed_out: false`; the merge-gate predicate is unchanged (still
   `status == Some(0) && !timed_out`). `cli` formats `timed_out` →
   ": timed out", else `detail` → ": {detail}", else the exit status.
3. **Remove the worktree on a `worktree_add_ci` failure after registration**:
   the `!ok_wt` path in `cmd_ci_run` now calls `worktree_remove_ci` before
   releasing the lock. Removal stays best-effort (a leftover is reported, never
   silently discarded). A `#[cfg(debug_assertions)]` seam
   (`GIT_FORGE_TEST_FAIL_WORKTREE_ADD=1`) in `worktree_add_ci` forces the path
   so the cleanup is red→green tested.
4. **Consolidate the Just quote-close rule** into one `closes_quote` helper
   used by `find_unquoted_colon`, `split_top_level_commas`, and
   `without_comment`: a closing quote matches when `bytes[i] == q && (q ==
   b'\'' || !is_escape_at(bytes, i))` (single quotes raw; backslash parity only
   for `"`). A scanner fix now lands in one place.
5. **Fix the `resolve_just` inline comment** to describe phase 1 (system dirs)
   without the false "ONLY / cannot influence via HOME" claim; the
   `~/.cargo/bin` fall-through and the trusted-operator rationale live in the
   doc comment.
6. **Replace the stringly `CiPlan.label` with an enum**
   (`CiSh` / `JustCheck(&'static str)`) whose `label()` returns the persisted
   plan name; `JustCheck` carries the snapshot justfile relative path.

## Alternatives considered

- Keep `/bin/kill` and only tighten its env: rejected — fork+exec latency keeps
  the wider pid-reuse window, the spawn-failure mode remains (the deadline path
  must survive quota exhaustion), and the PATH shim needs perpetual defense.
- `std`-only: rejected — the standard library exposes no `kill(2)`; `libc` is
  the minimal direct binding and is already in the tree transitively via
  `git2`, so the new direct dependency reuses the pinned lock entry.
- Poll `/proc` for liveness: rejected — Linux-only, not portable to the macOS
  dev target; `kill(pid, 0)` is the POSIX probe.
- Consolidate `resolve_git`/`resolve_just` into one trusted-binary resolver
  (review finding): REFUTED — they genuinely differ (`git` needs the absolute
  path returned, `just` the relative justfile name) and only `just` documents a
  PATH self-attack residual; a shared abstraction would be an Optional/None
  kludge. Similarly the proposed worktree single-ownership "verify-gone"
  postcondition was REFUTED: removal returns the leftover for caller repair,
  and a silent re-remove would double-delete.

## Consequences (implemented)

- The post-reap pid-reuse window is narrowed to the syscall boundary; the
  `/bin/kill` spawn-failure mode and its PATH-shim vector are gone.
- A leaked background process is now reported as such (not "timed out"); the
  run records a failed Check either way.
- A `worktree_add_ci` registration failure no longer leaks the temp worktree.
- New direct dependency `libc = "0.2"` (justification: no `std` `kill(2)`;
  already transitively pinned via `git2`, MIT/Apache). Allowed-license policy
  holds.
- This record PARTIALLY SUPERSEDES `ci-plan-env-isolation.md`: its
  env-tightening (decision 1), trusted-path resolver (decision 2), and
  closure-validator (decision 4) are unchanged; its bounded-reap policy
  (decision 3) is unchanged in substance (`reap_with_grace` still bounds the
  reap when a kill fails with EPERM) but the kill PRIMITIVE is now a syscall,
  so the `/bin/kill could not spawn` framing there is historical.
