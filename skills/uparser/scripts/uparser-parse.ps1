<#
  uparser-parse.ps1 — one-shot "do the right thing" parse for coding agents
  (Windows mirror of uparser-parse.sh). Give it a file; it returns Markdown on
  stdout and the binary's own semantic exit code.

  It never selects `mock`, picks `native` (offline) when no VLM endpoint is
  configured anywhere and `auto` when one is, and defaults --format to markdown.
  It no longer injects --endpoint/--model: the binary resolves those itself
  (per key, keyed on the post-routing protocol, with [defaults] layering and
  api_key support this wrapper's reader cannot express). The config is consulted
  here only to answer "does any endpoint exist at all?".
  Anything you pass through (incl. an explicit --mode/--protocol/--endpoint/--format)
  is forwarded unchanged and always wins.

  Usage: .\uparser-parse.ps1 <file> [any uparser parse flags...]
#>
[CmdletBinding()]
param([Parameter(ValueFromRemainingArguments = $true)] [string[]] $Args)
$ErrorActionPreference = 'Stop'

if (-not $Args -or $Args.Count -lt 1) { Write-Error 'usage: uparser-parse.ps1 <file> [uparser parse flags...]'; exit 1 }
$cfg = if ($env:UPARSER_CONFIG) { $env:UPARSER_CONFIG } else { Join-Path $HOME '.config/uparser/config.toml' }

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

$a = @($Args)
$hasMode = ($a -contains '--mode') -or [bool]($a | Where-Object { $_ -like '--mode=*' })
$hasProto = ($a -contains '--protocol') -or [bool]($a | Where-Object { $_ -like '--protocol=*' })
$hasEp = ($a -contains '--endpoint') -or [bool]($a | Where-Object { $_ -like '--endpoint=*' })
$hasFormat = ($a -contains '--format') -or [bool]($a | Where-Object { $_ -like '--format=*' })

# Any VLM section, plus [defaults] — this previously looked only at
# [mineru-vlm], so a machine configured for e.g. dots-ocr alone silently
# fell through to native.
function Test-EndpointConfigured {
  if ($env:UPARSER_ENDPOINT) { return $true }
  foreach ($sec in @('defaults', 'mineru-vlm', 'monkeyocr-v2', 'navidc-ocr', 'dots-ocr', 'generic-vlm')) {
    if (Read-Ini $sec 'endpoint') { return $true }
  }
  return $false
}

$inject = @()
if (-not $hasFormat) { $inject += @('--format', 'markdown') }

if (-not $hasMode -and -not $hasProto) {
  if ($hasEp -or (Test-EndpointConfigured)) {
    $inject += @('--protocol', 'auto')
    [Console]::Error.WriteLine("uparser-parse: no --protocol given; using 'auto' (endpoint resolved by the binary)")
  }
  else {
    $inject += @('--protocol', 'native')
    [Console]::Error.WriteLine("uparser-parse: no --protocol and no endpoint configured; using 'native' (offline; bounded page OCR may apply)")
  }
}

$run = Join-Path $PSScriptRoot 'uparser-run.ps1'
& $run @('parse') @a @inject
exit $LASTEXITCODE
