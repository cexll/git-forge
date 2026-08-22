# Actor resolves configured user.email independent of user.name (event wire contract)

Status: active

## Problem

The event actor — wire contract `"actor": "<user.email>"` — was routed through
git2's `Repository::signature()`, which requires BOTH `user.name` and
`user.email` to be configured. A repo with only `user.email` set (a valid,
common state for a forge-only workflow) made `signature()` error, and the
store silently fell back to `forge@localhost`: every CLI-written event then
carried the wrong actor, violating VAL-102 ("'actor': '<user.email>'").
Existing tests covered this up by always configuring `user.name` first.

## Decision

`EventStore::actor()` now reads `user.email` directly from the repo config via
`repo.config().get_string("user.email")`, not via `Repository::signature()`.
Fallback to `forge@localhost` happens when the `user.email` key is absent
from repo, global, and system config, is present but empty/whitespace
(F-027, see below), or cannot be read as a usable string (F-028, see below).
Failure handling is stage-specific (F-028): a failure to OPEN the repo's own
config (the early `repo.config()` `?`) PROPAGATES as `StoreError` — a
repo-access environment fault that an identity fallback must never mask; a
failure to LOOK UP the value (`get_string` with a non-`NotFound` error) FALLS
BACK to `forge@localhost`. Because
the read is now fallible, every event-creation command
resolves the actor before any allocation or mutation: `cmd_new` resolves
before `allocate_id()`, and `cmd_pr_merge` resolves after the read-only guards
(stale-base, checked-out-base) but before any worktree reservation, so a
config error can never consume an ID or leak a pending-result ref.

### Extension (F-027): an empty or whitespace `user.email` value counts as absent

`git config user.email ""` leaves the key present-but-empty in `.git/config`,
and libgit2's `get_string` returns that empty string as-is (not `NotFound`).
Before this extension the store faithfully recorded the empty string as the
event `actor`, violating the wire contract `"actor": "<user.email>"` (an
empty string is not an email). `actor()` now treats a non-`NotFound`
`Ok(value)` whose `value` is empty or all-whitespace exactly like an absent
key and falls back to `forge@localhost`. Nothing else changed: a non-empty
configured email is still used verbatim, and value errors are governed by the
F-028 stage policy below.

### Extension (F-028): stage-specific config-failure policy

Scrutiny finding F-028: `actor()` hard-failed on any config problem, so every
event command errored on a repo whose identity config was not cleanly
parseable. The policy is now stage-specific inside `actor()`:

- **STAGE A — opening the repo's own config (`repo.config()`)** is a
  repo-access environment fault: the error PROPAGATES via `StoreError`
  (deliberately — a doc comment on `actor()` states this). If the config
  cannot be opened at all, actor resolution is impossible and a silent
  fallback (or an empty actor) would mask a broken repo; the store writes no
  event and mutates no ref.
- **STAGE B — looking up `user.email` (`get_string`)** FALLS BACK to
  `forge@localhost` on any value-level failure. `NotFound` (absent key) already
  fell back; the change extends the same rule to other `get_string` errors: a
  value that cannot be read as a usable string (e.g. non-UTF-8 bytes, which
  libgit2 returns raw and git2's `get_string` rejects) is semantically "no
  usable identity", so commands still succeed with the documented default,
  honoring the contract sentence that "commands still succeed".

`git2::ErrorCode::NotFound` and all other value-lookup errors now share one
fallback arm — the two stages differ on where the error originates (open vs
lookup), not on the key's absence.

## Alternatives considered

- **Keep `actor() -> String`, read email best-effort**: a non-fallible API
  hides a broken identity source and re-creates the same silent-wrong-actor
  class of bug under a different config failure (e.g. unreadable config file).
  Rejected: AGENTS requires `Result<T, E>` semantics and an unusable identity
  should surface, not silently degrade.
- **Continue via `signature()` but tolerate a missing name**: reconstructing
  the signature by hand is exactly the fallback-to-a-hardcoded-identity smell
  the wire contract forbids; the email is one config key, so reading it
  directly is both simpler and more precise.
- **Reject empty/whitespace emails with a config error** (F-027 extension):
  keener type-wise, but it would block every event command on a repo whose
  email is set to an empty string — the same "no usable identity must not
  block writing" rule the absent-key fallback already implements. An empty or
  whitespace value is semantically "no email configured", so treating it as
  absent (fallback `forge@localhost`) keeps the behavior uniform. Rejected.

## Consequences

- A repo with only `user.email` configured records that email as every event's
  actor; `user.name` neither gates nor influences the actor.
- No-identity repos still work (fallback `forge@localhost`), unchanged.
- New fallible API: callers resolve the actor once, early, before side
  effects; the merge path's ordering (guards → actor → worktree → execution)
  is documented at the call site.
- `docs/architecture/git-forge.md` updated in the same change (fallback
  condition is now "user.email absent or empty", not "user.email absent";
  both the Event Commit Layout bullet and the Event Identity "No-email
  fallback" bullet state the same condition). F-028 added the two-stage
  failure policy to both: lookup failures fall back, config-open failures
  propagate.
- F-027 extension: a `user.email` configured to an empty/whitespace value now
  yields the same fallback as an absent key — every event carries
  `forge@localhost` and commands succeed. Regression coverage: a hermetic
  issue-chain test (empty-email repo) proves the event-JSON actor field equals
  `forge@localhost` across new/comment/close/reopen; it failed against the
  pre-extension implementation (empty-string actors).
- F-028 extension: STAGE B regression via a hermetic issue-chain test on a
  repo whose `user.email` VALUE is non-UTF-8 (non-`NotFound` `get_string`
  error) — every event carries `forge@localhost` and commands succeed; it
  failed against the pre-fix implementation ("configuration value is not
  valid utf8").
- VAL-115 replan → VAL-117 (F-002): STAGE A is documented, not regressed.
  The earlier CLI regression `cli_events_propagate_config_open_failure_as_clean_error`
  (chmod 000 on `.git/config`) was DELETED: it observed the git-update-ref
  subprocess seam, not the actor STAGE A branch, and implied config causality
  without proving it. STAGE A evidence is now source inspection + a probe
  record: six attempted fixtures on the pinned libgit2 1.9.6 produced no clean
  `repo.config()` error on an already-open repository (probe record at
  `.specs/git-forge-contract-fix/evidence/assertions-contract-fix/VAL-117-STAGE-A-probe.txt`).
  A directory config at lazy-load resolves to `NotFound` (STAGE B fallback);
  malformed config at parse crashes libgit2 (SIGSEGV, exit 139) — a LOCAL
  SAFETY FINDING requiring escalation (reproducer in the probe record), not
  acceptable evidence for this branch. No snapshot-loading invariant or
  absolute unreachability is claimed; the deliberate propagation semantics
  are unchanged (config-open failures still propagate via `StoreError`).
- F-005 extension: a whitespace-only `user.email` value (`"   "`) gets the
  same fallback as an empty one and now has its own hermetic issue-chain
  regression (whitespace-email repo) proving the event-JSON actor equals
  `forge@localhost` across new/comment/close/reopen, alongside the F-027
  empty-string case.
- Regression coverage: hermetic email-only tests for both issue and PR chains
  prove the name-less path. The hermetic forge child sets HOME/XDG dirs to a
  fresh exclusively-created directory and disables system/global config
  (`GIT_CONFIG_NOSYSTEM=1`, `env_remove` of `GIT_CONFIG_GLOBAL`/`GIT_CONFIG_SYSTEM`),
  so no machine/user-level identity can leak in. The new tests fail against
  the pre-fix implementation.