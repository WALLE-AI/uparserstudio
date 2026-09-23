<#
  ensure_uparser.ps1 — guarantee a runnable, reasonably CURRENT uparser.exe is
  present on Windows, printing its path. Windows twin of ensure_uparser.sh.

  Resolution order:
    0) $env:UPARSER_BIN            explicit, wins unconditionally, never version-checked
    1) resolve the target version  latest_version.ps1 (TTL-cached, degrades offline)
    2) a local cargo workspace build   used when not older than the target
    3) uparser(.exe) on PATH           used when not older than the target
    4) the versioned download cache
    5) download from GitHub Releases   direct -> ghfast.top mirror, sha256
    6) build from source               build-windows.ps1

  ---------------------------------------------------------------------------
  THE ANTI-DOWNGRADE INVARIANT — read before changing anything below.

  Two independent guards, and both are needed:

   (a) A supersede only ever happens when the local binary is strictly OLDER
       than the target (Compare-UpVersion -eq -1). This alone makes a literal
       downgrade impossible whatever the target turned out to be, and is why a
       developer's 0.5.0-dev survives contact with an older published release.

   (b) latest_version.ps1 reports HOW it got its answer via `source`, and only
       an answer that came from GitHub (`api`, or a fresh `cache` entry, which
       IS a previous api answer inside its TTL) or an explicit `user_pin` may
       trigger a DOWNLOAD. `fallback_pin` / `api_no_asset` / `last_known_good`
       are guesses made while the network was unavailable.

  `cache` MUST count as authoritative. Treating it as a guess looks safer but
  silently defeats the entire feature: after the first successful lookup, every
  call for the next TTL window would decline to upgrade. Guard (a) is what
  makes this safe.
  ---------------------------------------------------------------------------

  Env: UPARSER_BIN, UPARSER_VERSION, UPARSER_REPO, UPARSER_HOME,
       UPARSER_SKILL_HOME, UPARSER_OFFLINE, UPARSER_PRERELEASE,
       UPARSER_VERSION_TTL, UPARSER_PREFER_WORKSPACE, GITHUB_TOKEN.
#>
[CmdletBinding()] param([switch] $Refresh)
$ErrorActionPreference = 'Stop'

$here = $PSScriptRoot

# 0) explicit binary — the escape hatch for an unreleased workspace build.
if ($env:UPARSER_BIN) {
  if (-not (Test-Path -LiteralPath $env:UPARSER_BIN -PathType Leaf)) {
    throw "UPARSER_BIN does not exist: $($env:UPARSER_BIN)"
  }
  return (Resolve-Path -LiteralPath $env:UPARSER_BIN).Path
}

# Compare-UpVersion / Get-UpBinVersion / Get-UpFallbackPin
. (Join-Path $here 'latest_version.ps1') -SourceOnly

$plat      = 'windows-x86_64'
$cacheRoot = if ($env:UPARSER_HOME) { $env:UPARSER_HOME } else { Join-Path $HOME '.cache/uparser' }
$state     = if ($env:UPARSER_SKILL_HOME) { $env:UPARSER_SKILL_HOME } else { Join-Path $HOME '.cache/uparser-skill' }
$memoTtl   = if ($env:UPARSER_VERSION_TTL) { [int]$env:UPARSER_VERSION_TTL } else { 21600 }
$memo      = Join-Path $state "resolved-$plat.txt"

# --- resolved-path memo ------------------------------------------------------
# uparser-run.ps1 now calls this on EVERY invocation (it used to short-circuit
# to PATH, which quietly disabled the upgrade ladder). Remember the answer for
# as long as the version answer itself is fresh, so a batch does not re-resolve
# per file. Memoizing a PATH is safe: an in-place upgrade keeps the same path,
# and the TTL bounds how long a new release goes unnoticed.
function Get-UpNow { [int]((Get-Date -UFormat %s) -as [double]) }
if (-not $Refresh -and (Test-Path -LiteralPath $memo)) {
  try {
    $parts = (Get-Content -LiteralPath $memo -Raw).Trim() -split "`t", 2
    if ($parts.Count -eq 2 -and (Test-Path -LiteralPath $parts[1] -PathType Leaf)) {
      if (((Get-UpNow) - [int]$parts[0]) -le $memoTtl) { return $parts[1] }
    }
  } catch { }   # any parse failure is a clean miss
}
function Set-UpMemo($p) {
  try {
    New-Item -ItemType Directory -Force -Path $state | Out-Null
    $tmp = "$memo.$PID"
    ((Get-UpNow).ToString() + "`t" + $p) | Set-Content -LiteralPath $tmp -Encoding ascii
    Move-Item -Force -LiteralPath $tmp -Destination $memo
  } catch { }
}

