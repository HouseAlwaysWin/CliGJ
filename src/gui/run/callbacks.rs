//! `AppWindow` callback wiring (tabs, prompt, chips, selection, rename, inject).

use std::cell::RefCell;
use std::rc::Rc;

use slint::{spawn_local, ComponentHandle, Model, SharedString};

use crate::terminal::key_encoding::{self, MOD_CTRL};
use crate::terminal::prompt_key::PromptKeyAction;
use crate::workspace_files;

use crate::gui::at_picker::commit_at_file_pick;
use crate::gui::composer_sync::sync_composer_line_to_conpty;
use crate::gui::slint_ui::AppWindow;
use crate::gui::state::{GuiState, TerminalChunk};
use crate::gui::ui_sync::{load_tab_to_ui, push_terminal_view_to_ui, tab_update_from_ui};
use crate::terminal::windows_conpty;

use super::helpers::{
    clear_all_prompt_images, clipboard_raster_image_file, contains_cjk_char, copy_to_clipboard,
    inject_paths_and_images_from_paths, is_local_prompt_edit_key, remove_prompt_image_at,
    selected_text_from_terminal_lines,
};

fn is_pty_enter_key(k: &str) -> bool {
    matches!(k, "Return" | "\n" | "\r")
}

pub(crate) fn connect(app: &AppWindow, state: Rc<RefCell<GuiState>>) {
    connect_tabs(app, Rc::clone(&state));
    connect_prompt_and_picker(app, Rc::clone(&state));
    connect_chips(app, Rc::clone(&state));
    connect_terminal_selection(app, Rc::clone(&state));
    connect_terminal_viewport(app, Rc::clone(&state));
    connect_terminal_resize(app, Rc::clone(&state));
    connect_toggles(app, Rc::clone(&state));
    connect_rename(app, Rc::clone(&state));
    connect_move_inject(app, Rc::clone(&state));
}

fn connect_tabs(app: &AppWindow, state: Rc<RefCell<GuiState>>) {
    let st_tab = Rc::clone(&state);
    let app_weak = app.as_weak();
    app.on_tab_changed(move |new_index| {
        let Some(ui) = app_weak.upgrade() else {
            return;
        };
        let mut s = st_tab.borrow_mut();
        s.timer_prompt_snapshot = None;
        if let Err(e) = s.switch_tab(new_index as usize, &ui) {
            eprintln!("CliGJ: tab switch: {e}");
        }
    });

    let st_close = Rc::clone(&state);
    let app_weak = app.as_weak();
    app.on_tab_close_requested(move |index| {
        let Some(ui) = app_weak.upgrade() else {
            return;
        };
        let mut s = st_close.borrow_mut();
        if let Err(e) = s.close_tab(index as usize, &ui) {
            eprintln!("CliGJ: close tab: {e}");
        }
    });

    let st_new = Rc::clone(&state);
    let app_weak = app.as_weak();
    app.on_new_tab_requested(move || {
        let Some(ui) = app_weak.upgrade() else {
            return;
        };
        let mut s = st_new.borrow_mut();
        if let Err(e) = s.add_tab(&ui) {
            eprintln!("CliGJ: new tab: {e}");
        }
    });

    let st_cmd = Rc::clone(&state);
    let app_weak = app.as_weak();
    app.on_cmd_type_changed(move |kind| {
        let Some(ui) = app_weak.upgrade() else {
            return;
        };
        let mut s = st_cmd.borrow_mut();
        if let Err(e) = s.change_current_cmd_type(kind.as_str(), &ui) {
            eprintln!("CliGJ: cmd type change: {e}");
        }
    });
}

