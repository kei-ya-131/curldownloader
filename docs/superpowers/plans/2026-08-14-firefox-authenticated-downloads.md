# Firefox Authenticated Downloads Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 讓 Firefox 嗅探到的 HTTP/HTTPS GET 下載可把實際請求身份安全交給 curl，受保護任務可跨主程式重啟續傳，並提供明確的 Firefox 重新授權操作。

**Architecture:** Firefox 以 `webRequest.onSendHeaders` 觀察實際送出的標頭，短期按 request ID／重新導向鏈配對 `downloads.onCreated`，再經 Native Messaging 傳送經篩選的 request context。Rust 將公開安全子集明文保存，將敏感 context 以 Windows DPAPI CurrentUser 加密，解密後只透過 curl stdin config 套用到所有探測與傳輸路徑；401／403 轉為可操作的重新授權狀態，Firefox 以單一五分鐘 task session 更新原任務並在來源驗證後續傳。

**Tech Stack:** Rust 2024、serde／serde_json、zeroize、windows-sys Win32 DPAPI、curl stdin config、Firefox Manifest V2 WebExtensions、Node.js `node:test`。

## Global Constraints

- 只接管 Firefox 可交接的 HTTP/HTTPS GET；不支援 `blob:`、`data:`、DRM 或 POST body 重播。
- 不新增匿名分類探測；有敏感或未知標頭時直接加密整份經篩選 context。
- Cookie、Authorization、token、敏感 Referer、敏感來源頁 URL 及 DPAPI 明文不得出現在命令列、UI、任務快照、日誌或錯誤診斷。
- DPAPI 必須使用 CurrentUser 預設範圍與 `CRYPTPROTECT_UI_FORBIDDEN`，不得使用 `CRYPTPROTECT_LOCAL_MACHINE`；所有 DPAPI 輸出以 `LocalFree` 釋放。
- `Host`、`Content-Length`、`Connection`、`Proxy-Authorization`、`Range`、`If-Range`、`Accept-Encoding` 及 hop-by-hop headers 不得由 Firefox 傳給 curl。
- 標頭名稱／值拒絕 NUL、CR、LF；最多 64 個標頭、單值最多 8 KiB、完整 context 最多 48 KiB。
- WebRequest 暫存最多 256 筆、15 秒後失效；Firefox 重新授權 session 同時只綁定一個 task ID，五分鐘後失效。
- 舊 schema 與未提供 request context 的舊 Firefox 擴充功能必須保持相容。
- 保持 `rust-version = "1.97.1"`，不引入新的密碼學 crate 或自行管理加密金鑰。
- ETag、Last-Modified 或總長度顯示來源改變時，不得拼接舊部分檔。
- 完成、取消、移除或清除受保護任務時刪除 DPAPI 密文，只保留不含秘密的來源分類旗標。

---

## File Structure

- Create `firefox-extension/request-context.js`: 純 JavaScript 的標頭篩選、重新導向追蹤、15 秒有界快取與 DownloadItem 配對。
- Create `firefox-extension/tests/request-context.test.js`: 對 request tracker 的真實輸入／輸出行為測試。
- Modify `firefox-extension/manifest.json`: 載入 request-context script，加入 `webRequest` 與 HTTP/HTTPS host 權限。
- Modify `firefox-extension/core.js`: enqueue／refresh wire message 只負責可預測的序列化。
- Modify `firefox-extension/background.js`: 註冊 WebRequest listener、把 context 放入 pending download、執行重新授權 session 與來源頁分頁操作。
- Modify `firefox-extension/settings.js` and `firefox-extension/settings.html`: 顯示「更新授權並續傳」確認流程。
- Modify `firefox-extension/popup.js`: 顯示重新授權按鈕與進行中狀態。
- Modify `firefox-extension/tests/{core,background,manifest,popup}.test.js`: 保護 Firefox 交接、權限、重傳與 UI 行為。
- Create `src/dpapi.rs`: 單一責任的 CurrentUser protect／unprotect Win32 wrapper。
- Create `src/request_context.rs`: wire 驗證、敏感分類、持久化 envelope、零化 runtime context 與 curl config escaping。
- Modify `src/lib.rs`: 匯出新 Rust 模組。
- Modify `src/model.rs`: schema v4、來源授權狀態、持久化 context、重新授權狀態與 EngineCommand。
- Modify `src/ipc.rs`: enqueue context、重新授權 claim／refresh wire contract 與已清理錯誤。
- Modify `src/curl.rs`: 所有 curl spec 接受同一 request context，秘密只寫 stdin config。
- Modify `src/download.rs`: runtime context map、重啟解密、401／403 狀態、refresh source 驗證及密文清理。
- Modify `src/app.rs`: Details「來源授權」欄位、重新授權／取消等待按鈕與來源改變決策。
- Modify `src/storage.rs`: schema v4 recovery 與密文 lifecycle 測試支援。
- Modify `tests/support/mod.rs`: 可要求 Cookie／Referer、回傳 403、支援 Range 的測試 server route。
- Modify `tests/{native_bridge,download_flow}.rs`: wire、秘密不洩漏、跨重啟與來源更新整合測試。
- Modify `README.md` and `README.zh-Hant.md`: 權限、DPAPI、公開／受保護狀態、重新授權與限制說明。

---

### Task 1: Firefox Request Context Tracker

**Files:**
- Create: `firefox-extension/request-context.js`
- Create: `firefox-extension/tests/request-context.test.js`

**Interfaces:**
- Produces: `createRequestContextTracker({ now, ttlMs, maxEntries })`。
- Produces tracker methods: `observeSendHeaders(details)`, `observeRedirect(details)`, `observeComplete(details)`, `claimDownload(download)`。
- Produces: `filterRequestHeaders(headers) -> Array<{name, value}>`。
- `claimDownload` returns `null` or `{ headers, sourcePageUrl, initialUrl, finalUrl, tabId, incognito, cookieStoreId, capturedUnixMs }`。

- [ ] **Step 1: Write failing tests for filtering and injection rejection**

