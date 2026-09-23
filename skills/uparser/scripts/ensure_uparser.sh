#!/usr/bin/env bash
# ensure_uparser.sh — guarantee a runnable, reasonably CURRENT `uparser` is
# present, and print its absolute path on stdout. This is THE entry point;
# find_uparser.sh is now only the from-source rung underneath it.
#
# Resolution order:
#   0) $UPARSER_BIN                    explicit, wins unconditionally, never version-checked
#   1) resolve the target version      latest_version.sh (TTL-cached, degrades offline)
#   2) a local cargo workspace build   used when it is not older than the target
#   3) `uparser` on PATH               used when it is not older than the target
#   4) the versioned download cache    $UPARSER_HOME/versions/v<V>/<platform>/
#   5) download from GitHub Releases   direct -> ghfast.top mirror, sha256, smoke test
#   6) build from source               find_uparser.sh --build
#
# ---------------------------------------------------------------------------
# THE ANTI-DOWNGRADE INVARIANT — read before changing anything below.
#
# Two independent guards, and both are needed:
#
#  (a) A supersede only ever happens when the local binary is strictly OLDER
#      than the target (ver_cmp == -1). This alone makes a literal downgrade
#      impossible whatever the target turned out to be, and is why a
#      developer's 0.5.0-dev survives contact with an older published release.
#
#  (b) latest_version.sh reports HOW it got its answer via `source`, and only
#      an answer that came from GitHub (`api`, or a fresh `cache` entry, which
#      IS a previous api answer inside its TTL) or an explicit `user_pin` may
#      trigger a DOWNLOAD. `fallback_pin` / `api_no_asset` / `last_known_good`
#      are guesses made while the network was unavailable; acting on them can
#      only waste a doomed download or chase a hardcoded constant that has
#      drifted. They may still step aside for a binary that is already in the
#      cache, since that costs nothing.
#
# Note that `cache` MUST count as authoritative. Treating it as a guess looks
# safer but silently defeats the entire feature: after the first successful
# lookup, every call for the next TTL window would decline to upgrade, so the
# upgrade would only ever fire on the rare call that happens to refresh the
# cache. Guard (a) is what makes this safe.
# ---------------------------------------------------------------------------
#
# Env: UPARSER_BIN, UPARSER_VERSION, UPARSER_REPO, UPARSER_HOME,
#      UPARSER_SKILL_HOME, UPARSER_OFFLINE, UPARSER_PRERELEASE,
#      UPARSER_VERSION_TTL, UPARSER_PREFER_WORKSPACE, GITHUB_TOKEN.
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO="${UPARSER_REPO:-WALLE-AI/uparserstudio}"
CACHE_ROOT="${UPARSER_HOME:-$HOME/.cache/uparser}"

