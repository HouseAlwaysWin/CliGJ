//! Slint timers/dispatchers: terminal stream, composer sync, startup injection.

use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::thread;
use std::time::Duration;

use slint::{ComponentHandle, Model, SharedString, Timer};
#[cfg(target_os = "windows")]
use slint::winit_030::WinitWindowAccessor;
use serde_json::json;

use crate::gui::ipc::{IpcBridge, IpcGuiCommand, IpcGuiResponse};
use crate::gui::slint_ui::AppWindow;
use crate::gui::state::{GuiState, TabState, TerminalChunk, TerminalMode, TERMINAL_SCROLLBACK_CAP};
use crate::gui::ui_sync::{
    push_terminal_view_to_ui, scrollable_terminal_line_count, terminal_scroll_top_for_tab,
};
use cligj_terminal::types::RawPtyEvent;
use cligj_terminal::render::ColoredLine;

use super::helpers::{auto_disable_raw_on_cjk_prompt, inject_path_into_current};

const INTERACTIVE_TRAILING_BLANK_KEEP: usize = 1;
fn line_has_visible_text(line: &ColoredLine) -> bool {
    line.spans
        .iter()
        .any(|span| span.text.chars().any(|ch| !ch.is_whitespace()))
}

fn line_plain_text(line: &ColoredLine) -> String {
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

fn trim_or_drop_shell_preamble_snapshot(lines: &mut Vec<ColoredLine>, markers: &[String]) -> bool {
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

fn reset_terminal_model_cache(tab: &mut TabState) {
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
    if replace_reflow_without_overlap
        && overlap == 0
        && !tab.interactive_history_lines.is_empty()
        && snapshot.len() > 3
    {
        // Resize reflow can rewrite the same logical screen with different line breaks.
        // Exact line overlap fails in that case, so replace the stale-width archive instead
        // of appending the reflowed snapshot as duplicate content.
        tab.interactive_history_lines.clear();
    }
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

fn append_interactive_history_block(tab: &mut TabState, lines: &[ColoredLine]) {
    append_interactive_snapshot(tab, lines);
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

fn maybe_archive_repainted_frame_before_replace(tab: &mut TabState, next_frame: &[ColoredLine]) {
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

fn archive_dropped_interactive_prefix(tab: &mut TabState, new_origin: usize) {
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

fn snapshot_starts_mid_interactive_frame(lines: &[ColoredLine], markers: &[String]) -> bool {
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

fn compose_interactive_terminal_lines(tab: &mut TabState) {
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

/// After each ConPTY snapshot, physical rows `[0, chunk_first)` are not in the slice. The old
/// code kept them as **blank leading rows** in `terminal_lines`, so as scrollback grew,
/// `len()` approached the full PTY height → huge black gap above the real output and O(n) work
/// (clear loop + Slint scroll extent). Drop that prefix and bump `terminal_physical_origin`
/// so the dense buffer only holds the snapshot tail (same idea as `enforce_scrollback_cap`).
fn compact_terminal_lines_after_snapshot(tab: &mut TabState, leading: usize, force: bool) {
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
        // `then_some` evaluates its argument eagerly; `k - leading` must run only when `k >= leading`.
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
    /// Changed line indices from reader thread; empty = unknown, diff all.
    changed_indices: Vec<usize>,
    append_text: String,
    /// Any chunk asked to flush scrollback (e.g. PTY resize).
    reset_terminal_buffer: bool,
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
        let keep_text = entry
            .replace_lines
            .as_ref()
            .is_some_and(|l| l.is_empty())
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
        let stale_shell_update =
            tab.terminal_mode == TerminalMode::InteractiveAi
                && update.terminal_mode == Some(TerminalMode::Shell);
        if stale_shell_update {
            // The launcher command is submitted while the reader may still have Shell snapshots
            // queued. Once the UI has switched to InteractiveAi, those Shell frames are stale
            // pre-launch scrollback and must not be archived as interactive CLI output.
            continue;
        }
        if let Some(mode) = update.terminal_mode {
            tab.terminal_mode = mode;
        }
        if let Some(v) = update.set_auto_scroll {
            tab.auto_scroll = v;
        }

        let mut replaced_with_vt_lines = false;
        if let Some(mut new_lines) = update.replace_lines {
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
            replaced_with_vt_lines = true;

            if tab.terminal_mode == TerminalMode::InteractiveAi {
                if update.changed_indices.is_empty() && !new_lines.is_empty() {
                    let previous_terminal_lines = Some(tab.terminal_lines.clone());
                    let previous_cursor_row = tab.terminal_cursor_row;
                    let previous_cursor_col = tab.terminal_cursor_col;
                    let previous_origin = tab.terminal_physical_origin;
                    let mut snapshot_lines = std::mem::take(&mut new_lines);
                    let drop_shell_preamble_snapshot = trim_or_drop_shell_preamble_snapshot(
                        &mut snapshot_lines,
                        &tab.interactive_markers,
                    );
                    let frame_end = snapshot_lines
                        .iter()
                        .rposition(line_has_visible_text)
                        .map(|idx| {
                            (idx + 1 + INTERACTIVE_TRAILING_BLANK_KEEP)
                                .min(snapshot_lines.len())
                        })
                        .unwrap_or(0);
                    snapshot_lines.truncate(frame_end);
                    let restore_previous_resize_frame = update.reset_terminal_buffer
                        && snapshot_starts_mid_interactive_frame(
                            &snapshot_lines,
                            &tab.interactive_markers,
                        )
                        && previous_terminal_lines
                            .as_ref()
                            .is_some_and(|previous| !previous.is_empty());

                    if drop_shell_preamble_snapshot {
                        tab.interactive_frame_lines.clear();
                        tab.terminal_lines.clear();
                        reset_terminal_model_cache(tab);
                    } else if restore_previous_resize_frame {
                        if let Some(previous_terminal_lines) = previous_terminal_lines {
                            tab.terminal_lines = previous_terminal_lines;
                        }
                        tab.terminal_cursor_row = previous_cursor_row;
                        tab.terminal_cursor_col = previous_cursor_col;
                        tab.terminal_physical_origin = previous_origin;
                        reset_terminal_model_cache(tab);
                    } else if snapshot_lines.is_empty() {
                        if let Some(previous_terminal_lines) = previous_terminal_lines {
                            if !previous_terminal_lines.is_empty() {
                                tab.terminal_lines = previous_terminal_lines;
                            }
                        }
                    } else {
                        if update.reset_terminal_buffer {
                            // Resize snapshots are a reflowed baseline; old frame rows are stale.
                            if !tab.interactive_archive_repainted_frames {
                                tab.interactive_history_lines.clear();
                                tab.interactive_last_archived_signature.clear();
                            }
                            tab.interactive_frame_lines.clear();
                            tab.terminal_lines.clear();
                        } else {
                            maybe_archive_repainted_frame_before_replace(tab, &snapshot_lines);
                            archive_dropped_interactive_prefix(tab, chunk_first_idx);
                        }
                        tab.interactive_frame_lines = snapshot_lines;
                        compose_interactive_terminal_lines(tab);
                    }

                    if !restore_previous_resize_frame {
                        tab.terminal_physical_origin = chunk_first_idx;
                        tab.terminal_cursor_row = update
                            .cursor_row
                            .and_then(|phys| phys.checked_sub(chunk_first_idx))
                            .map(|row| tab.interactive_history_lines.len() + row);
                        tab.terminal_cursor_col = update.cursor_col;
                        if let Some(cursor_row) = tab.terminal_cursor_row {
                            if cursor_row >= tab.terminal_lines.len() {
                                tab.terminal_cursor_row = None;
                            }
                        }
                    }
                    reset_terminal_model_cache(tab);
                    tab.terminal_text.clear();
                } else {
                let previous_terminal_lines = if update.reset_terminal_buffer {
                    Some(tab.terminal_lines.clone())
                } else {
                    None
                };
                if update.reset_terminal_buffer {
                    // Resize rewrites/reflows the current screen. Do not archive the old frame;
                    // the next snapshot is the replacement baseline.
                    if !tab.interactive_archive_repainted_frames {
                        tab.interactive_history_lines.clear();
                        tab.interactive_last_archived_signature.clear();
                    }
                    tab.interactive_frame_lines.clear();
                    tab.terminal_lines.clear();
                    tab.terminal_physical_origin = chunk_first_idx;
                    tab.terminal_cursor_row = None;
                    tab.terminal_cursor_col = None;
                    reset_terminal_model_cache(tab);
                }

                if chunk_first_idx < tab.terminal_physical_origin {
                    let prepend = tab.terminal_physical_origin - chunk_first_idx;
                    let mut rebased = vec![ColoredLine::default(); prepend];
                    rebased.append(&mut tab.interactive_frame_lines);
                    tab.interactive_frame_lines = rebased;
                    tab.terminal_physical_origin = chunk_first_idx;
                    reset_terminal_model_cache(tab);
                }

                let origin = tab.terminal_physical_origin;
                let leading = chunk_first_idx.saturating_sub(origin);
                if leading > 0 {
                    let drop_leading = leading.min(tab.interactive_frame_lines.len());
                    if drop_leading > 0 && !update.reset_terminal_buffer {
                        let dropped: Vec<ColoredLine> = tab
                            .interactive_frame_lines
                            .iter()
                            .take(drop_leading)
                            .cloned()
                            .collect();
                        append_interactive_history_block(tab, &dropped);
                    }
                    tab.interactive_frame_lines.drain(0..drop_leading);
                    tab.terminal_physical_origin =
                        tab.terminal_physical_origin.saturating_add(drop_leading);
                    if leading > drop_leading {
                        tab.terminal_physical_origin = chunk_first_idx;
                    }
                }

                let origin = tab.terminal_physical_origin;
                let local_len = phys_end.saturating_sub(origin);
                if local_len > tab.interactive_frame_lines.len() {
                    tab.interactive_frame_lines
                        .resize(local_len, ColoredLine::default());
                }

                let indices = &update.changed_indices;
                if indices.is_empty() && !new_lines.is_empty() {
                    for i in 0..new_lines.len() {
                        let phys = chunk_first_idx + i;
                        let Some(local) = phys.checked_sub(origin) else {
                            continue;
                        };
                        if local < tab.interactive_frame_lines.len() {
                            tab.interactive_frame_lines[local] = std::mem::take(&mut new_lines[i]);
                        }
                    }
                } else {
                    for (delta_idx, &snapshot_idx) in indices.iter().enumerate() {
                        let phys = chunk_first_idx + snapshot_idx;
                        let Some(local) = phys.checked_sub(origin) else {
                            continue;
                        };
                        if local < tab.interactive_frame_lines.len() && delta_idx < new_lines.len()
                        {
                            tab.interactive_frame_lines[local] =
                                std::mem::take(&mut new_lines[delta_idx]);
                        }
                    }
                }

                let tail_keep = phys_end.saturating_sub(origin);
                if tab.interactive_frame_lines.len() > tail_keep {
                    tab.interactive_frame_lines.truncate(tail_keep);
                }

                let history_len = tab.interactive_history_lines.len();
                tab.terminal_cursor_row = update.cursor_row.and_then(|phys| {
                    phys.checked_sub(tab.terminal_physical_origin)
                        .map(|row| history_len + row)
                });
                tab.terminal_cursor_col = update.cursor_col;
                compose_interactive_terminal_lines(tab);
                if tab.terminal_lines.is_empty() {
                    if let Some(previous_terminal_lines) = previous_terminal_lines {
                        tab.terminal_lines = previous_terminal_lines;
                        reset_terminal_model_cache(tab);
                    }
                }
                if let Some(cursor_row) = tab.terminal_cursor_row {
                    if cursor_row >= tab.terminal_lines.len() {
                        tab.terminal_cursor_row = None;
                    }
                }
                tab.terminal_text.clear();
                }
            } else {
                if update.reset_terminal_buffer {
                tab.terminal_lines.clear();
                tab.terminal_physical_origin = chunk_first_idx;
                tab.terminal_cursor_row = None;
                tab.terminal_cursor_col = None;
                tab.terminal_model_rows.clear();
                tab.terminal_model_hashes.clear();
                tab.terminal_model_dirty.clear();
                tab.last_window_first = usize::MAX;
                tab.last_window_last = usize::MAX;
                tab.last_window_total = usize::MAX;
            }

            if chunk_first_idx < tab.terminal_physical_origin {
                let prepend = tab.terminal_physical_origin - chunk_first_idx;
                let mut rebased = vec![ColoredLine::default(); prepend];
                rebased.append(&mut tab.terminal_lines);
                tab.terminal_lines = rebased;
                tab.terminal_physical_origin = chunk_first_idx;
                tab.terminal_model_rows.clear();
                tab.terminal_model_hashes.clear();
                tab.terminal_model_dirty.clear();
                tab.last_window_first = usize::MAX;
                tab.last_window_last = usize::MAX;
                tab.last_window_total = usize::MAX;
            }

            let origin = tab.terminal_physical_origin;

            // Dense buffer: index `i` is physical row `origin + i`. Resize to fit through phys_end.
            let local_len = phys_end.saturating_sub(origin);
            if local_len > tab.terminal_lines.len() {
                tab.terminal_lines.resize(local_len, ColoredLine::default());
            }

            // Rows before `chunk_first_idx` are outside this snapshot; previously we filled them with
            // blanks (see `compact_terminal_lines_after_snapshot` doc). Merge first, then drop.

            let indices = &update.changed_indices;
            if indices.is_empty() && !new_lines.is_empty() {
                for i in 0..new_lines.len() {
                    let phys = chunk_first_idx + i;
                    let Some(local) = phys.checked_sub(origin) else {
                        continue;
                    };
                    if local < tab.terminal_lines.len() {
                        tab.terminal_lines[local] = std::mem::take(&mut new_lines[i]);
                        tab.terminal_model_rows.remove(&local);
                        tab.terminal_model_hashes.remove(&local);
                        tab.terminal_model_dirty.insert(local);
                    }
                }
            } else {
                for (delta_idx, &snapshot_idx) in indices.iter().enumerate() {
                    let phys = chunk_first_idx + snapshot_idx;
                    let Some(local) = phys.checked_sub(origin) else {
                        continue;
                    };
                    if local < tab.terminal_lines.len() && delta_idx < new_lines.len() {
                        tab.terminal_lines[local] = std::mem::take(&mut new_lines[delta_idx]);
                        tab.terminal_model_rows.remove(&local);
                        tab.terminal_model_hashes.remove(&local);
                        tab.terminal_model_dirty.insert(local);
                    }
                }
            }

            let tail_keep = phys_end.saturating_sub(origin);
            if tab.terminal_lines.len() > tail_keep {
                tab.terminal_lines.truncate(tail_keep);
                tab.terminal_model_rows.retain(|k, _| *k < tail_keep);
                tab.terminal_model_hashes.retain(|k, _| *k < tail_keep);
                tab.terminal_model_dirty.retain(|k| *k < tail_keep);
            }

            let leading = chunk_first_idx.saturating_sub(origin);
            let drop_snapshot_prefix =
                update.reset_terminal_buffer && tab.terminal_mode == TerminalMode::InteractiveAi;
            compact_terminal_lines_after_snapshot(tab, leading, drop_snapshot_prefix);

            tab.terminal_cursor_row = update
                .cursor_row
                .and_then(|phys| phys.checked_sub(tab.terminal_physical_origin));
            tab.terminal_cursor_col = update.cursor_col;

            // 裁掉游標以下的尾端空白行（PTY screen 的空白填充），
            // 避免 terminal_total_lines 過大導致新分頁出現不必要的滾輪。
            let cursor_local_end = tab.terminal_cursor_row.map(|r| r + 1).unwrap_or(0);
            let interactive_tail_end = tab
                .terminal_lines
                .iter()
                .rposition(line_has_visible_text)
                .map(|idx| (idx + 1 + INTERACTIVE_TRAILING_BLANK_KEEP).min(tab.terminal_lines.len()))
                .unwrap_or(0);
            let trim_floor = if tab.terminal_mode == TerminalMode::InteractiveAi {
                interactive_tail_end
            } else {
                cursor_local_end
            };
            while tab.terminal_lines.len() > trim_floor {
                if tab
                    .terminal_lines
                    .last()
                    .is_some_and(|line| !line_has_visible_text(line))
                {
                    tab.terminal_lines.pop();
                } else {
                    break;
                }
            }
            if let Some(cursor_row) = tab.terminal_cursor_row {
                if cursor_row >= tab.terminal_lines.len() {
                    tab.terminal_cursor_row = None;
                }
            }

            tab.enforce_scrollback_cap();
            tab.terminal_text.clear();
                }
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
            // Keep Slint scroll extent in sync when the buffer was cleared (e.g. ConPTY respawn).
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

    // 即使沒新資料，分頁切換後仍需 resync scroll
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
        // Apply each chunk in order. Batching multiple `replace` chunks into one map is unsafe:
        // if `first_line_idx` differs (scrollback moved between PTY renders), the later chunk
        // overwrote the earlier and most lines were dropped ("random" gaps / eaten text).
        let mut current_changed = false;
        for chunk in drained {
            let mut pending: HashMap<u64, PendingTabUpdate> = HashMap::new();
            fold_chunk_into_pending(chunk, &mut pending);
            current_changed |= apply_pending_updates(&mut s, pending, current_id);
        }
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

fn handle_ipc_gui_command(
    ui: &AppWindow,
    s: &mut GuiState,
    ipc: &IpcBridge,
    cmd: IpcGuiCommand,
) {
    match cmd {
        IpcGuiCommand::OpenTab {
            id,
            profile,
            focus,
            response_tx,
        } => {
            let mut out_id = id;
            if let Err(e) = s.add_tab(ui) {
                let _ = response_tx.send(IpcGuiResponse {
                    id: out_id,
                    ok: false,
                    result: json!({}),
                    error: Some(format!("openTab failed: {e}")),
                });
                return;
            }
            if let Some(profile) = profile {
                if !profile.trim().is_empty() {
                    let _ = s.change_current_cmd_type(profile.as_str(), ui);
                }
            }
            if s.current >= s.tabs.len() {
                let _ = response_tx.send(IpcGuiResponse {
                    id: out_id,
                    ok: false,
                    result: json!({}),
                    error: Some("openTab failed: no current tab".to_string()),
                });
                return;
            }
            let created_index = s.current;
            let tab = &s.tabs[created_index];
            let created_id = tab.id;
            let created_cmd_type = tab.cmd_type.clone();
            let created_title = s
                .titles
                .row_data(created_index)
                .unwrap_or_else(|| SharedString::from("Tab"))
                .to_string();
            if !focus && created_index > 0 {
                let _ = s.switch_tab(created_index - 1, ui);
            }
            ipc.publish_tab_changed(
                created_id,
                created_index,
                created_title,
                created_cmd_type.clone(),
            );
            let _ = response_tx.send(IpcGuiResponse {
                id: out_id.take(),
                ok: true,
                result: json!({
                    "tabId": created_id,
                    "tabIndex": created_index,
                    "cmdType": created_cmd_type,
                }),
                error: None,
            });
        }
        IpcGuiCommand::FocusWindow { id, response_tx } => {
            let mut out_id = id;
            #[cfg(target_os = "windows")]
            let focused = ui
                .window()
                .with_winit_window(|w| {
                    w.set_visible(true);
                    w.set_minimized(false);
                    w.focus_window();
                    true
                })
                .unwrap_or(false);
            #[cfg(not(target_os = "windows"))]
            let focused = {
                ui.window().request_activate();
                true
            };
            let _ = response_tx.send(IpcGuiResponse {
                id: out_id.take(),
                ok: focused,
                result: json!({ "focused": focused }),
                error: if focused {
                    None
                } else {
                    Some("focusWindow failed".to_string())
                },
            });
        }
        IpcGuiCommand::SendPrompt {
            id,
            tab_id,
            prompt,
            submit,
            selection_payloads,
            file_path_payloads,
            file_origin_payloads,
            response_tx,
        } => {
            let mut out_id = id;
            if let Some(target_id) = tab_id {
                if let Some(idx) = s.tabs.iter().position(|t| t.id == target_id) {
                    let _ = s.switch_tab(idx, ui);
                } else {
                    let _ = response_tx.send(IpcGuiResponse {
                        id: out_id.take(),
                        ok: false,
                        result: json!({}),
                        error: Some(format!("sendPrompt failed: tabId {target_id} not found")),
                    });
                    return;
                }
            }
            if s.current >= s.tabs.len() {
                let _ = response_tx.send(IpcGuiResponse {
                    id: out_id.take(),
                    ok: false,
                    result: json!({}),
                    error: Some("sendPrompt failed: no active tab".to_string()),
                });
                return;
            }
            let cur = s.current;
            let file_origin_payloads_converted: Vec<Option<crate::gui::state::PromptFileOrigin>> =
                file_origin_payloads
                    .into_iter()
                    .map(|origin| {
                        origin.map(|o| crate::gui::state::PromptFileOrigin {
                            client_id: o.client_id,
                            uri_scheme: o.uri_scheme,
                        })
                    })
                    .collect();
            if let Some(origin) = file_origin_payloads_converted
                .iter()
                .rev()
                .filter_map(|origin| origin.as_ref())
                .find(|origin| !origin.client_id.trim().is_empty() || !origin.uri_scheme.trim().is_empty())
                .cloned()
            {
                s.tabs[cur].prompt_last_file_origin = Some(origin);
            }
            if submit {
                ui.set_ws_prompt(SharedString::from(prompt.as_str()));
                s.tabs[cur].prompt = SharedString::from(prompt.as_str());
                s.tabs[cur].prompt_picked_selections = selection_payloads;
                s.tabs[cur].prompt_picked_files_abs = file_path_payloads;
                s.tabs[cur].prompt_picked_file_origins = file_origin_payloads_converted;
                while s.tabs[cur].prompt_picked_file_origins.len() < s.tabs[cur].prompt_picked_files_abs.len() {
                    s.tabs[cur].prompt_picked_file_origins.push(None);
                }
                crate::gui::ui_sync::sync_prompt_file_chips_to_ui(ui, &s.tabs[cur]);
            } else {
                let current_prompt = s.tabs[cur].prompt.to_string();
                let merged_prompt = if prompt.trim().is_empty() {
                    current_prompt
                } else {
                    cligj_workspace::append_attachment_token(
                        current_prompt.as_str(),
                        prompt.as_str(),
                    )
                };
                ui.set_ws_prompt(SharedString::from(merged_prompt.as_str()));
                s.tabs[cur].prompt = SharedString::from(merged_prompt.as_str());
                for payload in selection_payloads {
                    if !s.tabs[cur].prompt_picked_selections.iter().any(|p| p == &payload) {
                        s.tabs[cur].prompt_picked_selections.push(payload);
                    }
                }
                for (path, origin) in file_path_payloads.into_iter().zip(
                    file_origin_payloads_converted
                        .into_iter()
                        .chain(std::iter::repeat(None)),
                ) {
                    if !s.tabs[cur].prompt_picked_files_abs.iter().any(|p| p == &path) {
                        s.tabs[cur].prompt_picked_files_abs.push(path);
                        s.tabs[cur].prompt_picked_file_origins.push(origin);
                    }
                }
                crate::gui::ui_sync::sync_prompt_file_chips_to_ui(ui, &s.tabs[cur]);
            }
            if submit {
                if let Err(e) = s.submit_current_prompt(ui) {
                    let _ = response_tx.send(IpcGuiResponse {
                        id: out_id.take(),
                        ok: false,
                        result: json!({}),
                        error: Some(format!("sendPrompt failed: {e}")),
                    });
                    return;
                }
            } else {
                use crate::gui::composer_sync::sync_composer_line_to_conpty;
                sync_composer_line_to_conpty(ui, s);
            }
            let tab = &s.tabs[s.current];
            let _ = response_tx.send(IpcGuiResponse {
                id: out_id.take(),
                ok: true,
                result: json!({
                    "tabId": tab.id,
                    "submitted": submit
                }),
                error: None,
            });
        }
    }
}

pub(crate) fn spawn_ipc_bridge_timer(
    app: &AppWindow,
    state: Rc<RefCell<GuiState>>,
    ipc: IpcBridge,
    ipc_rx: mpsc::Receiver<IpcGuiCommand>,
) -> Timer {
    let app_weak = app.as_weak();
    let timer = Timer::default();
    let mut startup_retry_pending = true;
    timer.start(slint::TimerMode::Repeated, Duration::from_millis(40), move || {
        let Some(ui) = app_weak.upgrade() else {
            return;
        };

        if startup_retry_pending {
            let snap0 = ipc.snapshot();
            if !snap0.running {
                let _ = ipc.start();
            }
            startup_retry_pending = false;
        }

        let snap = ipc.snapshot();
        ui.set_ws_ipc_running(snap.running);
        ui.set_ws_ipc_client_count(snap.client_count as i32);
        let status_text = if snap.running {
            format!("IPC ON ({})", snap.client_count)
        } else if !snap.last_error.trim().is_empty() {
            format!("IPC OFF ({})", snap.last_error)
        } else {
            "IPC OFF".to_string()
        };
        ui.set_ws_ipc_status_text(SharedString::from(status_text.as_str()));

        let mut pending: Vec<IpcGuiCommand> = Vec::new();
        while let Ok(cmd) = ipc_rx.try_recv() {
            pending.push(cmd);
            if pending.len() >= 16 {
                break;
            }
        }
        if pending.is_empty() {
            return;
        }
        let mut s = state.borrow_mut();
        for cmd in pending {
            handle_ipc_gui_command(&ui, &mut s, &ipc, cmd);
        }
    });
    timer
}

pub(crate) fn spawn_composer_at_sync_timer(app: &AppWindow, state: Rc<RefCell<GuiState>>) -> Timer {
    use crate::gui::at_picker::sync_at_file_picker;
    use crate::gui::composer_sync::sync_composer_line_to_conpty;
    use crate::gui::ui_sync::tab_update_from_ui;

    let app_weak = app.as_weak();
    let timer = Timer::default();
    const PROMPT_UNDO_CAP: usize = 200;
    timer.start(slint::TimerMode::Repeated, Duration::from_millis(20), move || {
        let Some(ui) = app_weak.upgrade() else {
            return;
        };
        let mut s = state.borrow_mut();
        if s.current >= s.tabs.len() {
            return;
        }
        
        let prompt_now = ui.get_ws_prompt().to_string();
        let raw = ui.get_ws_raw_input();
        let key = (s.current, prompt_now, raw);
        
        if s.timer_snapshot.as_ref() == Some(&key) {
            return;
        }

        if !key.1.is_empty() {
            auto_disable_raw_on_cjk_prompt(&ui, &mut s);
        }
        // Keep tab prompt + attachment chips synchronized on every real composer change.
        let cur = s.current;
        let old_prompt = s.tabs[cur].prompt.to_string();
        let new_prompt = key.1.clone();
        if old_prompt != new_prompt {
            let tab = &mut s.tabs[cur];
            tab.prompt_undo_stack.push(old_prompt);
            if tab.prompt_undo_stack.len() > PROMPT_UNDO_CAP {
                let overflow = tab.prompt_undo_stack.len() - PROMPT_UNDO_CAP;
                tab.prompt_undo_stack.drain(0..overflow);
            }
            tab.prompt_redo_stack.clear();
        }
        tab_update_from_ui(&mut s.tabs[cur], &ui);
        
        sync_composer_line_to_conpty(&ui, &mut s);
        sync_at_file_picker(&ui, &mut s);
        
        if s.current < s.tabs.len() {
            s.timer_snapshot = Some(key);
        }
    });
    timer
}

pub(crate) fn spawn_inject_startup_timer(
    app: &AppWindow,
    state: Rc<RefCell<GuiState>>,
    path: PathBuf,
) -> Timer {
    let app_weak = app.as_weak();
    let timer = Timer::default();
    timer.start(slint::TimerMode::SingleShot, Duration::from_millis(500), move || {
        let Some(ui) = app_weak.upgrade() else {
            return;
        };
        let mut s = state.borrow_mut();
        if let Err(e) = inject_path_into_current(&ui, &mut s, path.as_path()) {
            eprintln!("CliGJ: --inject-file {}: {e}", path.display());
        }
    });
    timer
}

#[cfg(test)]
mod tests {
    use super::*;
    use cligj_terminal::render::ColoredSpan;

    fn line(text: &str) -> ColoredLine {
        ColoredLine {
            blank: false,
            spans: vec![ColoredSpan {
                text: text.to_string(),
                fg: [240, 240, 240],
                bg: [18, 18, 18],
            }],
        }
    }

    #[test]
    fn codex_snapshot_with_shell_preamble_is_trimmed_not_dropped() {
        let mut lines = vec![
            line("Microsoft Windows [Version 10.0.19045]"),
            line("(c) Microsoft Corporation."),
            line("D:\\Projects\\CliGJ>codex"),
            line(">_ OpenAI Codex (v0.123.0)"),
            line("model: gpt-5.4 xhigh /model to change"),
        ];
        let markers = vec!["openai codex".to_string(), "/model to change".to_string()];

        assert!(!trim_or_drop_shell_preamble_snapshot(
            &mut lines,
            &markers
        ));
        assert_eq!(line_plain_text(&lines[0]), ">_ OpenAI Codex (v0.123.0)");
    }

    #[test]
    fn repainted_frame_transition_without_scroll_does_not_archive() {
        let (tx, _rx) = std::sync::mpsc::channel();
        let mut tab = TabState::new(1, tx, None);
        tab.terminal_mode = TerminalMode::InteractiveAi;
        tab.interactive_launcher_program = "codex".to_string();
        tab.interactive_archive_repainted_frames = true;
        tab.interactive_frame_lines = vec![
            line("╭────────────────────────╮"),
            line("│  >_ OpenAI Codex       │"),
            line("│  Welcome               │"),
            line("\u{203a} Use /skills to list available skills"),
        ];
        let status = vec![
            line("/status"),
            line("╭────────────────────────╮"),
            line("│  >_ OpenAI Codex       │"),
            line("│  Model: gpt-5.4        │"),
            line("\u{203a} Use /skills to list available skills"),
        ];

        maybe_archive_repainted_frame_before_replace(&mut tab, &status);
        assert!(tab.interactive_history_lines.is_empty());
    }

    #[test]
    fn repainted_footer_typing_does_not_archive_each_prompt_edit() {
        let (tx, _rx) = std::sync::mpsc::channel();
        let mut tab = TabState::new(1, tx, None);
        tab.terminal_mode = TerminalMode::InteractiveAi;
        tab.interactive_launcher_program = "codex".to_string();
        tab.interactive_archive_repainted_frames = true;
        tab.interactive_frame_lines = vec![
            line("│  >_ OpenAI Codex       │"),
            line("\u{203a} /st"),
        ];
        let next = vec![
            line("│  >_ OpenAI Codex       │"),
            line("\u{203a} /sta"),
        ];

        maybe_archive_repainted_frame_before_replace(&mut tab, &next);
        assert!(tab.interactive_history_lines.is_empty());
    }
}
