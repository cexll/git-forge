#!/usr/bin/env bash
set -euo pipefail

# Devflow worker gate for the t0 slice only. Other features intentionally
# exit cleanly without a receipt; they will be handled by slice-specific
# workers as the mission progresses.
#
# The generic worker it execs now requires the spawn command to pass PACK_DIR
# (slice source root). The L1 implementation-packs/ directory was removed
# after the mission completed, so this gate is historical: it only serves a
# completed mission and any future spawn must set PACK_DIR explicitly.
if [[ "${FEATURE_ID:-}" != "t0" ]]; then
  echo "t0-only worker gate: refusing feature ${FEATURE_ID:-<unset>} without a receipt" >&2
  exit 0
fi
# The t0 gate predates the PACK_DIR contract and historically delegated to a
# fixed implementation-packs path. After L1 removed that directory, a future
# t0 re-run must supply PACK_DIR explicitly; preserving the old empty-arg
# behavior would make the worker fail loudly on a configuration error, which is
# the correct outcome for a spawn that forgot its slice source.
exec /Users/chenwenjie/workspaces/git-forge/scripts/devflow-worker.sh
