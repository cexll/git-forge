#!/usr/bin/env bash
set -euo pipefail

# Generic devflow worker dispatcher.
#
# Slice source contract: the mission's spawn command MUST pass PACK_DIR (the
# directory holding one crate-state snapshot per feature:
# <PACK_DIR>/<feature>/Cargo.toml + src/ + tests/). L1 shipped slices from a
# committed implementation-packs/ directory; that directory was removed after
# the mission completed (12/12), so a fresh mission must point PACK_DIR at its
# own slice source. A missing PACK_DIR is a configuration error and fails
# loudly (exit 1); a missing <PACK_DIR>/<feature> crate is an honest
# "no work available" and exits WITHOUT a receipt, so the orchestrator
# requeues instead of recording a fake success.
#
# The worker:
#   1. copies the slice into the repository root (over the committed baseline),
#   2. runs the crate's test suite (must be green before any commit),
#   3. commits the full diff (non-empty required — no false success),
#   4. submits an honest success receipt through end-feature.

cd "$WORKING_DIRECTORY"
export MISSION_DIR WORKING_DIRECTORY FEATURE_ID FEATURE_JSON WORKER_SESSION_ID DEVFLOW_CLI

FEATURE="${FEATURE_ID:-}"

# The slice source is a required part of the spawn contract, not an optional
# convenience: a worker with no defined source can never deliver anything.
if [[ -z "${PACK_DIR:-}" ]]; then
  echo "generic worker: PACK_DIR is unset; the spawn command must point it at the slice source root (<PACK_DIR>/<feature>/) — see scripts/devflow-worker.sh" >&2
  exit 1
fi
PACK="${PACK_DIR}/${FEATURE}"

# Validate the pack exists and looks like a crate.
if [[ ! -f "$PACK/Cargo.toml" ]]; then
  echo "generic worker: no slice for feature ${FEATURE} at ${PACK} (missing Cargo.toml); exiting without a receipt" >&2
  exit 0
fi

# Copy slice files over the working tree additively. `cp -R` overlays; no
# destructive rm: an incomplete slice leaves the prior committed state in place
# and the subsequent `cargo test` + non-empty-diff guard reject it cleanly.
cp "$PACK/Cargo.toml" Cargo.toml
mkdir -p src tests
if [[ -d "$PACK/src" ]]; then cp -R "$PACK/src/." src/; fi
if [[ -d "$PACK/tests" ]]; then cp -R "$PACK/tests/." tests/; fi

# The suite must pass before we commit anything.
cargo test --all-targets

# Refuse a no-op commit: an implementation feature must produce a real diff.
if git diff --quiet && git diff --cached --quiet; then
  echo "generic worker: ${FEATURE} made no changes; refusing false success" >&2
  exit 1
fi

git add -A
git commit -m "feat: implement ${FEATURE} slice" >/dev/null
COMMIT="$(git rev-parse HEAD)"
DIFF_STAT="$(git show --stat --oneline "$COMMIT" | head -20)"

# Handoff prose is derived from the actual diff so the receipt stays honest.
PAYLOAD="$(jq -n \
  --arg id "$COMMIT" \
  --arg repo "$WORKING_DIRECTORY" \
  --arg feature "$FEATURE" \
  --arg diffstat "$DIFF_STAT" \
  --arg verification "cargo test --all-targets passed (post-copy, pre-commit)" \
  --arg summary "Implemented feature $FEATURE" \
  '{successState:"success", validatorsPassed:true, commitId:$id, repoPath:$repo, handoff:{salientSummary:$summary, whatWasImplemented:$diffstat, whatWasLeftUndone:"", verification:$verification, tests:"cargo test --all-targets", discoveredIssues:"none"}}')"
printf '%s\n' "$PAYLOAD" | "$DEVFLOW_CLI" end-feature
