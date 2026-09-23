<#
  uparser-check.ps1 — one-call preflight (Windows mirror of uparser-check.sh).
  Ensures the binary exists and prints a compact JSON status to stdout so an
  agent can branch programmatically. Exit 0 if the binary is usable, 2 if not.

  { "binary": "<path>|null", "ok": true|false,
    "version": "<installed>|null", "latest": "<newest published>|null",
    "latest_source": "api|cache|last_known_good|fallback_pin|user_pin|...",
    "protocols": [ ... ], "endpoint": "<url>|null",
    "endpoint_reachable": true|false|null,
    "quality_protocol": "<name>", "quality_reason": "<why>" }

  The quality_* fields come from pick_protocol.ps1 — the SAME code
  uparser-parse.ps1 uses, so the two can no longer disagree about what would
  run. (They used to: this script hardcoded mineru-vlm while parse enumerated
  six sections.)

  Usage: .\uparser-check.ps1 [--protocol mineru-vlm] [--endpoint <url>] [--file <path>]
#>
[CmdletBinding()]
param([Parameter(ValueFromRemainingArguments = $true)] [string[]] $Args)
$ErrorActionPreference = 'Stop'

$proto = 'mineru-vlm'; $epCli = ''; $file = ''
for ($i = 0; $i -lt $Args.Count; $i++) {
  switch -Wildcard ($Args[$i]) {
    '--protocol'   { $proto = $Args[$i + 1] }
    '--protocol=*' { $proto = $Args[$i].Split('=', 2)[1] }
    '--endpoint'   { $epCli = $Args[$i + 1] }
    '--endpoint=*' { $epCli = $Args[$i].Split('=', 2)[1] }
    '--file'       { $file  = $Args[$i + 1] }
    '--file=*'     { $file  = $Args[$i].Split('=', 2)[1] }
  }
}

# Backslashes MUST be escaped: every Windows path this emits ("C:\temp\...")
# contains sequences like \t and \u that are valid JSON escapes, so an
# unescaped path produces JSON that a strict parser rejects outright.
function J($v) {
  if ($null -eq $v -or $v -eq '') { 'null' }
  else { '"' + (($v -replace '\\', '\\') -replace '"', '\"') + '"' }
}

$bin = (& (Join-Path $PSScriptRoot 'ensure_uparser.ps1') | Select-Object -Last 1)
if (-not $bin -or -not (Test-Path $bin)) {
  Write-Output '{"binary":null,"ok":false,"version":null,"latest":null,"latest_source":null,"protocols":[],"endpoint":null,"endpoint_reachable":null,"quality_protocol":null,"quality_reason":null}'
  [Console]::Error.WriteLine('uparser-check: binary not found and could not be downloaded/built')
  exit 2
}

$installed = ''
try { if ((& $bin --version 2>$null | Select-Object -First 1) -match '([0-9][0-9A-Za-z.+-]*)') { $installed = $Matches[1] } } catch { }

# newest published build for this platform, and how sure we are of it
$latest = ''; $lsrc = ''
try {
  $lv = (& (Join-Path $PSScriptRoot 'latest_version.ps1') -Json | Select-Object -Last 1) | ConvertFrom-Json
  $latest = [string]$lv.version; $lsrc = [string]$lv.source
} catch { }

$names = @()
try { $names = (& $bin protocols | ConvertFrom-Json | ForEach-Object { $_.name }) } catch { $names = @() }
$namesJson = ($names | ForEach-Object { '"' + $_ + '"' }) -join ','

# `doctor` resolves the endpoint itself (flag > $env:UPARSER_ENDPOINT >
# config[<protocol>] > config[defaults] > built-in default) and echoes the
# resolved value back, so read it from doctor's output rather than
# re-implementing the lookup here — this wrapper's copy could only ever see
# one flat section and knew nothing about [defaults] or api_key.
$ep = $null
$reachable = 'null'
try {
  $d = if ($epCli) { (& $bin doctor $proto --endpoint $epCli | ConvertFrom-Json) }
       else { (& $bin doctor $proto | ConvertFrom-Json) }
  $ep = $d.endpoint
  if ($null -ne $d.reachable) { $reachable = if ($d.reachable) { 'true' } else { 'false' } }
} catch { $reachable = 'false' }

# what would uparser-parse.ps1 actually pick right now?
$qproto = ''; $qreason = ''
try {
  # Hashtable splat, not an array: @('-Json') would be passed positionally and
  # rejected as "no positional parameter accepts -Json".
  $pickArgs = @{ Bin = $bin; Json = $true }
  if ($file) { $pickArgs['File'] = $file }
  $pick = (& (Join-Path $PSScriptRoot 'pick_protocol.ps1') @pickArgs | Select-Object -Last 1) | ConvertFrom-Json
  $qproto = [string]$pick.protocol; $qreason = [string]$pick.reason
} catch { }

Write-Output ('{"binary":' + (J $bin) + ',"ok":true,"version":' + (J $installed) +
  ',"latest":' + (J $latest) + ',"latest_source":' + (J $lsrc) +
  ',"protocols":[' + $namesJson + '],"endpoint":' + (J $ep) +
  ',"endpoint_reachable":' + $reachable +
  ',"quality_protocol":' + (J $qproto) + ',"quality_reason":' + (J $qreason) + '}')
