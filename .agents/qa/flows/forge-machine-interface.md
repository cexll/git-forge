# Flow: forge-machine-interface

## Scope
- Actor: CLI operator of git-forge, plus the devflow adapter (`scripts/devflow.cjs`) consuming the machine surface.
- Surface: cli (`--json` machine output + `version` semver) and the adapter's forge plumbing (version-gate + `--json` parsing).
- App: git-forge — local, git-native forge; state lives as event chains under `refs/forge/*` inside an ordinary git repository.
- Entrypoint: `git forge version`; `git forge <cmd> --json` on the seven JSON-enabled surfaces (`issue new/list/show`, `pr create/list/show`, `ci run`); adapter exports `forgeProbeVersion` / `forgeSupportsJson` / `semverAtLeast`.
- Related path patterns:
  - `src/machine.rs` — `json_requested` (pure bare-`--json` splitter), `reject_flags` / `reject_leftover_flags` (unknown-option refusals beside the per-command parsers).
  - `tests/t3_merge.rs` — `t7_json_schemas_byte_match_fixtures`, `t7_version_prints_semver_exit_zero`, `t7_unknown_flag_still_exits_nonzero`.
  - `tests/fixtures/json/*.json` — the byte-locked schema fixtures (issue-new, issue-list, issue-show, pr-create, pr-list, pr-show, ci-run).
  - `scripts/devflow.cjs` (adapter repo `/Users/chenwenjie/.agents/skills/implement`) — `FORGE_MIN_JSON_VERSION {0,3,0}`, `forgeProbeVersion` (memoized `git forge version`), `forgeSupportsJson`, `firstJsonObject`/`firstJsonArray`, and the branch points `forgeEnsureTicketIssue` / `forgeCreatePR` / `forgePrMergedRead` / supersede check.
  - `tests/devflow/forge-adapter.test.cjs` (36 tests) + `dual-path-parity.test.cjs` (6 tests) + `fixtures/devflow/fake-git-forge.cjs`.
  - `docs/architecture/git-forge.md` (normative surface) and `.agents/skills/git-forge/SKILL.md` (help quirk documentation).
