[CmdletBinding()]
param(
    [string] $Version = '0.4.0-rc.1',
    [string] $BinaryPath = '',
    [string] $PdfiumPath = '',
    [string] $PdfiumDistributionPath = '',
    [string] $OutputDirectory = '',
    [switch] $SkipSmokeTest
)

$ErrorActionPreference = 'Stop'
$repoRoot = (Resolve-Path -LiteralPath (Join-Path $PSScriptRoot '..')).Path
$workspace = Join-Path $repoRoot 'uparser'

if ($Version -notmatch '^\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?$') {
    throw "Invalid semantic version: $Version"
}

if (-not $BinaryPath) {
    $BinaryPath = Join-Path $workspace 'target\release\uparser.exe'
}
if (-not $PdfiumPath) {
    $PdfiumPath = Join-Path $workspace 'target\release\pdfium.dll'
}
if (-not $OutputDirectory) {
    $OutputDirectory = Join-Path $repoRoot 'dist'
}

$BinaryPath = (Resolve-Path -LiteralPath $BinaryPath).Path
$PdfiumPath = (Resolve-Path -LiteralPath $PdfiumPath).Path
$outputRoot = [System.IO.Path]::GetFullPath($OutputDirectory)
$repoPrefix = $repoRoot.TrimEnd('\') + '\'
if (-not $outputRoot.StartsWith($repoPrefix, [System.StringComparison]::OrdinalIgnoreCase)) {
    throw "OutputDirectory must remain inside the repository: $outputRoot"
}

$coreManifest = Get-Content -LiteralPath (Join-Path $workspace 'crates\uparser-core\Cargo.toml') -Raw
$declaredVersion = [regex]::Match($coreManifest, '(?m)^version\s*=\s*"([^"]+)"').Groups[1].Value
if ($declaredVersion -ne $Version) {
    throw "Package version $Version does not match uparser-core $declaredVersion"
}

if (-not $PdfiumDistributionPath) {
    $pdfiumCache = Join-Path $env:LOCALAPPDATA 'pdfium-rs'
    $PdfiumDistributionPath = Get-ChildItem -LiteralPath $pdfiumCache -Directory -Recurse -ErrorAction SilentlyContinue |
        Where-Object {
            (Test-Path -LiteralPath (Join-Path $_.FullName 'LICENSE') -PathType Leaf) -and
            (Test-Path -LiteralPath (Join-Path $_.FullName 'licenses') -PathType Container) -and
            (Test-Path -LiteralPath (Join-Path $_.FullName 'bin\pdfium.dll') -PathType Leaf)
        } |
        Select-Object -ExpandProperty FullName -First 1
}
if (-not $PdfiumDistributionPath) {
    throw 'Cannot locate the PDFium distribution licenses. Pass -PdfiumDistributionPath.'
}
$PdfiumDistributionPath = (Resolve-Path -LiteralPath $PdfiumDistributionPath).Path

if (-not $SkipSmokeTest) {
    $versionOutput = & $BinaryPath --version
    if ($LASTEXITCODE -ne 0 -or $versionOutput -notmatch [regex]::Escape($Version)) {
        throw "uparser executable smoke test failed or reported the wrong version"
    }
}

New-Item -ItemType Directory -Force -Path $outputRoot | Out-Null
$assetBase = "uparser-v$Version-windows-x86_64"
$exeAsset = Join-Path $outputRoot "$assetBase.exe"
$dllAsset = Join-Path $outputRoot "$assetBase-pdfium.dll"
$zipAsset = Join-Path $outputRoot "$assetBase.zip"
$sumFile = Join-Path $outputRoot 'SHA256SUMS'
$staging = Join-Path $outputRoot ".$assetBase-staging"

if (Test-Path -LiteralPath $staging) {
    Remove-Item -LiteralPath $staging -Recurse -Force
}
New-Item -ItemType Directory -Path $staging | Out-Null

try {
    Copy-Item -LiteralPath $BinaryPath -Destination $exeAsset -Force
    Copy-Item -LiteralPath $PdfiumPath -Destination $dllAsset -Force
    Copy-Item -LiteralPath $BinaryPath -Destination (Join-Path $staging 'uparser.exe')
    Copy-Item -LiteralPath $PdfiumPath -Destination (Join-Path $staging 'pdfium.dll')

    $thirdParty = Join-Path $staging 'third-party'
    $nativeNotices = Join-Path $thirdParty 'uparser-native-engine'
    $pdfiumNotices = Join-Path $thirdParty 'pdfium'
    New-Item -ItemType Directory -Path $nativeNotices, $pdfiumNotices | Out-Null
    Copy-Item -LiteralPath (Join-Path $workspace 'crates\uparser-native-engine\LICENSE') -Destination $nativeNotices
    Copy-Item -LiteralPath (Join-Path $workspace 'crates\uparser-native-engine\ATTRIBUTION.md') -Destination $nativeNotices
    Copy-Item -LiteralPath (Join-Path $PdfiumDistributionPath 'LICENSE') -Destination $pdfiumNotices
    Copy-Item -LiteralPath (Join-Path $PdfiumDistributionPath 'licenses') -Destination $pdfiumNotices -Recurse
    $pdfiumVersion = Join-Path $PdfiumDistributionPath 'VERSION'
    if (Test-Path -LiteralPath $pdfiumVersion) {
        Copy-Item -LiteralPath $pdfiumVersion -Destination $pdfiumNotices
    }

    $notes = Join-Path $repoRoot "RELEASE_NOTES_v$Version.md"
    if (Test-Path -LiteralPath $notes) {
        Copy-Item -LiteralPath $notes -Destination (Join-Path $staging 'RELEASE_NOTES.md')
    }

    $commit = (git -C $repoRoot rev-parse HEAD).Trim()
    $dirty = [bool](git -C $repoRoot status --porcelain --untracked-files=no)
    [ordered]@{
        version = $Version
        target = 'windows-x86_64'
        features = @('native', 'pdfium')
        source_commit = $commit
        source_dirty = $dirty
        product_license = 'UNLICENSED'
        pdfium_required_for = @('rasterization', 'ocr', 'vision protocols')
        packaged_utc = [DateTime]::UtcNow.ToString('o')
    } | ConvertTo-Json -Depth 4 | Set-Content -LiteralPath (Join-Path $staging 'RELEASE_MANIFEST.json') -Encoding utf8

    if (Test-Path -LiteralPath $zipAsset) {
        Remove-Item -LiteralPath $zipAsset -Force
    }
    Compress-Archive -Path (Join-Path $staging '*') -DestinationPath $zipAsset -CompressionLevel Optimal

    @($exeAsset, $dllAsset, $zipAsset) | ForEach-Object {
        $hash = (Get-FileHash -LiteralPath $_ -Algorithm SHA256).Hash.ToLowerInvariant()
        "$hash  $([System.IO.Path]::GetFileName($_))"
    } | Set-Content -LiteralPath $sumFile -Encoding ascii
} finally {
    if (Test-Path -LiteralPath $staging) {
        Remove-Item -LiteralPath $staging -Recurse -Force
    }
}

Get-Item -LiteralPath $exeAsset, $dllAsset, $zipAsset, $sumFile |
    Select-Object Name, Length, LastWriteTime
