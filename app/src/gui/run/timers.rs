//! Slint timers/dispatchers: entry points for terminal stream, IPC, composer sync, startup injection.

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::mpsc;
use std::time::Duration;

use slint::{ComponentHandle, Timer};

use crate::gui::ipc::{IpcBridge, IpcGuiCommand};
use crate::gui::slint_ui::AppWindow;
use crate::gui::state::{GuiState, TerminalChunk};

use super::helpers::{auto_disable_raw_on_cjk_prompt, inject_path_into_current};

pub(crate) fn spawn_terminal_stream_dispatcher(
    app: &AppWindow,
    state: Rc<RefCell<GuiState>>,
    rx: mpsc::Receiver<TerminalChunk>,
    ipc: IpcBridge,
) -> std::thread::JoinHandle<()> {
    super::timers_terminal::spawn_terminal_stream_dispatcher(app, state, rx, ipc)
}

pub(crate) fn spawn_ipc_bridge_timer(
    app: &AppWindow,
    state: Rc<RefCell<GuiState>>,
    ipc: IpcBridge,
    ipc_rx: mpsc::Receiver<IpcGuiCommand>,
) -> Timer {
    super::timers_ipc::spawn_ipc_bridge_timer(app, state, ipc, ipc_rx)
}

pub(crate) fn spawn_composer_at_sync_timer(app: &AppWindow, state: Rc<RefCell<GuiState>>) -> Timer {
    use crate::gui::at_picker::sync_at_file_picker;
    use crate::gui::composer_sync::sync_composer_line_to_conpty;
    use crate::gui::ui_sync::tab_update_from_ui;

    let app_weak = app.as_weak();
    let timer = Timer::default();
    const PROMPT_UNDO_CAP: usize = 200;
    timer.start(
        slint::TimerMode::Repeated,
        Duration::from_millis(20),
        move || {
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
        },
    );
    timer
}

pub(crate) fn spawn_inject_startup_timer(
    app: &AppWindow,
    state: Rc<RefCell<GuiState>>,
    path: PathBuf,
) -> Timer {
    let app_weak = app.as_weak();
    let timer = Timer::default();
    timer.start(
        slint::TimerMode::SingleShot,
        Duration::from_millis(500),
        move || {
            let Some(ui) = app_weak.upgrade() else {
                return;
            };
            let mut s = state.borrow_mut();
            if let Err(e) = inject_path_into_current(&ui, &mut s, path.as_path()) {
                eprintln!("CliGJ: --inject-file {}: {e}", path.display());
            }
        },
    );
    timer
}
