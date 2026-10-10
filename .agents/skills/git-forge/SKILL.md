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
- Set `GIT_FORGE_ROLE=worker` when the worker creates a PR or updates its snapshot. Set `GIT_FORGE_ROLE=reviewer` when the reviewer approves it. `orchestrator` is also supported for prepare, CI, and merge. An unset/empty role keeps the single-user default; an unknown role fails before a write. The merge gate rejects an approval from the SAME role that created the PR when both events have roles. A different role must approve. Roles record the pipeline seat, not an authenticated identity.

## Setup check

The binary dispatches through git's `git-<cmd>` PATH lookup:

- `git forge ...` works when `git-forge` is on PATH (`cargo install --path .` in the git-forge repo).
- `git issue ...` / `git pr ...` work when `git-issue` / `git-pr` symlinks to the same binary are on PATH (thin argv[0] wrappers — identical behavior).

Use `git-forge --help` (exit 0) or `git forge help` for setup. Use `git forge version` for the installed version, not `--version`.
This skill targets the current git-forge checkout, whose manifest reports `0.3.1`. Earlier binaries can report the same version without the JSON errors, mutation results, idempotency keys, or diagnostics below. Check capabilities with command help and disposable-repository probes; version alone is not sufficient. Do not test writes in the user's active repository.
Use `git forge issue --help`, `git forge pr --help`, or `git forge ci --help` for command help.
Bare `git forge --help` / `git issue --help` / `git pr --help` exit 1: git intercepts leading `--help` and opens a missing man page.
Use `git issue help` or `git pr help` instead. CLI errors use `git-forge: <message>` and exit 1.
Help is incomplete: some supported `--json` flags and recycle options are omitted from the aggregate help. Use `git forge pr merge --help` for recycle options. The contracts below follow the current CLI and observable results, not help alone.

## Commands

### Issues — `git forge issue <sub>` (or `git issue <sub>`)

```
new <title> [description] [--label <x>]... [--parent <n>] [--blocked-by <n>]... [--idempotency-key <k>] [--json]
apply <dir|file> [--dry-run] [--json]
list [--parent <n>] [--state open|closed|all] [--json]
show <n> [--json]                             # state, title, dependencies, comments
comment <n> <body> [--idempotency-key <k>] [--json]
close <n> [--json]  /  reopen <n> [--json]
```

`new` prints `issue #<n> created: <title>` by default. Issues and PRs share sequential IDs. `--parent` sets the parent issue; each `--blocked-by` adds a dependency. `list --parent` selects child issues. `issue list/show --json` includes `open`, `key`, `parent`, `blocked_by`, and computed `blocked`. Show also includes derived direct `children`. Compute `blocked` from the complete graph, not the filtered list. Closing all blockers clears `blocked`; reopening a blocker sets it again.

### Declarative issue plans

Use one parent Issue for the spec and child Issues for vertical-slice tickets. A ticket's `blocked_by` edges identify the Issues to close before execution. Author the whole graph, inspect `apply --dry-run --json`, then apply it once. Create each implementation PR with `--closes <ticket-id>` so its successful merge closes the ticket atomically.

`apply` reads one Markdown file or the directly contained regular `.md` files in a directory, sorted by filename. It ignores directory-entry symlinks. Frontmatter is a strict small format, not general YAML. Require stable repository-wide `key` and non-empty `title`. Use inline arrays of scalar strings for `labels` and `blocked_by`; `[]` is valid. Nested mappings, block lists, unknown fields, duplicate fields, and malformed frontmatter are rejected before any write. The body after the closing delimiter is the exact description, including whitespace and trailing newlines.

Scalars can use single or double quotes. Double-quoted strings support JSON escapes. Single-quoted strings cannot contain an internal single quote. Quote keys, references, titles, or labels containing brackets, commas, or quote characters. Blank lines and whole-line comments are ignored. In a field value, an unquoted `#` starts a comment at the beginning or after whitespace; a quoted `#` is literal.

`parent` and `blocked_by` can name keys in the same batch or existing Issues as quoted `"#<n>"` references. Omitted `labels`, `parent`, and `blocked_by` clear those values. Omitted `state` preserves an existing Issue's state; a new Issue starts open. Explicit `state: open` or `state: closed` enforces that state. Validate the whole resulting graph, including existing Issues outside the plan. Invalid references, duplicate plan keys or ambiguous stored keys, self-parenting, self-blocking, parent cycles, and blocking cycles reject the entire batch. Parent and blocking graphs are validated independently.

