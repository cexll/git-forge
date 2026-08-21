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

# 4. F-015 regression: size-gate must stay immune to a hostile user/root tokei
# config. Run from an OWNED temp project (never the real repo): a synthetic
# over-800-line src file plus hostile tokei.toml (cwd, $HOME, XDG) and a
# .tokeignore must make `just size-gate` exit non-zero with the exact
# 'exceeds 800 code lines (801)' diagnostic, and a clean project must exit
# zero. tokei/just are required prereqs (AGENTS) — their absence is a FAIL,
# not a fail-closed pass.
guard_tmp="$(mktemp -d)"
trap 'rm -rf "$guard_tmp" "$tmp"' EXIT
if ! command -v tokei >/dev/null 2>&1; then
  echo "FAIL  gate: tokei missing (required prereq for size-gate)"; fail=$((fail+1))
elif ! command -v just >/dev/null 2>&1; then
  echo "FAIL  gate: just missing (required to run size-gate)"; fail=$((fail+1))
else
  mkdir -p "$guard_tmp/src"
  probe="$guard_tmp/src/_size_probe.rs"
  for i in $(seq 1 801); do echo "x"; done > "$probe"
  # Hostile root config (current-directory lookup channel, F-015) and a hostile
  # HOME bracket.
  printf 'types = ["Python"]\n' > "$guard_tmp/tokei.toml"
  hostile_home="$guard_tmp/home"
  mkdir -p "$hostile_home"
  # Hostile HOME/XDG channels too: configs in $HOME/tokei.toml and
  # $HOME/tokei/config.toml (both tokei lookup locations) must not hide the
  # over-limit file, so the F-015 isolation cannot regress on that axis.
  mkdir -p "$hostile_home/tokei"
  printf 'types = ["Python"]\n' > "$hostile_home/tokei.toml"
  printf 'types = ["Python"]\n' > "$hostile_home/tokei/config.toml"
  # Hostile IGNORE channel: a project .tokeignore excluding the probe lets a
  # gate that lost --no-ignore silently skip it; the guardrail catches that.
  printf 'src/_size_probe.rs\n' > "$guard_tmp/.tokeignore"
  # Run the repo's OWN size-gate recipe against the temp project's cwd (so a
  # repo-root tokei.toml there is the gate's current-directory config channel)
  # with a hostile HOME/XDG bracket: the gate must still count the 801-line src
  # file and reject it with the exact diagnostic (F-015 isolation regression).
  gate_out="$(HOME="$hostile_home" XDG_CONFIG_HOME="$hostile_home" just --justfile "$repo_root/justfile" --working-directory "$guard_tmp" size-gate 2>&1)" && gate_rc=0 || gate_rc=$?
  if [ "$gate_rc" -eq 0 ]; then
    echo "FAIL  gate: size-gate passed with an 801-line src file + hostile tokei config"; fail=$((fail+1))
  elif ! echo "$gate_out" | grep -q "exceeds 800 code lines (801)"; then
    echo "FAIL  gate: size-gate rejected without the expected 801 diagnostic: $(echo "$gate_out" | head -1)"; fail=$((fail+1))
  else
    echo "PASS  gate: size-gate rejects 801-line src under hostile config"; pass=$((pass+1))
  fi
  rm -f "$probe" "$guard_tmp/.tokeignore"
  if HOME="$hostile_home" XDG_CONFIG_HOME="$hostile_home" just --justfile "$repo_root/justfile" --working-directory "$guard_tmp" size-gate >/dev/null 2>&1; then
    echo "PASS  gate: size-gate accepts clean temp project"; pass=$((pass+1))
  else
    echo "FAIL  gate: size-gate rejected a clean temp project"; fail=$((fail+1))
  fi
fi

echo "guardrail self-test: $pass passed, $fail failed"
[ "$fail" -eq 0 ]
