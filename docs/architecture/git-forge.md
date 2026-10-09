# git-forge Architecture

## Scope & Goals

A local, git-native forge: issues, pull requests, review, and merge inside an ordinary git repository, using git protocol only, with zero resident processes and no GitHub synchronization. Commands surface as `git forge ...` (plus thin `git issue`/`git pr` wrappers). L1 is single-user, single-machine, single repository (no forge-state sync); multi-clone sync, convergence, and bare-remote enforcement are L2.

Must-haves (L1): issue event chains; PR event chains with review (approve/reject, whole-PR and commit-anchored inline comments); merge with merge-commit/squash/rebase strategies; command-level merge gate; on-demand CI execution (`git forge ci run`, recorded as a `ci.check`); sequential-id allocation via counter CAS. Code push remains native git; forge refs sync (`git forge clone/init` refspec wiring, `git forge push/pull`, remote-tracking, DAG convergence) is L2.

Must-not-haves (L1): web UI, single-file export, GitHub bridge, accounts/permissions, milestones/assignees, pre-receive enforcement, forge refs sync (push/pull/refspecs/convergence). Labels are **in** L1 scope: `issue created`/`pr created` events carry an optional `labels` array (see Event JSON Schema v1), and the CLI accepts repeatable `--label` on `issue new`/`pr create`.

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
| `event` (model) | `Event`, `EventKind`, `JsonValue`, `is_uuid_v4` | Event schema v1, UUID generation, kind/body validation | Deep: pure, no I/O, fully testable; deletion would scatter state logic across CLI and store |
| `fold` (state derivation) | `fold(events) -> FoldState`, `IssueState`/`PrState`/`SeqState`, `first_allocation` | Deterministic ordering, issue/PR state derivation, effective review decision | Pure, no I/O |
| `store` (git refs) | `EventStore`: `append_event`, `read_chain`, `create_pr`, `allocate_id` (versioned counter CAS); `append_event_claimed`, `create_pr_claimed`, `apply_issue_batch_claimed`, `idempotency_record` (idempotency records) | Ref naming under `refs/forge/*`, commit creation, packed-ref-safe reads, counter commit chain | Deep: hides all git plumbing; callers never touch refs directly |
| `identity` (actor) | `resolve_identity`, `bind_identity`, `open_mutation_store` | The invoking repo's `user.email` resolved once per write via `crate::git::config_get_identity` (a safe `git config --null --get` child — never libgit2, VAL-115 stage A). Stage A (config-open fault) propagates a clean CLI error; stage B (absent/empty/non-UTF-8 email) falls back to `forge@localhost`. Signature + actor are bound before any write. | Shallow: never a libgit2 `repo.config()` read at write time (the VAL-115 SIGSEGV-on-corrupt-config path) |
| `git` (adapter) | `worktree_add`/`worktree_remove`/`worktree_list`, `base_checked_out_elsewhere`, `execute_strategy`, `cleanup_failed_worktree`, `require_single_merge_base` | `git` binary shell-outs for merge execution and worktree plumbing. `git/strategy.rs` owns merge/squash/rebase execution and conflict classification before cleanup. | Deep: protocol correctness delegated to git itself; the merge-gate predicate stays in `merge_state` |
| `ci` (runner) | `ci_timeout`, `snapshot_ci_plan`, `run_ci_plan` | Bounded CI plan execution in a disposable worktree: immutable-snapshot plan resolution/materialization, process-group kill+reap under a deadline, env tightening, one failure `reason` per non-green run and the bounded 4 KiB output tail (`ci/output.rs`) | Security-sensitive: isolates the plan subprocess (`/bin/bash` / `just`) and its environment |
| `merge_state` (merge gate + finalize) | `read_merge_gate`/`validate_merge_gate_at`, `finalize_pr_merge`, `run_finalize`, `create_pending_result_ref`/`delete_pending_result_ref` | Merge-gate predicate bound to the exact validated head OID, one-transaction completion, and pending-result lifecycle. `merge_state/finalize.rs` owns CAS retries and adopted-input revalidation. | Deep: gate and merge atomicity live here, not `cli` |
| `pr_merge` (merge commands) | `cmd_pr_merge`, `pr_merge_help` | Merge orchestration (live-base resolution + moved-base candidate routing, checked-out-base guard, temp-worktree lifecycle, pending-result cleanup); gate + finalize delegated to `merge_state` | Deep: the orchestration boundary; `cli` dispatches to it |
| `issue` (commands) | `run_issue` dispatch; issue `cmd_*` handlers | Issue command parsing + human-readable output; apply and new graph publication delegated to `issue_apply`; graph JSON derived from folded states | Shallow: command surface only |
| `issue_plan` (plans) | strict plan parsing, whole-graph validation, desired-state diff | Minimal frontmatter grammar; repository-wide key mapping and reference resolution; parent/blocking cycle checks | No YAML dependency; shared graph rules for apply and new |
| `issue_apply` (publication) | `cmd_apply`, `new_issue_keyed` | Complete Issue graph snapshot, allocation, event construction and one atomic snapshot/CAS publication via store | Owns orchestration, not a second storage implementation |
| `idempotency` (keys) | `FLAG`, `set_key`, `take_key`, `issue_new_request`/`pr_create_request`/`comment_request`, `replay`, `recheck`, `output`, `comment` | `--idempotency-key` parsing, the canonical inputs per command, replay or `E_CONFLICT` before any write, the record re-read after a failed keyed write (`recheck`), and the keyed comment operation shared by `issue comment` and `pr comment` (`comment`); the record and its atomic publication live in `store/idempotency.rs` | CLI-layer owner of the key contract; one implementation for the four keyed commands |
| `cli_error` (failures) | `CliError` | Stable codes, preserved human messages, optional structured data and JSON error rendering; typed storage conversion | CLI-layer boundary; core and store do not import it |
| `pr_create` (create command) | `cmd_pr_create` | Branch resolution and atomic PR creation through `store::create_pr` | CLI-layer owner, extracted to retain the file-size gate |
| `pr_review` (review command) | `cmd_pr_review` | Review argument handling and generation/candidate-bound event publication | CLI-layer owner, extracted to retain the file-size gate |
| `cli` (entry) | `run_pr`, `run_ci` dispatch; `cmd_pr_diff` (arguments + result handling) | Command parsing; the merge-gate predicate + finalize live in `merge_state`, orchestration in `pr_merge`; `cmd_pr_diff` delegates the subprocess to `git::git_in_ci` | Shallow by design: dispatch only, no direct subprocess spawning (diff goes through the sanitized git adapter) |
| `sync` (L2) | — | Explicit forge refspec push, additive fetch refspec, per-entity DAG merge convergence | Not built in L1; no `src/sync.rs` yet |
| `hooks` (L2) | — | Pre-receive/post-receive hooks, refspec validation | Not built in L1; no `src/hooks.rs` yet |

## Dependency Rules

```
cli → store → event/fold
cli → git
cli → ci
cli → pr_merge → store → event/fold
cli → merge_state → store → event/fold
cli → identity
issue → issue_apply → issue_plan
issue_apply → store → event/fold
ci → git2 (plan materialization; no core, no store)
git → git2 (no core, no store)
event/fold: zero external dependencies (no git, no I/O)
```

- `event`/`fold` never import git2 or perform I/O; tests run with no infrastructure.
- `store` depends on `event`/`fold` types only.
- Command modules parse user input; `issue_plan` parses the strict Markdown plan format. The merge-gate predicate + finalize construction live in `merge_state`; merge orchestration lives in `pr_merge`.
- `merge_state` owns the merge-gate predicate + finalize transaction construction; `pr_merge` owns the merge orchestration; `cli` dispatches to it. `identity` resolves the actor/signature once per write.
- `git` is the only module that shells out for merge execution; it does not import `cli` or `store`.
- `ci` is the only module that executes the CI plan subprocess (`/bin/bash` for `.forge/ci.sh`, `just` for the `just check` fallback) and performs process-group signalling via a direct `kill(2)` syscall (`libc`, no spawned `/bin/kill`); it imports git2 for plan materialization but never `core`/`store`.
- `sync` (L2) will depend on `store`, `event`/`fold`, and `git`; it is not wired in L1.

## Directory Structure

Flat, feature-first (complexity ladder: solo dev / MVP — layers not earned yet):

