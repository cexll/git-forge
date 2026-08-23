# Duplicate-Code Detection Becomes a Blocking src-Only cargo-dupes Gate

Status: active

## Problem

The repo's L3 profile (`constraints.yaml` `strictness_profile`) recorded
`anti_drift.duplicate_code_threshold_percent` (3) as review-only, with an
explicit removal trigger: "install a Rust duplicate-code detector (e.g.
SonarQube, manual review→tool)". The Code Canonicality rule ("same logic has
exactly one implementation") had no machine backstop: PR review alone could let
a near-duplicate block land in `src/`. The eng-init audit on 2026-08-23 scored
`duplicate_code_detection` 0/1 because no detector was installed and nothing
blocked.

## Decision

- Install `cargo-dupes` (0.2.1, MIT, crates.io) as a required local
  prerequisite, documented explicitly as `cargo install cargo-dupes`. `just
  setup` stays `cargo fetch` and does NOT auto-install it.
- Add `just dupes-check`: runs `cargo dupes check -p src --exclude-tests
  --max-exact-percent 3 --max-near-percent 3`. `-p src` scopes the gate to
  production code; `--exclude-tests` drops `#[test]` fns and `#[cfg(test)]`
  mods; the two `--max-*-percent` flags make the gate exit non-zero when src/
  exact or near-duplicate line percentage exceeds the recorded 3% cap. A
  missing binary exits 1 with "cargo-dupes missing — run cargo install
  cargo-dupes" — fail loud, never silent pass.
- Wire `dupes-check` into the `just check` blocking aggregate and into
  `.git-hooks/pre-commit` (runs the same cargo command).
- Promote `duplicate_code_threshold_percent` from review-only to block in
  constraints.yaml (`enforced_by: "just dupes-check + pre-commit (cargo-dupes
  check -p src --exclude-tests ...)"`), remove the now-fulfilled downgrade row,
  and add the gate to the verification matrix (`level: block`, `required_at:
  L3`). Mirror the row in AGENTS.md Enforcement table and Enforcement Index.
- Scope is src/ ONLY at this time: `tests/` stays outside the gate
  (documented). The measured src/ duplication is 1.4% exact / 0.0% near (123
  units, 2,543 lines), under the 3% cap, so the gate passes with headroom.
  Four small src/ duplicate groups remain and are within tolerance: repeated
  one-line closures (`pr_merge.rs`/`cli.rs`), `StoreError::source`/
  `JsonValue::as_str|as_object` match bodies, and two 3–4 line
  `EventStore`/`BoundEventStore` and `open`/`init` method bodies.

## Alternatives considered

- **Rust-native `jscpd` (crates.io, v5.0.16)**: a high-performance Rust
  reimplementation of the well-known jscpd, and a literal match for the
  registry's `jscpd` validator name. Rejected for this gate: token-shingle
  based over 150+ languages, more false-positive noise on a Rust-only repo,
  and multi-language scope the project does not need.
- **`dupehound` (v0.1.2)**: winnowing-based near-duplicate detector. Rejected:
  v0.1.2 is very early and its default `card` feature pulls a heavy SVG renderer
  dependency.
- **SonarQube `sonar-rust` copy/paste detection**: rejected — requires a
  SonarQube server (external running service), incompatible with the
  single-user, zero-resident-process, local-only project model.
- **PR review only (status quo)**: rejected — review-only gave no machine
  backstop; this is exactly the gap the audit flagged.

## Consequences

- `duplicate_code_detection` moves from review-only to block: a new src/
  duplicate cluster over 3% fails `just check` and the commit.
- Dev-time cost: `cargo-dupes` is a fresh `cargo-install`ed prerequisite; like
  tokei/cargo-machete it must be present for `just check` to pass.
- Threshold is duplicated in the recipe (3) per the established size-gate
  convention; constraints.yaml stays the canonical documented source.
- Known, intentionally out-of-scope: the integration-test helper duplication
  (`tmpdir`, `forge`, `git`, `event`, `candidate_name`, `make_tmpdir` across
  `tests/*.rs`) is not gated. It is a candidate for a separate test-helper
  consolidation refactor, not this gate.
