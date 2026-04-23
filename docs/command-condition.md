 現在流程是：

  1. 打開「指令設定」
  2. 按「新增」
  3. 填：
      - name: 顯示名稱
      - command: 例如 my-ai --tui
      - Pin: 固定底部行數，通常 0
      - Markers: 用逗號分隔，例如 My AI, Ready for input
      - Replay: 如果這個 CLI 像 Codex 一樣重畫畫面、不產生 scrollback，就打開

  判斷規則是：

  - command 的第一個 token 會用來判斷是不是 interactive CLI。
    例如 UI 裡加 my-ai --tui，之後你手動輸入 my-ai --model x 也會被當成 interactive CLI。
  - Markers 用來判斷進入 CLI 後，哪裡開始是真正的 CLI 畫面。
  - Replay 對應 archive_repainted_frames，用來保留重畫型 TUI 的歷史。