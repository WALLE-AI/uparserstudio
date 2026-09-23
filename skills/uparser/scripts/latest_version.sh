#!/usr/bin/env bash
# latest_version.sh — resolve the newest uparser release that actually carries
# an asset for THIS platform, and compare version strings correctly.
#
# Why this exists as its own file: four scripts need the same answer
# (ensure_uparser.sh, find_uparser.sh, uparser-check.sh, uparser-parse.sh) and
# the platform→asset mapping, the TTL cache, the degradation ladder and the
# version comparison all have sharp edges that must not be reimplemented three
# times slightly differently.
#
# THE CORE POINT: "the latest release" is NOT "the latest release I can use".
# Release assets are published per platform and are incomplete — v0.4.0 carries
# only windows assets, v0.3.0 only linux. Resolving via /releases/latest would
# hand Linux a v0.4.0 that 404s on download, which then degrades silently into
# a multi-minute from-source build. So we scan the release list and take the
# newest one whose asset list actually contains our platform's file.
#
# Usage:
#   latest_version.sh [--platform <p>] [--suffix <s>] [--json] [--refresh]
#   . latest_version.sh --source-only     # just define the functions, resolve nothing
#
# stdout: one line — the version WITHOUT the leading "v" (e.g. "0.4.0")
#         with --json: {"version":..,"source":..,"asset":..,"age_secs":..}
# exit:   0 resolved · 3 nothing nameable for this platform (caller builds)
#
# `source` tells the caller HOW authoritative the answer is, and callers MUST
# branch on it — see the anti-downgrade rule in ensure_uparser.sh:
#   user_pin       $UPARSER_VERSION was set              → authoritative
#   api            a live API call matched an asset      → authoritative
#   cache          a fresh previous api answer           → advisory
#   last_known_good a stale previous api answer          → advisory
#   api_no_asset   API answered, no asset for us         → advisory
#   fallback_pin   built-in table, network never worked  → advisory
set -euo pipefail

REPO="${UPARSER_REPO:-WALLE-AI/uparserstudio}"
API_BASE="${UPARSER_API_BASE:-https://api.github.com}"   # overridable ONLY so the
                                                         # failure ladder is testable
SKILL_STATE="${UPARSER_SKILL_HOME:-$HOME/.cache/uparser-skill}"

# Deliberately NOT under $HOME/.cache/uparser: `uparser cache clear` is
# remove_dir_all() on exactly that directory (uparser-core/src/cache.rs), so
# state kept there is silently destroyed by a routine user action.

TTL_OK="${UPARSER_VERSION_TTL:-21600}"        # 6h. Anonymous GitHub API is 60 req/hr
                                              # per IP; this keeps a busy machine to a
                                              # handful of calls per day.
TTL_ERR="${UPARSER_VERSION_ERROR_TTL:-900}"   # 15m negative cache. Without this an
                                              # offline 100-file batch pays 100 connect
                                              # timeouts. There is no API mirror to fall
                                              # back on: ghfast.top proxies release
                                              # DOWNLOADS, not api.github.com (403).

# Built-in fallback pins: the newest release known to carry each platform's
# asset at the time of writing. Only ever used when the network never worked.
fallback_pin() {
  case "$1" in
    windows-x86_64) echo "0.4.0" ;;
    linux-x86_64)   echo "0.3.0" ;;
    *)              echo "" ;;
  esac
}

# ---------------------------------------------------------------------------
# ver_cmp <a> <b> -> prints -1 | 0 | 1 | ?
#
# `?` means "could not parse" and callers MUST treat it as "keep what you have"
# rather than coercing to 0.0.0, which would silently downgrade a working but
# oddly-versioned binary.
#
# Do NOT replace this with `sort -V`. Verified on this machine: sort -V orders
# 0.4.0 BEFORE 0.4.0-rc.2, i.e. it thinks the release candidate is newer than
# the release. Semver says the opposite, and getting it backwards means always
# being exactly one release wrong, in the wrong direction.
# ---------------------------------------------------------------------------
ver_cmp() {
  awk -v a="${1#v}" -v b="${2#v}" 'BEGIN{
    if (a !~ /^[0-9]+([.][0-9]+)*([-+].*)?$/ || b !~ /^[0-9]+([.][0-9]+)*([-+].*)?$/) { print "?"; exit }
    ai=index(a,"-"); if(ai){pa=substr(a,ai+1); a=substr(a,1,ai-1)} else pa=""
    bi=index(b,"-"); if(bi){pb=substr(b,bi+1); b=substr(b,1,bi-1)} else pb=""
    sub(/[+].*/,"",pa); sub(/[+].*/,"",pb)          # build metadata is not ordered
    sub(/[+].*/,"",a);  sub(/[+].*/,"",b)
    n=split(a,x,"."); m=split(b,y,".")
    for(i=1;i<=3;i++){ u=(i<=n)?x[i]+0:0; v=(i<=m)?y[i]+0:0
                       if(u!=v){ print (u<v)?-1:1; exit } }
    if(pa==""&&pb==""){ print 0;  exit }
    if(pa==""){ print 1;  exit }                    # release beats its own prerelease
    if(pb==""){ print -1; exit }
    n=split(pa,x,"."); m=split(pb,y,"."); k=(n>m)?n:m
    for(i=1;i<=k;i++){
      if(i>n){ print -1; exit }                     # shorter prerelease chain is lower
      if(i>m){ print 1;  exit }
      xn=(x[i] ~ /^[0-9]+$/); yn=(y[i] ~ /^[0-9]+$/)
      if(xn&&yn){ if(x[i]+0!=y[i]+0){ print (x[i]+0<y[i]+0)?-1:1; exit } }
      else if(xn){ print -1; exit }                 # numeric identifier < alphanumeric
      else if(yn){ print 1;  exit }
      else if(x[i]!=y[i]){ print (x[i]<y[i])?-1:1; exit } }
    print 0 }'
}