One atomic ref transaction verifies every read Issue tip and the counter, then publishes all changes or none. Unchanged Issue tips are verified too. `--dry-run` reports the diff without writing; prospective IDs are tentative. `--json` returns a plain array of `{key, id, action, changed_fields}` in filename order. Reapply the same keys to update Issues. A no-op returns `action: unchanged`, writes no events, and consumes no IDs. Read dependencies from `issue list/show`, not apply output. Select open tickets with `blocked: false` for execution. Read the bundled wire contract for the complete parser, event, and transaction rules.

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

Include `project-spec` and `project-api` in the same apply; Issue `#12` must already exist.

```sh
git forge issue apply /path/to/spec-batch --dry-run --json
git forge issue apply /path/to/spec-batch --json
git forge issue list --parent 1 --state all --json
git forge issue show 1 --json
git forge issue new "Child" "Child detail" --parent 1 --blocked-by 2 --json
```

### Pull requests — `git forge pr <sub>` (or `git pr <sub>`)

```
create --source <branch> --base <branch> <title> [--body <text>] [--label <x>]... [--closes <n>] [--idempotency-key <k>] [--json]
list [--json]
show <n> [--json]                             # snapshot, generation, review, CI, prepared candidate
comment <n> <body> [--idempotency-key <k>] [--json]
review <n> --approve|--reject [--file <f> --line <l> --commit <c>] [--json]
diff <n> [--json]                             # current snapshot's three-dot diff
update <n> [--json]                           # new immutable snapshot generation
prepare <n> --strategy merge|squash|rebase|ff [--json]
merge <n> [--merge|--squash|--rebase|--ff] [--recycle-worktree <path>] [--force] [--json]
abandon <n> --reason <text> [--json]            # terminal; merge/review/CI refuse; no reopen
ci: git forge ci run <n> [--json]              # current snapshot or prepared candidate
```

`--source`/`--base` must be **canonical local branches** (`refs/heads/*`; bare name or full form). Tags, remote-tracking refs, OIDs, and revision expressions are rejected.

`--closes <n>` links an Issue to the PR. A successful merge closes that Issue in the same transaction as the branch advance and PR completion. `--merge` is the default; `--squash`, `--rebase`, and `--ff` select other strategies. Always supply the PR ID; the CLI requires it despite the optional ID in help. `--force` requires `--recycle-worktree` and never bypasses the merge gate or dirty-base guard.

## Standard workflow

Use `--json` for automation. This shell example uses `jq` to extract allocated IDs; a program can use its own JSON parser.

```sh
set -eu
issue_out=$(git forge issue new "short title" "optional detail" --json)
issue=$(printf '%s' "$issue_out" | jq -er '.id')
# Work on a distinct feature branch. Commit before PR creation.
out=$(GIT_FORGE_ROLE=worker git forge pr create --source feature --base main "add feature" --closes "$issue" --json)
pr=$(printf '%s' "$out" | jq -er '.id')
git forge pr show "$pr" --json
git forge pr diff "$pr" --json
GIT_FORGE_ROLE=orchestrator git forge pr prepare "$pr" --strategy merge --json
GIT_FORGE_ROLE=orchestrator git forge ci run "$pr" --json
# The reviewer must inspect the exact prepared candidate before approval.
GIT_FORGE_ROLE=reviewer git forge pr review "$pr" --approve --json
GIT_FORGE_ROLE=orchestrator git forge pr merge "$pr" --merge --json
git forge issue show "$issue" --json             # open: false
```

Read the candidate commit from the prepare result's `candidate`, or from `pr show --json` at `prepared.result_commit`. Review its changes against `prepared.base_head` with `git diff <base_head> <result_commit>`. The snapshot-only `pr diff` does not show changes introduced by integration with the current base.

## Semantics that bite (read before merging)

