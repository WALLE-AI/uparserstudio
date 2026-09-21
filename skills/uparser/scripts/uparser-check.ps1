<#
  uparser-check.ps1 — one-call preflight (Windows mirror of uparser-check.sh).
  Ensures the binary exists and prints a compact JSON status to stdout so an
  agent can branch programmatically. Exit 0 if the binary is usable, 2 if not.

  { "binary": "<path>|null", "ok": true|false, "protocols": [ ... ],
    "endpoint": "<url>|null", "endpoint_reachable": true|false|null }

  Usage: .\uparser-check.ps1 [--protocol mineru-vlm] [--endpoint <url>]
  (endpoint also read from $env:UPARSER_ENDPOINT or config[<protocol>|mineru-vlm])
#>
[CmdletBinding()]
param([Parameter(ValueFromRemainingArguments = $true)] [string[]] $Args)
$ErrorActionPreference = 'Stop'

$proto = 'mineru-vlm'; $epCli = ''
for ($i = 0; $i -lt $Args.Count; $i++) {
  switch -Wildcard ($Args[$i]) {
    '--protocol' { $proto = $Args[$i + 1] }
    '--protocol=*' { $proto = $Args[$i].Split('=', 2)[1] }
    '--endpoint' { $epCli = $Args[$i + 1] }
    '--endpoint=*' { $epCli = $Args[$i].Split('=', 2)[1] }
  }
}

function J($v) { if ($null -eq $v -or $v -eq '') { 'null' } else { '"' + $v + '"' } }

$bin = (& (Join-Path $PSScriptRoot 'ensure_uparser.ps1') | Select-Object -Last 1)
if (-not $bin -or -not (Test-Path $bin)) {
  Write-Output '{"binary":null,"ok":false,"protocols":[],"endpoint":null,"endpoint_reachable":null}'
  [Console]::Error.WriteLine('uparser-check: binary not found and could not be downloaded/built')
  exit 2
}

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

Write-Output ('{"binary":' + (J $bin) + ',"ok":true,"protocols":[' + $namesJson + '],"endpoint":' + (J $ep) + ',"endpoint_reachable":' + $reachable + '}')
