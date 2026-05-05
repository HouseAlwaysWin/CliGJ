use std::cell::RefCell;
use std::rc::Rc;

use slint::ComponentHandle;

use crate::gui::slint_ui::AppWindow;
use crate::gui::state::{GuiState, TerminalMode};
use crate::gui::terminal_menu;

pub(super) fn connect_terminal_menu(app: &AppWindow, state: Rc<RefCell<GuiState>>) {
    let st_hover = Rc::clone(&state);
    let app_weak = app.as_weak();
    app.on_terminal_menu_option_hovered(move |row| {
        let Some(ui) = app_weak.upgrade() else {
            return;
        };
        if row < 0 {
            return;
        }
        let mut s = st_hover.borrow_mut();
        if s.current >= s.tabs.len() {
            return;
        }
        let current = s.current;
        let tab = &mut s.tabs[current];
        if tab.terminal_mode != TerminalMode::InteractiveAi {
            return;
        }
        let target_row = row as usize;
        if tab.terminal_menu_active_row == Some(target_row)
            || tab.terminal_menu_pending_row == Some(target_row)
        {
            return;
        }
        let Some(bytes) = terminal_menu::move_menu_row_bytes(tab, target_row) else {
            return;
        };
        if bytes.is_empty() {
            terminal_menu::mark_menu_pending_row(tab, target_row);
            return;
        }
        tab.interactive_follow_output = true;
        terminal_menu::mark_menu_pending_row(tab, target_row);
        if let Err(e) = s.inject_bytes_into_current(&ui, &bytes) {
            eprintln!("CliGJ: terminal menu hover: {e}");
        }
    });

    let st_menu = Rc::clone(&state);
    let app_weak = app.as_weak();
    app.on_terminal_menu_option_chosen(move |row| {
        let Some(ui) = app_weak.upgrade() else {
            return;
        };
        if row < 0 {
            return;
        }
        let mut s = st_menu.borrow_mut();
        if s.current >= s.tabs.len() {
            return;
        }
        let current = s.current;
        let target_row = row as usize;
        let Some(bytes) = terminal_menu::activate_menu_row_bytes(&s.tabs[current], target_row) else {
            return;
        };
        if s.tabs[current].terminal_mode == TerminalMode::InteractiveAi {
            s.tabs[current].interactive_follow_output = true;
        }
        terminal_menu::mark_menu_pending_row(&mut s.tabs[current], target_row);
        if let Err(e) = s.inject_bytes_into_current(&ui, &bytes) {
            eprintln!("CliGJ: terminal menu click: {e}");
        }
    });
}
