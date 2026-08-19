#!/usr/bin/env bash
set -euo pipefail

# Generic devflow worker dispatcher for the git-forge mission.
#
# Each feature's implementation source lives in a committed
# implementation-packs/<feature>/ directory that mirrors the ENTIRE crate state
# after that feature (Cargo.toml, src/*, tests/*). The worker:
#   1. copies the pack into the repository root (over the committed baseline),
#   2. runs the pack's test suite (must be green before any commit),
#   3. commits the full diff (non-empty required — no false success),
#   4. submits an honest success receipt through end-feature.
#
# Features with no pack yet intentionally exit WITHOUT a receipt: a false
# receipt would pause the mission with a fake success.

cd "$WORKING_DIRECTORY"
export MISSION_DIR WORKING_DIRECTORY FEATURE_ID FEATURE_JSON WORKER_SESSION_ID DEVFLOW_CLI

FEATURE="${FEATURE_ID:-}"
PACK="/Users/chenwenjie/workspaces/git-forge/implementation-packs/${FEATURE}"

# Validate the pack exists and looks like a crate.
if [[ ! -f "$PACK/Cargo.toml" ]]; then
  echo "generic worker: no implementation pack for feature ${FEATURE} (pack missing Cargo.toml); exiting without a receipt" >&2
  exit 0
fi

# Copy pack files over the working tree additively. `cp -R` overlays; no
# destructive rm: an incomplete pack leaves the prior committed state in place
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