- Proven by: the P2 sweeps (v-p2 + a-final), VAL-201 `pass`, VAL-203 `pass`, VAL-202 `fail` on the unknown-flag half (finding F-001: 9 surfaces silently ignored unknown flags, exit 0 — `issue new` even created an entity) with the quirk-documentation half `pass`; F-001 fixed at forge commit `41ea9a0` where `t7_unknown_flag_still_exits_nonzero` (fixture now creates issue #1 / PR #2 first — the pre-fix test passed vacuously) asserts the flag is NAMED in the refusal on all 9 surfaces plus `pr create`/`ci run`, proves no side effect, and proves `--json` still parses everywhere; cargo test 320 passed / 0 failed, dogfood pass=141 fail=0 (6 new `--bogus` assertions). Evidence: `.specs/forge-p2/forge-p2/missions/evidence/v-p2/` and `evidence/a-final/`.

## Driver contract for future runs
- Start/health:
  - Build the binary fresh from the checkout under test — NEVER reuse a workspace binary (`~/.local/bin/git-forge` predating #7 prints `unknown command 'version'` and masks the gate). Toolchain: every cargo/commit command needs `export PATH="$HOME/.cargo/bin:$PATH" RUSTUP_TOOLCHAIN=1.93.0-aarch64-apple-darwin` (the default brew cargo SIGABRTs, rc=134).
  - Record binary path + forge commit in the run log. `git forge version` → bare semver `0.3.0`, exit 0, via BOTH dispatch forms (`git forge version` and direct `git-forge version`).
- Seed/auth:
  - Disposable scratch repo (`mktemp -d`, trap cleanup) with `user.name`/`user.email` set; entity ids come from the lazy shared counter — parse `issue #<n> created:` / `PR #<n> created:`, never hardcode.
  - Byte-stability oracle needs a PINNED-DATE git environment (the fixtures freeze `ts`); live `--json` probes run on a normal clock and intentionally differ from fixtures in CONTENT (different repo state/titles) — only the SCHEMA is byte-stable.
- Primary action: run the committed t7 byte/semver/unknown-flag tests; probe each JSON surface live; exercise the adapter's version-gate exports and a full `--json`-only cycle; drive the unknown-flag matrix on every surface with the target entities EXISTING (a missing entity masks flag handling with `does not exist` — the vacuous-test trap).
- State oracle:
  - Byte-compare: command stdout == committed fixture bytes (pinned-date scenario; two independent runs byte-identical).
  - JSON members: `issue new --json` → `{"id":1,...}` with allocated id; `pr create --json` → `id`, `source_head`, `base_head`, `merge_base`, `closes`, `decision`, `decision_role`, `merged`, `abandoned`, `ci:{status,plan}`, `generation`, `prepared`, `creator_role`; `ci run <n> --json` → `{"id":n,"status":"success","plan":".forge/ci.sh","generation":0}`.
  - Adapter gate: `forgeProbeVersion` → `{major:0,minor:3,patch:0}` / `null`; `forgeSupportsJson` → true on ≥0.3.0, false on 0.2.9 or garbage/missing; `parseSemver` accepts `0.3.0` and `git-forge 0.3.0`, rejects prose.
- Evidence sinks: per-command transcripts (stdout, stderr verbatim on refusals, exit code), fixture/live JSON snapshots, the unknown-flag exit matrix, adapter matrix + parity logs, run-log summary line.
- Cleanup/isolation: scratch repo removed on exit; adapter node tests run from `/Users/chenwenjie/.agents/skills/implement` against the fake forge fixture (never a real forge write).
- Forbidden boundary: never weaken the byte-compare into a structural diff (the fixtures byte-lock the schema); never assert `--json` parsing against a fixture that does not exist (vacuous-pass trap); the fake adapter fixture has NO `version` handler — the matrix exercises the prose path only, the JSON path is proven live against the real binary.

## Future run steps
1. Fresh build with the rustup env; record binary path + commit; `git forge version` → `0.3.0` exit 0 (both dispatch forms); `version --bogus` → exit 1 `unknown option '--bogus'`.
2. VAL-201 (byte-stable schemas): `cargo test --test t3_merge t7_json` → `t7_json_schemas_byte_match_fixtures` PASS: every JSON-enabled command's stdout byte-matches its committed fixture under `tests/fixtures/json/` (issue-new, issue-list, issue-show, pr-create, pr-list, pr-show, ci-run) in the pinned-date scenario, and two independent scenario runs are byte-identical. Plus `t7_version_prints_semver_exit_zero` PASS.
3. VAL-201 (live `--json` probes): on the scratch repo run `issue new ... --json` → exit 0, JSON carries the allocated `id`; `pr create ... --json` → exit 0 with `source_head`/`base_head`/`merge_base`/`closes` embedded; `ci run <n> --json` → exit 0 with `{"id":n,"status":"success","plan":".forge/ci.sh","generation":0}`; `issue list/show --json`, `pr list/show --json` → exit 0, structural match with the fixture schemas. Live content differs from fixtures; schema identity is proven by step 2, not here.
4. VAL-202 (help quirk): bare `git forge --help` → exit 1 `No manual entry for git-forge` (a leading flag through real git dispatch); `git forge help` → exit 0 with usage listing `version` and the quirk paragraph; direct `git-forge --help` → exit 0; the quirk paragraph also present in `.agents/skills/git-forge/SKILL.md`.
5. VAL-202 (unknown flags, post-F-001 contract): on a repo where the target entities EXIST, inject `--bogus` into EVERY command surface (`issue new/list/show/comment/close/reopen`, `pr create/list/show/comment/diff/review/abandon/merge/update`, `ci run`, `version`). Every refusal exits 1 and NAMES the flag (`unknown option '--bogus'`); `issue new t --bogus` writes NOTHING (list count unchanged); `--json=x` is refused as an unknown option with the bare-flag hint (the machine flag is bare `--json`); bare `--json` still parses on every JSON-enabled surface. Guard: `cargo test --test t3_merge t7_unknown_flag_still_exits_nonzero` PASS.
6. VAL-203 (adapter version gate + `--json` parsing), from `/Users/chenwenjie/.agents/skills/implement`: `node --test tests/devflow/forge-adapter.test.cjs` → 35 pass / 0 fail; `node --test tests/devflow/dual-path-parity.test.cjs` → 6 pass / 0 fail (prose path — the fake fixture has no `version` handler). Then live against the real binary: `forgeProbeVersion` → `{major:0,minor:3,patch:0}`, `forgeSupportsJson` → true, `semverAtLeast(0.3.0, 0.3.0)` → true, `0.2.9` → false (gate closes the JSON path); stale binary probe (`unknown command 'version'`, exit 1) → `parseSemver` null → `forgeSupportsJson` false → prose fallback.
7. VAL-203 (live adapter `--json` cycle): drive the exact adapter call shapes against the real binary — `issue new --json` (scan+create with session-label match) → `issue list/show --json` → `pr create --json --closes <issue>` → `ci run --json` → `pr show --json`; the whole cycle parses from machine JSON with no prose regex: allocated ids captured from `obj.id`, `closes` round-trips in the JSON, merged-read returns null before merge (`LIVE-JSON-CYCLE OK`).

## Blocking checks for future runs
- Byte-stability: the committed fixture byte-compare passes under pinned dates and two independent runs are byte-identical; live outputs differ in content only. Any additive `--json` member requires updating the committed fixtures (the fixtures byte-lock the schema).
- `version`: bare semver, exit 0, both `version` and `forge version` dispatch forms; any extra token (`--bogus`, positional) exits 1.
- Unknown flags exit 1 naming the flag on EVERY surface, with no side effect (`issue new --bogus` creates no entity, list counts unchanged); `--json=x` is never treated as `--json`; the unknown-flag test's fixture creates the entities it probes (a missing entity turns the assertion vacuous).
- Adapter gate: ≥0.3.0 → JSON path (ids from `obj.id`); older/garbage/missing version → prose path (FORGE_RE parsers); both paths capture the same ids; a probe failure is a gate decision, not a guessed id.
- The unknown-flag matrix runs against entities that exist; a refusal that says `does not exist` does NOT prove flag handling.

## Negative / boundary checks
- `--json=x` stays an unknown option (the bare-token matcher never matched it) — the refusal names `--json=x` and hints at bare `--json`.
- Bare `git forge --help` exits 1 as a documented quirk — usage display via `git forge help` is not an error (exit 0); only real command failures exit 1.
- The adapter's JSON probe never masks a broken forge: `parseSemver` on prose garbage returns null (prose path), and the fake fixture's missing `version` handler proves the prose path stays exercised in the matrix.
- The pre-fix failure mode is the regression being guarded: a surface that swallows `--bogus` with exit 0 (9 surfaces did, pre-`41ea9a0`) fails this flow's step 5 and the committed t7 guard.

## Evidence expected
- The t7 byte-compare + semver + unknown-flag cargo outputs (`t7-json-cargo.txt`, `t7-version-cargo.txt` shape).
- Live `--json` snapshots for all seven surfaces (`issue-new.json`, `issue-list.json`, `issue-show.json`, `pr-create.json`, `pr-list.json`, `pr-show.json`, `ci-run.json` shape) with exit codes.
- The unknown-flag exit matrix: one line per surface with exit code and verbatim refusal, run on a repo with the entities present.
- The version probe artifact (`0.3.0`, exit 0, both forms; `version --bogus` exit 1).
- Adapter matrix + parity logs (36/36 and 6/6) and the live gate/cycle artifacts (`version-gate-live.json`: probe object, supportsJson, 0.2.9 false; `adapter-json-cycle-live.json`: `LIVE-JSON-CYCLE OK` with ids).
- A run-log ending line `ALL <k> ASSERTIONS EXERCISED OK`.