```javascript
test('filters transport-owned headers and keeps website identity', () => {
  assert.deepEqual(filterRequestHeaders([
    { name: 'Host', value: 'files.example.test' },
    { name: 'Cookie', value: 'session=abc' },
    { name: 'Referer', value: 'https://example.test/page' },
    { name: 'Range', value: 'bytes=0-1' }
  ]), [
    { name: 'Cookie', value: 'session=abc' },
    { name: 'Referer', value: 'https://example.test/page' }
  ]);
});

test('rejects request headers containing a newline', () => {
  assert.throws(
    () => filterRequestHeaders([{ name: 'X-Test', value: 'ok\r\nInjected: yes' }]),
    /invalid request header/i
  );
});
```

- [ ] **Step 2: Run the new test and verify RED**

Run: `node --test firefox-extension/tests/request-context.test.js`

Expected: FAIL because `../request-context.js` does not exist.

- [ ] **Step 3: Implement strict filtering constants and limits**

```javascript
const BLOCKED_HEADERS = new Set([
  'host', 'content-length', 'connection', 'proxy-authorization',
  'range', 'if-range', 'accept-encoding', 'keep-alive', 'proxy-connection',
  'transfer-encoding', 'te', 'trailer', 'upgrade'
]);
const MAX_HEADERS = 64;
const MAX_VALUE_BYTES = 8 * 1024;
const MAX_CONTEXT_BYTES = 48 * 1024;

function filterRequestHeaders(headers) {
  // Validate token names, reject NUL/CR/LF, enforce literal byte limits,
  // discard BLOCKED_HEADERS, and return fresh {name, value} objects.
}
```

- [ ] **Step 4: Run filtering tests and verify GREEN**

Run: `node --test firefox-extension/tests/request-context.test.js`

Expected: PASS for filtering and injection cases.

- [ ] **Step 5: Write failing tracker tests for redirect correlation, concurrency and TTL**

```javascript
test('claims the exact redirect chain for a Firefox DownloadItem', () => {
  let now = 1_000;
  const tracker = createRequestContextTracker({ now: () => now, ttlMs: 15_000, maxEntries: 256 });
  tracker.observeSendHeaders({
    requestId: 'r1', url: 'https://example.test/start', tabId: 4,
    requestHeaders: [{ name: 'Cookie', value: 'session=one' }]
  });
  tracker.observeRedirect({ requestId: 'r1', url: 'https://example.test/start', redirectUrl: 'https://cdn.test/file.zip' });
  tracker.observeSendHeaders({
    requestId: 'r1', url: 'https://cdn.test/file.zip', tabId: 4,
    requestHeaders: [{ name: 'Cookie', value: 'cdn=two' }]
  });
  assert.deepEqual(tracker.claimDownload({
    id: 9, url: 'https://example.test/start', referrer: 'https://example.test/page',
    incognito: false, cookieStoreId: 'firefox-default'
  }), {
    headers: [{ name: 'Cookie', value: 'cdn=two' }],
    sourcePageUrl: 'https://example.test/page',
    initialUrl: 'https://example.test/start',
    finalUrl: 'https://cdn.test/file.zip',
    tabId: 4,
    incognito: false,
    cookieStoreId: 'firefox-default',
    capturedUnixMs: 1_000
  });
});
```

Add literal tests proving two same-URL requests with different `tabId`/`referrer` do not swap cookies, a claimed entry cannot be claimed twice, a 15,001 ms entry returns `null`, and inserting entry 257 evicts the oldest.

Add a GET-only test proving a POST observation is discarded. Add separate fixtures proving `incognito: true` and `cookieStoreId: 'firefox-container-2'` survive in claimed metadata without becoming request headers.

- [ ] **Step 6: Run tracker tests and verify RED**

Run: `node --test firefox-extension/tests/request-context.test.js`

Expected: FAIL because tracker methods are missing.

- [ ] **Step 7: Implement the bounded request-ID tracker**

```javascript
function createRequestContextTracker({ now = Date.now, ttlMs = 15_000, maxEntries = 256 } = {}) {
  const byRequestId = new Map();
  function observeSendHeaders(details) {
    pruneExpired();
    const previous = byRequestId.get(details.requestId);
    byRequestId.set(details.requestId, {
      initialUrl: previous ? previous.initialUrl : details.url,
      finalUrl: details.url,
      tabId: details.tabId,
      headers: filterRequestHeaders(details.requestHeaders || []),
      capturedUnixMs: now(),
      completedUnixMs: null
    });
    evictOldestUntil(maxEntries);
  }
  function observeRedirect(details) {
    const entry = byRequestId.get(details.requestId);
    if (entry) entry.finalUrl = details.redirectUrl;
  }
  function observeComplete(details) {
    const entry = byRequestId.get(details.requestId);
    if (entry) entry.completedUnixMs = now();
  }
  function claimDownload(download) {
    pruneExpired();
    return claimBestUnconsumedEntry(download);
  }
  return { observeSendHeaders, observeRedirect, observeComplete, claimDownload };
}
```

Implement `pruneExpired`, `evictOldestUntil`, and `claimBestUnconsumedEntry` in the same closure. Ranking order is exact `initialUrl + referrer`, exact `initialUrl`, then nearest capture time; equal scores return `null` rather than risking cross-task credentials.

- [ ] **Step 8: Run tracker tests and all Firefox tests**

Run: `node --test firefox-extension/tests/request-context.test.js firefox-extension/tests/*.test.js`

Expected: all tests PASS.

- [ ] **Step 9: Commit Task 1**

```powershell
git add firefox-extension/request-context.js firefox-extension/tests/request-context.test.js
git commit -m "feat: track Firefox download request context"
```

---

### Task 2: Rust Request Context Validation and DPAPI Protection

**Files:**
- Create: `src/dpapi.rs`
- Create: `src/request_context.rs`
- Modify: `src/lib.rs`
- Modify: `Cargo.toml`

**Interfaces:**
- Produces: `WireRequestHeader { name, value }` and `WireRequestContext { headers, source_page_url, initial_url, final_url, incognito, cookie_store_id }` with redacted `Debug`.
- Produces: `PreparedRequestContext { stored: StoredRequestContext, runtime: RequestContext, authorization: SourceAuthorization }`.
- Produces: `prepare(wire: WireRequestContext) -> Result<PreparedRequestContext, RequestContextError>`.
- Produces: `restore(stored: &StoredRequestContext) -> Result<Option<RequestContext>, RequestContextError>`.
- Produces: `dpapi::protect_current_user(&[u8]) -> io::Result<Vec<u8>>` and `dpapi::unprotect_current_user(&[u8]) -> io::Result<Zeroizing<Vec<u8>>>`.

