#!/bin/sh
# Puts a finished release on Cloudflare R2, where asqu.dev/download/ sends installers,
# `asqu update` and Homebrew. scripts/release.sh runs it after building; it can also be
# run by hand on a folder of finished downloads:
#
#   sh scripts/upload.sh <folder with the release's asqu-* files> <version>
#
# Each file goes to two places in the "asqu-downloads" bucket: <version>/<file>, which never
# changes (Homebrew needs that), and <file>, the latest. VERSION goes last, so nothing ever
# sees the new version number before its files are there.
#
# The key: an R2 token kept in this Mac's Keychain as "Cloudflare R2" (account "api-token"),
# or passed in ASQU_R2_TOKEN. It can only read and write files in this one bucket.
# The Cloudflare account ID: kept in the Keychain as "Cloudflare R2" (account "account-id"),
# or passed in ASQU_R2_ACCOUNT. Neither is ever written in this repo.
#
# Settings (optional):
#   ASQU_UPLOAD_DRY_RUN=1   check everything and list what would be sent, send nothing
set -eu

dir="${1:?usage: sh scripts/upload.sh <folder> <version>}"
version="${2:?usage: sh scripts/upload.sh <folder> <version>}"
bucket="asqu-downloads"
fail() { printf 'upload: %s\n' "$*" >&2; exit 1; }
account="${ASQU_R2_ACCOUNT:-$(security find-generic-password -s "Cloudflare R2" -a account-id -w 2>/dev/null || true)}"
[ -n "$account" ] || fail "no Cloudflare account ID: it isn't in the Keychain as \"Cloudflare R2\" (account \"account-id\")"
endpoint="https://$account.r2.cloudflarestorage.com/$bucket"

case "$version" in v[0-9]*.[0-9]*.[0-9]*) ;; *) fail "\"$version\" isn't a version like v1.2.3" ;; esac
[ -d "$dir" ] || fail "no folder $dir"
cd "$dir"

# Nothing is sent unless every download is there, matches its fingerprint and is signed.
set -- $(ls asqu-*.tar.gz asqu-*.zip 2>/dev/null || true)
[ "$#" -gt 0 ] || fail "no asqu-* downloads in $dir"
files=""
for f in "$@"; do
  [ -f "$f.sha256" ] || fail "$f has no fingerprint ($f.sha256)"
  [ -f "$f.sig" ] || fail "$f isn't signed ($f.sig)"
  [ "$(cut -d ' ' -f 1 <"$f.sha256")" = "$(shasum -a 256 "$f" | cut -d ' ' -f 1)" ] \
    || fail "$f doesn't match its fingerprint"
  files="$files $f $f.sha256 $f.sig"
done

if [ "${ASQU_UPLOAD_DRY_RUN:-}" = 1 ]; then
  echo "Dry run: would send to $bucket:"
  for f in $files; do echo "  $version/$f and $f"; done
  echo "  VERSION ($version), last"
  exit 0
fi

if [ -z "${ASQU_R2_TOKEN:-}" ]; then
  ASQU_R2_TOKEN="$(security find-generic-password -s "Cloudflare R2" -a api-token -w 2>/dev/null || true)"
  [ -n "$ASQU_R2_TOKEN" ] || fail "no R2 token: it isn't in the Keychain as \"Cloudflare R2\""
fi
# R2's S3 address takes the token's id as the key name and its SHA-256 as the secret.
key_id="$(curl -fsS -H "Authorization: Bearer $ASQU_R2_TOKEN" \
  "https://api.cloudflare.com/client/v4/accounts/$account/tokens/verify" \
  | sed -n 's/.*"id":"\([0-9a-f]*\)".*/\1/p')"
[ -n "$key_id" ] || fail "Cloudflare didn't accept the R2 token"
secret="$(printf %s "$ASQU_R2_TOKEN" | shasum -a 256 | cut -d ' ' -f 1)"
unset ASQU_R2_TOKEN

# The key goes to curl in a config file on stdin, so it never shows in the process list.
put() { # <file> <name in the bucket> <cache rule>
  printf 'user = "%s:%s"\n' "$key_id" "$secret" | curl -fsS -o /dev/null -K - \
    --aws-sigv4 "aws:amz:auto:s3" -X PUT \
    -H "x-amz-content-sha256: UNSIGNED-PAYLOAD" \
    -H "Content-Type: application/octet-stream" \
    -H "Cache-Control: $3" \
    --data-binary "@$1" "$endpoint/$2" || fail "couldn't send $1"
}

for f in $files; do put "$f" "$version/$f" "public, max-age=31536000, immutable"; done
for f in $files; do put "$f" "$f" "public, max-age=300"; done
tmp="$(mktemp "${TMPDIR:-/tmp}/asqu-version.XXXXXX")"
printf '%s\n' "$version" >"$tmp"
put "$tmp" VERSION "public, max-age=300"
rm -f "$tmp"
echo "Sent $version to $bucket ($(echo $files | wc -w | tr -d ' ') files, plus VERSION)."
