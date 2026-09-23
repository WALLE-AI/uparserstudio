<#
  latest_version.ps1 — Windows twin of latest_version.sh. Resolves the newest
  uparser release that actually carries a windows-x86_64 asset, and compares
  version strings with correct semver ordering.

  THE CORE POINT (same as the .sh): "the latest release" is NOT "the latest
  release I can use". Assets are published per platform and are incomplete —
  v0.4.0 carries only windows assets, v0.3.0 only linux. Resolving via
  /releases/latest would hand Linux a v0.4.0 that 404s, and would break the
  moment a release ships Linux-only. So we scan the list and take the newest
  release whose asset list actually contains our file.

  Usage:
    .\latest_version.ps1 [-Platform <p>] [-Json] [-Refresh]
    . .\latest_version.ps1 -SourceOnly     # dot-source: define functions only

  stdout: the version WITHOUT the leading "v" (e.g. "0.4.0"), or the JSON object
  exit:   0 resolved · 3 nothing nameable for this platform (caller builds)

  `source` tells the caller HOW authoritative the answer is, and callers MUST
  branch on it — see the anti-downgrade rule in ensure_uparser.ps1:
    user_pin / api          -> authoritative, may trigger a supersede
    cache / last_known_good / api_no_asset / fallback_pin -> advisory only
#>
[CmdletBinding()]
param(
  [string] $Platform = '',
  [switch] $Json,
  [switch] $Refresh,
  [switch] $SourceOnly
)

$script:UpRepo    = if ($env:UPARSER_REPO)     { $env:UPARSER_REPO }     else { 'WALLE-AI/uparserstudio' }
$script:UpApiBase = if ($env:UPARSER_API_BASE) { $env:UPARSER_API_BASE } else { 'https://api.github.com' }
# UPARSER_API_BASE exists ONLY so the failure ladder is testable on a machine
# with working internet. Do not use it for anything else.

# Deliberately NOT under ~/.cache/uparser: `uparser cache clear` is a
# remove_dir_all() on exactly that directory (uparser-core/src/cache.rs), so
# state kept there is destroyed by a routine user action.
$script:UpState = if ($env:UPARSER_SKILL_HOME) { $env:UPARSER_SKILL_HOME } else { Join-Path $HOME '.cache/uparser-skill' }

$script:UpTtlOk  = if ($env:UPARSER_VERSION_TTL)       { [int]$env:UPARSER_VERSION_TTL }       else { 21600 }  # 6h
$script:UpTtlErr = if ($env:UPARSER_VERSION_ERROR_TTL) { [int]$env:UPARSER_VERSION_ERROR_TTL } else { 900 }    # 15m
# The negative TTL is the important one: without it an offline 100-file batch
# pays 100 connect timeouts. There is no API mirror to fall back on —
# ghfast.top proxies release DOWNLOADS, not api.github.com (verified: 403).

function Get-UpFallbackPin([string]$plat) {
  switch ($plat) {
    'windows-x86_64' { '0.4.0' }
    'linux-x86_64'   { '0.3.0' }
    default          { '' }
  }
}

<#
  Compare-UpVersion <a> <b> -> -1 | 0 | 1 | $null

  $null means "could not parse", and callers MUST treat that as "keep what you
  have" rather than coercing to 0.0.0, which would silently downgrade a working
  but oddly-versioned binary.

  Hand-rolled on purpose. [version] throws on "0.4.0-rc.2", and
  [System.Management.Automation.SemanticVersion] does not exist on Windows
  PowerShell 5.1 (verified on this machine).
