# Release Install Script

Status: active

## Problem

v0.2.1 ships prebuilt tarballs (`aarch64-apple-darwin`,
`x86_64-unknown-linux-gnu`) plus `SHA256SUMS.txt` on GitHub Releases, but
installing them meant manual download/unpack/symlink steps. A single-command
installer is the standard consumption path for a CLI distributed this way.

## Decision (implemented)

`scripts/install.sh` — a POSIX `sh` installer in this repository, run as:

```sh
curl -fsSL https://raw.githubusercontent.com/cexll/git-forge/master/scripts/install.sh | sh
```

Behavior:

1. **Platform detection** via `uname -s/-m` maps to a release target triple;
   unsupported combos (e.g. Intel macOS) fail with a build-from-source hint.
2. **Tag resolution**: default is the latest release, resolved through the
   `github.com/<repo>/releases/latest` web redirect (`%{url_effective}`),
   NOT the `api.github.com` JSON endpoint — the unauthenticated API rate
   limit (60 req/h/IP) made the default path fail behind shared IPs
   (observed live: HTTP 403). `VERSION=vX.Y.Z` pins a tag.
3. **Integrity**: downloads the tarball AND `SHA256SUMS.txt`, then compares
   the tarball's digest against the matching entry with plain string
   equality — fail-closed on mismatch and on a missing entry (no
   `sha256sum -c` portability assumptions; works with `sha256sum` or
   `shasum`).
4. **Install**: extracts, `install -m 0755 git-forge` into
   `${INSTALL_DIR:-~/.local/bin}` (sudo only when the dir is not writable),
   re-creates the relative `git-issue`/`git-pr` symlinks, smoke-runs
   `<dir>/git-forge --help`, and prints a PATH hint when the dir is not on
   PATH. Re-running upgrades in place; downloads go to a `mktemp` dir cleaned
   by an EXIT trap.

## Alternatives considered

- **Homebrew tap** — a second repo plus bottle plumbing; disproportionate
  for an L1 single-user tool. Revisit if the audience grows beyond the
  single-developer scope.
- **crates.io `cargo install`** — requires publishing machinery and a long
  source build on every machine; the prebuilt tarballs already exist.
- **Attaching install.sh to each release** — pins the installer to the
  release it installs, but then installer fixes require re-cutting a
  release; the master-raw URL self-updates.
- **`just install`** — only helps after the repo is already cloned.

## Notes

- The binaries are unsigned; installing via curl does not set the macOS
  quarantine xattr, so Gatekeeper does not block the installed binary
  (browser downloads would be quarantined).
- `curl | sh` executes fetched code; the SHA-256 verification binds the
  *payload* to the release's published sums, and the transport is HTTPS to
  github.com. Under the L1 single-user threat model this is the accepted
  distribution channel, same as rustup/deno-style installers.
