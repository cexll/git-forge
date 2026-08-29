#!/bin/sh
# install.sh — install git-forge from GitHub Releases.
#
#   curl -fsSL https://raw.githubusercontent.com/cexll/git-forge/master/scripts/install.sh | sh
#
# Environment overrides:
#   VERSION=v0.2.1        pin a release tag (default: latest release)
#   INSTALL_DIR=~/.local/bin   install destination (default: $HOME/.local/bin)
#
# Installs the git-forge binary plus git-issue/git-pr symlinks, after
# verifying the tarball against the release's SHA256SUMS.txt (fail-closed).
set -eu

REPO="cexll/git-forge"
BINARY="git-forge"
LINKS="git-issue git-pr"

fail() { printf 'install: error: %s\n' "$*" >&2; exit 1; }
need() { command -v "$1" >/dev/null 2>&1 || fail "missing required tool: $1"; }

need curl
need tar

# --- platform -> release target triple ---
os=$(uname -s)
arch=$(uname -m)
case "$os/$arch" in
	Darwin/arm64) target="aarch64-apple-darwin" ;;
	Linux/x86_64) target="x86_64-unknown-linux-gnu" ;;
	*) fail "no prebuilt binary for $os/$arch — build from source instead (see README: cargo install --path .)" ;;
esac

# --- resolve tag ---
if [ "${VERSION:-}" ]; then
	tag=$VERSION
else
	# /releases/latest redirects to /releases/tag/<tag>; using the web
	# redirect avoids the tiny unauthenticated api.github.com rate limit.
	url=$(curl -fsSL -o /dev/null -w '%{url_effective}' "https://github.com/$REPO/releases/latest") ||
		fail "could not resolve latest release — pin one: VERSION=vX.Y.Z"
	tag=${url##*/}
	[ -n "$tag" ] || fail "could not resolve latest release tag — pin one: VERSION=vX.Y.Z"
fi

asset="$BINARY-$tag-$target.tar.gz"
base="https://github.com/$REPO/releases/download/$tag"

# --- checksum tool ---
if command -v sha256sum >/dev/null 2>&1; then
	sha256() { sha256sum "$1" | awk '{print $1}'; }
elif command -v shasum >/dev/null 2>&1; then
	sha256() { shasum -a 256 "$1" | awk '{print $1}'; }
else
	fail "need sha256sum or shasum to verify the download"
fi

# --- download into a temp dir ---
tmp=$(mktemp -d "${TMPDIR:-/tmp}/git-forge-install.XXXXXX") || fail "mktemp failed"
trap 'rm -rf "$tmp"' EXIT
trap 'exit 1' INT TERM

printf 'install: downloading %s\n' "$asset"
curl -fsSL --retry 3 -o "$tmp/$asset" "$base/$asset" ||
	fail "download failed: $base/$asset (unknown VERSION? run without VERSION for latest)"
curl -fsSL --retry 3 -o "$tmp/SHA256SUMS.txt" "$base/SHA256SUMS.txt" ||
	fail "download failed: $base/SHA256SUMS.txt"

# --- verify checksum (fail closed) ---
expect=$(awk -v a="$asset" '$2 == a {print $1}' "$tmp/SHA256SUMS.txt")
[ -n "$expect" ] || fail "no checksum entry for $asset in SHA256SUMS.txt"
actual=$(sha256 "$tmp/$asset")
[ "$actual" = "$expect" ] ||
	fail "checksum mismatch for $asset — expected $expect, got $actual; aborting"
printf 'install: checksum verified\n'

# --- extract ---
tar -xzf "$tmp/$asset" -C "$tmp" || fail "failed to extract $asset"
srcdir="$tmp/$BINARY-$tag-$target"
[ -x "$srcdir/$BINARY" ] || fail "unexpected archive layout: $BINARY-$tag-$target/$BINARY missing"

# --- install ---
dir=${INSTALL_DIR:-"$HOME/.local/bin"}
mkdir -p "$dir" || fail "cannot create install dir: $dir"
SUDO=""
if [ ! -w "$dir" ]; then
	need sudo
	SUDO="sudo"
fi
$SUDO install -m 0755 "$srcdir/$BINARY" "$dir/$BINARY" || fail "install into $dir failed"
for link in $LINKS; do
	(cd "$dir" && $SUDO ln -sf "$BINARY" "$link") || fail "creating $dir/$link failed"
done

# --- smoke test the installed binary ---
"$dir/$BINARY" --help >/dev/null 2>&1 || fail "installed $dir/$BINARY failed to run (--help)"

printf 'install: installed %s to %s (git-forge, git-issue, git-pr)\n' "$tag" "$dir"
case ":$PATH:" in
	*":$dir:"*) ;;
	*) printf 'install: note: %s is not on PATH — add: export PATH="%s:$PATH"\n' "$dir" "$dir" ;;
esac
printf 'install: try: git forge --help\n'
