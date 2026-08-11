<#
  ensure_uparser.ps1 — guarantee a runnable uparser.exe is present on Windows,
  printing its path. Resolution order:
    1) uparser(.exe) already on PATH
    2) previously downloaded copy in the cache
    3) download a version-pinned prebuilt from GitHub Releases (direct, then
       ghfast.top mirror), verify sha256
    4) on failure / no published Windows asset, fall back to building from
       source via build-windows.ps1

  Env overrides: UPARSER_VERSION, UPARSER_REPO, UPARSER_HOME (cache root).
  Returns the binary path as the last output line.
#>
[CmdletBinding()] param()
$ErrorActionPreference = 'Stop'

$version = if ($env:UPARSER_VERSION) { $env:UPARSER_VERSION } else { '0.1.1' }
$repo    = if ($env:UPARSER_REPO)    { $env:UPARSER_REPO }    else { 'WALLE-AI/uparserstudio' }
$cache   = Join-Path (if ($env:UPARSER_HOME) { $env:UPARSER_HOME } else { Join-Path $HOME '.cache/uparser' }) 'bin'
$here    = $PSScriptRoot

# 1) already on PATH
$onPath = (Get-Command uparser.exe -ErrorAction SilentlyContinue).Source
if (-not $onPath) { $onPath = (Get-Command uparser -ErrorAction SilentlyContinue).Source }
if ($onPath) { return $onPath }
# 2) cached
$cached = Join-Path $cache 'uparser.exe'
if (Test-Path $cached) { return $cached }

# 3) map platform -> asset (only x64 published)
if ([Environment]::Is64BitOperatingSystem) {
  $asset = "uparser-v$version-windows-x86_64.exe"
} else {
  Write-Warning "no prebuilt for 32-bit Windows — building from source"
  & (Join-Path $here 'build-windows.ps1'); return (Join-Path $HOME '.local/bin/uparser.exe')
}

$base = "https://github.com/$repo/releases/download/v$version"
New-Item -ItemType Directory -Force -Path $cache | Out-Null
$tmp = Join-Path ([System.IO.Path]::GetTempPath()) ([System.IO.Path]::GetRandomFileName())

function Fetch($url, $dest) {
  try { Invoke-WebRequest -UseBasicParsing -Uri $url -OutFile $dest -TimeoutSec 30; return $true }
  catch {
    try { Invoke-WebRequest -UseBasicParsing -Uri "https://ghfast.top/$url" -OutFile $dest -TimeoutSec 40; return $true }
    catch { return $false }
  }
}

Write-Host "downloading $asset (v$version) ..." -ForegroundColor Cyan
if (-not (Fetch "$base/$asset" $tmp)) {
  Write-Warning "download failed (direct + mirror) — building from source"
  & (Join-Path $here 'build-windows.ps1'); return (Join-Path $HOME '.local/bin/uparser.exe')
}

# checksum (best-effort)
$sums = "$tmp.sums"
if (Fetch "$base/SHA256SUMS" $sums) {
  $line = Get-Content $sums | Where-Object { $_ -match ([regex]::Escape($asset) + '\s*$') } | Select-Object -First 1
  if ($line) {
    $want = ($line -split '\s+')[0].ToLower()
    $got  = (Get-FileHash -Algorithm SHA256 $tmp).Hash.ToLower()
    if ($want -ne $got) { Remove-Item $tmp,$sums -Force; throw "checksum mismatch for $asset" }
  }
  Remove-Item $sums -Force -ErrorAction SilentlyContinue
}

Move-Item -Force $tmp $cached
return $cached
