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
OWNED temp project whose `src/` holds a synthetic 801-line Rust file and whose
root holds a hostile `tokei.toml` (`types = ["Python"]`), under a hostile
HOME/XDG bracket. The gate must exit non-zero WITH the exact diagnostic
`exceeds 800 code lines (801)` (a missing tokei or a broken recipe is a FAIL,
not a false pass); after removing the probe, the gate must exit zero on the
same clean project. Fixtures live in `mktemp -d` dirs created and removed by
the script — the real repo tree is never touched.

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
- A missing tokei now fails the guardrail (it is a required prereq per AGENTS)
  rather than silently passing as "rejected".
- A regression in the size-gate isolation or counting fails the guardrail
  loudly on a clean machine instead of silently passing.