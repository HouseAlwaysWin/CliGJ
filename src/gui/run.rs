use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::mpsc;
use std::time::Duration;

use arboard::Clipboard;
use slint::{ComponentHandle, Model, ModelRc, SharedString, Timer, VecModel};
#[cfg(target_os = "windows")]
use slint::winit_030::{winit, EventResult, WinitWindowAccessor};

use crate::terminal::key_encoding;
use crate::terminal::prompt_key::PromptKeyAction;
use crate::terminal::render::ColoredLine;
use crate::workspace_files;

use super::at_picker::{commit_at_file_pick, sync_at_file_picker};
use super::composer_sync::sync_composer_line_to_conpty;
use super::slint_ui::AppWindow;
use super::state::{GuiState, TabState, TerminalChunk};
use super::ui_sync::{
    colored_lines_to_model, load_tab_to_ui, sync_tab_count, tab_update_from_ui,
};

pub fn run_gui(inject_file: Option<PathBuf>) {
    #[cfg(target_os = "windows")]
    {
        if let Err(e) = slint::BackendSelector::new()
            .backend_name("winit".into())
            .select()
        {
            eprintln!("CliGJ: select winit backend failed: {e}");
        }
    }
    let app = AppWindow::new().expect("failed to build app window");

    let titles = Rc::new(VecModel::from(vec![SharedString::from("工作階段 1")]));

    let (tx, rx) = mpsc::channel::<TerminalChunk>();

    let state = Rc::new(RefCell::new(GuiState {
        tabs: vec![TabState::new(1, tx.clone())],
        titles: Rc::clone(&titles),
        current: 0,
        next_id: 2,
        tx,
        pending_scroll: false,
        workspace_file_cache: Vec::new(),
        workspace_file_cache_root: None,
        at_picker_query_snapshot: String::new(),
    }));

    app.set_tab_titles(ModelRc::from(Rc::clone(&titles)));
    sync_tab_count(&app, state.borrow().tabs.len());
    load_tab_to_ui(&app, &state.borrow().tabs[0]);

    #[cfg(target_os = "windows")]
    {
        let state_for_drop = Rc::clone(&state);
        let app_weak = app.as_weak();
        app.window().on_winit_window_event(move |_window, event| {
            match event {
                winit::event::WindowEvent::DroppedFile(path) => {
                    let Some(ui) = app_weak.upgrade() else {
                        return EventResult::Propagate;
                    };
                    let mut s = state_for_drop.borrow_mut();
                    if let Err(e) = inject_path_into_current(&ui, &mut s, path.as_path()) {
                        eprintln!("CliGJ: dropped file {}: {e}", path.display());
                    }
                    EventResult::PreventDefault
                }
                _ => EventResult::Propagate,
            }
        });
    }

    let state_for_stream = Rc::clone(&state);
    let app_weak = app.as_weak();
    let timer = Timer::default();
    timer.start(
        slint::TimerMode::Repeated,
        Duration::from_millis(16),
        move || {
            let Some(ui) = app_weak.upgrade() else { return; };
            let mut s = state_for_stream.borrow_mut();
            let current_id = s.tabs.get(s.current).map(|t| t.id);
            let mut current_changed = false;
            let mut processed = 0usize;
            const MAX_CHUNKS_PER_TICK: usize = 96;
            while processed < MAX_CHUNKS_PER_TICK {
                let Ok(chunk) = rx.try_recv() else { break; };
                processed += 1;
                for tab in s.tabs.iter_mut() {
                    if tab.id != chunk.tab_id {
                        continue;
                    }
                    if let Some(v) = chunk.set_auto_scroll {
                        tab.auto_scroll = v;
                    }
                    if chunk.replace {
                        tab.terminal_text = chunk.text.clone();
                        tab.terminal_lines = chunk.lines.clone();
                    } else {
                        tab.append_terminal(&chunk.text);
                    }
                    if current_id == Some(chunk.tab_id) {
                        current_changed = true;
                        if tab.auto_scroll {
                            s.pending_scroll = true;
                        }
                    }
                    break;
                }
            }

            if current_changed {
                let text = s.tabs[s.current].terminal_text.clone();
                ui.set_ws_terminal_text(SharedString::from(text.as_str()));
                ui.set_ws_terminal_lines(colored_lines_to_model(
                    &s.tabs[s.current].terminal_lines,
                ));
                if !s.tabs[s.current].auto_scroll {
                    ui.invoke_ws_scroll_terminal_to_top();
                }
            }

            if s.pending_scroll {
                ui.invoke_ws_scroll_terminal_to_bottom();
                s.pending_scroll = false;
            }
        },
    );

    let state_for_tab = Rc::clone(&state);
    let app_weak = app.as_weak();
    app.on_tab_changed(move |new_index| {
        let Some(ui) = app_weak.upgrade() else {
            return;
        };
        let mut s = state_for_tab.borrow_mut();
        if let Err(e) = s.switch_tab(new_index as usize, &ui) {
            eprintln!("CliGJ: tab switch: {e}");
        }
    });

    let state_for_close = Rc::clone(&state);
    let app_weak = app.as_weak();
    app.on_tab_close_requested(move |index| {
        let Some(ui) = app_weak.upgrade() else {
            return;
        };
        let mut s = state_for_close.borrow_mut();
        if let Err(e) = s.close_tab(index as usize, &ui) {
            eprintln!("CliGJ: close tab: {e}");
        }
    });

    let state_for_new = Rc::clone(&state);
    let app_weak = app.as_weak();
    app.on_new_tab_requested(move || {
        let Some(ui) = app_weak.upgrade() else {
            return;
        };
        let mut s = state_for_new.borrow_mut();
        if let Err(e) = s.add_tab(&ui) {
            eprintln!("CliGJ: new tab: {e}");
        }
    });

    let state_for_cmd = Rc::clone(&state);
    let app_weak = app.as_weak();
    app.on_cmd_type_changed(move |kind| {
        let Some(ui) = app_weak.upgrade() else {
            return;
        };
        let mut s = state_for_cmd.borrow_mut();
        if let Err(e) = s.change_current_cmd_type(kind.as_str(), &ui) {
            eprintln!("CliGJ: cmd type change: {e}");
        }
    });

    let state_for_submit = Rc::clone(&state);
    let app_weak = app.as_weak();
    app.on_submit_prompt(move || {
        let Some(ui) = app_weak.upgrade() else {
            return;
        };
        let mut s = state_for_submit.borrow_mut();
        if let Err(e) = s.submit_current_prompt(&ui) {
            eprintln!("CliGJ: prompt submit: {e}");
        }
    });

    let state_for_hist_prev = Rc::clone(&state);
    let app_weak = app.as_weak();
    app.on_prompt_history_prev(move || {
        let Some(ui) = app_weak.upgrade() else { return; };
        let mut s = state_for_hist_prev.borrow_mut();
        if let Err(e) = s.history_prev_current_prompt(&ui) {
            eprintln!("CliGJ: history prev: {e}");
        }
    });

    let state_for_hist_next = Rc::clone(&state);
    let app_weak = app.as_weak();
    app.on_prompt_history_next(move || {
        let Some(ui) = app_weak.upgrade() else { return; };
        let mut s = state_for_hist_next.borrow_mut();
        if let Err(e) = s.history_next_current_prompt(&ui) {
            eprintln!("CliGJ: history next: {e}");
        }
    });

    let state_for_prompt_keys = Rc::clone(&state);
    let app_weak = app.as_weak();
    app.on_prompt_key_route(move |raw_tty, mod_mask, key, shift| {
        let Some(ui) = app_weak.upgrade() else {
            return false;
        };
        let key_str = key.as_str();
        if raw_tty && is_local_prompt_edit_key(mod_mask as u32, key_str) && !ui.get_ws_prompt().is_empty() {
            // If user has text in the composer, let TextEdit handle basic edit keys even in Raw.
            // This restores mouse-select + delete behavior in the prompt box.
            return false;
        }
        if raw_tty && contains_cjk_char(key_str) {
            let mut s = state_for_prompt_keys.borrow_mut();
            if let Err(e) = s.toggle_raw_input_current(&ui) {
                eprintln!("CliGJ: raw input auto-toggle (CJK): {e}");
            }
            // Let TextEdit handle this key so IME/CJK text lands in the UI composer.
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
                    let mut s = state_for_prompt_keys.borrow_mut();
                    let idx = ui.get_ws_at_selected() as usize;
                    commit_at_file_pick(&ui, &mut *s, idx);
                    return true;
                }
                "Escape" => {
                    let prompt = ui.get_ws_prompt().to_string();
                    let new_p = workspace_files::strip_active_at_segment(&prompt);
                    ui.set_ws_prompt(SharedString::from(new_p.as_str()));
                    ui.set_ws_at_picker_open(false);
                    let mut s = state_for_prompt_keys.borrow_mut();
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
                let mut s = state_for_prompt_keys.borrow_mut();
                if let Err(e) = s.toggle_raw_input_current(&ui) {
                    eprintln!("CliGJ: raw input toggle: {e}");
                }
                true
            }
            PromptKeyAction::Submit => {
                let mut s = state_for_prompt_keys.borrow_mut();
                if let Err(e) = s.submit_current_prompt(&ui) {
                    eprintln!("CliGJ: prompt submit: {e}");
                }
                true
            }
            PromptKeyAction::HistoryPrev => {
                let mut s = state_for_prompt_keys.borrow_mut();
                if let Err(e) = s.history_prev_current_prompt(&ui) {
                    eprintln!("CliGJ: history prev: {e}");
                }
                true
            }
            PromptKeyAction::HistoryNext => {
                let mut s = state_for_prompt_keys.borrow_mut();
                if let Err(e) = s.history_next_current_prompt(&ui) {
                    eprintln!("CliGJ: history next: {e}");
                }
                true
            }
            PromptKeyAction::PtyKey(k) => {
                let Some(bytes) = key_encoding::encode_for_pty(mod_mask as u32, k.as_str()) else {
                    return false;
                };
                let mut s = state_for_prompt_keys.borrow_mut();
                if let Err(e) = s.inject_bytes_into_current(&ui, &bytes) {
                    eprintln!("CliGJ: pty key: {e}");
                }
                true
            }
        }
    });

    let state_for_at_pick = Rc::clone(&state);
    let app_weak_atpick = app.as_weak();
    app.on_at_picker_choose(move |index| {
        let Some(ui) = app_weak_atpick.upgrade() else {
            return;
        };
        if index < 0 {
            return;
        }
        let mut s = state_for_at_pick.borrow_mut();
        commit_at_file_pick(&ui, &mut *s, index as usize);
    });

    let state_for_chip_remove = Rc::clone(&state);
    let app_weak = app.as_weak();
    app.on_prompt_chip_remove_requested(move |index| {
        let Some(ui) = app_weak.upgrade() else {
            return;
        };
        if index < 0 {
            return;
        }
        let mut s = state_for_chip_remove.borrow_mut();
        if s.current >= s.tabs.len() {
            return;
        }
        let current = s.current;
        let idx = index as usize;
        if idx >= s.tabs[current].prompt_picked_files_abs.len() {
            return;
        }
        s.tabs[current].prompt_picked_files_abs.remove(idx);
        load_tab_to_ui(&ui, &s.tabs[current]);
    });

    let state_for_chip_clear = Rc::clone(&state);
    let app_weak = app.as_weak();
    app.on_prompt_chip_clear_requested(move || {
        let Some(ui) = app_weak.upgrade() else {
            return;
        };
        let mut s = state_for_chip_clear.borrow_mut();
        if s.current >= s.tabs.len() {
            return;
        }
        let current = s.current;
        s.tabs[current].prompt_picked_files_abs.clear();
        load_tab_to_ui(&ui, &s.tabs[current]);
    });

    let state_for_selection = Rc::clone(&state);
    let app_weak = app.as_weak();
    app.on_terminal_selection_committed(move |sr, sc, er, ec| {
        let Some(ui) = app_weak.upgrade() else {
            return;
        };
        let mut s = state_for_selection.borrow_mut();
        if s.current >= s.tabs.len() {
            return;
        }
        let selected = selected_text_from_terminal_lines(
            &s.tabs[s.current],
            sr,
            sc,
            er,
            ec,
        );
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

    let state_for_at_sync = Rc::clone(&state);
    let app_weak_atsync = app.as_weak();
    let timer_at = Timer::default();
    timer_at.start(
        slint::TimerMode::Repeated,
        Duration::from_millis(120),
        move || {
            let Some(ui) = app_weak_atsync.upgrade() else {
                return;
            };
            let mut s = state_for_at_sync.borrow_mut();
            auto_disable_raw_on_cjk_prompt(&ui, &mut s);
            sync_composer_line_to_conpty(&ui, &mut *s);
            sync_at_file_picker(&ui, &mut *s);
        },
    );

    let state_for_raw_toggle = Rc::clone(&state);
    let app_weak = app.as_weak();
    app.on_toggle_raw_input_requested(move || {
        let Some(ui) = app_weak.upgrade() else { return; };
        let mut s = state_for_raw_toggle.borrow_mut();
        if let Err(e) = s.toggle_raw_input_current(&ui) {
            eprintln!("CliGJ: raw input toggle: {e}");
        }
    });

    let state_for_select_toggle = Rc::clone(&state);
    let app_weak = app.as_weak();
    app.on_toggle_terminal_select_mode_requested(move || {
        let Some(ui) = app_weak.upgrade() else { return; };
        let mut s = state_for_select_toggle.borrow_mut();
        if let Err(e) = s.toggle_terminal_select_mode_current(&ui) {
            eprintln!("CliGJ: terminal select mode toggle: {e}");
        }
    });

    let state_for_rename = Rc::clone(&state);
    let app_weak = app.as_weak();
    app.on_rename_tab_requested(move |index| {
        let Some(ui) = app_weak.upgrade() else { return; };
        let s = state_for_rename.borrow_mut();
        if index < 0 || (index as usize) >= s.tabs.len() { return; }
        let title = s.titles.row_data(index as usize).unwrap_or_else(|| SharedString::from("Tab"));
        ui.set_ws_rename_index(index);
        ui.set_ws_rename_text(title);
        ui.set_ws_rename_open(true);
    });

    let state_for_rename_commit = Rc::clone(&state);
    let app_weak = app.as_weak();
    app.on_rename_commit(move |index, text| {
        let Some(ui) = app_weak.upgrade() else { return; };
        let s = state_for_rename_commit.borrow_mut();
        if index < 0 || (index as usize) >= s.tabs.len() { return; }
        s.titles.set_row_data(index as usize, SharedString::from(text.as_str()));
        ui.set_ws_rename_open(false);
    });

    let app_weak = app.as_weak();
    app.on_rename_cancel(move || {
        let Some(ui) = app_weak.upgrade() else { return; };
        ui.set_ws_rename_open(false);
    });

    let state_for_move = Rc::clone(&state);
    let app_weak = app.as_weak();
    app.on_move_tab_requested(move |from, to| {
        let Some(ui) = app_weak.upgrade() else { return; };
        let mut s = state_for_move.borrow_mut();
        let _ = s.move_tab(from as usize, to as usize, &ui);
    });

    let state_for_inject = Rc::clone(&state);
    let app_weak = app.as_weak();
    app.on_inject_file_requested(move || {
        let Some(ui) = app_weak.upgrade() else {
            return;
        };
        let Some(path) = rfd::FileDialog::new().pick_file() else {
            return;
        };
        let mut s = state_for_inject.borrow_mut();
        if let Err(e) = inject_path_into_current(&ui, &mut s, path.as_path()) {
            eprintln!("CliGJ: inject file {}: {e}", path.display());
        }
    });

    let _inject_startup_timer: Option<Timer> = inject_file.map(|path| {
        let state_inj = Rc::clone(&state);
        let app_weak = app.as_weak();
        let timer = Timer::default();
        timer.start(
            slint::TimerMode::SingleShot,
            Duration::from_millis(500),
            move || {
                let Some(ui) = app_weak.upgrade() else {
                    return;
                };
                let mut s = state_inj.borrow_mut();
                if let Err(e) = inject_path_into_current(&ui, &mut s, path.as_path()) {
                    eprintln!("CliGJ: --inject-file {}: {e}", path.display());
                }
            },
        );
        timer
    });

    let _at_file_sync_timer = timer_at;

    app.run().expect("failed to run app window");
}

fn normalize_text_for_conpty(text: &str) -> Vec<u8> {
    text.replace("\r\n", "\n").replace('\n', "\r\n").into_bytes()
}

fn inject_path_into_current(ui: &AppWindow, s: &mut GuiState, path: &Path) -> Result<(), String> {
    let text = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
    let bytes = normalize_text_for_conpty(&text);
    s.inject_bytes_into_current(ui, &bytes)
}

fn auto_disable_raw_on_cjk_prompt(ui: &AppWindow, s: &mut GuiState) {
    if s.current >= s.tabs.len() {
        return;
    }
    if !s.tabs[s.current].raw_input_mode {
        return;
    }
    let prompt = ui.get_ws_prompt().to_string();
    if !contains_cjk_char(&prompt) {
        return;
    }
    if let Err(e) = s.toggle_raw_input_current(ui) {
        eprintln!("CliGJ: raw input auto-toggle (prompt CJK): {e}");
    }
}

fn contains_cjk_char(text: &str) -> bool {
    text.chars().any(|ch| {
        matches!(
            ch as u32,
            0x3400..=0x4DBF // CJK Unified Ideographs Extension A
                | 0x4E00..=0x9FFF // CJK Unified Ideographs
                | 0xF900..=0xFAFF // CJK Compatibility Ideographs
                | 0x20000..=0x2CEAF // CJK Unified Ideographs Extension B-E
                | 0x2EBF0..=0x2EE5F // CJK Unified Ideographs Extension I
                | 0x3000..=0x303F // CJK Symbols and Punctuation
                | 0xFF00..=0xFFEF // Halfwidth and Fullwidth Forms
        )
    })
}

fn is_local_prompt_edit_key(mod_mask: u32, key: &str) -> bool {
    if mod_mask & (key_encoding::MOD_CTRL | key_encoding::MOD_ALT | key_encoding::MOD_META) != 0 {
        return false;
    }
    matches!(
        key,
        "Backspace" | "Delete" | "LeftArrow" | "RightArrow" | "Home" | "End"
    )
}

fn colored_line_plain_text(line: &ColoredLine) -> String {
    line.spans.iter().fold(String::new(), |mut acc, s| {
        acc.push_str(s.text.as_str());
        acc
    })
}

/// Inclusive character slice on one logical line (Unicode scalar indices, matching Slint `char-count`).
fn slice_line_chars_inclusive(line: &ColoredLine, start: usize, end_inclusive: usize) -> String {
    let s = colored_line_plain_text(line);
    let chars: Vec<char> = s.chars().collect();
    let n = chars.len();
    if n == 0 || start > end_inclusive {
        return String::new();
    }
    let start = start.min(n - 1);
    let end_inclusive = end_inclusive.min(n - 1);
    chars[start..=end_inclusive].iter().collect()
}

fn slice_line_from_char(line: &ColoredLine, start: usize) -> String {
    let s = colored_line_plain_text(line);
    let chars: Vec<char> = s.chars().collect();
    let n = chars.len();
    if start >= n {
        return String::new();
    }
    chars[start..].iter().collect()
}

fn slice_line_to_char_inclusive(line: &ColoredLine, end_inclusive: usize) -> String {
    slice_line_chars_inclusive(line, 0, end_inclusive)
}

fn selected_text_from_terminal_lines(tab: &TabState, sr: i32, sc: i32, er: i32, ec: i32) -> String {
    if tab.terminal_lines.is_empty() {
        return String::new();
    }
    let sr = sr.max(0) as usize;
    let sc = sc.max(0) as usize;
    let er = er.max(0) as usize;
    let ec = ec.max(0) as usize;
    let max_row = tab.terminal_lines.len() - 1;
    let sr = sr.min(max_row);
    let er = er.min(max_row);
    if sr > er {
        return String::new();
    }
    let mut out = String::new();
    if sr == er {
        let line = &tab.terminal_lines[sr];
        return slice_line_chars_inclusive(line, sc, ec);
    }
    for row_idx in sr..=er {
        if row_idx > sr {
            out.push('\n');
        }
        let line = &tab.terminal_lines[row_idx];
        if row_idx == sr {
            out.push_str(&slice_line_from_char(line, sc));
        } else if row_idx == er {
            out.push_str(&slice_line_to_char_inclusive(line, ec));
        } else {
            out.push_str(&colored_line_plain_text(line));
        }
    }
    out
}

fn copy_to_clipboard(text: &str) -> Result<(), String> {
    let mut clipboard = Clipboard::new().map_err(|e| e.to_string())?;
    clipboard.set_text(text.to_string()).map_err(|e| e.to_string())
}