- [ ] **Step 1: Write failing unit tests for exact validation and sensitivity decisions**

```rust
#[test]
fn cookie_makes_the_entire_context_encrypted() {
    let prepared = prepare(WireRequestContext {
        headers: vec![
            WireRequestHeader::new("Accept", "application/pdf"),
            WireRequestHeader::new("Cookie", "session=secret"),
        ],
        source_page_url: Some("https://chatgpt.com/c/1".into()),
        initial_url: "https://chatgpt.com/backend-api/estuary/content?id=1".into(),
        final_url: "https://chatgpt.com/backend-api/estuary/content?id=1".into(),
    }).unwrap();
    assert!(prepared.stored.public.is_none());
    assert!(prepared.stored.encrypted.is_some());
    assert_eq!(prepared.authorization, SourceAuthorization::Encrypted);
}

#[test]
fn safe_allowlist_context_stays_public() {
    let prepared = prepare(context_with_headers(vec![("Accept", "application/pdf")] )).unwrap();
    assert!(prepared.stored.public.is_some());
    assert!(prepared.stored.encrypted.is_none());
}
```

Add cases for `Authorization`, unknown `X-Token`, Referer with query, blocked `Range`, 65 headers, 8 KiB + 1 value, total 48 KiB + 1, and CR/LF rejection.

- [ ] **Step 2: Run unit tests and verify RED**

Run: `cargo test --lib request_context::tests`

Expected: FAIL because modules and types do not exist.

- [ ] **Step 3: Implement redacted types, allowlist and storage envelope**

```rust
#[derive(Clone, Deserialize, Serialize)]
pub struct WireRequestHeader { pub name: String, pub value: String }

impl WireRequestHeader {
    pub fn new(name: &str, value: &str) -> Self {
        Self { name: name.into(), value: value.into() }
    }
}

#[derive(Clone, Deserialize, Serialize)]
pub struct WireRequestContext {
    pub headers: Vec<WireRequestHeader>,
    pub source_page_url: Option<String>,
    pub initial_url: String,
    pub final_url: String,
    #[serde(default)]
    pub incognito: bool,
    #[serde(default)]
    pub cookie_store_id: Option<String>,
}

#[derive(Clone, Deserialize, Serialize)]
pub struct PublicRequestContext {
    pub headers: Vec<WireRequestHeader>,
    pub source_page_url: Option<String>,
    pub initial_url: String,
    pub final_url: String,
    pub incognito: bool,
    pub cookie_store_id: Option<String>,
}

pub struct RequestHeader {
    name: String,
    value: Zeroizing<String>,
}

pub struct RequestContext {
    headers: Vec<RequestHeader>,
    source_page_url: Option<Zeroizing<String>>,
    initial_url: String,
    final_url: String,
    incognito: bool,
    cookie_store_id: Option<String>,
}

#[derive(Clone, Default, Deserialize, Serialize)]
pub struct StoredRequestContext {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub public: Option<PublicRequestContext>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub encrypted: Option<Vec<u8>>,
    #[serde(default)]
    pub was_protected: bool,
}

pub enum SourceAuthorization {
    Public, Encrypted, NeedsFirefox, DecryptionFailed, ProtectedCleared,
}
```

In the test module define this complete fixture helper so every omitted browser field has a literal default:

```rust
fn context_with_headers(pairs: Vec<(&str, &str)>) -> WireRequestContext {
    WireRequestContext {
        headers: pairs.into_iter().map(|(name, value)| WireRequestHeader::new(name, value)).collect(),
        source_page_url: Some("https://app.test/page".into()),
        initial_url: "https://files.test/file.pdf".into(),
        final_url: "https://files.test/file.pdf".into(),
        incognito: false,
        cookie_store_id: Some("firefox-default".into()),
    }
}
```

Implement custom `Debug` for wire/runtime types that prints counts and URL origins only, never values or full URLs.

- [ ] **Step 4: Write failing Windows DPAPI round-trip and tamper tests**

```rust
#[cfg(windows)]
#[test]
fn dpapi_round_trip_does_not_embed_plaintext() {
    let plaintext = b"Cookie: session=super-secret";
    let encrypted = protect_current_user(plaintext).unwrap();
    assert!(!encrypted.windows(plaintext.len()).any(|window| window == plaintext));
    assert_eq!(&*unprotect_current_user(&encrypted).unwrap(), plaintext);
}

#[cfg(windows)]
#[test]
fn tampered_dpapi_blob_is_rejected() {
    let mut encrypted = protect_current_user(b"secret").unwrap();
    encrypted[encrypted.len() / 2] ^= 0x55;
    assert!(unprotect_current_user(&encrypted).is_err());
}
```

- [ ] **Step 5: Run DPAPI tests and verify RED**

Run: `cargo test --lib dpapi::tests`

Expected: FAIL because wrappers are missing.

- [ ] **Step 6: Implement non-interactive CurrentUser DPAPI wrappers**

```rust
pub fn protect_current_user(plaintext: &[u8]) -> io::Result<Vec<u8>> {
    // Build DATA_BLOB, call CryptProtectData with CRYPTPROTECT_UI_FORBIDDEN,
    // copy output, zero temporary plaintext buffers, and LocalFree output.pbData.
}

pub fn unprotect_current_user(ciphertext: &[u8]) -> io::Result<Zeroizing<Vec<u8>>> {
    // Call CryptUnprotectData with CRYPTPROTECT_UI_FORBIDDEN, copy to Zeroizing,
    // SecureZeroMemory the Win32 plaintext buffer, then LocalFree it.
}
```

Use the existing `windows-sys` `Win32_Security_Cryptography` feature; add only a missing `Win32_System_Memory` feature if `LocalFree` is not exposed by the current feature set.

- [ ] **Step 7: Connect `prepare`/`restore` to DPAPI and run all library tests**

Run: `cargo test --lib request_context::tests`

Run: `cargo test --lib dpapi::tests`

