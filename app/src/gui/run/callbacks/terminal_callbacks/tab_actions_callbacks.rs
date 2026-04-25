use std::cell::RefCell;
use std::rc::Rc;

use slint::{ComponentHandle, Model, SharedString};

use crate::gui::ipc::IpcBridge;
use crate::gui::slint_ui::AppWindow;
use crate::gui::state::GuiState;

use super::super::refresh_terminal_tab_view;
use crate::gui::run::helpers::{clear_all_prompt_images, remove_prompt_image_at};

pub(super) fn connect_toggles(app: &AppWindow, state: Rc<RefCell<GuiState>>, ipc: IpcBridge) {
    let st_raw = Rc::clone(&state);
    let app_weak = app.as_weak();
    app.on_toggle_raw_input_requested(move || {
        let Some(ui) = app_weak.upgrade() else {
            return;
        };
        let mut s = st_raw.borrow_mut();
        if let Err(e) = s.toggle_raw_input_current(&ui) {
            eprintln!("CliGJ: raw input toggle: {e}");
        }
    });

    let st_pin = Rc::clone(&state);
    let app_weak = app.as_weak();
    app.on_terminal_pin_lines_edited(move |new_text| {
        let Some(ui) = app_weak.upgrade() else {
            return;
        };
        let parsed = if new_text.trim().is_empty() {
            0
        } else {
            match new_text.trim().parse::<usize>() {
                Ok(v) => v,
                Err(_) => {
                    let s = st_pin.borrow();
                    if s.current < s.tabs.len() {
                        ui.set_ws_terminal_pin_lines(SharedString::from(
                            s.tabs[s.current].terminal_pinned_footer_lines.to_string().as_str(),
                        ));
                    }
                    return;
                }
            }
        };

        let mut s = st_pin.borrow_mut();
        if s.current >= s.tabs.len() {
            return;
        }
        let current = s.current;
        let tab = &mut s.tabs[current];
        tab.terminal_pinned_footer_lines = parsed;
        tab.terminal_pinned_footer_override = Some(parsed);
        ui.set_ws_terminal_pin_lines(SharedString::from(parsed.to_string().as_str()));
        refresh_terminal_tab_view(&ui, tab);
    });

    let app_weak = app.as_weak();
    let ipc_toggle = ipc.clone();
    app.on_toggle_ipc_server_requested(move || {
        let Some(ui) = app_weak.upgrade() else {
            return;
        };
        if let Err(e) = ipc_toggle.toggle() {
            eprintln!("CliGJ: IPC toggle: {e}");
        }
        let snap = ipc_toggle.snapshot();
        ui.set_ws_ipc_running(snap.running);
        ui.set_ws_ipc_client_count(snap.client_count as i32);
        let status_text = if snap.running {
            format!("IPC ON ({})", snap.client_count)
        } else if !snap.last_error.trim().is_empty() {
            format!("IPC OFF ({})", snap.last_error)
        } else {
            "IPC OFF".to_string()
        };
        ui.set_ws_ipc_status_text(SharedString::from(status_text.as_str()));
    });
}

pub(super) fn connect_rename(app: &AppWindow, state: Rc<RefCell<GuiState>>) {
    let st_req = Rc::clone(&state);
    let app_weak = app.as_weak();
    app.on_rename_tab_requested(move |index| {
        let Some(ui) = app_weak.upgrade() else {
            return;
        };
        let s = st_req.borrow_mut();
        if index < 0 || (index as usize) >= s.tabs.len() {
            return;
        }
        let title = s
            .titles
            .row_data(index as usize)
            .unwrap_or_else(|| SharedString::from("Tab"));
        ui.set_ws_rename_index(index);
        ui.set_ws_rename_text(title);
        ui.set_ws_rename_open(true);
    });

    let st_commit = Rc::clone(&state);
    let app_weak = app.as_weak();
    app.on_rename_commit(move |index, text| {
        let Some(ui) = app_weak.upgrade() else {
            return;
        };
        let s = st_commit.borrow_mut();
        if index < 0 || (index as usize) >= s.tabs.len() {
            return;
        }
        s.titles
            .set_row_data(index as usize, SharedString::from(text.as_str()));
        ui.set_ws_rename_open(false);
    });

    let app_weak = app.as_weak();
    app.on_rename_cancel(move || {
        let Some(ui) = app_weak.upgrade() else {
            return;
        };
        ui.set_ws_rename_open(false);
    });
}

