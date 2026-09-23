<#
  uparser-run.ps1 — locate (or download/build) the uparser binary and run it.

  This script used to also read ~/.config/uparser/config.toml and inject
  --endpoint/--model. That is gone: the binary now reads the same file itself,
  and does it correctly in three ways this wrapper could not.

    - It keys the lookup on the protocol chosen AFTER routing. This wrapper
      used the protocol as literally written on the command line, so a plain
      `uparser-run.ps1 parse doc.pdf` looked up a section named [auto] and
      silently injected nothing.
    - It layers [defaults] under [<protocol>], per key.
    - It resolves api_key/headers/timeout_secs/max_retries and pipeline's
      per-stage endpoints, none of which this wrapper ever handled.

  So this is now purely about finding the binary. Configuration lives in
  ~/.config/uparser/config.toml (override with $env:UPARSER_CONFIG) —
  see ../references/config.example.toml.

  Usage: .\uparser-run.ps1 parse --protocol mineru-vlm doc.pdf
#>
[CmdletBinding()]
param([Parameter(ValueFromRemainingArguments = $true)] [string[]] $Args)
$ErrorActionPreference = 'Stop'

# --- locate the real binary: always via ensure_uparser.ps1 ---
#
# This deliberately does NOT short-circuit to Get-Command first. It used to,
# and that made ensure_uparser.ps1's whole version-checking and supersede
# ladder dead code on the most common call path: a machine with any old
# uparser.exe on PATH would keep using it forever, silently, with the upgrade
# logic never running. ensure_uparser.ps1 consults PATH itself, at the right
# priority and with a version comparison. $env:UPARSER_BIN is likewise handled
# there (and still wins unconditionally).
$bin = (& (Join-Path $PSScriptRoot 'ensure_uparser.ps1') | Select-Object -Last 1)
if (-not $bin -or -not (Test-Path $bin)) { Write-Error 'uparser binary not found and could not be downloaded/built'; exit 2 }

& $bin @Args
exit $LASTEXITCODE