# Extract a version from `<bin> --version` output ("uparser 0.4.0-rc.2").
# Prints nothing when it cannot parse — callers treat that as "keep it".
bin_version() {
  [ -x "$1" ] || return 0
  "$1" --version 2>/dev/null | awk 'NR==1{print $2}' \
    | sed -E 's/^[^0-9]*//; s/[^0-9A-Za-z.+-].*$//' || true
}

# platform triple for this host, or "" when unsupported
detect_platform() {
  case "$(uname -s)" in
    Linux)                 os=linux ;;
    MINGW*|MSYS*|CYGWIN*)  os=windows ;;
    Darwin)                os=macos ;;
    *)                     echo ""; return ;;
  esac
  case "$(uname -m)" in
    x86_64|amd64) arch=x86_64 ;;
    *)            echo ""; return ;;
  esac
  echo "$os-$arch"
}

# the filename suffix that platform's binary asset carries
platform_suffix() {
  case "$1" in
    windows-*) echo ".exe" ;;
    *)         echo "" ;;
  esac
}

now_secs() { date +%s; }

# ---------------------------------------------------------------------------
# Everything above is reusable; stop here when sourced with --source-only.
# ---------------------------------------------------------------------------
case "${1:-}" in --source-only) return 0 2>/dev/null || exit 0 ;; esac

PLAT=""; SFX=""; AS_JSON=0; REFRESH=0
while [ $# -gt 0 ]; do
  case "$1" in
    --platform) PLAT="$2"; shift 2 ;;
    --suffix)   SFX="$2";  shift 2 ;;
    --json)     AS_JSON=1; shift ;;
    --refresh)  REFRESH=1; shift ;;
    --source-only) shift ;;
    *) echo "latest_version.sh: unknown arg: $1" >&2; exit 1 ;;
  esac
done

[ -n "$PLAT" ] || PLAT="$(detect_platform)"
if [ -z "$PLAT" ]; then
  echo "latest_version.sh: unsupported platform $(uname -s)-$(uname -m)" >&2
  exit 3
fi
# An explicitly passed --platform with no --suffix still needs the right suffix.
[ -n "$SFX" ] || SFX="$(platform_suffix "$PLAT")"

emit() { # <version> <source> <asset> <age>
  if [ "$AS_JSON" -eq 1 ]; then
    printf '{"version":"%s","source":"%s","asset":"%s","age_secs":%s}\n' "$1" "$2" "$3" "$4"
  else
    printf '%s\n' "$1"
  fi
}

# 1) explicit pin — authoritative, no network, no cache write
if [ -n "${UPARSER_VERSION:-}" ]; then
  v="${UPARSER_VERSION#v}"
  echo "latest_version: using pinned \$UPARSER_VERSION=$v" >&2
  emit "$v" user_pin "uparser-v$v-$PLAT$SFX" 0
  exit 0
fi

STATE="$SKILL_STATE/latest-$PLAT.json"
cached_field() { # <field>
  [ -f "$STATE" ] || return 1
  grep -o "\"$1\"[[:space:]]*:[[:space:]]*\"\?[^,\"}]*" "$STATE" 2>/dev/null \
    | sed 's/.*:[[:space:]]*"\?//' | head -1
}
c_ts="$(cached_field stored_at || true)"
c_ver="$(cached_field version || true)"
c_src="$(cached_field source || true)"
c_asset="$(cached_field asset || true)"
age=-1
if [ -n "${c_ts:-}" ]; then age=$(( $(now_secs) - c_ts )); fi

# 2) fresh positive cache
if [ "$REFRESH" -eq 0 ] && [ -n "${c_ver:-}" ] && [ "$c_src" != "error" ] \
   && [ "$age" -ge 0 ] && [ "$age" -le "$TTL_OK" ]; then
  emit "$c_ver" cache "${c_asset:-}" "$age"; exit 0
fi

# 3) fresh negative cache — skip the network entirely
skip_net=0
if [ "$REFRESH" -eq 0 ] && [ "${c_src:-}" = "error" ] && [ "$age" -ge 0 ] && [ "$age" -le "$TTL_ERR" ]; then
  skip_net=1
