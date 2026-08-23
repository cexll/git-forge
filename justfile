# git-forge — dev entry point (L3 strict profile; see constraints.yaml)
set shell := ["bash", "-uc"]

default: check

# ── Setup ────────────────────────────────────────────────────────
setup:
    cargo fetch

# ── Checks (fast gates; exit non-zero on any failure) ───────────
# Blocking aggregate: fmt, clippy, tests, guardrail self-test, decision-record
# structure, and the size gate all fail `just check`. Coverage is intentionally
# EXCLUDED — it is a slow llvm-cov instrumented build (~50s) unsuitable for
# pre-commit/just-check, and there is no CI runner to host it as a blocking
# gate; it stays a standalone review-only check for single-user local dev (see
# constraints.yaml downgrades).
check: fmt-check lint deps-check dupes-check test test-guardrails decisions-check size-gate harness-check docs-check
    echo "fast checks passed"

# Format
fmt-check:
    @if [ -f Cargo.toml ]; then cargo fmt --check; else echo "fmt: no Cargo.toml yet — lands with devflow t0"; fi

fmt:
    cargo fmt

# Lint (L3 block)
lint:
    @if [ -f Cargo.toml ]; then cargo clippy --all-targets -- -D warnings -D clippy::cognitive_complexity -D clippy::too_many_lines; else echo "lint: no Cargo.toml yet — lands with devflow t0"; fi

# Unused-dependency gate (class B, block): cargo-machete exits non-zero on an
# unused [dependencies] entry. Previously the unused_dependencies criterion was
# review-only.
deps-check:
    @if [ -f Cargo.toml ]; then cargo machete; else echo "deps-check: no Cargo.toml yet — lands with devflow t0"; fi

# Duplicate-code gate (class B, block): cargo-dupes exits non-zero when src/
# exact or near-duplicate lines exceed duplicate_code_threshold_percent (3) at
# the default min-unit size. Scoped to src/ (production) and excludes #[test]
# fns / #[cfg(test)] mods; integration-test helper duplication is out of scope
# (documented). Previously the duplicate_code criterion was review-only.
dupes-check:
    @if [ -f Cargo.toml ]; then command -v cargo-dupes >/dev/null 2>&1 || { echo "cargo-dupes missing — run cargo install cargo-dupes" >&2; exit 1; }; cargo dupes check -p src --exclude-tests --max-exact-percent 3 --max-near-percent 3; else echo "dupes-check: no Cargo.toml yet — lands with devflow t0"; fi

# Rendered eng-init harness validation (agents_md_validation): proves that
# AGENTS.md Verification Matrix commands, Enforcement Index paths, and the
# constraints.yaml verification mirror all still resolve. Owns the
# check_rendered_harness.py gate.
harness-check:
    @python3 scripts/check_rendered_harness.py . --require-section "Verification Matrix" --require-section "Code Canonicality" --require-enforcement-index --require-generated-section-registry --forbid-root-backups

# Documentation gate (documentation_gates): fails when the wire contract loses
# its Known Limitations section or a doc cross-reference points at a missing
# file. Content drift stays review-only; structural drift blocks.
docs-check:
    @python3 scripts/check_docs.py

