[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)][string]$OutputDirectory
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$repositoryRoot = Split-Path -Parent $PSScriptRoot
$distributionDirectory = Join-Path $repositoryRoot 'dist'
$absoluteOutput = [IO.Path]::GetFullPath($OutputDirectory)
$portableExecutable = Join-Path $distributionDirectory 'CurlDownloader.exe'
$extensionPackage = Join-Path $distributionDirectory 'curl-downloader.xpi'



$distributionExecutables = @(Get-ChildItem -LiteralPath $distributionDirectory -Filter '*.exe' -File -ErrorAction Stop)
if ($distributionExecutables.Count -ne 1 -or $distributionExecutables[0].Name -ne 'CurlDownloader.exe') {
    throw 'portable 輸入必須只包含一份 CurlDownloader.exe。'
}
foreach ($required in @($portableExecutable, $extensionPackage)) {
    if (-not (Test-Path -LiteralPath $required -PathType Leaf)) {
        throw "portable 發行檔缺少必要檔案：$required"
    }
}

New-Item -ItemType Directory -Path $absoluteOutput -Force | Out-Null
Copy-Item -LiteralPath $portableExecutable -Destination (Join-Path $absoluteOutput 'CurlDownloader.exe') -Force
Copy-Item -LiteralPath $extensionPackage -Destination (Join-Path $absoluteOutput 'curl-downloader.xpi') -Force


[IO.File]::WriteAllText(
    (Join-Path $absoluteOutput 'portable.flag'),
    "Curl Downloader portable mode`r`n",
    [Text.UTF8Encoding]::new($false)
)

$allowedEntries = @('CurlDownloader.exe', 'curl-downloader.xpi', 'portable.flag')
$outputEntries = @(Get-ChildItem -LiteralPath $absoluteOutput -Force)
$unexpectedEntries = @($outputEntries | Where-Object { $allowedEntries -notcontains $_.Name })
if ($unexpectedEntries.Count -gt 0 -or $outputEntries.Count -ne $allowedEntries.Count) {
    $names = ($outputEntries | Select-Object -ExpandProperty Name) -join ', '
    throw "portable 輸出只允許三項檔案；目前內容：$names"
}
$outputExecutables = @(Get-ChildItem -LiteralPath $absoluteOutput -Filter '*.exe' -File)
if ($outputExecutables.Count -ne 1 -or $outputExecutables[0].Name -ne 'CurlDownloader.exe') {
    throw 'portable 輸出必須只包含一份 CurlDownloader.exe。'
}
Write-Output "已建立 portable 發行目錄：$absoluteOutput"
Write-Output '內容包括單一 GUI、Firefox XPI 及 portable.flag；不含額外 Native host 或啟動腳本。'
Write-Output '首次啟動 CurlDownloader.exe 時，GUI 啟動時會自動註冊 Firefox Native host。'
Write-Output '之後可在 Firefox 載入 curl-downloader.xpi；若設定頁等待 Native host，按「重試 Curl Downloader」。'