#>
function Compare-UpVersion([string]$a, [string]$b) {
  $rx = '^[0-9]+(\.[0-9]+)*([-+].*)?$'
  $a = $a -replace '^v', ''; $b = $b -replace '^v', ''
  if ($a -notmatch $rx -or $b -notmatch $rx) { return $null }

  $sa = $a -split '-', 2; $sb = $b -split '-', 2
  $ca = ($sa[0] -split '\+')[0]; $cb = ($sb[0] -split '\+')[0]
  $pa = if ($sa.Count -gt 1) { ($sa[1] -split '\+')[0] } else { '' }
  $pb = if ($sb.Count -gt 1) { ($sb[1] -split '\+')[0] } else { '' }

  $xa = $ca -split '\.'; $xb = $cb -split '\.'
  for ($i = 0; $i -lt 3; $i++) {
    $u = if ($i -lt $xa.Count) { [int]$xa[$i] } else { 0 }
    $v = if ($i -lt $xb.Count) { [int]$xb[$i] } else { 0 }
    if ($u -ne $v) { return $(if ($u -lt $v) { -1 } else { 1 }) }
  }
  if ($pa -eq '' -and $pb -eq '') { return 0 }
  if ($pa -eq '') { return 1 }      # release beats its own prerelease
  if ($pb -eq '') { return -1 }

  $ya = $pa -split '\.'; $yb = $pb -split '\.'
  $k = [Math]::Max($ya.Count, $yb.Count)
  for ($i = 0; $i -lt $k; $i++) {
    if ($i -ge $ya.Count) { return -1 }   # shorter prerelease chain is lower
    if ($i -ge $yb.Count) { return 1 }
    $an = $ya[$i] -match '^[0-9]+$'; $bn = $yb[$i] -match '^[0-9]+$'
    if ($an -and $bn) {
      if ([int]$ya[$i] -ne [int]$yb[$i]) { return $(if ([int]$ya[$i] -lt [int]$yb[$i]) { -1 } else { 1 }) }
    }
    elseif ($an) { return -1 }            # numeric identifier < alphanumeric
    elseif ($bn) { return 1 }
    else {
      # CompareOrdinal, not -lt: PowerShell string comparison is culture-aware
      # and would order differently under e.g. a Turkish or Chinese locale.
      $c = [string]::CompareOrdinal($ya[$i], $yb[$i])
      if ($c -ne 0) { return $(if ($c -lt 0) { -1 } else { 1 }) }
    }
  }
  return 0
}

# Version out of `<bin> --version` ("uparser 0.4.0-rc.2").
# Returns '' when unparseable — callers treat that as "keep it".
function Get-UpBinVersion([string]$bin) {
  if (-not $bin -or -not (Test-Path -LiteralPath $bin)) { return '' }
  try {
    $line = (& $bin --version 2>$null | Select-Object -First 1)
    if ($line -match '([0-9]+\.[0-9]+\.[0-9]+[0-9A-Za-z.+-]*)') { return $Matches[1] }
  } catch { }
  return ''
}

if ($SourceOnly) { return }

$ErrorActionPreference = 'Stop'
if (-not $Platform) { $Platform = 'windows-x86_64' }
$sfx = if ($Platform -like 'windows-*') { '.exe' } else { '' }

function Write-UpResult($version, $source, $asset, $age) {
  if ($Json) {
    Write-Output ('{"version":"' + $version + '","source":"' + $source +
                  '","asset":"' + $asset + '","age_secs":' + $age + '}')
  } else { Write-Output $version }
}

# 1) explicit pin — authoritative, no network, no cache write
if ($env:UPARSER_VERSION) {
  $v = $env:UPARSER_VERSION -replace '^v', ''
  [Console]::Error.WriteLine("latest_version: using pinned `$UPARSER_VERSION=$v")
  Write-UpResult $v 'user_pin' "uparser-v$v-$Platform$sfx" 0
  exit 0
}

$stateFile = Join-Path $script:UpState "latest-$Platform.json"
$cv = ''; $cs = ''; $ca = ''; $age = -1
if (Test-Path -LiteralPath $stateFile) {
  try {
    $st = Get-Content -LiteralPath $stateFile -Raw | ConvertFrom-Json
    $cv = [string]$st.version; $cs = [string]$st.source; $ca = [string]$st.asset
    $age = [int]((Get-Date -UFormat %s) -as [double]) - [int]$st.stored_at
  } catch { $cv = ''; $cs = ''; $ca = ''; $age = -1 }   # any parse failure is a clean miss
}

# 2) fresh positive cache
if (-not $Refresh -and $cv -and $cs -ne 'error' -and $age -ge 0 -and $age -le $script:UpTtlOk) {
  Write-UpResult $cv 'cache' $ca $age; exit 0
}

