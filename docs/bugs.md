# 終端機分頁與顯示 Bug 修復計畫

## 問題描述

使用者回報四個相關的終端機顯示問題：
1. 開新分頁會沒畫面，且影響其他頁面
2. Resize 和滾輪造成訊息重複
3. 切換分頁再切回來有機率變白
4. Resize 後滾輪消失、訊息跑到上方被切割

## 根因分析

經過詳細程式碼審查，我找到以下五個互相關聯的根因：

### 根因 A：分頁切換時 Model 殘留舊資料（→ Bug 1, 3）

每個分頁有獨立的 `terminal_slint_model`（`Rc<VecModel<TermLine>>`），但 `load_tab_to_ui` 在切換時**直接綁定舊 model 而不清空**。舊 model 仍包含上一次顯示的 window 範圍資料。若新分頁的行數/scroll 不同，Slint 的 `for` 迴圈會用過時的 row 資料渲染，導致空白或殘影，直到 `push_terminal_view_to_ui` 重建完成。

```
// 問題碼 (ui_sync.rs:408-409)
ui.set_ws_terminal_lines(ModelRc::from(Rc::clone(&tab.terminal_slint_model)));
// ← model 裡還保留舊分頁的行資料
```

### 根因 B：`apply_scroll_top_px` 觸發冗餘的延遲 viewport-changed（→ Bug 2, 4）

Slint 函式 `apply_scroll_top_px` 內部呼叫 `root.viewport-changed()`。每次 Rust 設定 scroll 位置後，**也一定會直接呼叫 `push_terminal_view_to_ui`**。所以 `viewport-changed()` 觸發的延遲 handler 是多餘的。
- 延遲 handler 讀取的 scroll 值可能因 Slint 內部 clamp 而與直接呼叫不同
- 導致 model 被用不同的 window 參數重建兩次 → 短暫顯示錯誤內容

### 根因 C：Resize 風暴（→ Bug 2, 4）

`gj_viewer.slint` 的 `changed width` / `changed height` 對**每一像素**的變化都觸發 `terminal-resize-requested`。每次 resize：
1. PTY 送 `ControlCommand::Resize` → 讀取線程清除快取 → 全量重發所有行
2. 大量 chunk 湧入 → 大量 `apply_pending_updates` 和 model 重建
3. 中間狀態的行數和 scroll 會導致閃爍/重複

但實際上，1 像素的寬度變化通常不改變 cols/rows 格數，不需要 resize PTY。

### 根因 D：Model 更新遺漏 fallback（→ Bug 2）

在 `push_terminal_view_to_ui` 中，window 移動後逐行更新 model：
```rust
if let Some(row) = tab.terminal_model_rows.get(&line_idx) {
    model.set_row_data(model_idx, row.clone());
}
```

若 `terminal_model_rows` 缺少某行的快取（理論上不應發生，但邊界情況可能觸發），`set_row_data` 被跳過，**舊的 model entry 保留在位**。此舊 entry 來自不同的全域行索引 → 內容重複。

### 根因 E：`terminal_scroll_resync_next` 只在有新資料時處理（→ Bug 3, 4）

`refresh_current_terminal` 中，`terminal_scroll_resync_next` 只在 `current_changed == true` 時清除。若切換回一個靜默的分頁（無 PTY 資料到達），resync 永遠不執行，scroll 可能停留在錯誤位置。

---

## 提案修改

### 1. Slint 層：消除冗餘 viewport-changed 和 resize 風暴

#### [MODIFY] [gj_viewer.slint](file:///d:/Projects/CliGJ/ui/components/gj_viewer.slint)

**修改 A — 移除 `apply_scroll_top_px` 中的 `viewport-changed()` 呼叫**

```diff
 public function apply_scroll_top_px(top_px: float) {
     let ch: length = max(root.row-height, root.terminal-total-lines * root.row-height);
     let max_scroll: float = max(0.0, (ch / 1px) - (sv.height / 1px));
     let t: float = min(max(top_px, 0.0), max_scroll);
     sv.viewport-y = -(t * 1px);
-    root.viewport-changed();
 }
```

Rust 透過 `invoke_ws_apply_terminal_scroll_top_px` 設定 scroll 後，一定會緊接呼叫 `push_terminal_view_to_ui`，不需要多一次延遲的 viewport-changed handler。

**修改 B — 新增 grid 尺寸追蹤，避免無效 resize**

```diff
+private property <int> last-resize-cols: 0;
+private property <int> last-resize-rows: 0;
+
 function sync-terminal-grid-to-pty() {
     let cols = floor((self.width - root.terminal-left-inset - root.stripe-width) / root.char-width);
     let rows = floor(self.height / root.row-height);
-    if (cols > 0 && rows > 0) {
+    if (cols > 0 && rows > 0
+        && (cols != root.last-resize-cols || rows != root.last-resize-rows))
+    {
+        root.last-resize-cols = cols;
+        root.last-resize-rows = rows;
         root.terminal-resize-requested(cols, rows);
     }
 }
```

