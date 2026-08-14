[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)][string]$ExecutablePath,
    [switch]$SkipNativeMessaging
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

if (-not ('CurlDownloaderSmokeWin32' -as [type])) {
    Add-Type -TypeDefinition @"
using System;
using System.Runtime.InteropServices;
using System.Threading;

public static class CurlDownloaderSmokeWin32
{
    private delegate bool EnumWindowsProc(IntPtr hwnd, IntPtr lParam);

    [DllImport("user32.dll", CharSet = CharSet.Unicode, EntryPoint = "FindWindowW", SetLastError = true)]
    public static extern IntPtr FindWindow(string className, string windowName);

    [DllImport("user32.dll", EntryPoint = "SendMessageW")]
    public static extern IntPtr SendMessage(IntPtr hwnd, uint message, IntPtr wParam, IntPtr lParam);

    [DllImport("user32.dll", EntryPoint = "IsWindowVisible")]
    [return: MarshalAs(UnmanagedType.Bool)]
    public static extern bool IsWindowVisible(IntPtr hwnd);

    [DllImport("user32.dll", EntryPoint = "GetForegroundWindow")]
    public static extern IntPtr GetForegroundWindow();

    [DllImport("user32.dll", EntryPoint = "GetWindowThreadProcessId")]
    public static extern uint GetWindowThreadProcessId(IntPtr hwnd, out uint processId);

    [DllImport("user32.dll", EntryPoint = "GetWindow")]
    private static extern IntPtr GetWindow(IntPtr hwnd, uint command);

    [DllImport("user32.dll", CharSet = CharSet.Unicode, EntryPoint = "GetWindowTextW")]
    private static extern int GetWindowText(IntPtr hwnd, char[] buffer, int length);

    [DllImport("user32.dll", CharSet = CharSet.Unicode, EntryPoint = "GetClassNameW")]
    private static extern int GetClassName(IntPtr hwnd, char[] buffer, int length);

    [DllImport("user32.dll", EntryPoint = "EnumWindows")]
    private static extern bool EnumWindows(EnumWindowsProc callback, IntPtr lParam);

    public static IntPtr FindMainWindow(uint targetProcessId)
    {
        IntPtr fallback = IntPtr.Zero;
        EnumWindows((hwnd, _) =>
        {
            uint processId;
            GetWindowThreadProcessId(hwnd, out processId);
            if (processId != targetProcessId || GetWindow(hwnd, 4) != IntPtr.Zero) return true;

            var titleBuffer = new char[256];
            var titleLength = GetWindowText(hwnd, titleBuffer, titleBuffer.Length);
            var title = new string(titleBuffer, 0, Math.Max(titleLength, 0));
            var classBuffer = new char[256];
            var classLength = GetClassName(hwnd, classBuffer, classBuffer.Length);
            var className = new string(classBuffer, 0, Math.Max(classLength, 0));
            if (className == "CurlDownloaderTrayWindow" ||
                className == "NVOpenGLPbuffer" ||
                className == "Winit Thread Event Target")
            {
                return true;
            }
            if (title == "Curl Downloader")
            {
                fallback = hwnd;
                return false;
            }
            if (fallback == IntPtr.Zero) fallback = hwnd;
            return true;
        }, IntPtr.Zero);
        return fallback;
    }
}

public static class CurlDownloaderSmokeFakeShell
{
    [UnmanagedFunctionPointer(CallingConvention.Winapi)]
    private delegate IntPtr WindowProc(IntPtr hwnd, uint message, IntPtr wParam, IntPtr lParam);

    [StructLayout(LayoutKind.Sequential, CharSet = CharSet.Unicode)]
    private struct WindowClass
    {
        public uint style;
        public WindowProc lpfnWndProc;
        public int cbClsExtra;
        public int cbWndExtra;
        public IntPtr hInstance;
        public IntPtr hIcon;
        public IntPtr hCursor;
        public IntPtr hbrBackground;
        [MarshalAs(UnmanagedType.LPWStr)] public string lpszMenuName;
        [MarshalAs(UnmanagedType.LPWStr)] public string lpszClassName;
    }

    [StructLayout(LayoutKind.Sequential)]
    private struct Message
    {
        public IntPtr hwnd;
        public uint message;
        public IntPtr wParam;
        public IntPtr lParam;
        public uint time;
        public int ptX;
        public int ptY;
    }

    private const uint WM_COPYDATA = 0x004A;
    private const uint WM_QUIT = 0x0012;
    private static readonly WindowProc Callback = WindowProcImpl;
    private static Thread thread;
    private static uint threadId;
    private static IntPtr window;
    private static ManualResetEventSlim ready;

    [DllImport("kernel32.dll", EntryPoint = "GetModuleHandleW")]
    private static extern IntPtr GetModuleHandle(IntPtr moduleName);

    [DllImport("kernel32.dll", EntryPoint = "GetCurrentThreadId")]
    private static extern uint GetCurrentThreadId();

    [DllImport("user32.dll", CharSet = CharSet.Unicode, EntryPoint = "FindWindowW")]
    private static extern IntPtr FindWindow(string className, string windowName);

    [DllImport("user32.dll", CharSet = CharSet.Unicode, EntryPoint = "RegisterClassW")]
    private static extern ushort RegisterClass(ref WindowClass windowClass);

    [DllImport("user32.dll", CharSet = CharSet.Unicode, EntryPoint = "CreateWindowExW")]
    private static extern IntPtr CreateWindowEx(
        uint exStyle,
        string className,
        string title,
        uint style,
        int x,
        int y,
        int width,
        int height,
        IntPtr parent,
        IntPtr menu,
        IntPtr instance,
        IntPtr param);

    [DllImport("user32.dll", EntryPoint = "GetMessageW")]
    private static extern int GetMessage(ref Message message, IntPtr hwnd, uint minFilter, uint maxFilter);

    [DllImport("user32.dll", EntryPoint = "TranslateMessage")]
    private static extern bool TranslateMessage(ref Message message);

    [DllImport("user32.dll", EntryPoint = "DispatchMessageW")]
    private static extern IntPtr DispatchMessage(ref Message message);

    [DllImport("user32.dll", EntryPoint = "PostThreadMessageW")]
    private static extern bool PostThreadMessage(uint threadId, uint message, IntPtr wParam, IntPtr lParam);

    [DllImport("user32.dll", EntryPoint = "DestroyWindow")]
    private static extern bool DestroyWindow(IntPtr hwnd);

    [DllImport("user32.dll", EntryPoint = "DefWindowProcW")]
    private static extern IntPtr DefWindowProc(IntPtr hwnd, uint message, IntPtr wParam, IntPtr lParam);

    private static IntPtr WindowProcImpl(IntPtr hwnd, uint message, IntPtr wParam, IntPtr lParam)
    {
        if (message == WM_COPYDATA) return new IntPtr(1);
        return DefWindowProc(hwnd, message, wParam, lParam);
    }

    private static void Run()
    {
        threadId = GetCurrentThreadId();
        var windowClass = new WindowClass
        {
            lpfnWndProc = Callback,
            hInstance = GetModuleHandle(IntPtr.Zero),
            lpszClassName = "Shell_TrayWnd"
        };
        RegisterClass(ref windowClass);
        window = CreateWindowEx(
            0x80,
            "Shell_TrayWnd",
            "CurlDownloader smoke fake taskbar",
            0,
            0,
            0,
            1,
            1,
            IntPtr.Zero,
            IntPtr.Zero,
            windowClass.hInstance,
            IntPtr.Zero);
        ready.Set();

        var message = new Message();
        while (GetMessage(ref message, IntPtr.Zero, 0, 0) > 0)
        {
            TranslateMessage(ref message);
            DispatchMessage(ref message);
        }

        if (window != IntPtr.Zero) DestroyWindow(window);
    }

    public static bool StartIfUnavailable()
    {
        if (FindWindow("Shell_TrayWnd", null) != IntPtr.Zero) return false;
        ready = new ManualResetEventSlim(false);
        thread = new Thread(Run) { IsBackground = true };
        thread.Start();
        if (!ready.Wait(3000)) throw new InvalidOperationException("fake taskbar 啟動逾時");
        return true;
    }

    public static void Stop()
    {
        if (thread == null) return;
        PostThreadMessage(threadId, WM_QUIT, IntPtr.Zero, IntPtr.Zero);
        thread.Join(3000);
        thread = null;
        ready?.Dispose();
        ready = null;
    }
}
"@
}

