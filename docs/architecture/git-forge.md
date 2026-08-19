# git-forge Architecture

## Scope & Goals

A local, git-native forge: issues, pull requests, review, and merge inside an ordinary git repository, using git protocol only, with zero resident processes and no GitHub synchronization. Commands surface as `git forge ...` (plus thin `git issue`/`git pr` wrappers). L1 is single-user, single-machine, single repository (no forge-state sync); multi-clone sync, convergence, and bare-remote enforcement are L2.

Must-haves (L1): issue event chains; PR event chains with review (approve/reject, whole-PR and commit-anchored inline comments); merge with merge-commit/squash/rebase strategies; command-level merge gate; sequential-id allocation via counter CAS. Code push remains native git; forge refs sync (`git forge clone/init` refspec wiring, `git forge push/pull`, remote-tracking, DAG convergence) is L2.

Must-not-haves (L1): CI execution, web UI, single-file export, GitHub bridge, accounts/permissions, labels/milestones/assignees, pre-receive enforcement, forge refs sync (push/pull/refspecs/convergence).

## Constraints

- Git protocol only; no second protocol, no daemon.
- Ordinary repository (not bare) is the L1 shape; bare receiving remote is L2.
- Forge refs are git-managed; may be packed by gc; read via ref APIs only.
- Single mutable ref tip per entity; convergence is L2 (DAG merge, remote-tracking namespace, refspec wiring; `remote.<name>.push` must never be modified).
- Rust + libgit2 (`git2`); history-level operations shell out to the `git` binary.
- MIT license.

## Module Map

| Module | Interface (what callers must know) | Behind the seam | Depth |
|---|---|---|---|
| `core` (event model + folding) | `Event` types, `fold(events) -> State`, `gate(state) -> Result` | Event schema, deterministic ordering, issue/PR state derivation, merge-gate rule | Deep: pure, no I/O, fully testable; deletion would scatter state logic across CLI and store |
| `store` (git refs) | `append(entity, event) -> EventId`, `read_chain(entity) -> Vec<Event>`, `list(kind) -> Vec<EntityRef>`, `allocate_id() -> u64` (versioned counter CAS) | Ref naming under `refs/forge/*`, commit creation, packed-ref-safe reads, counter commit chain | Deep: hides all git plumbing; callers never touch refs directly |
| `sync` (application, L2) | `forge_push(remote)`, `forge_pull(remote)`, `ensure_refspecs(remote)` | Explicit forge refspec push, additive fetch refspec into remote-tracking namespace, per-entity DAG merge convergence (single merge commit, UUID-stable events), refspec validation/repair | Deep: convergence logic lives here, one place; not built in L1 |
| `git` (adapter) | `merge(base, head, strategy)`, `push(remote, refspec)`, `fetch(remote)`, `worktree_add(pr)` | git2 + `git` binary shell-outs | Deep: protocol correctness delegated to git itself |
| `cli` (entry) | `git forge <subcommand>` parsing and dispatch | clap wiring, thin `git-issue`/`git-pr` wrappers | Shallow by design: no logic, only dispatch |
| `hooks` (installer) | `install(repo)` | Writes pre-receive/post-receive hooks (L2 enforcement), validates refspec config | Shallow; L2 grows it |

## Dependency Rules

```
cli → store → core
cli → git
core: zero external dependencies (no git, no I/O)
```

- `core` never imports git2 or performs I/O; tests run with no infrastructure.
- `store` depends on `core` types only.
- `cli` is the only module that parses user input.
- `sync` (L2) depends on `store`, `core`, and `git`; it is not wired in L1.

## Directory Structure

Flat, feature-first (complexity ladder: solo dev / MVP — layers not earned yet):

```
git-forge/
├── Cargo.toml
├── CONTEXT.md
├── docs/
│   ├── adr/0001-…0006-…
│   └── architecture/git-forge.md
├── src/
│   ├── main.rs          # git forge entry
│   ├── cli.rs           # clap commands + wrapper dispatch
│   ├── event.rs         # event model + JSON schema
│   ├── fold.rs          # state derivation (pure)
│   ├── gate.rs          # merge gate rule (pure)
│   ├── store.rs         # refs read/write via git2
│   ├── sync.rs          # forge push/pull, refspec, rebase-retry
│   ├── git.rs           # git binary adapter
│   └── hooks.rs         # hook installer
└── tests/
    ├── e2e_workflow.rs  # issue → PR → review → gate → merge
    ├── e2e_counter.rs   # concurrent issue new → distinct sequential ids (CAS)
    └── (L2) e2e_push_pull.rs, e2e_converge.rs  # refspec sync, DAG convergence
```