# 1) target version + how authoritative it is
$target = ''; $tsrc = 'none'; $asset = ''
try {
  $r = (& (Join-Path $here 'latest_version.ps1') -Json | Select-Object -Last 1) | ConvertFrom-Json
  $target = [string]$r.version; $tsrc = [string]$r.source; $asset = [string]$r.asset
} catch { }

$authoritative = @('api', 'cache', 'user_pin') -contains $tsrc

$cacheDir  = Join-Path $cacheRoot "versions\v$target\$plat"
$cachedBin = Join-Path $cacheDir 'uparser.exe'

# Decide what to do with a local candidate. Returns the path to use, or $null
# when the caller should move on (i.e. we want the target instead).
function Test-UpCandidate([string]$cand, [string]$label) {
  if (-not $cand -or -not (Test-Path -LiteralPath $cand -PathType Leaf)) { return $null }
  if (-not $target) { return $cand }

  $lv = Get-UpBinVersion $cand
  if (-not $lv) {
    [Console]::Error.WriteLine("uparser: could not read a version from $label ($cand) - keeping it")
    return $cand
  }
  $c = Compare-UpVersion $lv $target
  if ($null -eq $c) {
    [Console]::Error.WriteLine("uparser: unparseable version '$lv' from $label - keeping it")
    return $cand
  }
  if ($c -eq 0) { return $cand }
  if ($c -eq 1) {
    if ($authoritative) {
      [Console]::Error.WriteLine("uparser: $label is $lv, newer than the newest published release $target - keeping it")
    }
    return $cand
  }
  # $c -eq -1: the candidate is older than the target.
  if ($authoritative) {
    [Console]::Error.WriteLine("uparser: $label is $lv; using newer $target instead ($label is left untouched;")
    [Console]::Error.WriteLine("        set UPARSER_BIN to override, or UPARSER_VERSION=$lv to pin)")
    return $null
  }
  # Advisory target: only step aside for a binary that is ALREADY downloaded.
  if (Test-Path -LiteralPath $cachedBin -PathType Leaf) {
    [Console]::Error.WriteLine("uparser: $label is $lv; using already-cached $target")
    return $null
  }
  return $cand
}

function Complete-Up($p) { Set-UpMemo $p; return $p }

# 2) a local cargo workspace build. This rung exists so a developer with a
#    fresh local build does not silently get an older downloaded release —
#    the exact hazard find_uparser.sh's own comment warns about.
$ws = $null
foreach ($root in @($here, $PWD.Path)) {
  $d = $root
  while ($d) {
    if (Test-Path -LiteralPath (Join-Path $d 'uparser\Cargo.toml')) { $ws = Join-Path $d 'uparser'; break }
    if ((Test-Path -LiteralPath (Join-Path $d 'Cargo.toml')) -and
        (Test-Path -LiteralPath (Join-Path $d 'crates\uparser-core'))) { $ws = $d; break }
    $parent = Split-Path -Parent $d
    if ($parent -eq $d) { break }
    $d = $parent
  }
  if ($ws) { break }
}
if ($ws) {
  $wsBin = Join-Path $ws 'target\release\uparser.exe'
  if (Test-Path -LiteralPath $wsBin -PathType Leaf) {
    if ($env:UPARSER_PREFER_WORKSPACE -eq '1') { return (Complete-Up $wsBin) }
    $pick = Test-UpCandidate $wsBin 'the local workspace build'
    if ($pick) { return (Complete-Up $pick) }
  }
}

# 3) PATH
$onPath = (Get-Command uparser.exe -ErrorAction SilentlyContinue).Source
if (-not $onPath) { $onPath = (Get-Command uparser -ErrorAction SilentlyContinue).Source }
if ($onPath) {
  $pick = Test-UpCandidate $onPath 'PATH uparser'
  if ($pick) { return (Complete-Up $pick) }
}