```
git-forge/
├── Cargo.toml
├── AGENTS.md
├── CONTEXT.md
├── constraints.yaml
├── justfile
├── docs/
│   ├── adr/0001-…0006-…
│   └── architecture/git-forge.md
├── src/
│   ├── main.rs          # `git forge` entry binary
│   ├── lib.rs           # module root: declares ci, cli, event, fold, git, identity, issue, pr_merge, store
│   ├── cli.rs           # shared CLI plumbing + pr/ci command surfaces + dispatch; cmd_pr_diff git diff
│   ├── ci.rs            # CI runner: plan resolution/materialization, bounded exec (bash/just), process-group kill
│   ├── ci/validate.rs   # Just fallback closure validator (F-012) split out for the size gate
│   ├── ci/output.rs     # bounded capture of the plan's combined output (last 4 KiB tail)
│   ├── event.rs         # event model + JSON schema (pure)
│   ├── fold.rs          # state derivation (pure)
│   ├── git.rs           # git binary adapter: merge-execution shell-outs (worktree, strategies, cleanup, merge-base)
│   ├── identity.rs      # actor/user.email resolution (F-028) + commit-signature binding
│   ├── issue.rs         # issue command surface: new/apply/list/show/comment/close/reopen
│   ├── issue_plan.rs    # strict plan parser, complete graph validation and diff
│   ├── issue_apply.rs   # snapshot + atomic apply/new graph publication
│   ├── store/issue_batch.rs # snapshot/CAS batch ref transaction
│   ├── store/idempotency.rs # idempotency record refs, published in the mutation's transaction
│   ├── idempotency.rs   # --idempotency-key parsing, canonical inputs, replay
│   ├── pr_merge.rs      # cmd_pr_merge: merge-gate predicate + orchestration (temp worktree, pending result, atomic finalize)
│   └── store.rs         # refs read/write via git2
└── tests/
    ├── t0_core.rs       # event/fold unit tests
    ├── t1a_store.rs     # store layer tests
    ├── t1b_issue.rs     # issue CLI tests
    ├── t2_pr.rs         # PR create/show/review/diff tests
    └── t3_merge.rs      # merge strategy + failure-cleanup tests
    (L2) e2e_push_pull.rs, e2e_converge.rs  # refspec sync, DAG convergence (not built)
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

### CLI Failure Contract (schema 1)

Every command failure exits 1. Without `--json`, stdout is empty and stderr
prints `git-forge: <message>`. With a bare `--json` token, stdout contains one
JSON error object and stderr keeps the same line. This also applies when a
command rejects `--json` because it has no machine success result.

A bare `--json` is a standalone token: not the value of a value-taking
option and not after an end-of-options `--` (`issue new`, `pr create`).
`issue new t --label --json` creates an Issue labeled `--json` and prints
the human result; `--idempotency-key --json` uses the key `--json`. Each
command declares its value-taking options to the shared split
(`machine::json_requested_with`), and the result of that split also decides
whether a failure prints a JSON error. Where the decision is made:

| Path | JSON error when |
|---|---|
| Top level (`git-forge <cmd>`), unknown command, `forge <sub>` with an unknown `<sub>`, `version` | any argument is `--json` (no command grammar ran) |
| Namespace dispatch (`issue`, `pr`, `ci`, the `forge` form and the `git-issue`/`git-pr` wrappers) | the subcommand slot is `--json`, or a standalone `--json` follows the subcommand; an unknown subcommand has no value-taking options, so every `--json` after it is standalone |
| Namespace help (`help`, `-h`, `--help` as the subcommand, or a leading help token after a known subcommand) | a standalone `--json` follows the subcommand (`E_USAGE`, help has no JSON) |
| A known subcommand | its own split decides, before its parser reports any usage error |

```json
{"error":{"code":"E_USAGE","message":"usage: git forge pr show <n> [--json]","input":"","hint":"Run the command with --help and correct its arguments.","retryable":false}}
```

`code`, `message`, `input`, and `hint` are strings. `retryable` is a boolean.
`input` identifies the offending argument when known; it is empty for a
missing argument or a failure without one offending value. `hint` names the
next action. Consumers must use `code`, not parse the message. Command-specific
errors may add `data` without changing these fields. Existing success JSON
keeps its current schema-1 shapes; it has no common envelope.

| Code | Meaning |
|---|---|
| `E_USAGE` | Missing or malformed command arguments, or unknown options |
| `E_NOT_FOUND` | Referenced Issue, PR, or branch does not exist |
| `E_CONFLICT` | Compare-and-swap conflict, exhausted CAS retries, or stale snapshot; read current state before retrying. Also an idempotency key reused with different inputs (`retryable: false`) |
| `E_BASE_MOVED` | Base movement prevents the requested operation |
| `E_PREPARE_CONFLICT` | Candidate preparation encounters a merge conflict |
| `E_GATE` | Merge gate refuses the operation |
| `E_CI_FAILED` | CI execution or cleanup fails |
| `E_INVALID` | Domain validation rejects input or repository state |
| `E_INTERNAL` | Unclassified storage, process, or internal failure |

Known limitation: preparation classifies failures from the unmerged index before
cleanup. A failing hook that leaves unmerged index entries returns
`E_PREPARE_CONFLICT`; the CLI does not distinguish hook-created entries from
strategy conflicts. Hook and process failures without unmerged entries remain
`E_INTERNAL`.

The CLI preserves typed CAS failures as `E_CONFLICT` with `retryable: true`.
Other failures are not automatically retryable. The prepare-conflict,
moved-base, and CI-failure payloads are below.

Canonical local branch names that are absent return `E_NOT_FOUND` with the
supplied branch argument as `input`. Malformed or noncanonical branch inputs
remain `E_INVALID`. An invalid configured role returns `E_INVALID` and names
the rejected role in `input`. Version usage failures retain the offending
flag or positional in `input`.

Merge and prepare print their JSON success results (below) under `--json`;
without it their human success output is unchanged. Prepared-candidate currentness, strategy
binding, and gate validation precede merge-base and strategy preconditions for
every merge strategy. Staleness, mismatched candidate gate inputs, and missing
required candidates return `E_GATE` with the existing candidate remedy.
Finalization preserves the original human context and cleanup suffix while
retaining retryable `E_CONFLICT` for recognized CAS rejection and exhaustion.
Each finalization ref expects a nonzero existing OID; movement or deletion
rejects the entire transaction. Expected-absent creation occurs only in the
separate pending-result publication, not finalization. Unknown Git diagnostics
remain `E_INTERNAL`; missing-reference classification requires an exact named
update operation with a nonzero expected OID.
Preparation returns `E_PREPARE_CONFLICT` when a failed strategy leaves an
unmerged index, subject to the hook limitation documented beside the code list.

#### Prepare conflict

`pr prepare <n> --strategy merge|squash|rebase` reads the unmerged index
entries of the temporary worktree before cleanup. When at least one exists,
the command fails with `E_PREPARE_CONFLICT`. The message is:

```text
PR #<n> cannot be prepared with strategy <s>: source <source> conflicts with base '<base_ref>' at <base> in <k> path(s): <p1>, <p2>, ...; merge '<base_ref>' into the source branch '<source_ref>', resolve the conflicts, then run `git forge pr update <n>` or create a replacement PR; worktree cleaned up, no ref changes made
```

- The message lists at most 20 paths, then ` (and <m> more)`.
- Paths are the unmerged (stage 1-3) index entries, one per exact path
  byte sequence, in index order, whatever the conflict kind (content,
  add/add, modify/delete, rename/delete, or a hook that writes unmerged
  entries). Paths that differ only in case stay distinct under
  `core.ignorecase`.
- Each path is the git-quoted path (`core.quotePath=false`, git's
  `quote_c_style`), in the message and in `data.paths`. Valid UTF-8 without
  `"`, `\` or a control byte appears as is. Any other path is double-quoted
  with `\"`, `\\`, `\a \b \t \n \v \f \r`, and `\ooo` octal for every
  other control byte. Unlike git with `core.quotePath=false`, an invalid
  UTF-8 byte is also `\ooo` octal, so distinct paths stay distinct. Example:
  the bytes `new⏎line` become `"new\nline"` and `bad<0x80>.txt` becomes
  `"bad\200.txt"`.
- The message also escapes control characters that quoting leaves (C1
  controls U+0080-U+009F in paths, any control in branch names) in `\x9b`
  style; `data.paths` keeps the git-quoted path.
- Git's own diagnostic text is not forwarded: it names commands
  (`git rebase --continue`) that do not apply to the removed worktree.
- The final clause is the unchanged cleanup outcome. When worktree removal
  fails, it names the leftover worktree instead.
- `input` is the PR id, `retryable` is `false`, and `hint` is
  ``Merge the base branch into the source branch, resolve the conflicts, then run `git forge pr update <n>` or create a replacement PR.``
- `data` keys, in order: `pr` (number), `strategy`, `base` (the base tip the
  candidate was built on), `source` (the snapshot source commit), and `paths`
  (every conflicted path, without the 20-path limit).

```json
{"error":{"code":"E_PREPARE_CONFLICT","message":"PR #1 cannot be prepared with strategy merge: ...","input":"1","hint":"Merge the base branch into the source branch, resolve the conflicts, then run `git forge pr update 1` or create a replacement PR.","retryable":false,"data":{"pr":1,"strategy":"merge","base":"<40-hex>","source":"<40-hex>","paths":["shared.txt"]}}}
```

The `ff` strategy has no conflict path: a source that does not sit on the
base tip fails the fast-forward precondition before a worktree exists. A
failure without unmerged entries keeps its `git <s> failed: <cause>` message
and `E_INTERNAL`. `pr merge` strategy execution is not part of this contract.

#### Moved base

`pr update <n>` resolves the live base branch tip. When it differs from the
snapshot `base_head`, the update is refused before any write with
`E_BASE_MOVED`. The base can have gained commits, been rewritten, or been
reset to an ancestor; all three refuse the same way, also when the source
did not move or an earlier update raised the generation. The message is:

