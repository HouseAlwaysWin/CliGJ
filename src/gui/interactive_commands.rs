//! Interactive launcher rows: loaded from config `[[ui.interactive_commands]]` (name + command).

use slint::{ModelRc, SharedString, VecModel};

use crate::core::config::AppConfig;
use crate::gui::slint_ui::{AppWindow, InteractiveCmdEditorRow};
use crate::gui::state::GuiState;

/// Seed when config has no `interactive_commands` (and no legacy customs).
pub(crate) fn default_interactive_command_pairs() -> Vec<(String, String)> {
    vec![
        ("Gemini".into(), "gemini".into()),
        ("Codex".into(), "codex".into()),
        ("Claude".into(), "claude".into()),
        ("Copilot".into(), "copilot".into()),
    ]
}

/// Returns `(rows, should_persist)` — persist when migrating legacy or seeding defaults.
pub(crate) fn load_from_config(cfg: &AppConfig) -> (Vec<(String, String)>, bool) {
    let primary = cfg.interactive_commands();
    if !primary.is_empty() {
        return (primary, false);
    }
    let legacy = cfg.interactive_custom_commands();
    if !legacy.is_empty() {
        let mut out = default_interactive_command_pairs();
        for (n, c) in legacy {
            if out.iter().all(|(on, _)| on != &n) {
                out.push((n, c));
            }
        }
        return (out, true);
    }
    (default_interactive_command_pairs(), true)
}

pub(crate) fn build_interactive_command_labels(gs: &GuiState) -> Vec<SharedString> {
    gs.interactive_commands
        .iter()
        .map(|(label, _)| SharedString::from(label.as_str()))
        .collect()
}

pub(crate) fn sync_interactive_command_choices_to_ui(ui: &AppWindow, gs: &GuiState) {
    let labels = build_interactive_command_labels(gs);
    ui.set_ws_interactive_command_choices(ModelRc::new(VecModel::from(labels)));
}

/// Load full name/command list into the interactive-command editor modal.
pub(crate) fn sync_interactive_manage_editor_to_ui(ui: &AppWindow, gs: &GuiState) {
    let rows: Vec<InteractiveCmdEditorRow> = gs
        .interactive_commands
        .iter()
        .map(|(n, c)| InteractiveCmdEditorRow {
            name: SharedString::from(n.as_str()),
            line: SharedString::from(c.as_str()),
            key_locked: is_reserved_preset_display_name(n),
            expanded: false,
        })
        .collect();
    ui.set_ws_interactive_manage_rows(ModelRc::new(VecModel::from(rows)));
}

/// Full PTY payload including line ending(s).
pub(crate) fn resolve_interactive_launch(line_label: &str, gs: &GuiState) -> Option<String> {
    for (name, cmd) in &gs.interactive_commands {
        if name == line_label {
            let c = cmd.trim();
            if c.is_empty() {
                return None;
            }
            if c.ends_with('\n') {
                return Some(c.to_string());
            }
            return Some(format!("{c}\r\n"));
        }
    }
    None
}

/// Built-in entries from seed config (same display `name` as in default_interactive_command_pairs).
pub(crate) fn is_reserved_preset_display_name(name: &str) -> bool {
    default_interactive_command_pairs()
        .iter()
        .any(|(n, _)| n == name)
}
