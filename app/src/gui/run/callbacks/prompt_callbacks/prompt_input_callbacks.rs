use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;

use slint::{ComponentHandle, Image, Model, SharedString};

use crate::gui::at_picker::commit_at_file_pick;
use crate::gui::composer_sync::sync_composer_line_to_conpty;
use crate::gui::interactive_commands::{self, spec_for_label};
use crate::gui::slint_ui::{AppWindow, TerminalHistoryWindow};
use crate::gui::state::{GuiState, TerminalMode};
use crate::gui::terminal_menu;
use crate::gui::ui_sync::tab_update_from_ui;
use crate::gui::zoom::{UI_ZOOM_STEP_PERCENT, adjust_ui_zoom_percent, reset_ui_zoom_percent};
use cligj_terminal::key_encoding::{self, MOD_ALT, MOD_CTRL, MOD_META, MOD_SHIFT};
use cligj_terminal::prompt_key::PromptKeyAction;
use cligj_workspace as workspace_files;

use super::super::is_pty_enter_key;
use crate::gui::run::helpers::{
    clipboard_file_paths_hdrop, clipboard_raster_image_file, contains_cjk_char,
    inject_paths_and_images_from_paths, is_local_prompt_edit_key, push_prompt_image,
};

fn schedule_submit_current_prompt(app_weak: slint::Weak<AppWindow>, state: Rc<RefCell<GuiState>>) {
    slint::Timer::single_shot(std::time::Duration::from_millis(0), move || {
        let Some(ui) = app_weak.upgrade() else {
            return;
        };
        let mut s = state.borrow_mut();
        if let Err(e) = s.submit_current_prompt(&ui) {
            eprintln!("CliGJ: prompt submit: {e}");
        }
    });
}

fn schedule_clipboard_paths_attach(
    app_weak: slint::Weak<AppWindow>,
    state: Rc<RefCell<GuiState>>,
    paths: Vec<PathBuf>,
) {
    slint::Timer::single_shot(std::time::Duration::from_millis(0), move || {
        let Some(ui) = app_weak.upgrade() else {
            return;
        };
        let mut s = state.borrow_mut();
        if let Err(e) = inject_paths_and_images_from_paths(&ui, &mut *s, &paths) {
            eprintln!("CliGJ: paste paths: {e}");
        }
    });
}

fn schedule_clipboard_image_attach(
    app_weak: slint::Weak<AppWindow>,
    state: Rc<RefCell<GuiState>>,
    path: PathBuf,
    img: Image,
) {
    slint::Timer::single_shot(std::time::Duration::from_millis(0), move || {
        let Some(ui) = app_weak.upgrade() else {
            return;
        };
        let mut s = state.borrow_mut();
        let abs = path.to_string_lossy().to_string();
        if let Err(e) = push_prompt_image(&ui, &mut *s, abs, img.clone()) {
            eprintln!("CliGJ: paste image: {e}");
        }
    });
}

fn handle_zoom_shortcut(
    ui: &AppWindow,
    history_window: &TerminalHistoryWindow,
    state: &Rc<RefCell<GuiState>>,
    mod_mask: u32,
    key: &str,
) -> bool {
    let has_ctrl = mod_mask & MOD_CTRL != 0;
    let has_alt_or_meta = mod_mask & (key_encoding::MOD_ALT | key_encoding::MOD_META) != 0;
    if !has_ctrl || has_alt_or_meta {
        return false;
    }

    let result = match key {
        "-" | "_" => {
            let mut s = state.borrow_mut();
            adjust_ui_zoom_percent(
                ui,
                Some(history_window),
                &mut *s,
                -UI_ZOOM_STEP_PERCENT,
                true,
            )
        }
        "+" | "=" => {
            let mut s = state.borrow_mut();
            adjust_ui_zoom_percent(
                ui,
                Some(history_window),
                &mut *s,
                UI_ZOOM_STEP_PERCENT,
                true,
            )
        }
        "0" => {
            let mut s = state.borrow_mut();
            reset_ui_zoom_percent(ui, Some(history_window), &mut *s, true)
        }
        _ => return false,
    };

    if let Err(e) = result {
        eprintln!("CliGJ: ui zoom shortcut: {e}");
    }
    true
}

fn inject_plain_interactive_key(
    ui: &AppWindow,
    state: &Rc<RefCell<GuiState>>,
    key: &str,
) -> bool {
    let Some(bytes) = terminal_menu::plain_key_bytes(key) else {
        return false;
    };
    let mut s = state.borrow_mut();
    if s.current >= s.tabs.len() {
        return false;
    }
    let current = s.current;
    if s.tabs[current].terminal_mode != TerminalMode::InteractiveAi {
        return false;
    }
    s.tabs[current].interactive_follow_output = true;
    if let Err(e) = s.inject_bytes_into_current(ui, &bytes) {
        eprintln!("CliGJ: plain interactive key: {e}");
    }
    true
}

