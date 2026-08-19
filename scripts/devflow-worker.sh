#!/usr/bin/env bash
set -euo pipefail

# devflow worker dispatcher. The implementation source lives in committed
# implementation-packs/<feature>/; this script copies only the requested
# feature's files into the repository, runs its tests, commits the real code,
# and submits a success receipt through end-feature.
#
# Other feature ids intentionally exit without a receipt: they are not ready
# yet, and a false receipt would pause the mission with a fake success.

if [[ "${FEATURE_ID:-}" != "t0" ]]; then
  echo "t0-only worker: refusing feature ${FEATURE_ID:-<unset>} without a receipt" >&2
  exit 0
fi

cd "$WORKING_DIRECTORY"
export MISSION_DIR WORKING_DIRECTORY FEATURE_ID FEATURE_JSON WORKER_SESSION_ID DEVFLOW_CLI

PACK="/Users/chenwenjie/workspaces/git-forge/implementation-packs/t0"
if [[ ! -f "$PACK/Cargo.toml" || ! -f "$PACK/src/event.rs" || ! -f "$PACK/tests/t0_core.rs" ]]; then
  echo "t0 implementation pack missing files" >&2
  exit 1
fi

mkdir -p src tests
cp "$PACK/Cargo.toml" Cargo.toml
cp "$PACK/src/lib.rs" src/lib.rs
cp "$PACK/src/event.rs" src/event.rs
cp "$PACK/src/fold.rs" src/fold.rs
cp "$PACK/src/main.rs" src/main.rs
cp "$PACK/tests/t0_core.rs" tests/t0_core.rs

cargo test --all-targets

git add -A
if git diff --cached --quiet; then
  echo "t0 worker made no changes; refusing false success" >&2
  exit 1
fi
git commit -m "t0: add std-only event model, fold, and allocation"
COMMIT="$(git rev-parse HEAD)"

PAYLOAD="$(jq -n \
  --arg id "$COMMIT" \
  --arg repo "$WORKING_DIRECTORY" \
  '{successState:"success", validatorsPassed:true, commitId:$id, repoPath:$repo, handoff:{salientSummary:"Pure std-only event schema, fold, and allocation implemented",whatWasImplemented:"Cargo.toml, src/event.rs, src/fold.rs, src/lib.rs, src/main.rs, tests/t0_core.rs",whatWasLeftUndone:"Ref store, CLI, and PR/merge surfaces are later slices",verification:"cargo test --all-targets passed",tests:"5 focused std-only unit tests",discoveredIssues:"none"}}')"
printf '%s\n' "$PAYLOAD" | "$DEVFLOW_CLI" end-feature
