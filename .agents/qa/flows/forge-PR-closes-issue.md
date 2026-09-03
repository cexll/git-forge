# Flow: forge-PR-closes-issue

## Scope
- Actor: CLI operator (human or agent) of git-forge. No server, no authentication, no network.
- Surface: cli
- App: git-forge — local, git-native forge; state lives as event chains under `refs/forge/*` inside an ordinary git repository.
- Entrypoint: `git forge pr create --source <branch> --base <branch> <title> --closes <issue>` then `git forge pr merge <n>`.
- Related path patterns:
  - `src/cli.rs` — `pr create --closes`, `pr merge`, `issue show` command wiring.
  - `src/merge_state.rs` — merge gate (approve + green CI + non-base checkout) and the finalize transaction that closes the linked issue.
  - `src/event.rs`, `src/store.rs` — event kinds, counter CAS, ref transactions.
  - `tests/t3_merge.rs` — finalize-transaction and closes-issue tests.
  - `scripts/gf-dogfood.sh` — the dogfood oracle that proved the same behavior (s0/s1 sections).
  - `docs/architecture/git-forge.md` (normative wire contract) and `CONTEXT.md` (glossary).
- Proven by: a-final acceptance sweep, VAL-001..VAL-004, all `pass` at forge source commit `125dba3`.

## Driver contract for future runs
- Start/health:
  - Build the binary fresh from the checkout under test — NEVER reuse a workspace `target/release/git-forge` (it predates P0 and reports phantom failures). Use a controlled target dir and a private bin dir:
    `cargo build --release --locked --offline --target-dir <tmp>/target` then copy `git-forge` into `<tmp>/bin/` and `export PATH="<tmp>/bin:$PATH"`.
  - Toolchain on this machine: every cargo/commit command needs `PATH="$HOME/.cargo/bin:$PATH" RUSTUP_TOOLCHAIN=1.93.0-aarch64-apple-darwin` (the default brew cargo SIGABRTs, rc=134).
- Seed/auth:
  - Disposable clone of a real project (`mktemp -d`, trap cleanup) — never the live worktree. Resolve the default branch dynamically: `BASE="$(git -C "$SRC" branch --show-current)"`; never hardcode a branch name.
  - `git config user.email` is set: the author is recorded on every forge event.
  - Feature branches carry `.forge/ci.sh` as a regular file (not a symlink) with **byte-identical content across all feature branches** — differing bytes make the next PR merge fail with an add/add conflict (`git merge failed: ; worktree cleaned up, no ref changes made`).
  - Issue/PR ids come from ONE shared sequence; never hardcode ids — parse them from the create output.
- Primary action: create an issue → create a PR that declares `--closes <issue>` → `review --approve` → green `ci run` → `merge` from a NON-base checkout → the linked issue shows `closed`, closed in the same finalize transaction.
- State oracle:
  - `git forge pr show <n>` — the `closes: #<m>` line (present exactly when created with `--closes`).
  - `git forge issue show <m>` — status word `closed` / `open`.
  - Event chain (read-only observation, never write): `git log --format=%s refs/forge/issues/<m> | grep -c forge:issue.close:<m>` — exactly 1 close event after a fresh merge; stays at its prior count when the issue was already closed.
  - Merge stdout names the new base tip: `merged PR #<n> into <base> (<sha>)`.
- Evidence sinks: one transcript per scenario (command, stdout, stderr verbatim on refusals, exit code); `pr show`/`issue show` snapshots; event-chain counts before/after; a run-log summary line.
- Cleanup/isolation: the disposable clone is removed on exit; forge refs are only ever observed (`git log`) or driven through the CLI, never hand-written; nothing in the developer's live checkout is touched.
- Forbidden boundary: never hand-craft or rewrite `refs/forge/*`; never merge while the base branch is checked out (merge refuses); no `git push`/`pull` (forge state is not synced in L1).

## Future run steps
1. Fresh build with the rustup env, then `export PATH="<tmp>/bin:$PATH"`.
2. `git clone -q "$SRC" <tmp>/work`; `BASE="$(git -C <tmp>/work branch --show-current)"`; `cd <tmp>/work`.
3. Create the feature branch:
   `git checkout -B feat/closes "$BASE"`; make one change commit; `mkdir -p .forge && printf '#!/bin/sh\necho ok\n' > .forge/ci.sh`; `git add -A && git commit -qm "closes c1"`; `git checkout -q "$BASE"`.
