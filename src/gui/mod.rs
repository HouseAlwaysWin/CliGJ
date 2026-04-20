//! Slint UI shell: tab/workspace, ConPTY, composer, `@` file picker.

mod at_picker;
mod composer_sync;
mod interactive_commands;
mod shell_profiles;
mod gui_state;
mod run;
mod slint_ui;
mod state;
mod ui_sync;

pub use run::run_gui;
