# Curl Downloader

Curl Downloader 是一個 Windows portable 下載器，使用內置或 PATH 中可用的 curl，支援多段下載、續傳、Proxy 及任務狀態管理。發行目錄只包含一份 `CurlDownloader.exe`；Firefox Native Messaging host 由同一份 EXE 以 stdio 模式執行，不會另帶 helper EXE。

## Build

使用 Rust 1.97.1 建立 MSVC 版本：

```powershell
powershell -ExecutionPolicy Bypass -File scripts\build-release.ps1
```

沒有 Visual Studio linker 時可使用 GNU fallback：

```powershell
powershell -ExecutionPolicy Bypass -File scripts\build-release-gnu.ps1
```

成功後 `dist\` 會驗證並只輸出 `CurlDownloader.exe`。建置時會把 `assets\curl-downloader.ico` 嵌入 EXE，系統匣及 EXE 使用相同 Cyberpunk 圖示。

## Use

輸入 HTTP/HTTPS URL 後建立任務，在任務設定中確認檔名、Windows 絕對下載目錄、分段數及 Proxy，再開始下載。任務卡會顯示進度、速度、ETA、狀態及錯誤；完成任務可開啟檔案或資料夾。程式最小化時會留在 Windows 系統匣，雙擊系統匣 Cyber 圖示可還原視窗。

Windows 下載只會在背景啟動隱藏的 curl 子程序，不會顯示 CMD 視窗。Proxy 密碼只存在本次工作流程的記憶體及 pipe，不會寫入 `state.json` 或 extension storage。

## Firefox extension

Firefox extension 會攔截 HTTP/HTTPS 下載，先暫停原生下載並開啟設定頁。設定頁可調整下載名稱、Windows 絕對目錄、Proxy 類型／主機／連接埠／帳號及本次密碼；「使用 Firefox」會恢復原生下載，「取消」會取消並清理原生下載。

### 程式生命週期

插件第一次需要主程式時，會啟動同一份 `CurlDownloader.exe`，以最小化及系統匣常駐方式運行；整個系統只維持一個 Curl Downloader 主程序。

- 最小化或按主視窗的一般關閉按鈕：只會隱藏到系統匣，下載及 Native Messaging 背景控制器繼續運作。
- 雙擊系統匣圖示：還原並把現有 GUI 帶到最前面，不會建立第二份 EXE。
- 系統匣右鍵選擇「關閉」：先保存下載進度及續傳資料，再終止主程序。
- popup、Badge 及狀態查詢屬於被動操作，不會因手動關閉而自動重啟 EXE。
- 新下載、重試 Native host、選擇目錄、開啟檔案／資料夾或顯示任務，屬於明確操作，可以重新啟動同一份 EXE。
- 手動再次雙擊 EXE：只會顯示已存在的程序，不會多開。

正常使用流程（包括 Portable Firefox ESR）：

1. 首次使用前直接啟動 `CurlDownloader.exe` 一次。GUI 啟動時會自動在 `HKCU\Software\Mozilla\NativeMessagingHosts\curl_downloader` 建立或更新 manifest，`path` 直接指向目前的 `CurlDownloader.exe`；不需要手動開啟 regedit。
2. 在 Firefox `about:addons` 使用「從檔案安裝附加元件」載入 `curl-downloader.xpi`，並按需要允許私人視窗。
3. 若設定頁顯示 Native host 未啟動或尚未註冊，啟動 `CurlDownloader.exe` 後按「重試 Curl Downloader」。

Native Messaging 會在需要時建立一條可重用的 `connectNative` 長連線；有下載或 popup 開啟時快速同步，閒置後自動釋放連線。若 GUI 被系統匣右鍵手動關閉，插件會收到「已手動關閉」狀態，不會反覆重啟；下一次明確下載或重試操作才會重新啟動同一份 EXE。

插件圖示會顯示 Cyber 進度環及 Badge，例如 `68%/3` 代表三個進行中任務的加權整體進度；總大小未知時顯示 `—/3`。有活動任務時約每 500ms 同步；popup 開啟時約每 200ms 更新，沒有活動任務且 popup 關閉時停止背景同步。

點擊工具列圖示可查看所有進行中任務及最近完成的 10 筆。任務卡支援「開啟檔案」、「開啟資料夾」及點擊任務跳轉到 Curl Downloader 詳細畫面。

Portable Firefox 的 Native host 仍由 Firefox 的 per-user HKCU 發現機制管理，不需要把 manifest 複製到 Portable Firefox 目錄。若 Firefox 先顯示原生儲存位置對話框，請在同一個 Portable Firefox profile 的下載設定中關閉「總是詢問儲存檔案的位置」，或確認 `browser.download.useDownloadDir` 為 `true`。

如需手動修復註冊：

```powershell
powershell -ExecutionPolicy Bypass -File scripts\install-firefox-native-host.ps1 `
  -ExecutablePath "$PWD\dist\CurlDownloader.exe"
```

卸載本工具建立的 per-user registry key 及 manifest：

```powershell
powershell -ExecutionPolicy Bypass -File scripts\install-firefox-native-host.ps1 `
  -ExecutablePath "$PWD\dist\CurlDownloader.exe" -Uninstall
```

## Portable package

先建立 `dist\CurlDownloader.exe` 及 `dist\curl-downloader.xpi`，再執行：

```powershell
powershell -ExecutionPolicy Bypass -File scripts\package-portable.ps1 `
  -OutputDirectory "$PWD\dist\CurlDownloaderPortable"
```

portable 目錄只包含 `CurlDownloader.exe`、`curl-downloader.xpi` 及 `portable.flag`，不會附帶第二個 Native host EXE、註冊腳本或啟動腳本；狀態會保存到 portable 目錄內的 `data\state.json`。雙擊同一份 EXE 即可啟動 GUI／系統匣，GUI 會自動在 HKCU 建立 Native Messaging manifest。Firefox Native Messaging 的 per-user HKCU 發現機制是平台要求；extension 本身不會直接寫 Registry。

## Data locations

- 一般模式：`%APPDATA%\CurlDownloader\state.json`
- Portable 模式：portable 目錄內的 `data\state.json`
- 下載部分檔：目的地內的 `.curl-downloader` 目錄

## Security software warning

未簽署的新 Windows EXE 可能被企業防毒軟件以「新發現程式」攔截；如需消除提示，請由管理員加入白名單或使用有效程式碼簽署憑證。程式不要求系統管理員權限。