pub(super) fn connect(
    app: &AppWindow,
    state: Rc<RefCell<GuiState>>,
    history_window: Rc<TerminalHistoryWindow>,
) {
    let st_submit = Rc::clone(&state);
    let app_weak = app.as_weak();
    app.on_submit_prompt(move || {
        schedule_submit_current_prompt(app_weak.clone(), Rc::clone(&st_submit));
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
    let history_window_keys = Rc::clone(&history_window);
    let app_weak = app.as_weak();
    app.on_prompt_key_route(move |raw_tty, mod_mask, key, shift| {
        let Some(ui) = app_weak.upgrade() else {
            return false;
        };
        let key_str = key.as_str();
        if handle_zoom_shortcut(
            &ui,
            &history_window_keys,
            &st_keys,
            mod_mask as u32,
            key_str,
        ) {
            return true;
        }
        // App-level prompt undo/redo (prevents Slint TextInput undo-stack panic).
        if !raw_tty
            && !ui.get_ws_at_picker_open()
            && (mod_mask as u32) & MOD_CTRL != 0
            && matches!(key_str, "z" | "Z")
            && (mod_mask as u32) & MOD_SHIFT == 0
        {
            let mut s = st_keys.borrow_mut();
            if s.current < s.tabs.len() {
                let cur = s.current;
                let tab = &mut s.tabs[cur];
                if let Some(prev) = tab.prompt_undo_stack.pop() {
                    let current = tab.prompt.to_string();
                    tab.prompt_redo_stack.push(current);
                    tab.prompt = SharedString::from(prev.as_str());
                    ui.set_ws_prompt(SharedString::from(prev.as_str()));
                    tab_update_from_ui(tab, &ui);
                    sync_composer_line_to_conpty(&ui, &mut s);
                }
            }
            return true;
        }
        if !raw_tty
            && !ui.get_ws_at_picker_open()
            && (mod_mask as u32) & MOD_CTRL != 0
            && (matches!(key_str, "y" | "Y")
                || ((mod_mask as u32) & key_encoding::MOD_SHIFT != 0
                    && matches!(key_str, "z" | "Z")))
        {
            let mut s = st_keys.borrow_mut();
            if s.current < s.tabs.len() {
                let cur = s.current;
                let tab = &mut s.tabs[cur];
                if let Some(next) = tab.prompt_redo_stack.pop() {
                    let current = tab.prompt.to_string();
                    tab.prompt_undo_stack.push(current);
                    tab.prompt = SharedString::from(next.as_str());
                    ui.set_ws_prompt(SharedString::from(next.as_str()));
                    tab_update_from_ui(tab, &ui);
                    sync_composer_line_to_conpty(&ui, &mut s);
                }
            }
            return true;
        }
        // Composer: Ctrl+V — HDROP paths (images vs files), then raster → temp PNG path.
        if !raw_tty
            && !ui.get_ws_at_picker_open()
            && (mod_mask as u32) & MOD_CTRL != 0
            && matches!(key_str, "v" | "V")
        {
            #[cfg(target_os = "windows")]
            if let Some(paths) = clipboard_file_paths_hdrop() {
                schedule_clipboard_paths_attach(app_weak.clone(), Rc::clone(&st_keys), paths);
                return true;
            }
            if let Some((path, img)) = clipboard_raster_image_file() {
                schedule_clipboard_image_attach(app_weak.clone(), Rc::clone(&st_keys), path, img);
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
                        drop(s);
                        schedule_submit_current_prompt(app_weak.clone(), Rc::clone(&st_keys));
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
        if !raw_tty
            && matches!(key_str, "UpArrow" | "DownArrow")
            && (mod_mask as u32 & (MOD_CTRL | MOD_SHIFT | MOD_ALT | MOD_META)) == MOD_ALT
            && inject_plain_interactive_key(&ui, &st_keys, key_str)
        {
            return true;
        }
        if !raw_tty
            && matches!(key_str, "Return" | "\n" | "\r")
            && (mod_mask as u32 & (MOD_CTRL | MOD_SHIFT | MOD_ALT | MOD_META)) == 0
        {
            let has_menu = {
                let s = st_keys.borrow();
                s.current < s.tabs.len() && terminal_menu::has_terminal_menu(&s.tabs[s.current])
            };
            if has_menu && inject_plain_interactive_key(&ui, &st_keys, "Return") {
                return true;
            }
        }
        match cligj_terminal::prompt_key::route_prompt_key(raw_tty, mod_mask as u32, key_str, shift)
        {
            PromptKeyAction::Reject => false,
            PromptKeyAction::ToggleRawInput => {
                let mut s = st_keys.borrow_mut();
                if let Err(e) = s.toggle_raw_input_current(&ui) {
                    eprintln!("CliGJ: raw input toggle: {e}");
                }
                true
            }
            PromptKeyAction::Submit => {
                schedule_submit_current_prompt(app_weak.clone(), Rc::clone(&st_keys));
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
                    if s.current < s.tabs.len() {
                        let cur = s.current;
                        let tab = &mut s.tabs[cur];
                        // If the user is actively typing in interactive raw mode, keep following
                        // the latest output so prompt redraws don't end up off-screen.
                        if raw_tty
                            && !is_nav
                            && tab.terminal_mode == crate::gui::state::TerminalMode::InteractiveAi
                        {
                            tab.interactive_follow_output = true;
                        }
                    }
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
        let interactive_spec = {
            let s = st_ai.borrow();
            spec_for_label(line_label.as_str(), &*s)
        };
        let Some(interactive_spec) = interactive_spec else {
            return;
        };
        let pinned_footer_lines = interactive_spec.pinned_footer_lines;
        let launcher_program = interactive_spec
            .command
            .split_whitespace()
            .next()
            .map(interactive_commands::normalized_program_name)
            .unwrap_or_default();

        if !interactive_spec.interactive_cli {
            let mut s = st_ai.borrow_mut();
            if let Err(e) = s.inject_bytes_into_current(&ui, launch_cmd.as_bytes()) {
                eprintln!("CliGJ: inject command: {e}");
            }
            return;
        }

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
        s.tabs[current].interactive_markers = interactive_spec.markers;
        s.tabs[current].interactive_archive_repainted_frames =
            interactive_spec.archive_repainted_frames;
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
}