# 0) explicit binary — the escape hatch for an unreleased workspace build.
#    Never version-checked, never second-guessed.
if [ -n "${UPARSER_BIN:-}" ]; then
  [ -x "$UPARSER_BIN" ] || { echo "UPARSER_BIN is not executable: $UPARSER_BIN" >&2; exit 2; }
  case "$UPARSER_BIN" in
    /*) echo "$UPARSER_BIN" ;;
    *)  (cd "$(dirname "$UPARSER_BIN")" && printf '%s/%s\n' "$PWD" "$(basename "$UPARSER_BIN")") ;;
  esac
  exit 0
fi

# ver_cmp / bin_version / detect_platform / platform_suffix
. "$HERE/latest_version.sh" --source-only

PLAT="$(detect_platform)"
SFX="$(platform_suffix "${PLAT:-}")"

# --- resolved-path memo ------------------------------------------------------
# uparser-run.sh now calls this script on EVERY invocation (it used to
# short-circuit to PATH, which quietly disabled the upgrade ladder). Doing the
# full resolution each time costs ~4s on Windows, almost all of it process
# spawning — which a 100-file batch would pay 100 times. So remember the answer
# for as long as the version answer itself is considered fresh.
#
# Safe to memoize a PATH: if the binary at that path is later rebuilt or
# upgraded in place the path does not change, and the TTL bounds how long a
# newly published release goes unnoticed. `--refresh` forces a re-resolve.
SKILL_STATE="${UPARSER_SKILL_HOME:-$HOME/.cache/uparser-skill}"
MEMO="$SKILL_STATE/resolved-${PLAT:-unknown}.txt"
MEMO_TTL="${UPARSER_VERSION_TTL:-21600}"
REFRESH=0
[ "${1:-}" = "--refresh" ] && REFRESH=1

if [ "$REFRESH" -eq 0 ] && [ -f "$MEMO" ]; then
  memo_ts="$(awk '{print $1}' "$MEMO" 2>/dev/null || true)"
  memo_path="$(cut -f2- "$MEMO" 2>/dev/null || true)"
  if [ -n "${memo_ts:-}" ] && [ -n "${memo_path:-}" ] && [ -x "$memo_path" ]; then
    if [ $(( $(date +%s) - memo_ts )) -le "$MEMO_TTL" ]; then
      echo "$memo_path"; exit 0
    fi
  fi
fi

remember() {
  mkdir -p "$SKILL_STATE" 2>/dev/null || return 0
  printf '%s\t%s\n' "$(date +%s)" "$1" > "$MEMO.$$" 2>/dev/null \
    && mv -f "$MEMO.$$" "$MEMO" 2>/dev/null || true
}
# Every exit path below goes through this so the memo cannot drift from what we
# actually returned.
answer() { remember "$1"; echo "$1"; exit 0; }

# 1) target version + how authoritative it is
resolved="$("$HERE/latest_version.sh" --json 2>/dev/null || true)"
if [ -n "$resolved" ]; then
  TARGET="$(printf '%s' "$resolved" | sed -n 's/.*"version":"\([^"]*\)".*/\1/p')"
  TSRC="$(printf '%s' "$resolved"  | sed -n 's/.*"source":"\([^"]*\)".*/\1/p')"
  ASSET="$(printf '%s' "$resolved" | sed -n 's/.*"asset":"\([^"]*\)".*/\1/p')"
else
  TARGET=""; TSRC="none"; ASSET=""
fi

# See guard (b) above: `cache` is a previous `api` answer inside its TTL, so it
# is authoritative too. `last_known_good` is the same data gone stale after the
# network failed, which is a different thing and stays advisory.
authoritative=0
case "$TSRC" in api|cache|user_pin) authoritative=1 ;; esac

CACHE="$CACHE_ROOT/versions/v$TARGET/${PLAT:-unknown}"
CACHED_BIN="$CACHE/uparser$SFX"

# Decide what to do with a local candidate at $1 described as $2.
# Prints the path and returns 0 when the candidate should be used as-is.
# Returns 1 when the caller should move on (i.e. we want the target instead).
consider() {
  cand="$1"; label="$2"
  [ -x "$cand" ] || return 1
  [ -n "$TARGET" ] || { echo "$cand"; return 0; }   # nothing to compare against

  lv="$(bin_version "$cand")"
  if [ -z "$lv" ]; then
    echo "uparser: could not read a version from $label ($cand) — keeping it" >&2
    echo "$cand"; return 0
  fi
  cmp="$(ver_cmp "$lv" "$TARGET")"
  case "$cmp" in
    0)  echo "$cand"; return 0 ;;                    # already current
    1)  # locally newer than anything published — a dev build. Keep it.
        if [ "$authoritative" -eq 1 ]; then
          echo "uparser: $label is $lv, newer than the newest published release $TARGET — keeping it" >&2
        fi
        echo "$cand"; return 0 ;;
    '?') echo "uparser: unparseable version '$lv' from $label — keeping it" >&2
         echo "$cand"; return 0 ;;
  esac

  # cmp == -1: the candidate is older than the target.
  if [ "$authoritative" -eq 1 ]; then
    echo "uparser: $label is $lv; using newer $TARGET instead ($label is left untouched;" >&2
    echo "        set UPARSER_BIN to override, or UPARSER_VERSION=$lv to pin)" >&2
    return 1
  fi
  # Advisory target. Only step aside if the newer build is ALREADY downloaded;
  # never download to satisfy a pin/cache we could not confirm against the API.
  if [ -x "$CACHED_BIN" ]; then
    echo "uparser: $label is $lv; using already-cached $TARGET" >&2
    return 1
  fi
  echo "$cand"; return 0
}

# 2) a local cargo workspace build.
#    This rung exists because find_uparser.sh's own comment warns about exactly
#    the opposite failure: a developer with a fresh local build silently getting
#    an older downloaded release instead.
if [ "${UPARSER_PREFER_WORKSPACE:-0}" = "1" ]; then
  ws_bin="$(UPARSER_ENSURE_INTERNAL=1 "$HERE/find_uparser.sh" --locate-only 2>/dev/null || true)"
  if [ -n "$ws_bin" ] && [ -x "$ws_bin" ]; then answer "$ws_bin"; fi