# ── Size gate (L3: block) ───────────────────────────────────────
# Per-file cap: every src/ file must stay at or under 800 code lines
# (constraints.yaml size_limits.max_file_lines). The recipe captures the
# repository root, creates a fresh temporary directory, and runs from that
# empty cwd (`cd "$T"`). Its actual tokei invocation is
# `HOME="$T" XDG_CONFIG_HOME="$T" tokei --no-ignore --output json "$REPO/src/"`:
# it counts the source tree through an absolute path, keeps the output
# machine-readable, and disables .gitignore/.ignore/.tokeignore suppression
# so an ignore rule cannot hide a source file from the gate. `set -o pipefail`
# propagates either tokei or python3 failures, and the EXIT trap cleans up the
# temporary directory.
# tokei is a required local prerequisite; `just setup` remains `cargo fetch`
# and does not install it. A missing binary fails loudly with
# `tokei missing — run brew install tokei`.
#
# CONFIG ISOLATION (F-015): tokei loads user configuration from
# $XDG_CONFIG_HOME/tokei.toml (or $HOME/.config/tokei.toml when XDG is unset),
# $HOME/tokei.toml, and the current directory (`./tokei.toml`). A hostile
# config such as `types = ["Python"]` can remove Rust from its language table
# and make an over-limit file invisible. The fresh temporary cwd (`cd "$T"`)
# plus HOME/XDG_CONFIG_HOME redirection to that directory neutralizes all three
# sources. The python3 reader receives the same absolute `$REPO/src` prefix as
# argv and evaluates the per-file `reports[]` against that prefix.
size-gate:
    @set -o pipefail; \
    if ! command -v tokei >/dev/null 2>&1; then \
        echo "tokei missing — run brew install tokei" >&2; \
        exit 1; \
    fi; \
    REPO=$(pwd); \
    T=$(mktemp -d) || { echo "size-gate: mktemp failed" >&2; exit 1; }; \
    trap 'rm -rf "$T"' EXIT; \
    cd "$T" && HOME="$T" XDG_CONFIG_HOME="$T" tokei --no-ignore --output json "$REPO/src/" | python3 -c 'import json,sys; data=json.load(sys.stdin); prefix=sys.argv[1]; bad=[(r["name"],r["stats"]["code"]) for v in data.values() for r in v.get("reports",[]) if r["name"].startswith(prefix + "/") and r["stats"]["code"]>800]; [print("size-gate: %s exceeds 800 code lines (%d)"%(n,c), file=sys.stderr) for n,c in sorted(bad)]; sys.exit(1 if bad else 0)' "$REPO/src"

# ── Tests ────────────────────────────────────────────────────────
test:
    @if [ -f Cargo.toml ]; then cargo test --all-targets; else echo "test: no Cargo.toml yet — lands with devflow t0"; fi

# Unit tests only (fast)
unit:
    @if [ -f Cargo.toml ]; then cargo test --lib; else echo "unit: no Cargo.toml yet — lands with devflow t0"; fi

# Real enforced e2e gate (F-008): `just e2e` runs `dogfood-all` — BOTH 45/45
# dogfood oracles on disposable clones: the master-default oracle
# (scripts/gf-dogfood.sh) and the owned main-default regression
# (scripts/gf-dogfood-main-default.sh). Supersedes the previous stub target
# that only echoed a compatibility note. ~3min; intentionally NOT part of
# `just check` (fast path).
e2e: dogfood-all

# Full 45-check dogfood e2e (scripts/gf-dogfood.sh). Self-contained:
# builds/refreshes the release binary from THIS checkout (never a stale one)
# into a controlled temp --target-dir (immune to CARGO_TARGET_DIR / cargo
# target-dir config; no git-visible target/ left in the checkout), preflights
# GDOGFOOD_SRC (env, default dsh-deepwork: existing dir, git repo, PLAN.md
# present), clones it into a mktemp -d disposable area, runs the 45 checks from
# a non-base checkout, and exits 0 iff the dogfood summary reports pass=45
# fail=0. Temp dirs are trap-cleaned on exit.
dogfood:
    bash scripts/gf-dogfood.sh

# Owned F-033 regression: the same 45-check dogfood flow against a MAIN-default
# source (scripts/gf-dogfood-main-default.sh builds a throwaway `git init -b
# main` repo with PLAN.md, exports GDOGFOOD_SRC at it, and runs the FULL real
# scripts/gf-dogfood.sh, asserting the summary reports pass=45 fail=0). RED
# against a gf-dogfood.sh that hardcodes master; GREEN after the fix.
dogfood-main-default:
    bash scripts/gf-dogfood-main-default.sh

