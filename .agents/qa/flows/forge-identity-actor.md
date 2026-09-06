# Flow: forge-identity-actor

## Scope
- Actor: CLI operators acting in the three pipeline seats — `worker`, `reviewer`, `orchestrator` — attributed via `GIT_FORGE_ROLE`.
- Surface: cli (role attribution on events, `pr show` rendered roles, `--json` role members) and the actor/identity layer (`user.email`, per-worktree config, env overrides).
- App: git-forge — local, git-native forge; state lives as event chains under `refs/forge/*` inside an ordinary git repository.
- Entrypoint: `GIT_FORGE_ROLE=<role> git forge ...` on any write command; `git forge pr show <n>` / `pr show <n> --json` for rendered roles; `pr create` → `pr review --approve` → `pr merge` for the self-approval gate.
- Related path patterns:
  - `src/identity.rs` — `resolve_identity`/`bind_identity` (actor resolved once per write via `crate::git::config_get_identity`, never a libgit2 config read); role resolution from `GIT_FORGE_ROLE` (closed set, fail-closed).
  - `src/event.rs` — additive `role` body field (schema v1 unchanged; unset role writes a byte-identical wire shape).
  - `src/merge_state.rs` — the self-approval gate (effective approve's role vs `pr.created` role).
  - `src/fold.rs` — effective review decision; `src/store.rs` — event commit signature binding.
  - `tests/t4_identity.rs` — the actor distinguishability suite (11 tests, including `three_worktrees` and the hermetic-env self-approval legs).
  - `docs/architecture/git-forge.md` — role semantics + review gate + Event Identity (normative), and `.agents/skills/git-forge/references/wire-contract.md`.
- Proven by: the P2 sweeps (v-p2 + a-final), VAL-204 `pass` in both rounds — three-role chain with role attribution on every event, unknown-role fail-closed refusal, `pr show`/`--json` role rendering, self-approval gate refusal then cross-role recovery, committed `t4_identity` 11/11. Evidence: `.specs/forge-p2/forge-p2/missions/evidence/v-p2/VAL-204-three-role-chain.txt`, `VAL-204-self-approval-gate.txt`, and `evidence/a-final/VAL-204-*.txt`.

## Driver contract for future runs
- Start/health:
  - Build the binary fresh from the checkout under test; toolchain: `export PATH="$HOME/.cargo/bin:$PATH" RUSTUP_TOOLCHAIN=1.93.0-aarch64-apple-darwin` (the default brew cargo SIGABRTs). Record binary path + forge commit.
- Seed/auth:
  - Disposable scratch repo (`mktemp -d`, trap cleanup) with a SINGLE shared `user.name`/`user.email` for the whole role chain (the proven run used `pipeline@example.com`) — role attribution must be distinguishable INDEPENDENTLY of the actor email.
  - `.forge/ci.sh` (`#!/bin/sh\necho ok`) committed on the base branch; PR source branches need their own committed plan (a source branch without `.forge/ci.sh` and without a repo justfile fails CI with `no justfile in PR snapshot for the just check fallback (F-012)` — exit 1, correct behavior, not a bug).
  - Roles are per-invocation env: `GIT_FORGE_ROLE=worker|reviewer|orchestrator` on each command; anything else is refused.
- Primary action: run a three-role chain (worker creates, reviewer approves, orchestrator runs CI), probe an unknown role, render roles, then exercise the self-approval gate (worker approves own PR → merge refuses; reviewer approves → merge passes after fresh CI).
- State oracle:
  - Event bodies: read `.forge/event.json` per commit via `git show <sha>:.forge/event.json` — every event carries `actor` (the shared email) plus `role` exactly as exported. SKIP the counter ref commit (it has no `.forge/event.json`; `git show` fails on it with a fatal path error — that is the counter commit, not a missing event).
  - `pr show <n>` renders `created-by: <role>` and `decision-by: <role> (<decision>)`; `pr show <n> --json` carries additive `creator_role`/`decision_role` members (null on roleless chains).
  - Merge gate refusals: self-approval (`PR #<n> is self-approved: the effective approve was recorded by the same role ('<role>') that created the PR`); stale CI (`latest CI Check is pending`/`stale`) — distinct refusal lines.
  - Roleless default: no `GIT_FORGE_ROLE` writes NO `role` key — the wire shape is byte-identical to the pre-role schema.
- Evidence sinks: per-step transcripts (command, stdout, stderr verbatim, exit code), the raw event-chain dump (kind/actor/role per event), `pr show` prose + JSON snapshots, the gate-refusal transcripts, the t4_identity cargo output.
- Cleanup/isolation: scratch repo removed on exit; the forge harness strips `GIT_FORGE_ROLE` AND `GIT_CONFIG_COUNT`/`GIT_CONFIG_KEY_0`/`GIT_CONFIG_VALUE_0` from the ambient env so identity injection can never mask actor assertions (hermetic t4 harness does `env_remove` on all four).
- Forbidden boundary: role is attribution provenance, NOT authentication — never assert it proves a distinct human; never set `GIT_FORGE_ROLE` in a shared profile (it leaks into unrelated commands); read-only paths (`list`/`show`/`diff`) never resolve identity.

## Future run steps
1. Fresh build; record binary + commit. Scratch repo with shared `user.email`, `.forge/ci.sh` on base.
2. VAL-204 (three-role chain, one shared actor): `GIT_FORGE_ROLE=worker git forge issue new ...` (or `pr create`) → exit 0; `GIT_FORGE_ROLE=reviewer git forge pr review <n> --approve` → exit 0; `GIT_FORGE_ROLE=orchestrator git forge ci run <n>` → exit 0. Dump the event chain skipping the counter commit: `pr.created` role=`worker`, `ci.check` role=`worker`, `pr.review` role=`reviewer`, `ci.check` role=`orchestrator` — three distinct roles, one shared `actor` email. Cross-check: `docs/architecture/git-forge.md` names the same closed set; committed `t4_identity` suite green (11 passed / 0 failed).
3. VAL-204 (unknown role fails closed): `GIT_FORGE_ROLE=impostor git forge issue new ...` (proven also with the typo `wrker`) → exit 1 BEFORE any write: `unknown GIT_FORGE_ROLE '<value>' (expected one of: worker, reviewer, orchestrator)`; the rerun afterward shows no entity created by the refused command.
4. VAL-204 (rendered roles): `git forge pr show <n>` → `created-by: worker` and `decision-by: reviewer (approve)` lines; `git forge pr show <n> --json` → `"creator_role":"worker","decision_role":"reviewer"` present alongside the other members.
5. VAL-204 (self-approval gate): `GIT_FORGE_ROLE=worker git forge pr create ...`; `GIT_FORGE_ROLE=worker git forge pr review <n> --approve` → exit 0 (the approve WRITES); `pr merge <n>` → exit 1 `PR #<n> is self-approved: the effective approve was recorded by the same role ('worker') that created the PR; the merge gate requires an approval from a different role when roles are configured`; base unchanged, PR unmerged. Then `GIT_FORGE_ROLE=reviewer git forge pr review <n> --approve` → exit 0; fresh green CI (as orchestrator); `pr merge <n>` → exit 0, `merged PR #<n> into <base> (<sha>)`. (In the proven run the fresh-CI leg initially hit the F-012 just-fallback refusal because the source branch lacked a plan — seed the plan, `pr update`, re-approve as reviewer, CI green, merge.)
6. VAL-204 (promised identity-override interfaces): per-worktree identity — with `extensions.worktreeConfig = true`, `git config --worktree user.email <email>` gives each worktree its own event actor (t4's three-worktrees test: three worktrees, three distinct actors, one repo chain). Per-invocation override — `GIT_CONFIG_COUNT=1 GIT_CONFIG_KEY_0=user.email GIT_CONFIG_VALUE_0=<email> git forge ...` overrides the actor for that one command without touching any config file.
7. VAL-204 (one-sided coverage keeps the gate open): a roleless `pr.created` with a roled approve — or the reverse — must NOT refuse: self-approval compares only when BOTH events record roles (indistinguishability is not self-approval). Committed t4 legs pin this.

## Blocking checks for future runs
- Three-role chain: every event written by a roled command carries that command's role; the actor email is IDENTICAL across all events (role ≠ actor; the proven run used one shared email).
- Unknown `GIT_FORGE_ROLE` values fail closed with the closed-set message, exit 1, BEFORE any store open or write (no event, no entity).
- The self-approval gate refuses merge (exit 1) when the effective approve's role equals the `pr.created` role, and passes once a DIFFERENT role approves plus fresh green CI on the current generation; the self-approve event itself still writes (refusal is at the gate, not at review).
- Role comparison only fires on two-sided coverage; one-sided chains keep the gate open.
- `pr show` prose renders `created-by:`/`decision-by:` and `--json` adds `creator_role`/`decision_role` — null on roleless chains, never a missing member.
- Unset `GIT_FORGE_ROLE` writes no `role` key (wire shape byte-identical to the pre-role schema).
- Committed `t4_identity` suite green; hermetic harness strips `GIT_FORGE_ROLE` and `GIT_CONFIG_COUNT`/`GIT_CONFIG_KEY_0`/`GIT_CONFIG_VALUE_0`.

## Negative / boundary checks
- An unknown role never produces a partial write or a fallback default role — the refusal is the whole interaction.
- The self-approval refusal is distinct from the stale-CI refusal (`pending`/`stale` Check messages); a self-approved PR with green CI still refuses on the role comparison, and a cross-role PR with pending CI still refuses on CI — each gate input is independent.
- A typo'd role (`wrker`) is refused exactly like an invented one (`impostor`) — no prefix/fuzzy matching.
- Reading the event chain must skip the counter ref commit; a `git show <counter-sha>:.forge/event.json` fatal error there is the counter commit, not evidence of a missing event.
- Identity resolution is stage-specific (F-028): a config-OPEN fault propagates as a clean CLI error (no event), while an absent/empty/non-UTF-8 `user.email` falls back to actor `forge@localhost` and commands still succeed.

## Evidence expected
- The raw event-chain dump: one JSON line per event with `kind`, `actor`, `role`, in chain order, counter commit explicitly skipped.
- The unknown-role refusal transcript (verbatim stderr, exit 1) plus a no-side-effect proof (no entity created).
- `pr show` prose and `--json` snapshots showing `created-by:`/`decision-by:` and `creator_role`/`decision_role`.
- The self-approval-gate transcript: worker self-approve writes → merge refusal verbatim → reviewer approve → fresh CI → merge success with the merged sha.
- The t4_identity cargo output (11 passed / 0 failed) and the hermetic-env note (stripped `GIT_FORGE_ROLE` + `GIT_CONFIG_*`).
- A run-log ending line `ALL <k> ASSERTIONS EXERCISED OK`.
