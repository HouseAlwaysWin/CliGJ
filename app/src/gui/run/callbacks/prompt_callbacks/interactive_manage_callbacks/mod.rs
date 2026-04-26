use std::cell::RefCell;
use std::rc::Rc;

use crate::gui::slint_ui::AppWindow;
use crate::gui::state::GuiState;

mod editor_callbacks;
mod save_callbacks;

pub(super) fn connect(app: &AppWindow, state: Rc<RefCell<GuiState>>) {
    editor_callbacks::connect(app, Rc::clone(&state));
    save_callbacks::connect(app, Rc::clone(&state));
}