# 3) fresh negative cache / offline -> skip the network entirely
$skipNet = $false
if (-not $Refresh -and $cs -eq 'error' -and $age -ge 0 -and $age -le $script:UpTtlErr) { $skipNet = $true }
if ($env:UPARSER_OFFLINE -eq '1') { $skipNet = $true }

function Write-UpState($version, $source, $asset) {
  try {
    New-Item -ItemType Directory -Force -Path $script:UpState | Out-Null
    $now = [int]((Get-Date -UFormat %s) -as [double])
    $tmp = "$stateFile.$PID"
    ('{"stored_at":' + $now + ',"version":"' + $version + '","asset":"' + $asset +
     '","source":"' + $source + '"}') | Set-Content -LiteralPath $tmp -Encoding ascii
    Move-Item -Force -LiteralPath $tmp -Destination $stateFile
  } catch { }   # state is an optimization; never fail the run over it
}

# 4) live API call
if (-not $skipNet) {
  # PS 5.1 on some hosts still defaults to TLS 1.0, which api.github.com
  # rejects with an opaque "request was aborted". Harmless if already set.
  try {
    [Net.ServicePointManager]::SecurityProtocol =
      [Net.SecurityProtocolType]::Tls12 -bor [Net.ServicePointManager]::SecurityProtocol
  } catch { }

  $hdrs = @{ 'User-Agent' = 'uparser-skill'; 'Accept' = 'application/vnd.github+json' }
  $tok = if ($env:UPARSER_GITHUB_TOKEN) { $env:UPARSER_GITHUB_TOKEN } else { $env:GITHUB_TOKEN }
  if ($tok) { $hdrs['Authorization'] = "Bearer $tok" }

  $releases = $null
  try {
    # per_page=100, not 30: once the repo has >30 releases, a platform whose
    # newest asset is older than #30 would silently resolve to nothing.
    $releases = Invoke-RestMethod -UseBasicParsing -TimeoutSec 20 -Headers $hdrs `
                  -Uri "$($script:UpApiBase)/repos/$($script:UpRepo)/releases?per_page=100"
  } catch { $releases = $null }

  if ($null -ne $releases) {
    $allowPre = ($env:UPARSER_PRERELEASE -eq '1')
    # Exact asset-name equality, not a regex: the release object has its own
    # "name" ("v0.4.0") and ships sibling assets (SHA256SUMS, the .zip, the
    # pdfium dll) that a loose match would happily accept.
    $hit = $releases |
      Where-Object { -not $_.draft -and ($allowPre -or -not $_.prerelease) } |
      Where-Object { $_.assets.name -contains ("uparser-" + $_.tag_name + "-" + $Platform + $sfx) } |
      Select-Object -First 1

    if ($hit) {
      $v = [string]$hit.tag_name -replace '^v', ''
      $asset = "uparser-" + $hit.tag_name + "-" + $Platform + $sfx
      Write-UpState $v 'api' $asset
      Write-UpResult $v 'api' $asset 0; exit 0
    }
    # API answered fine, no release carries our asset. A stable fact, not a
    # transient failure, so it is cached with the SUCCESS ttl.
    [Console]::Error.WriteLine("latest_version: no published release carries an asset for $Platform")
    if ($cv -and $cs -ne 'error') { Write-UpResult $cv 'last_known_good' $ca $age; exit 0 }
    $p = Get-UpFallbackPin $Platform
    if ($p) { Write-UpState $p 'api_no_asset' "uparser-v$p-$Platform$sfx"
              Write-UpResult $p 'api_no_asset' "uparser-v$p-$Platform$sfx" 0; exit 0 }
    exit 3
  }
  [Console]::Error.WriteLine('latest_version: GitHub API unreachable; falling back (no API mirror exists)')
  Write-UpState $cv 'error' $ca
}

# 5) serve stale — the primary anti-downgrade mechanism
if ($cv) { Write-UpResult $cv 'last_known_good' $ca $age; exit 0 }

# 6) built-in pin
$p = Get-UpFallbackPin $Platform
if ($p) { Write-UpResult $p 'fallback_pin' "uparser-v$p-$Platform$sfx" -1; exit 0 }

# 7) nothing nameable — caller builds from source
[Console]::Error.WriteLine("latest_version: no fallback pin for $Platform")
exit 3