# 4) already downloaded?
if (Test-Path -LiteralPath $cachedBin -PathType Leaf) { return (Complete-Up $cachedBin) }

if (-not $target -or -not $asset) {
  Write-Warning 'uparser: no published binary nameable for this platform - building from source'
  & (Join-Path $here 'build-windows.ps1'); return (Join-Path $HOME '.local/bin/uparser.exe')
}

if (-not [Environment]::Is64BitOperatingSystem) {
  Write-Warning 'no prebuilt for 32-bit Windows - building from source'
  & (Join-Path $here 'build-windows.ps1'); return (Join-Path $HOME '.local/bin/uparser.exe')
}

$repo = if ($env:UPARSER_REPO) { $env:UPARSER_REPO } else { 'WALLE-AI/uparserstudio' }
$base = "https://github.com/$repo/releases/download/v$target"
New-Item -ItemType Directory -Force -Path $cacheDir | Out-Null
$tmp    = Join-Path ([System.IO.Path]::GetTempPath()) ([System.IO.Path]::GetRandomFileName())
$dllAsset = "uparser-v$target-$plat-pdfium.dll"
$dllTmp = "$tmp.pdfium.dll"

# Direct, then the ghfast.top mirror (needed on networks that cannot reach
# github.com's download host). NOTE: ghfast.top mirrors release DOWNLOADS only
# - it does NOT proxy api.github.com (verified: 403), which is why version
# resolution in latest_version.ps1 has no mirror and degrades to a pin instead.
function Get-UpFile($url, $dest) {
  try { Invoke-WebRequest -UseBasicParsing -Uri $url -OutFile $dest -TimeoutSec 30; return $true }
  catch {
    try { Invoke-WebRequest -UseBasicParsing -Uri "https://ghfast.top/$url" -OutFile $dest -TimeoutSec 40; return $true }
    catch { return $false }
  }
}

Write-Host "uparser: downloading $asset (v$target) ..." -ForegroundColor Cyan
if (-not (Get-UpFile "$base/$asset" $tmp)) {
  Write-Warning 'uparser: download failed (direct + mirror) - building from source'
  & (Join-Path $here 'build-windows.ps1'); return (Join-Path $HOME '.local/bin/uparser.exe')
}

# checksum (best-effort; an absent SHA256SUMS is not fatal)
$sums = "$tmp.sums"
if (Get-UpFile "$base/SHA256SUMS" $sums) {
  $line = Get-Content $sums | Where-Object { $_ -match ([regex]::Escape($asset) + '\s*$') } | Select-Object -First 1
  if ($line) {
    $want = ($line -split '\s+')[0].ToLower()
    $got = (Get-FileHash -Algorithm SHA256 $tmp).Hash.ToLower()
    if ($want -ne $got) { Remove-Item $tmp, $sums -Force; throw "checksum mismatch for $asset" }
  }
  # pdfium.dll must land beside the exe or every rasterization/vision protocol
  # fails at runtime while `native` keeps working - i.e. it looks half-fine.
  if (Get-UpFile "$base/$dllAsset" $dllTmp) {
    $dllLine = Get-Content $sums | Where-Object { $_ -match ([regex]::Escape($dllAsset) + '\s*$') } | Select-Object -First 1
    if ($dllLine) {
      $dllWant = ($dllLine -split '\s+')[0].ToLower()
      $dllGot = (Get-FileHash -Algorithm SHA256 $dllTmp).Hash.ToLower()
      if ($dllWant -ne $dllGot) {
        Remove-Item $tmp, $dllTmp, $sums -Force -ErrorAction SilentlyContinue
        throw "checksum mismatch for $dllAsset"
      }
    }
    Move-Item -Force $dllTmp (Join-Path $cacheDir 'pdfium.dll')
  }
  else {
    Write-Warning "uparser: could not fetch $dllAsset; PDF rasterization, OCR and vision protocols will fail until it is present beside the exe"
  }
  Remove-Item $sums -Force -ErrorAction SilentlyContinue
}

Move-Item -Force $tmp $cachedBin
return (Complete-Up $cachedBin)
