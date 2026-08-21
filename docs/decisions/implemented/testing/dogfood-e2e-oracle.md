# Dogfood E2E Oracle Lives In-Repo as the Executable Merge-Lifecycle Check

Status: active

## Problem

The L1 e2e surface was a placeholder: `just e2e` resolved to
`tests/e2e_workflow.rs` and `tests/e2e_counter.rs`, which do not exist. The
proven 45-check dogfood oracle — merge gates, PR-create guards, review-CAS,
concurrent comment folding, stale-base and checked-out-base refusals — lived
only as an ad-hoc script in `/tmp`: reproducible on this machine but owned by
no commit. AGENTS.md's Verification Matrix claimed an e2e surface the repository
did not contain, and the merge-lifecycle oracle could be lost or silently drift
from the code it exercises with no tracked home.

## Decision

House the proven 45/45 dogfood script in-repo as `scripts/gf-dogfood.sh`, the
executable merge-lifecycle oracle. It self-builds the release binary from the
checkout that contains it (never a stale binary) into a controlled temp
`--target-dir` — immune to `CARGO_TARGET_DIR` / cargo `target-dir` config, so
the checkout never gains a git-visible `target/` — then hard-copies
`git-forge`/`git-issue`/`git-pr` dispatch binaries into a private temp bin dir.
It runs against disposable clones of `GDOGFOOD_SRC` (env, default dsh-deepwork)
with trap-EXIT cleanup, keeping the fixed deterministic branch/tag setup (local
feature branches and a local `v1.0` tag created inside the clone; the source
repo's state is never the test surface).

`just dogfood` is the entry point: it runs the script and exits 0 only when the
summary reports `pass=45 fail=0`. The AGENTS.md Verification Matrix now names
the real e2e surfaces — the t2_pr/t3_merge integration tests (`just test`) plus
`scripts/gf-dogfood.sh` (`just dogfood`) — and no longer claims the nonexistent
files. `just e2e` remains a placeholder for future dedicated e2e test files: it
exits 0 with an honest note naming the real surfaces.

## Alternatives considered

- **Keep the oracle only in `/tmp`**: rejected — a machine-local script is not a
  shipped artifact; the verified 45/45 behavior has no tracked home, cannot be
  reviewed in a PR, and drifts silently from the code it exercises.
- **Add `tests/e2e_workflow.rs` now**: rejected — standing up a dedicated
  integration harness (test fixtures, repo orchestration, isolation) is a real
  project; the executable oracle already covers the merge-lifecycle 45 checks,
  and the placeholder stays honest until that harness lands.
- **Gate `tests/` files with the 800-line size gate**: rejected — tests/t3_merge.rs
  exceeds 800 code lines and is tracked as an open concern pending a separate
  split; that split is recorded in its own decision
  (process/800-line-file-gate-blocking-tokei) and stays out of this change.

## Consequences

- `just dogfood` has a local prerequisite: `GDOGFOOD_SRC` (default
  /Users/chenwenjie/workspaces/dsh-deepwork) must be an existing directory that
  is a git repository and holds the working files the checks use (PLAN.md). The
  script preflights this before cloning and fails with a clear one-line error
  naming the path and the requirement when any check fails.
- Verification policy now has a repo-owned executable oracle: the 45/45
  merge-lifecycle behavior is reproducible anywhere with the prerequisite, and
  the AGENTS.md Verification Matrix rows describe real, owned surfaces rather
  than nonexistent files.
- The self-build writes only under the run's temp dir (`--target-dir`), so the
  checkout stays free of build artifacts and the tested binary always comes
  fresh from this checkout.
- `tests/` line-size remains ungated pending the separate t3_merge split;
  `just e2e` stays a placeholder until dedicated e2e test files land.