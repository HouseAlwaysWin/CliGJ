use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;

use slint::{ComponentHandle, Image, Model, SharedString};

use crate::gui::at_picker::commit_at_file_pick;
use crate::gui::interactive_commands::{self, spec_for_label};
use crate::gui::slint_ui::{AppWindow, TerminalHistoryWindow};
use crate::gui::state::{GuiState, TerminalMode};
use crate::gui::terminal_menu;
use crate::gui::zoom::{UI_ZOOM_STEP_PERCENT, adjust_ui_zoom_percent, reset_ui_zoom_percent};
use cligj_terminal::key_encoding::{self, MOD_ALT, MOD_CTRL, MOD_META, MOD_SHIFT};
use cligj_terminal::prompt_key::PromptKeyAction;

use crate::gui::run::helpers::{
    clipboard_file_paths_hdrop, clipboard_raster_image_file,
    inject_paths_and_images_from_paths, push_prompt_image,
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

fn inject_plain_interactive_key(ui: &AppWindow, state: &Rc<RefCell<GuiState>>, key: &str) -> bool {
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

fn clear_forwarded_interactive_prompt(ui: &AppWindow, state: &Rc<RefCell<GuiState>>) {
    ui.set_ws_prompt(SharedString::new());
    let mut s = state.borrow_mut();
    if s.current >= s.tabs.len() {
        return;
    }
    let current = s.current;
    let tab = &mut s.tabs[current];
    tab.prompt = SharedString::new();
    tab.composer_pty_mirror.clear();
    tab.history_cursor = None;
    tab.history_draft.clear();
    s.timer_snapshot = Some((current, String::new()));
}

fn is_printable_char(key: &str) -> bool {
    if key.is_empty() {
        return false;
    }
    let ch = key.chars().next().unwrap();
    !ch.is_control() && key.len() == ch.len_utf8()
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
    app.on_prompt_key_route(move |mod_mask, key, shift| {
        let Some(ui) = app_weak.upgrade() else {
            return false;
        };
        let key_str = key.as_str();

        // Debug: log Ctrl-modified keys to help diagnose shortcut issues.
        if mod_mask as u32 & MOD_CTRL != 0 {
            eprintln!(
                "CliGJ DEBUG: key_route ctrl key_str={:?} bytes={:?} mod_mask={} shift={}",
                key_str,
                key_str.as_bytes(),
                mod_mask,
                shift,
            );
        }

        if handle_zoom_shortcut(
            &ui,
            &history_window_keys,
            &st_keys,
            mod_mask as u32,
            key_str,
        ) {
            return true;
        }

        // Modal file picker: when open, intercept all keys for picker interaction.
        if ui.get_ws_at_picker_open() {
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
                    if choices.row_count() > 0 {
                        let idx = ui.get_ws_at_selected() as usize;
                        commit_at_file_pick(&ui, &mut *s, idx);
                    }
                    return true;
                }
                "Escape" => {
                    ui.set_ws_at_picker_open(false);
                    let mut s = st_keys.borrow_mut();
                    s.at_picker_filter.clear();
                    s.at_picker_query_snapshot.clear();
                    s.at_picker_open_snapshot = false;
                    return true;
                }
                "Backspace" => {
                    let mut s = st_keys.borrow_mut();
                    s.at_picker_filter.pop();
                    s.at_picker_query_snapshot.clear();
                    drop(s);
                    crate::gui::at_picker::refresh_file_picker_from_filter(&ui, &mut st_keys.borrow_mut());
                    return true;
                }
                _ => {
                    if is_printable_char(key_str) {
                        let mut s = st_keys.borrow_mut();
                        s.at_picker_filter.push_str(key_str);
                        s.at_picker_query_snapshot.clear();
                        drop(s);
                        crate::gui::at_picker::refresh_file_picker_from_filter(&ui, &mut st_keys.borrow_mut());
                        return true;
                    }
                    return true;
                }
            }
        }

        // Ctrl+V: HDROP paths (images vs files), then raster -> temp PNG path.
        if !ui.get_ws_at_picker_open()
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

        // Interactive menu Enter forwarding
        if matches!(key_str, "Return" | "\n" | "\r")
            && (mod_mask as u32 & (MOD_CTRL | MOD_SHIFT | MOD_ALT | MOD_META)) == 0
        {
            let has_menu = {
                let s = st_keys.borrow();
                s.current < s.tabs.len() && terminal_menu::has_terminal_menu(&s.tabs[s.current])
            };
            if has_menu && inject_plain_interactive_key(&ui, &st_keys, "Return") {
                clear_forwarded_interactive_prompt(&ui, &st_keys);
                return true;
            }
        }

        match cligj_terminal::prompt_key::route_prompt_key(mod_mask as u32, key_str, shift)
        {
            PromptKeyAction::Reject => false,
            PromptKeyAction::OpenFilePicker => {
                let mut s = st_keys.borrow_mut();
                s.at_picker_filter.clear();
                s.at_picker_query_snapshot.clear();
                drop(s);
                crate::gui::at_picker::open_file_picker(&ui, &mut st_keys.borrow_mut());
                true
            }
            PromptKeyAction::Submit => {
                schedule_submit_current_prompt(app_weak.clone(), Rc::clone(&st_keys));
                true
            }
            PromptKeyAction::PtyKey(k) => {
                let bytes = match key_encoding::encode_for_pty(mod_mask as u32, k.as_str()) {
                    Some(b) => b,
                    None => return false,
                };
                let inject_ok = {
                    let mut s = st_keys.borrow_mut();
                    if s.current < s.tabs.len() {
                        let cur = s.current;
                        if s.tabs[cur].terminal_mode
                            == crate::gui::state::TerminalMode::InteractiveAi
                        {
                            s.tabs[cur].interactive_follow_output = true;
                        }
                    }
                    s.inject_bytes_into_current(&ui, &bytes)
                };
                if let Err(e) = inject_ok {
                    eprintln!("CliGJ: pty key: {e}");
                }
                true
            }
        }
    });

    let st_pick = Rc::clone(&state);
    let app_weak = app.as_weak();
    app.on_at_picker_choose(move |index| {
        eprintln!("CliGJ DEBUG: at_picker_choose called, index={}", index);
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
