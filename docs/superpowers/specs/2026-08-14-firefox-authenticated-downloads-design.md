# Firefox 受保護下載與跨重啟續傳設計

日期：2026-08-14

## 背景與根因

Firefox 原生下載會沿用瀏覽器實際發出請求時的 Cookie、Authorization、Referer、User-Agent 與其他來源背景。現有 Curl Downloader Bridge 只把 URL、檔名、目錄、分段數與 Proxy 設定交給主程式；curl 因此以不同的請求身份重新探測來源。需要登入狀態或防盜鏈背景的 HTTP/HTTPS GET 連結會回傳 401 或 403，即使 Firefox 原生下載能成功。Proxy 只改變連線路徑，不能補回網站身份資料。

## 目標

- Firefox 可嗅探且能由一般 HTTP/HTTPS GET 重播的下載，交接後應盡量保持與 Firefox 原生下載相同的存取能力。
- Cookie、Authorization 或其他敏感請求標頭不得出現在命令列、畫面、任務快照、日誌或錯誤診斷。
- 受保護任務在主程式重啟後可使用既有部分檔安全續傳。
- 公開、沒有敏感標頭的任務不建立 DPAPI 密文。
- 不新增匿名分類探測，避免消耗一次性 URL、觸發限流或令短效連結失效。
- 保持現有 Proxy、分段、暫停、檔案衝突及 Firefox 交接流程相容。

## 非目標與技術界線

- 不接管 `blob:`、`data:`、DRM 或只能在瀏覽器程序內解密的內容。
- 不重播 POST body；本次範圍限於 Firefox 可交接的 HTTP/HTTPS GET 下載。
- 已過期且網站無法重新簽發的簽署 URL 無法保證續傳。
- 伺服器顯示來源版本已改變時，不會把舊部分檔拼到新內容。
- 不讀取 Firefox profile 或 cookie database。

## 採用方案

採用「擷取 Firefox 實際請求背景，敏感背景以 Windows DPAPI CurrentUser 保存」方案。

不採用 Cookie API 重建，因為它會漏掉 Authorization、自訂 token 及部分來源標頭。不採用直接讀取 Firefox profile，因為該做法侵入性高，會受鎖檔、資料庫格式與主密碼影響。

## Firefox 擷取與配對

擴充功能增加必要的 WebRequest 與 HTTP/HTTPS host 權限，在請求送出階段觀察下載候選的 request headers。它按 WebRequest request ID 維持短期重新導向鏈，並以最終 URL、原始 URL、分頁、時間及下載建立事件做配對。

配對資料只保留有限時間及有限筆數。同 URL 同時下載時，不單憑 URL 取最舊或任意記錄；配對必須使用最近且未消耗、重新導向鏈及分頁資料相容的候選。配對失敗時仍按舊行為交接 URL，不能把另一個請求的身份資料誤配。

擴充功能傳送的是經篩選的 `request_context`，而不是未限制的瀏覽器請求。以下由 curl 或傳輸層自行產生，不得轉交：

- `Host`
- `Content-Length`
- `Connection`
- `Proxy-Authorization`
- `Range`
- `If-Range`
- hop-by-hop headers

標頭名稱及值必須拒絕 NUL、CR 與 LF。IPC 對單一值、標頭數量及整體 request context 設上限，並保持在 Native Messaging frame 上限以內。

## 敏感資料分類

分類不發出額外網絡請求。

- 只有已知安全的非敏感標頭：任務不建立 DPAPI 密文；需要跨重啟重播時可明文保存該安全子集。
- 存在 Cookie、Authorization 或任何 token／credential 類標頭：整份經篩選 request context 視為敏感並加密。
- Referer 含 userinfo 或 query、任何無法可靠判定的自訂標頭均採保守策略視為敏感，優先避免明文保存。

可明文保存的安全子集只容許固定 allowlist，例如 User-Agent、Accept、Accept-Encoding、Accept-Language，以及不含 userinfo／query 的 Origin 或 Referer。只要 request context 內有一項敏感或未知標頭，便不拆出可推斷背景的明文副本，而是加密保存整份經篩選 context。

公開網站若順帶送出 session cookie，可能被保守分類為敏感；這只增加很小的本機加密成本，不影響下載成功率。分類不會以「先匿名請求再重試」判定，避免一次性或短效 URL 的額外風險。

## IPC 與資料模型

`enqueue` request 增加可選的 request context。舊擴充功能或舊請求未提供該欄位時維持原行為。Native host 驗證並正規化後，才把資料交給下載引擎。

下載任務增加以下概念：

- 記憶體內的零化 request context，供目前程序使用；
- 可選的公開安全 request context，供不含敏感資料的任務跨重啟重播；
- 可選 DPAPI 密文，供受保護任務跨重啟恢復；
- 是否需要 Firefox 重新授權的狀態；
- request context 版本，供日後 schema 遷移。

敏感明文不進入一般可序列化任務快照。持久化可保存固定 allowlist 內的公開安全 context；其餘只保存 DPAPI 密文及非秘密分類資訊。密文使用 Windows CurrentUser 範圍，只有同一 Windows 帳戶可解密；複製狀態到另一帳戶或電腦不保證可用。DPAPI 錯誤只回報固定、已清理的診斷。

舊 schema 任務預設沒有 request context，可繼續載入及處理公開來源。

