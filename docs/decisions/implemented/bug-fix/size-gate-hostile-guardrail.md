# Committed size-gate hostile-config guardrail (F-020)

Status: active

## Problem

The F-015 fix (size-gate config/ignore isolation) was verified by an ad-hoc
hostile-config demonstration whose fixtures were removed; no committed
regression test protected the isolation. A future regression — removing the
empty-dir execution, the `--no-ignore` flag, or HOME/XDG isolation — would be
invisible on a clean machine (the gate would stay exit 0) and F-015 could
silently return as a bypass.

## Decision

`scripts/test-guardrails.sh` gains a fourth check (still wired into `just check`
via `test-guardrails`): run the repo's own `size-gate` recipe
(`just --justfile <repo>/justfile --working-directory <temp>`) against an
OWNED temp project holding a synthetic 801-line Rust file plus hostile config
on every relevant tokei channel: the project's own `tokei.toml` (current
directory); `$XDG_CONFIG_HOME/tokei.toml` (default
`$HOME/.config/tokei.toml`) and `$HOME/tokei.toml` (user configuration); and
a project `.tokeignore` excluding the probe (ignore-rule channel; a gate that
lost `--no-ignore` would silently skip it). The fixture writes the project
`tokei.toml`, then sets `HOME` and `XDG_CONFIG_HOME` to the same temporary
bracket, so its one `$HOME/tokei.toml` fixture exercises both user-config
locations. The gate must exit non-zero WITH the exact diagnostic
`exceeds 800 code lines (801)`; after removing the probe and `.tokeignore`,
the gate must exit zero on the same clean project. tokei and
just are required prereqs — their absence is a FAIL, not a false pass. Fixtures
live in `mktemp -d` dirs created and removed by the script — the real repo tree
is never touched.

## Alternatives considered

- **Fixture in the real repo**: an 801-line `src/_size_probe.rs` and a
  committed `tokei.toml` are user-visible and would pollute the tree / trip
  other guards; an owned temp project is hermetic and self-cleaning.
- **Receiver without `--working-directory`**: running the gate from the repo
  root measures the repo itself, not the hostile temp project; the cwd config
  channel must be exercised from the temp project's directory.
- **`exceeds`-only without exact count**: asserting only non-zero would pass
  when tokei is missing or the recipe errors; requiring the `(801)` diagnostic
  proves the counting path actually ran against the over-limit file.

## Consequences

- `test-guardrails.sh` now 5 checks (3 naming + 2 gate), all passing;
  `just check` (which runs test-guardrails) exits 0.
- `AGENTS.md` Verification Matrix and `constraints.yaml` guardrails evidence
  updated to `5/5` (they previously stated 3/3).
- A missing tokei in the gate's prereq now fails the guardrail (it is a
  required prereq per AGENTS); a missing `just` likewise.
- A regression in the size-gate isolation (empty-dir execution, `--no-ignore`,
  HOME/XDG override) or counting fails the guardrail loudly on a clean machine
  instead of silently passing.