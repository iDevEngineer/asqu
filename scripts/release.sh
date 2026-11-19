#!/bin/sh
# Asqu's own release step. Never run it by hand: `asqu release v1.2.3` runs it, in a fresh
# copy of files that passed the tests, and only after a person says yes.
#
# Builds Asqu for each kind of computer, packs each build with the licence, writes its
# fingerprint (SHA-256) and signs it with the release key. The finished downloads land in
# the project's target/dist/<version>/ folder (never saved in the history), and are then
# sent to Cloudflare R2 by scripts/upload.sh.
#
# The release key: kept in this Mac's Keychain under the name "asqu-release-key" (or
# passed in ASQU_RELEASE_KEY). It is only ever given to the Asqu already installed on this
# computer, never to anything built here.
#
# Settings (optional):
#   ASQU_RELEASE_TARGETS="aarch64-apple-darwin ..."   build only these
#   ASQU_RELEASE_UPLOAD=0                             build and sign, but don't send
set -eu

version="${ASQU_RELEASE_VERSION:?run this through: asqu release <version>}"
project="${ASQU_PROJECT_DIR:?run this through: asqu release <version>}"
fail() { printf 'release: %s\n' "$*" >&2; exit 1; }

in_cargo="v$(sed -n 's/^version = "\(.*\)"$/\1/p' Cargo.toml | head -n 1)"
[ "$version" = "$in_cargo" ] || fail "asked to release $version, but Cargo.toml says $in_cargo"

targets="${ASQU_RELEASE_TARGETS:-aarch64-apple-darwin x86_64-apple-darwin x86_64-unknown-linux-musl x86_64-pc-windows-msvc}"

# Find everything needed before spending minutes building.
signer="$(command -v asqu || true)"
[ -n "$signer" ] || fail "the installed asqu isn't on the PATH (it signs the downloads)"
if [ -z "${ASQU_RELEASE_KEY:-}" ]; then
  ASQU_RELEASE_KEY="$(security find-generic-password -s asqu-release-key -w 2>/dev/null || true)"
  [ -n "$ASQU_RELEASE_KEY" ] || fail "no release key: it isn't in the Keychain as \"asqu-release-key\""
fi
for t in $targets; do
  case "$t" in
    *-apple-darwin) rustup target list --installed | grep -qx "$t" || fail "missing the Rust target $t (rustup target add $t)" ;;
    *-linux-musl) cargo zigbuild --help >/dev/null 2>&1 || fail "building for Linux needs cargo-zigbuild and zig" ;;
    *-windows-msvc) cargo xwin --help >/dev/null 2>&1 || fail "building for Windows needs cargo-xwin" ;;
    *) fail "don't know how to build for $t" ;;
  esac
done

out="$project/target/dist/$version"
[ ! -e "$out" ] || fail "$out already exists; move it away first"
work="$(mktemp -d "${TMPDIR:-/tmp}/asqu-release.XXXXXX")"
trap 'rm -rf "$work"' EXIT INT TERM

for t in $targets; do
  echo "Building for $t..."
  case "$t" in
    *-apple-darwin) cargo build --release --locked --target "$t" ;;
    *-linux-musl) cargo zigbuild --release --locked --target "$t" ;;
    *-windows-msvc) cargo xwin build --release --locked --target "$t" ;;
  esac
  built="target/$t/release"
  case "$t" in
    *-windows-msvc)
      asset="asqu-$t.zip"
      cp LICENSE "$built/LICENSE.txt"
      (cd "$built" && zip -q -j "$work/$asset" asqu.exe LICENSE.txt)
      ;;
    *)
      asset="asqu-$t.tar.gz"
      cp LICENSE "$built/LICENSE"
      tar -czf "$work/$asset" -C "$built" asqu LICENSE
      ;;
  esac
  (cd "$work" && shasum -a 256 "$asset" > "$asset.sha256")
done

echo "Signing..."
(cd "$work" && ASQU_RELEASE_KEY="$ASQU_RELEASE_KEY" "$signer" sign-release asqu-*.tar.gz $(ls asqu-*.zip 2>/dev/null || true))
unset ASQU_RELEASE_KEY

mkdir -p "$(dirname "$out")"
mv "$work" "$out"
trap - EXIT INT TERM
printf '%s\n' "$version" > "$out/VERSION"
echo "Built and signed $version:"
ls "$out"
echo "The downloads are in $out"

if [ "${ASQU_RELEASE_UPLOAD:-1}" = 0 ]; then
  echo "Not sent anywhere (ASQU_RELEASE_UPLOAD=0)."
else
  sh scripts/upload.sh "$out" "$version"
fi