Expected: all Task 2 tests PASS and no secret appears in assertion failure output.

- [ ] **Step 8: Commit Task 2**

```powershell
git add Cargo.toml Cargo.lock src/lib.rs src/dpapi.rs src/request_context.rs
git commit -m "feat: protect Firefox request context with DPAPI"
```

---

### Task 3: Persisted Task Model and Native Wire Contract

**Files:**
- Modify: `src/model.rs`
- Modify: `src/storage.rs`
- Modify: `src/ipc.rs`
- Modify: `tests/native_bridge.rs`

**Interfaces:**
- Consumes: `StoredRequestContext`, `PreparedRequestContext`, `WireRequestContext`, `SourceAuthorization` from Task 2.
- Produces `DownloadTask.request_context: StoredRequestContext`, `DownloadTask.authorization: SourceAuthorization`, and `DownloadTask.reauthorization_requested: bool`.
- Produces `TaskSnapshot.authorization` and `TaskSnapshot.reauthorization_requested` without URLs or header values.
- Extends `IpcRequest::Enqueue` with `request_context: Option<WireRequestContext>`.

- [ ] **Step 1: Write failing wire tests for context round-trip and redaction**

```rust
#[test]
fn enqueue_accepts_a_firefox_request_context_without_echoing_secrets() {
    let value = serde_json::json!({
        "type": "enqueue",
        "request_id": "auth-1",
        "url": "https://files.test/a.pdf",
        "filename": "a.pdf",
        "target_dir": "C:\\Downloads",
        "requested_segments": 4,
        "proxy": WireProxy::direct(),
        "request_context": {
            "headers": [{"name":"Cookie","value":"session=secret"}],
            "source_page_url":"https://app.test/page",
            "initial_url":"https://files.test/a.pdf",
            "final_url":"https://files.test/a.pdf",
            "incognito":false,
            "cookie_store_id":"firefox-default"
        }
    });
    let request: IpcRequest = serde_json::from_value(value).unwrap();
    assert!(!format!("{request:?}").contains("session=secret"));
}
```

Add a frame-size/invalid-header test asserting stable code `invalid_request_context` and a legacy enqueue fixture with no `request_context`.

- [ ] **Step 2: Run native bridge tests and verify RED**

Run: `cargo test --test native_bridge enqueue_accepts_a_firefox_request_context_without_echoing_secrets`

Expected: FAIL because the wire field is unknown or unhandled.

- [ ] **Step 3: Extend model schema and snapshots**

```rust
pub const CURRENT_SCHEMA_VERSION: u32 = 4;

pub struct DownloadTask {
    #[serde(default)]
    pub request_context: StoredRequestContext,
    #[serde(default)]
    pub authorization: SourceAuthorization,
    #[serde(default)]
    pub reauthorization_requested: bool,
}
```

`ConfiguredTask` carries `request_context: Option<PreparedRequestContext>`. `TaskSnapshot` carries only `authorization` and `reauthorization_requested`; do not expose `StoredRequestContext`.

- [ ] **Step 4: Write failing storage migration and terminal cleanup tests**

Add literal schema-v3 JSON proving it loads as `Public` with empty context. Add a task containing an encrypted byte vector and assert `mark_authorization_cleared()` removes both public/encrypted payloads, sets `was_protected = true`, and produces `ProtectedCleared`.

- [ ] **Step 5: Run storage/model tests and verify RED**

Run: `cargo test --lib model::tests`

Run: `cargo test --lib storage::tests`

Expected: FAIL on schema v4 defaults and cleanup behavior.

- [ ] **Step 6: Implement schema defaults, migration and IPC dispatch**

Deserialize `request_context` with `#[serde(default)]`; validate/prepare it in the trusted `Enqueue` dispatch before sending `EngineCommand::AddConfiguredWithOrigin`. Map every validation failure to:

```rust
WireError {
    code: "invalid_request_context".into(),
    message: "Firefox 下載請求資料無效。".into(),
}
```

Never include the validation input or secret length in the message.

- [ ] **Step 7: Run model, storage and native bridge suites**

Run: `cargo test --lib model::tests`

Run: `cargo test --lib storage::tests`

Run: `cargo test --test native_bridge`

Expected: all PASS.

- [ ] **Step 8: Commit Task 3**

```powershell
git add src/model.rs src/storage.rs src/ipc.rs tests/native_bridge.rs
git commit -m "feat: carry authenticated request context over native bridge"
```

---

### Task 4: Apply Request Context to Every curl Path

**Files:**
- Modify: `src/request_context.rs`
- Modify: `src/curl.rs`
- Modify: `src/download.rs`
- Modify: `tests/proxy_flow.rs`

**Interfaces:**
- Consumes: runtime `RequestContext` from Task 2.
- Produces: `RequestContext::append_curl_config(&self, config: &mut Zeroizing<String>) -> Result<(), String>`.
- All four builders accept `request_context: Option<&RequestContext>`.

- [ ] **Step 1: Write failing tests proving secrets use stdin on all curl builders**

```rust
#[test]
fn website_credentials_only_enter_stdin_config() {
    let context = prepare(WireRequestContext {
        headers: vec![WireRequestHeader::new("Cookie", "session=very-secret")],
        source_page_url: Some("https://app.test/page".into()),
        initial_url: "https://example.test/file".into(),
        final_url: "https://example.test/file".into(),
        incognito: false,
        cookie_store_id: Some("firefox-default".into()),
    }).unwrap().runtime;
    let spec = build_head_probe(
        &ProxySettings::default(), Some(&context),
        "https://example.test/file", Path::new("headers.txt")
    ).unwrap();
    assert!(!spec.args.iter().any(|arg| arg.to_string_lossy().contains("very-secret")));
    assert!(spec.stdin_config.as_deref().unwrap().contains("session=very-secret"));
}
```

Repeat the observable assertion for `build_range_probe`, `build_single_transfer`, and `build_segment_transfer`. Add a combined Proxy password + Cookie case proving both exist in stdin and neither exists in `args`/`last_command_line`.

- [ ] **Step 2: Run curl tests and verify RED**

Run: `cargo test --lib curl::tests`

Expected: compile FAIL because builders do not accept request context.

- [ ] **Step 3: Implement curl config escaping and merge with existing stdin config**