## Data Flow

```mermaid
flowchart LR
    U[User] -->|git forge issue new| CLI
    CLI -->|append event| STORE
    STORE -->|commit under refs/forge| GIT[(.git)]
    CLI -->|fold for display| CORE
    CORE -->|state| CLI
    CLI -->|forge push (L2)| SYNC
    SYNC -->|explicit refspec (L2)| GIT
    GIT -->|fetch refspec (L2)| SYNC
    CLI -->|pr merge| GATE
    GATE -->|approved?| CORE
    GATE -->|merge via git binary| GIT
```

## Wire Contract

This contract is normative for tickets and tests. Versions: schema version field is `v: 1` in every event; do not parse unversioned events.

### Ref Layout

```
refs/forge/issues/<n>                    # event chain, n = sequential issue id
refs/forge/prs/<n>/head                  # event chain, n = sequential PR id
refs/forge/prs/<n>/meta                  # convenience pointer to pr.created snapshot commit (not separate data copy)
refs/forge/prs/<n>/source                # immutable snapshot ref → source_head (keeps PR commits reachable)
refs/forge/prs/<n>/base                  # immutable snapshot ref → base_head (keeps merge_base reachable)
refs/forge/prs/<n>/result                # transient pending merge-result ref → result_commit (keeps it reachable across worktree removal; created before cleanup, deleted in final transaction or reported as leftover on failure)
refs/forge/meta/counter                  # versioned counter commit chain (next sequential id)
refs/remotes/<remote>/forge/*            # remote-tracking mirror; never local-authoritative
```

- Entity ID is sequential integer. `refs/forge/meta/counter` is a ref to a versioned counter commit whose tree contains `.forge/counter.json` `{v:1, next: <u64>}`; each allocation appends a new counter commit and updates the ref.
- **Lazy initialization**: the first allocation (`issue new`/`pr create`) atomically creates entity `#1` and a counter commit `{next:2}` in one ref transaction; `git forge init` is optional and only pre-creates the counter. A clean repo needs no setup step before `#1`.
- Allocation is atomic: one `git update-ref --stdin` transaction performs (1) CAS on `refs/forge/meta/counter` (expected old counter commit, new counter commit) and (2) `create` of the entity ref with **expected absence** — Issue: `refs/forge/issues/<n>`; PR: `refs/forge/prs/<n>/head`, `.../meta`, `.../source` (→ `source_head`), and `.../base` (→ `base_head`) all in the same transaction. A concurrent allocator or a pre-existing entity ref fails the transaction (retry with fresh read for counter collision; reject if the entity ref already exists). L1 requires a concurrent-allocation test proving two simultaneous `issue new` calls receive distinct `#n`, a stale-counter/preexisting-entity collision test, and a PR first-allocation collision test.
- PR uses `pr.created` as the **authoritative** base/source snapshot; `refs/forge/prs/<n>/meta` is a convenience pointer only. `pr.created` records `base_ref`, `base_head`, `source_ref`, `source_head`, and `merge_base` (= `git merge-base(base_head, source_head)` at creation).
- **Review content**: the PR patch is `merge_base..source_head` (equivalently `git diff base_head...source_head`). An approval authorizes exactly this patch; it never includes base-only commits or their reversions.
- **Immutable snapshot refs**: `refs/forge/prs/<n>/source` and `/base` are immutable git refs pinning `source_head` and `base_head`; they make the PR's commits reachable even after base/source branches are deleted or rebased, so `git gc` cannot prune them. A branch-delete plus `git gc` PR diff/merge test is required.
- **Stale-base rejection (L1)**: at merge time, if the current `base_ref` tip differs from `base_head`, `git forge pr merge` refuses with a clear error and makes no ref changes; the user recreates the PR (or merges manually). Live-base merge with a fresh merge-result review is L2.
- All per-entity refs have exactly one mutable tip and are git-managed; read via ref APIs.

### Event Commit Layout

Each event is a git commit with a one-file tree:

- Tree blob: `.forge/event.json` containing the versioned event JSON (below).
- Non-event merge commits (L2 convergence only): empty tree, parent = local tip + remote tip, message `forge:merge:<id>`; the fold algorithm ignores commits whose tree has no `.forge/event.json`. L1 never creates them.
- Message for normal events: `forge:<kind>:<id>` (stable subject for tooling).
- Committer/author identity: the invoking `user.email` / `user.name` from git config.

