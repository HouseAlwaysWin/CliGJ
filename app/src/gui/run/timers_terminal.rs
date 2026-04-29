//! Terminal stream dispatcher and VT snapshot merge logic.

use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread;

use slint::{ComponentHandle, SharedString};

use crate::gui::ipc::IpcBridge;
use crate::gui::slint_ui::AppWindow;
use crate::gui::state::{GuiState, TERMINAL_SCROLLBACK_CAP, TabState, TerminalChunk, TerminalMode};
use crate::gui::ui_sync::{
    push_terminal_view_to_ui, scrollable_terminal_line_count, terminal_scroll_top_for_tab,
};
use cligj_terminal::render::ColoredLine;
use cligj_terminal::types::{RawPtyEvent, ResetReason};

const INTERACTIVE_TRAILING_BLANK_KEEP: usize = 1;

pub(super) fn line_has_visible_text(line: &ColoredLine) -> bool {
    line.spans
        .iter()
        .any(|span| span.text.chars().any(|ch| !ch.is_whitespace()))
}

pub(super) fn line_plain_text(line: &ColoredLine) -> String {
    let mut text = String::new();
    for span in &line.spans {
        text.push_str(span.text.as_str());
    }
    text
}

fn line_has_shell_preamble_marker(line: &ColoredLine) -> bool {
    let text = line_plain_text(line).to_ascii_lowercase();
    text.contains("microsoft windows") || text.contains("microsoft corporation")
}

fn line_has_interactive_ai_marker(line: &ColoredLine, markers: &[String]) -> bool {
    let text = line_plain_text(line).to_ascii_lowercase();
    markers.iter().any(|marker| {
        let marker = marker.trim().to_ascii_lowercase();
        !marker.is_empty() && text.contains(marker.as_str())
    })
}

pub(super) fn trim_or_drop_shell_preamble_snapshot(
    lines: &mut Vec<ColoredLine>,
    markers: &[String],
) -> bool {
    if markers.is_empty() {
        return false;
    }
    let Some(shell_marker_idx) = lines.iter().position(line_has_shell_preamble_marker) else {
        return false;
    };
    if let Some(ai_marker_idx) = lines
        .iter()
        .position(|line| line_has_interactive_ai_marker(line, markers))
    {
        if ai_marker_idx > shell_marker_idx {
            lines.drain(0..ai_marker_idx);
        }
        return false;
    }
    true
}

pub(super) fn reset_terminal_model_cache(tab: &mut TabState) {
    tab.terminal_model_rows.clear();
    tab.terminal_model_hashes.clear();
    tab.terminal_model_dirty.clear();
    tab.last_window_first = usize::MAX;
    tab.last_window_last = usize::MAX;
    tab.last_window_total = usize::MAX;
}

fn history_ends_with_block(history: &[ColoredLine], block: &[ColoredLine]) -> bool {
    history.len() >= block.len() && &history[history.len() - block.len()..] == block
}

fn longest_history_snapshot_overlap(history: &[ColoredLine], snapshot: &[ColoredLine]) -> usize {
    let max_overlap = history.len().min(snapshot.len());
    for overlap in (1..=max_overlap).rev() {
        if history[history.len() - overlap..] == snapshot[..overlap] {
            return overlap;
        }
    }
    0
}

fn longest_snapshot_prefix_seen(history: &[ColoredLine], snapshot: &[ColoredLine]) -> usize {
    if history.is_empty() || snapshot.is_empty() {
        return 0;
    }
    let mut best = 0usize;
    for start in 0..history.len() {
        let mut len = 0usize;
        while start + len < history.len()
            && len < snapshot.len()
            && history[start + len] == snapshot[len]
        {
            len += 1;
        }
        best = best.max(len);
        if best == snapshot.len() {
            break;
        }
    }
    best
}

