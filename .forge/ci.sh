#!/bin/bash
set -e
# Forge CI plan (git forge ci run executes this in the PR's immutable snapshot).
# forge isolates global/system git config plus control env; the repo's own
# tests must therefore not depend on a user-level git identity being present.
# Rustup toolchain, not Homebrew cargo (SIGABRT on LLVM 23.1).
export PATH="$HOME/.cargo/bin:$PATH"
export RUSTUP_TOOLCHAIN=1.93.0-aarch64-apple-darwin
cargo test