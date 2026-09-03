# Flow: forge-PR-abandon

## Scope
- Actor: CLI operator (human or agent) of git-forge. No server, no authentication, no network.
- Surface: cli
- App: git-forge — local, git-native forge; state lives as event chains under `refs/forge/*` inside an ordinary git repository.
- Entrypoint: `git forge pr abandon <n> --reason <text>` — the PR enters a terminal state; every subsequent mutation refuses.
- Related path patterns:
  - `src/cli.rs` — `pr abandon` command wiring.
  - `src/merge_state.rs` — `validate_merge_gate_at`: the abandon refusal fires BEFORE the approve/CI gates and before the checked-out-base guard / worktree creation, so refused commands leave no side effects.
  - `src/event.rs` — `pr.abandoned` event kind (F-013/F-014 wire contract) and the `abandoned: <reason>` fold into `pr show`.
  - `tests/t3_merge.rs` — the abandon boundary matrix (refusals of merge/review/ci/second-abandon/merged-PR).
  - `scripts/gf-dogfood.sh` — the dogfood oracle VAL-005..VAL-010 section.
  - `docs/architecture/git-forge.md` (normative wire contract) and `CONTEXT.md` (glossary).
- Proven by: a-final acceptance sweep, VAL-005..VAL-010, all `pass` at forge source commit `125dba3`.

## Driver contract for future runs
- Start/health:
  - Build the binary fresh from the checkout under test — NEVER reuse a workspace `target/release/git-forge` (it predates P0 and lacks the `abandon` subcommand — phantom failures). Controlled target dir + private bin dir, `export PATH="<tmp>/bin:$PATH"`.
  - Toolchain on this machine: every cargo/commit command needs `PATH="$HOME/.cargo/bin:$PATH" RUSTUP_TOOLCHAIN=1.93.0-aarch64-apple-darwin` (the default brew cargo SIGABRTs, rc=134).
- Seed/auth:
  - Disposable clone of a real project (`mktemp -d`, trap cleanup) — never the live worktree. Resolve the default branch dynamically: `BASE="$(git -C "$SRC" branch --show-current)"`; never hardcode a branch name.
  - `git config user.email` is set: the author is recorded on every forge event.
  - The seed PR needs only one change commit on the feature branch; **no `.forge/ci.sh` is required** — the abandoned refusal precedes the gate checks, so the PR can stay at `ci: pending`.
  - Issue/PR ids come from ONE shared sequence; never hardcode ids — parse them from the create output.
- Primary action: create a PR → `pr abandon <n> --reason <text>` → the PR folds to `abandoned: <text>`; then `merge` / `review --approve` / `ci run` / a second `abandon` all refuse with non-zero exit naming the original reason, and the event chain gains no events.
- State oracle:
  - `git forge pr show <n>` — the `abandoned: <reason>` line (first reason wins), plus `no review yet` / `ci: pending` / `merged: <sha>` as the refusal leaves them unchanged.
  - Event chains (read-only observation, never write): `git log --format=%s refs/forge/prs/<n>/head | grep -c forge:pr.abandoned:<n>` (exactly 1 after abandon, still 1 after a second attempt, 0 for a merged PR); `grep -c forge:ci.check:<n>` unchanged after a refused `ci run`.
  - Refusal stderr: `git-forge: PR #<n> is abandoned: <reason>`; second abandon: `git-forge: PR #<n> is already abandoned: <reason>` (the ORIGINAL reason, never the new one); merged PR: `git-forge: PR #<n> is already merged`.
- Evidence sinks: one transcript per scenario (command, stdout, stderr verbatim on refusals, exit code); `pr show` snapshots; event-chain counts before/after; a run-log summary line.
- Cleanup/isolation: disposable clone removed on exit; forge refs observed (`git log`) or driven through the CLI only; refusals pre-empt the base-guard/worktree path, so a refused merge leaves no temp worktree and no pending ref.
- Forbidden boundary: never hand-craft or rewrite `refs/forge/*`; abandonment has no reopen path (no `pr reopen` subcommand) — the only forward move is a brand-new PR; no `git push`/`pull` (forge state is not synced in L1).

