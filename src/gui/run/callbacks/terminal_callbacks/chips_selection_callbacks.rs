use std::cell::RefCell;
use std::rc::Rc;

use slint::{ComponentHandle, SharedString};

use crate::gui::reveal_in_explorer::reveal_path_in_file_manager;
use crate::gui::slint_ui::AppWindow;
use crate::gui::state::GuiState;
use crate::gui::run::helpers::{
    clear_all_prompt_files, copy_to_clipboard, remove_prompt_file_at,
    selected_text_from_terminal_lines,
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
        remove_prompt_file_at(&ui, &mut s, index as usize);
    });

    let st_clear = Rc::clone(&state);
    let app_weak = app.as_weak();
    app.on_prompt_chip_clear_requested(move || {
        let Some(ui) = app_weak.upgrade() else {
            return;
        };
        let mut s = st_clear.borrow_mut();
        clear_all_prompt_files(&ui, &mut s);
    });

    let st_path_explorer = Rc::clone(&state);
    app.on_prompt_path_chip_open_explorer_requested(move |index| {
        if index < 0 {
            return;
        }
        let idx = index as usize;
        let s = st_path_explorer.borrow();
        if s.current >= s.tabs.len() {
            return;
        }
        let current = s.current;
        let Some(path) = s.tabs[current].prompt_picked_files_abs.get(idx) else {
            return;
        };
        reveal_path_in_file_manager(path);
    });

    let st_img_explorer = Rc::clone(&state);
    app.on_prompt_image_open_explorer_requested(move |index| {
        if index < 0 {
            return;
        }
        let idx = index as usize;
        let s = st_img_explorer.borrow();
        if s.current >= s.tabs.len() {
            return;
        }
        let current = s.current;
        let Some(img) = s.tabs[current].prompt_picked_images.get(idx) else {
            return;
        };
        reveal_path_in_file_manager(&img.abs_path);
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