```rust
impl RequestContext {
    pub fn append_curl_config(&self, config: &mut Zeroizing<String>) -> Result<(), String> {
        for header in self.headers() {
            let line = format!("{}: {}", header.name(), header.value());
            config.push_str("header = \"");
            push_curl_config_escaped(config, &line)?;
            config.push_str("\"\n");
        }
        Ok(())
    }
}
```

Escape `\` and `"`; reject NUL/CR/LF before escaping. Change `CurlCommandSpec::base` to always build a single optional `Zeroizing<String>` and append Proxy credentials and website headers to it.

- [ ] **Step 4: Thread context through every builder and `Engine::spawn_job`**

Change signatures to the exact common ordering:

```rust
pub fn build_head_probe(proxy: &ProxySettings, context: Option<&RequestContext>, url: &str, headers: &Path) -> Result<CurlCommandSpec, String>;
pub fn build_range_probe(proxy: &ProxySettings, context: Option<&RequestContext>, url: &str, headers: &Path) -> Result<CurlCommandSpec, String>;
pub fn build_single_transfer(proxy: &ProxySettings, context: Option<&RequestContext>, url: &str, output: &Path, existing: u64, if_range: Option<&str>, headers: &Path) -> Result<CurlCommandSpec, String>;
pub fn build_segment_transfer(proxy: &ProxySettings, context: Option<&RequestContext>, url: &str, start: u64, end: u64, existing: u64, if_range: Option<&str>, headers: &Path) -> Result<CurlCommandSpec, String>;
```

- [ ] **Step 5: Run curl and Proxy tests and verify GREEN**

Run: `cargo test --lib curl::tests`

Run: `cargo test --test proxy_flow`

Expected: all PASS and test secrets absent from command-line metrics.

- [ ] **Step 6: Commit Task 4**

```powershell
git add src/request_context.rs src/curl.rs src/download.rs tests/proxy_flow.rs
git commit -m "feat: replay Firefox identity through curl stdin"
```

---

### Task 5: Firefox Listener Registration and Authenticated Handoff

**Files:**
- Modify: `firefox-extension/manifest.json`
- Modify: `firefox-extension/core.js`
- Modify: `firefox-extension/background.js`
- Modify: `firefox-extension/tests/manifest.test.js`
- Modify: `firefox-extension/tests/core.test.js`
- Modify: `firefox-extension/tests/background.test.js`

**Interfaces:**
- Consumes tracker from Task 1.
- Produces enqueue JSON field `request_context` matching Task 3, including `incognito` and `cookie_store_id` browser identity metadata.
- Registers `webRequest.onSendHeaders`, `onBeforeRedirect`, `onCompleted`, and `onErrorOccurred` without blocking/modification permissions.

- [ ] **Step 1: Write failing manifest behavior test**

```javascript
test('manifest grants read-only request observation and loads tracker before background', () => {
  assert.equal(manifest.permissions.includes('webRequest'), true);
  assert.equal(manifest.permissions.includes('webRequestBlocking'), false);
  assert.equal(manifest.permissions.includes('http://*/*'), true);
  assert.equal(manifest.permissions.includes('https://*/*'), true);
  assert.deepEqual(manifest.background.scripts.slice(0, 2), ['core.js', 'request-context.js']);
});
```

- [ ] **Step 2: Run manifest test and verify RED**

Run: `node --test firefox-extension/tests/manifest.test.js`

Expected: FAIL because permissions/script are absent.

- [ ] **Step 3: Update manifest with minimum read-only permissions**

Add `webRequest`, `http://*/*`, and `https://*/*`; do not add `webRequestBlocking`. Load `request-context.js` before `background.js`.

- [ ] **Step 4: Write failing handoff tests with complete WebRequest fixtures**

Extend `makeFakeBrowser` with real listener slots for `onSendHeaders`, `onBeforeRedirect`, `onCompleted`, `onErrorOccurred`. Test that an observed Cookie/Referer becomes the exact `request_context` in the native `enqueue`, while a download with no claim omits the field and still enqueues successfully.

```javascript
assert.deepEqual(enqueue.request_context.headers, [
  { name: 'Cookie', value: 'session=secret' },
  { name: 'Referer', value: 'https://chatgpt.com/c/1' }
]);
assert.equal(enqueue.request_context.source_page_url, 'https://chatgpt.com/c/1');
```

- [ ] **Step 5: Run core/background tests and verify RED**

Run: `node --test firefox-extension/tests/core.test.js firefox-extension/tests/background.test.js`

Expected: FAIL because listeners and enqueue serialization are missing.

- [ ] **Step 6: Register tracker listeners and enrich pending downloads**

Use `onSendHeaders` because it observes the final sent headers after other extensions. Pass `['requestHeaders']`; do not return a blocking response. In `handleCreatedDownload`, call `tracker.claimDownload(download)` before pausing, and store the claimed context on the in-memory pending object. `clonePending` must omit header values so settings never receives secrets unnecessarily; `submitExternalDownload` reads the original pending object and sends the context directly to native.

- [ ] **Step 7: Update `buildEnqueueMessage` with an optional fourth argument**

```javascript
function buildEnqueueMessage(download, form, requestId, requestContext) {
  const message = {
    type: 'enqueue', request_id: String(requestId), url: String(download.url),
    filename: String(form.filename || download.filename || '').trim(),
    target_dir: String(form.targetDir || '').trim(),
    requested_segments: Number(form.segments) || 4,
    proxy: {
      enabled: Boolean((form.proxy || {}).enabled),
      protocol: String((form.proxy || {}).protocol || 'http'),
      host: String((form.proxy || {}).host || '').trim(),
      port: Number((form.proxy || {}).port) || 8080,
      username: String((form.proxy || {}).username || ''),
      password: String((form.proxy || {}).password || '')
    }
  };
  if (requestContext) message.request_context = requestContext;
  return message;
}
```

Keep this inline Proxy object identical to the current `core.test.js` proxy fixture so request-context work cannot change Proxy wire behavior.

- [ ] **Step 8: Run all Firefox tests and extension package validation**

Run: `node --test firefox-extension/tests/*.test.js`

Run: `powershell -ExecutionPolicy Bypass -File scripts/test-firefox-extension-package.ps1`

