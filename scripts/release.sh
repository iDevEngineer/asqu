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
#   ASQU_RELEASE_LINUX_TESTS=0                        skip the Linux tests in Docker
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

# Tests on all three systems. Mac: `asqu release` already ran them in this fresh copy.
# Windows: this Mac can't run them, so GitHub runs them for each pushed commit; stop if
# that run failed for the commit being released, warn if there's no result yet.
windows_tests() {
  command -v gh >/dev/null 2>&1 || { echo "Warning: can't see the Windows test result (gh isn't installed)."; return; }
  commit="$(git -C "$project" rev-parse HEAD 2>/dev/null)" || { echo "Warning: can't see the Windows test result (no Git commit)."; return; }
  short="${commit%"${commit#???????}"}"
  [ -z "$(git -C "$project" status --porcelain)" ] ||
    echo "Warning: there are changes since commit $short, so its Windows test result doesn't cover them."
  repo="$(git -C "$project" remote get-url origin 2>/dev/null | sed -E 's#^(git@github\.com:|https://github\.com/)##; s#\.git$##')"
  runs="$(gh api "repos/$repo/commits/$commit/check-runs" \
    --jq '.check_runs[] | select(.name | test("windows")) | "\(.status) \(.conclusion)"' 2>&1)" || {
    case "$runs" in
      *"No commit found"*) echo "Warning: Windows tests haven't run: commit $short isn't pushed." ;;
      *) echo "Warning: can't see the Windows test result (GitHub didn't answer)." ;;
    esac
    return
  }
  case "$runs" in
    "") echo "Warning: Windows tests haven't run for commit $short (is it pushed?)." ;;
    *"completed failure"*|*"completed cancelled"*|*"completed timed_out"*)
      fail "Windows tests failed for commit $short; fix them first" ;;
    *queued*|*in_progress*) echo "Warning: Windows tests are still running for commit $short." ;;
    *) echo "Windows tests passed for commit $short." ;;
  esac
}
# Linux: in Docker, on the same Rust version as this Mac. Downloaded crates and builds are
# kept in Docker volumes, so later releases are quicker.
linux_tests() {
  docker info >/dev/null 2>&1 || {
    echo "Warning: Linux tests skipped: Docker isn't running (open Docker to run them)."
    return
  }
  rust="$(rustc --version | cut -d' ' -f2)"
  echo "Running the tests on Linux (Rust $rust)..."
  docker run --rm -v "$PWD":/src:ro -w /src \
    -v asqu-linux-cargo:/usr/local/cargo/registry -v asqu-linux-target:/target \
    -e CARGO_TARGET_DIR=/target "rust:$rust-bookworm" cargo test --locked --quiet ||
    fail "the tests failed on Linux"
}
windows_tests
[ "${ASQU_RELEASE_LINUX_TESTS:-1}" = 0 ] || linux_tests

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
