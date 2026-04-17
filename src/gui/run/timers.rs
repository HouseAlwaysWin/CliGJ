//! Slint timers: terminal output pump, composer / `@` picker sync.

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::mpsc;
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

/// Batched pump: coalesce ConPTY output and refresh active terminal model.
pub(crate) fn spawn_terminal_stream_timer(
    app: &AppWindow,
    state: Rc<RefCell<GuiState>>,
    rx: mpsc::Receiver<TerminalChunk>,
) -> Timer {
    let app_weak = app.as_weak();
    let timer = Timer::default();
    timer.start(slint::TimerMode::Repeated, Duration::from_millis(24), move || {
        let Some(ui) = app_weak.upgrade() else {
            return;
        };
        let mut s = state.borrow_mut();
        let current_id = s.tabs.get(s.current).map(|t| t.id);
        let mut current_changed = false;
        let mut processed = 0usize;
        const MAX_CHUNKS_PER_TICK: usize = 512;
        let mut pending: HashMap<u64, PendingTabUpdate> = HashMap::new();
        while processed < MAX_CHUNKS_PER_TICK {
            let Ok(chunk) = rx.try_recv() else {
                break;
            };
            processed += 1;
            let entry = pending.entry(chunk.tab_id).or_default();
            if let Some(v) = chunk.set_auto_scroll {
                entry.set_auto_scroll = Some(v);
            }
            if chunk.replace {
                entry.replace_text = Some(chunk.text);
                entry.replace_lines = Some(chunk.lines);
                entry.append_text.clear();
            } else if let Some(text) = entry.replace_text.as_mut() {
                text.push_str(&chunk.text);
            } else {
                entry.append_text.push_str(&chunk.text);
            }
        }

        for (tab_id, update) in pending {
            let Some(tab_idx) = s.tabs.iter().position(|t| t.id == tab_id) else {
                continue;
            };
            let tab = &mut s.tabs[tab_idx];
            if let Some(v) = update.set_auto_scroll {
                tab.auto_scroll = v;
            }
            if let Some(text) = update.replace_text {
                tab.terminal_text = text;
                tab.terminal_lines = update.replace_lines.unwrap_or_default();
                if !update.append_text.is_empty() {
                    tab.append_terminal(&update.append_text);
                }
            } else if !update.append_text.is_empty() {
                tab.append_terminal(&update.append_text);
            }
            if current_id == Some(tab.id) {
                current_changed = true;
            }
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
            push_terminal_view_to_ui(&ui, tab);
        } else if s.current < s.tabs.len() {
            let st = ui.get_ws_terminal_scroll_top_px();
            let vh = ui.get_ws_terminal_viewport_height_px();
            let cur = s.current;
            let tab = &mut s.tabs[cur];
            if (st - tab.last_pushed_scroll_top).abs() > 0.5
                || (vh - tab.last_pushed_viewport_height).abs() > 0.5
            {
                push_terminal_view_to_ui(&ui, tab);
            }
        }

        if s.pending_scroll {
            ui.invoke_ws_scroll_terminal_to_bottom();
            if s.current < s.tabs.len() {
                let cur = s.current;
                let tab = &mut s.tabs[cur];
                push_terminal_view_to_ui(&ui, tab);
            }
            s.pending_scroll = false;
        }
    });
    timer
}

/// Composer → ConPTY mirror and `@` file picker refresh.
pub(crate) fn spawn_composer_at_sync_timer(app: &AppWindow, state: Rc<RefCell<GuiState>>) -> Timer {
    use crate::gui::at_picker::sync_at_file_picker;
    use crate::gui::composer_sync::sync_composer_line_to_conpty;

    let app_weak = app.as_weak();
    let timer = Timer::default();
    timer.start(slint::TimerMode::Repeated, Duration::from_millis(40), move || {
        let Some(ui) = app_weak.upgrade() else {
            return;
        };
        let mut s = state.borrow_mut();
        if s.current >= s.tabs.len() {
            return;
        }
        if ui.get_ws_raw_input() {
            auto_disable_raw_on_cjk_prompt(&ui, &mut s);
            return;
        }
        let prompt_now = ui.get_ws_prompt().to_string();
        let raw = ui.get_ws_raw_input();
        let key = (s.current, prompt_now, raw);
        if s.timer_prompt_snapshot.as_ref() == Some(&key) {
            return;
        }
        auto_disable_raw_on_cjk_prompt(&ui, &mut s);
        sync_composer_line_to_conpty(&ui, &mut *s);
        sync_at_file_picker(&ui, &mut *s);
        if s.current < s.tabs.len() {
            s.timer_prompt_snapshot = Some((
                s.current,
                ui.get_ws_prompt().to_string(),
                ui.get_ws_raw_input(),
            ));
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
