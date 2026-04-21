//! `AppWindow` callback wiring (tabs, prompt, chips, selection, rename, inject).

use std::cell::RefCell;
use std::collections::HashSet;
use std::rc::Rc;

use slint::{spawn_local, ComponentHandle, Model, ModelRc, SharedString, VecModel};

use crate::terminal::key_encoding::{self, MOD_CTRL};
use crate::terminal::prompt_key::PromptKeyAction;
use crate::workspace_files;

use crate::core::config::AppConfig;
use crate::gui::at_picker::commit_at_file_pick;
use crate::gui::composer_sync::sync_composer_line_to_conpty;
use crate::gui::ipc::IpcBridge;
use crate::gui::interactive_commands::{
    self, sync_interactive_command_choices_to_ui, sync_interactive_manage_editor_to_ui,
};
use crate::gui::shell_profiles::{
    default_shell_profile_name, normalize_shell_profile_command, sync_shell_manage_editor_to_ui,
    sync_shell_profile_choices_to_ui,
};
use crate::gui::slint_ui::{AppWindow, InteractiveCmdEditorRow};
use crate::gui::state::GuiState;
use crate::gui::ui_sync::{
    clamp_saved_scroll_top, load_tab_to_ui, push_terminal_view_to_ui, tab_update_from_ui,
    terminal_scroll_top_for_tab, TERMINAL_ROW_HEIGHT_PX, UI_LAYOUT_EPOCH,
};
use crate::terminal::windows_conpty;

use super::helpers::{
    clear_all_prompt_images, clipboard_raster_image_file, contains_cjk_char, copy_to_clipboard,
    inject_paths_and_images_from_paths, is_local_prompt_edit_key, remove_prompt_image_at,
    selected_text_from_terminal_lines,
};

fn is_pty_enter_key(k: &str) -> bool {
    matches!(k, "Return" | "\n" | "\r")
}

fn model_interactive_editor_rows(m: &ModelRc<InteractiveCmdEditorRow>) -> Vec<InteractiveCmdEditorRow> {
    (0..m.row_count())
        .filter_map(|i| m.row_data(i))
        .collect()
}

fn set_manage_rows(ui: &AppWindow, rows: Vec<InteractiveCmdEditorRow>) {
    ui.set_ws_interactive_manage_rows(ModelRc::new(VecModel::from(rows)));
}