## curl 請求資料流

所有會接觸來源的 curl 路徑必須使用同一 request context：

- HEAD 探測；
- Range `0-0` 探測；
- 單線下載與續傳；
- 每個分段下載與續傳；
- 重新探測及錯誤後重試。

敏感標頭加入 curl 的 stdin config，沿用現有 Proxy 密碼的標準輸入機制及零化記憶體；不得加入 `args`，亦不得進入 `last_command_line`。curl config 的字串編碼必須正確處理引號與反斜線，並在寫入前完成 CR/LF 驗證。

Proxy 認證與網站認證維持分離。啟用 Proxy 不移除來源 request context，停用 Proxy也不改變網站身份資料。

## 跨重啟續傳

引擎保存任務時只寫入 DPAPI 密文。重啟載入受保護任務時，在目前 Windows 使用者下解密到零化記憶體，再按現有部分檔狀態恢復。

續傳前仍使用 ETag、Last-Modified、總長度與既有 If-Range 規則驗證來源。來源改變時停止拼接並回報來源已變更，不破壞既有部分檔。DPAPI 解密失敗、身份過期或來源回傳 401／403 時，任務保留部分檔並進入「需要 Firefox 重新授權」狀態，而不是誤報 Proxy 問題或清除進度。

完成、取消、移除或清除任務時，同步移除密文及可清理的臨時分段。程序關閉及錯誤路徑應讓明文 request context 由零化容器釋放。

## UI 與診斷

任務詳細資料新增固定的「來源授權」欄位，只顯示以下非秘密狀態：

- `公開（無加密資料）`：任務沒有敏感 request context 或 DPAPI 密文；
- `Firefox 授權（DPAPI 加密）`：任務已加密保存敏感 request context；
- `需要 Firefox 重新授權`：保存的身份已失效或來源拒絕；
- `授權資料無法解密`：DPAPI 密文損壞、移至其他帳戶或無法由目前帳戶解密。

不顯示標頭名稱清單、Cookie、token、密文或可推斷秘密長度的詳細資料。401／403 且任務具有 request context 時，主要動作提示重新由 Firefox 授權；沒有 request context 時維持一般來源存取錯誤提示。

## 錯誤與安全處理

- 配對不確定時寧可不附身份資料，不可跨下載洩漏 Cookie。
- 所有輸入在進入資料模型與 curl config 前驗證。
- 敏感資料不出現在 Debug／Serialize 輸出；相關型別使用自訂或省略 Debug。
- IPC frame 超限時回傳固定錯誤，不回顯任何標頭。
- DPAPI 密文被竄改或無法解密時不嘗試當作明文使用。
- 任務持久化保持原有原子寫入流程，避免密文與進度狀態不同步。

## 測試策略

### Firefox 單元測試

- 擷取及篩選 request headers。
- request ID 重新導向鏈與 DownloadItem 配對。
- 同 URL 並行下載不會互相取用身份資料。
- 過期候選會清除，配對失敗維持無 request context 的相容行為。
- CR/LF、過大資料及禁止標頭被拒絕。
- 建立 enqueue message 時正確包含或省略 request context。

### Rust 單元與 IPC 測試

- request context wire round-trip、預設相容、上限與輸入驗證。
- 敏感分類對無標頭、安全 allowlist、Cookie、Authorization、含 query Referer 及自訂標頭的結果。
- Windows DPAPI round-trip，密文不含明文；不可解密資料產生已清理錯誤。
- 任務刪除、取消、完成及清除後不再保存密文。
- 任務快照及診斷不含秘密。
- 任務詳細資料按公開、已加密、需要重新授權及解密失敗狀態顯示正確的「來源授權」文字。

### curl 規格測試

- HEAD、Range 探測、單線及分段命令都把 request context 放入 stdin config。
- Cookie、Authorization 及測試 token 不出現在命令列紀錄。
- Proxy 密碼與網站標頭可同時安全加入同一 stdin config。
- curl config escaping 不容許 header injection。

### 整合及回歸測試

建立本地 HTTP server，要求指定 Cookie 與 Referer，並支援 Range：

1. 無身份標頭時回 403，證明現有失敗模式；
2. Firefox-style request context 可完成探測及下載；
3. 中途停止引擎並保存狀態；
4. 新引擎載入同一狀態，以 DPAPI 解密後沿用部分檔完成；
5. 改變 ETag 或總長度時拒絕錯誤拼接；
6. 測試秘密不出現在 state 明文、命令列與診斷。

最後執行 Firefox extension 全套測試、Rust 全套測試、套件與 manifest 驗證，並回歸 Proxy、檔案衝突、批次下載及 Native Messaging 交接流程。

## 驗收準則

- 使用者提供的 ChatGPT estuary 類受保護連結，在 Firefox 原生下載可用的身份仍有效時，可由 Curl Downloader 成功下載。
- 直接及 Proxy 模式都使用相同網站身份資料。
- 受保護任務中斷並重啟主程式後，可由既有部分檔安全續傳。
- 公開、沒有敏感標頭的任務不保存 DPAPI 密文。
- 任務詳細資料可直接辨識來源是公開、DPAPI 加密、需要重新授權或無法解密。
- 不新增匿名分類請求。
- 秘密不出現在 state 明文、curl 命令列、UI、日誌或錯誤診斷。
- 所有新增測試及現有回歸測試通過。