- **PRs are immutable snapshots per generation.** `pr create` freezes `source_head`, `base_head`, and `merge_base` into `refs/forge/prs/<n>/{source,base,head,meta}` (generation 0). Later commits on the source branch are NOT in the PR — `git forge pr update <n>` re-pins the PR to the branch tip as a NEW immutable generation (`refs/forge/prs/<n>/gen/<k>/...`); a pre-update approve or CI Check is stale and cannot satisfy the merge gate (re-review + fresh `ci run` after update). The snapshot refs also keep commits reachable, so `diff`/`merge` still work after the source branch is deleted or `git gc` runs.
- **Update requires the original base tip.** `pr update` returns `E_BASE_MOVED` without publishing a generation if the base moved. A base already deleted at the check returns `E_NOT_FOUND`; deletion after the check returns `E_BASE_MOVED` with `data.current_base: null`. To merge the existing source snapshot, prepare against the current base, run CI, obtain candidate-bound approval, and merge. To include new source commits after base movement, create a replacement PR. Preserve `--closes`, record both PR IDs on the linked Issue, and abandon the old PR with `git forge pr abandon <old-id> --reason "Superseded by PR #<new-id>"`. Prepare, run CI, and obtain approval on the replacement. Do not merge manually to bypass the gate.
- **Self-PRs are rejected**: same source/base, or distinct branches resolving to the same commit, fail `pr create` with a non-zero exit.
- **Merge gate is generation-bound and two-part**: the *last reachable* review must be `approve` on the PR's CURRENT snapshot generation AND the *latest* `ci.check` must be `success` on that same generation. A later `--reject` flips the effective decision and blocks merge. No CI Check (or a failed/pending/stale one) refuses with a message naming `git forge ci run <n>` as the remedy.
- **CI uses the snapshot's plan.** A regular `.forge/ci.sh` runs from the current source snapshot or prepared candidate. Only an absent `.forge/ci.sh` permits the self-contained `just check` fallback: no `import`/`mod` or `set fallback`. A present symlink or other non-regular entry is refused, not replaced by fallback. A failed run records status `failed` and exits 1. Success requires verified worktree cleanup. `GIT_FORGE_CI_TIMEOUT` sets seconds: default 300, clamped to 1–86400.
- **Prepare binds the merge candidate.** Prepare works with an unchanged base and is required after base movement. It does not increment the generation. CI and approval must bind the exact candidate and strategy; snapshot-only gate inputs cannot satisfy the candidate gate. A changed candidate requires fresh CI and approval. `pr update` invalidates the candidate, so prepare again. Merge must use the prepared strategy and current base tip; its result is exactly the candidate commit.
- **Checked-out base and recycling.** A clean base worktree is refreshed to the merged result; a dirty base refuses, including untracked files. No flag bypasses this guard. Recycle requires a registered source-branch worktree; `--force` permits a dirty source recycle only. Remove a source worktree only with user authorization when it contains user-owned changes. Squash/rebase keep an existing source branch. Merge/ff delete it only when its live tip equals the snapshot source head and is an ancestor of the result. A post-merge recycle failure exits 1 even though the PR is already merged; inspect `pr show --json` before any retry. Recycle never touches the base worktree.
- **Merge is terminal and atomic**: a merged PR cannot merge again; base + PR-chain finalize in one ref transaction, and a failed hook/conflict cleans up without touching refs (any expected pending-result ref is best-effort deleted; a surviving one is reported as a leftover). Strategies are mutually exclusive.
- **Worktree isolation is conditional.** Prepare, CI, and merge use disposable worktrees. Unrelated dirty worktrees remain untouched, but a dirty checked-out base blocks merge. Sparse-checkout and submodules in a base worktree are outside the clean/refresh contract.

## Common recipes

- Inline review anchored to a line: `GIT_FORGE_ROLE=reviewer git forge pr review 2 --approve --file feature.txt --line 1 --commit <hash>` — `--file`/`--line` require `--commit`. The anchor must resolve to a real commit object. It need not be a PR snapshot commit. The CLI stores the resolved OID, so the anchor does not drift.
- Re-run CI after fixing the plan: fix `.forge/ci.sh` (or the justfile) on the source branch. With an unchanged base, run `GIT_FORGE_ROLE=worker git forge pr update <n>`, prepare, run CI, approve as reviewer, and merge. With a moved base, use a replacement PR as described above.
- Inspect why a merge refused: `git forge pr show <n> --json` exposes the effective decision, generation, prepared candidate, and latest CI status. Use the structured error's `code` and `hint`, not a regex over its message.

