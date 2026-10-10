#!/bin/bash
set -e
# Forge CI plan (git forge ci run executes this in the PR's immutable snapshot).
# forge isolates global/system git config plus control env; the repo's own
# tests must therefore not depend on a user-level git identity being present.
# Rustup toolchain, not Homebrew cargo (SIGABRT on LLVM 23.1).
export PATH="$HOME/.cargo/bin:$PATH"
export RUSTUP_TOOLCHAIN=1.93.0-aarch64-apple-darwin
cargo test

# Adapter fixtures use /usr/bin/env node, so children need Node on PATH too.
export PATH="$PATH:/opt/homebrew/bin:/opt/homebrew/opt/node/bin"
if ! command -v node >/dev/null 2>&1; then
  echo "forge CI: node missing — install Node.js or add it to PATH; adapter tests are required" >&2
  exit 1
fi
node --test .specs/forge-p1/adapter/forge-adapter.test.cjs