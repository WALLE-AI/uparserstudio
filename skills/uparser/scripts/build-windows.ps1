<#
  build-windows.ps1 — build uparser from source on Windows (MSVC) and install
  the skill + binary. There is no prebuilt Windows binary; this compiles one.

  Requirements on the target machine:
    - rustup with the MSVC toolchain  (https://rustup.rs)
    - Visual Studio Build Tools (C++), for the MSVC linker
    - git, and a working internet connection for the first build
      (crates.io + a one-time PDFium download when -Features includes pdfium)

  Usage (from the skill's scripts/ dir, or anywhere with -Workspace):
    # safest first try — pure-Rust native only, no PDFium download:
    .\build-windows.ps1 -Features native
    # full (adds page rasterization for the VLM/OCR protocols):
    .\build-windows.ps1 -Features "native,pdfium"
    # if the workspace isn't auto-found:
    .\build-windows.ps1 -Workspace C:\path\to\uparserstudio\uparser

  NOTE: native Windows build is not yet CI-verified. `native`/`parse` should
  work; `doctor pipeline` degrades (its /proc/meminfo memory report is
  Linux-only and returns null on Windows — non-fatal).
#>
[CmdletBinding()]
param(
  [string] $Features = 'native,pdfium',
  [string] $Workspace = ''
)
$ErrorActionPreference = 'Stop'

# --- locate the uparser workspace (walk up from this script) ---
if (-not $Workspace) {
  $d = $PSScriptRoot
  while ($d) {
    if (Test-Path (Join-Path $d 'uparser/Cargo.toml')) { $Workspace = Join-Path $d 'uparser'; break }
    if ((Test-Path (Join-Path $d 'Cargo.toml')) -and (Test-Path (Join-Path $d 'crates/uparser-core'))) { $Workspace = $d; break }
    $parent = Split-Path $d -Parent
    if ($parent -eq $d) { break }
    $d = $parent
  }
}
if (-not $Workspace -or -not (Test-Path (Join-Path $Workspace 'Cargo.toml'))) {
  Write-Error 'uparser workspace not found. Pass -Workspace <path to uparserstudio\uparser>.'; exit 2
}

Write-Host "[*] building uparser (features: $Features) in $Workspace ..." -ForegroundColor Cyan
Push-Location $Workspace
try { cargo build --release -p uparser-core --features $Features } finally { Pop-Location }

$exe = Join-Path $Workspace 'target/release/uparser.exe'
if (-not (Test-Path $exe)) { Write-Error "build finished but $exe not found"; exit 3 }

# --- install binary to ~/.local/bin ---
$binDir = Join-Path $HOME '.local/bin'
New-Item -ItemType Directory -Force -Path $binDir | Out-Null
Copy-Item $exe (Join-Path $binDir 'uparser.exe') -Force
Write-Host "[ok] binary -> $binDir\uparser.exe"

# --- install skill to ~/.claude/skills/uparser ---
$skillSrc = Split-Path $PSScriptRoot -Parent            # skills/uparser
$skillDst = Join-Path $HOME '.claude/skills/uparser'
New-Item -ItemType Directory -Force -Path (Split-Path $skillDst -Parent) | Out-Null
if (Test-Path $skillDst) { Remove-Item -Recurse -Force $skillDst }
Copy-Item -Recurse $skillSrc $skillDst
Write-Host "[ok] skill  -> $skillDst"

# --- PATH hint + verify ---
if (-not (($env:PATH -split ';') -contains $binDir)) {
  Write-Host "[!] add ~/.local/bin to PATH permanently:" -ForegroundColor Yellow
  Write-Host "    setx PATH `"$binDir;`$env:PATH`""
  $env:PATH = "$binDir;$env:PATH"
}
& (Join-Path $binDir 'uparser.exe') protocols > $null
if ($LASTEXITCODE -eq 0) { Write-Host "[done] verified: uparser protocols OK" -ForegroundColor Green }
else { Write-Error "binary built but failed to run"; exit 4 }
