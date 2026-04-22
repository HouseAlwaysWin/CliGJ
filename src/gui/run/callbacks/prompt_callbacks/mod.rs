use std::cell::RefCell;
use std::rc::Rc;

use crate::gui::slint_ui::AppWindow;
use crate::gui::state::GuiState;

mod interactive_manage_callbacks;
mod prompt_input_callbacks;
mod shell_manage_callbacks;

pub(super) fn connect_prompt_and_picker(app: &AppWindow, state: Rc<RefCell<GuiState>>) {
    prompt_input_callbacks::connect(app, Rc::clone(&state));
    interactive_manage_callbacks::connect(app, Rc::clone(&state));
    shell_manage_callbacks::connect(app, Rc::clone(&state));
}