```text
base branch '<base_ref>' has moved since PR #<n> was created; run git forge pr prepare <n> --strategy <s> to merge the existing snapshot, or create a replacement PR from the updated source with the same --closes and abandon PR #<n>
```

- `<s>` is printed literally: `pr update` has no strategy to name. Both
  remedies pass the merge gate; the message never suggests a manual merge.
- The message escapes control characters in the branch name in `\x9b`
  style; `input` and `data.base` keep the exact name.
- `input` is the base branch name, `retryable` is `false`, and `hint` is
  `Inspect the current base and PR snapshot before proceeding.`
- `data` keys, in order: `pr` (number), `base` (the base branch name),
  `snapshot_base` (the snapshot base commit), and `current_base` (the live
  base tip).

```json
{"error":{"code":"E_BASE_MOVED","message":"base branch 'main' has moved since PR #1 was created; run git forge pr prepare 1 --strategy <s> to merge the existing snapshot, or create a replacement PR from the updated source with the same --closes and abandon PR #1","input":"main","hint":"Inspect the current base and PR snapshot before proceeding.","retryable":false,"data":{"pr":1,"base":"main","snapshot_base":"<40-hex>","current_base":"<40-hex>"}}}
```

A deleted base branch is not a moved base: `pr update` keeps its
`branch '<base_ref>' does not exist` refusal with `E_NOT_FOUND`.

#### CI failure

A `ci run` that is not green records a `failed` Check with exactly one
`reason` and the output `tail`, then exits 1 with `E_CI_FAILED`. The reason
is the first cause in run order:

| `reason` | Cause | Detail in the message |
|---|---|---|
| `plan_invalid` | The snapshot has no plan, the plan is a symlink or not a regular file, or the `just check` justfile fails validation | the refusal or validation text |
| `spawn_failed` | The plan could not be started: plan materialization I/O, no trusted `just`, the output pipe, or the spawn | the start failure |
| `timeout` | The plan exceeded `GIT_FORGE_CI_TIMEOUT` | `timed out` |
| `nonzero_exit` | The plan exited non-zero or was terminated by a signal | `exited with status <c>` or `terminated by signal <s>` |
| `cleanup_failed` | A background process survived the plan, the process group could not be proven empty, or the temporary worktree could not be removed after an otherwise green run | `left a background process running`, `could not prove the process group is empty`, or the worktree leftover |

The message is `CI run for PR #<n> failed (<plan>): <detail>; reason:
<reason>; output tail: <tail>`. `(<plan>)` is absent when no plan was
selected. The tail clause is absent when the run printed nothing. A leftover
worktree keeps its `CI run recorded <status> but temp worktree cleanup
failed: <leftover>` message, followed by the same reason and tail clauses,
and the hint `Inspect the reported worktree cleanup failure.`

The tail is the last 4096 bytes of the plan's combined stdout and stderr.
When the cut splits a UTF-8 character, the partial character is dropped. A
split is proven only by a lead byte at most 3 bytes before the cut, followed
only by continuation bytes, whose declared length reaches past the cut. Other
bytes that are not valid UTF-8, including a standalone continuation byte at
the cut, become U+FFFD, and the text is trimmed again
so that the stored tail never exceeds 4096 bytes. `data.tail` and the stored
`tail` are this exact text; the message shows it through the terminal escape
(`\xNN` for control characters, so a newline is `\x0a`).

```json
{"error":{"code":"E_CI_FAILED","message":"CI run for PR #1 failed (.forge/ci.sh): exited with status 1; reason: nonzero_exit; output tail: MARKER\\x0a","input":"1","hint":"Fix the CI plan and run CI again.","retryable":false,"data":{"pr":1,"reason":"nonzero_exit","tail":"MARKER\n"}}}
```

Failures before or after the CI result are not Checks and keep their codes:
a missing or abandoned PR, a missing snapshot ref, a temporary worktree that
cannot be created, and a Check that cannot be appended. The merge gate still
accepts only a `success` Check bound to the current generation and candidate;
`reason` and `tail` never change a gate decision.

Missing required positionals return usage errors, never panics. In particular,
`pr show` and `pr diff` use `usage: git forge pr <verb> <n> [--json]`.

### CLI Success Results (schema 1)

Every command that accepts the bare `--json` token prints one JSON object or
array on stdout on success, exit 0, with nothing on stderr. Without `--json`
the human output is unchanged. There is no common envelope; field order is
fixed per command.

The existing shapes of `issue new|apply|list|show`, `pr create|list|show`,
and `ci run` are unchanged; `tests/fixtures/json/` pins them byte for byte.
The `ci` member of `pr create|show` is `null` or `{"status","plan","reason",
"tail"}`: `reason` and `tail` are the latest Check's failure reason and output
tail (CI failure, above), and `null` for a pending or green Check and for a
Check recorded before these fields existed.

Mutation results open with `id` (the Issue or PR number) and `state`, the
entity state after the command: `open|closed` for an Issue and
`open|merged|abandoned` for a PR. `state` is folded from the entity chain up
to the event commit the command wrote, so an event that a concurrent writer
appended first is included and a later one is not. `event` is the commit the
command appended to the entity chain; `pr merge` has no `event` and returns
`merge_commit` instead. Commit ids are full 40-character hex strings.

| Command | Result object (keys in order) |
|---|---|
| `issue comment <n> <body>` | `id`, `state`, `event` |
| `issue close <n>` / `issue reopen <n>` | `id`, `state`, `event` |
| `pr comment <n> <body>` | `id`, `state`, `event` |
| `pr review <n> --approve\|--reject` | `id`, `state`, `decision` (`approve\|reject`), `generation` (the snapshot generation the decision binds), `candidate` (bound prepared result commit, or `null`), `event` |
| `pr update <n>` | `id`, `state`, `generation` (the new generation), `source_head`, `base_head`, `merge_base` (the new snapshot), `event` |
| `pr prepare <n> --strategy <s>` | `id`, `state`, `strategy`, `generation`, `candidate` (result commit pinned at `refs/forge/prs/<n>/result`), `result_tree`, `base_head` (base tip the candidate was built on), `event` |
| `pr merge [<n>]` | `id`, `state` (`merged`), `strategy`, `base` (base branch name), `generation`, `merge_commit` (the new base tip), `recycle` (the `--recycle-worktree` outcome line, or `null`) |
| `pr abandon <n> --reason <text>` | `id`, `state` (`abandoned`, or `merged` when a concurrent merge landed before the abandon event), `reason`, `event` |
| `pr diff <n>` (read only) | `id`, `state`, `generation`, `base`, `head`, `diff` |

In `pr diff`, `base` and `head` are the commits of the current generation's
snapshot, and `diff` is the exact `git diff base...head` text that the human
form prints. Unlike `pr show`, `base` here is a commit, not a branch name.

```json
{"id":3,"state":"open","event":"5f0c..."}
{"id":3,"state":"open","decision":"approve","generation":1,"candidate":"9ab1...","event":"77de..."}
{"id":3,"state":"merged","strategy":"squash","base":"main","generation":1,"merge_commit":"9ab1...","recycle":null}
```

Help has no JSON result. A help request combined with `--json` exits 1 with
`E_USAGE` and writes nothing. A help request is a `-h` or `--help` token, or
a `help` subcommand word, where the command's parser reads it as help. A
`-h` or `--help` token that is the value of an option (for example
`--label --help`) is data, with or without `--json`. Every command decides
help and argument usage errors before it opens the repository, so a help
request under `--json` is `E_USAGE` also outside a git repository.

A `pr merge --recycle-worktree` whose recycle step fails after the merge is
already final still exits 1 with the merged line and the recycle failure in
the error message; it does not print a success result.


### Idempotency Keys (schema 1)

`issue new`, `pr create`, `issue comment` and `pr comment` accept
`--idempotency-key <k>`. A retry with the same key and the same inputs
returns the first result instead of a second Issue, PR or comment.

- **Key**: any non-empty value of at most 100 bytes, compared byte for
  byte. A missing value, an empty value, a longer value, or a repeated flag
  is `E_USAGE` before any read or write.
- **Scope**: per repository and per command kind. The same key on two
  command kinds names two independent records. Keys never expire.
- **Same inputs** (normalized as the command stores them):
  - `issue new`: title, description, labels in order, parent, and the
    blocking Issues as a set.
  - `pr create`: title, source and base branch names (a `refs/heads/`
    prefix removed; the names, not the commits they point at), body,
    labels in order, and `--closes`.
  - `issue comment` / `pr comment`: the target id and the comment body.
  `--json` is an output mode, not an input.
- **Record**: `refs/forge/idempotency/<command>/<hex>`, where `<hex>` is
  the lowercase hex of the key bytes, points at a root commit whose tree
  holds `.forge/idempotency.json`:
  `{"v":1,"command":"issue new","key":"<k>","inputs":"<digest>","id":<n>,"json":"<result>","text":"<result>"}`.
  `inputs` is the git blob id of the canonical inputs. `json` and `text`
  are the exact `--json` and human results of the recorded mutation, so a
  replay prints them byte for byte in either mode.
- **Atomicity**: the record ref is created (expected absent) in the same
  `git update-ref --stdin` transaction as the mutation's refs. Either both
  are published or neither is. When the transaction fails and a record for
  the key exists, a concurrent call with the same key committed first; the
  command answers from that record instead of retrying.