$temporaryDirectory = Join-Path ([IO.Path]::GetTempPath()) ("CurlDownloader-Background-Probe-" + [Guid]::NewGuid().ToString('N'))
$probeExecutable = Join-Path $temporaryDirectory 'CurlDownloader.exe'
$renamedExecutable = Join-Path $temporaryDirectory 'CurlDownloader.probe.exe'
$probeProcess = $null
$nativeSession = $null
$oldTestEnvironment = [Environment]::GetEnvironmentVariable('CURL_DOWNLOADER_TEST_SHUTDOWN_MANUAL', 'Process')
$oldNativeClientEnvironment = [Environment]::GetEnvironmentVariable('CURL_DOWNLOADER_TEST_NATIVE_CLIENT', 'Process')
$fakeShellStarted = $false

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

function Start-NativeSession {
    param([Parameter(Mandatory = $true)][string]$Executable)

    $start = [Diagnostics.ProcessStartInfo]::new($Executable)
    $start.Arguments = '--native-messaging-host'
    $start.UseShellExecute = $false
    $start.RedirectStandardInput = $true
    $start.RedirectStandardOutput = $true
    $start.RedirectStandardError = $true
    $start.CreateNoWindow = $true
    $process = [Diagnostics.Process]::Start($start)
    if ($null -eq $process) { throw '無法啟動 persistent Native host。' }
    return $process
}

