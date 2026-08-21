# size-gate isolates tokei from user config and ignore rules (F-015)

Status: active

## Problem

`just size-gate` (L3 block gate, wired into `just check`) counted source lines
via `tokei --output json src/`. Tokei loads a user configuration from
`$HOME/tokei.toml` (and `$XDG_CONFIG_HOME/tokei/config.toml`). A hostile or
merely unusual config such as `types = ["Python"]` replaces the built-in
language table, which removes Rust entirely from the report — an over-800-line
`src/*.rs` file then reports `code = 0` and the gate passes. Ignore rules
(`.gitignore`, `.ignore`, `.tokeignore`) can likewise suppress a file from the
walk. The gate could be bypassed by the very environment it runs in, so the
L3 size guarantee was advisory rather than enforced.

## Decision

`just size-gate` runs tokei inside a config- and ignore-isolated environment:

- the gate first captures the repo root (`$REPO`), creates a fresh empty
  directory (`mktemp -d`), and **changes into it** before invoking tokei with
  the absolute source path `$REPO/src/` — this neutralizes the current
  directory's `tokei.toml` lookup, which tokei also consults (verified: a
  repo-root `types = ["Python"]` `tokei.toml` hid every Rust file until the
  gate ran from the empty dir);
- `HOME` and `XDG_CONFIG_HOME` are pointed at that same empty directory, so no
  user `tokei.toml`/`config.toml` can load;
- `--no-ignore` disables `.gitignore`/`.ignore`/`.tokeignore` suppression;
- the temp dir is removed via a `trap ... EXIT` so an interrupted or failing
  run never leaks it;
- `set -o pipefail` makes the gate's exit code reflect tokei's own failure
  (not the reader's), so a broken tokei invocation fails loudly;
- the reader receives the absolute `$REPO/src` prefix as an argv so the
  per-file `reports[]` membership test is exact
  (`name.startswith(prefix + "/") and code > 800`).

The per-file `code` limit remains 800 (constraints.yaml
`size_limits.max_file_lines`), enforced by the same python3 reader over
`reports[]`.

## Alternatives considered

- **`--files` + per-file counting only**: still subject to user config type
  remapping; isolated env is the only robust fix for the language-table
  bypass.
- **`env -i` (empty environment)**: would drop every required variable (PATH,
  HOME of the invoking user, just's context) and break tokei's invocation;
  targeted override of the two config locations is narrower and sufficient.
- **Pointing config paths at `/tmp` fallback without ownership**: a shared
  path could already contain a hostile config; only a fresh exclusive dir
  guarantees the gate can never see one.

## Consequences

- A user with a `$HOME/tokei.toml` cannot make the size gate invisible;
  `types = ["Python"]` regression demonstrated: gate exits non-zero for an
  801-line Rust file under the hostile config, and exits 0 on the clean tree.
- The gate now needs `mktemp` (POSIX, available on both macOS and Linux)
  instead of only `tokei`; `just setup` is unchanged.
- Verified against a synthetic 801-line `src/_size_probe.rs` fixture (removed
  after the run).