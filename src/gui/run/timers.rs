//! Slint timers/dispatchers: terminal stream, composer sync, startup injection.

use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::thread;
use std::time::Duration;

use slint::{ComponentHandle, SharedString, Timer};

use crate::gui::slint_ui::AppWindow;
use crate::gui::state::{GuiState, TerminalChunk};
use crate::gui::ui_sync::push_terminal_view_to_ui;
use crate::terminal::render::ColoredLine;

use super::helpers::{auto_disable_raw_on_cjk_prompt, inject_path_into_current};

#[derive(Default)]
struct PendingTabUpdate {
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
                tab.terminal_lines.clear();
                tab.terminal_model_rows.clear();
                tab.terminal_model_hashes.clear();
                tab.terminal_model_dirty.clear();
                tab.terminal_physical_origin = 0;
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

            // Physical rows [0..chunk_first_idx) are outside this snapshot — clear matching locals.
            let clear_local_end = chunk_first_idx.saturating_sub(origin);
            for i in 0..clear_local_end.min(tab.terminal_lines.len()) {
                tab.terminal_lines[i] = ColoredLine::default();
                tab.terminal_model_rows.remove(&i);
                tab.terminal_model_hashes.remove(&i);
                tab.terminal_model_dirty.insert(i);
            }

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

            tab.terminal_cursor_row = update.cursor_row.and_then(|phys| phys.checked_sub(origin));
            tab.terminal_cursor_col = update.cursor_col;

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
        let auto_scroll = s.tabs[current].auto_scroll;
        let tab = &mut s.tabs[current];
        if tab.terminal_lines.is_empty() {
            ui.set_ws_terminal_text(SharedString::from(tab.terminal_text.as_str()));
        }
        if auto_scroll {
            ui.invoke_ws_scroll_terminal_to_bottom();
        }
        push_terminal_view_to_ui(ui, tab);
        return;
    }

    let st = ui.get_ws_terminal_scroll_top_px();
    let vh = ui.get_ws_terminal_viewport_height_px();
    let cur = s.current;
    let tab = &mut s.tabs[cur];
    if (st - tab.last_pushed_scroll_top).abs() > 0.5
        || (vh - tab.last_pushed_viewport_height).abs() > 0.5
    {
        push_terminal_view_to_ui(ui, tab);
    }
}

fn flush_pending_scroll(ui: &AppWindow, s: &mut GuiState) {
    if !s.pending_scroll {
        return;
    }
    ui.invoke_ws_scroll_terminal_to_bottom();
    if s.current < s.tabs.len() {
        let cur = s.current;
        let tab = &mut s.tabs[cur];
        push_terminal_view_to_ui(ui, tab);
    }
    s.pending_scroll = false;
}

pub(crate) fn spawn_terminal_stream_dispatcher(
    app: &AppWindow,
    state: Rc<RefCell<GuiState>>,
    rx: mpsc::Receiver<TerminalChunk>,
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
            if let Ok(mut q) = queue.lock() {
                q.push_back(first);
                const MAX_BATCH: usize = 256;
                while q.len() < MAX_BATCH {
                    let Ok(c) = rx.try_recv() else {
                        break;
                    };
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
