# git-forge — dev entry point (L3 strict profile; see constraints.yaml)
set shell := ["bash", "-uc"]

default: check

# ── Setup ────────────────────────────────────────────────────────
setup:
    cargo fetch

# ── Checks (fast gates; exit non-zero on any failure) ───────────
# Blocking aggregate: fmt, clippy, tests, guardrail self-test, decision-record
# structure all fail `just check`. Coverage is intentionally EXCLUDED — it is a
# slow llvm-cov instrumented build unsuitable for pre-commit and currently has
# nothing to measure (empty scaffold); it is a standalone review-only gate until
# the first product commit qualifies it (see constraints.yaml downgrades).
check: fmt-check lint test test-guardrails decisions-check
    echo "fast checks passed"

# Format
fmt-check:
    @if [ -f Cargo.toml ]; then cargo fmt --check; else echo "fmt: no Cargo.toml yet — lands with devflow t0"; fi

fmt:
    cargo fmt

# Lint (L3 block)
lint:
    @if [ -f Cargo.toml ]; then cargo clippy --all-targets -- -D warnings; else echo "lint: no Cargo.toml yet — lands with devflow t0"; fi

# ── Tests ────────────────────────────────────────────────────────
test:
    @if [ -f Cargo.toml ]; then cargo test --all-targets; else echo "test: no Cargo.toml yet — lands with devflow t0"; fi

# Unit tests only (fast)
unit:
    @if [ -f Cargo.toml ]; then cargo test --lib; else echo "unit: no Cargo.toml yet — lands with devflow t0"; fi

# Integration / CLI e2e. Fails LOUDLY if the test files exist but the suite
# fails; reports "not yet" only when the files are genuinely absent.
e2e:
    @if [ -f tests/e2e_workflow.rs ] || [ -f tests/e2e_counter.rs ]; then \
        cargo test --test e2e_workflow --test e2e_counter; \
    else \
        echo "e2e: no e2e test files yet (land with t1b/t2/t3)"; \
    fi

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