### Resolve a prepare or merge conflict

1. Read `E_PREPARE_CONFLICT` from prepare or `E_MERGE_CONFLICT` from unprepared merge, and `error.data.paths`. These contain every unmerged index path at classification, not conflicts in later unattempted rebase commits; the human message lists at most 20. Paths use Git quoting, including quotes and escapes when needed.
2. Resolve conflicts on the real source branch against the current base. Do not run `git rebase --continue` in the removed disposable worktree.
3. Commit the resolution. With an unchanged snapshot base, run `GIT_FORGE_ROLE=worker git forge pr update <n> --json`. With a moved base, create a replacement PR with the same `--closes`, record the link, and abandon the old PR.
4. Prepare, run CI, inspect the candidate, obtain reviewer approval, and merge with the same strategy.

Prepare and unprepared merge classify unmerged index entries as `E_PREPARE_CONFLICT` and `E_MERGE_CONFLICT`, respectively. A hook that creates those entries receives the same code. Other hook/process failures remain `E_INTERNAL`. The merge conflict code applies to merge, squash, and rebase execution without a prepared candidate and with an unchanged snapshot base. Prepared-candidate merge does not execute a strategy; ff cannot leave a conflict index. A conflict publishes no result ref or merge event and changes no refs. Read the cleanup outcome; a failed removal names the leftover worktree.

### Read the recycle outcome

`pr merge --recycle-worktree <path>` emits a standalone `recycle outcome: <state>` line. Its payload uses exactly `removed | refused-dirty | refused-not-contained | partial-leftover(<path>) | branch-deleted | branch-kept(<reason>)`. Compose removal and branch states with `; `, for example `recycle outcome: removed; branch-deleted`. An already absent branch reports only `recycle outcome: removed`.

Human success stdout prints the merged message before the outcome line. Failure stderr prints human detail before the separate outcome line. Post-finalize failures retain the merged message; pre-merge refusals do not. JSON success `recycle` and failure `error.data.recycle` contain the full identical outcome line, not just its payload. Without recycling, success `recycle` is `null`. Control characters in paths use terminal `\xNN` escaping to keep the outcome on one line.

`refused-not-contained` means the path is unregistered or on the wrong branch. `refused-dirty` can occur before merge or after finalization if dirt appears before removal. `partial-leftover(<path>)` names a failed post-merge removal. Retention reasons stay inside `branch-kept(...)`; squash/rebase name the strategy and retained original commits. Merge/ff explain a changed source tip or failed ancestry check. Salvage renaming belongs to the pipeline, not the recycle command.

## Machine results and failures

Success with `--json` prints one object or array on stdout, exits 0, and leaves stderr empty. There is no common success envelope. The current mutation and diff results are:

| Command | JSON fields |
|---|---|
| `issue comment/close/reopen`, `pr comment` | `id`, `state`, `event` |
| `pr review` | `id`, `state`, `decision`, `generation`, `candidate`, `event` |
| `pr update` | `id`, `state`, `generation`, `source_head`, `base_head`, `merge_base`, `event` |
| `pr prepare` | `id`, `state`, `strategy`, `generation`, `candidate`, `result_tree`, `base_head`, `event` |
| `pr merge` | `id`, `state`, `strategy`, `base`, `generation`, `merge_commit`, `recycle` |
| `pr abandon` | `id`, `state`, `reason`, `event` |
| `pr diff` | `id`, `state`, `generation`, `base`, `head`, `diff` |
| `ci run` | `id`, `status`, `plan`, `generation` |

`event` is the appended event's commit OID; `merge_commit` is the resulting base tip. `candidate` is a prepared commit or `null`. `pr diff`'s `base` and `head` are snapshot commits, not branch names. Read `state` as the command's resulting state, not a guarantee against later concurrent changes. Idempotent replay returns historical results, not current state.