Expected: all PASS.

- [ ] **Step 9: Commit Task 5**

```powershell
git add firefox-extension/manifest.json firefox-extension/core.js firefox-extension/background.js firefox-extension/tests/manifest.test.js firefox-extension/tests/core.test.js firefox-extension/tests/background.test.js
git commit -m "feat: hand Firefox request identity to native downloads"
```

---

### Task 6: Engine Restart Recovery and Auth Failure State

**Files:**
- Modify: `src/curl.rs`
- Modify: `src/download.rs`
- Modify: `src/model.rs`
- Modify: `tests/support/mod.rs`
- Modify: `tests/download_flow.rs`

**Interfaces:**
- Consumes persisted/runtime context from Tasks 2–4.
- Produces `TaskStatus::NeedsFirefoxAuthorization` and `SourceAuthorization::{NeedsFirefox,DecryptionFailed,ProtectedCleared}` transitions.
- Produces `parse_last_http_status(headers: &str) -> Option<u16>`.

- [ ] **Step 1: Extend the real test server with required request headers**

Add to `tests/support/mod.rs`:

```rust
pub struct Route {
    pub required_headers: Vec<(String, String)>,
}
```

The server returns `403 Forbidden` with an empty body when any literal required header is absent; otherwise it follows the existing Range behavior. Update existing fixtures with `required_headers: Vec::new()` mechanically.

Add this test-harness boundary so integration tests use the real engine path without duplicating channel setup:

```rust
impl EngineHarness {
    pub fn add_firefox_configured_with_context(
        &mut self,
        url: String,
        filename: &str,
        wire: WireRequestContext,
    ) -> TaskId {
        let prepared = request_context::prepare(wire).unwrap();
        self.add_configured_with_origin(url, filename.into(), TaskOrigin::Firefox, Some(prepared))
    }
}

fn protected_context() -> WireRequestContext {
    context_with_headers(vec![
        ("Cookie", "session=valid"),
        ("Referer", "https://app.test/page"),
    ])
}
```

`add_configured_with_origin` is added beside the existing `add_configured` harness helper and sends `EngineCommand::AddConfiguredWithOrigin` with the supplied origin/context. `context_with_headers` is a test-support constructor with the same literal defaults defined in Task 2; place its test-only copy in `tests/support/mod.rs` because integration tests cannot access a private unit-test module.

- [ ] **Step 2: Write the failing authenticated download integration test**

```rust
#[test]
fn firefox_cookie_and_referer_complete_a_protected_download() {
    let server = TestHttpServer::start(vec![Route {
        path: "/private.pdf",
        body: b"protected payload",
        ranges: true,
        etag: "private-v1",
        filename: "private.pdf",
        required_headers: vec![
            ("cookie".into(), "session=valid".into()),
            ("referer".into(), "https://app.test/page".into()),
        ],
    }]);
    let mut harness = EngineHarness::new(2);
    let id = harness.add_firefox_configured_with_context(
        format!("{}/private.pdf", server.base_url), protected_context()
    );
    harness.wait_for(id, TaskStatus::Completed, Duration::from_secs(60));
    assert_eq!(fs::read(harness.download_dir().join("private.pdf")).unwrap(), b"protected payload");
}
```

- [ ] **Step 3: Run the integration test and verify RED**

Run: `cargo test --test download_flow firefox_cookie_and_referer_complete_a_protected_download -- --nocapture`

Expected: FAIL with curl 403 until context reaches every curl process.

- [ ] **Step 4: Write failing restart and plaintext-leak tests**

Start a large throttled protected route, wait for non-zero progress, send `EngineCommand::Shutdown`, load the same `state.json` into a new engine, and assert completion from existing part bytes. Assert the raw state contains neither `session=valid` nor the Referer query token, but contains a non-empty encrypted byte array.

- [ ] **Step 5: Run restart test and verify RED**

Run: `cargo test --test download_flow protected_download_resumes_after_engine_restart -- --nocapture`

Expected: FAIL because engine load does not restore runtime context.

- [ ] **Step 6: Implement runtime context restoration and sanitized failure states**

Add `runtime_contexts: HashMap<TaskId, RequestContext>` to `Engine`. In `Engine::new`, restore each stored context; on DPAPI failure leave parts intact and set `TaskStatus::NeedsFirefoxAuthorization` plus `SourceAuthorization::DecryptionFailed`. On successful load set `Encrypted`; public safe context restores without DPAPI.

Parse the last HTTP header status even when curl exits 22. For Firefox-origin tasks with a stored context, map 401/403 to:

```rust
TaskError {
    kind: ErrorKind::Http,
    summary: "需要 Firefox 重新授權".into(),
    code: Some(status.into()),
    diagnostic: format!("HTTP {status}"),
    action: "在 Firefox 重新授權".into(),
}
```

Do not include headers or URLs in the diagnostic.

- [ ] **Step 7: Implement terminal secret cleanup**

Before persisting Completed/Cancelled/removed tasks, call `clear_secret_material()` to remove encrypted/public context and runtime map entries while keeping `was_protected`. Confirm completed protected tasks snapshot as `ProtectedCleared`.

- [ ] **Step 8: Run download flow and full Rust tests**

Run: `cargo test --test download_flow`

Run: `cargo test --all-targets`

Expected: all PASS.

- [ ] **Step 9: Commit Task 6**

```powershell
git add src/curl.rs src/download.rs src/model.rs tests/support/mod.rs tests/download_flow.rs
git commit -m "feat: resume protected downloads across restarts"
```

---

### Task 7: Reauthorization Backend and Source Refresh

**Files:**
- Modify: `src/model.rs`
- Modify: `src/download.rs`
- Modify: `src/ipc.rs`
- Modify: `tests/native_bridge.rs`
- Modify: `tests/download_flow.rs`

**Interfaces:**
- Produces Engine commands `RequestFirefoxReauthorization`, `CancelFirefoxReauthorization`, and `RefreshFirefoxSource`.
- Produces IPC requests `begin_reauthorization`, `claim_reauthorization`, `cancel_reauthorization`, and `refresh_task_source`.
- `refresh_task_source` carries `allow_source_change: bool`; the first request always sends `false`.
- `claim_reauthorization` is the only response allowed to reveal a decrypted `source_page_url`, and only to the already authenticated Native Messaging client.
- Produces `SourceRefreshOutcome::{Resumed, SourceChanged, AuthorizationRejected}`.
- Produces redacted `ReauthorizationDetails { task_id, filename, source_origin, source_page_url, incognito, cookie_store_id }`; its `Debug` implementation omits `source_page_url`.

