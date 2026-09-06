# Flow: forge-live-base-merge

## Scope
- Actor: CLI operator (human or agent) of git-forge merging a PR after the base branch has advanced.
- Surface: cli (moved-base routing, prepared candidates, result-bound gates, finalize CAS).
- App: git-forge — local, git-native forge; state lives as event chains under `refs/forge/*`; candidate pin at `refs/forge/prs/<n>/result`.
- Entrypoint: `git forge pr merge <n>` (moved-base refusal), `git forge pr prepare <n> --strategy <merge|squash|rebase|ff>`, result-bound `git forge ci run <n>` + `git forge pr review <n> --approve`, then `pr merge <n>` again; CAS injection via `GIT_FORGE_TEST_MERGE_BARRIER=<dir>` (debug builds only).
- Related path patterns:
  - `src/pr_merge.rs` — merge orchestration: live-base resolution, moved-base candidate routing, temp-worktree lifecycle.
  - `src/merge_state.rs` — gate predicate (every gate input bound to the candidate identity: strategy/result_commit/result_tree), finalize transaction (candidate bind, delete `/result`, CAS `refs/heads/<base>`).
  - `src/ci.rs`, `src/fold.rs` — candidate-bound `ci.check` / `pr.review` events (generation + live base_head + candidate identity).
  - `tests/t3_merge.rs` — the `live_base` filter (4 tests incl. `live_base_merge_refuses_then_passes_after_fresh_gate`, `live_base_merge_requires_each_gate_input_rebound`) and the CAS suite (`ff_finalize_base_cas_clash_leaves_no_partial_write`, `live_base_finalize_cas_conflict_leaves_no_partial_write`, `finalize_cas_failure_leaves_linked_issue_open`, `merge_issue_cas_exhaustion_keeps_issue_open`).
  - `docs/architecture/git-forge.md` — Live-base merge, `pr prepare`, moved-base routing, test-only pending-window barrier (normative).
- Proven by: the P2 sweeps (v-p2 + a-final), VAL-207 `pass` (two-PR scenario: moved-base refusal naming `pr prepare` remedy → prepared candidate against the current tip → result-bound CI + approve → merge == candidate) and VAL-208 `pass` (live CAS injection via the barrier: refusal `prepared candidate is stale (built against base tip <T1>, current base tip of '<base>' is <T2>)`, exit 1, zero partial writes; committed CAS tests 4/4). Evidence: `.specs/forge-p2/forge-p2/missions/evidence/v-p2/VAL-207-live-base-two-pr.txt`, `VAL-207-committed-tests.txt`, `VAL-208-cas-conflict-live.txt`, `VAL-208-cas-committed-tests.txt` (and the a-final counterparts).

## Driver contract for future runs
- Start/health:
  - Build the binary fresh from the checkout under test, DEBUG for any injection scenario — the barrier seam is `#[cfg(debug_assertions)]`: a release binary silently ignores it and the injection "passes" for the wrong reason or hangs. Toolchain: `export PATH="$HOME/.cargo/bin:$PATH" RUSTUP_TOOLCHAIN=1.93.0-aarch64-apple-darwin`. Record binary path + forge commit.