只有在 cols/rows 實際改變時才通知 PTY，避免每像素 resize 造成的全量重發。

---

### 2. Rust 層：model 同步和 scroll resync

#### [MODIFY] [ui_sync.rs](file:///d:/Projects/CliGJ/src/gui/ui_sync.rs)

**修改 A — `load_tab_to_ui` 清空 model 並重置 window 追蹤**

在綁定 model 和 push view 之前，清空 `terminal_slint_model` 並重置 `last_window_*`：

```diff
 pub(crate) fn load_tab_to_ui(ui: &AppWindow, tab: &mut TabState) {
     // ... 設定各種 UI 屬性 ...

     let n = tab.terminal_lines.len();
     ui.set_ws_terminal_total_lines(n as i32);
-    // 綁定持久 model
-    ui.set_ws_terminal_lines(ModelRc::from(Rc::clone(&tab.terminal_slint_model)));
+    // 清空舊 model 資料，強制 push_terminal_view_to_ui 完整重建
+    {
+        let model = &tab.terminal_slint_model;
+        while model.row_count() > 0 {
+            model.remove(0);
+        }
+    }
+    tab.last_window_first = usize::MAX;
+    tab.last_window_last = usize::MAX;
+    tab.last_window_total = usize::MAX;
+    ui.set_ws_terminal_lines(ModelRc::from(Rc::clone(&tab.terminal_slint_model)));

     ui.invoke_ws_bump_terminal_size();
     // ...
 }
```

**修改 B — `push_terminal_view_to_ui` 增加 fallback 防護**

在 model 更新迴圈中，若 `terminal_model_rows.get` 回傳 `None`，使用空白行取代跳過：

```diff
-if let Some(row) = tab.terminal_model_rows.get(&line_idx) {
-    model.set_row_data(model_idx, row.clone());
-}
+let row = tab.terminal_model_rows.get(&line_idx).cloned()
+    .unwrap_or_else(empty_term_line);
+model.set_row_data(model_idx, row);
```

此變更套用到所有三個 `set_row_data` 位置。

---

#### [MODIFY] [timers.rs](file:///d:/Projects/CliGJ/src/gui/run/timers.rs)

**修改 — `refresh_current_terminal` 在無新資料時也處理 scroll resync**

```diff
 fn refresh_current_terminal(ui: &AppWindow, s: &mut GuiState, current_changed: bool) {
     if s.current >= s.tabs.len() {
         return;
     }
     if current_changed {
         // ... 現有的 current_changed 邏輯 ...
         return;
     }

+    // 即使沒新資料，分頁切換後仍需 resync scroll
+    let cur = s.current;
+    let tab = &mut s.tabs[cur];
+    if tab.terminal_scroll_resync_next {
+        tab.terminal_scroll_resync_next = false;
+        let vh = ui.get_ws_terminal_viewport_height_px().max(1.0);
+        let exp = terminal_scroll_top_for_tab(tab, vh);
+        ui.invoke_ws_apply_terminal_scroll_top_px(exp);
+        push_terminal_view_to_ui(ui, tab, Some(exp));
+        tab.terminal_saved_scroll_top_px = ui.get_ws_terminal_scroll_top_px();
+        return;
+    }
+
     let st = ui.get_ws_terminal_scroll_top_px();
     let vh = ui.get_ws_terminal_viewport_height_px();
-    let cur = s.current;
-    let tab = &mut s.tabs[cur];
     // ... 現有的 scroll/viewport 差異檢測 ...
 }
```

---

## 變更摘要

| 檔案 | 修改 | 解決問題 |
|------|------|----------|
| `gj_viewer.slint` | 移除 `apply_scroll_top_px` 的 `viewport-changed()` | Bug 2, 4 |
| `gj_viewer.slint` | 新增 resize 去抖（cols/rows 追蹤） | Bug 2, 4 |
| `ui_sync.rs` | `load_tab_to_ui` 清空 model + 重置 window 追蹤 | Bug 1, 3 |
| `ui_sync.rs` | `push_terminal_view_to_ui` model 更新 fallback | Bug 2 |
| `timers.rs` | `refresh_current_terminal` 非 changed 路徑處理 resync | Bug 3, 4 |

## 驗證計畫

### 自動測試
- `cargo build` 確認編譯通過

### 手動驗證
1. **Bug 1**: 開啟新分頁 → 確認顯示 shell 輸出 → 切回舊分頁確認內容正常
2. **Bug 2**: 拖曳視窗邊緣 resize → 確認無訊息重複；滾動滾輪 → 確認無重複
3. **Bug 3**: 在分頁 1 執行命令 → 切到分頁 2 → 切回分頁 1 → 確認非空白
4. **Bug 4**: Resize 視窗 → 確認滾輪仍可用且訊息位置正確