- **Replay**: the command reads the record before it resolves its identity
  or writes anything. Same inputs: exit 0 with the recorded result, also
  after the entity was closed, abandoned or commented on, or a branch moved.
  Different inputs: exit 1 with `E_CONFLICT`, `retryable: false`, the key
  as `input`, and `data` with `command`, `key` and the recorded `id`:

```json
{"error":{"code":"E_CONFLICT","message":"idempotency key 'k1' was already used by issue new with different inputs (issue #3)","input":"k1","hint":"Repeat the original inputs to read the recorded result, or use a new idempotency key.","retryable":false,"data":{"command":"issue new","key":"k1","id":3}}}
```

- **Loser after validation**: a same-key call that passed the lookup
  before the winner committed re-reads the record when any later step
  fails, including validation of state the winner's commit or a later
  change made stale (a deleted branch, Issue, PR, parent or blocker). It
  answers from the record (the result, or `E_CONFLICT`) instead of the
  failure.

A record ref that exists but is not well formed is `E_INVALID`
(`malformed idempotency record <ref>`): a symbolic ref, a target that is
not a commit, or a commit without a valid record blob for its key. Only an
absent ref means a new key. Commands without the flag read and write no
record.

### Ref Layout

```
refs/forge/issues/<n>                    # event chain, n = sequential issue id
refs/forge/prs/<n>/head                  # event chain, n = sequential PR id
refs/forge/prs/<n>/meta                  # convenience pointer to pr.created snapshot commit (not separate data copy)
refs/forge/prs/<n>/gen/<k>/source        # t1: immutable snapshot ref for generation k >= 1 → gen-k source_head
refs/forge/prs/<n>/gen/<k>/base          # t1: immutable snapshot ref for generation k >= 1 → gen-k base_head
refs/forge/prs/<n>/result                # prepared-candidate/pending merge-result ref → result_commit (created by `pr prepare`, deleted by the merge final transaction or reported as leftover on failure)
refs/forge/meta/counter                  # versioned counter commit chain (next sequential id)
refs/forge/idempotency/<command>/<hex>   # idempotency record (immutable): <command> = issue-new|pr-create|issue-comment|pr-comment, <hex> = lowercase hex of the key bytes
refs/remotes/<remote>/forge/*            # remote-tracking mirror; never local-authoritative
```

- Entity ID is sequential integer. `refs/forge/meta/counter` is a ref to a versioned counter commit whose tree contains `.forge/counter.json` `{v:1, next: <u64>}`; each allocation appends a new counter commit and updates the ref.
- **Lazy initialization**: the first allocation (`issue new`/`pr create`) atomically creates entity `#1` and a counter commit `{next:2}` in one ref transaction; `git forge init` is optional and only pre-creates the counter. A clean repo needs no setup step before `#1`.
- Allocation is atomic: one `git update-ref --stdin` transaction performs (1) CAS on `refs/forge/meta/counter` (expected old counter commit, new counter commit) and (2) `create` of the entity ref with **expected absence** — Issue: `refs/forge/issues/<n>`; PR: `refs/forge/prs/<n>/head`, `.../meta`, `.../source` (→ `source_head`), and `.../base` (→ `base_head`) all in the same transaction. A concurrent allocator or a pre-existing entity ref fails the transaction (retry with fresh read for counter collision; reject if the entity ref already exists). L1 requires a concurrent-allocation test proving two simultaneous `issue new` calls receive distinct `#n`, a stale-counter/preexisting-entity collision test, and a PR first-allocation collision test.
- PR uses `pr.created` as the **authoritative** base/source snapshot; `refs/forge/prs/<n>/meta` is a convenience pointer only. `pr.created` records `base_ref`, `base_head`, `source_ref`, `source_head`, and `merge_base` (= `git merge-base(base_head, source_head)` at creation).
- **Review content**: the PR patch is `merge_base..source_head` (equivalently `git diff base_head...source_head`). An approval authorizes exactly this patch; it never includes base-only commits or their reversions.
- **Immutable snapshot refs, per generation (t1)**: `refs/forge/prs/<n>/source` and `/base` pin generation 0's `source_head`/`base_head`; `pr update` bumps the generation and pins the new snapshot under `refs/forge/prs/<n>/gen/<k>/source` and `refs/forge/prs/<n>/gen/<k>/base`. Every generation's refs are immutable git refs; they keep the PR's commits reachable even after base/source branches are deleted or rebased, so `git gc` cannot prune them. A branch-delete plus `git gc` PR diff/merge test is required.
- All per-entity refs have exactly one mutable tip and are git-managed; read via ref APIs.

### Event Commit Layout

Each event is a git commit with a one-file tree:

- Tree blob: `.forge/event.json` containing the versioned event JSON (below).
- Non-event merge commits (L2 convergence only): empty tree, parent = local tip + remote tip, message `forge:merge:<id>`; the fold algorithm ignores commits whose tree has no `.forge/event.json`. L1 never creates them.
- Message for normal events: `forge:<kind>:<id>` (stable subject for tooling).
- Committer/author identity: the invoking `user.email` / `user.name` from git config, resolved by the CLI via the git binary (`git config --null --get`) and bound as an explicit signature before any write (never a libgit2 config read at write time — see Event Identity).
- Event `actor` (see below): the invoking repo's `user.email`. When `user.email` is absent or empty — not set, or set to an empty/whitespace value, in repo, global, or system config — commands still succeed and the actor falls back to `forge@localhost` (the same default the store commit signature uses); a repo without an email is never blocked from writing events. The identity config-failure policy is stage-specific (F-028): a failure to **open** the repo's config at all (the CLI's `git config --get` resolver exits 128; an environment fault) propagates as a clean CLI error — no silent fallback, no empty actor, no event written; a failure to **look up** `user.email` (absent key, empty/whitespace value, or a non-UTF-8 value) falls back to `forge@localhost`, with commands still succeeding. `user.name` does NOT gate the actor: the actor is always the configured email.
- Event `role` body field (t8, #8): every event MAY carry a `role` in its body — one of the closed set `worker` | `reviewer` | `orchestrator` — resolved ONCE per command from `GIT_FORGE_ROLE` and stamped onto every event the command writes (additive schema-v1 field; no kind changes). An unset/empty `GIT_FORGE_ROLE` writes no `role` key — the wire shape is byte-identical to the pre-t8 schema, so the single-user default (one identity, indistinguishable actors) is unchanged. An unknown value is a fail-closed clean CLI error before any store open or write. The role is attribution provenance, not authentication: it records WHICH PIPELINE SEAT wrote the event, claimed by the writer and as trustworthy as the machine's own config — it does not prove a distinct human. Roles make actors DISTINGUISHABLE even when they share one `user.email` (the single-machine multi-agent case); two events with the same role and same email are the same seat.

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
| `issue.created` | issue | `{title, description?, labels?, key?, parent?, blocked_by?}` |
| `issue.updated` | issue | changed fields only: `{title?, description?, labels?, parent?, blocked_by?}` |
| `issue.comment` | issue | `{body}` |
| `issue.close` | issue | `{}` |
| `issue.reopen` | issue | `{}` |
| `pr.update` | pr | `{old_source_head, new_source_head, old_base_head, new_base_head, old_merge_base, new_merge_base, generation}` |
| `pr.prepared` | pr | `{generation, source_head, base_head, strategy, result_commit, result_tree, merge_base}` |
| `pr.comment` | pr | `{body}` |
| `pr.review` | pr | `{decision: "approve"|"reject", generation, base_head, strategy?, result_commit?, result_tree?, body?, file?, line?, commit?}` |
| `pr.merge` | pr | `{strategy: "merge"|"squash"|"rebase"|"ff", result_commit}` |
| `pr.abandoned` | pr | `{reason}` |
| `ci.check` | pr | `{status: "pending"|"success"|"failed", plan?, generation, base_head, strategy?, result_commit?, result_tree?, reason?, tail?}` |

For `issue new`, an omitted or empty positional description leaves the created
event's `description` field absent. Human `issue show` omits the description
line for that absent field. An apply plan's empty body is an explicit empty
description and remains stored as `""`.

`pr.review` and `ci.check` bind the `generation` they were taken against (t1) and the live `base_head` (t2); when a prepared candidate exists at the time of the event, both also bind the candidate identity (`strategy`, `result_commit`, `result_tree`). `pr.review` is anchored to optional `commit` + permission to include `file`/`line` for inline comment. Inline comments are immutable references to a snapshot commit; they never follow later diff changes.

`pr.abandoned` makes the PR **terminal** (see PR Lifecycle): `reason` is the required abandonment text; the PR is then refused by `pr merge`, `pr review`, and `ci run` before any write, and no reopen exists — a reopened PR is a new entity.

`ci.check` records an on-demand **CI Run** on a PR. Its `body` carries `status` (`pending` | `success` | `failed`) and, once a run has executed, `plan` (the plan that ran: `.forge/ci.sh`, or the `just check` fallback), plus the `generation` captured at run start and the live `base_head` (and the prepared-candidate identity when the run validated a candidate); the initial pending marker from `pr create` carries only `status: "pending"` — no plan executes at creation. A `failed` Check written by `ci run` also carries `reason` and `tail` (CI failure, above); `plan` is absent when the plan was refused before selection. Checks without these fields fold as `None`. The top-level `ts`/`actor` record when the Check was written and who invoked the run. Folding keeps the LATEST `ci.check` for the PR, so the merge gate reads a single current status and requires it to be `success` on the PR's CURRENT snapshot generation — a green Check from an older generation is stale (an in-flight run whose append lands after a concurrent `pr update` still carries its captured generation and never greens the new one).

**Effective review decision (L1)**: the last reachable `pr.review` event in the entity chain's parent order (single local chain in L1) is the effective PR decision; the merge gate requires that decision to be `approve` AND bound to the PR's current snapshot generation — each `pr.review` stamps the `generation` captured at review time (plus the live `base_head`, and the prepared-candidate identity when one is current), and the gate compares that captured generation with the PR's CURRENT generation, so a pre-update approval is stale and refused with a re-review remedy. Repeated decisions by the same or different actors simply advance the effective decision; no per-actor union semantics. DAG linearization for multi-chain convergence is deferred to ADR-0004/L2. `approve → reject` and `reject → approve` transitions are first-class test cases.

### Issue Graph and Declarative Apply (L1)

`git forge issue apply <dir|file> [--dry-run] [--json]` (also `git issue apply`)
publishes the desired Issue metadata and state from Markdown plans. A directory
is nonrecursive: read directly contained regular `.md` files in filename order;
ignore symlinks within directories. A single file uses the same parser. All
files must parse and validate before any ref write.

#### Strict frontmatter format

```markdown
---
key: project-schema       # stable repository-wide identity
title: Store tickets
parent: project-spec
blocked_by: [project-api, "#12"]
labels: [ready-for-agent]
state: open
---
<exact Markdown description>
```

- Opening and closing frontmatter delimiters are `---` lines. `key` and `title`
  are required non-empty scalar strings; whitespace-only titles are invalid.
  A decoded key may contain whitespace or punctuation, but must not start with
  the reserved `#` reference prefix. Quoted keys preserve their decoded bytes.
- The only fields are `key`, `title`, `parent`, `blocked_by`, `labels`, and
  `state`. Unknown/duplicate fields, missing delimiters and malformed values
  are errors naming the file and field. This is not general YAML: no nested
  mappings, block lists, tags, anchors, or multiline scalar syntax. The parser
  remains minimal without a heavy YAML dependency.
- Scalars are strings, optionally enclosed in single or double quotes. Single
  quoted strings cannot contain an internal single quote; double quoted strings
  use JSON escapes. Plain scalars cannot contain `[`, `]`, commas or quotes;
  quote keys, references, titles or labels containing those characters.
  `blocked_by` and `labels` are inline arrays of scalar strings, for example
  `[api, "#12"]` or `['ready-for-agent']`; an empty array is `[]`.
  Comments start at an unquoted `#` preceded by whitespace. Existing Issue
  references must be quoted (`"#12"` or `'#12'`); bare `#n` is not a reference.
- `parent` is one reference; `blocked_by` is an array of references. References
  resolve to another entry's key in this apply or an existing Issue id `"#<n>"`.
  Keys are stable repository-wide identities stored on Issues, not scoped to a
  directory, filename or session. A repeated apply matches the existing Issue
  by key; duplicate plan keys or ambiguous stored identities are errors.
- The bytes after the closing delimiter line are the description: preserve
  Markdown, whitespace and trailing newlines exactly, without trimming or
  injecting a ticket identity line.
- Omitted `labels`, `parent`, and `blocked_by` mean empty labels, no parent,
  and no blockers; they clear existing values. Omitted `state` preserves an
  existing Issue's open/closed state; a new Issue starts open. Explicit
  `state: open` or `state: closed` enforces that state. Title is always required.

#### Graph, diff and event semantics

Validate the complete resulting graph, including existing Issues outside the
plan. Every parent/blocker must resolve to an Issue (not a PR). Parent links
and blocking edges are independent acyclic graphs; unknown references,
self-blocking, blocking cycles and parent cycles are rejected before publication.
An Issue's optional `key`, optional `parent` id, and `blocked_by` ids are stored;
`children` and `blocked` are derived, never stored.

Each entry is `create`, `update`, or `unchanged`, with its resolved sequential
Issue id and changed fields. Apply JSON is a plain array of objects
`{key, id, action, changed_fields}`; human output reports the same diff.
Prospective ids are allocated deterministically in filename order. `--dry-run`
reports tentative ids but publishes no refs/events and consumes no counter
values. Re-applying an identical plan appends no events and allocates no ids.

`issue.created` gains optional `key`, `parent` and `blocked_by` fields under
schema v1. Old chains lacking them fold to `key: null`, `parent: null`, and
`blocked_by: []`. `issue.updated` carries only metadata fields actually changed:
title/description strings, a labels array, a parent id or `null` (clear), and a
blocked_by id array. It does not change the key or carry state. State transitions
use existing `issue.close`/`issue.reopen` events; an already-satisfied state
writes neither. New Issues begin open and an explicit closed plan adds a close
event to the same unpublished chain. Metadata-only edits write only
`issue.updated`, never a replacement `issue.created`.

#### Atomic complete-graph snapshot/CAS

Read and fold the entire Issue graph together with its observed ref tips and
the counter tip. Resolve keys, allocate prospective ids, validate the complete
desired graph, and prepare all event chains before publishing. ONE
`git update-ref --stdin` transaction verifies every read Issue tip (including
unchanged Issues and dependency targets outside the plan) and the counter tip.
Changed tips are CAS updates; unchanged read tips are verify-only operations;
new Issue refs require expected absence. Allocation advances the shared counter
in that transaction; without allocations the observed counter is still verified.
The counter also protects against concurrent allocations introducing graph
members after the snapshot. An absent counter is verified as absent.

Snapshot reads bracket Issue ref enumeration and folding with counter-tip reads.
A moved counter discards the entire read and starts a fresh snapshot, bounded
by the same three-attempt allocation limit. Exhaustion returns `CasConflict`;
`issue new` retries it within its allocation loop, while `issue apply` reports
the conflict. A stable counter pointing at an existing Issue ref remains an
error, not a retryable conflict. Existing Issue tip changes remain protected
by publication CAS; the snapshot read does not lock the graph.

A moved dependency, changed Issue tip, counter conflict or occupied new ref
fails the entire transaction: no partial Issue refs or counter advancement.
Prepared immutable git objects may exist, but are not published forge state.
A fresh snapshot and full revalidation are required before another publication
attempt; never retry only the stale changed subset. `issue new` uses the same
whole-graph validator and atomic graph publication, not separate edge rules.
`issue apply` reports a conflict without retrying. Individual `issue new` keeps
the existing three-attempt allocation bound: only an identified moved-ref CAS
conflict retries, with a fresh complete snapshot, id and graph validation.
Other transaction failures propagate without retrying, even if another ref
moves concurrently. Retry classification requires Git's expected-old-value
rejection for a ref and expected value in the attempted transaction. Git runs
with `LC_ALL=C`; unrecognized diagnostics fail closed without retrying.

#### Individual commands and graph display

- `issue new <title> [description] [--label <x>]... [--parent <n>]
  [--blocked-by <n>]... [--json]` accepts an optional parent and repeatable
  blockers, sharing apply's graph validation. The existing comment, close,
  reopen and `pr create --closes <n>` commands remain available.
- `issue list [--parent <n>] [--state open|closed|all] [--json]` filters by direct
  parent and state; the default state filter is `all` (preserves existing list
  behavior). `issue show <n> [--json]` displays one Issue.
- JSON keeps the existing fields and adds `key` (string or null), `parent`
  (Issue id or null), `blocked_by` (Issue ids), and `blocked` (boolean). Show
  also adds `children` (direct child Issue ids). Compute `blocked` against the
  complete graph, not a filtered list: it is true if any referenced blocker is
  open, and false for an empty blocker array. Children are derived from all
  parent links. An open, unblocked child is the executor's frontier.

### PR Lifecycle (L1: immutable snapshot)

- L1 PRs are **per-generation immutable snapshots**: `pr.created` fixes `source_head` and `base_head` as generation 0; the PR never silently follows branch movement. `git forge pr update <n>` re-pins the PR to the source branch tip as a NEW immutable snapshot generation (`pr.update` event + `gen/<k>` refs, t1); an approval/CI Check from an older generation is stale and cannot satisfy the merge gate.
- `git forge pr create --source <branch> --base <branch> <title> [--body <text>] [--label <x>]...` — L1 requires `--source`, `--base`, and a non-empty title positional argument; missing `--source`, missing `--base`, or empty/whitespace-only title errors (no heuristic defaults; no self-PR). `--body` sets the optional PR description; `--label` (repeatable) attaches labels. `--source` and `--base` must resolve to canonical local `refs/heads/*` branches: tags, remote-tracking refs, OIDs, and revision expressions are rejected (merge later CASes `refs/heads/<base>` and the checked-out-base guard operates on local branches). Explicit `--source` equal to `--base` (or distinct local branches resolving to the same commit) is rejected — no self-PR. Deferred to L2: derive default base from `refs/remotes/<remote>/HEAD` when present.
- `pr.created` records `source_ref`, `base_ref`, `source_head`, `base_head`, and `merge_base` (= `git merge-base(base_head, source_head)` at creation). **L1 requires exactly one merge base, checked with `git merge-base --all base_head source_head`**: count != 1 rejects with a clear error and creates no PR. Zero bases covers unrelated histories and shallow repositories (incomplete history); for shallow repos the error must ask the user to deepen/unshallow. Multiple bases covers criss-cross histories. Review diff and rebase are undefined without a single base.
- Immutable snapshot refs `refs/forge/prs/<n>/source` (→ `source_head`) and `/base` (→ `base_head`) are created atomically with `/head` and `/meta`, keeping PR commits reachable through `git gc` and providing the merge inputs after branch deletion/rebasing.
- **Review content**: the PR patch is `merge_base..source_head` (equivalently `git diff base_head...source_head`). An approval authorizes this exact patch; it never includes base-only commits or their reversions.
- **Live-base merge (t2)**: when the current `base_ref` tip differs from the snapshot `base_head`, the merge is NOT refused outright. The merge result is a function of (source snapshot, CURRENT base tip), so the merge binds a **prepared candidate** (`pr prepare`): the candidate is materialized against the current base tip, pinned at `refs/forge/prs/<n>/result`, and validated by a candidate-bound `ci run` + candidate-bound `pr review --approve`. The merge gate requires the candidate to be current (prepared generation == snapshot generation, prepared base_head == current base tip), the strategy to match the merge's `--strategy`, and every gate input to be bound to the candidate's identity (strategy/result_commit/result_tree). The finalize CASes `refs/heads/<base>` FROM the candidate's base_head TO the candidate's result_commit — never re-materializing at merge time.
- **`git forge pr prepare <n> --strategy <merge|squash|rebase|ff>`**: materializes the strategy in a temporary worktree detached at the CURRENT base tip (source snapshot for rebase), appends `pr.prepared` (generation, source_head, base_head = current base tip, strategy, result_commit, result_tree, merge_base) and CAS-pins `refs/forge/prs/<n>/result` → result_commit in ONE atomic ref transaction. Re-prepare REPLACES the prior candidate (CAS old→new); it does NOT bump the generation. `--strategy` is required; `ff` requires the source to sit directly on the current base tip. Stdout: `prepared PR #<n> for strategy <s> (candidate <result_commit>, generation <g>, base <base_head>)`; `pr show` renders an additive `prepared: strategy <s> result <hex>` line. `pr update` invalidates the candidate (generation bump → merge refuses with a re-prepare remedy).
- **Moved-base routing (t2)**: at merge time, the base branch's CURRENT tip is resolved live. When it equals the snapshot `base_head`, the legacy path merges exactly as before. When it differs, the merge result is re-derived against the CURRENT tip — but only through the **prepared-candidate flow** (the two bullets above): a moved base without a current prepared candidate refuses with the `pr prepare` remedy and makes no ref changes; a stale candidate (superseded by `pr update`) refuses with a re-prepare remedy. (A base that has NOT moved but is checked out is handled by the checked-out base guard + live-base refresh, below.)
- **Abandoned PRs are terminal (L1)**: `git forge pr abandon <n> --reason <text>` appends a single `pr.abandoned` event (body `{"reason": <text>}`) to the PR `/head` chain; the reason is required (empty/whitespace-only is rejected before any write). An abandoned PR is **terminal**: `pr merge`, `pr review`, and `ci run` all refuse it with a non-zero exit naming the abandon reason, before any write (no `pr.merge`, no `pr.review`, no `ci.check` appended; no ref changes; CI plan not materialized/executed). A second `abandon` refuses with the ORIGINAL reason and appends nothing; `abandon` on an already-merged PR refuses; unknown PR ids refuse without consuming the counter. There is **no reopen** — a reopened PR is a new entity. `pr show` renders `abandoned: <reason>`.
- Merge semantics are defined against the snapshot AND the live base: `source_oid` is resolved from the immutable generation-current source ref (`refs/forge/prs/<n>/source` for generation 0, `refs/forge/prs/<n>/gen/<k>/source` for `pr update`'s generation k), never the live source branch; the strategy executes against the CURRENT base tip (`merge_base` is recomputed against it; identical to the snapshot `base_head` when the base has not moved). A moved base goes through the prepared-candidate flow above — the candidate result was materialized at prepare time and is never re-materialized at merge time. `merge` = `git merge --no-ff --no-edit <source-oid>` (noninteractive merge commit of `source_head` into the current base tip; normal hooks run); `squash` = `git merge --squash <source-oid>` then **`git commit -m "<PR title>"`** (noninteractive via `-m`; the title is the required non-empty `pr.created` field; identity from `user.name`/`user.email`; normal `pre-commit`/`commit-msg` hooks run — never `-n`); `rebase` = `git rebase --onto <current base tip> <merge_base>` in a worktree detached at `/source`, taking resulting HEAD as `result_commit`; `ff` = `git merge --ff-only <source-oid>` (fast-forward; the CLI has already checked the source sits directly on the current base tip, and `merge --ff-only` fails closed if git still refuses). All executed via the `git` binary.
- **Failure/conflict rule (L1)**: any failed merge leaves the disposable worktree cleaned: **default merge** (conflict, or a failing `pre-merge-commit`/`prepare-commit-msg`/`commit-msg` hook — MERGE_HEAD remains; `pre-commit` may also run when an enabled `pre-merge-commit` hook invokes it, e.g. the sample script, so a failing `pre-commit` follows the same abort path) runs `git merge --abort`; **rebase** (conflict or hook failure) runs `git rebase --abort` **only when rebase state exists**: resolve both `git rev-parse --git-path rebase-merge` and `git rev-parse --git-path rebase-apply`, then test whether either directory exists (in a linked worktree `.git` is a gitfile, so literal `.git/rebase-*` paths would be wrong; the worktree-local state lives under the resolved git dir). A failing `pre-rebase` hook occurs before that state exists, so abort is skipped and the hook error is preserved; **squash** has no `MERGE_HEAD`, so its conflicts or commit/hook failures reset to the `/base` OID (`git reset --hard <base-oid>`). All failure paths then run `git clean -fd` in the disposable worktree (removes untracked merge-driver/hook artifacts) before forced worktree removal (`git worktree remove --force`). In all cases: append no `pr.merge` event, update neither `refs/heads/<base>` nor the PR chain ref, and exit non-zero. The failure leaves the PR unchanged: fix the source branch and run `pr update` (a new immutable generation), re-run `ci run`, and re-approve — a moved base additionally goes through the prepared-candidate flow (`pr prepare`) above.
- **Merge execution (L1, legacy path)**: `git forge pr merge` performs the merge in a **temporary worktree**: for merge/squash/**ff**, detached at the CURRENT base tip (`git worktree add --detach <tmp> <base-tip>`; the immutable snapshot `base_head` when the base has not moved — a moved base never reaches this path, it requires the prepared candidate above) — for **ff**, run `git merge --ff-only <source-oid>` and take its resulting HEAD as `result_commit`; for **rebase**, detached at the immutable generation-current `/source` OID, run `git rebase --onto <current base tip> <merge_base>`, and take its resulting HEAD as `result_commit`. The user's current branch/worktree is untouched and the merge targets the correct snapshot. Temporary worktree is removed after success or failure (including conflict abort); use `git worktree remove --force` only for cleanup of the disposable worktree, never a user worktree. Checked-out-base handling follows the live-base refresh paragraph below: a clean checked-out base proceeds and its worktree is refreshed to the result commit; a dirty one refuses before any side effect.
- **Merge completion keeps `result_commit` reachable until the final transaction**: `pr.merge.body.result_commit` is only JSON, not a Git graph edge, so a GC running after the worktree is removed could prune the result commit before base CAS. Before removing the disposable worktree, create the **pending-result ref** `refs/forge/prs/<n>/result` → `result_commit` (single `git update-ref` create). Then remove and verify the disposable worktree (success path: `git worktree remove --force`, no `git clean -fd`; verify via `git worktree list --porcelain` that the path is no longer registered and the directory is gone). If removal/verification fails, report the leftover path, delete the pending-result ref, create no `pr.merge` event, update neither ref, and exit non-zero. After successful cleanup, re-read the PR chain tip and re-fold the chain; if the effective decision is no longer `approve` (a concurrent `reject`), delete the pending-result ref, create no `pr.merge`, and abort with no ref changes. Otherwise create the `pr.merge` event commit (sole parent = newest PR chain tip, `result_commit` in body). The final `git update-ref --stdin` transaction atomically performs **three** updates: delete `refs/forge/prs/<n>/result` (expected its OID), CAS `refs/heads/<base>` from the **gate-validated CURRENT base tip** to `result_commit` (the candidate path CASes from the candidate's `base_head`; the unmoved-base legacy path CASes from the snapshot `base_head` — never from a re-read tip), and CAS the PR chain ref (expected the same newest PR tip → `pr.merge` event commit). On any failure (concurrent PR append/reject, base moved, stale pending ref), the whole transaction rolls back — no "code merged but PR still open" state. After a transaction failure, git-forge **best-effort deletes the expected pending-result ref**; if that cleanup fails or the process dies before it, the ref remains and is reported in the error as a durable leftover path (`refs/forge/prs/<n>/result`), keeping `result_commit` reachable and safely GC-able by deleting the ref.
- **Test-only pending-window barrier**: to prove reachability honestly, L1 exposes `GIT_FORGE_TEST_MERGE_BARRIER=<dir>` (accepted only in test/debug builds, ignored in release; not part of the user CLI contract). Protocol: after the pending ref exists and the worktree is removed/verified, but before creating `pr.merge` or the final transaction, git-forge (1) atomically creates `<dir>/ready` via `O_CREAT|O_EXCL`, then polls for `<dir>/release` to appear (created atomically by the test with `O_CREAT|O_EXCL`) until a bounded deadline (default 30 s). The **test leaves `release` in place until git-forge deletes it**, so the poll cannot miss the signal. (2) On deadline, fail the merge: delete both `<dir>/ready` and `<dir>/release` best-effort if they exist (git-forge owns cleanup once it has entered the barrier), delete the pending ref best-effort, keep base/PR refs unchanged, and exit non-zero with a clear error. On observing `release`, git-forge deletes both `<dir>/release` (observed signal, owned by it at that point) and `<dir>/ready` (its own sentinel) best-effort, then proceeds to the final transaction. The test waits for `<dir>/ready` to appear, runs `git gc --prune=now`, verifies `git cat-file -e <result_commit>` still succeeds (pending ref is the only reachability edge) and base/PR refs are unchanged, then atomically creates `<dir>/release` and waits for git-forge to remove it; git-forge resumes and the final transaction deletes the pending ref. On every barrier exit (timeout or release), git-forge removes both sentinels if present so a deadline race cannot leave a stale `release`. Failure paths still use their abort/reset plus `git clean -fd` before forced removal, as defined above.
- **Test-only write barrier**: `GIT_FORGE_TEST_WRITE_BARRIER=<dir>` (test/debug builds only, ignored in release; not part of the user CLI contract) parks a mutation command where it resolves its identity, which is after its read-only snapshot and before its first forge write. It uses the same `<dir>/ready` / `<dir>/release` protocol and 30 s deadline as the pending-window barrier; on deadline the command fails with no ref updates. Tests use it to append a concurrent event between a command's snapshot read and its own append.
- **Checked-out base guard + live-base refresh (L1)**: when `base_ref` is checked out in a non-temporary worktree (the main worktree or a linked one), the merge no longer refuses outright. It proceeds only while every such worktree is **clean** — `git status --porcelain` (default flags) empty: modified tracked files AND untracked files count as dirty, ignored files do not; a dirty worktree refuses before any side effect, and no flag unlocks that refusal. On a clean worktree the merge proceeds and, after the base CAS moves the branch ref, the worktree tree is refreshed to the result commit (a bare `git reset --hard` in the worktree; HEAD there is attached to the moved branch, so the refresh moves no ref). Immediately before that reset the refresh **re-checks cleanliness against the pre-merge tip** (`git diff --quiet` + `ls-files --others`, so the normal pending-refresh state of "index/worktree still at the pre-merge tree" is not misreported as dirt): dirt that appeared in the guard→refresh window refuses, the whole completion rolls back, and the uncommitted work survives. **Never-split posture**: if the refresh fails, the whole completion rolls back — base branch CAS'd back to the snapshot `base_head`, PR head chain restored, pending-result ref cleaned best-effort — so no ref shows a moved state with a stale tree, and the failing worktree is reported as a leftover. When the PR declares `--closes <n>` and the same transaction closed the linked issue, the rollback re-anchors `refs/forge/issues/<n>` in the SAME all-or-nothing batch, CAS'd FROM the `issue.close` commit the transaction applied (its actual current value, F-001) TO the pre-close tip — a `--closes` PR whose refresh fails therefore leaves the issue open, base/PR/issue refs unchanged, and the base worktree consistent (VAL-107).
- **Source-worktree recycle (`--recycle-worktree <path>`, L1)**: after the base refresh and the finalize transaction, `git forge pr merge <n> --recycle-worktree <path>` removes the merged source worktree and conditionally deletes its branch. **Pre-merge validation**: the path must be a REGISTERED worktree whose checked-out branch is the PR's `source_ref`, and the worktree must be clean (`git status --porcelain` default flags: untracked counts as dirty, ignored does not) — otherwise the merge refuses BEFORE any side effect (PR unmerged, refs unchanged). `--force` unlocks ONLY the dirty refusal (never containment, never the base dirty-worktree refusal; `--force` without `--recycle-worktree` is a usage error). When the recycle epilogue itself fails **after** a terminal merge (phase-2 refused-dirty, partial-leftover), the command exits non-zero but the error output still carries the `merged PR #<n> into <base> (<sha>)` line — the adapter classifies these as `worktree_partial_removal`, never as "PR NOT merged". **Post-merge removal**: because dirt can appear in …the validate→remove window (TOCTOU, F-003), phase 2 RE-RUNS the dirty check on the target immediately before the removal and refuses with `refused-dirty` — the worktree and its uncommitted changes stay untouched, the merge itself stays terminal — unless `--force` was passed, in which case the removal proceeds. The validated worktree is removed (`git worktree remove --force`) and verified gone (directory absent + unregistered); a removal that leaves either behind reports `partial-leftover(<path>)` naming the exact path. **Branch deletion (VAL-111)**: `--squash`/`--rebase` synthesize a new commit, so the branch is always KEPT with `branch-kept(<reason>)` naming the strategy; `--ff`/`--merge` delete the branch only when its live tip still equals the PR's snapshot `source_head` AND that tip is an ancestor of the integrated result commit (`git merge-base --is-ancestor`) — a moved branch holds unmerged work and is kept (salvage renaming is pipeline-side and runs BEFORE this call, adapter contract clause 8). The recycle NEVER touches the base worktree (it runs after the #3 refresh, which leaves the base clean at the result commit). **Outcome vocabulary** (forge→adapter contract, consumed verbatim): `removed | refused-dirty | refused-not-contained | partial-leftover(<path>) | branch-deleted | branch-kept(<reason>)`.

### CI (on-demand, L1)

- **`git forge ci run <pr>`** executes the repo CI plan against the PR's **current generation's immutable source snapshot** (`refs/forge/prs/<n>/source` for generation 0, `refs/forge/prs/<n>/gen/<k>/source` for generation k) in a temporary worktree, so it validates exactly the PR's own commits; the developer's working tree and current branch are untouched. When the PR carries a CURRENT prepared candidate (see PR Lifecycle), the run instead validates the CANDIDATE RESULT commit and binds the candidate identity. The plan is `.forge/ci.sh` from the run OID's tree when it is a **regular file**; a `.forge/ci.sh` that is a symlink or otherwise not a regular file is **refused up front** — the plan is resolved before the temporary worktree is created, so the command errors before execution and before any `ci.check` is appended (no Check result records the refusal). When `.forge/ci.sh` is absent, the `just check` fallback runs. The command appends a `ci.check` event recording the outcome (`status`: `success` when the plan exits 0, `failed` otherwise), the `plan` that ran, the `generation` captured at run start, and the live `base_head` — even a failing plan records a `failed` Check before the command exits nonzero — then cleans up the temporary worktree. The captured generation makes an in-flight Check that appends after a concurrent `pr update` stale (never green for the new generation).
- **Cleanup-before-green (F-025)**: `git forge ci run` records `success` only after the temporary worktree removal is VERIFIED (directory gone + path no longer registered). A passing plan whose worktree cannot be removed is recorded as `failed` and the command exits nonzero, so the merge gate cannot accept a green Check whose CI run actually failed integrity.
- **Pending marker on create**: `git forge pr create` publishes an initial `ci.check` with `status: "pending"` (no `plan` — no plan executes at creation) atomically with PR allocation, as the `/head` chain tip, so a freshly created PR surfaces a pending Check without running anything. `git forge ci run <pr>` appends a later Check, and fold keeps the latest.
- **Merge gate**: the command-level merge gate requires the latest `ci.check` to be `success` AND bound to the PR's current snapshot generation (a green Check from an older generation is stale and refuses, naming `git forge ci run <pr>` as the remedy); a `pending`/`failed`/absent Check refuses the merge before any worktree/ref side effect. On the prepared-candidate path every gate input (Check + approval) must additionally be bound to the candidate's identity. CI is purely on-demand — no server, no daemon, nothing runs unless invoked.

### Event Identity

- Every event carries an immutable UUID `id` (v4) generated at creation, independent of its commit OID. On CAS retry, the event UUID is retained even though the commit OID and parent change.
- `actor` is the invoking repo's `user.email` from git config (the email, never `user.name`); the CLI resolves it once per command via the safe git binary (`git -C <worktree> config --null --get user.email`, `crate::git::config_get_identity`) and binds the corresponding committer identity via `EventStore::bind_signature` before any write — never a libgit2 `repo.config()` read at write time (that path can SIGSEGV on a config corrupted after `Repository::open`, VAL-115 STAGE A). Read-only paths (list/show/diff) never resolve identity.
- **No-email fallback**: when `user.email` is not configured or is empty (absent, or set to an empty/whitespace value, in repo, global, or system config), the actor falls back to `forge@localhost` and commands still succeed; the store commit signature uses the same default (`git-forge` / `forge@localhost`) when name or email is unusable, and uses the configured pair when both are usable and form a valid signature. `user.name` does not control the actor.
- **Config-failure policy (two-stage, F-028)**: identity config failures are stage-specific. STAGE A — opening the repo's config at all (`git config --get` exits 128, an environment fault) **propagates** as a clean CLI error (actor resolution is impossible, and silent fallback or an empty actor would mask a broken repo); no event is written and no forge ref mutates; the resolver returns Err before any libgit2 open, so the libgit2 SIGSEGV-on-corrupt-config path is never reached. STAGE B — looking up `user.email` (absent key, empty/whitespace value, or a non-UTF-8 value) **falls back** to `forge@localhost`: no usable identity, so commands still succeed, matching the contract sentence that a repo without a usable identity is never blocked from writing events. An isolated child-process regression (`tests/t1a_store.rs` `val115_postopen_corrupt_write_does_not_segv`) proves a write after a post-open config corruption completes without SIGSEGV.
- **Per-worktree identity is a PROMISED interface (t8, #8)**: identity resolution shells out to `git -C <worktree> config` (never a parsed config file), so with `extensions.worktreeConfig = true`, `git config --worktree user.email <email>` gives each worktree its own event actor in the shared forge — three worktrees with three distinct identities produce three distinguishable actors on one repo's event chains, without touching the shared repo config. This is contractual: identity is whatever `git config` resolves in the invoking worktree, and git-forge will keep resolving it through the git binary.
- **`GIT_CONFIG_COUNT` environment override is a PROMISED interface (t8, #8)**: `GIT_CONFIG_COUNT=1 GIT_CONFIG_KEY_0=user.email GIT_CONFIG_VALUE_0=<email> git forge ...` overrides the actor (and signature email) for that one command without touching any config file — the documented per-invocation identity contract for pipelines. (Note: forge's own CI plan execution strips `GIT_CONFIG_COUNT`/`GIT_CONFIG_*` from the plan's subprocess env (F-027 tightening), so a plan's git calls never inherit the invoking command's identity override; the override is honored for the forge command itself, not inside the CI sandbox.)
- **Role semantics and the review gate (t8, #8)**: when roles are configured, `pr show` renders `created-by: <role>` and `decision-by: <role> (<decision>)` lines, and `--json` adds additive `creator_role`/`decision_role` members (null on roleless chains). The merge gate compares the effective approve's role with the `pr.created` role: an approve recorded by the SAME role that created the PR refuses the merge with a `self-approved` error naming the role — "someone other than the implementer approved" must come from a different seat. The comparison runs ONLY when both events record roles: one-sided coverage (a roleless create or a roleless approve) keeps the gate open — indistinguishability is not self-approval, so partially-configured pipelines and legacy chains behave exactly as before.
- State folding and refs identify events by UUID, not by commit hash; UUIDs never change.

### Deterministic Single-Chain CAS Append (L1)

- **L1 is a single chain, not a DAG**: every normal append creates an event commit whose sole parent is the current entity-ref tip; CAS updates the ref (`update-ref <entity-ref> <new-oid> <observed-tip-oid>`). On CAS failure, recreate the event with the new tip as sole parent while **retaining the same UUID**, then retry with a bounded retry count (default 3). If the bounded retries are exhausted, report an error and update no refs.
- Fold order is exactly newest-parent order: events oldest → tip (following sole parents; merge nodes do not exist in L1).
- During fold for issue/PR state, the last reachable `pr.review` is effective. No topological/DAG language is used in L1; multi-parent merge nodes and DAG linearization are L2 (ADR-0004).
- Non-event merge commits (L2 convergence) would be empty-tree and ignored by fold.

- Writes: CLI → store → git refs (append-only commits).
- Reads: CLI → store → fold (core) → display.
- Merge: gate checks folded PR state (approve + latest Check, both bound to the current snapshot generation; on the prepared-candidate path every gate input additionally bound to the candidate identity), then — candidate path — binds the PREPARED candidate directly (no worktree, no re-materialization; the result was committed at prepare time and is pinned at `refs/forge/prs/<n>/result`), or — unmoved-base legacy path — merges in a temporary worktree detached at the CURRENT base tip via the `git` binary (merge-commit default; `--squash`/`--rebase`/`--ff` flags), pins `result_commit` under the pending `refs/forge/prs/<n>/result`, cleans the worktree, then completes in ONE `git update-ref --stdin` transaction: (1) delete the pending `/result` ref, (2) CAS `refs/heads/<base>` from the GATE-VALIDATED CURRENT base tip (the candidate's `base_head` on the candidate path; the snapshot `base_head` on the unmoved legacy path — never from a re-read tip) to `result_commit`, (3) CAS the PR `/head` chain to the `pr.merge` event commit, and (4) when the PR declares `--closes <n>`, CAS `refs/forge/issues/<n>` onto the `issue.close` event commit in the same batch — skipped as an idempotent no-op when the target issue is already closed (an issue ref that vanished is a hard refusal, never a silent skip). All-or-nothing: if any CAS fails, nothing applies — base, PR, and linked-issue refs all unchanged — and the pending-result ref is best-effort deleted, with a surviving ref reported as a leftover.
- Sync (L2): `git forge push` sends `refs/forge/*:refs/forge/*` explicitly; `git fetch`/`git pull` receive forge refs into the remote-tracking namespace via the additive fetch refspec installed by `git forge clone/init`; `git forge pull` then converges local `refs/forge/*` per entity.

## Decisions

Recorded in `docs/adr/`:

- 0001: Git ref event chains as authoritative storage; SQLite index is a rebuildable cache only.
- 0002: Git extension surfaces (`git forge`, wrappers, hooks), not a git fork.
- 0003: Command-level merge gate in L1; direct `git merge` bypass documented; bare-remote pre-receive enforcement deferred to L2.
- 0004: Forge refs sync via remote-tracking namespace and explicit convergence — L2 (not in L1 scope).
- 0005: Rust + libgit2, MIT.
- 0006: L1 PRs are immutable snapshots per generation; `pr.update` (t1) re-pins the PR to the source branch tip as a NEW immutable generation (never rewriting existing ones) and invalidates older-generation approvals/CI.

## Open Questions

- L2: DAG-merge convergence details (remote-tracking namespace, refspec wiring, merge-node fold) — ADR-0004.
- L2: bare-remote branch protection semantics (which refs protected, who approves).
- L2: single-file export format (`git forge export project.forge`) — SQLite snapshot vs bundle.
- L2: web UI surface (read-only browse vs write paths).

## Known Limitations

- The L1 merge gate is command-level only; direct `git merge` bypasses it. Bare-remote pre-receive enforcement is L2 (ADR-0003).
- In L1 single-user operation the Review actor is the repository's own `user.email`, so an `approve` records that the pipeline's review step passed — it does not attest independent human review. See `docs/decisions/implemented/process/forge-review-approval-is-orchestration-ceremony.md`.
- L1 PRs are immutable snapshots per generation (ADR-0006 as amended by t1): `pr.update` extends the PR with a new immutable generation but never rewrites history; there is no arbitrary history edit/squash of an existing PR.
- Live-base merge (t2) requires the `pr prepare` candidate flow: a moved base without a prepared candidate refuses naming `pr prepare`; the prepared candidate binds the merge result, and `pr update` invalidates it (re-prepare remedy). The result-bound approval/CI gate applies only to the candidate path; the unmoved-base legacy path keeps the generation-stamped review/CI binding semantics (legacy-path approvals carry generation + live `base_head`, but no candidate identity).
- The candidate pin (`refs/forge/prs/<n>/result`) is reused by the prepared candidate and the transient pending-result ref; a prepared candidate is consumed (ref deleted) by the merge final transaction.
- `pr.prepared` / candidate binding is local to this single-user tool: no cross-clone synchronization of candidates yet (L2 sync out of scope).
- Identity is resolved through the `git` binary before any write, never a libgit2 `repo.config()` read — that path can SIGSEGV on a config corrupted after `Repository::open` (VAL-115). The trade-off is git-binary spawn cost per write.
- `git forge pr merge` merges while `base_ref` is checked out only when every such worktree is clean (untracked counts as dirty, ignored does not); the clean worktree is refreshed to the result commit after the base CAS, and a refresh failure rolls the refs back and reports the worktree as a leftover. **Sparse-checkout and submodules in a base worktree are out of L1 scope**: behavior with either present is not covered by the clean/refresh contract (the porcelain cleanliness check and the bare `reset --hard` refresh make no promises about sparse index state or submodule contents) and may refuse or mis-refresh.
- The test-only `GIT_FORGE_TEST_MERGE_BARRIER` and `GIT_FORGE_TEST_WRITE_BARRIER` seams exist only in test/debug builds and are ignored in release.
- The CI output tail is diagnostic text, not an exact byte record: bytes that are not valid UTF-8 become U+FFFD, so distinct invalid bytes look the same. A plan descendant that leaves the process group (for example with `setsid`) can keep the output pipe open; `ci run` then waits at most about one second for the pipe to close and records the tail read so far.