function Send-NativeRequest {
    param(
        [Parameter(Mandatory = $true)][Diagnostics.Process]$Session,
        [Parameter(Mandatory = $true)][hashtable]$Request
    )

    if ($Session.HasExited) { throw "Native host 已退出：$($Session.ExitCode)" }
    $json = $Request | ConvertTo-Json -Compress
    $body = [Text.Encoding]::UTF8.GetBytes($json)
    $frame = [byte[]]::new(4 + $body.Length)
    [BitConverter]::GetBytes([uint32]$body.Length).CopyTo($frame, 0)
    $body.CopyTo($frame, 4)
    $Session.StandardInput.BaseStream.Write($frame, 0, $frame.Length)
    $Session.StandardInput.BaseStream.Flush()

    $lengthBytes = [byte[]]::new(4)
    Read-Exact -Stream $Session.StandardOutput.BaseStream -Buffer $lengthBytes
    $length = [BitConverter]::ToUInt32($lengthBytes, 0)
    if ($length -gt 4MB) { throw 'Native host 回覆超過測試上限。' }
    $responseBytes = [byte[]]::new($length)
    Read-Exact -Stream $Session.StandardOutput.BaseStream -Buffer $responseBytes
    return [Text.Encoding]::UTF8.GetString($responseBytes) | ConvertFrom-Json
}

function Get-ProcessWindow {
    param([Parameter(Mandatory = $true)][uint32]$ProcessId)

    try {
        return [CurlDownloaderSmokeWin32]::FindMainWindow($ProcessId)
    } catch {
        return [IntPtr]::Zero
    }
}

function Wait-For {
    param(
        [Parameter(Mandatory = $true)][scriptblock]$Condition,
        [int]$TimeoutMilliseconds = 5000,
        [string]$FailureMessage = '等待條件逾時。'
    )

    $deadline = [DateTime]::UtcNow.AddMilliseconds($TimeoutMilliseconds)
    while ([DateTime]::UtcNow -lt $deadline) {
        if (& $Condition) { return }
        Start-Sleep -Milliseconds 50
    }
    throw $FailureMessage
}