- Seed/auth:
  - Disposable scratch repo (`mktemp -d`, trap cleanup); `.forge/ci.sh` on base; TWO feature branches forked from the SAME base commit, each with its own committed plan; `pr create` both BEFORE any merge (the proven run's first attempt created PR2 after merging PR1, so its snapshot base was already the moved tip and the merge correctly took the unmoved legacy path — a probe-order bug, not a product defect; create both PRs first).
  - Gate both PRs green on gen0 (approve + `ci run`). Shared lazy counter: parse ids from output, never hardcode.
- Primary action: merge PR1 (advances the base); attempt PR2 merge → moved-base refusal; `pr prepare 2 --strategy merge` against the CURRENT tip; candidate-bound CI + candidate-bound approve; merge → result == candidate. Then the CAS leg: prepare, gate, start merge with the barrier, inject a base advance while parked, release, assert refusal + zero partial writes.
- State oracle:
  - Refusal: `base branch '<base>' has moved since the PR snapshot (<new_tip> != snapshot <snapshot_base>); prepare the merge result first: run \`git forge pr prepare <n> --strategy <s>\`, then \`git forge ci run <n>\` and \`git forge pr review <n> --approve\`, then merge` — exit 1, PR head ref unchanged, unmerged.
  - Prepare stdout: `prepared PR #<n> for strategy <s> (candidate <result_commit>, generation <g>, base <current_base_tip>)`, exit 0; `pr show` renders an additive `prepared: strategy <s> result <hex>` line; `refs/forge/prs/<n>/result` == candidate.
  - Gate events bind the candidate: `ci.check` and `pr.review` bodies carry `strategy`, `result_commit`, `result_tree` alongside `generation` + live `base_head`.
  - Success: `merged PR #<n> into <base> (<candidate_sha>)` — result == candidate; merged base contains BOTH the concurrent base advance and the PR source; the `/result` ref is consumed (absent after merge).
  - CAS refusal: `merge execution finished but final transaction failed (git command failed: ... cannot lock ref 'refs/heads/<base>': is at <injected_tip> but expected <prepared_base>) ... refs unchanged` — exit 1; audit: base holds the injected value, PR head unchanged, 0 `pr.merge` events on the chain, `/result` ref ABSENT (deleted by the failed transaction's cleanup), `pr show` unmerged, no leftover registered worktree.
- Evidence sinks: per-step transcripts (stdout/stderr verbatim, exit codes), ref ledgers (base tip, PR head, `/result` before/after every command), the candidate-bound event bodies, `pr show` snapshots (prepared line, merged/unmerged), barrier timing notes, committed-test outputs.
- Cleanup/isolation: scratch repo + worktree root removed on exit; the barrier dir is temp + trap-cleaned (protocol: git-forge creates `<dir>/ready` O_CREAT|O_EXCL then polls for `<dir>/release` — the test writes release and leaves it until git-forge deletes it; on deadline the merge fails and deletes both).
- Forbidden boundary: the barrier seam is NOT part of the user CLI contract (test/debug only, ignored in release); never hand-move `refs/forge/prs/<n>/result` (re-prepare REPLACES the candidate via its own CAS; `pr update` invalidates the candidate with a re-prepare remedy); never re-merge PR2 with gates bound to the old base and call the legacy-path success a live-base proof (the proven run's PR2-after-merge probe order error).

## Future run steps
1. Fresh DEBUG build; scratch repo; fork two feature branches from the same base commit; `pr create` BOTH; gate both green; record the shared base tip and both PR snapshot `base_head`s (== the shared tip).
2. VAL-207 (moved-base refusal): `pr merge 1` → exit 0, base advances to T2. `pr merge 2` → exit 1 with the moved-base message naming T2 ≠ snapshot T1 and the full `pr prepare`/`ci run`/`pr review --approve` remedy; PR2 head ref unchanged; unmerged.
3. VAL-207 (prepared candidate + result-bound gates): `pr prepare 2 --strategy merge` → exit 0, stdout names candidate C1, generation 0, base T2; `refs/forge/prs/2/result` == C1; `pr show 2` shows `prepared: strategy merge result C1`. `ci run 2` → green with the candidate bound (event body carries strategy/result_commit/result_tree); `pr review 2 --approve` → bound the same way. `pr merge 2` → exit 0, merged sha == C1; base contains the concurrent advance AND PR2's source; `/result` absent.
4. Committed guards (VAL-207): `cargo test --test t3_merge live_base` → 4 passed / 0 failed, including `live_base_merge_refuses_then_passes_after_fresh_gate` and `live_base_merge_requires_each_gate_input_rebound` (each gate input must be rebound — one fresh input alone cannot pass the candidate gate).
5. VAL-208 (finalize CAS injection): seed a fresh two-PR fixture as in step 1; merge PR1; `pr prepare 2` against the current tip T1; candidate-bound CI + approve. `GIT_FORGE_TEST_MERGE_BARRIER=<dir> git forge pr merge 2` backgrounded; wait for `<dir>/ready`; inject `NEW=$(git commit-tree <base>^{tree} -m concurrent) && git update-ref refs/heads/<base> $NEW $T1`; write `<dir>/release`; wait. Assert exit 1; stderr `prepared candidate is stale (built against base tip <T1>, current base tip of '<base>' is <T2>)` (or the final-transaction `cannot lock ref` form when the injection lands in the finalize window — both name the expected vs actual tips); ref audit: base == injected value, PR head byte-identical, 0 `pr.merge` events, `/result` absent, no leftover registered worktree.
6. Committed guards (VAL-208): the four CAS tests green in a DEBUG build — `ff_finalize_base_cas_clash_leaves_no_partial_write`, `live_base_finalize_cas_conflict_leaves_no_partial_write`, `finalize_cas_failure_leaves_linked_issue_open`, `merge_issue_cas_exhaustion_keeps_issue_open` (a failed finalize leaves the linked issue OPEN — no `issue.close` survives a refused merge).

## Blocking checks for future runs
- A moved base without a current prepared candidate refuses with the `pr prepare` remedy and makes NO ref changes (PR head, base, `/result` all unchanged).
- The prepared candidate is materialized at prepare time against the CURRENT base tip and pinned at `/result`; re-prepare replaces it; `pr update` invalidates it (re-prepare remedy); prepare does NOT bump the generation.
- Every gate input on the candidate path is bound to the candidate identity (strategy + result_commit + result_tree); the merge refuses unless candidate generation == snapshot generation AND prepared base_head == current base tip AND the strategy matches `--strategy`.
- Merge on the candidate path yields exactly the candidate commit (no re-materialization at merge time); the merged base contains both the concurrent advance and the PR source; `/result` is consumed.
- CAS conflict refuses with a full no-partial-write audit: base at the concurrent/injected value, PR head unchanged, zero `pr.merge` events, `/result` absent (or the transient pending ref cleaned), no leftover worktree, linked issue still open.
- Both PRs are created off the same base BEFORE any merge — a PR created after the merge has the moved tip as its snapshot base and proves nothing about live-base routing.

## Negative / boundary checks
- The unmoved base keeps the legacy path: same-base merge with gen0 gates passes without any `pr prepare` (the proven run's corrected probe confirmed the legacy path still exists unchanged).
- A stale candidate (superseded by `pr update`) refuses with a re-prepare remedy, distinct from the moved-base remedy.
- `ff` strategy requires the source to sit directly on the current base tip; `--strategy` is required on `pr prepare` (missing strategy is a usage refusal).
- Gate rebinding is per-input: fresh candidate CI with a PRE-candidate approve refuses (and vice versa) — the committed `live_base_merge_requires_each_gate_input_rebound` leg pins this.
- A merge refusal in the finalize window must never leave the base between two values or the PR half-merged: the ref audit is the proof, the exit code alone is not.

## Evidence expected
- The two-PR transcript: both creates + gen0 gates, PR1 merge (base advance recorded), PR2 refusal verbatim with both OIDs and the remedy.
- The prepare/candidate transcript: prepare stdout (candidate, generation, base), `/result` ref value, `pr show` prepared line, candidate-bound `ci.check` + `pr.review` event bodies, merge output with result == candidate, post-merge `/result` absent, content containment check (both the advance and the source present in the merged base).
- The CAS transcript: barrier ready/release notes, the injected base value, the refusal verbatim (expected vs actual tip), and the full ref audit (base, PR head, event count, `/result`, worktrees, `pr show` unmerged).
- Committed-test outputs: `live_base` filter 4/4 and the four CAS tests (debug build).
- A run-log ending line `ALL <k> ASSERTIONS EXERCISED OK`.