fn connect_prompt_and_picker(app: &AppWindow, state: Rc<RefCell<GuiState>>) {
    let st_submit = Rc::clone(&state);
    let app_weak = app.as_weak();
    app.on_submit_prompt(move || {
        let Some(ui) = app_weak.upgrade() else {
            return;
        };
        let mut s = st_submit.borrow_mut();
        if let Err(e) = s.submit_current_prompt(&ui) {
            eprintln!("CliGJ: prompt submit: {e}");
        }
    });

    let st_hist_prev = Rc::clone(&state);
    let app_weak = app.as_weak();
    app.on_prompt_history_prev(move || {
        let Some(ui) = app_weak.upgrade() else {
            return;
        };
        let mut s = st_hist_prev.borrow_mut();
        if let Err(e) = s.history_prev_current_prompt(&ui) {
            eprintln!("CliGJ: history prev: {e}");
        }
    });

    let st_hist_next = Rc::clone(&state);
    let app_weak = app.as_weak();
    app.on_prompt_history_next(move || {
        let Some(ui) = app_weak.upgrade() else {
            return;
        };
        let mut s = st_hist_next.borrow_mut();
        if let Err(e) = s.history_next_current_prompt(&ui) {
            eprintln!("CliGJ: history next: {e}");
        }
    });

    let st_keys = Rc::clone(&state);
    let app_weak = app.as_weak();
    app.on_prompt_key_route(move |raw_tty, mod_mask, key, shift| {
        let Some(ui) = app_weak.upgrade() else {
            return false;
        };
        let key_str = key.as_str();
        // Composer: Ctrl+V — HDROP paths (images vs files), then raster → temp PNG path.
        if !raw_tty
            && !ui.get_ws_at_picker_open()
            && (mod_mask as u32) & MOD_CTRL != 0
            && matches!(key_str, "v" | "V")
        {
            #[cfg(target_os = "windows")]
            if let Some(paths) = super::helpers::clipboard_file_paths_hdrop() {
                let mut s = st_keys.borrow_mut();
                if let Err(e) = inject_paths_and_images_from_paths(&ui, &mut *s, &paths) {
                    eprintln!("CliGJ: paste paths: {e}");
                }
                return true;
            }
            if let Some((path, img)) = clipboard_raster_image_file() {
                let mut s = st_keys.borrow_mut();
                let abs = path.to_string_lossy().to_string();
                if let Err(e) = super::helpers::push_prompt_image(&ui, &mut *s, abs, img) {
                    eprintln!("CliGJ: paste image: {e}");
                }
                return true;
            }
        }
        if raw_tty
            && is_local_prompt_edit_key(mod_mask as u32, key_str)
            && !ui.get_ws_prompt().is_empty()
        {
            return false;
        }
        if raw_tty && contains_cjk_char(key_str) {
            let mut s = st_keys.borrow_mut();
            if let Err(e) = s.toggle_raw_input_current(&ui) {
                eprintln!("CliGJ: raw input auto-toggle (CJK): {e}");
            }
            return false;
        }
        if ui.get_ws_at_picker_open() && !raw_tty {
            match key_str {
                "UpArrow" => {
                    let m = ui.get_ws_at_choices();
                    let n = m.row_count() as i32;
                    if n <= 0 {
                        return true;
                    }
                    let cur = ui.get_ws_at_selected();
                    ui.set_ws_at_selected((cur - 1).max(0));
                    ui.invoke_ws_scroll_at_picker_into_view();
                    return true;
                }
                "DownArrow" => {
                    let m = ui.get_ws_at_choices();
                    let n = m.row_count() as i32;
                    if n <= 0 {
                        return true;
                    }
                    let cur = ui.get_ws_at_selected();
                    ui.set_ws_at_selected((cur + 1).min(n - 1));
                    ui.invoke_ws_scroll_at_picker_into_view();
                    return true;
                }
                "Return" | "\n" | "\r" => {
                    let mut s = st_keys.borrow_mut();
                    let choices = ui.get_ws_at_choices();
                    if ui.get_ws_at_picker_open() && choices.row_count() > 0 {
                        let idx = ui.get_ws_at_selected() as usize;
                        commit_at_file_pick(&ui, &mut *s, idx);
                    } else {
                        // 統一 Enter：不論是否 Raw，都觸發提交
                        if let Err(e) = s.submit_current_prompt(&ui) {
                            eprintln!("CliGJ: prompt submit: {e}");
                        }
                    }
                    return true;
                }
                "Escape" => {
                    let prompt = ui.get_ws_prompt().to_string();
                    let new_p = workspace_files::strip_active_at_segment(&prompt);
                    ui.set_ws_prompt(SharedString::from(new_p.as_str()));
                    ui.set_ws_at_picker_open(false);
                    let mut s = st_keys.borrow_mut();
                    let idx = s.current;
                    tab_update_from_ui(&mut s.tabs[idx], &ui);
                    sync_composer_line_to_conpty(&ui, &mut *s);
                    return true;
                }
                _ => {}
            }
        }
        match crate::terminal::prompt_key::route_prompt_key(
            raw_tty,
            mod_mask as u32,
            key_str,
            shift,
        ) {
            PromptKeyAction::Reject => false,
            PromptKeyAction::ToggleRawInput => {
                let mut s = st_keys.borrow_mut();
                if let Err(e) = s.toggle_raw_input_current(&ui) {
                    eprintln!("CliGJ: raw input toggle: {e}");
                }
                true
            }
            PromptKeyAction::Submit => {
                let mut s = st_keys.borrow_mut();
                if let Err(e) = s.submit_current_prompt(&ui) {
                    eprintln!("CliGJ: prompt submit: {e}");
                }
                true
            }
            PromptKeyAction::HistoryPrev => {
                let mut s = st_keys.borrow_mut();
                if let Err(e) = s.history_prev_current_prompt(&ui) {
                    eprintln!("CliGJ: history prev: {e}");
                }
                true
            }
            PromptKeyAction::HistoryNext => {
                let mut s = st_keys.borrow_mut();
                if let Err(e) = s.history_next_current_prompt(&ui) {
                    eprintln!("CliGJ: history next: {e}");
                }
                true
            }
            PromptKeyAction::PtyKey(k) => {
                let bytes = match key_encoding::encode_for_pty(mod_mask as u32, k.as_str()) {
                    Some(b) => b,
                    None => return false,
                };
                let is_nav = matches!(k.as_str(), "LeftArrow" | "RightArrow" | "Home" | "End" | "Backspace" | "Delete");
                let inject_ok = {
                    let mut s = st_keys.borrow_mut();
                    s.inject_bytes_into_current(&ui, &bytes)
                };
                match &inject_ok {
                    Ok(()) => {
                        if raw_tty && is_pty_enter_key(k.as_str()) {
                            ui.set_ws_prompt(SharedString::new());
                            let mut s = st_keys.borrow_mut();
                            let idx = s.current;
                            if idx < s.tabs.len() {
                                s.tabs[idx].prompt = SharedString::new();
                            }
                        }
                    }
                    Err(e) => eprintln!("CliGJ: pty key: {e}"),
                }
                // 關鍵：如果是導航或刪除鍵，回傳 false 讓 Slint TextEdit 更新 GUI 游標位置
                if is_nav {
                    return false;
                }
                true
            }
        }
    });

    let st_pick = Rc::clone(&state);
    let app_weak = app.as_weak();
    app.on_at_picker_choose(move |index| {
        let Some(ui) = app_weak.upgrade() else {
            return;
        };
        if index < 0 {
            return;
        }
        let mut s = st_pick.borrow_mut();
        commit_at_file_pick(&ui, &mut *s, index as usize);
    });

    let st_ai = Rc::clone(&state);
    let app_weak = app.as_weak();
    app.on_ai_model_selected(move |model_name| {
        let Some(ui) = app_weak.upgrade() else {
            return;
        };
        let launch_cmd = match model_name.as_str() {
            "gemini" => "gemini\r\n",
            "codex" => "codex\r\n",
            "claude" => "claude\r\n",
            "copilot" => "copilot\r\n",
            _ => return,
        };

        let mut s = st_ai.borrow_mut();
        let cur_idx = s.current;
        if cur_idx >= s.tabs.len() { return; }
        
        let cmd_type = s.tabs[cur_idx].cmd_type.clone();
        let tab_id = s.tabs[cur_idx].id;
        let tx = s.tx.clone();

        // 策略：重啟 PTY 以實現絕對乾淨的切換
        #[cfg(target_os = "windows")]
        {
            // 1. 關閉舊 PTY 並徹底清空所有緩存
            s.tabs[cur_idx].conpty = None; // Drop 會觸發 ConPTY 關閉
            
            let tab = &mut s.tabs[cur_idx];
            tab.terminal_lines.clear();
            tab.terminal_text.clear();
            tab.terminal_model_dirty.clear();
            tab.terminal_model_rows.clear();
            tab.terminal_model_hashes.clear();
            while tab.terminal_slint_model.row_count() > 0 {
                tab.terminal_slint_model.remove(0);
            }
            tab.last_window_total = 0;
            tab.last_window_first = 0;
            tab.last_window_last = 0;
            
            ui.set_ws_terminal_line_offset(0);
            ui.set_ws_terminal_total_lines(0);
            ui.set_ws_terminal_lines(slint::ModelRc::from(Rc::clone(&tab.terminal_slint_model)));
            ui.invoke_ws_scroll_terminal_to_top();

            // 2. 開啟新 PTY
            if let Ok(spawn) = windows_conpty::spawn_conpty(&cmd_type, 120, 40) {
                let (control_tx, control_rx) = std::sync::mpsc::channel();
                windows_conpty::start_reader_thread(spawn.reader, control_rx, move |render| {
                    let _ = tx.send(TerminalChunk {
                        tab_id,
                        text: render.text,
                        lines: render.lines,
                        full_len: render.full_len,
                        first_line_idx: render.first_line_idx,
                        replace: true,
                        set_auto_scroll: if render.filled { Some(true) } else { None },
                        changed_indices: render.changed_indices,
                        cursor_col: render.cursor_col,
                        cursor_row: render.cursor_row,
                        reset_terminal_buffer: render.reset_terminal_buffer,
                    });
                });
                s.tabs[cur_idx].conpty = Some(spawn.session);
                s.tabs[cur_idx].conpty_control_tx = Some(control_tx);
            }
        }

        // 3. 延時啟動新指令
        let app_weak_inner = ui.as_weak();
        let st_ai_inner = Rc::clone(&st_ai);
        slint::Timer::single_shot(std::time::Duration::from_millis(600), move || {
            let Some(ui) = app_weak_inner.upgrade() else { return; };
            let mut s = st_ai_inner.borrow_mut();
            let _ = s.inject_bytes_into_current(&ui, launch_cmd.as_bytes());
        });
    });
}