fn append_interactive_snapshot_with_reflow_policy(
    tab: &mut TabState,
    lines: &[ColoredLine],
    replace_reflow_without_overlap: bool,
) {
    let snapshot: Vec<ColoredLine> = lines
        .iter()
        .filter(|line| line_has_visible_text(line))
        .cloned()
        .collect();
    if snapshot.is_empty() {
        return;
    }
    if history_ends_with_block(&tab.interactive_history_lines, &snapshot) {
        return;
    }

    let tail_overlap = longest_history_snapshot_overlap(&tab.interactive_history_lines, &snapshot);
    let seen_overlap = longest_snapshot_prefix_seen(&tab.interactive_history_lines, &snapshot);
    let overlap = tail_overlap.max(seen_overlap);
    // Preserve archived conversation even when a full-screen TUI redraw/reflow produces a snapshot
    // with no shared prefix. Clearing here made the history window drop earlier Codex replies.
    let _ = replace_reflow_without_overlap;
    tab.interactive_history_lines
        .extend(snapshot.into_iter().skip(overlap));

    if tab.interactive_history_lines.len() > TERMINAL_SCROLLBACK_CAP {
        let excess = tab.interactive_history_lines.len() - TERMINAL_SCROLLBACK_CAP;
        tab.interactive_history_lines.drain(0..excess);
    }
}

fn append_interactive_snapshot(tab: &mut TabState, lines: &[ColoredLine]) {
    append_interactive_snapshot_with_reflow_policy(tab, lines, true);
}

fn append_interactive_snapshot_preserving_history(tab: &mut TabState, lines: &[ColoredLine]) {
    append_interactive_snapshot_with_reflow_policy(tab, lines, false);
}

pub(super) fn append_interactive_history_block(tab: &mut TabState, lines: &[ColoredLine]) {
    append_interactive_snapshot(tab, lines);
}

pub(super) fn archive_dropped_interactive_prefix(tab: &mut TabState, new_origin: usize) {
    if new_origin <= tab.terminal_physical_origin {
        return;
    }
    let dropped_len =
        (new_origin - tab.terminal_physical_origin).min(tab.interactive_frame_lines.len());
    if dropped_len == 0 {
        tab.terminal_physical_origin = new_origin;
        return;
    }
    let dropped: Vec<ColoredLine> = tab
        .interactive_frame_lines
        .iter()
        .take(dropped_len)
        .cloned()
        .collect();
    append_interactive_history_block(tab, &dropped);
}

fn visible_interactive_lines(lines: &[ColoredLine]) -> Vec<ColoredLine> {
    lines
        .iter()
        .filter(|line| line_has_visible_text(line))
        .cloned()
        .collect()
}

fn interactive_frame_signature(lines: &[ColoredLine]) -> String {
    let mut signature = String::new();
    for line in lines.iter().filter(|line| line_has_visible_text(line)) {
        let text = line_plain_text(line);
        let trimmed = text.trim();
        if trimmed.is_empty() {
            continue;
        }
        if !signature.is_empty() {
            signature.push('\n');
        }
        signature.push_str(trimmed);
    }
    signature
}

fn longest_repainted_viewport_overlap(current: &[ColoredLine], next: &[ColoredLine]) -> usize {
    let max_overlap = current.len().min(next.len());
    for overlap in (3..=max_overlap).rev() {
        if current[current.len() - overlap..] == next[..overlap] {
            return overlap;
        }
    }
    0
}

fn archive_repainted_block_if_new(tab: &mut TabState, lines: &[ColoredLine]) {
    let signature = interactive_frame_signature(lines);
    if signature.is_empty() || signature == tab.interactive_last_archived_signature {
        return;
    }
    append_interactive_snapshot_preserving_history(tab, lines);
    tab.interactive_last_archived_signature = signature;
}

pub(super) fn maybe_archive_repainted_frame_before_replace(
    tab: &mut TabState,
    next_frame: &[ColoredLine],
) {
    if tab.interactive_frame_lines.is_empty()
        || next_frame.is_empty()
        || !tab.interactive_archive_repainted_frames
    {
        return;
    }

    let current_visible = visible_interactive_lines(&tab.interactive_frame_lines);
    let next_visible = visible_interactive_lines(next_frame);
    if current_visible.is_empty() || next_visible.is_empty() {
        return;
    }

    let viewport_overlap = longest_repainted_viewport_overlap(&current_visible, &next_visible);
    if viewport_overlap > 0 && viewport_overlap < current_visible.len() {
        let dropped_len = current_visible.len() - viewport_overlap;
        archive_repainted_block_if_new(tab, &current_visible[..dropped_len]);
    }
}

