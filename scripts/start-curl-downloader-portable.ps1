[CmdletBinding()]
param(
    [string]$FirefoxExecutablePath,
    [string[]]$FirefoxArgumentList = @()
)

Set-StrictMode -Version Latest
$portableRoot = $PSScriptRoot
$portableExecutable = Join-Path $portableRoot 'CurlDownloader.exe'
if (-not (Test-Path -LiteralPath $portableExecutable -PathType Leaf)) {
    throw "找不到 portable CurlDownloader.exe：$portableExecutable"
}

$env:CURL_DOWNLOADER_PORTABLE = '1'
Start-Process -FilePath $portableExecutable -WorkingDirectory $portableRoot -WindowStyle Normal | Out-Null

if (-not [string]::IsNullOrWhiteSpace($FirefoxExecutablePath)) {
    $firefox = Resolve-Path -LiteralPath $FirefoxExecutablePath -ErrorAction Stop
    if (-not (Test-Path -LiteralPath $firefox.Path -PathType Leaf)) {
        throw "Firefox 執行檔不存在：$FirefoxExecutablePath"
    }
    $firefoxWorkingDirectory = Split-Path -Parent $firefox.Path
    Start-Process -FilePath $firefox.Path `
        -ArgumentList $FirefoxArgumentList `
        -WorkingDirectory $firefoxWorkingDirectory `
        -WindowStyle Normal | Out-Null
}

Write-Output "已啟動 portable Curl Downloader：$portableExecutable"
if (-not [string]::IsNullOrWhiteSpace($FirefoxExecutablePath)) {
    Write-Output "已啟動 Firefox：$FirefoxExecutablePath"
}