# git-forge

A local, git-native forge: issues, pull requests, review, and merge inside an
ordinary git repository — git protocol only, zero resident processes, no
GitHub synchronization, no second protocol. Forge state lives as event chains
under `refs/forge/*`, so code and forge state can be stored in the same
repository. Forge-state sync/refspec wiring across clones is L2.

## What it does

- **Issues**: sequential ids via an atomic counter CAS, comments, close/reopen.
- **Pull requests**: immutable snapshots (`source_ref`/`base_ref`/`source_head`/
  `base_head`/`merge_base`), list/show/three-dot diff, whole-PR and
  commit-anchored inline reviews (`--approve`/`--reject`).
- **Merge gate**: `pr merge` refuses until the last reachable review is an
  `approve`; approve → reject transitions are honored.
- **Merge strategies**: `--merge` (default, merge commit), `--squash`
  (single commit), `--rebase` (linear replay onto the base tip).
- All state in git refs (`refs/forge/*`); no daemon, no second protocol.

## Install

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
# A reject would set the effective decision to reject and block merge; the
# last review must be an approve before `pr merge` succeeds.
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