### Event JSON Schema (v1)

```json
{
  "v": 1,
  "id": "<uuid-v4>",
  "kind": "<kind>",
  "entity": "issue",
  "entity_id": 1,
  "ts": "<RFC3339 UTC>",
  "actor": "<user.email>",
  "body": { }
}
```

Kinds and body fields:

| kind | entity | body |
|---|---|---|
| `issue.created` | issue | `{title, description?}` |
| `issue.comment` | issue | `{body}` |
| `issue.close` | issue | `{}` |
| `issue.reopen` | issue | `{}` |
| `pr.created` | pr | `{title, description?, source_ref, base_ref, source_head, base_head, merge_base}` |
| `pr.comment` | pr | `{body}` |
| `pr.review` | pr | `{decision: "approve"|"reject", body?, file?, line?, commit?}` |
| `pr.merge` | pr | `{strategy: "merge"|"squash"|"rebase", result_commit}` |

`pr.review` is anchored to optional `commit` + permission to include `file`/`line` for inline comment. Inline comments are immutable references to a snapshot commit; they never follow later diff changes.

**Effective review decision (L1)**: the last reachable `pr.review` event in the entity chain's parent order (single local chain in L1) is the effective PR decision; the merge gate requires that decision to be `approve`. Repeated decisions by the same or different actors simply advance the effective decision; no per-actor union or stale-approval semantics. DAG linearization for multi-chain convergence is deferred to ADR-0004/L2. `approve → reject` and `reject → approve` transitions are first-class test cases.

### PR Lifecycle (L1: immutable snapshot)

