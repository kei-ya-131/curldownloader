# Curl Downloader

## Requirements

Windows 10/11 x64。發行檔內嵌並驗證 curl，不需要另外安裝 curl 或 VC Runtime。

## Build

使用 Rust 1.97.1、MSVC target 建立單一執行檔：

```powershell
powershell -ExecutionPolicy Bypass -File scripts/build-release.ps1
```

成功後 `dist/` 只會有 `CurlDownloader.exe`。

## Use

在上方輸入 HTTP/HTTPS URL，按「新增下載」，於任務設定修改檔名、下載目錄與分段數，再按「開始」。下載中的任務可以「暫停」，重啟程式或再次按「開始」會從工作目錄中的部分檔續傳。完成後可從佇列按「開啟位置」。

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
