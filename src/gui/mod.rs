//! Slint UI shell: tab/workspace, ConPTY, composer, `@` file picker.

mod at_picker;
mod font_assets;
mod composer_sync;
mod fonts;
mod i18n;
mod ipc;
mod interactive_commands;
mod prompt_attachments;
mod shell_profiles;
mod gui_state;
mod run;
mod slint_ui;
mod state;
mod ui_sync;
#[cfg(target_os = "windows")]
mod windows_tray;

pub use run::run_gui;