pub(super) fn snapshot_starts_mid_interactive_frame(
    lines: &[ColoredLine],
    markers: &[String],
) -> bool {
    let Some(first_visible) = lines
        .iter()
        .map(line_plain_text)
        .map(|text| text.trim().to_string())
        .find(|text| !text.is_empty())
    else {
        return false;
    };

    let has_interactive_footer = lines
        .iter()
        .any(|line| line_has_interactive_ai_marker(line, markers));

    has_interactive_footer
        && (first_visible.starts_with("│")
            || first_visible.starts_with("╰")
            || first_visible.contains("CLI Version")
            || first_visible.contains("Git Commit")
            || first_visible.contains("Sandbox")
            || first_visible.contains("Auth Method"))
}

pub(super) fn compose_interactive_terminal_lines(tab: &mut TabState) {
    let frame_end = tab
        .interactive_frame_lines
        .iter()
        .rposition(line_has_visible_text)
        .map(|idx| {
            (idx + 1 + INTERACTIVE_TRAILING_BLANK_KEEP).min(tab.interactive_frame_lines.len())
        })
        .unwrap_or(0);
    let frame_visible_block: Vec<ColoredLine> = tab
        .interactive_frame_lines
        .iter()
        .take(frame_end)
        .filter(|line| line_has_visible_text(line))
        .cloned()
        .collect();
    let frame_already_archived = !frame_visible_block.is_empty()
        && history_ends_with_block(&tab.interactive_history_lines, &frame_visible_block);

    tab.terminal_lines.clear();
    tab.terminal_lines
        .extend(tab.interactive_history_lines.iter().cloned());
    if !frame_already_archived {
        tab.terminal_lines
            .extend(tab.interactive_frame_lines.iter().take(frame_end).cloned());
    }

    if tab.terminal_lines.len() > TERMINAL_SCROLLBACK_CAP {
        let excess = tab.terminal_lines.len() - TERMINAL_SCROLLBACK_CAP;
        let hist_drop = excess.min(tab.interactive_history_lines.len());
        if hist_drop > 0 {
            tab.interactive_history_lines.drain(0..hist_drop);
        }
        tab.terminal_lines.drain(0..excess);
    }
    reset_terminal_model_cache(tab);
}

pub(super) fn compact_terminal_lines_after_snapshot(
    tab: &mut TabState,
    leading: usize,
    force: bool,
) {
    if leading == 0 || tab.terminal_lines.is_empty() {
        return;
    }
    let drop_leading = if force {
        leading.min(tab.terminal_lines.len())
    } else {
        tab.terminal_lines
            .iter()
            .take(leading.min(tab.terminal_lines.len()))
            .take_while(|line| line.blank && line.spans.is_empty())
            .count()
    };
    if drop_leading == 0 {
        return;
    }
    tab.terminal_lines.drain(0..drop_leading);
    tab.terminal_physical_origin = tab.terminal_physical_origin.saturating_add(drop_leading);

    let take_rows = std::mem::take(&mut tab.terminal_model_rows);
    tab.terminal_model_rows = take_rows
        .into_iter()
        .filter_map(|(k, v)| (k >= drop_leading).then(|| (k - drop_leading, v)))
        .collect();

    let take_hashes = std::mem::take(&mut tab.terminal_model_hashes);
    tab.terminal_model_hashes = take_hashes
        .into_iter()
        .filter_map(|(k, v)| (k >= drop_leading).then(|| (k - drop_leading, v)))
        .collect();

    let take_dirty = std::mem::take(&mut tab.terminal_model_dirty);
    tab.terminal_model_dirty = take_dirty
        .into_iter()
        .filter_map(|k| (k >= drop_leading).then(|| k - drop_leading))
        .collect();

    tab.last_window_first = usize::MAX;
    tab.last_window_last = usize::MAX;
    tab.last_window_total = usize::MAX;
}

