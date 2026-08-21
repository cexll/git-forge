# git-forge — dev entry point (L3 strict profile; see constraints.yaml)
set shell := ["bash", "-uc"]

default: check

# ── Setup ────────────────────────────────────────────────────────
setup:
    cargo fetch

# ── Checks (fast gates; exit non-zero on any failure) ───────────
# Blocking aggregate: fmt, clippy, tests, guardrail self-test, decision-record
# structure, and the size gate all fail `just check`. Coverage is intentionally
# EXCLUDED — it is a slow llvm-cov instrumented build unsuitable for pre-commit
# and currently has nothing to measure (empty scaffold); it is a standalone
# review-only gate until the first product commit qualifies it (see
# constraints.yaml downgrades).
check: fmt-check lint test test-guardrails decisions-check size-gate
    echo "fast checks passed"

# Format
fmt-check:
    @if [ -f Cargo.toml ]; then cargo fmt --check; else echo "fmt: no Cargo.toml yet — lands with devflow t0"; fi

fmt:
    cargo fmt

# Lint (L3 block)
lint:
    @if [ -f Cargo.toml ]; then cargo clippy --all-targets -- -D warnings; else echo "lint: no Cargo.toml yet — lands with devflow t0"; fi

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
# $HOME/tokei.toml and $XDG_CONFIG_HOME/tokei/config.toml. A hostile config
# such as `types = ["Python"]` can remove Rust from its language table and make
# an over-limit file invisible. Pointing HOME and XDG_CONFIG_HOME at the fresh
# temporary directory prevents user configuration from loading. The python3
# reader receives the same absolute `$REPO/src` prefix as argv and evaluates
# the per-file `reports[]` against that prefix.
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

# Integration / CLI e2e. Placeholder ONLY: tests/e2e_workflow.rs and
# tests/e2e_counter.rs do NOT exist (the real e2e surfaces are the t2_pr/t3_merge
# integration tests wired into `just test` and the 45-check dogfood via
# `just dogfood`, scripts/gf-dogfood.sh). Kept for compatibility: fails loudly
# if the old files ever exist but fail; otherwise exits 0 with this honest note.
e2e:
    @if [ -f tests/e2e_workflow.rs ] || [ -f tests/e2e_counter.rs ]; then \
        cargo test --test e2e_workflow --test e2e_counter; \
    else \
        echo "e2e: placeholder only — tests/e2e_workflow.rs / e2e_counter.rs do not exist; real e2e surfaces are tests/t2_pr.rs + tests/t3_merge.rs (just test) and scripts/gf-dogfood.sh (just dogfood)"; \
    fi

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

# ── Coverage (L3: 80% lines, block; standalone until qualified) ──
coverage:
    @if [ -f Cargo.toml ]; then cargo llvm-cov --all-targets --fail-under-lines 80; else echo "coverage: no Cargo.toml yet — lands with devflow t0"; fi

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
