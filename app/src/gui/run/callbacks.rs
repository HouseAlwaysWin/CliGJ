//! `AppWindow` callback wiring (tabs, prompt, chips, selection, rename, inject).

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use slint::{ComponentHandle, Model, ModelRc, SharedString, VecModel};

use crate::gui::ipc::IpcBridge;
use crate::gui::slint_ui::{AppWindow, InteractiveCmdEditorRow, TerminalHistoryWindow};
use crate::gui::state::{GuiState, TerminalMode};
use crate::gui::ui_sync::{
    clamp_saved_scroll_top, push_terminal_view_to_ui, terminal_scroll_top_for_tab,
};

mod prompt_callbacks;
mod terminal_callbacks;

fn is_pty_enter_key(k: &str) -> bool {
    matches!(k, "Return" | "\n" | "\r")
}

fn model_interactive_editor_rows(
    m: &ModelRc<InteractiveCmdEditorRow>,
) -> Vec<InteractiveCmdEditorRow> {
    (0..m.row_count()).filter_map(|i| m.row_data(i)).collect()
}

fn set_manage_rows(ui: &AppWindow, rows: Vec<InteractiveCmdEditorRow>) {
    ui.set_ws_interactive_manage_rows(ModelRc::new(VecModel::from(rows)));
}

fn set_shell_manage_rows(ui: &AppWindow, rows: Vec<InteractiveCmdEditorRow>) {
    ui.set_ws_shell_manage_rows(ModelRc::new(VecModel::from(rows)));
}

fn refresh_terminal_tab_view(ui: &AppWindow, tab: &mut crate::gui::state::TabState) {
    let vh = ui.get_ws_terminal_viewport_height_px().max(1.0);
    let saved = clamp_saved_scroll_top(tab, vh);
    tab.terminal_saved_scroll_top_px = saved;
    let scroll =
        if tab.terminal_mode == TerminalMode::InteractiveAi && tab.interactive_follow_output {
            terminal_scroll_top_for_tab(tab, vh)
        } else if tab.auto_scroll {
            terminal_scroll_top_for_tab(tab, vh)
        } else {
            saved
        };
    ui.invoke_ws_apply_terminal_scroll_top_px(scroll);
    push_terminal_view_to_ui(ui, tab, Some(scroll));
}

fn publish_current_tab_changed(ipc: &IpcBridge, s: &GuiState) {
    if s.current >= s.tabs.len() {
        return;
    }
    let tab = &s.tabs[s.current];
    let title = s
        .titles
        .row_data(s.current)
        .unwrap_or_else(|| SharedString::from("Tab"))
        .to_string();
    ipc.publish_tab_changed(tab.id, s.current, title, tab.cmd_type.clone());
}

pub(crate) fn connect(
    app: &AppWindow,
    state: Rc<RefCell<GuiState>>,
    ipc: IpcBridge,
    history_window: Rc<TerminalHistoryWindow>,
) {
    let history_window_visible = Rc::new(Cell::new(false));
    let history_refresh_on_tab_change: Rc<RefCell<Option<Rc<dyn Fn()>>>> =
        Rc::new(RefCell::new(None));

    connect_tabs(
        app,
        Rc::clone(&state),
        ipc.clone(),
        Rc::clone(&history_window_visible),
        Rc::clone(&history_refresh_on_tab_change),
    );
    prompt_callbacks::connect_prompt_and_picker(app, Rc::clone(&state), Rc::clone(&history_window));
    terminal_callbacks::connect_chips(app, Rc::clone(&state), ipc.clone());
    terminal_callbacks::connect_terminal_selection(app, Rc::clone(&state));
    terminal_callbacks::connect_terminal_context_menu(app, Rc::clone(&state));
    terminal_callbacks::connect_terminal_viewport(app, Rc::clone(&state));
    terminal_callbacks::connect_terminal_resize(app, Rc::clone(&state));
    terminal_callbacks::connect_terminal_wheel(app, Rc::clone(&state));
    terminal_callbacks::connect_terminal_history(
        app,
        Rc::clone(&state),
        Rc::clone(&history_window),
        Rc::clone(&history_window_visible),
        Rc::clone(&history_refresh_on_tab_change),
    );
    terminal_callbacks::connect_toggles(app, Rc::clone(&state), ipc.clone());
    terminal_callbacks::connect_rename(app, Rc::clone(&state));
    terminal_callbacks::connect_move_inject(app, Rc::clone(&state));
}

fn connect_tabs(
    app: &AppWindow,
    state: Rc<RefCell<GuiState>>,
    ipc: IpcBridge,
    history_window_visible: Rc<Cell<bool>>,
    history_refresh_on_tab_change: Rc<RefCell<Option<Rc<dyn Fn()>>>>,
) {
    let st_tab = Rc::clone(&state);
    let ipc_tab = ipc.clone();
    let history_visible_tab = Rc::clone(&history_window_visible);
    let history_refresh_tab = Rc::clone(&history_refresh_on_tab_change);
    let app_weak = app.as_weak();
    app.on_tab_changed(move |new_index| {
        let Some(ui) = app_weak.upgrade() else {
            return;
        };
        let switch_ok = {
            let mut s = st_tab.borrow_mut();
            s.timer_snapshot = None;
            if let Err(e) = s.switch_tab(new_index as usize, &ui) {
                eprintln!("CliGJ: tab switch: {e}");
                false
            } else {
                publish_current_tab_changed(&ipc_tab, &s);
                true
            }
        };
        if switch_ok && history_visible_tab.get() {
            if let Some(refresh) = history_refresh_tab.borrow().as_ref() {
                refresh();
            }
        }
    });

    let st_close = Rc::clone(&state);
    let ipc_close = ipc.clone();
    let app_weak = app.as_weak();
    app.on_tab_close_requested(move |index| {
        let Some(ui) = app_weak.upgrade() else {
            return;
        };
        let mut s = st_close.borrow_mut();
        if let Err(e) = s.close_tab(index as usize, &ui) {
            eprintln!("CliGJ: close tab: {e}");
        } else {
            publish_current_tab_changed(&ipc_close, &s);
        }
    });

    let st_new = Rc::clone(&state);
    let ipc_new = ipc.clone();
    let app_weak = app.as_weak();
    app.on_new_tab_requested(move || {
        let Some(ui) = app_weak.upgrade() else {
            return;
        };
        let mut s = st_new.borrow_mut();
        if let Err(e) = s.add_tab(&ui) {
            eprintln!("CliGJ: new tab: {e}");
        } else {
            publish_current_tab_changed(&ipc_new, &s);
        }
    });

    let st_cmd = Rc::clone(&state);
    let ipc_cmd = ipc.clone();
    let app_weak = app.as_weak();
    app.on_cmd_type_changed(move |kind| {
        let Some(ui) = app_weak.upgrade() else {
            return;
        };
        let mut s = st_cmd.borrow_mut();
        if let Err(e) = s.change_current_cmd_type(kind.as_str(), &ui) {
            eprintln!("CliGJ: cmd type change: {e}");
        } else {
            publish_current_tab_changed(&ipc_cmd, &s);
        }
    });
}
