# CI Plan Subprocess Environment and Reap Policy

Status: active

## Problem

The CI runner (`src/ci.rs` `run_ci_plan`) executes a PR's snapshot CI plan
(`.forge/ci.sh` or the `just check` fallback) inside a disposable worktree,
under a bounded deadline with process-group termination. When the plan
subprocess was first wired in, only `BASH_ENV`/`ENV` were removed and the
interpreter (`bash`/`just`) was resolved through the caller's inherited
`PATH`. A review of the delivered L2 CI gate found that a caller can forge a
green CI Check without the plan executing: a caller-controlled `PATH` shim for
`bash`/`just`, Git location overrides (`GIT_DIR`, `GIT_WORK_TREE`,
`GIT_INDEX_FILE`, `GIT_COMMON_DIR`), and Bash exported functions
(`BASH_FUNC_*%%`) each let the caller redirect or suppress the plan bytes.
Separately, the deadline path discarded the group-kill RESULT and then performed
an unbounded `child.wait()`, so a plan that exhausted the user's process quota
(left `/bin/kill` unable to spawn) could hang `ci run` forever with no Check
recorded and no worktree cleanup.

## Decision (implemented)

1. **Tighten the plan child's environment** via `tighten_ci_env`: remove
   `BASH_ENV`, `ENV`, `CDPATH`, `IFS`, `SHELL`, `SHELLOPTS`, `BASHOPTS`,
   `POSIXLY_CORRECT`, every Git location/config override (`GIT_DIR`,
   `GIT_WORK_TREE`, `GIT_INDEX_FILE`, `GIT_OBJECT_DIRECTORY`,
   `GIT_ALTERNATE_OBJECT_DIRECTORIES`, `GIT_COMMON_DIR`, `GIT_NAMESPACE`,
   `GIT_CONFIG`/`GIT_CONFIG_COUNT`/`GIT_CONFIG_PARAMETERS` are removed; `GIT_CONFIG_GLOBAL`/`GIT_CONFIG_SYSTEM`
   are PINNED to `/dev/null` (an empty trusted file, so the plan's git sees only the repo's own local
   config — merely removing them would restore caller `$HOME`/`$XDG_CONFIG_HOME` lookup), and every `BASH_FUNC_*%%` exported-function entry.
   The env is enumerated with `vars_os()` and the `BASH_FUNC_*%%` names matched
   byte-preservingly (a non-UTF-8 name whose ASCII edges still match is removed;
   `vars()` would panic on a non-UTF-8 key/value and strand the worktree).
   `JUST_DRY_RUN` is forced to `false`; the Just execution switches
   `JUST_ALLOW_MISSING`, `JUST_NO_DEPS`, `JUST_WORKING_DIRECTORY`,
   `JUST_DOTENV_COMMAND` are removed (an inherited `JUST_ALLOW_MISSING=true`
   would turn an absent `check` into success, `JUST_NO_DEPS=true` skips a
   failing dependency); `SHELLOPTS=noexec` is removed so a failing plan cannot
   be syntax-read-but-not-executed into a green Check.
2. **Resolve the interpreter by trusted absolute path**: `.forge/ci.sh` runs
   under `/bin/bash`; the `just` fallback resolves `just` via `resolve_just()`,
   which searches system install locations (`/usr/bin`, `/usr/local/bin`,
   `/opt/homebrew/bin`) and the operator's own `$HOME/.cargo/bin` /
   `$HOME/.local/bin` (the common cargo install site, which the fallback must
   find). It NEVER falls back to a bare `just` on `PATH` (which a caller could
   shim) — an install not found is a spawn failure (honest). A caller cannot
   influence the result through `PATH`; `HOME` must be ABSOLUTE (a relative
   `HOME` resolves against the child's worktree cwd, so the checked and executed
   file could differ) and is otherwise trusted because in this single-user local
   tool the operator who invokes `git forge ci run` is the trusted principal (no
   separate-principal privilege boundary to cross). The ordinary recipe shell is PINNED to `/bin/sh` via `just --shell /bin/sh` (so a `PATH` `sh`
   shim cannot green a failing recipe); `script` attributes (single-line/list/multiline/continued,
   incl. `set script-interpreter`/`set shell`/`set default-script`) and shebang recipes are REFUSED by
   the closure validator, because those run a PATH/script shebang interpreter that `--shell` does not pin. A NON-UTF-8
   `HOME` is likewise a documented edge: the resolver keeps the native bytes and
   the program/justfile are spawned as native `PathBuf`/`OsStr` (no
   `to_string_lossy`), so a non-UTF-8 HOME makes the fallback record a failed
   Check (honest) rather than running a replacement-char path — never a forged
   green. `resolve_just()` and `resolve_git()` also PROBE the candidate
   (`--version` succeeds) before accepting it, so a stale/non-executable system
   file does not shadow a valid `~/.cargo/bin` install.
3. **Check the group-kill result on the deadline path**: if the group kill fails
   (e.g. EPERM under a restrictive process quota), do NOT `child.wait()`
   unbounded. Instead `reap_with_grace` polls `try_wait` for ~500 ms, then
   `child.kill()`s the direct child and reaps it, so a still-running leader is
   not leaked and the run is recorded as failed.
4. **The `just check` fallback must be a SELF-CONTAINED justfile** (F-012/
   F-013): `set fallback` and ANY `import`/`mod` source directive are refused,
   because `just` resolves imported/module'd sources at execution (implicit
   module candidates, shell-expanded/decoded string paths) — which the runner
   cannot reliably pin to the immutable snapshot. The only source
   `just --justfile` reads is therefore the single immutable blob materialized
   into the worktree. The directive scanner is `#`-quote-aware (a `#` inside a
   quoted path is not a comment) and keyword-boundary-aware (a variable
   assignment like `important := '/tmp/x'` is not misread as a directive), and
   only TOP-LEVEL lines (column-0, not an indented recipe body like
   `    import os`) are inspected as directives.
