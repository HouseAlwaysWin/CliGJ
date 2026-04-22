use std::cell::RefCell;
use std::rc::Rc;

use slint::{ComponentHandle, Model, SharedString};

use crate::gui::interactive_commands::sync_interactive_manage_editor_to_ui;
use crate::gui::slint_ui::{AppWindow, InteractiveCmdEditorRow};
use crate::gui::state::GuiState;

use super::super::super::{model_interactive_editor_rows, set_manage_rows};

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

    let app_weak = app.as_weak();
    app.on_close_interactive_manage(move || {
        let Some(ui) = app_weak.upgrade() else {
            return;
        };
        ui.set_ws_interactive_manage_open(false);
    });
}