pub(super) fn connect_move_inject(app: &AppWindow, state: Rc<RefCell<GuiState>>) {
    let st_move = Rc::clone(&state);
    let app_weak = app.as_weak();
    app.on_move_tab_requested(move |from, to| {
        let Some(ui) = app_weak.upgrade() else {
            return;
        };
        let mut s = st_move.borrow_mut();
        let _ = s.move_tab(from as usize, to as usize, &ui);
    });

    let st_inj = Rc::clone(&state);
    let app_weak = app.as_weak();
    app.on_inject_file_requested(move || {
        let Some(ui) = app_weak.upgrade() else {
            return;
        };
        let Some(path) = rfd::FileDialog::new().pick_file() else {
            return;
        };
        let mut s = st_inj.borrow_mut();
        if let Err(e) = crate::gui::run::helpers::inject_path_into_current(&ui, &mut s, path.as_path()) {
            eprintln!("CliGJ: inject file {}: {e}", path.display());
        }
    });

    let st_ws_folder = Rc::clone(&state);
    let app_weak = app.as_weak();
    app.on_workspace_folder_requested(move || {
        let Some(ui) = app_weak.upgrade() else {
            return;
        };
        let Some(path) = rfd::FileDialog::new().pick_folder() else {
            return;
        };
        let path_str = path.to_string_lossy().to_string();
        let mut s = st_ws_folder.borrow_mut();
        if s.current >= s.tabs.len() {
            return;
        }
        let ct = s.tabs[s.current].cmd_type.clone();
        s.workspace_file_cache.clear();
        s.workspace_file_cache_root = None;
        ui.set_ws_file_path(SharedString::from(path_str.as_str()));
        if let Err(e) = s.change_current_cmd_type(ct.as_str(), &ui) {
            eprintln!("CliGJ: workspace folder + PTY respawn: {e}");
        }
    });

    let st_img = Rc::clone(&state);
    let app_weak = app.as_weak();
    app.on_inject_image_requested(move || {
        let Some(ui) = app_weak.upgrade() else {
            return;
        };
        let Some(path) = rfd::FileDialog::new()
            .add_filter(
                "Images",
                &[
                    "png", "jpg", "jpeg", "gif", "webp", "bmp", "ico", "tif", "tiff", "svg",
                ],
            )
            .pick_file()
        else {
            return;
        };
        let mut s = st_img.borrow_mut();
        if let Err(e) =
            crate::gui::run::helpers::push_prompt_image_from_path(&ui, &mut s, path.as_path())
        {
            eprintln!("CliGJ: inject image {}: {e}", path.display());
        }
    });

    let st_img_rm = Rc::clone(&state);
    let app_weak = app.as_weak();
    app.on_prompt_image_remove_requested(move |index| {
        let Some(ui) = app_weak.upgrade() else {
            return;
        };
        if index < 0 {
            return;
        }
        let mut s = st_img_rm.borrow_mut();
        remove_prompt_image_at(&ui, &mut *s, index as usize);
    });

    let st_img_clr = Rc::clone(&state);
    let app_weak = app.as_weak();
    app.on_prompt_images_clear_requested(move || {
        let Some(ui) = app_weak.upgrade() else {
            return;
        };
        let mut s = st_img_clr.borrow_mut();
        clear_all_prompt_images(&ui, &mut *s);
    });
}