5. **Documented residuals**: a descendant that `setsid`s into a new session
   escapes the process group (kill is containment-only-within-same-group); the
   worktree checkout still runs smudge/attribute filters before the CI deadline.
   Both are documented as known limitations (single-user local tool; full
   containment needs OS isolation), not silent gaps. The non-UTF-8 worktree path
   is refused fail-closed (`path_to_str`) instead of a `/tmp/none` sentinel that
   could target an unrelated worktree. The authenticated plan runs as the
   operator's own user, so (a) the plan's own `git` commands resolve
   `git`/helpers via the inherited `PATH` (a PATH shim is a SELF-attack in this
   single-user tool — the operator faking their own CI, a non-goal), and (b) the
   repo's OWN `.git/config` (snapshot content the operator authored) is
   intentionally trusted for the same reason: it can configure aliases /
   `include.path` / helpers, but the snapshot plan already executes arbitrary
   commands as the operator. The meaningful caller-ENV injection boundary
   (separate from the snapshot's own content) is the caller's environment
   (`BASH_ENV`, loader preloads, Git location/config overrides, exported
   functions), which IS tightened — those are NOT the repo's own content.
   `worktree_registered_path` parses `--porcelain` by lines (not `-z`/C-unquote),
   so a `TMPDIR` containing a Git-quoting byte (e.g. a literal newline) would
   escape the exact-path check — a documented exotic-environment edge, not
   reachable with the standard single-user macOS/iOS `TMPDIR` and therefore
   deferred (native `-z` parsing is the future fix if such an env surfaces).

## Alternatives considered

- `env_clear()` + a minimal re-set env: rejected because it breaks legitimate
  plans that need toolchain binaries on `PATH` (e.g. `cargo`, `node`), a
  functional regression worse than the residual `PATH` boundary for `just`.
- Resolve `just` by searching the caller's `PATH` at a "trusted" moment:
  rejected — there is no trusted moment; the caller IS the operator, so a
  caller-controlled `PATH` is untrusted by construction.
- Exact per-`settings`-deny-list for the BASH_FUNC entries: rejected — matching
  the `%%`-suffixed ASCII names is the minimal correct rule; a per-var list
  would miss a caller-defined function shadowing any builtin.

## Consequences (implemented)

- A malicious or accidental caller env cannot now suppress or redirect a
  snapshot plan into a green Check; a plan whose real commands would fail is
  recorded as failed.
- A kill-failure no longer hangs `ci run`; the run is recorded as failed and
  the temp worktree is cleaned.
- The `just` fallback now fails (recorded as a failed Check, honest spawn
  failure) when `just` is not at a system dir or the operator's `~/.cargo/bin` /
  `~/.local/bin` — watch for unusual-but-legit `just` installs and extend
  `resolve_just()` if one surfaces.
- A `just check` project must be self-contained (no `import`/`mod`, no
  `set fallback`) or its fallback is refused (recorded as a failed Check). A
  project that imports/modules should supply a `.forge/ci.sh` instead.
- The env denylist and the 500 ms grace-then-kill reaction are stable policy;
  weakening either incompatibly requires a new record.


## Posture (L1 single-user): the `just check` fallback validator is NOT a security boundary

In L1 the operator runs `git forge ci run` in their own shell and the snapshot plan
(`.forge/ci.sh` / the fallback justfile) is the operator's own content, executed as
the operator's own user. The closure validator (`src/ci/validate.rs`) therefore is
**defense-in-depth / hygiene**, not a security boundary: a crafted justfile that
bypasses the scanner can only make the operator's own CI appear green (a self-attack,
a non-goal for a single-user tool). The scanner is best-effort reject-on-ambiguity —
it may refuse an exotic-but-valid justfile (recorded as a FAILED Check, never green),
which is the safe direction. Structural options — (a) drop the `just check` fallback
entirely (require `.forge/ci.sh`), or (b) validate via a real Just parser — were
considered; (a) is a feature regression and (b) is disproportionate for L1, so the
fallback stays with the reject-on-ambiguity scanner. Future scanner gaps should be
triaged under this posture, not as cross-principal escape findings.

**Maintainer decision (2026-08-28): confirmed — keep the fallback with the
reject-on-ambiguity scanner.** Options (a) drop-the-fallback and (b) real-Just-parser
remain rejected for L1; reopening either requires a new record.
