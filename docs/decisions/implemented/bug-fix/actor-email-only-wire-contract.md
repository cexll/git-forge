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
Fallback to `forge@localhost` happens only when the `user.email` key is absent
from repo, global, and system config; a malformed or unreadable config (any
error other than a missing key) propagates as `StoreError` instead of being
swallowed. Because the read is now fallible, every event-creation command
resolves the actor before any allocation or mutation: `cmd_new` resolves
before `allocate_id()`, and `cmd_pr_merge` resolves after the read-only guards
(stale-base, checked-out-base) but before any worktree reservation, so a
config error can never consume an ID or leak a pending-result ref.

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

## Consequences

- A repo with only `user.email` configured records that email as every event's
  actor; `user.name` neither gates nor influences the actor.
- No-identity repos still work (fallback `forge@localhost`), unchanged.
- New fallible API: callers resolve the actor once, early, before side
  effects; the merge path's ordering (guards → actor → worktree → execution)
  is documented at the call site.
- `docs/architecture/git-forge.md` updated in the same change (fallback
  condition is now "user.email absent", not "no identity").
- Regression coverage: hermetic email-only tests for both issue and PR chains
  prove the name-less path. The hermetic forge child sets HOME/XDG dirs to a
  fresh exclusively-created directory and disables system/global config
  (`GIT_CONFIG_NOSYSTEM=1`, `env_remove` of `GIT_CONFIG_GLOBAL`/`GIT_CONFIG_SYSTEM`),
  so no machine/user-level identity can leak in. The new tests fail against
  the pre-fix implementation.