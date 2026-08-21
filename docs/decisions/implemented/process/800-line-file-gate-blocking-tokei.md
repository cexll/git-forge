# 800-Line File Gate Becomes a Blocking src-Only Tokei Check

Status: active

## Problem

The repo's L3 profile (`constraints.yaml` `strictness_profile`) recorded
`size_limits.max_file_lines` (800) as review-only, with an explicit removal
trigger: "install a CI line counter (tokei, cloc) and wire into just check".
The f1 git-adapter extraction left `src/cli.rs` at 785 tokei code lines — under
the ceiling but close enough that the limit had real teeth with no machine
backstop. The audit on 2026-08-21 verified the trigger condition: the downgrade
row was still present and accurate ("no automated line counter installed"), but
no gate enforced the 800-code-line ceiling. The limit was therefore a stated
profile-blocked control that only PR review could catch.

## Decision

- Install tokei (14.0.0, Homebrew) as a required local prerequisite, documented
  explicitly as `brew install tokei`. `just setup` stays `cargo fetch` and does
  NOT auto-install it.
- Add `just size-gate`: pipes `tokei --output json src/` into a python3 reader
  over the per-file reports; any src/ file over 800 code lines makes the gate
  exit non-zero. A missing tokei exits 1 with "tokei missing — run brew install
  tokei" — fail loud, never silent pass.
- Wire `size-gate` into the `just check` blocking aggregate
  (`fmt-check lint test test-guardrails decisions-check size-gate`).
- Promote `size_limits.max_file_lines` from review-only to block in
  constraints.yaml, `enforced_by: "just size-gate (tokei)"`, and remove the
  now-fulfilled downgrade row. Add the gate to the verification matrix
  (`level: block`, `required_at: L3`).
- Scope is src/ ONLY at this time: tests/ stay outside the gate (documented).
  tests/t3_merge.rs is 856 tokei code lines and is tracked as an open concern,
  not gated.

## Alternatives considered

- **Auto-install tokei in `just setup` or a fresh-clone bootstrap**: rejected —
  mutates a user machine and makes setup OS/package-manager-specific. The
  prerequisite is documented instead (`brew install tokei`) and the gate fails
  loud when tokei is absent.
- **Gate over src/ + tests/ immediately**: rejected — tests/t3_merge.rs already
  exceeds 800 code lines (856, pushed over the ceiling by f6/f2), so the
  aggregate would be red on the current tree. Widening the gate is a follow-up
  that must first split the oversized test file; it is kept out of this change.
- **A std-only line counter script**: rejected — tokei is the tool named in the
  recorded downgrade's removal trigger; a bespoke counter would add a second
  source of truth for line counts.

## Consequences

- The 800-code-line ceiling is now machine-enforced (block) for src/ files;
  review-only complexity (15) and per-function (150-line) limits are unchanged.
- Local prerequisites grow by Homebrew tokei (14.0.0); `just check` fails with a
  clear message until it is installed, so a fresh clone fails loudly rather than
  silently passing the gate.
- tests/ line size is intentionally un-gated pending a future split that first
  reduces tests/t3_merge.rs; this is recorded here, not silently accepted.
- The fulfilled downgrade row disappears from constraints.yaml; the gate is
  visible in the verification matrix with a real machine check behind it.