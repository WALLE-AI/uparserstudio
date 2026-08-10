#!/usr/bin/env bash
# uparser-run.sh — thin wrapper that injects --endpoint/--model from a config
# file so you don't have to pass them on every `parse` call (esp. handy when
# moving between machines / vLLM endpoints). It does NOT modify the binary.
#
# Config file (simple INI): $UPARSER_CONFIG, else ~/.config/uparser/config.toml
#   [mineru-vlm]
#   endpoint = http://10.0.0.5:19122/v1/chat/completions
#   model    = MinerU2.5-2604-1.2B
#
# Precedence: an explicit --endpoint/--model on the command line ALWAYS wins;
# the config only fills in what you omitted. --endpoint is injected for `parse`
# and `doctor`; --model only for `parse`.
#
# Usage: uparser-run.sh parse --protocol mineru-vlm doc.pdf
set -euo pipefail

CONFIG="${UPARSER_CONFIG:-$HOME/.config/uparser/config.toml}"

# --- locate the real binary: PATH first, else ensure_uparser.sh downloads a
#     version-pinned prebuilt from GitHub Releases (or builds from source) ---
BIN="$(command -v uparser || true)"
if [ -z "$BIN" ]; then
  here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
  BIN="$("$here/ensure_uparser.sh" | tail -1 || true)"
fi
[ -n "$BIN" ] && [ -x "$BIN" ] || { echo "uparser binary not found and could not be downloaded/built" >&2; exit 2; }

args=("$@")

# --- scan args: subcommand, protocol, and whether endpoint/model were given ---
# `parse` takes the protocol via --protocol; `doctor` takes it as the positional
# token right after the subcommand (doctor <protocol> [--endpoint <url>]).
sub=""; protocol="mock"; has_ep=0; has_model=0
for ((i=0; i<${#args[@]}; i++)); do
  case "${args[$i]}" in
    parse|classify|doctor|protocols|cache)
      if [ -z "$sub" ]; then
        sub="${args[$i]}"
        # doctor's protocol is positional: the next non-flag token
        if [ "$sub" = "doctor" ]; then
          nxt="${args[$((i+1))]:-}"
          case "$nxt" in ""|-*) ;; *) protocol="$nxt" ;; esac
        fi
      fi ;;
    --protocol)   protocol="${args[$((i+1))]:-mock}" ;;
    --protocol=*) protocol="${args[$i]#--protocol=}" ;;
    --endpoint|--endpoint=*) has_ep=1 ;;
    --model|--model=*)       has_model=1 ;;
  esac
done

# --- read one key from an INI section (strips optional surrounding quotes) ---
read_ini() { # $1=section $2=key
  [ -f "$CONFIG" ] || return 0
  awk -v s="[$1]" -v k="$2" '
    /^[[:space:]]*\[/ { cur=$0; gsub(/^[[:space:]]+|[[:space:]]+$/,"",cur) }
    cur==s && $0 ~ "^[[:space:]]*"k"[[:space:]]*=" {
      sub(/^[^=]*=[[:space:]]*/,"")
      gsub(/^["'"'"']|["'"'"'][[:space:]]*$/,"")
      print; exit
    }' "$CONFIG"
}

# --- inject --endpoint for `parse` and `doctor`; --model only for `parse` ---
if [ "$sub" = "parse" ] || [ "$sub" = "doctor" ]; then
  if [ "$has_ep" -eq 0 ]; then ep="$(read_ini "$protocol" endpoint)"; [ -n "$ep" ] && args+=(--endpoint "$ep"); fi
fi
if [ "$sub" = "parse" ] && [ "$has_model" -eq 0 ]; then
  md="$(read_ini "$protocol" model)"; [ -n "$md" ] && args+=(--model "$md")
fi

exec "$BIN" "${args[@]}"
