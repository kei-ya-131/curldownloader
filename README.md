# Curl Downloader

**English** | [繁體中文](README.zh-Hant.md)

Curl Downloader is a portable download manager for Windows. It uses either the bundled curl or a usable curl found on `PATH`, with support for segmented downloads, resuming, per-task proxy settings, and task status management. The release directory contains only one `CurlDownloader.exe`; the same executable runs as the Firefox Native Messaging host in stdio mode, with no separate helper executable.

## Version 0.4.0

Changes from 0.3.0:

- Firefox can hand off supported ChatGPT Blob downloads with the exact captured HTTP request and authorization, while unsupported Blobs retain Firefox fallback.
- Cookie lookup respects the download URL and Firefox container. Native handoff errors remain visible, and repeated downloads and fallback recovery keep their task state.
- Segment merging and Explorer operations run in the background. Resuming validates the requested byte range, and changes to the source or segment count discard incompatible partial files.
- Protected URL query credentials remain encrypted across restart and are cleared after completion or cancellation. Task details preserve segment timing and support copying complete paths and URLs.
- Creating a task in Firefox preserves the main window's hidden state. The folder picker belongs to the initiating foreground window, so it opens above Firefox without restoring Curl Downloader.
- Explorer location actions select the task's file and preserve the existing window layout. Startup prefers a compatible integrated GPU and preserves unreadable state files.

### Upgrade from 0.3.0

Close Curl Downloader from its tray menu, replace `CurlDownloader.exe` in the same directory, and update `curl-downloader.xpi` to 0.4.0. Keep the existing `data` directory, state file, and partial downloads. Launch the updated executable once to refresh Native Messaging registration, then reload ChatGPT tabs to activate the new Blob interceptor. Existing task records remain readable; timing absent from older records is shown as **Not recorded**.

## Build

Build the MSVC version with Rust 1.97.1:

```powershell
powershell -ExecutionPolicy Bypass -File scripts\build-release.ps1
```

If the Visual Studio linker is unavailable, use the GNU fallback:

```powershell
powershell -ExecutionPolicy Bypass -File scripts\build-release-gnu.ps1
```

