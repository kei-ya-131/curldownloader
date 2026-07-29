[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)][string]$ExecutablePath
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$resolvedExecutable = [IO.Path]::GetFullPath($ExecutablePath)
if (-not (Test-Path -LiteralPath $resolvedExecutable -PathType Leaf)) {
    throw "找不到測試用 CurlDownloader.exe：$resolvedExecutable"
}

$existing = @(Get-Process -Name 'CurlDownloader' -ErrorAction SilentlyContinue)
if ($existing.Count -gt 0) {
    throw '測試開始前發現已有 CurlDownloader.exe；為避免影響使用者程序，smoke test 已停止。'
}

$temporaryDirectory = Join-Path ([IO.Path]::GetTempPath()) ("CurlDownloader-Background-Probe-" + [Guid]::NewGuid().ToString('N'))
$probeExecutable = Join-Path $temporaryDirectory 'CurlDownloader.exe'
$probeProcess = $null

function Read-Exact {
    param(
        [Parameter(Mandatory = $true)][IO.Stream]$Stream,
        [Parameter(Mandatory = $true)][byte[]]$Buffer
    )

    $offset = 0
    while ($offset -lt $Buffer.Length) {
        $read = $Stream.Read($Buffer, $offset, $Buffer.Length - $offset)
        if ($read -le 0) { throw 'Native host 提前結束輸出。' }
        $offset += $read
    }
}

function Invoke-NativeRequest {
    param(
        [Parameter(Mandatory = $true)][string]$Executable,
        [Parameter(Mandatory = $true)][hashtable]$Request
    )

    $json = $Request | ConvertTo-Json -Compress
    $body = [Text.Encoding]::UTF8.GetBytes($json)
    $frame = [byte[]]::new(4 + $body.Length)
    [BitConverter]::GetBytes([uint32]$body.Length).CopyTo($frame, 0)
    $body.CopyTo($frame, 4)

    $start = [Diagnostics.ProcessStartInfo]::new($Executable)
    $start.Arguments = '--native-messaging-host'
    $start.UseShellExecute = $false
    $start.RedirectStandardInput = $true
    $start.RedirectStandardOutput = $true
    $start.RedirectStandardError = $true
    $start.CreateNoWindow = $true
    $process = [Diagnostics.Process]::Start($start)
    try {
        $process.StandardInput.BaseStream.Write($frame, 0, $frame.Length)
        $process.StandardInput.Close()
        $lengthBytes = [byte[]]::new(4)
        Read-Exact -Stream $process.StandardOutput.BaseStream -Buffer $lengthBytes
        $length = [BitConverter]::ToUInt32($lengthBytes, 0)
        if ($length -gt 4MB) { throw 'Native host 回覆超過測試上限。' }
        $responseBytes = [byte[]]::new($length)
        Read-Exact -Stream $process.StandardOutput.BaseStream -Buffer $responseBytes
        if (-not $process.WaitForExit(5000)) { throw 'Native host 沒有在限時內結束。' }
        return [Text.Encoding]::UTF8.GetString($responseBytes) | ConvertFrom-Json
    } finally {
        if (-not $process.HasExited) { $process.Kill() }
        $process.Dispose()
    }
}

try {
    New-Item -ItemType Directory -Path $temporaryDirectory -Force | Out-Null
    Copy-Item -LiteralPath $resolvedExecutable -Destination $probeExecutable
    [IO.File]::WriteAllText(
        (Join-Path $temporaryDirectory 'portable.flag'),
        "Curl Downloader portable background probe`r`n",
        [Text.UTF8Encoding]::new($false)
    )

    $probeDownload = Join-Path $temporaryDirectory 'downloads'
    New-Item -ItemType Directory -Path $probeDownload -Force | Out-Null
    New-Item -ItemType Directory -Path (Join-Path $temporaryDirectory 'data') -Force | Out-Null
    [IO.File]::WriteAllBytes((Join-Path $probeDownload 'probe.bin'), [byte[]](1, 2, 3, 4))
    $state = [ordered]@{
        schema_version = 1
        settings = [ordered]@{
            last_download_dir = $probeDownload
            max_curl_processes = 4
            next_task_id = 7002
        }
        tasks = @([ordered]@{
            id = 7001
            original_url = 'https://example.test/probe.bin'
            effective_url = $null
            filename = 'probe.bin'
            target_dir = $probeDownload
            requested_segments = 1
            actual_segments = 1
            total_size = 4
            etag = $null
            last_modified = $null
            range_support = 'Unknown'
            proxy = [ordered]@{
                enabled = $false
                protocol = 'Http'
                host = ''
                port = 8080
                username = ''
                requires_password = $false
            }
            status = 'Completed'
            segments = @([ordered]@{ index = 0; start = 0; end = 3; downloaded = 4 })
            active_millis = 1
            created_unix_ms = 1
            completed_unix_ms = 2
            last_error = $null
        })
    }
    [IO.File]::WriteAllText(
        (Join-Path $temporaryDirectory 'data\state.json'),
        ($state | ConvertTo-Json -Depth 8),
        [Text.UTF8Encoding]::new($false)
    )

    $probeProcess = Start-Process -FilePath $probeExecutable -ArgumentList '--minimized --skip-native-registration' -WindowStyle Hidden -PassThru
    $ready = $false
    $deadline = [DateTime]::UtcNow.AddSeconds(5)
    while ([DateTime]::UtcNow -lt $deadline) {
        try {
            $ping = Invoke-NativeRequest -Executable $probeExecutable -Request @{
                type = 'ping'
                request_id = 'background-probe-ping'
                auto_start = $false
            }
            if ($ping.type -eq 'action_result' -or $ping.type -eq 'pong') {
                $ready = $true
                break
            }
        } catch {
            Start-Sleep -Milliseconds 100
        }
    }
    if (-not $ready) { throw '最小化 CurlDownloader 未能在 5 秒內建立 Native pipe。' }

    $list = Invoke-NativeRequest -Executable $probeExecutable -Request @{
        type = 'list_tasks'
        request_id = 'background-probe-list'
        auto_start = $false
    }
    $task = @($list.tasks | Where-Object { $_.task_id -eq 7001 })
    if ($list.type -ne 'task_list' -or $task.Count -ne 1 -or $task[0].status -ne 'completed') {
        throw '隱藏背景控制器未能讀取已完成任務 7001。'
    }

    $show = Invoke-NativeRequest -Executable $probeExecutable -Request @{
        type = 'show_window'
        request_id = 'background-probe-show'
        auto_start = $false
    }
    if ($show.type -ne 'action_result' -or -not [bool]$show.ok) {
        throw '隱藏背景控制器未能處理 show_window。'
    }
    Write-Output '最小化背景控制器 smoke test 通過：已讀取完成任務並成功顯示視窗。'
} finally {
    if ($null -ne $probeProcess) {
        try {
            if (-not $probeProcess.HasExited) {
                Stop-Process -Id $probeProcess.Id -Force -ErrorAction SilentlyContinue
                $probeProcess.WaitForExit(5000) | Out-Null
            }
        } finally {
            $probeProcess.Dispose()
        }
    }
    if (Test-Path -LiteralPath $temporaryDirectory) {
        Remove-Item -LiteralPath $temporaryDirectory -Recurse -Force
    }
}
