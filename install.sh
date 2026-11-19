#!/bin/sh
# Installs Asqu, the version control that nobody has to manage.
#
#   curl -fsSL https://asqu.dev/install | sh
#
# Downloads the right build for this computer, checks its fingerprint (SHA-256),
# and puts `asqu` in ~/.local/bin. Nothing else on the system is changed.
#
# Settings (all optional):
#   ASQU_VERSION=v0.1.0         a specific release instead of the latest
#   ASQU_INSTALL_DIR=/some/dir  where to put the program
#   ASQU_DOWNLOAD_BASE=URL      download from a mirror (also used for testing)
set -eu

VERSION="${ASQU_VERSION:-latest}"
DEST="${ASQU_INSTALL_DIR:-$HOME/.local/bin}"

say() { printf '%s\n' "$*"; }
fail() { printf 'asqu install: %s\n' "$*" >&2; exit 1; }

case "$(uname -s)" in
  Darwin) os="apple-darwin" ;;
  Linux) os="unknown-linux-musl" ;;
  *) fail "this system ($(uname -s)) isn't supported yet. Asqu runs on macOS, Linux and Windows." ;;
esac
case "$(uname -m)" in
  arm64 | aarch64) arch="aarch64" ;;
  x86_64 | amd64) arch="x86_64" ;;
  *) fail "this processor ($(uname -m)) isn't supported yet" ;;
esac
target="$arch-$os"
asset="asqu-$target.tar.gz"

if [ -n "${ASQU_DOWNLOAD_BASE:-}" ]; then
  base="$ASQU_DOWNLOAD_BASE"
elif [ "$VERSION" = "latest" ]; then
  base="https://asqu.dev/download"
else
  case "$VERSION" in v[0-9]*.[0-9]*.[0-9]*) ;; *) fail "\"$VERSION\" isn't a version like v0.1.0" ;; esac
  base="https://asqu.dev/download/$VERSION"
fi

command -v curl >/dev/null 2>&1 || fail "curl is needed to download Asqu"
tmp="$(mktemp -d "${TMPDIR:-/tmp}/asqu-install.XXXXXX")"
trap 'rm -rf "$tmp"' EXIT INT TERM

say "Downloading Asqu for ${target}..."
curl -fsSL "$base/$asset" -o "$tmp/$asset" || fail "couldn't download $base/$asset"
curl -fsSL "$base/$asset.sha256" -o "$tmp/$asset.sha256" || fail "couldn't download the fingerprint for $asset"

expected="$(cut -d ' ' -f 1 <"$tmp/$asset.sha256")"
if command -v shasum >/dev/null 2>&1; then
  actual="$(shasum -a 256 "$tmp/$asset" | cut -d ' ' -f 1)"
elif command -v sha256sum >/dev/null 2>&1; then
  actual="$(sha256sum "$tmp/$asset" | cut -d ' ' -f 1)"
else
  fail "can't check the download's fingerprint (no shasum or sha256sum)"
fi
[ -n "$expected" ] && [ "$expected" = "$actual" ] || fail "the download doesn't match its fingerprint, so it wasn't installed"

tar -xzf "$tmp/$asset" -C "$tmp"
[ -f "$tmp/asqu" ] || fail "the download didn't contain the asqu program"
mkdir -p "$DEST"
mv -f "$tmp/asqu" "$DEST/asqu"
chmod 755 "$DEST/asqu"

say "Asqu is free to use under its licence: https://asqu.dev/license"
say "Installed $("$DEST/asqu" --version) to $DEST/asqu"
case ":$PATH:" in
  *":$DEST:"*) ;;
  *) say "" && say "Add Asqu to your PATH (then open a new terminal):" && say "  echo 'export PATH=\"$DEST:\$PATH\"' >> ~/.zshrc" ;;
esac
say ""
say "Next steps:"
say "  1. Go to a project folder:   cd ~/path/to/your-project"
say "  2. Optional, if you use AI:  asqu setup    (so any AI records your requests)"
say "  3. See your history:         asqu open     (opens in your browser; saves changes while it runs)"
