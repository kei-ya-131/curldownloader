[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)][string]$PackagePath
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$absolutePackage = [IO.Path]::GetFullPath($PackagePath)
if (-not (Test-Path -LiteralPath $absolutePackage -PathType Leaf)) {
    throw "XPI not found: $absolutePackage"
}

Add-Type -AssemblyName System.IO.Compression.FileSystem
$archive = [IO.Compression.ZipFile]::OpenRead($absolutePackage)
try {
    $names = @($archive.Entries | ForEach-Object FullName)
    if (@($names | Where-Object { $_ -match '\\' }).Count -ne 0) {
        throw 'XPI entries must use forward slashes.'
    }

    $manifestEntry = $archive.GetEntry("manifest.json")
    if ($null -eq $manifestEntry) {
        throw 'XPI is missing manifest.json.'
    }
    $reader = [IO.StreamReader]::new($manifestEntry.Open())
    try {
        $manifest = $reader.ReadToEnd() | ConvertFrom-Json
    } finally {
        $reader.Dispose()
    }

    $required = @($manifest.icons.PSObject.Properties.Value) +
        @($manifest.browser_action.default_icon.PSObject.Properties.Value) +
        @(0..10 | ForEach-Object { 'icons/progress-{0:D3}.png' -f ($_ * 10) })
    foreach ($name in $required | Select-Object -Unique) {
        if ($null -eq $archive.GetEntry($name)) {
            throw "XPI is missing icon: $name"
        }
    }
} finally {
    $archive.Dispose()
}

Write-Output "XPI icon package validation passed: $absolutePackage"
