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
use serde_json::json;

use crate::gui::ipc::{IpcBridge, IpcGuiCommand, IpcGuiResponse};
use crate::gui::slint_ui::AppWindow;
use crate::gui::state::{GuiState, TabState, TerminalChunk, TerminalMode};
use crate::gui::ui_sync::{
    push_terminal_view_to_ui, scrollable_terminal_line_count, terminal_scroll_top_for_tab,
};
use crate::terminal::render::ColoredLine;

use super::helpers::{auto_disable_raw_on_cjk_prompt, inject_path_into_current};

const INTERACTIVE_TRAILING_BLANK_KEEP: usize = 1;

fn line_has_visible_text(line: &ColoredLine) -> bool {
    line.spans
        .iter()
        .any(|span| span.text.chars().any(|ch| !ch.is_whitespace()))
}

/// After each ConPTY snapshot, physical rows `[0, chunk_first)` are not in the slice. The old
/// code kept them as **blank leading rows** in `terminal_lines`, so as scrollback grew,
/// `len()` approached the full PTY height → huge black gap above the real output and O(n) work
/// (clear loop + Slint scroll extent). Drop that prefix and bump `terminal_physical_origin`
/// so the dense buffer only holds the snapshot tail (same idea as `enforce_scrollback_cap`).
fn compact_terminal_lines_after_snapshot(tab: &mut TabState, leading: usize) {
    if leading == 0 || tab.terminal_lines.is_empty() {
        return;
    }
    let drop_leading = tab
        .terminal_lines
        .iter()
        .take(leading.min(tab.terminal_lines.len()))
        .take_while(|line| line.blank && line.spans.is_empty())
        .count();
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
    set_auto_scroll: Option<bool>,
    replace_text: Option<String>,
    replace_lines: Option<Vec<ColoredLine>>,
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
        let keep_text = entry.replace_lines.as_ref().is_some_and(|l| l.is_empty()) && entry.changed_indices.is_empty();
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

    for (tab_id, update) in pending {
        let Some(&tab_idx) = tab_index_by_id.get(&tab_id) else {
            continue;
        };
        let tab = &mut state.tabs[tab_idx];
        if let Some(mode) = update.terminal_mode {
            if !(tab.terminal_mode == TerminalMode::InteractiveAi && mode == TerminalMode::Shell) {
                tab.terminal_mode = mode;
            }
        }
        if let Some(v) = update.set_auto_scroll {
            tab.auto_scroll = v;
        }

        let mut replaced_with_vt_lines = false;
        if let Some(mut new_lines) = update.replace_lines {
            let chunk_first_idx = update.first_line_idx.unwrap_or(0);
            let full_len_in_chunk = update.full_len.unwrap_or(0);
            let phys_end = chunk_first_idx + full_len_in_chunk;
            replaced_with_vt_lines = true;

            if update.reset_terminal_buffer {
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
            compact_terminal_lines_after_snapshot(tab, leading);

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
        IpcGuiCommand::SendPrompt {
            id,
            tab_id,
            prompt,
            submit,
            selection_payloads,
            file_path_payloads,
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
            if submit {
                ui.set_ws_prompt(SharedString::from(prompt.as_str()));
                s.tabs[cur].prompt = SharedString::from(prompt.as_str());
                s.tabs[cur].prompt_picked_selections = selection_payloads;
                s.tabs[cur].prompt_picked_files_abs = file_path_payloads;
            } else {
                let current_prompt = s.tabs[cur].prompt.to_string();
                let merged_prompt = if prompt.trim().is_empty() {
                    current_prompt
                } else {
                    crate::workspace_files::append_attachment_token(
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
                for path in file_path_payloads {
                    if !s.tabs[cur].prompt_picked_files_abs.iter().any(|p| p == &path) {
                        s.tabs[cur].prompt_picked_files_abs.push(path);
                    }
                }
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

    let app_weak = app.as_weak();
    let timer = Timer::default();
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
        
        if s.timer_prompt_snapshot.as_ref() == Some(&key) {
            return;
        }

        if !key.1.is_empty() {
            auto_disable_raw_on_cjk_prompt(&ui, &mut s);
        }
        
        sync_composer_line_to_conpty(&ui, &mut s);
        sync_at_file_picker(&ui, &mut s);
        
        if s.current < s.tabs.len() {
            s.timer_prompt_snapshot = Some(key);
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
