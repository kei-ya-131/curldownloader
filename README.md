# Curl Downloader

## Requirements

Windows 10/11 x64。發行檔內嵌並驗證 curl，不需要另外安裝 curl 或 VC Runtime。

## Build

使用 Rust 1.97.1、MSVC target 建立單一執行檔：

```powershell
powershell -ExecutionPolicy Bypass -File scripts/build-release.ps1
```

成功後 `dist/` 只會有 `CurlDownloader.exe`。

如果本機沒有管理員權限或沒有 Visual Studio linker，可使用已安裝的 Rust GNU toolchain 建立 Windows x64 fallback release：

```powershell
powershell -ExecutionPolicy Bypass -File scripts/build-release-gnu.ps1
```

此 fallback 同樣只產生一個 `CurlDownloader.exe`，但正式 MSVC 發佈仍應使用上方腳本。

## Security software warning

本程式使用 `asInvoker`，不要求系統管理員權限，也不會啟動 PowerShell；下載工作只會啟動內嵌的 curl 子程序。未簽署的新 Windows EXE 可能被企業端防毒軟件以「新發現程式」攔截，請使用安全軟件的「Allow Once」或由管理員加入公司白名單。這類攔截不能由程式安全地繞過。

## Use

在上方輸入 HTTP/HTTPS URL，按「新增下載」後任務只會先排隊，不會立即連線。請在任務設定確認檔名、下載目錄、分段數及是否使用 Proxy，再按「開始」；下載開始後這些設定會鎖定。按「批量新增」可貼上多行網址，一行一個，建立多個待確認任務。下載中的任務可以「暫停」，重啟程式或再次按「開始」會從工作目錄中的部分檔續傳。完成後可從佇列按「開啟位置」。

Windows 下載時只會在背景啟動隱藏的 curl 子程序，不會顯示 CMD 控制台視窗。

## Proxy

每個任務可獨立設定 HTTP、HTTPS、SOCKS5 或 SOCKS5H Proxy。Proxy 密碼只在記憶體與 curl stdin 設定流中使用，不保存至 state.json；程式重啟後會要求重新輸入。

## Data locations

- 設定與任務狀態：`%APPDATA%\CurlDownloader\state.json`
- 下載部分檔：目的地內的 `.curl-downloader` 目錄

## Limits

分段整合期間約需目標檔案兩倍的可用空間。非正常終止可能留下暫存 curl runtime 目錄；下次正常啟動或結束時會清理目前 runtime。

## Verification

發行腳本會執行 `cargo fmt --check`、MSVC target 的 `cargo clippy --all-targets -- -D warnings`、完整 `cargo test`、release build，以及內嵌 `assets/curl.exe` 的 SHA-256 驗證。curl runtime hash 為：

`8d28c1093e0b6345917d2c1710c67f78f61834d76ef983ea9fb631c75e20312f`
