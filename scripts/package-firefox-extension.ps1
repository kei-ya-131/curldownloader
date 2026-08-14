[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)][string]$OutputPath
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$repositoryRoot = Split-Path -Parent $PSScriptRoot
$extensionRoot = Join-Path $repositoryRoot 'firefox-extension'
$runtimeFiles = @(
    'manifest.json',
    'core.js',
    'storage.js',
    'status.js',
    'native-session.js',
    'request-context.js',
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
            throw "Extension runtime file not found: $file"
        }
        Copy-Item -LiteralPath $source -Destination (Join-Path $stageDirectory $file) -Force
    }

    $sourceIcons = Join-Path $extensionRoot 'icons'
    $stageIcons = Join-Path $stageDirectory 'icons'
    $iconFiles = @(Get-ChildItem -LiteralPath $sourceIcons -Filter '*.png' -File -ErrorAction Stop)
    if ($iconFiles.Count -lt 14) { throw 'Extension icon assets are incomplete.' }
    New-Item -ItemType Directory -Path $stageIcons -Force | Out-Null
    foreach ($icon in $iconFiles) {
        Copy-Item -LiteralPath $icon.FullName -Destination (Join-Path $stageIcons $icon.Name) -Force
    }

    New-Item -ItemType Directory -Path $outputDirectory -Force | Out-Null
    Add-Type -AssemblyName System.IO.Compression
    $archiveStream = [IO.File]::Create($temporaryArchive)
    $archive = [IO.Compression.ZipArchive]::new(
        $archiveStream,
        [IO.Compression.ZipArchiveMode]::Create,
        $false
    )
    try {
        foreach ($file in Get-ChildItem -LiteralPath $stageDirectory -Recurse -File) {
            $relative = $file.FullName.Substring($stageDirectory.Length)
            $relative = $relative.TrimStart([char[]]@('\', '/')).Replace('\', '/')
            $entry = $archive.CreateEntry(
                $relative,
                [IO.Compression.CompressionLevel]::Optimal
            )
            $input = [IO.File]::OpenRead($file.FullName)
            $output = $entry.Open()
            try {
                $input.CopyTo($output)
            } finally {
                $output.Dispose()
                $input.Dispose()
            }
        }
    } finally {
        $archive.Dispose()
        $archiveStream.Dispose()
    }

    & (Join-Path $PSScriptRoot 'test-firefox-extension-package.ps1') -PackagePath $temporaryArchive
    Move-Item -LiteralPath $temporaryArchive -Destination $absoluteOutput -Force
    Write-Output "Firefox extension package created: $absoluteOutput"
} finally {
    if (Test-Path -LiteralPath $temporaryArchive) {
        Remove-Item -LiteralPath $temporaryArchive -Force
    }
    if (Test-Path -LiteralPath $stageDirectory) {
        Remove-Item -LiteralPath $stageDirectory -Recurse -Force
    }
}