fn connect_chips(app: &AppWindow, state: Rc<RefCell<GuiState>>) {
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
        load_tab_to_ui(&ui, &mut s.tabs[current]);
    });
}

fn connect_terminal_selection(app: &AppWindow, state: Rc<RefCell<GuiState>>) {
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

fn connect_terminal_resize(app: &AppWindow, state: Rc<RefCell<GuiState>>) {
    let st_resize = Rc::clone(&state);
    let app_weak = app.as_weak();
    app.on_terminal_resize_requested(move |cols, rows| {
        if cols <= 0 || rows <= 0 {
            return;
        }
        let app_weak2 = app_weak.clone();
        let st_resize2 = Rc::clone(&st_resize);
        // Defer: `invoke_ws_bump_terminal_size` runs during `load_tab_to_ui` while callers
        // may still hold `state` borrowed — same pattern as `terminal-viewport-changed`.
        let _ = spawn_local(async move {
            let Some(_ui) = app_weak2.upgrade() else {
                return;
            };
            let mut s = st_resize2.borrow_mut();
            let cur = s.current;
            if cur >= s.tabs.len() {
                return;
            }
            let tab = &mut s.tabs[cur];
            #[cfg(target_os = "windows")]
            if let Some(conpty) = &tab.conpty {
                let _ = conpty.resize(cols as i16, rows as i16);
                if let Some(tx) = &tab.conpty_control_tx {
                    let _ = tx.send(windows_conpty::ControlCommand::Resize {
                        cols: cols as u16,
                        rows: rows as u16,
                    });
                }
            }
        });
    });
}

fn connect_terminal_viewport(app: &AppWindow, state: Rc<RefCell<GuiState>>) {
    let st_vp = Rc::clone(&state);
    let app_weak = app.as_weak();
    app.on_terminal_viewport_changed(move || {
        let app_weak2 = app_weak.clone();
        let st_vp2 = Rc::clone(&st_vp);
        // Defer to after this Slint callback returns: avoids `RefCell` reborrow when
        // `viewport-changed` fires during another handler that already borrowed `state`.
        let _ = spawn_local(async move {
            let Some(ui) = app_weak2.upgrade() else {
                return;
            };
            let mut s = st_vp2.borrow_mut();
            if s.current >= s.tabs.len() {
                return;
            }
            let cur = s.current;
            let tab = &mut s.tabs[cur];
            push_terminal_view_to_ui(&ui, tab);
        });
    });
}

fn connect_toggles(app: &AppWindow, state: Rc<RefCell<GuiState>>) {
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

}

fn connect_rename(app: &AppWindow, state: Rc<RefCell<GuiState>>) {
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

fn connect_move_inject(app: &AppWindow, state: Rc<RefCell<GuiState>>) {
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
        if let Err(e) = super::helpers::inject_path_into_current(&ui, &mut s, path.as_path()) {
            eprintln!("CliGJ: inject file {}: {e}", path.display());
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
        if let Err(e) = super::helpers::push_prompt_image_from_path(&ui, &mut s, path.as_path()) {
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
