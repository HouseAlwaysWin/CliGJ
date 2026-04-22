mod chips_selection_callbacks;
mod history_callbacks;
mod tab_actions_callbacks;
mod viewport_callbacks;

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use crate::gui::ipc::IpcBridge;
use crate::gui::slint_ui::{AppWindow, TerminalHistoryWindow};
use crate::gui::state::GuiState;

pub(super) fn connect_chips(app: &AppWindow, state: Rc<RefCell<GuiState>>) {
    chips_selection_callbacks::connect_chips(app, state);
}

pub(super) fn connect_terminal_selection(app: &AppWindow, state: Rc<RefCell<GuiState>>) {
    chips_selection_callbacks::connect_terminal_selection(app, state);
}

pub(super) fn connect_terminal_history(
    app: &AppWindow,
    state: Rc<RefCell<GuiState>>,
    history_window: Rc<TerminalHistoryWindow>,
    history_window_visible: Rc<Cell<bool>>,
    history_refresh_on_tab_change: Rc<RefCell<Option<Rc<dyn Fn()>>>>,
) {
    history_callbacks::connect_terminal_history(
        app,
        state,
        history_window,
        history_window_visible,
        history_refresh_on_tab_change,
    );
}

pub(super) fn connect_terminal_resize(app: &AppWindow, state: Rc<RefCell<GuiState>>) {
    viewport_callbacks::connect_terminal_resize(app, state);
}

pub(super) fn connect_terminal_wheel(app: &AppWindow, state: Rc<RefCell<GuiState>>) {
    viewport_callbacks::connect_terminal_wheel(app, state);
}

pub(super) fn connect_terminal_viewport(app: &AppWindow, state: Rc<RefCell<GuiState>>) {
    viewport_callbacks::connect_terminal_viewport(app, state);
}

pub(super) fn connect_toggles(app: &AppWindow, state: Rc<RefCell<GuiState>>, ipc: IpcBridge) {
    tab_actions_callbacks::connect_toggles(app, state, ipc);
}

pub(super) fn connect_rename(app: &AppWindow, state: Rc<RefCell<GuiState>>) {
    tab_actions_callbacks::connect_rename(app, state);
}

pub(super) fn connect_move_inject(app: &AppWindow, state: Rc<RefCell<GuiState>>) {
    tab_actions_callbacks::connect_move_inject(app, state);
}
