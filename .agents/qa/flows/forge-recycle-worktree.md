# Flow: forge-recycle-worktree

## Scope
- Actor: CLI operator (human or agent) of git-forge. No server, no authentication, no network.
- Surface: cli
- App: git-forge — local, git-native forge; state lives as event chains under `refs/forge/*` inside an ordinary git repository.
- Entrypoint: `git forge pr merge <n> --recycle-worktree <path>` (composable with `--ff`/`--squash`/`--rebase`/`--merge`) — after base refresh and finalize, the registered source worktree is recycled and the structured outcome line is printed for the orchestrator.
- Related path patterns:
  - `src/pr_merge.rs` — CLI-level gating; `--recycle-worktree`/`--force` flag wiring; the structured outcome line.
  - `src/merge_state.rs` — finalize transaction ordering (recycle runs AFTER base refresh + finalize); `src/store.rs` — worktree registration check, dirty check, removal, branch-deletion decision.
  - `tests/t2_pr.rs`, `tests/t3_merge.rs` — recycle refusal and removal-failure injection tests.
  - `scripts/gf-dogfood.sh` — the dogfood oracle recycle section.
  - `docs/architecture/git-forge.md` and `CONTEXT.md` (glossary).
- Proven by: a-final acceptance sweep, VAL-109..VAL-112, all `pass` (evidence: `.specs/forge-p1/forge-p1/missions/evidence/a-final/VAL-109-transcript.txt` .. `VAL-112-pl.stderr.txt`), binary built from forge @ `7871091`.

## Driver contract for future runs
- Start/health:
  - Build the binary fresh from the checkout under test. The removal-failure seam (`GIT_FORGE_TEST_FAIL_RECYCLE_REMOVE`) is `#[cfg(debug_assertions)]`: a release binary silently ignores it, so the injection scenario needs a DEBUG build. Record binary path + forge commit in a `run-paths` sink.
  - Toolchain on this machine: every cargo/commit command needs `PATH="$HOME/.cargo/bin:$PATH" RUSTUP_TOOLCHAIN=1.93.0-aarch64-apple-darwin` (the default brew cargo SIGABRTs, rc=134).
- Seed/auth:
  - Disposable scratch repo + a sibling worktree root (`mktemp -d`, trap cleanup): base repo on `<base>`, per-feature worktrees created with `git worktree add -q <root>/wt<k> -b <branch> <base>` and one change commit each — NEVER the live worktree.
  - Issue/PR ids come from ONE lazy shared counter — NEVER hardcode; parse `issue #<n> created:` / `PR #<n> created:` from command output.
  - Standard fixture helper (`mkpr`): issue + `pr create --source <branch> --base <base> --closes <issue>` + `pr review <n> --approve` + `ci run <n>`; a registered worktree whose checked-out branch == the PR source_ref is the recycle candidate.
- Primary action: merge with `--recycle-worktree <path>`; inspect the human output and assert worktree/branch/base-worktree state. Pre-merge refusals contain `recycle refused (<reason>): <detail>` and leave refs unchanged; post-merge recycle errors retain the merged prefix and do not undo the merge.
- State oracle:
  - Success line: `merged PR #<n> into <base> (<sha>); <recycle>`, where `<recycle>` is `recycle outcome: removed; branch-deleted (<branch>)`, `recycle outcome: removed; branch-kept(<branch>: <reason>)`, or, under `ff`/`merge`, `recycle outcome: removed (branch <branch> absent)`; exact retention reasons are documented in `docs/architecture/git-forge.md` under Source-worktree recycle.
  - `git worktree list --porcelain` — registration; `[ -d <path> ]` — directory existence; `git rev-parse -q --verify <branch>` — branch deletion/kept; `git status --porcelain` + `git rev-parse HEAD` in the BASE worktree — untouched invariant.
