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
  `just e2e` (both 45/45 dogfood oracles, master-default + main-default),
  with `test: just test` kept as a separate L3 surface for the t2_pr/t3_merge
  integration tests; this matches AGENTS.md's Verification Matrix.

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
- `constraints.yaml` e2e surface is now `just e2e` (both 45/45 dogfood oracles)
  with `test: just test` separate; every row in the mirror resolves to a real
  justfile target.

## Addendum: default-branch resolution instead of hardcoded master (F-033)

### Problem

`scripts/gf-dogfood.sh` hardcoded `master` at every checkout, tag, PR
`--base`/`--source`, the `event_commit_is_oid` helper, and the squash/rebase
assertions — roughly 20 command sites. A `GDOGFOOD_SRC` whose default branch is
`main` passed preflight (PLAN.md is present, it is a git worktree root) and then
aborted at the first checkout (`git checkout -B feat/dogfood master` → `master`
is not a commit), even though every check is default-branch-agnostic.

### Decision

- Immediately after the disposable clones, resolve the clone's default branch:
  `BASE_BRANCH="$(git -C "$T/dogfood" branch --show-current)"`. `git clone`
  checks out the source's default branch whatever it is named, so this is the
  one honest source of truth.
- The resolution is rejected loudly when empty (detached/unborn HEAD): a
  one-line error naming the source path and the missing default branch, exiting
  before any check runs. No guess, no fallback to a hardcoded name.
- Every `master` reference becomes `$BASE_BRANCH` (`origin/$BASE_BRANCH` for
  the remote-tracking rejection check; `$BASE_BRANCH~0` and
  `$BASE_BRANCH~1..$BASE_BRANCH` for the rev-expr checks).
- An OWNED regression — `scripts/gf-dogfood-main-default.sh` + `just
  dogfood-main-default` — builds a throwaway `git init -b main` source with a
  base commit and PLAN.md, exports `GDOGFOOD_SRC` at it, and runs the FULL real
  dogfood flow, asserting `pass=45 fail=0`. It is RED against the pre-fix
  script (verified) and GREEN after (verified), so the fix can never silently
  regress.

### Consequences

- A main-default source dogfoods 45/45, as does the master-default source
  (`dsh-deepwork`).
- An empty `branch --show-current` resolution fails with a one-line error
  before any work (verified: detached, branchless source exits 1 naming the
  path).
- `grep` for `master` in `scripts/gf-dogfood.sh` returns zero hits.

## Addendum: BASE_BRANCH must stay data, never eval input (SEC-01)

### Problem

F-033's `$BASE_BRANCH` flows into the `at()` assertion helper, which runs its
test command via `eval "$*"`. The three at() calls taking the branch
interpolated it into the eval string (`"$BASE_BRANCH"` inline). Git ref
syntax forbids spaces, control characters, and patterns such as `..`, `@{`,
`~`, `^`, `:`, `?`, `*`, `[`, and backslash — but it permits `$()`,
backticks, `;`, `&`, `|`, `>`, and quotes in no-space combinations: verified
`git update-ref refs/heads/x$(id)` (no-space payload) succeeds. A
`GDOGFOOD_SRC` whose default branch is e.g. `x$(>$MARKER)` therefore made
eval re-parse the embedded `$()` and execute the redirection — command
injection at L3-gate runtime.

### Decision

- The three at() calls escape the interpolation (`\"\$BASE_BRANCH\"`): the
  branch name is expanded at eval time as a shell variable VALUE, which POSIX
  shells do not re-parse. The branch remains data inside the evaluated
  string; nothing in the payload is spliced into the string before parsing.
- Direct argv call sites (`git checkout -B feat "$BASE_BRANCH"`, ck(), the
  `event_commit_is_oid` helper) were already safe — `"$BASE_BRANCH"` expands
  to a single argv element, never parsed as syntax.
- No branch-name whitelist: Git-valid names including `$()`/backticks are a
  supported source contract; rejecting them would change dogfood behavior.
  The invariant is that the value never reaches a parser, not that the value
  is benign.
- An OWNED regression — `tests/dogfood-eval-injection-regression.sh` + `just
  dogfood-eval-regression` — creates a source repo whose default branch is
  `x$(>$MARKER)` via `git update-ref`, points HEAD at it, runs the FULL real
  dogfood flow, requires the vulnerable at() assertion AND the `DOGFOOD
  SUMMARY` to be reached (no preflight-crash false positive), and asserts the
  marker file was never created. RED against the inline-interpolated form
  (verified: `FAIL: injected $(>$MARKER) executed`) and GREEN after (verified).

### Consequences

- A maliciously-named default branch (`x$(>$MARKER)`) runs the full 45-check
  flow with the branch as data; the injected redirection never executes.
- `just e2e` (the enforced L3 e2e gate) now runs the SEC-01 regression as a
  self-contained target ordered BEFORE the GDOGFOOD_SRC-dependent dogfood, so
  a missing source fails only its own leg and never masks the regression.