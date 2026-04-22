use std::cell::RefCell;
use std::collections::HashSet;
use std::rc::Rc;

use slint::{ComponentHandle, Model, SharedString};

use crate::core::config::AppConfig;
use crate::gui::at_picker::commit_at_file_pick;
use crate::gui::composer_sync::sync_composer_line_to_conpty;
use crate::gui::fonts::{
    normalize_terminal_cjk_fallback_font_family, normalize_terminal_font_family,
};
use crate::gui::interactive_commands::{
    self, launcher_program_for_label, pinned_footer_lines_for_label,
    pinned_footer_lines_for_specs, sync_interactive_command_choices_to_ui,
    sync_interactive_manage_editor_to_ui,
};
use crate::gui::i18n::apply_slint_language_from_shell_setting;
use crate::gui::shell_profiles::{
    default_shell_profile_name, normalize_shell_profile_command, sync_shell_manage_editor_to_ui,
    sync_shell_profile_choices_to_ui,
};
use crate::gui::slint_ui::{AppWindow, InteractiveCmdEditorRow};
use crate::gui::state::{GuiState, TerminalMode};
use crate::gui::ui_sync::tab_update_from_ui;
use crate::terminal::key_encoding::{self, MOD_CTRL};
use crate::terminal::prompt_key::PromptKeyAction;
use crate::workspace_files;

use super::{
    is_pty_enter_key, model_interactive_editor_rows, refresh_terminal_tab_view, set_manage_rows,
    set_shell_manage_rows,
};
use crate::gui::run::helpers::{
    clipboard_raster_image_file, contains_cjk_char, inject_paths_and_images_from_paths,
    is_local_prompt_edit_key,
};

pub(super) fn connect_prompt_and_picker(app: &AppWindow, state: Rc<RefCell<GuiState>>) {
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
            if let Some(paths) = super::super::helpers::clipboard_file_paths_hdrop() {
                let mut s = st_keys.borrow_mut();
                if let Err(e) = inject_paths_and_images_from_paths(&ui, &mut *s, &paths) {
                    eprintln!("CliGJ: paste paths: {e}");
                }
                return true;
            }
            if let Some((path, img)) = clipboard_raster_image_file() {
                let mut s = st_keys.borrow_mut();
                let abs = path.to_string_lossy().to_string();
                if let Err(e) = super::super::helpers::push_prompt_image(&ui, &mut *s, abs, img) {
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
                let is_nav = matches!(
                    k.as_str(),
                    "LeftArrow" | "RightArrow" | "Home" | "End" | "Backspace" | "Delete"
                );
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
        let pinned_footer_lines = {
            let s = st_ai.borrow();
            pinned_footer_lines_for_label(line_label.as_str(), &*s)
        };
        let launcher_program = {
            let s = st_ai.borrow();
            launcher_program_for_label(line_label.as_str(), &*s).unwrap_or_default()
        };

        let mut s = st_ai.borrow_mut();
        if s.current >= s.tabs.len() {
            return;
        }
        if let Err(e) = s.respawn_conpty_for_interactive_command(&ui, pinned_footer_lines) {
            eprintln!("CliGJ: interactive command PTY restart: {e}");
            return;
        }
        let current = s.current;
        s.tabs[current].interactive_launcher_program = launcher_program;
        drop(s);

        let app_weak_inner = ui.as_weak();
        let st_ai_inner = Rc::clone(&st_ai);
        slint::Timer::single_shot(std::time::Duration::from_millis(300), move || {
            let Some(ui) = app_weak_inner.upgrade() else {
                return;
            };
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
        ui.set_ws_shell_startup_terminal_font_family(SharedString::from(
            s.startup_terminal_font_family.as_str(),
        ));
        ui.set_ws_shell_startup_terminal_cjk_fallback_font_family(SharedString::from(
            s.startup_terminal_cjk_fallback_font_family.as_str(),
        ));
        drop(s);
        ui.set_ws_shell_settings_nav(SharedString::from("startup"));
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
            pinned_footer_lines: SharedString::new(),
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
        let terminal_font_family =
            normalize_terminal_font_family(ui.get_ws_shell_startup_terminal_font_family().as_str())
                .to_string();
        let terminal_cjk_fallback_font_family = normalize_terminal_cjk_fallback_font_family(
            ui.get_ws_shell_startup_terminal_cjk_fallback_font_family()
                .as_str(),
        )
        .to_string();
        let choices = ui.get_ws_cmd_type_choices();
        if choices.row_count() == 0 {
            eprintln!("CliGJ: no shell profiles available");
            return;
        }
        if (0..choices.row_count())
            .all(|i| choices.row_data(i).unwrap_or_default().to_string() != profile)
        {
            profile = choices.row_data(0).unwrap_or_default().to_string();
            ui.set_ws_shell_startup_default_profile(SharedString::from(profile.as_str()));
        }
        match AppConfig::load_or_default() {
            Ok(mut cfg) => {
                cfg.set_ui_language(language.as_str());
                cfg.set_default_shell_profile(profile.as_str());
                cfg.set_terminal_font_family(terminal_font_family.as_str());
                cfg.set_terminal_cjk_fallback_font_family(terminal_cjk_fallback_font_family.as_str());
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
            s.startup_terminal_font_family = terminal_font_family.clone();
            s.startup_terminal_cjk_fallback_font_family = terminal_cjk_fallback_font_family.clone();
        }
        apply_slint_language_from_shell_setting(&ui, language.as_str());
        ui.set_ws_terminal_font_family(SharedString::from(terminal_font_family.as_str()));
        ui.set_ws_terminal_cjk_fallback_font_family(SharedString::from(
            terminal_cjk_fallback_font_family.as_str(),
        ));
        ui.set_ws_shell_startup_terminal_font_family(SharedString::from(
            terminal_font_family.as_str(),
        ));
        ui.set_ws_shell_startup_terminal_cjk_fallback_font_family(SharedString::from(
            terminal_cjk_fallback_font_family.as_str(),
        ));
        ui.set_ws_shell_manage_saved_hint(true);
        let ui_weak = ui.as_weak();
        slint::Timer::single_shot(std::time::Duration::from_millis(1600), move || {
            let Some(ui) = ui_weak.upgrade() else {
                return;
            };
            ui.set_ws_shell_manage_saved_hint(false);
        });
    });

    let st_lang_sync = Rc::clone(&state);
    let app_weak_lang = app.as_weak();
    app.on_shell_ui_language_changed(move |lang| {
        let Some(ui) = app_weak_lang.upgrade() else {
            return;
        };
        st_lang_sync.borrow_mut().startup_language = lang.to_string();
        apply_slint_language_from_shell_setting(&ui, lang.as_str());
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