- Evidence sinks: one transcript per scenario (command, stdout, stderr verbatim on refusals, exit code); worktree-list snapshots before/after; branch rev-parse results; base-worktree porcelain/HEAD snapshots; a run-log summary of observed outcome categories, not a verbatim machine vocabulary.
- Cleanup/isolation: scratch repo + worktree root removed on exit; forge refs only observed or driven through the CLI; recycle never runs against the developer's live checkout.
- Forbidden boundary: recycle NEVER touches the base worktree (only the named source path); `--force` unlocks ONLY the dirty source tree (never a dirty base tree — that is the #3 refusal, not recyclable); `partial-leftover` means a path-level removal failure after the merge is already intact, not a merge failure; never hand-craft `refs/forge/*`.

## Future run steps
1. Fresh build with the rustup env (DEBUG for the injection step); record `run-paths`.
2. Scratch repo init; `git worktree add -q <root>/wt1 -b feat/rec <base>`; one change commit in wt1; `mkpr "rec bug" "rec PR" feat/rec` (source_ref == wt1's checked-out branch).
3. VAL-109 (removed + branch-deleted): `git forge pr merge <n> --ff --recycle-worktree <root>/wt1` from the base worktree → exit 0; output contains `recycle outcome: removed; branch-deleted (feat/rec)`. Assert: wt1 directory GONE; `git worktree list --porcelain` has no wt1 entry; `git rev-parse -q --verify feat/rec` fails (branch deleted — the source tip IS an ancestor of the integrated base after `--ff`); base worktree `status --porcelain` empty and HEAD == result commit.
4. VAL-110 (refusal surface + --force): `git worktree add <root>/wt2 -b feat/rec2 <base>` + commit; `git worktree add <root>/wt3 -b feat/wrong <base>` (wrong branch, no commits); `mkpr "rec2 bug" "rec2 PR" feat/rec2`; record base ref. Three refusal calls in order:
   a. `git forge pr merge <n> --ff --recycle-worktree <root>/nope` (unregistered path) → exit 1, output contains `refused-not-contained`; PR unmerged; base ref unchanged.
   b. `... --recycle-worktree <root>/wt3` (registered but wrong branch) → exit 1, `refused-not-contained`; wt3 still registered and present.
   c. Plant an untracked file in wt2; `... --recycle-worktree <root>/wt2` (dirty, no `--force`) → exit 1, `refused-dirty` with the remedy `commit, stash or clean it, or pass --force`; wt2 EXISTS; `feat/rec2` alive; PR unmerged; base ref unchanged.
   Then `git forge pr merge <n> --ff --force --recycle-worktree <root>/wt2` → exit 0, output `removed; branch-deleted (feat/rec2)`.
5. VAL-111 (branch deletion follows strategy): `git worktree add <root>/wt4 -b feat/keep <base>` + commit; `mkpr "keep bug" "keep PR" feat/keep`; `git forge pr merge <n> --squash --recycle-worktree <root>/wt4` → exit 0; output contains `recycle outcome: removed; branch-kept(feat/keep: the squash merge synthesized a new commit, so the branch tip is not an ancestor of the integrated result commit — the branch holds the original commits; salvage renaming is pipeline-side)`; `git rev-parse -q --verify feat/keep` still resolves. Contrast: after `--ff` (steps 3/4) the branch is deleted (`branch-deleted` + rev-parse fails) — the same ancestor rule applied to a strategy that preserves the source tip.
6. VAL-112 (partial-leftover names the path; base untouched): `git worktree add <root>/wt5 -b feat/pl <base>` + commit; `mkpr "pl bug" "pl PR" feat/pl`; record `SRC_PL=$(git rev-parse feat/pl)`; `GIT_FORGE_TEST_FAIL_RECYCLE_REMOVE=<root>/wt5 git forge pr merge <n> --ff --recycle-worktree <root>/wt5` → exit 1; stderr contains `recycle outcome: partial-leftover(<root>/wt5): worktree removal failed ...`. Assert: the merge itself stands — `<base>` == `SRC_PL` (merged source head); base worktree porcelain empty and HEAD == result commit; wt5 stays registered with its directory present (locked leftover).
7. Close the run with the observed outcome categories: across steps 3–6, removal, dirty refusal, containment refusal, partial leftover, branch deletion, and branch retention must each have evidence in the full CLI messages.

## Blocking checks for future runs
- The binary under test is freshly built from the checkout under test, and DEBUG for the removal-failure injection (a release binary silently ignores `GIT_FORGE_TEST_FAIL_RECYCLE_REMOVE`).
- A registered, clean, source_ref-matching worktree under `--ff` recycles: exit 0, `recycle outcome: removed; branch-deleted (<branch>)`, directory gone, unregistered, branch rev-parse fails.
- Every refusal below exits nonzero, prints its vocabulary token, and leaves PR/base/branch/worktree state unchanged: unregistered path → `refused-not-contained`; wrong checked-out branch → `refused-not-contained`; dirty tree without `--force` → `refused-dirty` (remedy names `--force`); with `--force` the same call proceeds to `removed`.
- Branch deletion follows the strategy: `--ff`/`--merge` delete (source tip contained in the integrated base); `--squash`/`--rebase` keep with `branch-kept(<branch>: <reason>)` naming the synthesized-commit reason and pointing salvage renaming to the pipeline side.
- `partial-leftover(<path>)` names the exact path on removal failure; the merge/finalize stays intact (base == merged source head) and the worktree stays registered.
- The base worktree is byte-untouched by every recycle call: `status --porcelain` empty and HEAD == result commit after each.
- Check the full output forms above, including the branch-absent form; the outcome categories are not an exact six-state machine vocabulary.

## Negative / boundary checks
- An unregistered path (e.g. `<root>/nope`) and a registered-but-wrong-branch path both map to `refused-not-contained` — the two causes are not distinguished at the vocabulary level, and both consume no PR state.
- `refused-dirty` leaves the worktree AND its branch intact; the PR stays unmerged and unapproved-state unchanged; the retry with `--force` is the only unlock.
- After `partial-leftover` the PR is merged but the worktree is a leftover: the record for the orchestrator is the stderr line; a subsequent run must not assume removal happened.
- Recycle ordering: refusals and removal happen after finalize — a `refused-*` outcome therefore never coexists with an unmerged PR that had already advanced refs (verify pr show shows no `merged:` after each refusal).
- `--force` does not extend to the base worktree: a dirty BASE tree still refuses under #3 regardless of `--force` (guard is the other flow's contract; never conflate).

## Evidence expected
- One transcript per scenario (VAL-109..VAL-112) with a header naming the binary path, the forge source commit, and the date; every command with stdout, stderr (verbatim on refusals), and exit code.
- Full CLI messages with their prefixes and details, including `merged PR #<n> into <base> (<sha>); recycle outcome: removed; branch-deleted (<branch>)`, `merged PR #<n> into <base> (<sha>); recycle outcome: removed; branch-kept(<branch>: <reason>)`, `merged PR #<n> into <base> (<sha>); recycle outcome: removed (branch <branch> absent)`, `recycle refused (refused-dirty): <detail>`, and `merged PR #<n> into <base> (<sha>); recycle outcome: partial-leftover(<path>): <detail>` (placeholders are substituted, not printed literally).
- Worktree ledger: `git worktree list --porcelain` before/after each call; `[ -d <path> ]` results; `git rev-parse -q --verify <branch>` results (deleted vs kept).
- Base-worktree invariants: `git status --porcelain` (empty) and `git rev-parse HEAD` (== result commit) after every recycle call.
- An outcome-category census identifying which full CLI message proved each category.
- A run-log ending line `ALL <k> ASSERTIONS EXERCISED OK`.