#!/usr/bin/env bash
# P0 regression (code-review SEC-01): the dogfood oracle must treat a
# maliciously-named default branch as DATA, never eval it.
#
# A source repo whose default branch is `x$(>$MARKER)` — created via
# `git update-ref` (accepts no-space `$()` payloads, verified) — flows into
# gf-dogfood.sh as $BASE_BRANCH and then into at() assertions. The old
# inline-interpolated at() strings made eval re-parse the embedded `$()` and
# execute it: `>$MARKER` creates the marker file as a side effect (no stdout
# leakage, deterministic). The fix (escaped \$BASE_BRANCH, branch as data)
# must keep the payload inert: the marker file must never appear.
#
# The script must actually REACH the vulnerable at() assertion with this
# branch — a preflight crash would make a marker-only check pass silently —
# so the test requires the "base contains merged commit" check line (the
# first at() call taking $BASE_BRANCH) to be present in the output.
set -euo pipefail

repo_root="$(cd "$(dirname "$0")/.." && pwd)"
script="$repo_root/scripts/gf-dogfood.sh"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

src="$tmp/src"
marker="$tmp/owned-marker"
mkdir -p "$src"
git -C "$src" init -q -b master
printf 'x\n' > "$src/PLAN.md"
git -C "$src" add PLAN.md
git -C "$src" -c user.email=t@t -c user.name=t commit -qm init
# No-space side-effect payload: `>$MARKER` redirects to the marker path.
# git update-ref and check-ref-format --branch accept it (spaced payloads
# are rejected by git itself, so this is the realistic worst case). The
# payload stays a single-quoted literal — never expanded here — and the
# marker path is exported only for the git command that must not run it.
payload='x$(>$MARKER)'
MARKER="$marker" git -C "$src" update-ref "refs/heads/$payload" HEAD
MARKER="$marker" git -C "$src" symbolic-ref HEAD "refs/heads/$payload"

out="$(MARKER="$marker" GDOGFOOD_SRC="$src" bash "$script" 2>&1)" || true
# 1. The vulnerable call site must have executed with this branch.
if ! printf '%s' "$out" | grep -q "base contains merged commit"; then
  echo "FAIL: dogfood did not reach the \$BASE_BRANCH at() assertion (preflight crash?)"
  printf '%s\n' "$out" | tail -3
  exit 1
fi
# The dogfood run must also have completed (summary printed), proving the
# full 45-check flow ran rather than aborting early.
if ! printf '%s' "$out" | grep -q "DOGFOOD SUMMARY"; then
  echo "FAIL: dogfood run did not complete (no DOGFOOD SUMMARY)"
  printf '%s\n' "$out" | tail -3
  exit 1
fi
# 2. The injected redirection must never have run.
if [ -e "$marker" ]; then
  echo 'FAIL: injected $(>$MARKER) executed (eval re-parse created marker)'
  exit 1
fi
echo "PASS: malicious default branch used as data (no injection)"
