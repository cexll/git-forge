# Dogfood oracle: canonical-root preflight, locked/offline build, e2e mirror (F-016/F-017/F-018)

Status: active

## Problem

`scripts/gf-dogfood.sh` (the E2E oracle, `just dogfood`) had three hardening
gaps found by the milestone scrutiny validator:

- **F-016**: the `GDOGFOOD_SRC` preflight tested `git rev-parse
  --is-inside-work-tree` (exit code only), so a **subdirectory** of a git
  worktree passed preflight; cloning it produced a repository without the
  root layout the checks exercise, and the run could fail later with an
  unrelated error instead of a clear preflight denial.
- **F-017**: the release build ran `cargo build --release --target-dir TGT`
  with no lock/offline control; a remote crate-index update or a missing
  network could silently change what was built, and the "deterministic
  disposable-clone oracle" claim was unsupported.
- **F-018**: `constraints.yaml` (the machine-readable mirror of the AGENTS
  Verification Matrix) listed the e2e surface as only `just e2e`, while the
  real E2E oracle is `just dogfood`; the mirror drifted from the source of
  truth.

## Decision

- Preflight now computes the physical source path (`cd -P && pwd`) and the git
  top-level (`git rev-parse --show-toplevel`); `GDOGFOOD_SRC` passes only when
  they are exactly equal (canonical worktree root). A subdirectory is rejected
  with a one-line error naming the offending path and the detected top-level.
- The build now runs `cargo build --release --locked --offline --target-dir
  "$TGT"`: `--locked` pins the build to the committed `Cargo.lock`, `--offline`
  forbids registry/network access so the oracle cannot depend on the network
  or drift with the index, and `--target-dir` keeps the temp build out of the
  checkout (unchanged from f4).
- `constraints.yaml` `verification.surfaces.e2e` now reads
  `just e2e && just dogfood` with evidence spanning both surfaces, matching
  AGENTS.md's Verification Matrix.

## Alternatives considered

- **Resolve the top-level but not require equality**: accepting a subdirectory
  whose top-level differs re-opens F-016; strict equality is the invariant.
- **`--locked` only (no `--offline`)**: still permits a registry/index access;
  the oracle's determinism claim requires both.
- **Leave constraints mirror as-is**: it is a machine-readable contract, so a
  drift is a real misconfiguration, not documentation.

## Consequences

- Preflight rejects: missing path, non-directory, non-git path, and
  subdirectory-of-worktree (all verified: each exits 1 with a one-line error).
- The oracle builds offline and locked; a no-network machine with a warm
  dependency cache can still dogfood (same cache the test suite uses).
- `constraints.yaml` e2e surface is now `just e2e && just dogfood`; every row
  in the mirror resolves to a real justfile target.