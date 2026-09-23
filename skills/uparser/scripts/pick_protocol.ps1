<#
  pick_protocol.ps1 — Windows twin of pick_protocol.sh. Chooses the protocol
  that gives the best OUTPUT QUALITY among the model endpoints this machine can
  actually reach right now.

  ---------------------------------------------------------------------------
  WHY THIS EXISTS AT ALL — `--protocol auto` structurally cannot do this.
  Verified in uparser/crates/uparser-core/src/router.rs:
    * model_candidate() hardcodes the protocol name "mineru-vlm", so the router
      can NEVER select navidc-ocr / monkeyocr-v2 / dots-ocr, whatever you have
      configured. (references/protocols.md:58 states this for navidc-ocr.)
    * RoutingEnvironment::default() hardcodes model_protocol: true — the router
      never probes an endpoint, so it believes a dead one is live.
      (references/protocols.md:79: "does not probe remote endpoints during
      planning. `doctor` is the explicit network preflight.")
    * On a born-digital PDF it scores native 100 vs mineru-vlm 55, so `auto`
      picks native. That is the 0.8920-vs-0.9252 gap in UPARSER_LEADERBOARD.md.
  And `uparser parse` has no --prefer flag (only `uparser plan` does).
  ---------------------------------------------------------------------------

  Usage: .\pick_protocol.ps1 -Bin <uparser.exe> [-File <path>] [-Refresh] [-Json]
  stdout: the protocol name, or the JSON object with -Json
  exit:   0 always — it always names something, worst case "native".
#>
[CmdletBinding()]
param([string] $Bin, [string] $File = '', [switch] $Refresh, [switch] $Json)
$ErrorActionPreference = 'Stop'
if (-not $Bin) { Write-Error 'pick_protocol.ps1: -Bin is required'; exit 1 }

$cfg   = if ($env:UPARSER_CONFIG) { $env:UPARSER_CONFIG } else { Join-Path $HOME '.config/uparser/config.toml' }
$state = if ($env:UPARSER_SKILL_HOME) { $env:UPARSER_SKILL_HOME } else { Join-Path $HOME '.cache/uparser-skill' }
$ttlOk   = if ($env:UPARSER_PROBE_TTL)          { [int]$env:UPARSER_PROBE_TTL }          else { 300 }
$ttlNone = if ($env:UPARSER_PROBE_NEGATIVE_TTL) { [int]$env:UPARSER_PROBE_NEGATIVE_TTL } else { 60 }
# The negative TTL is short on purpose: "nothing reachable" must expire fast, or
# an endpoint that comes up mid-batch is ignored for the rest of the batch.

# Quality order. Evidence, both benchmarks in UPARSER_LEADERBOARD.md:
# opendataloader-bench overall — mineru-vlm .9252 > pipeline .9086 >
# navidc-ocr .9053 > monkeyocr-v2 .8824 > native .8766; OmniDocBench —
# navidc-ocr best tables/formulas, monkeyocr-v2 best text/reading-order.
# `mock` is deliberately absent and must stay absent: placeholder output,
# explicit-only. native/tesseract are fallbacks, not probe candidates.
$order = if ($env:UPARSER_QUALITY_ORDER) { $env:UPARSER_QUALITY_ORDER -split '\s+' }
         else { @('mineru-vlm','navidc-ocr','monkeyocr-v2','pipeline','dots-ocr','paddlex-structure','generic-vlm') }

function Write-Pick($protocol, $reason, $source, $configured, $probed) {
  if ($Json) {
    $c = ($configured | ForEach-Object { '"' + $_ + '"' }) -join ','
    Write-Output ('{"protocol":"' + $protocol + '","reason":"' + $reason +
                  '","source":"' + $source + '","configured":[' + $c + '],"probed":[' + ($probed -join ',') + ']}')
  } else { Write-Output $protocol }
  exit 0
}

# --- format gate -------------------------------------------------------------
# Graded by what the format actually gives up, NOT a blanket "structured =
# native". Three tiers:
#
#  (1) pdf/png/jpeg — a visual channel is the only channel. Probe.
#
#  (2) pptx/ppt/odp — slides ARE visual layout: absolutely-positioned text
#      boxes with no reading-order semantics to preserve. uparser's own router
#      agrees and scores a presentation +35 toward the model and -35 against
#      native even when the source is structured (router.rs, and the test
#      `presentation_routes_model_even_when_structured`). So probe these too —
#      but only if LibreOffice is actually installed, because the model route
#      needs it to materialize slides and its absence is a hard environment
#      failure (references/protocols.md:126).
#
#  (3) everything else structured (docx/xlsx/csv/odt/rtf/epub) — these carry
#      exact structure a model could only ever re-infer from pixels: real table
#      cells with spans, real list nesting, real headings. xlsx/csv do not even
#      rasterize (they read cells directly), and `--format document-json`, the
#      only lossless view with row/column spans, exists for these sources ONLY.
#      protocols.md:126: "visual conversion discards source semantics."
if ($File) {
  $ext = ([System.IO.Path]::GetExtension($File) -replace '^\.', '').ToLower()
  if ($ext -in @('pptx', 'ppt', 'odp')) {
    $soffice = (Get-Command soffice -ErrorAction SilentlyContinue) -or
               (Get-Command soffice.exe -ErrorAction SilentlyContinue) -or
               (Get-Command libreoffice -ErrorAction SilentlyContinue)
    if ($soffice) {
      [Console]::Error.WriteLine("uparser-parse: $ext is slide layout; probing model protocols (router scores")
      [Console]::Error.WriteLine('               presentations toward a model even when structured)')
    }
    else {
      [Console]::Error.WriteLine("uparser-parse: $ext would benefit from a model route, but LibreOffice is not")
      [Console]::Error.WriteLine("               installed to materialize the slides; keeping 'native'")
      Write-Pick 'native' 'no-libreoffice' 'format-gate' @() @()
    }
  }
  elseif ($ext -notin @('pdf', 'png', 'jpg', 'jpeg')) {
    [Console]::Error.WriteLine("uparser-parse: $ext carries exact structure a model can only re-infer; keeping")
    [Console]::Error.WriteLine("               'native' (a model route loses source semantics and needs LibreOffice)")
    Write-Pick 'native' 'structured-source' 'format-gate' @() @()
  }
}

if ($env:UPARSER_PREFER -in @('speed', 'cost')) {
  [Console]::Error.WriteLine("uparser-parse: UPARSER_PREFER=$($env:UPARSER_PREFER); skipping endpoint probes")
  Write-Pick 'auto' 'preference-speed' 'forced' @() @()
}
if ($env:UPARSER_NO_PROBE -eq '1') { Write-Pick 'auto' 'probe-disabled' 'forced' @() @() }

# --- which protocols are actually configured ---------------------------------
# One pass over the file, collecting every section that carries an `endpoint`
# (plus [pipeline.stages] as a marker for pipeline). Line-oriented and unaware
# of TOML inline tables, which is fine: it only decides WHETHER to probe. The
# binary remains the authority on endpoint resolution.
$have = @()
if (Test-Path -LiteralPath $cfg) {
  $cur = ''
  foreach ($line in Get-Content -LiteralPath $cfg) {
    if ($line -match '^\s*\[(.+?)\]\s*$') {
      $cur = $Matches[1].Trim()
      if ($cur -eq 'pipeline.stages') { $have += 'pipeline' }
      continue
    }
    if ($cur -and $line -match '^\s*endpoint\s*=') { $have += $cur; $cur = '' }
  }
}
# A [defaults] endpoint, or $env:UPARSER_ENDPOINT, makes every protocol
# resolvable. Semantically right (the binary would resolve it); the
# endpoint-dedupe in the probe loop stops it becoming 7 probes of one URL.
$blanket = ($have -contains 'defaults') -or [bool]$env:UPARSER_ENDPOINT
$configured = @($order | Where-Object { $blanket -or ($have -contains $_) })

if (-not $configured -or $configured.Count -eq 0) {
  [Console]::Error.WriteLine("uparser-parse: no model endpoint configured in $cfg; using 'native' (offline)")
  Write-Pick 'native' 'no-endpoint-configured' 'none' @() @()
}

# --- probe cache -------------------------------------------------------------
# Keyed on config CONTENTS (not mtime — a copied config would otherwise give a
# stale hit), the endpoint env overrides, and a binary identity. The identity is
# path+size+mtime rather than `--version`, which would spawn a 14 MB executable
# on every invocation; a downloaded upgrade changes the path and a local rebuild
# changes the mtime, so both still invalidate.
function Get-UpNow { [int]((Get-Date -UFormat %s) -as [double]) }
$binItem = Get-Item -LiteralPath $Bin -ErrorAction SilentlyContinue
$material = @($cfg,
              (Get-Content -LiteralPath $cfg -Raw -ErrorAction SilentlyContinue),
              $env:UPARSER_ENDPOINT, $env:UPARSER_MODEL, $Bin,
              $(if ($binItem) { "$($binItem.Length)|$($binItem.LastWriteTimeUtc.Ticks)" }),
              ($order -join ' ')) -join '|'
$sha = [System.Security.Cryptography.SHA256]::Create()
$key = ([BitConverter]::ToString($sha.ComputeHash([Text.Encoding]::UTF8.GetBytes($material))) -replace '-', '').Substring(0, 16).ToLower()
$probeFile = Join-Path $state "probe-$key.txt"

if (-not $Refresh -and (Test-Path -LiteralPath $probeFile)) {
  try {
    $parts = (Get-Content -LiteralPath $probeFile -Raw).Trim() -split '\s+', 2
    if ($parts.Count -eq 2) {
      $age = (Get-UpNow) - [int]$parts[0]
      $ttl = if ($parts[1] -eq 'native') { $ttlNone } else { $ttlOk }
      if ($age -ge 0 -and $age -le $ttl) { Write-Pick $parts[1] 'cached' 'cache' $configured @() }
    }
  } catch { }   # any parse failure is a clean miss
}
function Set-UpProbe($p) {
  try {
    New-Item -ItemType Directory -Force -Path $state | Out-Null
    $tmp = "$probeFile.$PID"
    ((Get-UpNow).ToString() + ' ' + $p) | Set-Content -LiteralPath $tmp -Encoding ascii
    Move-Item -Force -LiteralPath $tmp -Destination $probeFile
  } catch { }
}

# --- probe -------------------------------------------------------------------
$probed = @(); $seen = @()
foreach ($p in $configured) {
  $out = $null
  try { $out = (& $Bin doctor $p 2>$null | ConvertFrom-Json) } catch { $out = $null }
  $ep = if ($out) { [string]$out.endpoint } else { '' }
  # Skip a protocol whose endpoint we already probed — the [defaults] case would
  # otherwise probe one URL up to seven times, at ~2.5s each when it is down.
  if ($ep -and ($seen -contains $ep)) { continue }
  if ($ep) { $seen += $ep }

  # `doctor` ALWAYS exits 0 and reports status in this field — never read the
  # exit code. And any HTTP answer counts as reachable: the real MinerU
  # deployment replies 405 to this probe, so requiring 200 would mark a working
  # endpoint dead.
  $reach = [bool]($out -and $out.reachable -eq $true)
  $probed += ('{"protocol":"' + $p + '","reachable":' + $reach.ToString().ToLower() + ',"endpoint":"' + $ep + '"}')

  if ($reach) {
    [Console]::Error.WriteLine("uparser-parse: using '$p' for quality (reachable at $ep)")
    Set-UpProbe $p
    Write-Pick $p 'reachable' 'probe' $configured $probed
  }
  $detail = if ($out) { [string]$out.detail } else { '' }
  [Console]::Error.WriteLine("uparser-parse: $p unreachable at $(if($ep){$ep}else{'?'})$(if($detail){" - $detail"})")
}

# Nothing reachable. Fall back to native, NOT to `auto`:
# RoutingEnvironment::default() hardcodes model_protocol: true, so `auto` still
# believes a model endpoint exists and can route to the dead one — failing late,
# or appearing to have used a VLM when it did not.
[Console]::Error.WriteLine("uparser-parse: no configured model endpoint is reachable; using 'native'")
Set-UpProbe 'native'
Write-Pick 'native' 'no-endpoint-reachable' 'probe' $configured $probed