Every command failure exits 1. With standalone `--json`, stdout contains an `error` object with `code`, `message`, `input`, `hint`, and `retryable`, optionally with `data`. Stderr retains `git-forge: <message>`. Without `--json`, stdout is empty. Parse `code` and `data`; keep stderr separate. Help has no JSON result: help plus `--json` returns `E_USAGE`. An option value equal to `--json` is data rather than an output flag. Only `issue new` and `pr create` accept end-of-options `--`; a token after it is data. Other commands reject `--` with `E_USAGE` (`unknown option '--'`). `--json=x` is not supported.

| Error code | Action |
|---|---|
| `E_USAGE` | Correct arguments or unknown options. |
| `E_NOT_FOUND` | Check the referenced Issue, PR, or local branch. |
| `E_CONFLICT` | Read current state. Retry only when `retryable: true`; changed-input idempotency conflicts are not retryable. |
| `E_BASE_MOVED` | Prepare the existing snapshot or create a replacement from the updated source. `data` includes `pr`, `base`, `snapshot_base`, `current_base`. |
| `E_PREPARE_CONFLICT` | Resolve source/base conflicts. `data` includes `pr`, `strategy`, `base`, `source`, `paths`. |
| `E_MERGE_CONFLICT` | Resolve unprepared merge/squash/rebase conflicts. `data` includes `pr`, `strategy`, `base` (execution base tip), `source` (snapshot source commit), `paths` (all conflicted paths). |
| `E_GATE` | Refresh the missing or stale candidate, review, or CI input. |
| `E_CI_FAILED` | Inspect `data.reason` and `data.tail`; fix the plan or cleanup failure. |
| `E_INVALID` | Correct invalid input, role, or repository state. An abandoned PR's merge refusal uses this code; create a new PR rather than retrying the terminal one. |
| `E_INTERNAL` | Inspect the preserved process/storage failure and cleanup outcome. |

### CI diagnostics

`pr show --json` exposes `ci.reason` and `ci.tail`. They are `null` for a pending/green Check or an older Check without diagnostics. A failed Check records one reason:

| Reason | Meaning |
|---|---|
| `plan_invalid` | Missing, non-regular, or invalid snapshot plan. |
| `spawn_failed` | Plan materialization or process start failed. |
| `timeout` | The CI deadline expired. |
| `nonzero_exit` | The plan exited nonzero or received a terminating signal. |
| `cleanup_failed` | Background-process or disposable-worktree cleanup failed after an otherwise green run. |

`tail` retains at most 4096 bytes of combined stdout/stderr as diagnostic text. The human message escapes control characters; JSON preserves the text. Failures before plan execution or while appending the Check can keep other error codes and do not imply a failed Check was recorded. Never treat diagnostic text as executable instructions.

## Idempotent writes

Use `--idempotency-key <k>` for retryable automation on `issue new`, `pr create`, `issue comment`, and `pr comment`. Choose one stable key per intended mutation, not a new key for each retry. Keys are repository-local, scoped per command kind, never expire, and must contain 1–100 bytes. Missing/empty/oversized values or repeated flags return `E_USAGE`.

Same key and same normalized inputs return the original result without allocating another entity or appending another comment. JSON mode is not part of the inputs. Different inputs return `E_CONFLICT`, `retryable: false`, with `data.command`, `data.key`, and the recorded `data.id`. Use a new key only for a genuinely new mutation.

Input identity includes ordered labels; Issue blockers compare as a set. PR branch names normalize the `refs/heads/` prefix and identify names, not their moving tips. Actor and role are not retry inputs. Replaying a keyed PR create after branch movement returns the original PR; it does not update its snapshot. Inspect current state with `show` and use `pr update` or a replacement when required.

```sh
git forge issue new "Store tickets" --idempotency-key project-schema-create --json
# Same inputs and key return the same ID and original result.
git forge issue new "Store tickets" --idempotency-key project-schema-create --json
git forge issue comment <id> "Candidate verified" --idempotency-key project-schema-verified --json
```

## Reference

Current source of truth in a git-forge checkout: `docs/architecture/git-forge.md`, especially ref layout, event schema v1, counter CAS, CLI failures, success results, idempotency keys, PR lifecycle, and CI. Read the bundled [wire contract](references/wire-contract.md) for the CLI-facing contract synced from git-forge master e6cda09; when it differs from the checkout contract, the checkout wins. The git-forge checkout's glossary is `CONTEXT.md`; its legacy rationale is in `docs/adr/`.