# SEC-01 regression: a source repo whose default branch is `x$(>$MARKER)`
# (git update-ref accepts no-space payloads) must be used as data by
# gf-dogfood.sh — the eval re-parse must never execute the embedded `$()`.
# Asserts the vulnerable at() assertion was reached AND no marker file was
# created. ~30s, part of the enforced e2e gate.
dogfood-eval-regression:
    bash tests/dogfood-eval-injection-regression.sh

# Enforced e2e dogfood surface: both default-branch variants (main-default via
# scripts/gf-dogfood-main-default.sh + master-default via scripts/gf-dogfood.sh),
# plus the SEC-01 eval-injection regression (tests/dogfood-eval-injection-regression.sh)
# — a source repo whose default branch is `x$(>$MARKER)` must be used as data,
# never eval'd. ~3.5min, intentionally NOT in just check. Both self-contained
# targets (main-default + eval-regression, no external prereqs) run BEFORE the
# GDOGFOOD_SRC-dependent master-default dogfood, so the owned regressions still
# execute when the source is absent — a missing source fails only its own leg
# and never masks the self-contained ones.
dogfood-all: dogfood-main-default dogfood-eval-regression dogfood

# ── Coverage (L3: 94% lines, review-only/standalone; not in `just check`) ──
# cargo-llvm-cov needs llvm-profdata, which Homebrew's rust install does not
# ship in its sysroot (`lib/rustlib/*/bin/` has only rust-objcopy). rustup's
# matching-version toolchain does carry llvm-tools, so when the active rustc's
# sysroot lacks llvm-profdata we run cargo-llvm-cov under the rustup toolchain
# that ships it. Detect via the sysroot directly (command substitution), not a
# shell variable, so `just` hands `$(...)` to the shell unmangled.
coverage:
    @if [ -f Cargo.toml ]; then \
        if find "$(rustc --print sysroot)/lib/rustlib" -name llvm-profdata 2>/dev/null | grep -q .; then \
            cargo llvm-cov --all-targets --fail-under-lines 94; \
        elif rustup run 1.93.0-aarch64-apple-darwin true 2>/dev/null; then \
            echo "coverage: Homebrew rust lacks llvm-profdata; using rustup toolchain 1.93.0-aarch64-apple-darwin"; \
            rustup run 1.93.0-aarch64-apple-darwin cargo llvm-cov --all-targets --fail-under-lines 94; \
        else \
            echo "coverage: no llvm-profdata in this rustc sysroot and no rustup 1.93.0 toolchain; install one (rustup toolchain install 1.93.0-aarch64-apple-darwin && rustup component add llvm-tools-aarch64-apple-darwin)"; \
            exit 1; \
        fi; \
    else \
        echo "coverage: no Cargo.toml yet — lands with devflow t0"; \
    fi

# ── Build ────────────────────────────────────────────────────────
build:
    @if [ -f Cargo.toml ]; then cargo build --all-targets; else echo "build: no Cargo.toml yet — lands with devflow t0"; fi

# ── Runtime verification ─────────────────────────────────────────
# CLI surface: run the built binary against known input (lands with t1b).
verify-cli:
    @if [ -f target/debug/git-forge ]; then ./target/debug/git-forge --help; else echo "verify-cli: binary not built yet (t1b)"; fi

# ── Guardrails ───────────────────────────────────────────────────
test-guardrails:
    bash scripts/test-guardrails.sh

# ── Decision records ─────────────────────────────────────────────
# Four-zone lifecycle (proposed/implemented/rejected/archived). See docs/decisions/README.md.
decisions-check:
    python3 scripts/check-decisions.py docs/decisions

# ── Devflow mission helpers ──────────────────────────────────────
devflow:
    @node /Users/chenwenjie/.agents/skills/implement/scripts/devflow.cjs $(ARGS)