#[derive(Default)]
struct PendingTabUpdate {
    terminal_mode: Option<TerminalMode>,
    raw_pty_events: Vec<RawPtyEvent>,
    set_auto_scroll: Option<bool>,
    replace_text: Option<String>,
    replace_lines: Option<Vec<ColoredLine>>,
    snapshot_len: Option<usize>,
    full_len: Option<usize>,
    first_line_idx: Option<usize>,
    cursor_row: Option<usize>,
    cursor_col: Option<usize>,
    changed_indices: Vec<usize>,
    append_text: String,
    reset_terminal_buffer: bool,
    reset_reason: Option<ResetReason>,
}

fn fold_chunk_into_pending(chunk: TerminalChunk, pending: &mut HashMap<u64, PendingTabUpdate>) {
    let entry = pending.entry(chunk.tab_id).or_default();
    entry.terminal_mode = Some(chunk.terminal_mode);
    entry.raw_pty_events.extend(chunk.raw_pty_events);
    if let Some(v) = chunk.set_auto_scroll {
        entry.set_auto_scroll = Some(v);
    }

    if chunk.replace {
        if chunk.reset_terminal_buffer {
            entry.reset_terminal_buffer = true;
            entry.reset_reason = chunk.reset_reason;
        }
        entry.cursor_row = chunk.cursor_row;
        entry.cursor_col = chunk.cursor_col;
        let mut lines = chunk.lines;
        let indices = chunk.changed_indices;

        if let Some(existing_lines) = entry.replace_lines.as_mut() {
            if entry.first_line_idx == Some(chunk.first_line_idx) {
                let existing_indices = &mut entry.changed_indices;
                for (i, &new_idx) in indices.iter().enumerate() {
                    if let Some(pos) = existing_indices.iter().position(|&x| x == new_idx) {
                        existing_lines[pos] = std::mem::take(&mut lines[i]);
                    } else {
                        existing_indices.push(new_idx);
                        existing_lines.push(std::mem::take(&mut lines[i]));
                    }
                }
            } else {
                entry.replace_lines = Some(lines);
                entry.changed_indices = indices;
                entry.first_line_idx = Some(chunk.first_line_idx);
            }
        } else {
            entry.replace_lines = Some(lines);
            entry.changed_indices = indices;
            entry.first_line_idx = Some(chunk.first_line_idx);
        }

        entry.full_len = Some(chunk.full_len);
        entry.snapshot_len = Some(chunk.snapshot_len);
        let keep_text = entry.replace_lines.as_ref().is_some_and(|l| l.is_empty())
            && entry.changed_indices.is_empty();
        entry.replace_text = if keep_text { Some(chunk.text) } else { None };
        entry.append_text.clear();
        return;
    }

    if let Some(text) = entry.replace_text.as_mut() {
        text.push_str(&chunk.text);
    } else {
        entry.append_text.push_str(&chunk.text);
    }
}

fn apply_replace_lines_update(
    tab: &mut TabState,
    update: &PendingTabUpdate,
    new_lines: Vec<ColoredLine>,
) {
    let chunk_first_idx = update.first_line_idx.unwrap_or(0);
    let snapshot_len = update.snapshot_len.unwrap_or_else(|| {
        update
            .changed_indices
            .iter()
            .copied()
            .max()
            .map(|idx| idx + 1)
            .unwrap_or(new_lines.len())
    });
    let mut phys_end = chunk_first_idx.saturating_add(snapshot_len);
    if let Some(full_len) = update.full_len {
        if full_len > 0 {
            phys_end = phys_end.min(full_len.max(chunk_first_idx));
        }
    }

    if tab.terminal_mode == TerminalMode::InteractiveAi {
        super::timers_terminal_interactive::apply_interactive_replace(
            tab,
            &update.changed_indices,
            update.reset_terminal_buffer,
            update.reset_reason,
            update.cursor_row,
            update.cursor_col,
            chunk_first_idx,
            phys_end,
            new_lines,
        );
    } else {
        super::timers_terminal_shell::apply_shell_replace(
            tab,
            &update.changed_indices,
            update.reset_terminal_buffer,
            update.reset_reason,
            update.cursor_row,
            update.cursor_col,
            chunk_first_idx,
            phys_end,
            new_lines,
        );
    }
}

