# git-forge

[![release](https://img.shields.io/github/v/release/cexll/git-forge)](https://github.com/cexll/git-forge/releases)

A local, git-native forge: issues, pull requests, review, and merge inside an
ordinary git repository — git protocol only, zero resident processes, no
GitHub synchronization, no second protocol. Forge state lives as event chains
under `refs/forge/*`, so code and forge state can be stored in the same
repository. Forge-state sync/refspec wiring across clones is L2.

There is no web UI: `refs/forge/*` is not rendered by GitHub's (or any forge
host's) web interface — issues and PRs are read and written through the CLI
(`git forge ...` / `git issue ...` / `git pr ...`), and a host's Issues/Pull
Requests tabs stay unused. A web view is deferred L2 roadmap.

## What it does

- **Issues**: sequential ids via an atomic counter CAS, comments, close/reopen.
- **Pull requests**: immutable snapshots (`source_ref`/`base_ref`/`source_head`/
  `base_head`/`merge_base`), list/show/three-dot diff, whole-PR and
  commit-anchored inline reviews (`--approve`/`--reject`).
- **Merge gate**: `pr merge` refuses until the last reachable review is an
  `approve` AND the latest CI Check is green; approve → reject transitions are
  honored.
- **CI**: `git forge ci run <pr>` executes the repo CI plan against the PR's
  immutable snapshot and records the outcome as a `ci.check` Forge Event. A
  regular `.forge/ci.sh` runs; the `just check` fallback runs only when
  `.forge/ci.sh` is absent; a present symlink or other non-regular entry is
  refused up front without appending a Check result.
- **Merge strategies**: `--merge` (default, merge commit), `--squash`
  (single commit), `--rebase` (linear replay onto the base tip).
- All state in git refs (`refs/forge/*`); no daemon, no second protocol.

## Install

Quick install — downloads the latest prebuilt release, verifies its SHA-256
against the release's `SHA256SUMS.txt`, and installs into `~/.local/bin`:

```sh
curl -fsSL https://raw.githubusercontent.com/cexll/git-forge/master/scripts/install.sh | sh
```

Environment overrides: `VERSION=v0.2.1` pins a release tag,
`INSTALL_DIR=/some/path` changes the install dir (sudo is used automatically
for non-writable dirs). Prebuilt targets: `aarch64-apple-darwin` and
`x86_64-unknown-linux-gnu` (Intel macOS is not prebuilt — GitHub's intel
runners are paid larger-runner labels; build from source instead). Each
`git-forge-<tag>-<target>.tar.gz` contains the `git-forge` binary plus
relative `git-issue`/`git-pr` symlinks, re-created by the installer, so
`git forge ...` / `git issue ...` / `git pr ...` all dispatch. Re-running the
script upgrades in place; uninstall by removing those three files from the
install dir.

Or build from source:

```sh
# Build and install `git-forge` itself so `git forge ...` dispatches.
# Cargo installs into ~/.cargo/bin by default (already on your PATH when
# rustup/cargo is set up).
cargo install --path .

# Optional: thin wrappers in a user-writable PATH dir (git finds git-<cmd>
# on PATH). Choose one already on PATH, e.g.:
mkdir -p "$HOME/.local/bin"
ln -s "$HOME/.cargo/bin/git-forge" "$HOME/.local/bin/git-issue"
ln -s "$HOME/.cargo/bin/git-forge" "$HOME/.local/bin/git-pr"
```

`git forge ...` is the single real namespace; `git issue`/`git pr` are thin
argv[0]-dispatch wrappers over the same binary.

## Usage

```sh
# issues
git forge issue new "track a bug"        # -> issue #1
git forge issue list
git forge issue show 1
git forge issue comment 1 "noted"
git forge issue close 1 && git forge issue reopen 1

# pull requests (both branches must be canonical local refs/heads/*)
git checkout -b feature
git commit ...
git forge pr create --source feature --base main "add feature"
git checkout feature          # L1 refuses to merge while the base is checked out
git forge pr show 1
git forge pr diff 1                      # base_head...source_head
git forge pr review 1 --approve
git forge pr review 1 --approve --file src/lib.rs --line 42 --commit <hash>   # anchored inline
# A reject would set the effective decision to reject and block merge. To
# merge, the last review must be an approve AND the latest CI Check must be
# green — record it by running the CI plan:
git forge ci run 1                       # execute .forge/ci.sh (or `just check`), record a CI Check
# Merge strategies are mutually exclusive — a merged PR is terminal:
git forge pr merge 1                     # merge commit (default)
# or: git forge pr merge 1 --squash      # single commit
# or: git forge pr merge 1 --rebase      # linear replay onto the base tip
```

The merge runs in a disposable temporary worktree (so a dirty main worktree
survives), refuses a stale base or a checked-out base branch, and finalizes
base + PR chain atomically in one ref transaction.

## Design

- **Event chains**: every append is a commit with a single `.forge/event.json`
  file; state is derived by folding the chain (single-chain oldest→tip in L1).
- **Counter**: versioned counter commit chain + one `update-ref` CAS
  transaction; concurrent `issue new` get distinct sequential ids.
- **PRs are immutable snapshots** (ADR-0006): snapshot refs
  `refs/forge/prs/<n>/{source,base,head,meta}` keep commits reachable through
  `git gc`; approval authorizes exactly `merge_base..source_head`.
- Wire contract, event JSON schema v1, and ADRs: `docs/architecture/git-forge.md`,
  `docs/adr/`.

## Development

```sh
just check   # fmt-check + lint + test + guardrails
```

## License

MIT