## Future run steps
1. Fresh build with the rustup env, then `export PATH="<tmp>/bin:$PATH"`.
2. `git clone -q "$SRC" <tmp>/work`; `BASE="$(git -C <tmp>/work branch --show-current)"`; `cd <tmp>/work`.
3. `git checkout -B feat/ab "$BASE"`; one change commit (`git add -A && git commit -qm "abandon c1"`); `git checkout -q "$BASE"`.
4. `git forge pr create --source feat/ab --base "$BASE" "abandon PR"` → exit 0, `PR #<n> created: ...`; parse `<n>` with `sed -n 's/.*PR #\([0-9]*\) created:.*/\1/p'`. Record `abandoned-events-before=$(git log --format=%s refs/forge/prs/<n>/head | grep -c forge:pr.abandoned:<n>)` (0).
5. `git forge pr abandon <n> --reason "not needed anymore"` → exit 0, `PR #<n> abandoned: not needed anymore` (VAL-005).
6. Assert: `git forge pr show <n>` exits 0 and its output contains `abandoned: not needed anymore`.
7. `git checkout -q feat/ab`; `git forge pr merge <n>` → exit 1, stderr `git-forge: PR #<n> is abandoned: not needed anymore` (VAL-006); `git checkout -q "$BASE"`; `git forge pr show <n>` still shows `abandoned: not needed anymore`.
8. `git forge pr review <n> --approve` → exit 1, stderr names the original reason (VAL-007); `git forge pr show <n>` still shows `no review yet`.
9. Record `ci-check-before=$(git log --format=%s refs/forge/prs/<n>/head | grep -c forge:ci.check:<n>)`; `git forge ci run <n>` → exit 1, stderr names the original reason (VAL-008); assert the count is unchanged after; `git forge pr show <n>` still shows `ci: pending`.
10. `git forge pr abandon <n> --reason again` → exit 1, stderr `git-forge: PR #<n> is already abandoned: not needed anymore` (VAL-009); `abandoned-events-after=$(git log --format=%s refs/forge/prs/<n>/head | grep -c forge:pr.abandoned:<n>)` still 1; `git forge pr show <n>` still shows the FIRST reason, not `again`.
11. Merged-PR negative (VAL-010): on an already-merged PR `<m>` (created/approved/ci/merged in a separate scenario), `git forge pr abandon <m> --reason late` → exit 1, stderr `git-forge: PR #<m> is already merged`; `git log --format=%s refs/forge/prs/<m>/head | grep -c forge:pr.abandoned:<m>` stays 0; `git forge pr show <m>` still shows `merged: <sha>`.

## Blocking checks for future runs
- The binary under test is freshly built from the checkout under test (a stale binary lacks the `abandon` subcommand — check `git forge pr abandon --help` exits 0 / `git-forge --help` lists `pr abandon`).
- `git forge pr abandon <n> --reason <text>` exits 0 and prints `PR #<n> abandoned: <text>`; `git forge pr show <n>` contains `abandoned: <text>` verbatim.
- `git forge pr merge <n>`, `git forge pr review <n> --approve`, `git forge ci run <n>` on the abandoned PR each exit 1 and their stderr names the original abandon reason verbatim.
- A second `git forge pr abandon <n> --reason <other>` exits 1, stderr says `already abandoned: <original reason>`, and the abandoned-event count stays 1.
- `pr show` after any refusal shows the exact pre-refusal snapshot lines (`abandoned: ...`, `no review yet`, `ci: pending`, `merged: ...`) — nothing advanced.
- Event-chain counts are equal before/after every refused command (`grep -c` on `refs/forge/prs/<n>/head` for `forge:pr.abandoned:<n>` and `forge:ci.check:<n>`).
- `git forge pr abandon` on a merged PR exits 1 with `already merged` and appends no event.

## Negative / boundary checks
- `git forge pr abandon <n>` without `--reason` refuses (exit 1, missing/empty reason); a whitespace-only reason also refuses.
- `git forge pr abandon <unknown-id> --reason x` refuses with non-zero exit.
- No reopen: there is no `pr reopen` / undo path; after abandon the PR is terminal — the only way to proceed with the work is a new PR.
- All refusals precede any side effect: they can be run from any checkout, leave no temp worktree / pending ref, and the event chain stays byte-identical (checked via counts above).

## Evidence expected
- One transcript per scenario (VAL-005..VAL-010) with a header naming the binary path, the forge source commit, and the date; every command with its stdout, stderr, and exit code.
- `git forge pr show <n>` snapshots: `abandoned: not needed anymore` after abandon; unchanged `no review yet` / `ci: pending` after refusals; first reason retained after a second abandon; `merged: <sha>` for the merged-PR refusal.
- Verbatim stderr of each refusal: `git-forge: PR #7 is abandoned: not needed anymore`, `git-forge: PR #7 is already abandoned: not needed anymore`, `git-forge: PR #2 is already merged`.
- Ref-state counts: `git log --format=%s refs/forge/prs/<n>/head | grep -c forge:pr.abandoned:<n>` (1 after abandon, 1 after second abandon, 0 for merged) and `grep -c forge:ci.check:<n>` unchanged after a refused `ci run`.
- A run-log ending line `ALL <k> ASSERTIONS EXERCISED OK`.

## Known blockers
- No `.agents/qa/config.yaml` or command façade (`qa-e2e-*` targets) exists in this repo yet; these two contracts are standalone and not yet referenced by an install-e2e QA map — wiring them is a separate install feature, not part of this contract.
- Machine-toolchain quirk: default brew cargo SIGABRTs; all cargo/commit commands must carry the rustup env (`PATH="$HOME/.cargo/bin:$PATH" RUSTUP_TOOLCHAIN=1.93.0-aarch64-apple-darwin`).
- The workspace `target/release/git-forge` is stale relative to the P0 features — always build fresh from the checkout under test.
- VAL-010 needs a MERGED PR as input, which requires the full merge-capable setup (approve + green `.forge/ci.sh` + non-base checkout), a distinct fixture from the abandon-only scenario.