fn apply_pending_updates(
    state: &mut GuiState,
    pending: HashMap<u64, PendingTabUpdate>,
    current_id: Option<u64>,
) -> bool {
    let mut current_changed = false;
    let mut tab_index_by_id = HashMap::with_capacity(state.tabs.len());
    for (idx, t) in state.tabs.iter().enumerate() {
        tab_index_by_id.insert(t.id, idx);
    }

    for (tab_id, mut update) in pending {
        let Some(&tab_idx) = tab_index_by_id.get(&tab_id) else {
            continue;
        };
        let tab = &mut state.tabs[tab_idx];
        tab.append_raw_pty_events(std::mem::take(&mut update.raw_pty_events));
        let stale_shell_update = tab.terminal_mode == TerminalMode::InteractiveAi
            && update.terminal_mode == Some(TerminalMode::Shell);
        if stale_shell_update {
            continue;
        }
        if let Some(mode) = update.terminal_mode {
            tab.terminal_mode = mode;
        }
        if let Some(v) = update.set_auto_scroll {
            tab.auto_scroll = v;
        }

        let mut replaced_with_vt_lines = false;
        if let Some(new_lines) = update.replace_lines.take() {
            replaced_with_vt_lines = true;
            apply_replace_lines_update(tab, &update, new_lines);
        }

        if !update.append_text.is_empty() && !replaced_with_vt_lines {
            tab.append_terminal(&update.append_text);
        }

        if current_id == Some(tab.id) {
            current_changed = true;
        }
    }

    current_changed
}

fn refresh_current_terminal(ui: &AppWindow, s: &mut GuiState, current_changed: bool) {
    if s.current >= s.tabs.len() {
        return;
    }
    if current_changed {
        let current = s.current;
        let tab = &mut s.tabs[current];
        if tab.terminal_lines.is_empty() {
            ui.set_ws_terminal_text(SharedString::from(tab.terminal_text.as_str()));
        }
        let vh = ui.get_ws_terminal_viewport_height_px().max(1.0);
        let exp = terminal_scroll_top_for_tab(tab, vh);
        let n = scrollable_terminal_line_count(tab);
        let interactive = tab.terminal_mode == TerminalMode::InteractiveAi;
        let interactive_follow = interactive && tab.interactive_follow_output;

        let resync = tab.terminal_scroll_resync_next;
        if resync {
            tab.terminal_scroll_resync_next = false;
        }

        if n > 0 && (tab.auto_scroll || interactive) {
            ui.set_ws_terminal_total_lines(n as i32);
        } else if n == 0 && interactive {
            ui.set_ws_terminal_total_lines(0);
        }

        let scroll_arg = if resync {
            Some(exp)
        } else if (tab.auto_scroll || interactive_follow) && n > 0 {
            Some(exp)
        } else {
            None
        };
        if let Some(s) = scroll_arg {
            ui.invoke_ws_apply_terminal_scroll_top_px(s);
            push_terminal_view_to_ui(ui, tab, Some(s));
        } else {
            push_terminal_view_to_ui(ui, tab, None);
        }
        tab.terminal_saved_scroll_top_px = ui.get_ws_terminal_scroll_top_px();
        return;
    }

    let cur = s.current;
    let tab = &mut s.tabs[cur];
    if tab.terminal_scroll_resync_next {
        tab.terminal_scroll_resync_next = false;
        let vh = ui.get_ws_terminal_viewport_height_px().max(1.0);
        let exp = terminal_scroll_top_for_tab(tab, vh);
        ui.invoke_ws_apply_terminal_scroll_top_px(exp);
        push_terminal_view_to_ui(ui, tab, Some(exp));
        tab.terminal_saved_scroll_top_px = ui.get_ws_terminal_scroll_top_px();
        return;
    }

    let st = ui.get_ws_terminal_scroll_top_px();
    let vh = ui.get_ws_terminal_viewport_height_px();
    if tab.terminal_mode == TerminalMode::InteractiveAi && tab.interactive_follow_output {
        let exp = terminal_scroll_top_for_tab(tab, vh.max(1.0));
        if (tab.last_pushed_scroll_top - exp).abs() > 0.5
            || (vh - tab.last_pushed_viewport_height).abs() > 0.5
        {
            ui.invoke_ws_apply_terminal_scroll_top_px(exp);
            push_terminal_view_to_ui(ui, tab, Some(exp));
            tab.terminal_saved_scroll_top_px = ui.get_ws_terminal_scroll_top_px();
        }
        return;
    }

    if (st - tab.last_pushed_scroll_top).abs() > 0.5
        || (vh - tab.last_pushed_viewport_height).abs() > 0.5
    {
        push_terminal_view_to_ui(ui, tab, None);
        tab.terminal_saved_scroll_top_px = ui.get_ws_terminal_scroll_top_px();
    }
}

