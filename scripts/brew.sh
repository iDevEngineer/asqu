#!/bin/sh
# Points Homebrew at a finished release: rewrites Formula/asqu.rb in the tap (the repo
# `brew` reads) and pushes it. scripts/release.sh runs it after the upload; if that didn't
# work it can be run by hand, as often as needed:
#
#   sh scripts/brew.sh <folder with the release's asqu-* files> <version>
#
# Nothing is pushed unless each download is already live and matches its fingerprint, and
# the tap is never moved back to an older version.
#
# The push uses the GitHub sign-in that gh holds for the tap's owner, whichever account is
# active, and doesn't change the active account. The token is only read as Git asks for it,
# so it's never in a file, a web address or the process list.
#
# Settings (optional):
#   ASQU_BREW_DRY_RUN=1    check everything and show the change, push nothing
#   ASQU_BREW_TAP=URL      another tap (used by the tests)
#   ASQU_BREW_BASE=URL     look for the downloads somewhere else (used by the tests)
set -eu

dir="${1:?usage: sh scripts/brew.sh <folder> <version>}"
version="${2:?usage: sh scripts/brew.sh <folder> <version>}"
owner="iDevEngineer"
tap="${ASQU_BREW_TAP:-https://github.com/$owner/homebrew-asqu.git}"
base="${ASQU_BREW_BASE:-https://dl.asqu.dev}"
here="$(cd "$(dirname "$0")" && pwd)"
fail() { printf 'homebrew: %s\n' "$*" >&2; exit 1; }

case "$version" in
  *[!v0-9.]*) fail "\"$version\" isn't a version like v1.2.3" ;;
esac
printf '%s\n' "$version" | grep -Eqx 'v[0-9]+\.[0-9]+\.[0-9]+' || fail "\"$version\" isn't a version like v1.2.3"
[ -d "$dir" ] || fail "no folder $dir"
dir="$(cd "$dir" && pwd)"

sha() {
  if command -v shasum >/dev/null 2>&1; then shasum -a 256 "$1"; else sha256sum "$1"; fi | cut -d ' ' -f 1
}

work="$(mktemp -d "${TMPDIR:-/tmp}/asqu-brew.XXXXXX")"
trap 'rm -rf "$work"' EXIT
trap 'exit 130' INT TERM

# The same three downloads brew-formula.sh lists.
for t in aarch64-apple-darwin x86_64-apple-darwin x86_64-unknown-linux-musl; do
  f="asqu-$t.tar.gz"
  [ -f "$dir/$f.sha256" ] || fail "no fingerprint for $f in $dir (Homebrew needs all three Mac and Linux builds)"
  expected="$(cut -d ' ' -f 1 <"$dir/$f.sha256")"
  printf '%s\n' "$expected" | grep -Eqx '[0-9a-f]{64}' || fail "$f.sha256 doesn't hold a fingerprint"
  curl -fsSL --retry 3 --max-time 120 "$base/$version/$f" -o "$work/$f" || fail "couldn't download $base/$version/$f"
  [ "$(sha "$work/$f")" = "$expected" ] || fail "the live $f doesn't match its fingerprint"
done

# Git as the tap's owner: any other saved sign-in is cleared for these commands only.
as_owner() {
  GIT_TERMINAL_PROMPT=0 git \
    -c credential.helper= \
    -c credential.https://github.com.helper= \
    -c "credential.https://github.com.helper=!f() { test \"\$1\" = get || exit 0; t=\$(gh auth token --user $owner) || exit 1; echo username=$owner; echo \"password=\$t\"; }; f" \
    "$@"
}

copy="$work/tap"
as_owner clone -q "$tap" "$copy" || fail "couldn't get a copy of the tap"
formula="$copy/Formula/asqu.rb"
[ -f "$formula" ] || fail "the tap has no Formula/asqu.rb"

new="${version#v}"
now="$(sed -n 's/^ *version "\([0-9.]*\)".*/\1/p' "$formula")"
newest="$(printf '%s\n%s\n' "$now" "$new" | sort -t . -k1,1n -k2,2n -k3,3n | tail -n 1)"
[ "$newest" = "$new" ] || fail "Homebrew already has $now, which is newer than $new"

sh "$here/brew-formula.sh" "$version" "$dir" >"$formula"
if git -C "$copy" diff --quiet; then
  echo "Homebrew already has $version."
  exit 0
fi
if [ "${ASQU_BREW_DRY_RUN:-}" = 1 ]; then
  echo "Dry run: would push this to the tap:"
  git -C "$copy" --no-pager diff
  exit 0
fi

# Commit as whoever commits to Asqu: the temporary copy doesn't get that folder's Git settings.
project="${ASQU_PROJECT_DIR:-$here/..}"
for setting in user.name user.email; do
  value="$(git -C "$project" config "$setting" || true)"
  [ -z "$value" ] || git -C "$copy" config "$setting" "$value"
done
git -C "$copy" commit -q -m "asqu $new" -- Formula/asqu.rb || fail "couldn't commit the formula"
as_owner -C "$copy" push -q origin HEAD || fail "couldn't push to the tap"
echo "Homebrew now has $version."