After a successful build, `dist\` is validated and contains only `CurlDownloader.exe`. The build embeds `assets\curl-downloader.ico` into the executable, so the system tray and executable use the same Cyberpunk icon.

## Use

Enter an HTTP/HTTPS URL to create a task. Before starting the download, confirm the file name, absolute Windows download directory, segment count, and proxy settings. Task cards show progress, speed, ETA, status, and errors; completed tasks can open the downloaded file or its folder. When minimized, the application remains in the Windows system tray. Double-click the Cyber tray icon to restore the window.

On Windows, downloads start only hidden curl child processes in the background and do not display CMD windows.

Segment merging and folder opening run in the background so other tasks remain usable. Changing the source, destination, or segment count discards incompatible partial files before restarting; partial data is never reused for a different byte range. The selected task is revealed once when it moves into the completed section.

On Windows, DirectX 12 explicitly prefers a compatible integrated GPU; other devices are used when no compatible integrated GPU is available. An unreadable state file prevents startup and displays an error, preserving the existing task history.

Protected URL query credentials are retained only in encrypted authorization data. Task history stores URLs without query parameters, restores the complete URL from encrypted data on restart, and clears authorization data after completion or cancellation.

### Task details and segment history

Task details contain only two actual tabs: **Task overview** and **Segment settings**. URLs, file names, and complete save paths wrap automatically and can be selected and copied directly instead of being truncated with ellipses. Completed tasks still show each segment's byte range, size, downloaded bytes, status, start and completion times, active download duration, and average speed. This information remains available after restarting the application. Timing data missing from records created by older versions is shown as **Not recorded** and is never estimated.

When **Open file** or **Open folder** is selected from the extension or the main application, Curl Downloader first reuses an existing Explorer location, then opens the target through Windows Shell and attempts to bring the actual target window to the foreground. If Windows refuses the foreground switch, the target flashes for attention and the extension receives the stable `target_not_foreground` error. Curl Downloader's own GUI is not restored as a side effect.

All these features run inside the same `CurlDownloader.exe`. They do not use PowerShell, CMD, helper executables, process injection, or remote threads. Proxy passwords exist only in memory and pipes for the current workflow; they are never written to `state.json` or extension storage.

Open location opens the containing folder and selects that task's file, even if the folder is already open. If the file is unfinished or has been removed, it opens the folder alone. Opening a location preserves the existing Explorer window size, position, and internal keyboard focus. Only minimized windows are restored; a previously maximized window returns maximized. Activation uses the normal foreground request without joining Explorer's input queue or forcing focus onto its window frame.

## Firefox extension

The Firefox extension intercepts HTTP/HTTPS downloads, cancels and erases the native Firefox item to keep its download panel out of the way, and opens a configuration page. The page can change the download name, absolute Windows directory, proxy type, host, port, account, and password for the current request. **Use Firefox** recreates the native download, while **Cancel** cancels the intercepted request.

ChatGPT `blob:` downloads are intercepted in the page before Firefox starts saving. When a successful `Response.blob()` can be linked to its exact HTTP GET request in the same tab and frame, Curl Downloader receives that URL and captured authorization. Locally generated blobs or unconfirmed requests still show the choice page, with Curl submission disabled and **Use Firefox** available. Keep the original ChatGPT tab open until deciding. After updating the XPI, reload the ChatGPT page so the new page interceptor runs.

Run `python scripts/test-firefox-blob-download.py` for an isolated headless Firefox smoke test of the choice page and byte-for-byte Firefox fallback. It requires Firefox and OpenSSL; its local TLS fixture and test-only Native Messaging stub do not use your browser profile or launch the desktop application.

Failed Native handoffs keep their error code visible on the settings page. Firefox fallback is reported as restored only after acknowledgement, and a restored download cannot be submitted again from that page. Close it and click download again on the source page. Run the smoke test with --reject-native to verify rejection followed by repeated downloads, or --probe-native to read the registered host status without starting the GUI or enqueuing tasks.

### Application lifecycle

Authenticated downloads reuse authorization headers actually sent by Firefox. When a Cookie header is missing, the extension queries only unpartitioned cookies for the download URL and original container. Updating the extension requires granting the added cookies permission. Partitioned cookies and first-party isolation rely on the actual captured Firefox request; expired authorization can be refreshed in Firefox. Sites requiring browser-specific verification may still need Firefox to complete the download.

Starting the GUI automatically creates or updates the Native host registration in `HKCU\Software\Mozilla\NativeMessagingHosts\curl_downloader`. If the settings page cannot connect, select **Retry Curl Downloader**.

The first time the extension needs the main application, it starts the same `CurlDownloader.exe` minimized and resident in the system tray. The entire system keeps only one Curl Downloader main process.

- Minimizing or pressing the main window's standard close button only hides it in the system tray. Downloads and the Native Messaging background controller continue running.
- Double-clicking the tray icon restores the existing GUI and brings it to the foreground without creating a second executable process.
- Selecting **Close** from the tray icon's context menu saves download progress and resume data before terminating the main process.
- Popup, badge, and status queries are passive operations and do not restart the executable after a manual shutdown.
- A new download, retrying the Native host, or choosing a directory is an explicit start operation and may restart the same executable. Opening files or folders and viewing tasks remain passive and do not override a manual shutdown.
- Double-clicking the executable manually while it is already running only displays the existing instance.

Normal setup, including Portable Firefox ESR:

1. Before first use, launch `CurlDownloader.exe` once. When the GUI starts, it automatically creates or updates the manifest under `HKCU\Software\Mozilla\NativeMessagingHosts\curl_downloader`, with `path` pointing directly to the current `CurlDownloader.exe`. You do not need to open Registry Editor manually.
2. In Firefox `about:addons`, use **Install Add-on From File** to load `curl-downloader.xpi`, and allow it in private windows if required.
3. If the configuration page reports that the Native host is not running or registered, start `CurlDownloader.exe` and select **Retry Curl Downloader**.

Native Messaging creates a reusable long-lived `connectNative` connection when needed. It synchronizes quickly while downloads are active or the popup is open, then releases the connection automatically after becoming idle. The first load of the configuration page and clicking the extension icon again start the same executable in tray mode. If the GUI is closed manually from the tray context menu, the extension receives a **manually closed** state. The currently open popup keeps the application closed; close the popup and click the extension again to restart it.

The extension icon displays a Cyber progress ring and badge. For example, `68%/3` represents the weighted overall progress of three active tasks; `—/3` is shown when their total size is unknown. Synchronization runs approximately every 500 ms while tasks are active, approximately every 200 ms while the popup is open, and stops in the background when there are no active tasks and the popup is closed.

Click the toolbar icon to view all active tasks and the 10 most recently completed tasks. Task cards support **Open file**, **Open folder**, and clicking the task to open its detail view in Curl Downloader.

Portable Firefox still discovers the Native host through Firefox's per-user HKCU mechanism. The manifest does not need to be copied into the Portable Firefox directory. If Firefox first displays its native save-location dialog, disable **Always ask you where to save files** in that Portable Firefox profile, or confirm that `browser.download.useDownloadDir` is `true`.

To repair the registration manually:

```powershell
powershell -ExecutionPolicy Bypass -File scripts\install-firefox-native-host.ps1 `
  -ExecutablePath "$PWD\dist\CurlDownloader.exe"
```

To remove the per-user Registry key and manifest created by this tool:

```powershell
powershell -ExecutionPolicy Bypass -File scripts\install-firefox-native-host.ps1 `
  -ExecutablePath "$PWD\dist\CurlDownloader.exe" -Uninstall
```

## Portable package

First build `dist\CurlDownloader.exe` and `dist\curl-downloader.xpi`, then run:

```powershell
powershell -ExecutionPolicy Bypass -File scripts\package-portable.ps1 `
  -OutputDirectory "$PWD\dist\CurlDownloaderPortable"
```

The portable directory contains only `CurlDownloader.exe`, `curl-downloader.xpi`, and `portable.flag`. It does not include a second Native host executable, registration script, or launcher script. State is stored in `data\state.json` inside the portable directory. Double-click the same executable to start the GUI and tray process; the GUI automatically creates the Native Messaging manifest in HKCU. Firefox Native Messaging's per-user HKCU discovery mechanism is a platform requirement. The extension itself never writes to the Registry directly.

## Data locations

- Standard mode: `%APPDATA%\CurlDownloader\state.json`
- Portable mode: `data\state.json` inside the portable directory
- Partial download files: the hidden `.curl-downloader` directory inside the destination. It exists only while work is pending and is removed when a task completes, is cancelled, or its history is cleared.

## Security software warning

New, unsigned Windows executables may be blocked by enterprise antivirus software as newly encountered programs. To remove this warning, ask an administrator to allowlist the application or sign it with a valid code-signing certificate. The application does not require administrator privileges.
