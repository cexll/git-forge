#!/usr/bin/env bash
set -euo pipefail

# Devflow worker gate for the t0 slice only. Other features intentionally
# exit cleanly without a receipt; they will be handled by slice-specific
# workers as the mission progresses.
if [[ "${FEATURE_ID:-}" != "t0" ]]; then
  echo "t0-only worker gate: refusing feature ${FEATURE_ID:-<unset>} without a receipt" >&2
  exit 0
fi
exec /Users/chenwenjie/workspaces/git-forge/scripts/devflow-worker.sh
