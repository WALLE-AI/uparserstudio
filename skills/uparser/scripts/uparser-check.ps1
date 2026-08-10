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
$cfg = if ($env:UPARSER_CONFIG) { $env:UPARSER_CONFIG } else { Join-Path $HOME '.config/uparser/config.toml' }

$proto = 'mineru-vlm'; $epCli = ''
for ($i = 0; $i -lt $Args.Count; $i++) {
  switch -Wildcard ($Args[$i]) {
    '--protocol' { $proto = $Args[$i + 1] }
    '--protocol=*' { $proto = $Args[$i].Split('=', 2)[1] }
    '--endpoint' { $epCli = $Args[$i + 1] }
    '--endpoint=*' { $epCli = $Args[$i].Split('=', 2)[1] }
  }
}

function Read-Ini([string]$section, [string]$key) {
  if (-not (Test-Path $cfg)) { return $null }
  $cur = ''
  foreach ($line in Get-Content -LiteralPath $cfg) {
    if ($line -match '^\s*\[(.+?)\]\s*$') { $cur = $Matches[1].Trim(); continue }
    if ($cur -eq $section -and $line -match ('^\s*' + [regex]::Escape($key) + '\s*=\s*(.+?)\s*$')) {
      return $Matches[1].Trim().Trim('"').Trim("'")
    }
  }
  return $null
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

$ep = if ($epCli) { $epCli } elseif ($env:UPARSER_ENDPOINT) { $env:UPARSER_ENDPOINT } else { Read-Ini $proto 'endpoint' }
$reachable = 'null'
if ($ep) {
  try {
    $d = (& $bin doctor $proto --endpoint $ep | ConvertFrom-Json)
    $reachable = if ($d.reachable) { 'true' } else { 'false' }
  } catch { $reachable = 'false' }
}

Write-Output ('{"binary":' + (J $bin) + ',"ok":true,"protocols":[' + $namesJson + '],"endpoint":' + (J $ep) + ',"endpoint_reachable":' + $reachable + '}')
