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
    /// Changed line indices from reader thread; empty = unknown, diff all.
    changed_indices: Vec<usize>,
    append_text: String,
}

fn fold_chunk_into_pending(chunk: TerminalChunk, pending: &mut HashMap<u64, PendingTabUpdate>) {
    let entry = pending.entry(chunk.tab_id).or_default();
    if let Some(v) = chunk.set_auto_scroll {
        entry.set_auto_scroll = Some(v);
    }

    if chunk.replace {
        let mut lines = chunk.lines;
        let indices = chunk.changed_indices;
        
        if let Some(existing_lines) = entry.replace_lines.as_mut() {
            // 合併邏輯：將新變動的行插入到現有的待處理清單中
            let existing_indices = &mut entry.changed_indices;
            for (i, &new_idx) in indices.iter().enumerate() {
                if let Some(pos) = existing_indices.iter().position(|&x| x == new_idx) {
                    // 已存在該索引的變動，更新它
                    existing_lines[pos] = std::mem::take(&mut lines[i]);
                } else {
                    // 新的變動索引
                    existing_indices.push(new_idx);
                    existing_lines.push(std::mem::take(&mut lines[i]));
                }
            }
        } else {
            entry.replace_lines = Some(lines);
            entry.changed_indices = indices;
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
            let full_len = update.full_len.unwrap_or(0);
            replaced_with_vt_lines = full_len > 0;
            if replaced_with_vt_lines {
                let old_len = tab.terminal_lines.len();

                // 處理長度變化
                if full_len > old_len {
                    // 擴展
                    tab.terminal_lines.resize(full_len, ColoredLine::default());
                    for i in old_len..full_len {
                        tab.terminal_model_dirty.insert(i);
                    }
                } else if full_len < old_len {
                    // 縮減
                    tab.terminal_lines.truncate(full_len);
                    tab.terminal_model_rows.retain(|&k, _| k < full_len);
                    tab.terminal_model_hashes.retain(|&k, _| k < full_len);
                    tab.terminal_model_dirty.retain(|&k| k < full_len);
                }

                // 用 changed_indices 直接 swap 有變化的行
                let indices = &update.changed_indices;
                if indices.is_empty() && full_len == old_len && !new_lines.is_empty() {
                    // 回退機制：如果沒有 indices 但有 lines，且長度未變，視為全量更新（舊邏輯相容性）
                    for i in 0..full_len {
                        if i < new_lines.len() {
                            std::mem::swap(&mut tab.terminal_lines[i], &mut new_lines[i]);
                            tab.terminal_model_rows.remove(&i);
                            tab.terminal_model_hashes.remove(&i);
                            tab.terminal_model_dirty.insert(i);
                        }
                    }
                } else {
                    // Delta 更新
                    for (delta_idx, &real_idx) in indices.iter().enumerate() {
                        if real_idx < full_len && delta_idx < new_lines.len() {
                            std::mem::swap(&mut tab.terminal_lines[real_idx], &mut new_lines[delta_idx]);
                            tab.terminal_model_rows.remove(&real_idx);
                            tab.terminal_model_hashes.remove(&real_idx);
                            tab.terminal_model_dirty.insert(real_idx);
                        }
                    }
                }

                tab.terminal_text.clear();
            } else if update.full_len.is_some() {
                // full_len 為 0 且 replace 模式
                tab.terminal_lines.clear();
                tab.terminal_model_rows.clear();
                tab.terminal_model_hashes.clear();
                tab.terminal_model_dirty.clear();
                tab.terminal_text = update.replace_text.unwrap_or_default();
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

/// Event-driven terminal updates:
/// - background thread blocks on `rx.recv()`
/// - UI work is scheduled only when new chunks arrive.
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

/// Composer → ConPTY mirror and `@` file picker refresh.
pub(crate) fn spawn_composer_at_sync_timer(app: &AppWindow, state: Rc<RefCell<GuiState>>) -> Timer {
    use crate::gui::at_picker::sync_at_file_picker;
    use crate::gui::composer_sync::sync_composer_line_to_conpty;

    let app_weak = app.as_weak();
    let timer = Timer::default();
    timer.start(slint::TimerMode::Repeated, Duration::from_millis(90), move || {
        let Some(ui) = app_weak.upgrade() else {
            return;
        };
        let mut s = state.borrow_mut();
        if s.current >= s.tabs.len() {
            return;
        }
        if ui.get_ws_raw_input() {
            // Raw 模式下只在 prompt 非空時才做 CJK 檢查，避免無謂開銷
            let prompt = ui.get_ws_prompt();
            if !prompt.is_empty() {
                auto_disable_raw_on_cjk_prompt(&ui, &mut s);
            }
            return;
        }
        let prompt_now = ui.get_ws_prompt().to_string();
        let raw = ui.get_ws_raw_input();
        let key = (s.current, prompt_now, raw);
        if s.timer_prompt_snapshot.as_ref() == Some(&key) {
            return;
        }
        auto_disable_raw_on_cjk_prompt(&ui, &mut s);
        sync_composer_line_to_conpty(&ui, &mut s);
        sync_at_file_picker(&ui, &mut s);
        if s.current < s.tabs.len() {
            s.timer_prompt_snapshot = Some((s.current, ui.get_ws_prompt().to_string(), ui.get_ws_raw_input()));
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
