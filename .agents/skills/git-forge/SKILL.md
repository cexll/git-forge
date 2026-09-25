---
name: git-forge
description: File and track issues, and create/review/merge pull requests with git-forge — a local, git-native forge whose issues, PRs, reviews, CI checks, and merges live as event chains under refs/forge/* inside an ordinary git repository (no server, no GitHub). Use whenever working in a repo that has git-forge installed and the task involves `git forge`, `git issue`, `git pr`, filing an issue, opening/reviewing/merging a PR, running the CI gate, or inspecting forge state — instead of reaching for GitHub/GitLab tooling or hand-editing refs.
---

# git-forge: local issues & PRs

git-forge puts a forge inside the repo itself. Every issue/PR is an **event chain** of commits under `refs/forge/*`; state is derived by folding the chain. There is no daemon and nothing to sync in L1.

**Hard rules for agents:**

- Never read or write `refs/forge/*` directly, and never hand-craft event JSON — use the CLI. The store owns counter CAS, event schema v1, and ref transactions; bypassing it corrupts forge state.
- Forge state is **not** synced across clones in L1. `git push`/`git pull` do not carry it (L2 scope).
- The author recorded on every event is `git config user.email` — check it is set meaningfully before creating/reviewing. Per-worktree identity (`extensions.worktreeConfig` + `git config --worktree user.email`) and the per-invocation `GIT_CONFIG_COUNT`/`GIT_CONFIG_KEY_0`/`GIT_CONFIG_VALUE_0` env override are **promised interfaces**: identity is whatever `git config` resolves for the invoking worktree.
- Events MAY carry a **role** (`worker` | `reviewer` | `orchestrator`) from `GIT_FORGE_ROLE` — the pipeline seat that ran the command. Unset/empty means no role (single-user default, wire unchanged); an unknown value fails closed. The merge gate refuses a **self-approval**: when roles are configured and the effective approve comes from the SAME role that created the PR, merge exits non-zero — re-review under a different role is the remedy. Without roles on both events the gate is unchanged.

## Setup check

The binary dispatches through git's `git-<cmd>` PATH lookup:

- `git forge ...` works when `git-forge` is on PATH (`cargo install --path .` in the git-forge repo).
- `git issue ...` / `git pr ...` work when `git-issue` / `git-pr` symlinks to the same binary are on PATH (thin argv[0] wrappers — identical behavior).

Verify with `git-forge --help` (direct binary, exit 0) or a non-leading form like `git forge issue --help`. Bare `git forge --help` / `git issue --help` / `git pr --help` exit 1 — git itself intercepts a leading `--help` and looks up a man page that does not exist for custom subcommands; use the `help` subcommand instead (`git forge help`, `git issue help`, `git pr help`). All errors print `git-forge: <message>` on stderr with exit code 1; usage errors also exit 1.

## Commands

### Issues — `git forge issue <sub>` (or `git issue <sub>`)

```
new <title> [description] [--label <x>]...   # create; prints `issue #<n> created: <title>` (sequential id)
list                                         # all issues, folded state
show <n>                                     # state (open/closed), title, comments
comment <n> <body>
close <n>  /  reopen <n>
```

### Pull requests — `git forge pr <sub>` (or `git pr <sub>`)

```
create --source <branch> --base <branch> <title> [--body <text>] [--label <x>]...
list
show <n>                                     # snapshot fields + generation + effective review decision + CI status
comment <n> <body>
review <n> --approve|--reject [--file <f> --line <l> --commit <c>]  # honors GIT_FORGE_ROLE for role attribution
diff <n>                                     # three-dot diff merge_base...source_head from generation-current snapshot refs
update <n>                                   # re-pin to the source branch tip as a NEW immutable snapshot generation
prepare <n> --strategy merge|squash|rebase|ff  # materialize the merge result (candidate) against the CURRENT base tip
merge [<n>] [--merge|--squash|--rebase|--ff] [--recycle-worktree <path>] [--force]
abandon <n> --reason <text>                    # terminal: merge/review/CI refuse it afterwards
ci:  git forge ci run <n>                    # run CI plan against the generation snapshot (or prepared candidate), record a ci.check event
```

`--source`/`--base` must be **canonical local branches** (`refs/heads/*`; bare name or full form). Tags, remote-tracking refs, OIDs, and revision expressions are rejected.

## Standard workflow

```sh
git forge issue new "short title" "optional detail"     # 1. file the work -> issue #<n>
git checkout -b feature && git commit ...               # 2. do the work on a branch
# 3. open the PR; ids are shared with issues, so PARSE the id from the output:
out=$(git forge pr create --source feature --base main "add feature")
pr=$(printf '%s' "$out" | sed -n 's/.*PR #\([0-9]*\) created:.*/\1/p')
git forge pr show "$pr" && git forge pr diff "$pr"      # 4. inspect
git forge pr review "$pr" --approve                     # 5. review (approve)
git forge ci run "$pr"                                  # 6. record a green CI Check
git forge pr merge "$pr"                                # 7. merge (or --squash / --rebase / --ff)
git forge issue close <n>                               # 8. close the loop on the issue
```

## Semantics that bite (read before merging)

- **PRs are immutable snapshots per generation.** `pr create` freezes `source_head`, `base_head`, and `merge_base` into `refs/forge/prs/<n>/{source,base,head,meta}` (generation 0). Later commits on the source branch are NOT in the PR — `git forge pr update <n>` re-pins the PR to the branch tip as a NEW immutable generation (`refs/forge/prs/<n>/gen/<k>/...`); a pre-update approve or CI Check is stale and cannot satisfy the merge gate (re-review + fresh `ci run` after update). The snapshot refs also keep commits reachable, so `diff`/`merge` still work after the source branch is deleted or `git gc` runs.
- **Self-PRs are rejected**: same source/base, or distinct branches resolving to the same commit, fail `pr create` with a non-zero exit.
- **Merge gate is generation-bound and two-part**: the *last reachable* review must be `approve` on the PR's CURRENT snapshot generation AND the *latest* `ci.check` must be `success` on that same generation. A later `--reject` flips the effective decision and blocks merge. No CI Check (or a failed/pending/stale one) refuses with a message naming `git forge ci run <n>` as the remedy.
- **CI plan comes from the generation snapshot** (or the prepared candidate, when one is current): `ci run` executes `.forge/ci.sh` when it is a regular file, else the `just check` fallback (which requires a self-contained justfile — no `import`/`mod`, no `set fallback`). A failing plan still records a `failure` Check, then exits non-zero.
- **Moved base goes through `pr prepare`**: a base branch that moved since the snapshot does not refuse outright, but the merge REQUIRES a prepared candidate — `git forge pr prepare <n> --strategy <s>` materializes the result against the current base tip, then a candidate-bound `ci run` + candidate-bound `pr review --approve` re-take the gate before merge. A moved base without a candidate refuses naming `pr prepare`; `pr update` invalidates a candidate (re-prepare remedy).
- **Checked-out base**: a clean worktree holding the base branch merges fine — after the merge its tree is refreshed to the result commit; only a DIRTY base worktree refuses (untracked counts as dirty; no flag unlocks it). `--recycle-worktree <path>` removes the merged source worktree afterwards (`--force` recycles a dirty one).
- **Merge is terminal and atomic**: a merged PR cannot merge again; base + PR-chain finalize in one ref transaction, and a failed hook/conflict cleans up without touching refs (any expected pending-result ref is best-effort deleted; a surviving one is reported as a leftover). Strategies are mutually exclusive.
- **Merge runs in a disposable worktree** — the user's dirty main worktree is safe.

## Common recipes

- Inline review anchored to a line: `git forge pr review 2 --approve --file src/lib.rs --line 42 --commit <hash>` — `--file`/`--line` require `--commit`; the anchor must resolve to a real commit object (deliberately NOT constrained to the PR snapshot — spec permits anchoring to any commit) and the resolved OID is stored, so the anchor never drifts.
- Re-run CI after fixing the plan: fix `.forge/ci.sh` (or the justfile) on the source branch, then `git forge pr update <n>` (new snapshot generation), `ci run`, re-approve, and merge — no recreate needed.
- Inspect why a merge refused: `git forge pr show <n>` shows the effective decision and latest CI status; the error message names the failing gate.

## Reference

Normative wire contract (ref layout, event JSON schema v1, counter CAS, merge semantics): `docs/architecture/git-forge.md`. Glossary: `CONTEXT.md`. Legacy rationale: `docs/adr/`.

## Optional Issue workflow companion

See `docs/issue-workflow-bridge.md` for `git forge-workflow`. It delegates to devflow and does not change native L1 refs or introduce a native `issue sync` subcommand.