- [ ] **Step 1: Write failing idempotent state-transition tests**

```rust
#[test]
fn requesting_reauthorization_twice_keeps_one_pending_request() {
    let (_server, mut harness, id) = start_protected_task_with_rejected_cookie();
    harness.request_firefox_reauthorization(id);
    harness.request_firefox_reauthorization(id);
    let task = harness.snapshot(id);
    assert!(task.reauthorization_requested);
    assert_eq!(task.status, TaskStatus::NeedsFirefoxAuthorization);
}
```

Define `start_protected_task_with_rejected_cookie()` in `tests/download_flow.rs` by starting a `required_headers` route that expects `session=fresh`, enqueueing `session=expired`, waiting for `NeedsFirefoxAuthorization`, and returning `(TestHttpServer, EngineHarness, TaskId)` so the caller keeps the server guard alive. Add `request_firefox_reauthorization(id)` and `snapshot(id)` as thin test-harness channel/read helpers in `tests/support/mod.rs`; assertions remain on real engine snapshots, not mock calls.

Add cases rejecting GUI-origin, completed, cancelled and unknown task IDs; cancelling clears only the waiting flag and preserves parts/context.

- [ ] **Step 2: Run transition tests and verify RED**

Run: `cargo test --test download_flow reauthorization -- --nocapture`

Expected: compile FAIL because commands are absent.

- [ ] **Step 3: Add engine commands and trusted wire requests**

```rust
pub enum EngineCommand {
    RequestFirefoxReauthorization { id: TaskId, response: Sender<Result<(), String>> },
    CancelFirefoxReauthorization { id: TaskId, response: Sender<Result<(), String>> },
    RefreshFirefoxSource {
        id: TaskId,
        context: PreparedRequestContext,
        response: Sender<Result<SourceRefreshOutcome, String>>,
    },
}

pub enum SourceRefreshOutcome {
    Resumed,
    SourceChanged,
    AuthorizationRejected,
}
```

`begin_reauthorization` sets the flag. `claim_reauthorization` atomically clears the claimable flag and returns task ID, filename, source origin and decrypted source page URL; `list_tasks` exposes only booleans. All untrusted pipe clients receive `unauthorized_client` as today.

- [ ] **Step 4: Write failing refresh-source identity tests**

Test three literal server versions: same ETag/length resumes; changed ETag returns `source_changed` without deleting parts; new 403 remains `NeedsFirefoxAuthorization`. Test a 0% failed task can refresh and complete.

- [ ] **Step 5: Run source refresh tests and verify RED**

Run: `cargo test --test download_flow refresh_firefox_source -- --nocapture`

Expected: FAIL because source replacement/validation is missing.

- [ ] **Step 6: Implement atomic context replacement and source validation**

Prepare/encrypt the new context before mutating the task. Stop existing jobs, retain old stored/runtime context until the fresh Range probe proves accessible, then compare old validators. On same source commit the new context and restart; on changed source with `allow_source_change = false` return `SourceRefreshOutcome::SourceChanged` without mutation; on a repeated request with `allow_source_change = true`, remove old part files, reset validators, commit the fresh context and restart from byte zero. On auth rejection retain parts and set NeedsFirefoxAuthorization.

- [ ] **Step 7: Run wire and engine suites**

Run: `cargo test --test native_bridge`

Run: `cargo test --test download_flow reauthorization -- --nocapture`

Run: `cargo test --test download_flow refresh_firefox_source -- --nocapture`

Expected: all PASS with stable wire codes and no response containing secret values.

- [ ] **Step 8: Commit Task 7**

```powershell
git add src/model.rs src/download.rs src/ipc.rs tests/native_bridge.rs tests/download_flow.rs
git commit -m "feat: refresh protected tasks from Firefox"
```

---

### Task 8: Reauthorization UX in Firefox and Task Details

**Files:**
- Modify: `firefox-extension/background.js`
- Modify: `firefox-extension/popup.js`
- Modify: `firefox-extension/settings.js`
- Modify: `firefox-extension/settings.html`
- Modify: `firefox-extension/tests/background.test.js`
- Modify: `firefox-extension/tests/popup.test.js`
- Modify: `firefox-extension/tests/settings.test.js`
- Modify: `src/app.rs`

**Interfaces:**
- Consumes Task 7 wire actions and Task 1 tracker.
- Produces one in-memory `activeReauthorization` session `{ taskId, sourceOrigin, expectedFilename, expiresUnixMs }`.
- Details and Popup actions use the same task ID; no URL or secret is rendered.

- [ ] **Step 1: Write failing Task Details label/action tests**

Add table-driven Rust tests for these exact labels:

```rust
assert_eq!(source_authorization_label(SourceAuthorization::Public), "公開（無加密資料）");
assert_eq!(source_authorization_label(SourceAuthorization::Encrypted), "Firefox 授權（DPAPI 加密）");
assert_eq!(source_authorization_label(SourceAuthorization::NeedsFirefox), "需要 Firefox 重新授權");
assert_eq!(source_authorization_label(SourceAuthorization::DecryptionFailed), "授權資料無法解密");
assert_eq!(source_authorization_label(SourceAuthorization::ProtectedCleared), "受保護（授權資料已清除）");
```

Test that only Firefox NeedsAuthorization tasks emit `EngineCommand::RequestFirefoxReauthorization`, waiting tasks emit cancel, and 0% tasks still show the button.

- [ ] **Step 2: Run app tests and verify RED**

Run: `cargo test --lib app::tests`

Expected: FAIL because label/action helpers are missing.

- [ ] **Step 3: Implement Details field and buttons**

Add「來源授權」to `show_task_overview`/details. Render「在 Firefox 重新授權」or「取消等待 Firefox」next to the sanitized task error action. Keep source page URL and header names out of all widgets/tooltips.