fn flush_pending_scroll(ui: &AppWindow, s: &mut GuiState) {
    if !s.pending_scroll {
        return;
    }
    if s.current < s.tabs.len() {
        let cur = s.current;
        let tab = &mut s.tabs[cur];
        let n = scrollable_terminal_line_count(tab) as i32;
        ui.set_ws_terminal_total_lines(n);
        let vh = ui.get_ws_terminal_viewport_height_px().max(1.0);
        let scroll = terminal_scroll_top_for_tab(tab, vh);
        ui.invoke_ws_apply_terminal_scroll_top_px(scroll);
        push_terminal_view_to_ui(ui, tab, Some(scroll));
        tab.terminal_saved_scroll_top_px = ui.get_ws_terminal_scroll_top_px();
    }
    s.pending_scroll = false;
}

pub(crate) fn spawn_terminal_stream_dispatcher(
    app: &AppWindow,
    state: Rc<RefCell<GuiState>>,
    rx: mpsc::Receiver<TerminalChunk>,
    ipc: IpcBridge,
) -> thread::JoinHandle<()> {
    let queue: Arc<Mutex<VecDeque<TerminalChunk>>> = Arc::new(Mutex::new(VecDeque::new()));
    let wake_scheduled = Arc::new(AtomicBool::new(false));

    let queue_ui = Arc::clone(&queue);
    let scheduled_ui = Arc::clone(&wake_scheduled);
    let state_ui = Rc::clone(&state);
    let app_weak_ui = app.as_weak();
    app.on_terminal_data_ready(move || {
        scheduled_ui.store(false, Ordering::Release);
        let mut drained: Vec<TerminalChunk> = Vec::new();
        if let Ok(mut q) = queue_ui.lock() {
            drained.extend(q.drain(..));
        }
        if drained.is_empty() {
            return;
        }
        let Some(ui) = app_weak_ui.upgrade() else {
            return;
        };
        let mut s = state_ui.borrow_mut();
        let current_id = s.tabs.get(s.current).map(|t| t.id);
        let mut pending: HashMap<u64, PendingTabUpdate> = HashMap::new();
        for chunk in drained {
            fold_chunk_into_pending(chunk, &mut pending);
        }
        let current_changed = apply_pending_updates(&mut s, pending, current_id);
        refresh_current_terminal(&ui, &mut s, current_changed);
        flush_pending_scroll(&ui, &mut s);
    });

    let app_weak_bg = app.as_weak();
    thread::spawn(move || {
        loop {
            let first = match rx.recv() {
                Ok(c) => c,
                Err(_) => break,
            };
            ipc.publish_terminal_chunk(first.tab_id, first.text.as_str(), first.replace);
            if let Ok(mut q) = queue.lock() {
                q.push_back(first);
                const MAX_BATCH: usize = 256;
                while q.len() < MAX_BATCH {
                    let Ok(c) = rx.try_recv() else {
                        break;
                    };
                    ipc.publish_terminal_chunk(c.tab_id, c.text.as_str(), c.replace);
                    q.push_back(c);
                }
            } else {
                break;
            }

            if wake_scheduled.swap(true, Ordering::AcqRel) {
                continue;
            }
            if app_weak_bg
                .upgrade_in_event_loop(|ui| {
                    ui.invoke_terminal_data_ready();
                })
                .is_err()
            {
                wake_scheduled.store(false, Ordering::Release);
                break;
            }
        }
    })
}