fn set_shell_manage_rows(ui: &AppWindow, rows: Vec<InteractiveCmdEditorRow>) {
    ui.set_ws_shell_manage_rows(ModelRc::new(VecModel::from(rows)));
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

pub(crate) fn connect(app: &AppWindow, state: Rc<RefCell<GuiState>>, ipc: IpcBridge) {
    connect_tabs(app, Rc::clone(&state), ipc.clone());
    connect_prompt_and_picker(app, Rc::clone(&state));
    connect_chips(app, Rc::clone(&state));
    connect_terminal_selection(app, Rc::clone(&state));
    connect_terminal_viewport(app, Rc::clone(&state));
    connect_terminal_resize(app, Rc::clone(&state));
    connect_terminal_wheel(app, Rc::clone(&state));
    connect_toggles(app, Rc::clone(&state), ipc.clone());
    connect_rename(app, Rc::clone(&state));
    connect_move_inject(app, Rc::clone(&state));
}

fn connect_tabs(app: &AppWindow, state: Rc<RefCell<GuiState>>, ipc: IpcBridge) {
    let st_tab = Rc::clone(&state);
    let ipc_tab = ipc.clone();
    let app_weak = app.as_weak();
    app.on_tab_changed(move |new_index| {
        let Some(ui) = app_weak.upgrade() else {
            return;
        };
        let mut s = st_tab.borrow_mut();
        s.timer_prompt_snapshot = None;
        if let Err(e) = s.switch_tab(new_index as usize, &ui) {
            eprintln!("CliGJ: tab switch: {e}");
        } else {
            publish_current_tab_changed(&ipc_tab, &s);
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
    app.on_interactive_command_selected(move |line_label| {
        let Some(ui) = app_weak.upgrade() else {
            return;
        };
        let launch_cmd = {
            let s = st_ai.borrow();
            match interactive_commands::resolve_interactive_launch(line_label.as_str(), &*s) {
                Some(c) => c,
                None => return,
            }
        };

        let mut s = st_ai.borrow_mut();
        if s.current >= s.tabs.len() {
            return;
        }
        if let Err(e) = s.respawn_conpty_for_interactive_command(&ui) {
            eprintln!("CliGJ: interactive command PTY restart: {e}");
            return;
        }
        drop(s);

        let app_weak_inner = ui.as_weak();
        let st_ai_inner = Rc::clone(&st_ai);
        slint::Timer::single_shot(std::time::Duration::from_millis(300), move || {
            let Some(ui) = app_weak_inner.upgrade() else { return; };
            let mut s = st_ai_inner.borrow_mut();
            if let Err(e) = s.inject_bytes_into_current(&ui, launch_cmd.as_bytes()) {
                eprintln!("CliGJ: inject launch command: {e}");
            }
        });
    });

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

    let st_save = Rc::clone(&state);
    let app_weak = app.as_weak();
    app.on_save_interactive_manage(move || {
        let Some(ui) = app_weak.upgrade() else {
            return;
        };
        let rows_m = ui.get_ws_interactive_manage_rows();
        let n = rows_m.row_count();
        let mut seen = HashSet::<String>::new();
        let mut out: Vec<(String, String)> = Vec::new();
        for i in 0..n {
            let row = rows_m.row_data(i).unwrap();
            let name = row.name.to_string();
            let line = row.line.to_string();
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
            out.push((nt, lt.to_string()));
        }
        if out.is_empty() {
            eprintln!("CliGJ: need at least one interactive command");
            return;
        }
        {
            let mut s = st_save.borrow_mut();
            s.interactive_commands = out;
        }
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
    });

    let app_weak = app.as_weak();
    app.on_close_interactive_manage(move || {
        let Some(ui) = app_weak.upgrade() else {
            return;
        };
        ui.set_ws_interactive_manage_open(false);
    });

    let st_shell_manage = Rc::clone(&state);
    let app_weak = app.as_weak();
    app.on_manage_cmd_types_requested(move || {
        let Some(ui) = app_weak.upgrade() else {
            return;
        };
        let s = st_shell_manage.borrow();
        sync_shell_manage_editor_to_ui(&ui, &*s);
        ui.set_ws_shell_startup_language(SharedString::from(s.startup_language.as_str()));
        ui.set_ws_shell_startup_default_profile(SharedString::from(
            s.startup_default_shell_profile.as_str(),
        ));
        drop(s);
        ui.set_ws_shell_settings_nav(SharedString::from("啟動"));
        ui.set_ws_shell_manage_saved_hint(false);
        ui.set_ws_shell_manage_open(true);
    });

    let app_weak = app.as_weak();
    app.on_manage_add_shell_row(move || {
        let Some(ui) = app_weak.upgrade() else {
            return;
        };
        let mut rows = model_interactive_editor_rows(&ui.get_ws_shell_manage_rows());
        for r in &mut rows {
            r.expanded = false;
        }
        rows.push(InteractiveCmdEditorRow {
            name: SharedString::new(),
            line: SharedString::new(),
            key_locked: false,
            expanded: true,
            workspace_path: SharedString::new(),
        });
        set_shell_manage_rows(&ui, rows);
    });

    let app_weak = app.as_weak();
    app.on_toggle_shell_manage_row_expanded(move |idx| {
        let Some(ui) = app_weak.upgrade() else {
            return;
        };
        if idx < 0 {
            return;
        }
        let i = idx as usize;
        let m = ui.get_ws_shell_manage_rows();
        let n = m.row_count();
        let Some(cur) = m.row_data(i) else {
            return;
        };
        let opening = !cur.expanded;
        for j in 0..n {
            let Some(mut row) = m.row_data(j) else {
                continue;
            };
            row.expanded = opening && j == i;
            m.set_row_data(j, row);
        }
    });

    let app_weak = app.as_weak();
    app.on_remove_shell_manage_row(move |idx| {
        let Some(ui) = app_weak.upgrade() else {
            return;
        };
        if idx < 0 {
            return;
        }
        let i = idx as usize;
        let mut rows = model_interactive_editor_rows(&ui.get_ws_shell_manage_rows());
        if i >= rows.len() || rows[i].key_locked {
            return;
        }
        rows.remove(i);
        set_shell_manage_rows(&ui, rows);
    });

    let app_weak = app.as_weak();
    app.on_shell_manage_name_edited(move |idx, new_text| {
        let Some(ui) = app_weak.upgrade() else {
            return;
        };
        if idx < 0 {
            return;
        }
        let i = idx as usize;
        let m = ui.get_ws_shell_manage_rows();
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
    app.on_shell_manage_line_edited(move |idx, new_text| {
        let Some(ui) = app_weak.upgrade() else {
            return;
        };
        if idx < 0 {
            return;
        }
        let i = idx as usize;
        let m = ui.get_ws_shell_manage_rows();
        let Some(mut row) = m.row_data(i) else {
            return;
        };
        row.line = new_text;
        m.set_row_data(i, row);
    });

    let app_weak = app.as_weak();
    app.on_shell_manage_workspace_edited(move |idx, new_text| {
        let Some(ui) = app_weak.upgrade() else {
            return;
        };
        if idx < 0 {
            return;
        }
        let i = idx as usize;
        let m = ui.get_ws_shell_manage_rows();
        let Some(mut row) = m.row_data(i) else {
            return;
        };
        row.workspace_path = new_text;
        m.set_row_data(i, row);
    });

    let app_weak = app.as_weak();
    app.on_shell_manage_workspace_pick_folder(move |idx| {
        let Some(ui) = app_weak.upgrade() else {
            return;
        };
        if idx < 0 {
            return;
        }
        let i = idx as usize;
        let Some(path) = rfd::FileDialog::new().pick_folder() else {
            return;
        };
        let path_str = path.to_string_lossy().to_string();
        let m = ui.get_ws_shell_manage_rows();
        let Some(mut row) = m.row_data(i) else {
            return;
        };
        row.workspace_path = SharedString::from(path_str.as_str());
        m.set_row_data(i, row);
    });

    let st_shell_save = Rc::clone(&state);
    let app_weak = app.as_weak();
    app.on_save_shell_manage(move || {
        let Some(ui) = app_weak.upgrade() else {
            return;
        };
        let rows_m = ui.get_ws_shell_manage_rows();
        let n = rows_m.row_count();
        let mut seen = HashSet::<String>::new();
        let mut out: Vec<(String, String, String)> = Vec::new();
        for i in 0..n {
            let row = rows_m.row_data(i).unwrap();
            let name = row.name.to_string();
            let line = row.line.to_string();
            let workspace = row.workspace_path.to_string();
            let nt = name.trim();
            let (norm_line, normalized) = normalize_shell_profile_command(line.as_str());
            let lt = norm_line.trim();
            if nt.is_empty() && lt.is_empty() {
                continue;
            }
            if nt.is_empty() {
                eprintln!("CliGJ: shell profile row needs a display name");
                return;
            }
            if lt.is_empty() {
                eprintln!("CliGJ: shell profile row needs a startup command");
                return;
            }
            let nt = nt.to_string();
            if !seen.insert(nt.clone()) {
                eprintln!("CliGJ: duplicate shell profile name: {nt}");
                return;
            }
            if normalized {
                eprintln!("CliGJ: shell profile '{nt}' normalized to an in-app compatible command");
            }
            out.push((nt, lt.to_string(), workspace.trim().to_string()));
        }

        if out.is_empty() {
            eprintln!("CliGJ: need at least one shell profile");
            return;
        }
        if !out.iter().any(|(n, _, _)| n == "Command Prompt")
            || !out.iter().any(|(n, _, _)| n == "PowerShell")
        {
            eprintln!("CliGJ: default Command Prompt / PowerShell profiles are required");
            return;
        }

        {
            let mut s = st_shell_save.borrow_mut();
            s.shell_profiles = out;
            let fallback = default_shell_profile_name(&*s);
            let allowed: HashSet<String> = s.shell_profiles.iter().map(|(n, _, _)| n.clone()).collect();
            for tab in &mut s.tabs {
                if !allowed.contains(&tab.cmd_type) {
                    tab.cmd_type = fallback.clone();
                }
            }
        }

        let snapshot = st_shell_save.borrow().shell_profiles.clone();
        match AppConfig::load_or_default() {
            Ok(mut cfg) => {
                cfg.set_shell_profiles(&snapshot);
                if let Err(e) = cfg.save() {
                    eprintln!("CliGJ: save config: {e}");
                }
            }
            Err(e) => eprintln!("CliGJ: load config: {e}"),
        }

        let s = st_shell_save.borrow();
        sync_shell_profile_choices_to_ui(&ui, &*s);
        if s.current < s.tabs.len() {
            ui.set_ws_cmd_type(SharedString::from(s.tabs[s.current].cmd_type.as_str()));
        }
        let allowed: HashSet<String> = s.shell_profiles.iter().map(|(n, _, _)| n.clone()).collect();
        drop(s);
        let configured_default = ui.get_ws_shell_startup_default_profile().to_string();
        if !allowed.contains(&configured_default) {
            let fallback = st_shell_save
                .borrow()
                .shell_profiles
                .first()
                .map(|(n, _, _)| n.clone())
                .unwrap_or_else(|| "Command Prompt".to_string());
            ui.set_ws_shell_startup_default_profile(SharedString::from(fallback.as_str()));
            if let Ok(mut cfg) = AppConfig::load_or_default() {
                cfg.set_default_shell_profile(fallback.as_str());
                let _ = cfg.save();
            }
        }
        ui.set_ws_shell_manage_saved_hint(true);
        let ui_weak = ui.as_weak();
        slint::Timer::single_shot(std::time::Duration::from_millis(1600), move || {
            let Some(ui) = ui_weak.upgrade() else {
                return;
            };
            ui.set_ws_shell_manage_saved_hint(false);
        });
    });

    let st_startup_save = Rc::clone(&state);
    let app_weak = app.as_weak();
    app.on_save_shell_startup_settings(move || {
        let Some(ui) = app_weak.upgrade() else {
            return;
        };
        let language = ui.get_ws_shell_startup_language().to_string();
        let mut profile = ui.get_ws_shell_startup_default_profile().to_string();
        let choices = ui.get_ws_cmd_type_choices();
        if choices.row_count() == 0 {
            eprintln!("CliGJ: no shell profiles available");
            return;
        }
        if (0..choices.row_count()).all(|i| choices.row_data(i).unwrap_or_default().to_string() != profile) {
            profile = choices.row_data(0).unwrap_or_default().to_string();
            ui.set_ws_shell_startup_default_profile(SharedString::from(profile.as_str()));
        }
        match AppConfig::load_or_default() {
            Ok(mut cfg) => {
                cfg.set_ui_language(language.as_str());
                cfg.set_default_shell_profile(profile.as_str());
                if let Err(e) = cfg.save() {
                    eprintln!("CliGJ: save config: {e}");
                    return;
                }
            }
            Err(e) => {
                eprintln!("CliGJ: load config: {e}");
                return;
            }
        }
        {
            let mut s = st_startup_save.borrow_mut();
            s.startup_language = language.clone();
            s.startup_default_shell_profile = profile.clone();
        }
        ui.set_ws_shell_manage_saved_hint(true);
        let ui_weak = ui.as_weak();
        slint::Timer::single_shot(std::time::Duration::from_millis(1600), move || {
            let Some(ui) = ui_weak.upgrade() else {
                return;
            };
            ui.set_ws_shell_manage_saved_hint(false);
        });
    });

    let app_weak = app.as_weak();
    app.on_close_shell_manage(move || {
        let Some(ui) = app_weak.upgrade() else {
            return;
        };
        ui.set_ws_shell_manage_saved_hint(false);
        ui.set_ws_shell_manage_open(false);
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

fn connect_terminal_wheel(app: &AppWindow, state: Rc<RefCell<GuiState>>) {
    let st_wheel = Rc::clone(&state);
    let app_weak = app.as_weak();
    app.on_terminal_wheel(move |delta| {
        let Some(ui) = app_weak.upgrade() else {
            return false;
        };
        let mut s = st_wheel.borrow_mut();
        if s.current >= s.tabs.len() {
            return false;
        }
        if s.tabs[s.current].terminal_mode != crate::gui::state::TerminalMode::InteractiveAi {
            return false;
        }
        let current = s.current;
        let tab = &mut s.tabs[current];
        let vh = ui.get_ws_terminal_viewport_height_px().max(1.0);
        let max_scroll = ((tab.terminal_lines.len() as f32) * TERMINAL_ROW_HEIGHT_PX - vh).max(0.0);
        let current = if tab.interactive_follow_output {
            terminal_scroll_top_for_tab(tab, vh)
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
        ui.invoke_ws_apply_terminal_scroll_top_px(next);
        push_terminal_view_to_ui(&ui, tab, Some(next));
        true
    });
}

fn connect_terminal_viewport(app: &AppWindow, state: Rc<RefCell<GuiState>>) {
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

fn connect_toggles(app: &AppWindow, state: Rc<RefCell<GuiState>>, ipc: IpcBridge) {
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
