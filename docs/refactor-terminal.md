# 終端與 PTY 邏輯重構計畫

## 1. 背景與動機 (Background & Motivation)
目前 `CliGJ` 專案中的終端邏輯高度耦合在 `app/src/terminal/windows_conpty.rs` 中。這個檔案不僅包含了 Windows 專屬的 `CreatePseudoConsole` 底層 API 呼叫，還包含了跨平台的終端模擬（使用 `wezterm-term`）、背景讀取執行緒（Reader Thread）、指紋識別（Fingerprinting）以及渲染快照生成。
為了讓未來更容易支援 macOS/Linux，並朝向將 `cligj-terminal` 提取為獨立 Crate 的目標邁進，必須將「作業系統的 PTY 實作」與「跨平台的終端模擬及狀態管理」解耦。

## 2. 範圍與影響 (Scope & Impact)
- **影響範圍**：主要重構 `app/src/terminal/*` 目錄內的程式碼。
- **外部依賴**：`app/src/gui/run/timers.rs` 和 `app/src/gui/run/mod.rs` 將需要更新匯入路徑，但其核心的 AI 歷史記錄合併邏輯將保持不變。
- **目標平台**：維持現有 Windows 支援的穩定性，並為未來 Unix 平台留下乾淨的介面。

## 3. 解決方案 (Proposed Solution)

我們將 `app/src/terminal/` 拆分為以下幾個明確的模組：

1. **`types.rs` (數據結構層)**
   - 集中定義所有終端與 GUI 溝通的共用結構，包含：`TerminalRender`、`ReaderRenderMode`、`ControlCommand` 等。
   - 確保這些類型與任何作業系統底層 API 無關。

2. **`pty.rs` (抽象層)**
   - 定義一個 `Pty` 特徵 (Trait) 或抽象介面，包含核心能力：`read`、`write`、`resize`。
   - 這樣做可以讓 `TerminalSession` 不知道底層是 Windows ConPTY 還是 Unix PTY。

3. **`windows_conpty.rs` (平台實作層)**
   - 剝離原有的讀取執行緒與 `wezterm-term` 邏輯。
   - 專注於呼叫 Windows API 建立 ConPTY、配置 Startup Info 以及實作 `Pty` 的讀寫介面。

4. **`session.rs` (終端模擬與會話層)**
   - 新增 `TerminalSession` 結構。
   - 將 `start_reader_thread` 移至此處。它將接收一個 `Pty` 實例，負責將位元組送入 `wezterm-term::Terminal`，並執行指紋快照生成邏輯，最終將 `TerminalRender` 拋給 GUI。

5. **`render.rs` 與 `key_encoding.rs` (輔助層)**
   - 保持現狀，因為它們已經很好地解耦了。

## 4. 實作步驟 (Implementation Steps)

- **步驟一**：建立 `app/src/terminal/types.rs`，從 `windows_conpty.rs` 遷移通用的結構體與列舉。
- **步驟二**：建立 `app/src/terminal/pty.rs`，定義 `PtyProcess` 和 `PtyReader` / `PtyWriter` 的特徵。
- **步驟三**：建立 `app/src/terminal/session.rs`，遷移並重構 `start_reader_thread` 及其相依的快照邏輯（如 `terminal_render_from_lines_cached`）。
- **步驟四**：重構 `app/src/terminal/windows_conpty.rs`，使其僅負責 Windows API 呼叫，並返回符合抽象層介面的物件。
- **步驟五**：更新 `app/src/terminal/mod.rs` 以匯出新的模組結構。
- **步驟六**：修復 `app/src/gui/run/mod.rs`、`app/src/gui/run/timers.rs` 等外部呼叫處的匯入路徑及初始化邏輯。

## 5. 驗證與測試 (Verification & Testing)
- 確保專案能成功編譯 (`cargo build`)。
- 啟動 `CliGJ` 並測試基本的終端輸入、輸出、視窗縮放（Resize）是否能如常運作。
- 測試 AI 互動模式下的渲染與歷史記錄存檔是否保持原有的穩定性。
