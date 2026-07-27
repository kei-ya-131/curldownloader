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

發佈檔 `CurlDownloader.exe` 是直接啟動的原生 Windows GUI EXE，使用 `asInvoker`，不要求系統管理員權限，也不包含 PowerShell／CMD 啟動器。建置腳本中的 PowerShell 只在建置階段使用；請在檔案總管直接雙擊 `dist\CurlDownloader.exe`，不要以 `powershell -Command` 或其他 shell 作為啟動器。下載工作只會在按下開始後啟動 curl 子程序。

未簽署的新 Windows EXE 仍可能被企業端防毒軟件以「新發現程式」攔截；程式不能安全地停用或繞過 Trend Micro 的信譽判定。若要完全消除這類提示，需要由管理員加入白名單，或使用具有效程式碼簽署憑證的發佈檔。

## Use

在上方輸入 HTTP/HTTPS URL，按「新增下載」後任務只會先排隊，不會立即連線。請在任務設定確認檔名、下載目錄、分段數及是否使用 Proxy，再按「開始」；下載開始後這些設定會鎖定。按「批量新增」可貼上多行網址，一行一個，建立多個待確認任務。佇列以狀態圖樣及卡片分組顯示，並以進度、速度及剩餘時間作為主要資訊；勾選多個可編輯任務後，可按「批量設定 Proxy」一次套用相同 Proxy，完成後工具列會顯示實際套用及略過數量。下載中及已完成的任務不可批量修改。下載中的任務可以「暫停」，重啟程式或再次按「開始」會從工作目錄中的部分檔續傳。完成後可從佇列按「開啟位置」。

介面會跟隨 Windows／系統的明暗主題，卡片、文字及選取色會按當前主題調整；狀態圖樣及語意色亦會保留，方便在淺色或深色模式辨識狀態。

Windows 下載時只會在背景啟動隱藏的 curl 子程序，不會顯示 CMD 控制台視窗。

## Firefox extension

Firefox extension 會攔截 HTTP/HTTPS 下載，先暫停原下載並開啟設定頁。設定頁可填寫下載名稱、Windows 絕對目錄，以及 HTTP、HTTPS、SOCKS5 或 SOCKS5H Proxy 的主機、連接埠、帳號及密碼；按「交給 Curl Downloader」後，只有當 Native host 回報任務成功，才會取消並清理原 Firefox 下載。

建立發行版後，以目前使用者權限註冊 Native host：

```powershell
powershell -ExecutionPolicy Bypass -File scripts\install-firefox-native-host.ps1 `
  -ExecutablePath "$PWD\dist\CurlDownloader.exe"
```

Portable Firefox ESR 注意：Native host 仍是註冊到目前 Windows 使用者的
`HKCU\Software\Mozilla\NativeMessagingHosts\curl_downloader`，不需要把 manifest
複製到 Portable Firefox 目錄。若點擊下載時先出現 Firefox 原生「選擇下載檔案位置」
對話框，請在同一個 Portable Firefox profile 開啟 `about:preferences` → 一般 →
檔案與應用程式 → 下載，關閉「總是詢問儲存檔案的位置」；亦可在 `about:config`
將 `browser.download.useDownloadDir` 設為 `true`，然後重新啟動 Portable Firefox。
Firefox 的原生對話框會在 `downloads.onCreated` 之前出現，WebExtension 無法在該
對話框已開啟後取消它。

打包 extension：

```powershell
powershell -ExecutionPolicy Bypass -File scripts\package-firefox-extension.ps1 `
  -OutputPath "$PWD\dist\curl-downloader.xpi"
```

Firefox extension 本身不能寫入 Windows Registry 或任意啟動未註冊的 EXE；
`-ExecutablePath` 就是 Native host 使用的 Curl Downloader 位置。若 EXE 被移動，
請以新位置重新執行上面的 Native host 安裝指令，再重新啟動 Firefox。

在 Firefox 的 `about:addons` 使用「從檔案安裝附加元件」載入 XPI，並在附加元件設定中允許「在私人視窗中執行」。首次由 extension 取得預設目錄或提交下載時，Native host 會以目前的 EXE 路徑自動啟動 Curl Downloader；GUI 已開啟時不會建立第二個視窗。

若 Native host、GUI 或設定頁提交失敗，extension 會恢復原 Firefox 下載；若 Firefox 不接受 resume，會以一次性管理標記重新建立 fallback 下載，避免 fallback 再次進入攔截流程。若關閉設定頁而未提交，也會恢復 Firefox 下載。

extension storage 只保存目錄及非秘密 Proxy 預設值。Proxy 密碼只存在設定頁記憶體、Native Messaging pipe 及本次 curl 工作流，不會寫入 extension storage、`state.json` 或診斷輸出。私人視窗下載的任務資料會按既定流程交給 GUI，引擎狀態仍使用 `%APPDATA%\CurlDownloader\state.json`。

卸載 Native host（只移除本工具建立的 per-user registry key 及 manifest）：

```powershell
powershell -ExecutionPolicy Bypass -File scripts\install-firefox-native-host.ps1 `
  -ExecutablePath "$PWD\dist\CurlDownloader.exe" -Uninstall
```

程式啟動下載時會先從 PATH 尋找本機 `curl.exe`，並以 `curl.exe --version` 確認可以執行；找不到或無法啟動時，才會使用內嵌且經 SHA-256 驗證的 curl。程式只在第一次真正開始下載時選擇及驗證 curl；啟動程式、瀏覽歷史或新增待確認任務都不會建立內置 curl runtime。任務面板會顯示「尚未啟動」、「本機 curl」或「內置 curl」。若 curl 啟動失敗，可在任務的「詳細診斷」查看已清理 Proxy 憑證的作業系統／curl 錯誤。

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
