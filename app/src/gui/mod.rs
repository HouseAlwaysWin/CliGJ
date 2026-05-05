//! Slint UI shell: tab/workspace, ConPTY, composer, `@` file picker.

mod at_picker;
mod composer_sync;
mod font_assets;
mod fonts;
mod gui_state;
mod i18n;
mod interactive_commands;
mod ipc;
mod open_in_vscode;
mod prompt_attachments;
mod reveal_in_explorer;
mod run;
mod shell_profiles;
mod slint_ui;
mod state;
mod terminal_menu;
mod ui_sync;
#[cfg(target_os = "windows")]
mod windows_tray;
mod zoom;

pub use run::run_gui;
