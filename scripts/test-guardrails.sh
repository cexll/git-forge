#!/usr/bin/env bash
# test-guardrails.sh — guardrail self-test (L3: guardrail_self_test).
# Asserts BOTH directions: the naming guard accepts a clean tree and rejects a
# staged violation. A rejection-only self-test is blind to always-failing guards.
set -euo pipefail
repo_root="$(git rev-parse --show-toplevel)"
hook="$repo_root/.git-hooks/check-naming.sh"

pass=0; fail=0

# 1. Clean tree must pass (no staged added files).
"$hook" >/dev/null 2>&1
if [ $? -eq 0 ]; then
  echo "PASS  guard: clean tree accepted"; pass=$((pass+1))
else
  echo "FAIL  guard: clean tree rejected"; fail=$((fail+1))
fi

# 2. A staged violation must be rejected.
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT
violating="$tmp/test_new.rs"
echo "// staged violation" > "$violating"
# Simulate an added file via the path-argument mode (write-time check).
if "$hook" "$violating" >/dev/null 2>&1; then
  echo "FAIL  guard: staged _new suffix accepted"; fail=$((fail+1))
else
  echo "PASS  guard: staged _new suffix rejected"; pass=$((pass+1))
fi

# 3. Clean filename must pass the write-time check.
"$hook" src/event.rs >/dev/null 2>&1
if [ $? -eq 0 ]; then
  echo "PASS  guard: clean path accepted"; pass=$((pass+1))
else
  echo "FAIL  guard: clean path rejected"; fail=$((fail+1))
fi

echo "guardrail self-test: $pass passed, $fail failed"
[ "$fail" -eq 0 ]
