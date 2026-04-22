use std::cell::{Cell, RefCell};
use std::rc::Rc;

use slint::{spawn_local, ComponentHandle, Model, SharedString};

use crate::gui::ipc::IpcBridge;
use crate::gui::slint_ui::{AppWindow, TerminalHistoryWindow};
use crate::gui::state::{GuiState, TerminalMode};
use crate::gui::ui_sync::{
    clamp_saved_scroll_top, load_tab_to_ui, push_terminal_view_to_ui, scrollable_terminal_line_count,
    TERMINAL_ROW_HEIGHT_PX, UI_LAYOUT_EPOCH,
};
use crate::terminal::windows_conpty;

use super::refresh_terminal_tab_view;
use crate::gui::run::helpers::{
    clear_all_prompt_images, copy_to_clipboard, remove_prompt_image_at, selected_text_from_terminal_lines,
    terminal_history_plain_text,
};

pub(super) fn connect_chips(app: &AppWindow, state: Rc<RefCell<GuiState>>) {
    let st_remove = Rc::clone(&state);
    let app_weak = app.as_weak();
    app.on_prompt_chip_remove_requested(move |index| {
        let Some(ui) = app_weak.upgrade() else {
            return;
        };
        if index < 0 {
            return;
        }
        let mut s = st_remove.borrow_mut();
        if s.current >= s.tabs.len() {
            return;
        }
        let current = s.current;
        let idx = index as usize;
        if idx >= s.tabs[current].prompt_picked_files_abs.len() {
            return;
        }
        s.tabs[current].prompt_picked_files_abs.remove(idx);
        s.tabs[current].terminal_saved_scroll_top_px = ui.get_ws_terminal_scroll_top_px();
        load_tab_to_ui(&ui, &mut s.tabs[current]);
    });

    let st_clear = Rc::clone(&state);
    let app_weak = app.as_weak();
    app.on_prompt_chip_clear_requested(move || {
        let Some(ui) = app_weak.upgrade() else {
            return;
        };
        let mut s = st_clear.borrow_mut();
        if s.current >= s.tabs.len() {
            return;
        }
        let current = s.current;
        s.tabs[current].prompt_picked_files_abs.clear();
        s.tabs[current].terminal_saved_scroll_top_px = ui.get_ws_terminal_scroll_top_px();
        load_tab_to_ui(&ui, &mut s.tabs[current]);
    });
}

pub(super) fn connect_terminal_selection(app: &AppWindow, state: Rc<RefCell<GuiState>>) {
    let st_sel = Rc::clone(&state);
    let app_weak = app.as_weak();
    app.on_terminal_selection_committed(move |sr, sc, er, ec| {
        let Some(ui) = app_weak.upgrade() else {
            return;
        };
        let mut s = st_sel.borrow_mut();
        if s.current >= s.tabs.len() {
            return;
        }
        let selected = selected_text_from_terminal_lines(&s.tabs[s.current], sr, sc, er, ec);
        if selected.is_empty() {
            return;
        }
        if let Err(e) = copy_to_clipboard(selected.as_str()) {
            eprintln!("CliGJ: copy selection: {e}");
        }
        let current = s.current;
        s.tabs[current].selected_context = SharedString::from(selected.as_str());
        ui.set_ws_selected_context(SharedString::from(selected.as_str()));
    });
}

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
            let history_title = format!("{title} - 終端歷史");
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

    let history_window_close = Rc::clone(&history_window);
    let history_visible_close = Rc::clone(&history_window_visible);
    history_window.on_close_requested(move || {
        history_visible_close.set(false);
        let _ = history_window_close.hide();
    });
}

