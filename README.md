# CliGJ 專案說明

CliGJ 是一個以 Rust + Slint 開發的桌面工具，目標是把「本機終端機工作流」與「AI 指令互動」整合在同一個介面中，並透過 IPC 與 VS Code 擴充功能互通，讓你在編輯器與終端間快速來回。

English version: `README.en.md`

## 主要功能

### 1) 多分頁終端機工作區

- 支援多個終端分頁與分頁重新命名、排序。
- 每個分頁可綁定不同 shell profile（例如 Command Prompt、PowerShell、自訂命令）。
- 可管理啟動設定（預設 shell、語言、字型、工作區路徑）。

### 2) Prompt Composer 與附件注入

- 內建 prompt 輸入區，支援一般送出與可編輯填入模式。
- 可插入：
  - 檔案路徑
  - 圖片
  - `@` 工作區檔案選擇
- 支援 chip 管理（移除單一項目 / 清空）。

### 3) Interactive AI 命令整合

- 可管理 Gemini / Codex / Claude / Copilot 等互動指令（含自訂）。
- 支援 pinned footer 行數控制與快捷切換。
- Raw / 非 Raw 模式切換，平衡 TTY 直通與行內編輯能力。

### 4) IPC Bridge（桌面 App <-> VS Code）

- 內建 IPC server，讓外部工具可呼叫：
  - 開新分頁
  - 傳送 prompt
  - 填入 prompt
- IPC 狀態可視化（ON/OFF、client 數量、錯誤訊息）。

### 5) VS Code 擴充功能協作

- 在編輯器選取內容後，可直接：
  - Send Selection（直接送出）
  - Fill Selection（先填入可編輯）
- Explorer 多檔案右鍵可送出/填入檔案路徑。
- 提供快捷鍵與動態狀態列按鈕（有選取時顯示）。

### 6) 更新支援

- App 內提供「檢查更新 / 切換版本」介面（Windows ZIP 發版資產）。
- 更新流程包含版本選擇、下載、啟動新版與切換提示。

## 典型使用流程

1. 在 CliGJ 開啟目標工作區分頁。
2. 從 VS Code 選取程式碼，直接送到 CliGJ prompt。
3. 透過 AI 指令互動與終端機輸出往返調整。
4. 必要時插入檔案/圖片上下文，快速完成除錯或重構。

## 授權

- 本專案採用 `GPL-3.0-or-later`。
- 詳細授權條款請見根目錄 `LICENSE`。

