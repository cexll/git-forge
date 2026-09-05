# Flow: forge-ff-live-base-merge

## Scope
- Actor: CLI operator (human or agent) of git-forge. No server, no authentication, no network.
- Surface: cli (VAL-108 is a docs read on the same app).
- App: git-forge — local, git-native forge; state lives as event chains under `refs/forge/*` inside an ordinary git repository.
- Entrypoint: `git forge pr merge <n> --ff` — fast-forward strategy, with the base branch ALLOWED to be checked out in a live worktree (the P0 checked-out-base guard is relaxed by the clean/refresh contract).
- Related path patterns:
  - `src/pr_merge.rs` — merge gate + strategy dispatch (`--ff`).
  - `src/merge_state.rs` — finalize transaction: exact-base CAS, base-worktree refresh, pending-ref delete, issue close, rollback-on-refresh-failure.
  - `src/store.rs` — worktree refresh; `src/git.rs` — `merge --ff-only` / worktree ops; `src/cli.rs` — `--ff` flag wiring.
  - `tests/t3_merge.rs` — refresh-failure and CAS injection tests.
  - `scripts/gf-dogfood.sh` — the dogfood oracle (ff + live-base sections).
  - `docs/architecture/git-forge.md` (relaxed guard + Known Limitations) and `CONTEXT.md` (glossary).
- Proven by: a-final acceptance sweep, VAL-101..VAL-108, all `pass` (evidence: `.specs/forge-p1/forge-p1/missions/evidence/a-final/`), binary built from forge @ `7871091`.

## Driver contract for future runs
- Start/health:
  - Build the binary fresh from the checkout under test — NEVER reuse a workspace binary. The failure-injection seams (`GIT_FORGE_TEST_MERGE_BARRIER`, `GIT_FORGE_TEST_FAIL_BASE_REFRESH`) are `#[cfg(debug_assertions)]`: a release binary silently ignores them, so the sweep binary is a DEBUG build (`cargo build --locked --offline` without `--release`, no `--target-dir` trickery needed for the seams, a controlled target dir is still fine). Record binary path + forge commit in a `run-paths` sink.
  - Toolchain on this machine: every cargo/commit command needs `PATH="$HOME/.cargo/bin:$PATH" RUSTUP_TOOLCHAIN=1.93.0-aarch64-apple-darwin` (the default brew cargo SIGABRTs, rc=134).
