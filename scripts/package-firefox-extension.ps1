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
    'background.js',
    'settings.html',
    'settings.css',
    'settings.js'
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
    New-Item -ItemType Directory -Path $outputDirectory -Force | Out-Null
    # CreateFromDirectory produces the same root layout as Compress-Archive, without
    # the Windows PowerShell 5.1 file-lock race observed while archiving a stage folder.
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
    if (Test-Path -LiteralPath $temporaryArchive) {
        Remove-Item -LiteralPath $temporaryArchive -Force
    }
    if (Test-Path -LiteralPath $stageDirectory) {
        Remove-Item -LiteralPath $stageDirectory -Recurse -Force
    }
}
