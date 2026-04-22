use std::cell::RefCell;
use std::collections::HashSet;
use std::rc::Rc;

use slint::{ComponentHandle, Model, SharedString};

use crate::core::config::AppConfig;
use crate::gui::interactive_commands::{
    pinned_footer_lines_for_specs, sync_interactive_command_choices_to_ui,
    sync_interactive_manage_editor_to_ui,
};
use crate::gui::slint_ui::{AppWindow, InteractiveCmdEditorRow};
use crate::gui::state::{GuiState, TerminalMode};

use super::super::{model_interactive_editor_rows, refresh_terminal_tab_view, set_manage_rows};

pub(super) fn connect(app: &AppWindow, state: Rc<RefCell<GuiState>>) {
    let st_manage = Rc::clone(&state);
    let app_weak = app.as_weak();
    app.on_manage_interactive_commands_requested(move || {
        let Some(ui) = app_weak.upgrade() else {
            return;
        };
        let s = st_manage.borrow();
        sync_interactive_manage_editor_to_ui(&ui, &*s);
        drop(s);
        ui.set_ws_interactive_manage_open(true);
    });

    let app_weak = app.as_weak();
    app.on_manage_add_interactive_row(move || {
        let Some(ui) = app_weak.upgrade() else {
            return;
        };
        let mut rows = model_interactive_editor_rows(&ui.get_ws_interactive_manage_rows());
        rows.push(InteractiveCmdEditorRow {
            name: SharedString::new(),
            line: SharedString::new(),
            pinned_footer_lines: SharedString::from("0"),
            key_locked: false,
            expanded: false,
            workspace_path: SharedString::new(),
        });
        set_manage_rows(&ui, rows);
    });

    let app_weak = app.as_weak();
    app.on_remove_interactive_manage_row(move |idx| {
        let Some(ui) = app_weak.upgrade() else {
            return;
        };
        if idx < 0 {
            return;
        }
        let i = idx as usize;
        let mut rows = model_interactive_editor_rows(&ui.get_ws_interactive_manage_rows());
        if i >= rows.len() || rows[i].key_locked {
            return;
        }
        rows.remove(i);
        set_manage_rows(&ui, rows);
    });

    let app_weak = app.as_weak();
    app.on_interactive_manage_name_edited(move |idx, new_text| {
        let Some(ui) = app_weak.upgrade() else {
            return;
        };
        if idx < 0 {
            return;
        }
        let i = idx as usize;
        let m = ui.get_ws_interactive_manage_rows();
        let Some(mut row) = m.row_data(i) else {
            return;
        };
        if row.key_locked {
            return;
        }
        row.name = new_text;
        m.set_row_data(i, row);
    });

    let app_weak = app.as_weak();
    app.on_interactive_manage_line_edited(move |idx, new_text| {
        let Some(ui) = app_weak.upgrade() else {
            return;
        };
        if idx < 0 {
            return;
        }
        let i = idx as usize;
        let m = ui.get_ws_interactive_manage_rows();
        let Some(mut row) = m.row_data(i) else {
            return;
        };
        row.line = new_text;
        m.set_row_data(i, row);
    });

    let app_weak = app.as_weak();
    app.on_interactive_manage_pinned_lines_edited(move |idx, new_text| {
        let Some(ui) = app_weak.upgrade() else {
            return;
        };
        if idx < 0 {
            return;
        }
        let i = idx as usize;
        let m = ui.get_ws_interactive_manage_rows();
        let Some(mut row) = m.row_data(i) else {
            return;
        };
        row.pinned_footer_lines = new_text;
        m.set_row_data(i, row);
    });

    let st_save = Rc::clone(&state);
    let app_weak = app.as_weak();
    app.on_save_interactive_manage(move || {
        let Some(ui) = app_weak.upgrade() else {
            return;
        };
        let rows_m = ui.get_ws_interactive_manage_rows();
        let n = rows_m.row_count();
        let mut seen = HashSet::<String>::new();
        let mut out: Vec<(String, String, usize)> = Vec::new();
        for i in 0..n {
            let row = rows_m.row_data(i).unwrap();
            let name = row.name.to_string();
            let line = row.line.to_string();
            let pinned_footer_lines = row.pinned_footer_lines.to_string();
            let nt = name.trim();
            let lt = line.trim();
            if nt.is_empty() && lt.is_empty() {
                continue;
            }
            if nt.is_empty() {
                eprintln!("CliGJ: interactive command row needs a display name");
                return;
            }
            if lt.is_empty() {
                eprintln!("CliGJ: interactive command row needs a command line");
                return;
            }
            let nt = nt.to_string();
            if !seen.insert(nt.clone()) {
                eprintln!("CliGJ: duplicate interactive command name: {nt}");
                return;
            }
            let pinned = if pinned_footer_lines.trim().is_empty() {
                0
            } else {
                match pinned_footer_lines.trim().parse::<usize>() {
                    Ok(v) => v,
                    Err(_) => {
                        eprintln!(
                            "CliGJ: interactive command pinned footer rows must be a non-negative integer"
                        );
                        return;
                    }
                }
            };
            out.push((nt, lt.to_string(), pinned));
        }
        if out.is_empty() {
            eprintln!("CliGJ: need at least one interactive command");
            return;
        }
        let specs = out.clone();
        let refresh_current_interactive = {
            let mut s = st_save.borrow_mut();
            s.interactive_commands = out;
            let current = s.current;
            for tab in &mut s.tabs {
                if tab.terminal_mode != TerminalMode::InteractiveAi {
                    continue;
                }
                if tab.interactive_launcher_program.trim().is_empty() {
                    continue;
                }
                if tab.terminal_pinned_footer_override.is_some() {
                    continue;
                }
                tab.terminal_pinned_footer_lines =
                    pinned_footer_lines_for_specs(tab.interactive_launcher_program.as_str(), &specs);
            }
            current < s.tabs.len() && s.tabs[current].terminal_mode == TerminalMode::InteractiveAi
        };
        let snapshot = st_save.borrow().interactive_commands.clone();
        match AppConfig::load_or_default() {
            Ok(mut cfg) => {
                cfg.set_interactive_commands(&snapshot);
                if let Err(e) = cfg.save() {
                    eprintln!("CliGJ: save config: {e}");
                }
            }
            Err(e) => eprintln!("CliGJ: load config: {e}"),
        }
        sync_interactive_command_choices_to_ui(&ui, &st_save.borrow());
        ui.set_ws_interactive_manage_open(false);
        if refresh_current_interactive {
            let mut s = st_save.borrow_mut();
            if s.current < s.tabs.len() {
                let current = s.current;
                ui.set_ws_terminal_pin_lines(SharedString::from(
                    s.tabs[current].terminal_pinned_footer_lines.to_string().as_str(),
                ));
                refresh_terminal_tab_view(&ui, &mut s.tabs[current]);
            }
        }
    });

    let app_weak = app.as_weak();
    app.on_close_interactive_manage(move || {
        let Some(ui) = app_weak.upgrade() else {
            return;
        };
        ui.set_ws_interactive_manage_open(false);
    });
}