4. `git forge issue new "P0 linked target"` → exit 0, `issue #<m> created: ...`; parse `<m>` with `sed -n 's/.*issue #\([0-9]*\) created:.*/\1/p'`.
5. `git forge pr create --source feat/closes --base "$BASE" "PR closes" --closes <m>` → exit 0, `PR #<n> created: ...`; parse `<n>` the same way.
6. Assert: `git forge pr show <n>` exits 0 and its output contains `closes: #<m>` (VAL-001).
7. `git forge pr review <n> --approve` → exit 0 (approve recorded).
8. `git checkout -q feat/closes` (merge gate: base must NOT be checked out).
9. `git forge ci run <n>` → exit 0, `CI run for PR #<n> passed (.forge/ci.sh)`.
10. `git forge pr merge <n>` → exit 0, `merged PR #<n> into <base> (<sha>)`.
11. `git checkout -q "$BASE"`.
12. Assert: `git forge issue show <m>` exits 0 and its output contains `closed` (VAL-002).
13. Assert (ref-state): `git log --format=%s refs/forge/issues/<m> | grep -c forge:issue.close:<m>` is exactly 1.

## Blocking checks for future runs
- The binary under test is freshly built from the checkout under test (a stale binary lacks P0 subcommands and fails steps 4-13 for the wrong reason).
- `git forge pr show <n>` contains exactly `closes: #<m>`; no PR created without `--closes` shows the line.
- `git forge pr review <n> --approve` exits 0; `git forge ci run <n>` exits 0 with the seeded green `.forge/ci.sh`.
- `git forge pr merge <n>` exits 0 and prints `merged PR #<n> into <base> (<sha>)`; the base branch is NOT checked out at merge time.
- `git forge issue show <m>` displays `closed`, and the close-event count is exactly 1.
- Every refusal below exits non-zero with a `git-forge:` message naming the failing gate, and issue state/event counts after the refusal equal the before-snapshot.

## Negative / boundary checks
- Failed merge keeps the linkee open (VAL-004): new issue `<g>`; PR with `--closes <g>`; skip approval; `git checkout -q feat/gr`; `git forge pr merge <n>` → exit 1, stderr `git-forge: PR #<n> is not approved (effective decision: none)`; `git forge issue show <g>` still shows `open`; close-event count 0. The same shape holds for a red/absent CI check and for a merge conflict (`git merge failed: ; worktree cleaned up, no ref changes made`).
- Pre-closed linkee (VAL-003): `git forge issue close <m>` before the merge; the merge still succeeds (exit 0); `issue show <m>` stays `closed`; close-event count unchanged (1 → 1, no second close event).
- `pr create --closes` to a non-existent issue refuses with non-zero exit and consumes no sequence id.
- A PR created without `--closes` shows no `closes:` line at all.

## Evidence expected
- One transcript per scenario (VAL-001..VAL-004) with a header naming the binary path, the forge source commit, and the date; every command with its stdout, stderr (verbatim on refusals), and exit code.
- A `git forge pr show <n>` snapshot containing `closes: #<m>`; a `git forge issue show <m>` snapshot showing the status word (`closed` in the happy path, `open` after a refused merge).
- Verbatim stderr of the refused merge, e.g. `git-forge: PR #6 is not approved (effective decision: none)`.
- Ref-state counts: `git log --format=%s refs/forge/issues/<m> | grep -c forge:issue.close:<m>` before and after the merge, per scenario.
- A run-log ending line `ALL <k> ASSERTIONS EXERCISED OK`.

## Known blockers
- No `.agents/qa/config.yaml` or command façade (`qa-e2e-*` targets) exists in this repo yet; these two contracts are standalone and not yet referenced by an install-e2e QA map — wiring them is a separate install feature, not part of this contract.
- Machine-toolchain quirk: default brew cargo SIGABRTs; all cargo/commit commands must carry the rustup env (`PATH="$HOME/.cargo/bin:$PATH" RUSTUP_TOOLCHAIN=1.93.0-aarch64-apple-darwin`).
- The workspace `target/release/git-forge` is stale relative to the P0 features — always build fresh from the checkout under test.
