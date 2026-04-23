# Fix Terminal Message Duplication & Layout Corruption on Resize / Interactive AI Entry

## Problem Summary

When the terminal viewport is resized or when entering an interactive AI CLI (e.g. Gemini CLI), the terminal output frequently shows:
1. **Duplicated messages** — the same content appears multiple times in the scrollback
2. **Layout corruption (跑版)** — lines break incorrectly or overlap
3. **Width sensitivity** — narrower windows exacerbate both issues

## Root Cause Analysis

After thorough code review, I identified **three interrelated root causes**:

### 1. Interactive AI Snapshot Duplication During Resize (Primary)

In `windows_conpty.rs`, when a resize occurs:
- `line_cache` is cleared and `pending_reset = true` is set (line 488-491)
- A settle deadline is set so the next snapshot is delayed (line 492-493)
- The `interactive_snapshot_floor` is recalculated to `Viewport` mode (line 494-496)

However, when the settle deadline expires and the next full snapshot arrives with `reset_terminal_buffer = true`, the GUI-side code in `timers.rs` (line 396-409 in the InteractiveAi branch) does this:

```rust
if update.reset_terminal_buffer {
    let previous_frame = tab.interactive_frame_lines.clone();
    append_interactive_history_block(tab, &previous_frame);  // ← ARCHIVES the current frame
    tab.interactive_frame_lines.clear();
    ...
}
```

**The problem**: The current frame is archived into `interactive_history_lines`, but then the new full snapshot (which contains the SAME content reflowed for the new width) is also processed and appended — resulting in the same content appearing twice (once in history, once in the new frame).

### 2. Overlap Detection Fails After Reflow

The `longest_history_snapshot_overlap` and `longest_snapshot_prefix_seen` functions (lines 82-111) compare lines by exact `ColoredLine` equality. After a resize, wezterm-term **reflows** text to the new column width, causing:
- Lines that were `"Hello World"` at 80 cols might become `"Hello"` + `"World"` at 40 cols
- The overlap detection finds 0 overlap because the reflowed text doesn't match the archived history
- Result: the full reflowed content is appended again as "new" content

### 3. `compose_interactive_terminal_lines` Always Appends Frame to History

`compose_interactive_terminal_lines` (line 142-178) constructs `terminal_lines` from `history + frame`. When `frame_already_archived` is false (which happens after reflow because the fingerprints don't match), it appends the frame again:

```rust
if !frame_already_archived {
    tab.terminal_lines.extend(tab.interactive_frame_lines.iter().take(frame_end).cloned());
}
```

This is correct for normal operation but wrong when the resize path has already partially archived the frame.

## Proposed Changes

### [MODIFY] [windows_conpty.rs](file:///d:/DotNetProjects/CliGJ/src/terminal/windows_conpty.rs)

**Change 1: Clear history on resize in InteractiveAi mode**

When a resize occurs in `InteractiveAi` mode, the entire wezterm-term buffer is reflowed. The old `interactive_snapshot_floor` (which was set based on pre-reflow physical rows) is now stale. We should reset it to re-anchor the view:

- After resize, when `InteractiveFloorReset::Viewport` is applied, use `total.saturating_sub(term_rows)` as the floor — this is already correct.
- No changes needed here; the floor calculation is fine.

**Change 2: Send a full snapshot after resize settles (not incremental)**

After a resize settle, the reader already sends a full snapshot with `changed_indices` empty (because `line_cache` was cleared). This part is correct. No changes needed.

### [MODIFY] [timers.rs](file:///d:/DotNetProjects/CliGJ/src/gui/run/timers.rs)

**Change 1: On `reset_terminal_buffer` in InteractiveAi mode, discard history instead of archiving**

When `reset_terminal_buffer` is true (resize), we should NOT archive the current frame into history because:
- The frame content is about to be replaced by a reflowed version from wezterm-term
- Archiving it creates duplicates since the new snapshot covers the same logical content

```diff
 if update.reset_terminal_buffer {
-    let previous_frame = tab.interactive_frame_lines.clone();
-    append_interactive_history_block(tab, &previous_frame);
+    // On resize: history is stale (reflowed). Clear both history and frame
+    // so the next full snapshot starts fresh.
+    tab.interactive_history_lines.clear();
     tab.interactive_frame_lines.clear();
     tab.terminal_physical_origin = chunk_first_idx;
     tab.terminal_cursor_row = None;
     tab.terminal_cursor_col = None;
     reset_terminal_model_cache(tab);
 }
```

**Change 2: On `reset_terminal_buffer` in the full-snapshot (empty changed_indices) InteractiveAi path, also clear history**

The first InteractiveAi branch (line 355-393, where `changed_indices.is_empty() && !new_lines.is_empty()`) handles full snapshots. When `reset_terminal_buffer` is asserted, we should also reset history before processing:

```diff
 if update.changed_indices.is_empty() && !new_lines.is_empty() {
-    let previous_terminal_lines = tab.terminal_lines.clone();
+    let previous_terminal_lines = if update.reset_terminal_buffer {
+        // Resize: flush stale history. The full snapshot will be the
+        // new baseline. Keep a copy only as last-resort fallback.
+        tab.interactive_history_lines.clear();
+        tab.interactive_frame_lines.clear();
+        Some(tab.terminal_lines.clone())
+    } else {
+        Some(tab.terminal_lines.clone())
+    };
     let mut snapshot_lines = std::mem::take(&mut new_lines);
     ...
```

Also ensure the fallback uses `previous_terminal_lines` correctly (it's now always `Some`).

**Change 3: In `append_interactive_snapshot`, add a heuristic for "resize-caused total replacement"**

When the snapshot is much longer than the history overlap, and the history tail doesn't overlap at all, it means the content was reflowed. In this case, replace the history entirely:

```diff
 fn append_interactive_snapshot(tab: &mut TabState, lines: &[ColoredLine]) {
     ...
     let tail_overlap = longest_history_snapshot_overlap(&tab.interactive_history_lines, &snapshot);
     let seen_overlap = longest_snapshot_prefix_seen(&tab.interactive_history_lines, &snapshot);
     let overlap = tail_overlap.max(seen_overlap);
+    // If no overlap detected at all and we have history, the content was likely reflowed.
+    // Replace history entirely to avoid duplication.
+    if overlap == 0 && !tab.interactive_history_lines.is_empty() && snapshot.len() > 3 {
+        tab.interactive_history_lines.clear();
+    }
     tab.interactive_history_lines
         .extend(snapshot.into_iter().skip(overlap));
     ...
 }
```

## Open Questions

> [!IMPORTANT]
> The main trade-off is: on resize, do we want to preserve scrollback history from before the resize, or show a clean view? 
> 
> Since wezterm-term reflows content, the old history (at old column width) would look broken if kept alongside the new reflowed content. **I recommend clearing history on resize** for InteractiveAi mode, which matches what Gemini CLI itself does (it repaints the viewport on resize).

> [!NOTE]
> For Shell mode (non-interactive), the existing logic already handles resize correctly because it doesn't use the `interactive_history_lines` / `interactive_frame_lines` architecture — it just replaces `terminal_lines` directly from the snapshot.

## Verification Plan

### Manual Testing
1. Open CliGJ, enter `gemini` in a tab
2. After Gemini CLI loads, type a message and get a response
3. Resize the window (drag edge) — verify no duplicated messages
4. Maximize/restore — verify no duplicated messages
5. Test at different narrow widths (e.g. 60 cols, 40 cols) — verify layout integrity
6. Switch between tabs during resize — verify no cross-tab corruption

### Build Verification
```bash
cargo build --release
```