fi
[ "${UPARSER_OFFLINE:-0}" = "1" ] && skip_net=1

write_state() { # <version> <source> <asset>
  mkdir -p "$SKILL_STATE" 2>/dev/null || return 0
  printf '{"stored_at":%s,"version":"%s","asset":"%s","source":"%s"}\n' \
    "$(now_secs)" "$1" "$3" "$2" > "$STATE.$$" 2>/dev/null && mv -f "$STATE.$$" "$STATE" 2>/dev/null || true
}

# 4) live API call
if [ "$skip_net" -eq 0 ]; then
  TOKEN="${UPARSER_GITHUB_TOKEN:-${GITHUB_TOKEN:-}}"
  body="$(curl -fsSL --connect-timeout 8 --max-time 20 \
            -H 'Accept: application/vnd.github+json' \
            -H 'User-Agent: uparser-skill' \
            ${TOKEN:+-H "Authorization: Bearer $TOKEN"} \
            "$API_BASE/repos/$REPO/releases?per_page=100" 2>/dev/null || true)"
  # per_page=100, not 30: once the repo has >30 releases, a platform whose
  # newest asset is older than #30 would silently resolve to nothing.

  # The sed below splits the payload so that every key we care about starts a
  # line. This is NOT cosmetic: GitHub returns this endpoint MINIFIED (the whole
  # release list on one line) under some content negotiations and pretty-printed
  # under others, and a line-oriented state machine silently matches nothing on
  # the minified form — resolving to the fallback pin while looking like it
  # worked. Normalizing first makes the parse independent of that.
  if [ -n "$body" ]; then
    hit="$(printf '%s' "$body" \
      | sed -E 's/("tag_name"|"draft"|"prerelease"|"name")[[:space:]]*:/\n&/g' \
      | grep -E '"tag_name"|"draft"|"prerelease"|"name"' \
      | awk -v plat="$PLAT" -v sfx="$SFX" -v allow_pre="${UPARSER_PRERELEASE:-0}" '
          # Match the asset name EXACTLY against a constructed filename. A regex
          # on "uparser-" would also match the release objects own "name"
          # field ("v0.4.0") and sibling assets like SHA256SUMS / the .zip.
          function flush(){
            if(!done && tag!="" && found==1 && draft==0 && (allow_pre==1||pre==0)){
              done=1; print tag "\t" want; exit } }
          /"tag_name"[[:space:]]*:/ {
            flush()
            t=$0; sub(/.*"tag_name"[^:]*:[[:space:]]*"/,"",t); sub(/".*/,"",t)
            tag=t; found=0; pre=0; draft=0; want="uparser-" tag "-" plat sfx; next }
          /"draft"[[:space:]]*:[[:space:]]*true/      { draft=1; next }
          /"prerelease"[[:space:]]*:[[:space:]]*true/ { pre=1;   next }
          /"name"[[:space:]]*:/ {
            n=$0; sub(/.*"name"[^:]*:[[:space:]]*"/,"",n); sub(/".*/,"",n)
            if(n==want) found=1; next }
          # `exit` inside a function still runs END, so without the !done guard
          # flush() fires twice and prints the tag twice — which would make the
          # callers $(...) a two-line string and break every path check.
          END { flush() }')"
    if [ -n "$hit" ]; then
      tag="${hit%%$(printf '\t')*}"; asset="${hit##*$(printf '\t')}"
      v="${tag#v}"
      write_state "$v" api "$asset"
      emit "$v" api "$asset" 0; exit 0
    fi
    # API answered fine, but no release carries our asset. That is a stable
    # fact, not a transient failure, so cache it with the SUCCESS ttl.
    echo "latest_version: no published release carries an asset for $PLAT" >&2
    if [ -n "${c_ver:-}" ] && [ "$c_src" != "error" ]; then
      emit "$c_ver" last_known_good "${c_asset:-}" "$age"; exit 0
    fi
    p="$(fallback_pin "$PLAT")"
    if [ -n "$p" ]; then write_state "$p" api_no_asset "uparser-v$p-$PLAT$SFX"
                         emit "$p" api_no_asset "uparser-v$p-$PLAT$SFX" 0; exit 0; fi
    exit 3
  fi
  # transport/HTTP failure
  echo "latest_version: GitHub API unreachable; falling back (no API mirror exists)" >&2
  write_state "${c_ver:-}" error "${c_asset:-}"
fi

# 5) serve stale — the primary anti-downgrade mechanism
if [ -n "${c_ver:-}" ]; then
  emit "$c_ver" last_known_good "${c_asset:-}" "$age"; exit 0
fi

# 6) built-in pin
p="$(fallback_pin "$PLAT")"
if [ -n "$p" ]; then emit "$p" fallback_pin "uparser-v$p-$PLAT$SFX" -1; exit 0; fi

# 7) nothing nameable — caller builds from source
echo "latest_version: no fallback pin for $PLAT" >&2
exit 3
