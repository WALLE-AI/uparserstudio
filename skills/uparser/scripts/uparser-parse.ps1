<#
  uparser-parse.ps1 — one-shot "do the right thing" parse for coding agents
  (Windows mirror of uparser-parse.sh). Give it a file; it returns Markdown on
  stdout and the binary's own semantic exit code.

  What it decides for you:
    * ensures a current uparser.exe exists (ensure_uparser.ps1: version check
      against GitHub Releases, TTL-cached, degrades offline);
    * NEVER selects the explicit-only `mock` protocol;
    * picks the protocol when you pass neither --mode nor --protocol, and by
      default picks for QUALITY: it probes the model endpoints you actually
      configured and runs the best reachable one (pick_protocol.ps1);
    * defaults --format to markdown.

  Why quality-first is not just `--protocol auto`: `auto` never probes an
  endpoint, its model candidate is hardwired to mineru-vlm, and on a
  born-digital PDF it scores native above every model — see the long comment in
  pick_protocol.ps1 for the verified specifics.

  The cost is real and deliberate: on a born-digital PDF a VLM route trades
  roughly 15x wall-clock for about +0.05 overall accuracy (UPARSER_LEADERBOARD.md).
  Set UPARSER_PREFER=speed to get the old endpoint-agnostic routing back.
  Structured sources (DOCX/XLSX/CSV/...) always stay native regardless.

  Anything you pass through (incl. an explicit --mode/--protocol/--endpoint/--format)
  is forwarded unchanged and always wins.

  Usage: .\uparser-parse.ps1 <file> [any uparser parse flags...]
#>
[CmdletBinding()]
param([Parameter(ValueFromRemainingArguments = $true)] [string[]] $Args)
$ErrorActionPreference = 'Stop'

if (-not $Args -or $Args.Count -lt 1) { Write-Error 'usage: uparser-parse.ps1 <file> [uparser parse flags...]'; exit 1 }

$a = @($Args)
$hasMode   = ($a -contains '--mode')     -or [bool]($a | Where-Object { $_ -like '--mode=*' })
$hasProto  = ($a -contains '--protocol') -or [bool]($a | Where-Object { $_ -like '--protocol=*' })
$hasFormat = ($a -contains '--format')   -or [bool]($a | Where-Object { $_ -like '--format=*' })

# find the input file: the first argument that is neither a flag nor a flag's value
$file = ''; $skipNext = $false
foreach ($x in $a) {
  if ($skipNext) { $skipNext = $false; continue }
  if ($x -like '--*=*') { continue }
  if ($x -like '--*')   { $skipNext = $true; continue }
  if (-not $file) { $file = $x }
}

$inject = @()
if (-not $hasFormat) { $inject += @('--format', 'markdown') }

if (-not $hasMode -and -not $hasProto) {
  # Resolve the binary once here and hand the same one to both the protocol
  # probe and the run, instead of resolving it twice.
  $bin = (& (Join-Path $PSScriptRoot 'ensure_uparser.ps1') | Select-Object -Last 1)
  if (-not $bin -or -not (Test-Path $bin)) {
    Write-Error 'uparser binary not found and could not be downloaded/built'; exit 2
  }
  $env:UPARSER_BIN = $bin

  $proto = (& (Join-Path $PSScriptRoot 'pick_protocol.ps1') -Bin $bin -File $file | Select-Object -Last 1)
  if (-not $proto) { $proto = 'native' }
  $inject += @('--protocol', $proto)
}

$run = Join-Path $PSScriptRoot 'uparser-run.ps1'
& $run @('parse') @a @inject
exit $LASTEXITCODE
