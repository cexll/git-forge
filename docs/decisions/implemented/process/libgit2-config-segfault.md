# libgit2 SIGSEGV on config mutation after open / before deferred load

Status: active

## Problem

During the VAL-117 replan investigation (git-forge-contract-fix mission), probing
`EventStore::actor()`'s STAGE A (`repo.config()` open-failure) branch with the
pinned libgit2 1.9.6 (libgit2-sys 0.18.7+1.9.6, git2 0.20) exposed a libgit2
crash: **SIGSEGV (exit 139)** when the repository's `.git/config` is replaced
with malformed content after `Repository::open` and before the first
`repo.config()` call. Full probe record:
`.specs/git-forge-contract-fix/evidence/assertions-contract-fix/VAL-117-STAGE-A-probe.txt`.

## Reproducer (two independent triggers)

Both produce SIGSEGV (exit 139) in a Rust program that opens a repo via
`git2::Repository`, then calls `repo.config()`:

1. **Mutation after open**: open a repo with a valid `.git/config`, then
   replace the file with malformed content (e.g. `[user\nemail = broken`,
   missing `]`), then call `repo.config()`. libgit2 re-parses the replaced
   file on the lazy-load path and crashes (undefined behavior).
2. **Missing-at-open seam**: remove `.git/config` before `Repository::open`
   (open tolerates ENOTFOUND), create a malformed `.git/config` afterwards,
   then call `repo.config()`. Same crash on the lazy-load parse
   (`repository.c` `git_repository_config__weakptr`, lines 1403-1471).

Control observations:
- Malformed config **before** open fails cleanly at `Repository::open`
  ("failed to parse config file: missing ']' in section header") — actor()
  never runs; no crash.
- Config replaced by a **directory** (before or after open) is treated as
  absent: `repo.config()` succeeds, `get_string("user.email")` returns
  NotFound (the STAGE B fallback path). No crash.

## Decision

Record this as a **LOCAL SAFETY FINDING requiring escalation to upstream
libgit2** — it is a defect in the pinned dependency, not in git-forge code:

- The reproducer (fixtures 3 and 4 of the probe record) is preserved for an
  upstream issue report; no upstream issue has been filed yet.
- git-forge cannot defensibly mitigate the crash in its own code: the crash is
  inside libgit2's config parsing, triggered by a race between open-time
  snapshot state and a later filesystem mutation. **No "preflight config
  parseability" mitigation is proposed** — a preflight parses the file before
  open (where malformed content already fails cleanly) and cannot cover the
  mutation-after-open/deferred-load window that actually crashes, so it would
  not prevent the crash and would add cost.
- The STAGE A Err branch in `EventStore::actor()` remains an unexercised
  defensive path under this libgit2 (see VAL-117); the crash is recorded
  separately from any assertion evidence — it is NOT acceptable evidence for
  the VAL-117 STAGE A documentation.

## Alternatives considered

- **Preflight parse check before commands**: rejected — the crash trigger is
  a mutation after open, outside any preflight window; the check would not
  prevent the crash and duplicates open-time parsing.
- **Detecting file changes after open and refusing**: not feasible safely —
  git-forge does not control libgit2's snapshot lifecycle, and any check would
  itself race the mutation.
- **Upgrading the dependency**: out of scope for this repo change (pinned
  libgit2-sys 0.18.7+1.9.6); escalation is the correct first step.

## Consequences

- Reproducer and analysis are on record for an upstream libgit2 report.
- git-forge behavior on malformed-config-after-open remains "undefined
  behavior inside libgit2" (crash). The probe exercised only `repo.config()`
  in an isolated Rust process — no CLI command/ref-transaction path was run
  under the crash condition, so this record makes NO claim about ref
  integrity or partial-write behavior on that path; that remains untested and
  out of scope for this finding. Documented, not mitigated, until upstream
  responds.
- Future dependency bumps should re-test fixtures 3 and 4 as a regression
  gate before adopting a new libgit2.

## Addendum: write-path mitigation (VAL-115 resolution)

The write path no longer calls `repo.config()` at all. Identity is resolved
via the safe git binary (`crate::git::config_get_identity`, a
`git config --null --get` child) and bound as an explicit signature through
`EventStore::bind_signature`, which yields a write-capable `BoundEventStore`.
The libgit2 lazy-load crash therefore cannot be reached by any forge command:
- Resolve-identity commutes the open: the CLI resolves identity BEFORE the
  libgit2 `Repository::open`, so a malformed `.git/config` surfaces as a clean
  CLI error (`git config --get` exits 128), never as a libgit2 open failure
  or the post-open lazy-load SIGSEGV.
- Bound-write commutes the config read: `BoundEventStore` write methods use
  the pre-bound signature; they never read libgit2 config, so a config
  corrupted after `Repository::open` cannot page-fault the write. The ref
  transaction shells out to `git update-ref`, which itself parses the corrupt
  config and may fail with git's clean error — process never SIGSEGVs.

The pinned libgit2 defect remains (an upstream escalation is still warranted,
and a dependency bump must still re-test fixtures 3/4), but the git-forge
forge-write paths are no longer exposed to it. An isolated child-process
regression (`tests/t1a_store.rs` `val115_postopen_corrupt_write_does_not_segv`)
proves the write-after-post-open-corruption path completes without SIGSEGV.