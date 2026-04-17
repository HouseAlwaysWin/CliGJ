//! Slint timers/dispatchers: terminal stream, composer sync, startup injection.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet, VecDeque};
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
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
    append_text: String,
}

fn fold_chunk_into_pending(chunk: TerminalChunk, pending: &mut HashMap<u64, PendingTabUpdate>) {
    let entry = pending.entry(chunk.tab_id).or_default();
    if let Some(v) = chunk.set_auto_scroll {
        entry.set_auto_scroll = Some(v);
    }

    if chunk.replace {
        let lines = chunk.lines;
        let keep_text = lines.is_empty();
        entry.replace_lines = Some(lines);
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

/// ColoredLine fingerprint (same as ui_sync::line_fingerprint but usable here).
fn colored_line_fingerprint(line: &ColoredLine) -> u64 {
    let mut h = DefaultHasher::new();
    line.blank.hash(&mut h);
    line.spans.len().hash(&mut h);
    for span in &line.spans {
        span.text.hash(&mut h);
        span.fg.hash(&mut h);
        span.bg.hash(&mut h);
    }
    h.finish()
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
        if let Some(new_lines) = update.replace_lines {
            replaced_with_vt_lines = !new_lines.is_empty();
            if replaced_with_vt_lines {
                // Per-line diff: 只替換有變化的行，避免全量重建
                let old_len = tab.terminal_lines.len();
                let new_len = new_lines.len();
                let mut dirty_indices: HashSet<usize> = HashSet::new();

                // 比對共同範圍內的行
                let common = old_len.min(new_len);
                for i in 0..common {
                    let old_fp = colored_line_fingerprint(&tab.terminal_lines[i]);
                    let new_fp = colored_line_fingerprint(&new_lines[i]);
                    if old_fp != new_fp {
                        tab.terminal_lines[i] = new_lines[i].clone();
                        // 清除舊的 model cache，讓 sync_terminal_model_cache_range 重建
                        tab.terminal_model_rows.remove(&i);
                        tab.terminal_model_hashes.remove(&i);
                        dirty_indices.insert(i);
                    }
                }

                if new_len > old_len {
                    // 新增的行
                    for i in old_len..new_len {
                        tab.terminal_lines.push(new_lines[i].clone());
                        dirty_indices.insert(i);
                    }
                } else if new_len < old_len {
                    // 移除多餘的行
                    tab.terminal_lines.truncate(new_len);
                    tab.terminal_model_rows.retain(|&k, _| k < new_len);
                    tab.terminal_model_hashes.retain(|&k, _| k < new_len);
                    // 縮短時標記所有行 dirty（因為 model 索引可能變了）
                    for i in 0..new_len {
                        dirty_indices.insert(i);
                    }
                }

                tab.terminal_model_dirty.extend(&dirty_indices);
                tab.terminal_text.clear();
            } else {
                // 空 lines → fallback text 模式
                tab.terminal_lines = new_lines;
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
