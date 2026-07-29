[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)][string]$OutputPath
)

Set-StrictMode -Version Latest
$repositoryRoot = Split-Path -Parent $PSScriptRoot
$extensionRoot = Join-Path $repositoryRoot 'firefox-extension'
$runtimeFiles = @(
    'manifest.json',
    'core.js',
    'storage.js',
    'status.js',
    'background.js',
    'settings.html',
    'settings.css',
    'settings.js',
    'popup.html',
    'popup.css',
    'popup.js'
)
$absoluteOutput = [IO.Path]::GetFullPath($OutputPath)
$outputDirectory = Split-Path -Parent $absoluteOutput
$stageDirectory = Join-Path ([IO.Path]::GetTempPath()) ("CurlDownloader-Xpi-" + [Guid]::NewGuid().ToString('N'))
$temporaryArchive = "$absoluteOutput.$([Guid]::NewGuid().ToString('N')).zip"

try {
    New-Item -ItemType Directory -Path $stageDirectory -Force | Out-Null
    foreach ($file in $runtimeFiles) {
        $source = Join-Path $extensionRoot $file
        if (-not (Test-Path -LiteralPath $source -PathType Leaf)) {
            throw "Extension runtime file 不存在：$file"
        }
        Copy-Item -LiteralPath $source -Destination (Join-Path $stageDirectory $file) -Force
    }
    $sourceIcons = Join-Path $extensionRoot 'icons'
    $stageIcons = Join-Path $stageDirectory 'icons'
    $iconFiles = @(Get-ChildItem -LiteralPath $sourceIcons -Filter '*.png' -File -ErrorAction Stop)
    if ($iconFiles.Count -lt 14) { throw 'Extension icon assets 不完整。' }
    New-Item -ItemType Directory -Path $stageIcons -Force | Out-Null
    foreach ($icon in $iconFiles) {
        Copy-Item -LiteralPath $icon.FullName -Destination (Join-Path $stageIcons $icon.Name) -Force
    }
    New-Item -ItemType Directory -Path $outputDirectory -Force | Out-Null
    # Compress-Archive-compatible root layout; ZipFile avoids the Windows PowerShell file-lock race.
    Add-Type -AssemblyName System.IO.Compression.FileSystem
    [IO.Compression.ZipFile]::CreateFromDirectory(
        $stageDirectory,
        $temporaryArchive,
        [IO.Compression.CompressionLevel]::Optimal,
        $false
    )
    Move-Item -LiteralPath $temporaryArchive -Destination $absoluteOutput -Force
    Write-Output "已建立 Firefox extension：$absoluteOutput"
} finally {
    if (Test-Path -LiteralPath $temporaryArchive) { Remove-Item -LiteralPath $temporaryArchive -Force }
    if (Test-Path -LiteralPath $stageDirectory) { Remove-Item -LiteralPath $stageDirectory -Recurse -Force }
}