- L1 PRs are **one-shot immutable snapshots**: `pr.created` fixes `source_head` and `base_head`; the PR never follows later branch movement. An approval authorizes exactly that snapshot (`merge_base..source_head`). To propose new commits, create a new PR. `pr.update` events are L2.
- `git forge pr create --source <branch> --base <branch> <title>` — L1 requires `--source`, `--base`, and a non-empty title positional argument; missing `--source`, missing `--base`, or empty/whitespace-only title errors (no heuristic defaults; no self-PR). `--source` and `--base` must resolve to canonical local `refs/heads/*` branches: tags, remote-tracking refs, OIDs, and revision expressions are rejected (merge later CASes `refs/heads/<base>` and the checked-out-base guard operates on local branches). Explicit `--source` equal to `--base` (or distinct local branches resolving to the same commit) is rejected — no self-PR. Deferred to L2: derive default base from `refs/remotes/<remote>/HEAD` when present.
- `pr.created` records `source_ref`, `base_ref`, `source_head`, `base_head`, and `merge_base` (= `git merge-base(base_head, source_head)` at creation). **L1 requires exactly one merge base, checked with `git merge-base --all base_head source_head`**: count != 1 rejects with a clear error and creates no PR. Zero bases covers unrelated histories and shallow repositories (incomplete history); for shallow repos the error must ask the user to deepen/unshallow. Multiple bases covers criss-cross histories. Review diff and rebase are undefined without a single base.
- Immutable snapshot refs `refs/forge/prs/<n>/source` (→ `source_head`) and `/base` (→ `base_head`) are created atomically with `/head` and `/meta`, keeping PR commits reachable through `git gc` and providing the merge inputs after branch deletion/rebasing.
- **Review content**: the PR patch is `merge_base..source_head` (equivalently `git diff base_head...source_head`). An approval authorizes this exact patch; it never includes base-only commits or their reversions.
- **Stale-base rejection (L1)**: at merge time, if the current `base_ref` tip differs from `base_head`, `git forge pr merge` refuses with a clear error and makes no ref changes; the user recreates the PR (or merges manually). Live-base merge with fresh merge-result review is L2.
- Merge semantics are defined against the snapshot: `source_oid` is resolved from the immutable `refs/forge/prs/<n>/source` (snapshot `source_head`), not the live source branch; `base_oid` is resolved from `refs/forge/prs/<n>/base` (snapshot `base_head`). `merge` = `git merge --no-ff --no-edit <source-oid>` (noninteractive merge commit of `source_head` into `base_ref` tip; normal hooks run); `squash` = `git merge --squash <source-oid>` then **`git commit -m "<PR title>"`** (noninteractive via `-m`; the title is the required non-empty `pr.created` field; identity from `user.name`/`user.email`; normal `pre-commit`/`commit-msg` hooks run — never `-n`); `rebase` = `git rebase --onto refs/forge/prs/<n>/base <merge_base>` in a worktree detached at `/source`, taking resulting HEAD as `result_commit`. All executed via the `git` binary.
- **Failure/conflict rule (L1)**: any failed merge leaves the disposable worktree cleaned: **default merge** (conflict, or a failing `pre-merge-commit`/`prepare-commit-msg`/`commit-msg` hook — MERGE_HEAD remains; `pre-commit` may also run when an enabled `pre-merge-commit` hook invokes it, e.g. the sample script, so a failing `pre-commit` follows the same abort path) runs `git merge --abort`; **rebase** (conflict or hook failure) runs `git rebase --abort` **only when rebase state exists**: resolve both `git rev-parse --git-path rebase-merge` and `git rev-parse --git-path rebase-apply`, then test whether either directory exists (in a linked worktree `.git` is a gitfile, so literal `.git/rebase-*` paths would be wrong; the worktree-local state lives under the resolved git dir). A failing `pre-rebase` hook occurs before that state exists, so abort is skipped and the hook error is preserved; **squash** has no `MERGE_HEAD`, so its conflicts or commit/hook failures reset to the `/base` OID (`git reset --hard <base-oid>`). All failure paths then run `git clean -fd` in the disposable worktree (removes untracked merge-driver/hook artifacts) before forced worktree removal (`git worktree remove --force`). In all cases: append no `pr.merge` event, update neither `refs/heads/<base>` nor the PR chain ref, and exit non-zero. The user updates the source branch and creates a new PR (snapshot is immutable).
- **Merge execution (L1)**: `git forge pr merge` performs the merge in a **temporary worktree**: for merge/squash, detached at the immutable `/base` OID (`git worktree add --detach <tmp> refs/forge/prs/<n>/base`); for **rebase**, detached at the immutable `/source` OID, run `git rebase --onto refs/forge/prs/<n>/base <merge_base>`, and take its resulting HEAD as `result_commit`. The user's current branch/worktree is untouched and the merge targets the correct snapshot. Temporary worktree is removed after success or failure (including conflict abort); use `git worktree remove --force` only for cleanup of the disposable worktree, never a user worktree. Checked-out-base enforcement is separate: if `base_ref` is checked out in any non-temporary worktree, merge refuses before creating or updating anything (see below).
- **Merge completion keeps `result_commit` reachable until the final transaction**: `pr.merge.body.result_commit` is only JSON, not a Git graph edge, so a GC running after the worktree is removed could prune the result commit before base CAS. Before removing the disposable worktree, create the **pending-result ref** `refs/forge/prs/<n>/result` → `result_commit` (single `git update-ref` create). Then remove and verify the disposable worktree (success path: `git worktree remove --force`, no `git clean -fd`; verify via `git worktree list --porcelain` that the path is no longer registered and the directory is gone). If removal/verification fails, report the leftover path, delete the pending-result ref, create no `pr.merge` event, update neither ref, and exit non-zero. After successful cleanup, re-read the PR chain tip and re-fold the chain; if the effective decision is no longer `approve` (a concurrent `reject`), delete the pending-result ref, create no `pr.merge`, and abort with no ref changes. Otherwise create the `pr.merge` event commit (sole parent = newest PR chain tip, `result_commit` in body). The final `git update-ref --stdin` transaction atomically performs **three** updates: delete `refs/forge/prs/<n>/result` (expected its OID), CAS `refs/heads/<base>` (expected `base_head` → `result_commit`), and CAS the PR chain ref (expected the same newest PR tip → `pr.merge` event commit). On any failure (concurrent PR append/reject, base moved, stale pending ref), the whole transaction rolls back — no "code merged but PR still open" state. After a transaction failure, git-forge **best-effort deletes the expected pending-result ref**; if that cleanup fails or the process dies before it, the ref remains and is reported in the error as a durable leftover path (`refs/forge/prs/<n>/result`), keeping `result_commit` reachable and safely GC-able by deleting the ref.
- **Test-only pending-window barrier**: to prove reachability honestly, L1 exposes `GIT_FORGE_TEST_MERGE_BARRIER=<dir>` (accepted only in test/debug builds, ignored in release; not part of the user CLI contract). Protocol: after the pending ref exists and the worktree is removed/verified, but before creating `pr.merge` or the final transaction, git-forge (1) atomically creates `<dir>/ready` via `O_CREAT|O_EXCL`, then polls for `<dir>/release` to appear (created atomically by the test with `O_CREAT|O_EXCL`) until a bounded deadline (default 30 s). The **test leaves `release` in place until git-forge deletes it**, so the poll cannot miss the signal. (2) On deadline, fail the merge: delete `<dir>/ready` best-effort (git-forge's own sentinel), delete the pending ref best-effort, keep base/PR refs unchanged, and exit non-zero with a clear error. On observing `release`, git-forge deletes both `<dir>/release` (observed signal, owned by it at that point) and `<dir>/ready` (its own sentinel) best-effort, then proceeds to the final transaction. Each file is deleted only by the side that may safely do so; the test never removes `release` after creating it. The test waits for `<dir>/ready` to appear, runs `git gc --prune=now`, verifies `git cat-file -e <result_commit>` still succeeds (pending ref is the only reachability edge) and base/PR refs are unchanged, then atomically creates `<dir>/release` and waits for git-forge to remove it; git-forge resumes and the final transaction deletes the pending ref. Failure paths still use their abort/reset plus `git clean -fd` before forced removal, as defined above.
- **Checked-out base guard (L1)**: `git forge pr merge` refuses with a clear error and no ref changes if `base_ref` is checked out in any existing non-temporary worktree (including the main worktree or another linked worktree). Advancing a checked-out branch without updating its working tree would leave files at the old tree. Safe post-merge worktree refresh is L2; L1 tells the user to run the merge from/against an un-checked-out base or move off it.

### Event Identity

- Every event carries an immutable UUID `id` (v4) generated at creation, independent of its commit OID. On CAS retry, the event UUID is retained even though the commit OID and parent change.
- State folding and refs identify events by UUID, not by commit hash; UUIDs never change.

### Deterministic Single-Chain CAS Append (L1)

- **L1 is a single chain, not a DAG**: every normal append creates an event commit whose sole parent is the current entity-ref tip; CAS updates the ref (`update-ref <entity-ref> <new-oid> <observed-tip-oid>`). On CAS failure, recreate the event with the new tip as sole parent while **retaining the same UUID**, then retry with a bounded retry count (default 3). If the bounded retries are exhausted, report an error and update no refs.
- Fold order is exactly newest-parent order: events oldest → tip (following sole parents; merge nodes do not exist in L1).
- During fold for issue/PR state, the last reachable `pr.review` is effective. No topological/DAG language is used in L1; multi-parent merge nodes and DAG linearization are L2 (ADR-0004).
- Non-event merge commits (L2 convergence) would be empty-tree and ignored by fold.

- Writes: CLI → store → git refs (append-only commits).
- Reads: CLI → store → fold (core) → display.
- Merge: gate checks folded PR state, then merges in a temporary worktree detached at the immutable `/base` OID via the `git` binary (merge-commit default; `--squash`/`--rebase` flags), pins `result_commit` under `refs/forge/prs/<n>/result`, cleans the worktree, then atomically deletes that pending ref and CAS-updates `refs/heads/<base>` and the PR chain ref.
- Sync (L2): `git forge push` sends `refs/forge/*:refs/forge/*` explicitly; `git fetch`/`git pull` receive forge refs into the remote-tracking namespace via the additive fetch refspec installed by `git forge clone/init`; `git forge pull` then converges local `refs/forge/*` per entity.

## Decisions

Recorded in `docs/adr/`:

- 0001: Git ref event chains as authoritative storage; SQLite index is a rebuildable cache only.
- 0002: Git extension surfaces (`git forge`, wrappers, hooks), not a git fork.
- 0003: Command-level merge gate in L1; direct `git merge` bypass documented; bare-remote pre-receive enforcement deferred to L2.
- 0004: Forge refs sync via remote-tracking namespace and explicit convergence — L2 (not in L1 scope).
- 0005: Rust + libgit2, MIT.
- 0006: L1 PRs are immutable snapshots; `pr.update` and approval invalidation are L2.

## Open Questions

- L2: DAG-merge convergence details (remote-tracking namespace, refspec wiring, merge-node fold) — ADR-0004.
- L2: bare-remote branch protection semantics (which refs protected, who approves).
- L2: `pr.update` semantics and approval invalidation.
- L2: single-file export format (`git forge export project.forge`) — SQLite snapshot vs bundle.
- L2: CI plan format (shell script in repo vs config file) and status event schema.
- L2: web UI surface (read-only browse vs write paths).
