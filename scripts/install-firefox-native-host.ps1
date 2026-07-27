[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)][string]$ExecutablePath,
    [switch]$Uninstall
)

Set-StrictMode -Version Latest
$hostName = 'curl_downloader'
$extensionId = 'curl-downloader@kinkeil.local'
$registryPath = "HKCU:\Software\Mozilla\NativeMessagingHosts\$hostName"
$supportDirectory = Join-Path ([Environment]::GetFolderPath('ApplicationData')) 'CurlDownloader\firefox-native-host'
$manifestPath = Join-Path $supportDirectory "$hostName.json"

function Get-FullPath([string]$Path) {
    return [IO.Path]::GetFullPath($Path)
}

function Test-InDirectory([string]$Candidate, [string]$Directory) {
    $candidateFull = Get-FullPath $Candidate
    $directoryFull = (Get-FullPath $Directory).TrimEnd('\') + '\'
    return $candidateFull.StartsWith($directoryFull, [StringComparison]::OrdinalIgnoreCase)
}

if (-not (Test-InDirectory $manifestPath $supportDirectory)) {
    throw 'Native host manifest path 不在預期的 per-user 目錄內。'
}

if ($Uninstall) {
    if (Test-Path -LiteralPath $registryPath) {
        $registeredPath = $null
        try {
            $registeredPath = Get-ItemPropertyValue -LiteralPath $registryPath -Name '(default)'
        } catch {
            throw "無法讀取 Native host registry manifest path：$($_.Exception.Message)"
        }
        if ($registeredPath -and -not (Test-InDirectory $registeredPath $supportDirectory)) {
            throw '拒絕移除指向預期支援目錄以外的 Native host registry 設定。'
        }
        Remove-Item -LiteralPath $registryPath -Recurse -Force
    }
    if (Test-Path -LiteralPath $manifestPath) {
        Remove-Item -LiteralPath $manifestPath -Force
    }
    Write-Output "已移除 $hostName Native host 註冊。"
    exit 0
}

$resolvedExecutable = Resolve-Path -LiteralPath $ExecutablePath -ErrorAction Stop
if (-not (Test-Path -LiteralPath $resolvedExecutable.Path -PathType Leaf)) {
    throw "ExecutablePath 不是檔案：$ExecutablePath"
}
$absoluteExecutable = Get-FullPath $resolvedExecutable.Path

New-Item -ItemType Directory -Path $supportDirectory -Force | Out-Null
$manifest = [ordered]@{
    name = $hostName
    description = 'Curl Downloader Firefox Native Messaging host'
    path = $absoluteExecutable
    type = 'stdio'
    allowed_extensions = @($extensionId)
}
$json = $manifest | ConvertTo-Json -Depth 4
[IO.File]::WriteAllText($manifestPath, $json, [Text.UTF8Encoding]::new($false))

New-Item -Path $registryPath -Force | Out-Null
New-ItemProperty -LiteralPath $registryPath -Name '(default)' -Value $manifestPath -PropertyType String -Force | Out-Null
Write-Output "已註冊 $hostName：$manifestPath"
