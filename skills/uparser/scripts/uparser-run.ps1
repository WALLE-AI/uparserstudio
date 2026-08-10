<#
  uparser-run.ps1 — Windows wrapper that injects --endpoint/--model from a
  config file so you don't have to pass them on every `parse` call. It does
  NOT modify the binary.

  Config (simple INI): $env:UPARSER_CONFIG, else ~/.config/uparser/config.toml
    [mineru-vlm]
    endpoint = http://10.0.0.5:19122/v1/chat/completions
    model    = MinerU2.5-2604-1.2B

  Precedence: an explicit --endpoint/--model on the command line ALWAYS wins;
  the config only fills what you omitted. --endpoint is injected for `parse`
  and `doctor`; --model only for `parse`.

  Usage: .\uparser-run.ps1 parse --protocol mineru-vlm doc.pdf
#>
[CmdletBinding()]
param([Parameter(ValueFromRemainingArguments = $true)] [string[]] $Args)
$ErrorActionPreference = 'Stop'

$cfg = if ($env:UPARSER_CONFIG) { $env:UPARSER_CONFIG } else { Join-Path $HOME '.config/uparser/config.toml' }

# --- locate the real binary: PATH first, else ensure_uparser.ps1 downloads a
#     version-pinned prebuilt from GitHub Releases (or builds from source) ---
$bin = (Get-Command uparser.exe -ErrorAction SilentlyContinue).Source
if (-not $bin) { $bin = (Get-Command uparser -ErrorAction SilentlyContinue).Source }
if (-not $bin) { $bin = (& (Join-Path $PSScriptRoot 'ensure_uparser.ps1') | Select-Object -Last 1) }
if (-not $bin -or -not (Test-Path $bin)) { Write-Error 'uparser binary not found and could not be downloaded/built'; exit 2 }

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
$sub = ($a | Where-Object { $_ -in 'parse', 'classify', 'doctor', 'protocols', 'cache' } | Select-Object -First 1)

# `parse` takes the protocol via --protocol; `doctor` takes it as the positional
# token right after the subcommand (doctor <protocol> [--endpoint <url>]).
if ($sub -eq 'parse' -or $sub -eq 'doctor') {
  $proto = 'mock'
  if ($sub -eq 'doctor') {
    $di = [array]::IndexOf($a, 'doctor')
    if ($di -ge 0 -and ($di + 1) -lt $a.Count -and $a[$di + 1] -notlike '-*') { $proto = $a[$di + 1] }
  }
  for ($i = 0; $i -lt $a.Count; $i++) {
    if ($a[$i] -eq '--protocol') { $proto = $a[$i + 1] }
    elseif ($a[$i] -like '--protocol=*') { $proto = $a[$i].Split('=', 2)[1] }
  }
  $hasEp = ($a -contains '--endpoint') -or [bool]($a | Where-Object { $_ -like '--endpoint=*' })
  if (-not $hasEp) { $ep = Read-Ini $proto 'endpoint'; if ($ep) { $a += @('--endpoint', $ep) } }
  if ($sub -eq 'parse') {
    $hasModel = ($a -contains '--model') -or [bool]($a | Where-Object { $_ -like '--model=*' })
    if (-not $hasModel) { $md = Read-Ini $proto 'model'; if ($md) { $a += @('--model', $md) } }
  }
}

& $bin @a
exit $LASTEXITCODE