try {
    $fakeShellStarted = [CurlDownloaderSmokeFakeShell]::StartIfUnavailable()
    if ($fakeShellStarted) { Write-Warning '測試桌面沒有實際 Shell_TrayWnd，已建立短暫測試用 Shell_TrayWnd 讓系統匣回呼可被驗證。' }
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

    $env:CURL_DOWNLOADER_TEST_SHUTDOWN_MANUAL = '1'
    $env:CURL_DOWNLOADER_TEST_NATIVE_CLIENT = '1'
    $probeProcess = Start-Process -FilePath $probeExecutable -ArgumentList @('--minimized', '--skip-native-registration') -WorkingDirectory $temporaryDirectory -WindowStyle Hidden -PassThru
    Wait-For -FailureMessage '最小化 CurlDownloader 未能在 5 秒內啟動。' -Condition {
        -not $probeProcess.HasExited
    }

    if ($SkipNativeMessaging) {
        Wait-For -FailureMessage '最小化啟動後 GUI 沒有隱藏。' -Condition {
            $main = Get-ProcessWindow -ProcessId $probeProcess.Id
            $main -ne [IntPtr]::Zero -and -not [CurlDownloaderSmokeWin32]::IsWindowVisible($main)
        }
        Write-Output '最小化背景控制器 smoke test 通過：release EXE 已啟動並維持隱藏。Native Messaging 驗證由受控 smoke probe 執行。'
        return
    }

    $nativeSession = Start-NativeSession -Executable $probeExecutable
    $ping = $null
    $pipeDeadline = [DateTime]::UtcNow.AddSeconds(5)
    while ([DateTime]::UtcNow -lt $pipeDeadline) {
        try {
            $ping = Send-NativeRequest -Session $nativeSession -Request @{
                type = 'ping'
                request_id = 'background-probe-ping'
                auto_start = $false
            }
            if ($ping.type -eq 'pong' -and [bool]$ping.ok) { break }
        } catch {
            if ($nativeSession.HasExited) { throw }
            Start-Sleep -Milliseconds 100
        }
    }
    if ($null -eq $ping -or $ping.type -ne 'pong' -or -not [bool]$ping.ok) {
        throw 'persistent Native host ping 失敗。'
    }
    $nativePid = $nativeSession.Id
    $list = Send-NativeRequest -Session $nativeSession -Request @{
        type = 'list_tasks'
        request_id = 'background-probe-list'
        auto_start = $false
    }
    if ($nativeSession.Id -ne $nativePid -or $list.type -ne 'task_list') {
        throw 'ping 與 list_tasks 沒有使用同一 Native host session。'
    }
    $task = @($list.tasks | Where-Object { $_.task_id -eq 7001 })
    if ($task.Count -ne 1 -or $task[0].status -ne 'completed') {
        throw '隱藏背景控制器未能讀取已完成任務 7001。'
    }

    $tray = [CurlDownloaderSmokeWin32]::FindWindow('CurlDownloaderTrayWindow', $null)
    $trayAvailable = $tray -ne [IntPtr]::Zero
    if (-not $trayAvailable) {
        Write-Warning '目前測試桌面沒有 Shell_TrayWnd，略過只能在互動式 Windows 桌面驗證的系統匣雙擊；其餘 Native/GUI/關閉測試仍會執行。'
    } else {
        [uint32]$trayProcessId = 0
        [CurlDownloaderSmokeWin32]::GetWindowThreadProcessId($tray, [ref]$trayProcessId) | Out-Null
        if ($trayProcessId -ne $probeProcess.Id) { throw '系統匣 window 不屬於 probe EXE。' }

        Wait-For -FailureMessage '最小化啟動後 GUI 沒有隱藏。' -Condition {
            $main = Get-ProcessWindow -ProcessId $probeProcess.Id
            $main -ne [IntPtr]::Zero -and -not [CurlDownloaderSmokeWin32]::IsWindowVisible($main)
        }

        [CurlDownloaderSmokeWin32]::SendMessage($tray, 0x8001, [IntPtr]::new(1), [IntPtr]::new(0x0203)) | Out-Null
        Wait-For -FailureMessage '真實系統匣雙擊沒有還原 GUI。' -Condition {
            $main = Get-ProcessWindow -ProcessId $probeProcess.Id
            $main -ne [IntPtr]::Zero -and [CurlDownloaderSmokeWin32]::IsWindowVisible($main)
        }
    }

    $show = Send-NativeRequest -Session $nativeSession -Request @{
        type = 'show_window'
        request_id = 'background-probe-show'
        auto_start = $false
    }
    if ($show.type -ne 'action_result' -or -not [bool]$show.ok) {
        throw 'persistent Native host show_window 失敗。'
    }
    Wait-For -FailureMessage 'show_window 沒有把同一 GUI 顯示出來。' -Condition {
        $main = Get-ProcessWindow -ProcessId $probeProcess.Id
        $main -ne [IntPtr]::Zero -and [CurlDownloaderSmokeWin32]::IsWindowVisible($main)
    }
    if ($trayAvailable -and -not $fakeShellStarted) {
        Wait-For -FailureMessage 'show_window 沒有把同一 GUI 帶到最前面。' -Condition {
            $main = Get-ProcessWindow -ProcessId $probeProcess.Id
            $main -ne [IntPtr]::Zero -and [CurlDownloaderSmokeWin32]::GetForegroundWindow() -eq $main
        }
    }

    $shutdown = Send-NativeRequest -Session $nativeSession -Request @{
        type = 'shutdown_manual'
        request_id = 'background-probe-shutdown'
        auto_start = $false
    }
    if ($shutdown.type -ne 'action_result' -or -not [bool]$shutdown.ok) {
        throw '測試專用 ShutdownManual 沒有走通 controller 路徑。'
    }

    if (-not $probeProcess.WaitForExit(5000)) {
        throw 'GUI 沒有在手動關閉後 5 秒內退出。'
    }
    if (-not $nativeSession.WaitForExit(5000)) {
        throw 'Native host 沒有在手動關閉後 5 秒內退出。'
    }

    $probeProcess.Dispose()
    $probeProcess = $null
    $nativeSession.Dispose()
    $nativeSession = $null

    $nativeSession = Start-NativeSession -Executable $probeExecutable
    $passive = Send-NativeRequest -Session $nativeSession -Request @{
        type = 'list_tasks'
        request_id = 'background-probe-passive-after-manual-stop'
        auto_start = $true
    }
    if ($passive.type -ne 'error' -or $passive.error.code -ne 'manually_stopped') {
        throw '手動關閉後的被動 list_tasks 不應重新啟動 GUI。'
    }
    $passiveGuiProcesses = @(Get-Process -Name 'CurlDownloader' -ErrorAction SilentlyContinue | Where-Object {
        $_.Id -ne $nativeSession.Id -and $_.Path -eq $probeExecutable
    })
    if ($passiveGuiProcesses.Count -ne 0) {
        throw '手動關閉後的被動 list_tasks 意外啟動了 GUI。'
    }

    $explicit = Send-NativeRequest -Session $nativeSession -Request @{
        type = 'list_tasks'
        request_id = 'background-probe-explicit-restart'
        auto_start = $true
        start_intent_unix_ms = [DateTimeOffset]::UtcNow.ToUnixTimeMilliseconds()
    }
    if ($explicit.type -ne 'task_list') {
        throw '新的明確 start intent 沒有重新啟動 GUI。'
    }
    Wait-For -FailureMessage '新的明確 start intent 沒有啟動第二次最小化 GUI。' -Condition {
        @(Get-Process -Name 'CurlDownloader' -ErrorAction SilentlyContinue | Where-Object {
            $_.Id -ne $nativeSession.Id -and $_.Path -eq $probeExecutable
        }).Count -eq 1
    }
    $probeCandidates = @(Get-Process -Name 'CurlDownloader' -ErrorAction SilentlyContinue | Where-Object {
        $_.Id -ne $nativeSession.Id -and $_.Path -eq $probeExecutable
    })
    if ($probeCandidates.Count -ne 1) {
        throw '找不到新的最小化 GUI 程序。'
    }
    $probeProcess = $probeCandidates[0]
    Wait-For -FailureMessage '明確重啟後 GUI 沒有以最小化模式啟動。' -Condition {
        $main = Get-ProcessWindow -ProcessId $probeProcess.Id
        $main -ne [IntPtr]::Zero -and -not [CurlDownloaderSmokeWin32]::IsWindowVisible($main)
    }

    $restartShutdown = Send-NativeRequest -Session $nativeSession -Request @{
        type = 'shutdown_manual'
        request_id = 'background-probe-restart-shutdown'
        auto_start = $false
    }
    if ($restartShutdown.type -ne 'action_result' -or -not [bool]$restartShutdown.ok) {
        throw '重新啟動後的測試專用 ShutdownManual 失敗。'
    }
    if (-not $probeProcess.WaitForExit(5000)) {
        throw '重新啟動的 GUI 沒有在手動關閉後 5 秒內退出。'
    }
    if (-not $nativeSession.WaitForExit(5000)) {
        throw '重新啟動後的 Native host 沒有在手動關閉後 5 秒內退出。'
    }
    Move-Item -LiteralPath $probeExecutable -Destination $renamedExecutable
    Move-Item -LiteralPath $renamedExecutable -Destination $probeExecutable
    $trayMessage = if ($fakeShellStarted) { '測試用 Shell_TrayWnd 回呼' } elseif ($trayAvailable) { '真實系統匣雙擊' } else { '系統匣測試略過（目前桌面沒有 Shell_TrayWnd）' }
    Write-Output "最小化背景控制器 smoke test 通過：同一 portable EXE 已完成 ping/list_tasks、$trayMessage、show_window、ShutdownManual、被動停止查詢、明確重啟及檔案解鎖驗證。"
} finally {
    if ($null -ne $nativeSession) {
        try {
            if (-not $nativeSession.HasExited) {
                $nativeSession.StandardInput.Close()
                if (-not $nativeSession.WaitForExit(1000)) {
                    Stop-Process -Id $nativeSession.Id -Force -ErrorAction SilentlyContinue
                }
            }
        } catch {}
        try { $nativeSession.Dispose() } catch {}
    }
    if ($null -ne $probeProcess) {
        try {
            if (-not $probeProcess.HasExited) {
                Stop-Process -Id $probeProcess.Id -Force -ErrorAction SilentlyContinue
                $probeProcess.WaitForExit(5000) | Out-Null
            }
        } catch {}
        try { $probeProcess.Dispose() } catch {}
    }
    if ($fakeShellStarted) { [CurlDownloaderSmokeFakeShell]::Stop() }
    if ($null -eq $oldTestEnvironment) {
        Remove-Item Env:CURL_DOWNLOADER_TEST_SHUTDOWN_MANUAL -ErrorAction SilentlyContinue
    } else {
        $env:CURL_DOWNLOADER_TEST_SHUTDOWN_MANUAL = $oldTestEnvironment
    }
    if ($null -eq $oldNativeClientEnvironment) {
        Remove-Item Env:CURL_DOWNLOADER_TEST_NATIVE_CLIENT -ErrorAction SilentlyContinue
    } else {
        $env:CURL_DOWNLOADER_TEST_NATIVE_CLIENT = $oldNativeClientEnvironment
    }
    if (Test-Path -LiteralPath $temporaryDirectory) {
        $resolvedTemporaryDirectory = [IO.Path]::GetFullPath($temporaryDirectory)
        $tempRoot = [IO.Path]::GetFullPath([IO.Path]::GetTempPath())
        if (-not $resolvedTemporaryDirectory.StartsWith($tempRoot, [StringComparison]::OrdinalIgnoreCase)) {
            throw "拒絕清理不在暫存根目錄內的 smoke test 目錄：$resolvedTemporaryDirectory"
        }
        Remove-Item -LiteralPath $resolvedTemporaryDirectory -Recurse -Force
    }
}
