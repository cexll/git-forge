# Flow: forge-pr-update

## Scope
- Actor: CLI operator (human or agent) of git-forge running a fix round against an already-open PR.
- Surface: cli (PR snapshots, generations, the merge gate's generation binding).
- App: git-forge — local, git-native forge; PRs are per-generation immutable snapshots (ADR-0006), state lives as event chains under `refs/forge/*`.
- Entrypoint: `git forge pr update <n>` (re-pin to the source branch tip as a NEW generation), then `pr review <n> --approve` / `ci run <n>` / `pr merge <n>` for the stale-gate legs.
- Related path patterns:
  - `src/cli.rs` — `pr update` command surface (option-less grammar; unknown flags refused with usage).
  - `src/store.rs` — `pr.update` event append + `refs/forge/prs/<n>/gen/<k>/source|base` pinning in the same transaction.
  - `src/fold.rs` — generation tracking, effective review/CI binding to the CURRENT generation.
  - `src/merge_state.rs` — the gate's generation comparison (stale approve, stale CI Check).
  - `tests/t3_merge.rs` — the `update` filter (8 tests: `approve_before_update_cannot_merge_post_update_content`, `ci_green_before_update_cannot_merge_post_update_content`, ...).
  - `docs/architecture/git-forge.md` — PR Lifecycle (immutable snapshot, per-generation refs, `pr.update` body fields) and the Effective review decision / CI merge-gate paragraphs.
- Proven by: the P2 sweeps (v-p2 + a-final), VAL-205 `pass` (live re-pin: gen0 → gen1, generation refs pinned, old snapshot untouched, full old/new event body; committed `update` filter 8/8) and VAL-206 `pass` (stale approve refused naming generations; fresh approve + STALE CI refused; fresh CI completes; both legs committed-test-pinned). Evidence: `.specs/forge-p2/forge-p2/missions/evidence/v-p2/VAL-205-pr-update-repin.txt`, `VAL-206-stale-approve-leg.txt`, `VAL-206-stale-ci-leg.txt`, `VAL-205-committed-tests.txt`.

## Driver contract for future runs
- Start/health:
  - Build the binary fresh from the checkout under test; toolchain: `export PATH="$HOME/.cargo/bin:$PATH" RUSTUP_TOOLCHAIN=1.93.0-aarch64-apple-darwin` (brew cargo SIGABRTs). Record binary path + forge commit.
- Seed/auth:
  - Disposable scratch repo (`mktemp -d`, trap cleanup); `.forge/ci.sh` (`#!/bin/sh\necho ok`) on base; a feature branch with a gen0 commit; `pr create --source <feature> --base <base> <title>`; record gen0 `source_head`/`base_head` via `pr show <n> --json` (`.generation` = 0) and `git rev-parse`.
  - Fix round: a second commit on the feature branch, giving a new tip ≠ gen0 `source_head`.
- Primary action: `pr update <n>` on a gated gen0 PR; assert the re-pin (new generation refs, unchanged gen0 refs, event body); then drive the two stale-gate refusals (pre-update approve; pre-update CI with fresh approve) and the recovery (fresh gen1 CI + fresh gen1 approve → merge).
- State oracle:
  - Stdout: `PR #<n> updated to snapshot generation <k> (source <branch> -> <new_source_head>)`, exit 0.
  - Refs: `refs/forge/prs/<n>/gen/<k>/source` == the new tip, `gen/<k>/base` == base_head; gen0 `refs/forge/prs/<n>/source` and `/base` UNCHANGED (immutable, never rewritten).
  - `pr show <n>`: `generation: <k>` line; `pr show <n> --json` `.generation` == k with the new snapshot heads.
  - Event body (`git show <sha>:.forge/event.json`, kind `pr.update`): all four old/new pairs — `old_source_head`, `new_source_head`, `old_base_head`, `new_base_head`, `old_merge_base`, `new_merge_base` — plus `generation` == k.
  - Gate refusals: stale approve → `approve is stale (decision on generation <old>, current snapshot generation <new>); re-review after \`pr update\`` exit 1; stale CI → `latest CI Check is stale (run on generation <old>, current snapshot generation <new>); run \`git forge ci run <n>\`` exit 1. Both leave base and PR-head refs unchanged and `pr show` with no `merged:` line.
- Evidence sinks: per-step transcripts (stdout/stderr verbatim, exit codes), ref ledgers (gen0 + genk source/base before/after), `pr show` prose + JSON snapshots, the raw `pr.update` event body, committed-test output.
- Cleanup/isolation: scratch repo removed on exit; ids from command output (shared lazy counter), never hardcoded.
- Forbidden boundary: `pr update` takes NO options (`git forge pr update <n> --bogus` → exit 1 with the usage line naming the grammar); never rewrite an existing generation's refs (immutability is the contract); a single `pr update` never merges anything — gate inputs from the old generation are invalidated, not carried.

## Future run steps
1. Fresh build; scratch repo; seed gen0 PR (approved + CI green); record gen0 `source_head`/`base_head` and PR generation 0 from `pr show --json`.
2. VAL-205 (re-pin): commit the fix on the feature branch; `git forge pr update <n>` → exit 0, stdout names generation 1 and the new source head. Assert: `refs/forge/prs/<n>/gen/1/source` == new tip; `gen/1/base` == base_head; gen0 `/source` and `/base` byte-unchanged; `pr show <n>` renders `generation: 1` and the new diff (`base_head...new_tip`); `pr diff <n>` contains the fix-round change; the raw `pr.update` event body carries generation 1 and all four old/new pairs with `old_source_head` == gen0 and `new_source_head` == tip.
3. VAL-206 leg A (stale approve): approve on gen0 + CI green on gen0, then `pr update` to gen1, then `pr merge <n>` → exit 1 `approve is stale (decision on generation 0, current snapshot generation 1); re-review after \`pr update\``; base unchanged; PR unmerged. Recovery: fresh gen1 CI + fresh gen1 approve → `pr merge <n>` → exit 0 `merged PR #<n> into <base> (<sha>)`; the post-update content IS in the merged base.
4. VAL-206 leg B (stale CI under fresh approve): CI green on gen0, `pr update` to gen1, fresh gen1 approve ONLY (CI still gen0), `pr merge <n>` → exit 1 `latest CI Check is stale (run on generation 0, current snapshot generation 1); run \`git forge ci run <n>\``. Fresh gen1 CI → merge exit 0; post-update content in the merged base.
5. Committed guards: `cargo test --test t3_merge update` → 8 passed / 0 failed, including `approve_before_update_cannot_merge_post_update_content` and `ci_green_before_update_cannot_merge_post_update_content`.

## Blocking checks for future runs
- `pr update <n>` exits 0 and re-pins the PR as a NEW immutable generation; the new `gen/<k>/source|base` refs equal the branch tip and base_head; ALL prior generation refs are byte-unchanged.
- The `pr.update` event body carries `generation` and all four old/new pairs; `old_*` values equal the previous generation snapshot.
- A pre-update approve cannot merge post-update content: refusal names BOTH generations and the re-review remedy; refs unchanged.
- A pre-update CI Check cannot satisfy the gate after the update — even with a FRESH approve: refusal names both generations and the `ci run` remedy; fresh gen1 CI then completes the merge with the fresh approve.
- Each gate input (approve, CI Check) is invalidated independently — fixing one does not clear the other's staleness.
- Unknown flags on `pr update` exit 1 before any write (the surface is option-less).

## Negative / boundary checks
- `pr update` on the SAME tip still writes a new generation event and pins new gen refs (generation advances; content identity is not deduplicated) — assert the generation number moved, not the diff.
- Gen0 refs survive branch deletes and gc (the snapshot pins keep commits reachable) — deleting the feature branch after update must not break `pr show`/`pr diff` of the older generation.
- A merge refusal after `pr update` leaves the PR's snapshot fully intact: generation still k, gen refs unchanged, review/CI events unchanged (stale, not deleted).
- The stale-approve remedy text is the remedy for the OTHER gate input only when that input is the stale one — never conflate the two refusal lines.

## Evidence expected
- The re-pin transcript: `pr update` stdout with generation + new head, exit 0.
- Ref ledger: `git rev-parse` values of `gen/<k>/source`, `gen/<k>/base`, gen0 `/source`, `/base` before/after (old unchanged, new pinned).
- `pr show <n>` prose (`generation: 1`, new diff range) and `--json` (`.generation`, new heads) snapshots; `pr diff` containing the fix.
- The raw `pr.update` event body JSON with generation + four old/new pairs.
- Both refusal transcripts verbatim (stale approve; stale CI under fresh approve) with exit 1 and the recovery transcripts ending in `merged PR #<n> into <base> (<sha>)` with the post-update content confirmed in the base.
- The committed-test output (`update` filter, 8/8).
- A run-log ending line `ALL <k> ASSERTIONS EXERCISED OK`.
