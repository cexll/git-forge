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

- **Issues**: sequential ids via an atomic counter CAS, comments, close/reopen, parent/spec links and blocking edges. `issue apply` validates and atomically publishes declarative Markdown plans, matching stable repository-wide keys for idempotent re-apply. `pr create --closes <n>` links a PR to an Issue, and a successful merge closes it in the same ref transaction as the branch advance.
- **Pull requests**: immutable snapshots (`source_ref`/`base_ref`/`source_head`/
  `base_head`/`merge_base`), list/show/three-dot diff, whole-PR and
  commit-anchored inline reviews (`--approve`/`--reject`).
- **Merge gate**: `pr merge` refuses until the last reachable review is an
  `approve` AND the latest CI Check is green; approve → reject transitions are
  honored. `pr abandon --reason <text>` makes a PR terminal: merge, review and CI
  all refuse it, naming the reason; no reopen (a new attempt is a new PR).
- **CI**: `git forge ci run <pr>` executes the repo CI plan against the PR's
  immutable snapshot and records the outcome as a `ci.check` Forge Event. A
  regular `.forge/ci.sh` runs; the `just check` fallback runs only when
  `.forge/ci.sh` is absent; a present symlink or other non-regular entry is
  refused up front without appending a Check result.
- **Merge strategies**: `--merge` (default, merge commit), `--squash`
  (single commit), `--rebase` (linear replay onto the base tip), and `--ff`
  (fast-forward the base to the source; requires the source to sit directly
  on top of the snapshot base). After a successful merge,
  `--recycle-worktree <path>` removes the merged source worktree (and
  deletes its branch when safe); `--force` recycles a dirty source worktree.
- **PR generations (t1)**: `pr update <n>` re-pins an open PR to the source
  branch tip as a NEW immutable snapshot generation — follow-up commits join
  the same PR instead of recreating it. Approvals and CI Checks bind the
  generation they were taken against; after an update, fresh CI + a fresh
  approve are required (older-generation gate inputs are stale and refused).
- **Live-base merge (t2)**: a moved base no longer refuses outright —
  `pr prepare <n> --strategy <merge|squash|rebase|ff>` materializes the
  merge result against the CURRENT base tip and pins it as an immutable
  candidate; `ci run` and `pr review --approve` then validate and authorize
  exactly that candidate, and `pr merge` finalizes it atomically.
- **Machine interface (t7)**: `--json` on `issue new/apply/list/show`,
  `pr create/list/show`, and `ci run` emits stable machine-readable results
  (allocated ids included); `git forge version` prints a semver and exits 0
  — automation version-gates on it instead of parsing prose.
- **Actor roles (t8)**: `GIT_FORGE_ROLE` (worker | reviewer | orchestrator)
  attributes events to a role; the merge gate refuses a self-approval when
  both sides carry roles. Per-worktree identity (`extensions.worktreeConfig`)
  and `GIT_CONFIG_COUNT` overrides are promised interfaces.
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
git forge issue new "implement slice" --parent 1 --blocked-by 1 --json
git forge issue apply plans/ --dry-run --json
git forge issue apply plans/ --json
git forge issue list --parent 1 --state open --json
git forge issue show 1 --json
git forge issue comment 1 "noted"
git forge issue close 1 && git forge issue reopen 1

# pull requests (both branches must be canonical local refs/heads/*)
git checkout -b feature
git commit ...
git forge pr create --source feature --base main "add feature"
git checkout feature          # a clean checked-out base merges fine and is
                              # refreshed; only a DIRTY base worktree refuses
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
# or: git forge pr merge 1 --ff          # fast-forward (source sits on base)
# or: git forge pr merge 1 --recycle-worktree ../feature-wt   # remove the
#    merged source worktree afterwards; --force recycles a dirty one
```

The merge runs in a disposable temporary worktree (so a dirty main worktree
survives). A clean worktree with the base branch checked out also merges —
the merge refreshes it to the result commit — while a stale base, a dirty
base worktree, or a source not sitting directly on the snapshot base (for
`--ff`) refuses, and it finalizes base + PR chain atomically in one ref
transaction.

## Declarative issue plans

Use one parent Issue for the spec and child Issues for vertical-slice tickets.
Each plan file declares a stable repository-wide `key`, a required non-empty
`title`, and the desired metadata. Keys refer to other entries in the same apply;
quoted `"#<n>"` references target existing Issues.

```markdown
---
key: project-schema
title: Store tickets
parent: project-spec
blocked_by: [project-api, "#12"]
labels: [ready-for-agent]
state: open
---
Implement the schema slice.
```

`project-spec` and `project-api` must also be entries in that apply, and Issue
`#12` must already exist. A directory reads only directly contained regular `.md`
files, sorted by filename, ignoring symlinks within directories. Frontmatter is
a strict small format, not general YAML: inline arrays of scalar strings,
optional single/double quotes, and comments at unquoted whitespace followed by
`#`. Double quoted strings support JSON escapes; single quoted strings cannot
contain an internal single quote. Quote scalars containing brackets, commas or
quotes, and always quote existing `"#n"` references. Unknown/duplicate fields and
malformed values fail with file/field diagnostics.
The Markdown after the closing delimiter is the exact description, untrimmed.

Omitted `labels`, `parent`, and `blocked_by` clear them. Omitted `state` preserves
an existing Issue's state; a new Issue starts open. Explicit `open` or `closed`
enforces that state. Re-apply computes create/update/unchanged per key; a no-op
writes no events and consumes no ids. `--dry-run` returns the same diff without
publishing it. The apply validates the complete resulting graph and publishes
all changes in one ref transaction, checking every read graph tip and the counter
with CAS. Duplicate keys, unknown references, self-blocking and graph cycles
reject the whole apply with nothing published.
Apply JSON is a plain array of `{key, id, action, changed_fields}` objects;
prospective ids follow filename order and dry-run ids are tentative.

Issue JSON exposes `key`, `parent`, `blocked_by`, and computed `blocked`; show
also exposes derived `children`. An open ticket with `blocked: false` is ready
for execution. Close tickets with `pr create --closes <n>` on successful merge,
or keep using the individual new/comment/close/reopen commands. Full format,
event and transaction rules: [wire contract](docs/architecture/git-forge.md).

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
