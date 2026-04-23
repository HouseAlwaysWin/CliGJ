use std::cell::{Cell, RefCell};
use std::rc::Rc;

use slint::{ComponentHandle, Model, SharedString};

use crate::gui::i18n::terminal_history_title_suffix_for_shell_setting;
use crate::gui::slint_ui::{AppWindow, TerminalHistoryWindow};
use crate::gui::state::GuiState;
use crate::gui::run::helpers::{copy_to_clipboard, terminal_history_plain_text};

pub(super) fn connect_terminal_history(
    app: &AppWindow,
    state: Rc<RefCell<GuiState>>,
    history_window: Rc<TerminalHistoryWindow>,
    history_window_visible: Rc<Cell<bool>>,
    history_refresh_on_tab_change: Rc<RefCell<Option<Rc<dyn Fn()>>>>,
) {
    let refresh_history_snapshot: Rc<dyn Fn()> = Rc::new({
        let st_hist = Rc::clone(&state);
        let history_window = Rc::clone(&history_window);
        let app_weak = app.as_weak();
        move || {
            let Some(ui) = app_weak.upgrade() else {
                return;
            };
            let s = st_hist.borrow();
            if s.current >= s.tabs.len() {
                return;
            }
            let tab = &s.tabs[s.current];
            let title = s
                .titles
                .row_data(s.current)
                .unwrap_or_else(|| SharedString::from("Tab"))
                .to_string();
            let suffix = terminal_history_title_suffix_for_shell_setting(
                ui.get_ws_shell_startup_language().as_str(),
            );
            let history_title = format!("{title} - {suffix}");
            history_window.set_history_title(SharedString::from(history_title.as_str()));
            history_window.set_history_text(SharedString::from(terminal_history_plain_text(tab).as_str()));
            history_window.set_terminal_font_family(ui.get_ws_terminal_font_family());
        }
    });
    *history_refresh_on_tab_change.borrow_mut() = Some(Rc::clone(&refresh_history_snapshot));

    let refresh_on_open = Rc::clone(&refresh_history_snapshot);
    let history_window_open = Rc::clone(&history_window);
    let history_visible_open = Rc::clone(&history_window_visible);
    app.on_terminal_history_requested(move || {
        refresh_on_open();
        history_visible_open.set(true);
        if let Err(e) = history_window_open.show() {
            eprintln!("CliGJ: show terminal history window: {e}");
            history_visible_open.set(false);
        }
    });

    let refresh_on_demand = Rc::clone(&refresh_history_snapshot);
    history_window.on_refresh_requested(move || {
        refresh_on_demand();
    });

    let st_copy = Rc::clone(&state);
    history_window.on_copy_all_requested(move || {
        let s = st_copy.borrow();
        if s.current >= s.tabs.len() {
            return;
        }
        let text = terminal_history_plain_text(&s.tabs[s.current]);
        if let Err(e) = copy_to_clipboard(text.as_str()) {
            eprintln!("CliGJ: copy terminal history: {e}");
        }
    });

    let st_dump = Rc::clone(&state);
    let history_window_dump = Rc::clone(&history_window);
    history_window.on_dump_raw_pty_requested(move || {
        let s = st_dump.borrow();
        if s.current >= s.tabs.len() {
            history_window_dump.set_history_text(SharedString::from(
                "Raw PTY dump failed: no active tab",
            ));
            return;
        }
        let tab = &s.tabs[s.current];
        match tab.dump_raw_pty_events(None) {
            Ok(result) => {
                let message = format!(
                    "Raw PTY dump completed\n\nDirectory: {}\nRaw bytes: {}\nEvent index: {}\nEscaped raw stream: {}\nCurrent screen text: {}\nEvents: {}\nBytes: {}\n\n--- Terminal History ---\n{}",
                    result.dir.display(),
                    result.raw_path.display(),
                    result.index_path.display(),
                    result.escaped_path.display(),
                    result.screen_path.display(),
                    result.event_count,
                    result.byte_count,
                    terminal_history_plain_text(tab),
                );
                history_window_dump.set_history_text(SharedString::from(message.as_str()));
            }
            Err(e) => {
                history_window_dump.set_history_text(SharedString::from(
                    format!("Raw PTY dump failed: {e}").as_str(),
                ));
            }
        }
    });

    let history_window_close = Rc::clone(&history_window);
    let history_visible_close = Rc::clone(&history_window_visible);
    history_window.on_close_requested(move || {
        history_visible_close.set(false);
        let _ = history_window_close.hide();
    });
}