else
  ws_bin="$(UPARSER_ENSURE_INTERNAL=1 "$HERE/find_uparser.sh" --locate-only 2>/dev/null || true)"
  if [ -n "$ws_bin" ]; then
    if out="$(consider "$ws_bin" "the local workspace build")"; then answer "$out"; fi
  fi
fi

# 3) PATH
path_bin="$(command -v uparser 2>/dev/null || true)"
if [ -n "$path_bin" ]; then
  if out="$(consider "$path_bin" "PATH uparser")"; then answer "$out"; fi
fi

# 4) already downloaded?
if [ -x "$CACHED_BIN" ]; then answer "$CACHED_BIN"; fi

# From here we need to download, which requires both a target and a platform we
# publish for. Anything missing -> build from source.
if [ -z "$TARGET" ] || [ -z "${PLAT:-}" ] || [ -z "$ASSET" ]; then
  echo "uparser: no published binary nameable for this platform — building from source" >&2
  exec "$HERE/find_uparser.sh" --build
fi

base="https://github.com/$REPO/releases/download/v$TARGET"
tmp="$(mktemp -d)"; trap 'rm -rf "$tmp"' EXIT
mkdir -p "$CACHE"

# fetch <url> <dest>: direct, then the ghfast.top mirror (needed on networks
# that cannot reach github.com's download host). Both attempts abort quickly if
# the transfer stalls so a dead direct host does not burn the whole budget.
# NOTE: ghfast.top mirrors release DOWNLOADS only — it does NOT proxy
# api.github.com (verified: 403), which is why version resolution has no mirror.
fetch() {
  local dl="--connect-timeout 8 --speed-limit 3000 --speed-time 8"
  # shellcheck disable=SC2086
  curl -fsSL $dl --max-time 60  -o "$2" "$1" 2>/dev/null && return 0
  # shellcheck disable=SC2086
  curl -fsSL $dl --max-time 240 -o "$2" "https://ghfast.top/$1" 2>/dev/null
}

echo "uparser: downloading $ASSET (v$TARGET) ..." >&2
if ! fetch "$base/$ASSET" "$tmp/uparser$SFX"; then
  echo "uparser: download failed (direct + mirror) — building from source" >&2
  exec "$HERE/find_uparser.sh" --build
fi

# checksum, when SHA256SUMS is available (best-effort, absent is not fatal)
if fetch "$base/SHA256SUMS" "$tmp/SHA256SUMS"; then
  want="$(awk -v n="$ASSET" '$2==n {print $1}' "$tmp/SHA256SUMS")"
  got="$(sha256sum "$tmp/uparser$SFX" | awk '{print $1}')"
  if [ -n "$want" ] && [ "$want" != "$got" ]; then
    echo "uparser: checksum mismatch for $ASSET (want $want, got $got) — refusing" >&2
    exit 2
  fi
fi

# Windows needs pdfium.dll beside the exe or every rasterization/vision protocol
# fails at runtime while `native` keeps working — i.e. it looks half-fine.
if [ "$SFX" = ".exe" ]; then
  dll="uparser-v$TARGET-$PLAT-pdfium.dll"
  if fetch "$base/$dll" "$tmp/pdfium.dll"; then
    if [ -f "$tmp/SHA256SUMS" ]; then
      dwant="$(awk -v n="$dll" '$2==n {print $1}' "$tmp/SHA256SUMS")"
      dgot="$(sha256sum "$tmp/pdfium.dll" | awk '{print $1}')"
      if [ -n "$dwant" ] && [ "$dwant" != "$dgot" ]; then
        echo "uparser: checksum mismatch for $dll — refusing" >&2; exit 2
      fi
    fi
    mv -f "$tmp/pdfium.dll" "$CACHE/pdfium.dll"
  else
    echo "uparser: warning — could not fetch $dll; PDF rasterization, OCR and" >&2
    echo "        vision protocols will fail until it is present beside the exe" >&2
  fi
fi

chmod +x "$tmp/uparser$SFX" 2>/dev/null || true
# smoke test — catches a binary that downloaded fine but will not run here
# (glibc too old, wrong arch, truncated file)
if ! "$tmp/uparser$SFX" protocols >/dev/null 2>&1; then
  echo "uparser: the downloaded binary will not run here — building from source" >&2
  exec "$HERE/find_uparser.sh" --build
fi

mv -f "$tmp/uparser$SFX" "$CACHED_BIN"
answer "$CACHED_BIN"