- Seed/auth:
  - Disposable scratch repo (`mktemp -d`, trap cleanup) — `git init -q -b main`, `user.name`/`user.email` set, one base commit carrying `.forge/ci.sh` (`#!/bin/sh\necho ok`) and a committed `.gitignore`. Resolve the base branch dynamically; never hardcode a branch name.
  - Issue/PR ids come from ONE lazy shared counter (the proven run consumed #1..#24 in sequence) — NEVER hardcode; parse `issue #<n> created:` / `PR #<n> created:` from command output with `sed -n 's/.../\1/p'`.
  - Standard fixture helper (`mkpr`): `issue new <title>` → `pr create --source <branch> --base <base> --closes <issue> <title>` → `pr review <n> --approve` → `ci run <n>` (green `.forge/ci.sh`), so the approve/CI gates pass and `--ff` is the only variable.
- Primary action: merge a gate-green PR with `--ff` from the source checkout (or with the base checked out and clean); then assert result/ref/worktree state. Refusals (non-ff, moved base, dirty base, CAS clash, refresh-failure injection) each exit nonzero with refs byte-unchanged.
- State oracle:
  - Merge stdout: `merged PR #<n> into <base> (<sha>)`.
  - `git forge pr show <n>` — `merged: <sha>` line present exactly on success; absent after every refusal.
  - Ref state: `git rev-parse --verify -q <ref>` for `refs/heads/<base>`, `refs/forge/prs/<n>/head`, `refs/forge/issues/<m>`; parents: `git rev-list --parents -n 1 <base>` (2 fields = single parent).
  - Worktree: `git status --porcelain` (empty = clean; UNTRACKED counts as dirty, ignored does NOT); worktree HEAD via `git rev-parse HEAD` must equal the base ref after refresh.
- Evidence sinks: one transcript per scenario (command, stdout, stderr verbatim on refusals, exit code); ref values before/after every mutating command; `pr show`/`issue show` snapshots; a run-log summary line.
- Cleanup/isolation: scratch repo + worktree root removed on exit; forge refs only observed (`git log`/`rev-parse`) or driven through the CLI; nothing in a live checkout touched.
- Forbidden boundary: never hand-craft or rewrite `refs/forge/*`; no `git push`/`pull`; never weaken a refusal check into a skip; the `^open` grep on `issue show` is a known trap — `issue show` prints `#13 rf bug — open`, so a `^open`-anchored grep counts 0; anchor on the `open` suffix or read the raw line.

## Future run steps
1. Fresh DEBUG build with the rustup env; record `run-paths` (binary path, forge commit).
2. Scratch repo init (see Seed/auth); record base tip; `git checkout -q -b feat/ff`, one change commit, `git checkout -q <base>`.
3. VAL-101 (ff success, result == source_head): `mkpr "ff bug" "ff PR" feat/ff`; record `BASE_TIP`/`SRC_HEAD`; `git checkout -q feat/ff`; `git forge pr merge <n> --ff` → exit 0, stdout `merged PR #<n> into <base> (<sha>)`. Assert: `rev-parse <base>` == `SRC_HEAD` (result commit IS the verified source head); `git rev-list --parents -n 1 <base>` prints exactly 2 fields with parent == `BASE_TIP` (no merge commit, single-parent chain preserved); `pr show <n>` contains `merged: <SRC_HEAD>`.
4. VAL-102 (non-ff refusal names rebase/recreate): advance `<base>` one commit; `git checkout -q -b feat/oldb HEAD~1`; one commit; `mkpr "noff bug" "noff PR" feat/oldb` (merge_base != base_head); checkout source; `git forge pr merge <n> --ff` → exit 1; output contains `cannot fast-forward`, `merge_base != base_head`, and the remedy `use \`pr merge --rebase\` to replay the source or recreate the PR`; `<base>` ref unchanged; `pr show <n>` has no `merged:` line.
5. VAL-103 (moved-base refusal names recreate-or-rebase-and-recheck): create PR at snapshot base B1; advance `<base>` to B2 AFTER the create; checkout source; `git forge pr merge <n> --ff` → exit 1; output contains `base branch '<base>' has moved since PR #<n> was created (B2 != snapshot B1)` and the token `recreate-or-rebase-and-recheck`; `<base>` still B2; `refs/forge/prs/<n>/head` unchanged; PR unmerged.
6. VAL-104 (exact-base CAS clash, no partial write): ff-able PR; record base/pr-head/issue ref values. `GIT_FORGE_TEST_MERGE_BARRIER=<dir> git forge pr merge <n> --ff` backgrounded from the source checkout; wait for `<dir>/ready` (30s deadline); inject the concurrent base advance with commit-tree plumbing — `NEW_BASE=$(git rev-parse <base>^{tree} | xargs git commit-tree -m "concurrent base advance")`, `git update-ref refs/heads/<base> $NEW_BASE $BASE_BEFORE`; write `<dir>/release`; wait for the merge. Assert: exit 1; stderr names the ref-lock CAS failure (`cannot lock ref refs/heads/<base>: is at <NEW_BASE> ... expected <BASE_BEFORE>`); `<base>` holds the concurrent value (merge wrote nothing); pr-head and issue refs byte-identical to before; at most the pending result ref remains, nothing else written.
7. VAL-105 (clean checked-out base merges + refresh): base checked out in the main worktree and clean; `mkpr "live bug" "live PR" feat/live`; merge `--ff` → exit 0; `git status --porcelain` empty (worktree was refreshed); worktree HEAD == `rev-parse <base>` == result commit; worktree file content matches `git show <base>:<file>`.
8. VAL-106 (dirty vs ignored base): plant an untracked file in the base worktree → merge `--ff` → exit 1, stderr `base branch <base> is checked out in a dirty worktree ... including untracked files`; `<base>` unchanged. Remove it; plant an ignored-only file (pattern committed BEFORE the PR snapshot) → `git status --porcelain` still empty (default flags ignore it) → merge exits 0; porcelain empty after refresh.
9. VAL-107 (refresh-failure never split, --closes path): `mkpr "rf bug" "rf PR" feat/rf` (PR created WITH `--closes <m>`); record base/pr-head/issue ref values; `GIT_FORGE_TEST_FAIL_BASE_REFRESH=1 git forge pr merge <n> --ff` → exit 1; stderr contains `merge completed but base worktree refresh failed: ...` and `(worktree left at ...)` (leftover reported), and ZERO occurrences of `ROLLBACK FAILED`; `<base>` rolled back to the pre-merge tip; worktree HEAD == `<base>` ref (never split); porcelain empty; `refs/forge/prs/<n>/head` and `refs/forge/issues/<m>` restored byte-identical; `issue show <m>` still open (anchor the `open` suffix, see forbidden boundary); PR unmerged. Control: identical seam on a PR WITHOUT `--closes` → exit 1, base restored, `ROLLBACK FAILED` count 0.
10. VAL-108 (docs declare the out-of-scope surface): read `docs/architecture/git-forge.md` Known Limitations — it must name sparse-checkout AND submodules in a base worktree as out of L1 scope (may refuse or mis-refresh), not silently undefined.

## Blocking checks for future runs
- The binary under test is freshly built from the checkout under test, and DEBUG for any injection scenario (a release binary silently ignores the seams — injection scenarios then "pass" by refusing for the wrong reason or hang at the barrier).
- `git forge pr merge <n> --ff` on a gate-green, strictly-on-top PR exits 0 and prints `merged PR #<n> into <base> (<sha>)`; result commit == source head; base tip has exactly ONE parent equal to the pre-merge base tip.
- Every refusal below exits nonzero with the named stderr fragment, and base/pr-head/issue refs are byte-identical before/after: non-ff (`merge_base != base_head` + rebase/recreate remedy), moved base (`has moved since PR #<n> was created` + `recreate-or-rebase-and-recheck`), dirty base (`dirty worktree ... including untracked files`), CAS clash (`cannot lock ref refs/heads/<base> ... expected ...`), refresh failure (`base worktree refresh failed` + `worktree left at`).
- Refresh failure NEVER splits: base ref, worktree HEAD, pr head ref, and issue ref all agree with the pre-merge snapshot; no `ROLLBACK FAILED` line; the `--closes` linkee stays open.
- Ignored-only dirt in the base worktree does NOT refuse (merge succeeds); untracked dirt DOES.
- With the base checked out and clean, the merge succeeds and the worktree is refreshed to the result commit (`status --porcelain` empty, HEAD == base ref).
- Issue/PR ids are parsed from command output at every step (hardcoded ids break on the shared lazy counter).

## Negative / boundary checks
- `pr merge <n> --ff` on a PR whose source branched from an older tip refuses even though the base has not moved since (snapshot-level ff precondition: `merge_base != base_head`); remedy text names `pr merge --rebase` or recreating the PR.
- The CAS-clash scenario leaves the base at the concurrent value — the merge must not force-move or re-lock over a lost race; at most a pending result ref may remain, and no pr/issue event is appended.
- The refresh-failure control (PR without `--closes`) exercises the same rollback path with one fewer ref: base restored, nothing else written.
- After a refused merge the PR snapshot is untouched: `pr show` shows no `merged:` line, and review/CI state is unchanged.
- Known trap: `issue show` renders open status inside the title line (`#13 rf bug — open`); a `^open` grep misreads open as closed.

## Evidence expected
- One transcript per scenario (VAL-101..VAL-108) with a header naming the binary path, the forge source commit, and the date; every command with stdout, stderr (verbatim on refusals), and exit code.
- Ref-state ledger: `rev-parse` values of `refs/heads/<base>`, `refs/forge/prs/<n>/head`, `refs/forge/issues/<m>` before and after every merge attempt (success and refusal), plus `git rev-list --parents -n 1` for the single-parent check.
- `pr show` snapshots: `merged: <SRC_HEAD>` on success; no `merged:` line after every refusal; `issue show` snapshot showing the linkee open after the refresh-failure rollback.
- Verbatim stderr of each refusal, e.g. `cannot lock ref refs/heads/main: is at 40b2537 ... expected a3699f3` (CAS) and `merge completed but base worktree refresh failed: test seam ... (worktree left at ...)` (refresh).
- The VAL-108 Known-Limitations excerpt naming sparse-checkout and submodules.
- A run-log ending line `ALL <k> ASSERTIONS EXERCISED OK` (the proven run: `.specs/forge-p1/forge-p1/missions/evidence/a-final/VAL-101-transcript.txt` .. `VAL-108-known-limitations.txt`).