- [ ] **Step 4: Write failing Firefox session/tab behavior tests**

Extend the fake `tabs` API with `query`, `update`, and `create`. Test:

- existing exact source page -> `tabs.update(id, {active:true})`, no `create` and no reload;
- missing Container tab -> one `tabs.create({url: sourcePageUrl, cookieStoreId})`;
- private source without a private window -> instruction state, no normal `tabs.create` containing the source URL;
- missing/unsafe page -> one internal `reauthorize.html?taskId=<id>` page;
- duplicate click -> still one session/tab;
- 300,001 ms -> next download does not refresh the old task;
- exact referrer/origin/filename -> pending settings mode `reauthorize`, submit sends `refresh_task_source` instead of `enqueue`;
- mismatched download -> normal new-download flow and no secret crosses task IDs.

- [ ] **Step 5: Run Firefox UI tests and verify RED**

Run: `node --test firefox-extension/tests/background.test.js firefox-extension/tests/popup.test.js firefox-extension/tests/settings.test.js`

Expected: FAIL because actions/session mode are absent.

- [ ] **Step 6: Implement one five-minute session and tab reuse**

```javascript
const REAUTH_TTL_MS = 5 * 60 * 1000;
let activeReauthorization = null;

async function openReauthorization(details) {
  // Reject/replace only an expired session; otherwise preserve one task ID.
  // Query tabs, focus exact URL without reload, else create one safe page.
}
```

When background task polling sees `reauthorization_requested`, call `claim_reauthorization`. Popup click calls `begin_reauthorization` and uses its returned details immediately. Match the next DownloadItem using exact referrer/source origin and expected basename; uncertain matches open settings with an explicit task confirmation and never auto-submit.

- [ ] **Step 7: Implement settings refresh mode and Popup button**

Settings heading/button copy becomes「更新授權並續傳」when `reauthorizeTaskId` exists. Popup shows「重新授權」only for `needs_firefox_authorization`, and「等待 Firefox」disabled while pending. A refresh result of `source_changed` displays「來源已變更」with buttons「重新下載」and「保留舊任務」；「重新下載」重送同一 context 並設定 `allow_source_change: true`，「保留舊任務」取消 session；兩者都不會把舊分段拼到新來源。

- [ ] **Step 8: Run app and Firefox suites**

Run: `cargo test --lib app::tests`

Run: `node --test firefox-extension/tests/*.test.js`

Expected: all PASS.

- [ ] **Step 9: Commit Task 8**

```powershell
git add src/app.rs firefox-extension/background.js firefox-extension/popup.js firefox-extension/settings.js firefox-extension/settings.html firefox-extension/tests/background.test.js firefox-extension/tests/popup.test.js firefox-extension/tests/settings.test.js
git commit -m "feat: guide Firefox task reauthorization"
```

---

### Task 9: Documentation, Packaging and End-to-End Verification

**Files:**
- Modify: `README.md`
- Modify: `README.zh-Hant.md`
- Modify: `scripts/test-firefox-extension-package.ps1` only if the new script is not already included by the existing generic package check.

**Interfaces:**
- Consumes all previous tasks.
- Produces release-ready EXE/XPI verification evidence and user-facing security limitations.

- [ ] **Step 1: Update both READMEs with exact behavior**

Document the added `webRequest` + HTTP/HTTPS host permissions, 15-second in-memory correlation, DPAPI CurrentUser persistence, Details labels, five-minute manual reauthorization, no automatic URL replay, GET-only limitation, and the fact that administrator password reset/cross-account copies can make DPAPI data unavailable.

- [ ] **Step 2: Run formatting before verification**

Run: `cargo fmt --all -- --check`

Expected: PASS. If it reports differences, run `cargo fmt --all`, inspect the diff, then rerun the check.

- [ ] **Step 3: Run complete JavaScript tests**

Run: `node --test firefox-extension/tests/*.test.js`

Expected: all PASS with zero unhandled rejection warnings.

- [ ] **Step 4: Run complete Rust tests**

Run: `cargo test --all-targets`

Expected: all PASS, including authenticated 403 reproduction, direct/Proxy replay, restart resume, source-change refusal and secret-redaction cases.

- [ ] **Step 5: Run Firefox packaging and native manifest checks**

Run: `powershell -ExecutionPolicy Bypass -File scripts/test-firefox-extension-package.ps1`

Run: `powershell -ExecutionPolicy Bypass -File scripts/test-firefox-native-host-manifest.ps1`

Expected: both PASS and packaged XPI contains `request-context.js` plus updated manifest permissions.

- [ ] **Step 6: Build the release artifacts**

Run: `powershell -ExecutionPolicy Bypass -File scripts/build-release.ps1`

Expected: release build succeeds and `dist/CurlDownloader.exe` plus `dist/curl-downloader.xpi` are regenerated.

- [ ] **Step 7: Inspect the final diff for secrets and unrelated changes**

Run: `git diff --check`

Run: `git status --short`

Run: `rg -n "session=very-secret|session=valid|super-secret" src firefox-extension README.md README.zh-Hant.md`

Expected: diff check passes; secret fixture strings appear only in test files; status contains only planned files/artifacts.

- [ ] **Step 8: Commit Task 9**

```powershell
git add README.md README.zh-Hant.md scripts/test-firefox-extension-package.ps1 dist/CurlDownloader.exe dist/curl-downloader.xpi
git commit -m "docs: explain authenticated Firefox downloads"
```

If `dist/` is intentionally ignored or release binaries are not versioned, omit them from `git add` and report their verified absolute paths instead.

---

## Final Review Gate

- [ ] Confirm every new production behavior was preceded by a failing test observed for the intended reason.
- [ ] Confirm all request-context code paths use real values in tests; mocks stop only at Firefox/Win32/network process boundaries.
- [ ] Confirm `git grep` and command metrics contain no runtime secret value.
- [ ] Confirm protected downloads work both direct and through the existing Proxy settings.
- [ ] Confirm a protected partial download completes after engine restart under the same Windows account.
- [ ] Confirm source changes never merge old and new bytes.
- [ ] Confirm Details and Popup expose public/encrypted/reauthorization/cleared states without revealing URLs or headers.
- [ ] Confirm Firefox never automatically replays an old signed URL or clicks page content.
