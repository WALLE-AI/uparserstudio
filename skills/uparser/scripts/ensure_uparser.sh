#!/usr/bin/env bash
# ensure_uparser.sh — guarantee a runnable `uparser` binary is present, and
# print its absolute path on stdout. Resolution order:
#   1) `uparser` already on PATH
#   2) previously downloaded copy in the cache
#   3) download a version-pinned prebuilt from GitHub Releases (direct, then
#      ghfast.top mirror), verify its sha256, and smoke-test it
#   4) on any failure / unsupported platform, fall back to building from source
#      via find_uparser.sh --build
#
# Env overrides: UPARSER_VERSION, UPARSER_REPO, UPARSER_HOME (cache root).
set -euo pipefail

# Pinned to the last release that published a linux-x86_64 asset. v0.2.0
# only shipped a Windows binary (built from a Windows machine with no Linux
# cross-toolchain available); bumping this pin without a matching asset
# would make every Linux/WSL skill user silently fall back to a from-source
# build. Bump this once a linux-x86_64 asset exists for a newer release.
VERSION="${UPARSER_VERSION:-0.1.1}"
REPO="${UPARSER_REPO:-WALLE-AI/uparserstudio}"
CACHE="${UPARSER_HOME:-$HOME/.cache/uparser}/bin"
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

# 1) already on PATH
if command -v uparser >/dev/null 2>&1; then command -v uparser; exit 0; fi
# 2) cached download
if [ -x "$CACHE/uparser" ]; then echo "$CACHE/uparser"; exit 0; fi

# 3) map platform -> release asset
os="$(uname -s)"; arch="$(uname -m)"
case "$os-$arch" in
  Linux-x86_64|Linux-amd64) asset="uparser-v$VERSION-linux-x86_64" ;;
  *)
    echo "no prebuilt binary published for $os-$arch — building from source" >&2
    exec "$HERE/find_uparser.sh" --build ;;
esac

base="https://github.com/$REPO/releases/download/v$VERSION"
tmp="$(mktemp -d)"; trap 'rm -rf "$tmp"' EXIT
mkdir -p "$CACHE"

# fetch <url> <dest>: try direct, then the ghfast.top mirror (needed on
# networks that can't reach github.com's download host directly). Both attempts
# abort quickly if the transfer stalls (<3KB/s for 8s) so a dead direct host
# doesn't burn the whole budget before the mirror is tried.
fetch() {
  local dl="--connect-timeout 8 --speed-limit 3000 --speed-time 8"
  # shellcheck disable=SC2086
  curl -fsSL $dl --max-time 60  -o "$2" "$1" 2>/dev/null && return 0
  # shellcheck disable=SC2086
  curl -fsSL $dl --max-time 240 -o "$2" "https://ghfast.top/$1" 2>/dev/null
}

echo "downloading $asset (v$VERSION) ..." >&2
if ! fetch "$base/$asset" "$tmp/uparser"; then
  echo "download failed (direct + mirror) — building from source" >&2
  exec "$HERE/find_uparser.sh" --build
fi

# verify checksum when SHA256SUMS is available (best-effort, not fatal if absent)
if fetch "$base/SHA256SUMS" "$tmp/SHA256SUMS"; then
  want="$(awk -v n="$asset" '$2==n {print $1}' "$tmp/SHA256SUMS")"
  got="$(sha256sum "$tmp/uparser" | awk '{print $1}')"
  if [ -n "$want" ] && [ "$want" != "$got" ]; then
    echo "checksum mismatch for $asset (want $want, got $got) — refusing" >&2
    exit 2
  fi
fi

chmod +x "$tmp/uparser"
# smoke test — catches a glibc-too-old binary that downloaded fine but won't run
if ! "$tmp/uparser" protocols >/dev/null 2>&1; then
  echo "downloaded binary won't run here (likely glibc < 2.35) — building from source" >&2
  exec "$HERE/find_uparser.sh" --build
fi

mv "$tmp/uparser" "$CACHE/uparser"
echo "$CACHE/uparser"
