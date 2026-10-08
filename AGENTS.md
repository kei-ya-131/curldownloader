# Repository Guidelines

## Project Structure & Module Organization

本專案是 Windows 可攜式下載管理器，使用 Rust 2024、eframe 與 curl。`src/` 包含 GUI（`app.rs`）、下載引擎（`download.rs`、`curl.rs`）、狀態儲存及 Native Messaging 模組；同一執行檔提供 GUI 與 Native host。`tests/` 放置 Rust 整合測試，共用工具位於 `tests/support/`。`firefox-extension/` 包含擴充套件及其 `tests/`；`assets/` 存放 curl、字型與圖示，`scripts/` 提供建置、封裝及煙霧測試。`target/` 和 `dist/` 是產出目錄。

## Build, Test, and Development Commands

在儲存庫根目錄執行；標準建置使用 `rust-toolchain.toml` 指定的 Rust 1.97.1，以及 Windows MSVC 工具鏈。

- `cargo run`：啟動本機桌面程式；GUI 啟動會更新目前使用者的 Firefox Native host 註冊。
- `cargo fmt --check`：檢查 Rust 排版；以 `cargo fmt` 修正。
- `cargo clippy --all-targets -- -D warnings`：檢查所有 Rust 目標，將警告視為錯誤。
- `cargo test`：執行 Rust 單元與整合測試。
- `node --test firefox-extension/tests/*.test.js`：執行擴充套件測試。
- `powershell -ExecutionPolicy Bypass -File scripts/build-release.ps1`：檢查、測試並建置 `dist/CurlDownloader.exe`，包含 Windows 煙霧測試；此腳本會刪除並重建 `dist/`。

缺少 MSVC linker 時，使用 `scripts/build-release-gnu.ps1`；擴充套件封裝使用 `scripts/package-firefox-extension.ps1`。

## Coding Style & Naming Conventions

Rust 採 rustfmt 預設的四空格縮排；模組、函式使用 `snake_case`，型別使用 `PascalCase`。JavaScript 沿用兩空格縮排、分號、單引號及 `camelCase`；檔名沿用 `native-session.js` 等連字號格式。Native Messaging 欄位保持既有 `snake_case` 契約。提交前執行格式與 Clippy 檢查。

## Testing Guidelines

Rust 使用內建測試框架；JavaScript 使用 `node:test` 與 `node:assert/strict`。整合測試放在 `tests/*.rs`，擴充套件測試命名為 `*.test.js`。新增行為或修正錯誤時，涵蓋成功、失敗及重啟／恢復情境；使用本機測試伺服器與暫存目錄。未設定數值覆蓋率門檻。涉及視窗、系統匣或 Native host 的變更，另執行相關 `scripts/test-*.ps1`。

## Commit & Pull Request Guidelines

Git 歷史使用 `feat:`、`fix:`、`docs:`、`chore:` 前綴及簡短英文摘要，例如 `fix: keep GNU release build green`。每次提交聚焦單一變更。PR 應說明問題、修改後行為、測試命令與結果，並連結相關 issue；GUI 或擴充套件介面變更附上截圖，資料格式或生命週期變更說明相容性影響。

## Security & Agent Instructions

代理密碼僅保留於記憶體與管線，禁止寫入 `state.json`、擴充套件儲存或日誌。Cookie 與授權標頭也應視為機密，測試使用虛構值。維持單一主程式及既有授權驗證；`smoke-test-native-auth` 僅用於隔離測試產物。代理與貢獻者協作回應使用繁體中文。
