<#
  uparser-parse.ps1 — one-shot "do the right thing" parse for coding agents
  (Windows mirror of uparser-parse.sh). Give it a file; it returns Markdown on
  stdout and the binary's own semantic exit code.

  It never selects `mock`, picks
  `native` (offline) when no VLM endpoint is resolvable and `auto` (with the
  endpoint/model injected) when one is, and defaults --format to markdown.
  Endpoint is resolved from --endpoint / $env:UPARSER_ENDPOINT / config[mineru-vlm].
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
$hasModel = ($a -contains '--model') -or [bool]($a | Where-Object { $_ -like '--model=*' })
$hasFormat = ($a -contains '--format') -or [bool]($a | Where-Object { $_ -like '--format=*' })

$inject = @()
if (-not $hasFormat) { $inject += @('--format', 'markdown') }

if (-not $hasMode -and -not $hasProto) {
  $ep = if ($env:UPARSER_ENDPOINT) { $env:UPARSER_ENDPOINT } else { Read-Ini 'mineru-vlm' 'endpoint' }
  $md = if ($env:UPARSER_MODEL) { $env:UPARSER_MODEL } else { Read-Ini 'mineru-vlm' 'model' }
  if ($hasEp -or $ep) {
    $inject += @('--protocol', 'auto')
    if (-not $hasEp -and $ep) { $inject += @('--endpoint', $ep) }
    if (-not $hasModel -and $md) { $inject += @('--model', $md) }
    [Console]::Error.WriteLine("uparser-parse: no --protocol given; using 'auto' with endpoint $(if($ep){$ep}else{'<from cli>'})")
  }
  else {
    $inject += @('--protocol', 'native')
    [Console]::Error.WriteLine("uparser-parse: no --protocol and no endpoint; using 'native' (offline, no OCR)")
  }
}

$run = Join-Path $PSScriptRoot 'uparser-run.ps1'
& $run @('parse') @a @inject
exit $LASTEXITCODE