pub(super) fn connect_terminal_resize(app: &AppWindow, state: Rc<RefCell<GuiState>>) {
    let st_resize = Rc::clone(&state);
    let app_weak = app.as_weak();
    app.on_terminal_resize_requested(move |cols, rows| {
        if cols <= 0 || rows <= 0 {
            return;
        }
        let request_epoch = UI_LAYOUT_EPOCH.with(|c| c.get());
        // 直接讀取 thread-local 取得最新的 target tab ID
        // （不能 clone Cell，clone 會創建獨立副本，讀不到後續 set 的值）
        let target_tab_id = crate::gui::ui_sync::RESIZE_TARGET_TAB_ID.with(|c| c.get());
        let app_weak2 = app_weak.clone();
        let st_resize2 = Rc::clone(&st_resize);
        // Defer: `invoke_ws_bump_terminal_size` runs during `load_tab_to_ui` while callers
        // may still hold `state` borrowed — same pattern as `terminal-viewport-changed`.
        let _ = spawn_local(async move {
            let Some(_ui) = app_weak2.upgrade() else {
                return;
            };
            if UI_LAYOUT_EPOCH.with(|c| c.get()) != request_epoch {
                return;
            }
            let mut s = st_resize2.borrow_mut();
            // 透過 tab ID 找到正確的 tab，不依賴 s.current
            let Some(tab) = s.tabs.iter_mut().find(|t| t.id == target_tab_id) else {
                return;
            };
            if tab.last_pty_cols == cols as u16 && tab.last_pty_rows == rows as u16 {
                return;
            }
            tab.last_pty_cols = cols as u16;
            tab.last_pty_rows = rows as u16;
            #[cfg(target_os = "windows")]
            if let Some(conpty) = &tab.conpty {
                // 先通知 reader thread 的 wezterm-term resize
                if let Some(tx) = &tab.conpty_control_tx {
                    let _ = tx.send(windows_conpty::ControlCommand::Resize {
                        cols: cols as u16,
                        rows: rows as u16,
                    });
                }
                // 再通知 Win32 ConPTY
                let _ = conpty.resize(cols as i16, rows as i16);
            }
        });
    });
}

pub(super) fn connect_terminal_wheel(app: &AppWindow, state: Rc<RefCell<GuiState>>) {
    let st_wheel = Rc::clone(&state);
    let app_weak = app.as_weak();
    app.on_terminal_wheel(move |delta| {
        let Some(ui) = app_weak.upgrade() else {
            return false;
        };
        let (handled, next_scroll) = {
            let mut s = st_wheel.borrow_mut();
            if s.current >= s.tabs.len() {
                return false;
            }
            if s.tabs[s.current].terminal_mode != TerminalMode::InteractiveAi {
                return false;
            }
            let current = s.current;
            let tab = &mut s.tabs[current];
            let vh = ui.get_ws_terminal_viewport_height_px().max(1.0);
            let max_scroll = ((scrollable_terminal_line_count(tab) as f32) * TERMINAL_ROW_HEIGHT_PX
                - vh)
                .max(0.0);
            let current = if tab.interactive_follow_output {
                crate::gui::ui_sync::terminal_scroll_top_for_tab(tab, vh)
            } else {
                clamp_saved_scroll_top(tab, vh)
            };
            let steps = ((delta.abs() as f32) / 120.0).max(1.0).min(4.0);
            let amount = TERMINAL_ROW_HEIGHT_PX * 3.0 * steps;
            let mut next = if delta > 0 {
                (current - amount).max(0.0)
            } else if delta < 0 {
                (current + amount).min(max_scroll)
            } else {
                current
            };
            if max_scroll <= 0.5 {
                next = 0.0;
            }
            tab.interactive_follow_output = next >= (max_scroll - 1.0);
            tab.terminal_saved_scroll_top_px = next;
            (true, next)
        };
        if !handled {
            return false;
        }
        ui.invoke_ws_apply_terminal_scroll_top_px(next_scroll);
        let mut s = st_wheel.borrow_mut();
        if s.current >= s.tabs.len() {
            return false;
        }
        let current = s.current;
        let tab = &mut s.tabs[current];
        if tab.terminal_mode != TerminalMode::InteractiveAi {
            return false;
        }
        if (tab.terminal_saved_scroll_top_px - next_scroll).abs() > 0.5 {
            return true;
        }
        push_terminal_view_to_ui(&ui, tab, Some(next_scroll));
        true
    });
}

pub(super) fn connect_terminal_viewport(app: &AppWindow, state: Rc<RefCell<GuiState>>) {
    let st_vp = Rc::clone(&state);
    let app_weak = app.as_weak();
    app.on_terminal_viewport_changed(move || {
        let request_epoch = UI_LAYOUT_EPOCH.with(|c| c.get());
        let app_weak2 = app_weak.clone();
        let st_vp2 = Rc::clone(&st_vp);
        // Defer to after this Slint callback returns: avoids `RefCell` reborrow when
        // `viewport-changed` fires during another handler that already borrowed `state`.
        let _ = spawn_local(async move {
            let Some(ui) = app_weak2.upgrade() else {
                return;
            };
            if UI_LAYOUT_EPOCH.with(|c| c.get()) != request_epoch {
                return;
            }
            let mut s = st_vp2.borrow_mut();
            if s.current >= s.tabs.len() {
                return;
            }
            let cur = s.current;
            let tab = &mut s.tabs[cur];
            push_terminal_view_to_ui(&ui, tab, None);
        });
    });
}

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
        if let Err(e) = super::super::helpers::inject_path_into_current(&ui, &mut s, path.as_path()) {
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
            super::super::helpers::push_prompt_image_from_path(&ui, &mut s, path.as_path())
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
