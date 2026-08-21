#!/usr/bin/env bash
# gf-dogfood-main-default.sh — OWNED regression for F-033 (f3): the dogfood
# flow must resolve the clone's default branch instead of hardcoding `master`.
#
# Builds a throwaway MAIN-default source repo (`git init -b main`, one base
# commit holding PLAN.md), exports GDOGFOOD_SRC at it, then runs the FULL real
# dogfood flow (scripts/gf-dogfood.sh — the same script `just dogfood` runs)
# and asserts the DOGFOOD SUMMARY reports pass=45 fail=0.
#
# RED against a gf-dogfood.sh that still hardcodes master (the run aborts at
# the first `git checkout -B ... master`); GREEN after the default-branch fix.
# Self-contained: the temp source is trap-cleaned on exit; nothing escapes.
set -eu

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
GF_DOGFOOD="$SCRIPT_DIR/gf-dogfood.sh"

T="$(mktemp -d)"
trap 'rm -rf "$T"' EXIT
SRC="$T/main-default"

# Main-default source repo: default branch `main`, a base commit, PLAN.md.
git init -q -b main "$SRC"
cd "$SRC"
git config user.name Dogfood
git config user.email dogfood@x
printf '# main-default dogfood source\n\nPLAN.md for the F-033 default-branch regression.\n' > PLAN.md
git add PLAN.md
git commit -qm "chore(dogfood): base commit on main"

# Run the REAL dogfood flow against this main-default source.
export GDOGFOOD_SRC="$SRC"
set +e
OUT="$(bash "$GF_DOGFOOD" 2>&1)"
RC=$?
set -e
printf '%s\n' "$OUT"
if [ "$RC" -ne 0 ]; then
  echo "gf-dogfood-main-default: dogfood exited $RC (RED — gf-dogfood.sh cannot handle a main-default source)" >&2
  exit 1
fi
if ! printf '%s\n' "$OUT" | grep -Eq 'DOGFOOD SUMMARY pass=45 fail=0'; then
  echo "gf-dogfood-main-default: dogfood exited 0 but the summary is not pass=45 fail=0:" >&2
  printf '%s\n' "$OUT" | grep 'DOGFOOD SUMMARY' >&2 || true
  exit 1
fi
echo "gf-dogfood-main-default: PASS — main-default dogfood 45/45"
