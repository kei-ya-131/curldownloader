[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)][string]$ExecutablePath
)

Set-StrictMode -Version Latest
$hostName = 'curl_downloader'
$extensionId = 'curl-downloader@kinkeil.local'
$temporaryDirectory = Join-Path ([IO.Path]::GetTempPath()) ("CurlDownloader-NativeHost-Test-" + [Guid]::NewGuid().ToString('N'))
$manifestPath = Join-Path $temporaryDirectory "$hostName.json"

try {
    New-Item -ItemType Directory -Path $temporaryDirectory -Force | Out-Null
    $absoluteExecutable = [IO.Path]::GetFullPath((Resolve-Path -LiteralPath $ExecutablePath -ErrorAction Stop).Path)
    $manifest = [ordered]@{
        name = $hostName
        description = 'Curl Downloader Firefox Native Messaging host'
        path = $absoluteExecutable
        type = 'stdio'
        allowed_extensions = @($extensionId)
    }
    $json = $manifest | ConvertTo-Json -Depth 4
    [IO.File]::WriteAllText($manifestPath, $json, [Text.UTF8Encoding]::new($false))
    $parsed = Get-Content -Raw -LiteralPath $manifestPath | ConvertFrom-Json
    if ($parsed.name -ne $hostName -or $parsed.type -ne 'stdio' -or $parsed.path -ne $absoluteExecutable) {
        throw "Native host manifest fields are invalid."
    }
    if ($parsed.allowed_extensions.Count -ne 1 -or $parsed.allowed_extensions[0] -ne $extensionId) {
        throw "Native host allowed_extensions invalid."
    }
    Write-Output "Native host manifest smoke test passed: $manifestPath"
} finally {
    if (Test-Path -LiteralPath $temporaryDirectory) {
        Remove-Item -LiteralPath $temporaryDirectory -Recurse -Force
    }
}
