//! Interactive launcher rows: loaded from config `[[ui.interactive_commands]]`.

use slint::{ModelRc, SharedString, VecModel};

use crate::core::config::AppConfig;
use crate::gui::slint_ui::{AppWindow, InteractiveCmdEditorRow};
use crate::gui::state::GuiState;

pub(crate) type InteractiveCommandSpec = (String, String, usize);

/// Seed when config has no `interactive_commands` (and no legacy customs).
pub(crate) fn default_interactive_command_pairs() -> Vec<InteractiveCommandSpec> {
    vec![
        ("Gemini".into(), "gemini".into(), 8),
        ("Codex".into(), "codex".into(), 0),
        ("Claude".into(), "claude".into(), 0),
        ("Copilot".into(), "copilot".into(), 0),
    ]
}

/// Returns `(rows, should_persist)`; persist when migrating legacy or seeding defaults.
pub(crate) fn load_from_config(cfg: &AppConfig) -> (Vec<InteractiveCommandSpec>, bool) {
    let primary = cfg.interactive_commands();
    if !primary.is_empty() {
        return (primary, false);
    }
    let legacy = cfg.interactive_custom_commands();
    if !legacy.is_empty() {
        let mut out = default_interactive_command_pairs();
        for (n, c, pinned) in legacy {
            if out.iter().all(|(on, _, _)| on != &n) {
                out.push((n, c, pinned));
            }
        }
        return (out, true);
    }
    (default_interactive_command_pairs(), true)
}

pub(crate) fn build_interactive_command_labels(gs: &GuiState) -> Vec<SharedString> {
    gs.interactive_commands
        .iter()
        .map(|(label, _, _)| SharedString::from(label.as_str()))
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
        .map(|(n, c, pinned)| InteractiveCmdEditorRow {
            name: SharedString::from(n.as_str()),
            line: SharedString::from(c.as_str()),
            pinned_footer_lines: SharedString::from(pinned.to_string().as_str()),
            key_locked: is_reserved_preset_display_name(n),
            expanded: false,
            workspace_path: SharedString::new(),
        })
        .collect();
    ui.set_ws_interactive_manage_rows(ModelRc::new(VecModel::from(rows)));
}

/// Full PTY payload including line ending(s).
pub(crate) fn resolve_interactive_launch(line_label: &str, gs: &GuiState) -> Option<String> {
    for (name, cmd, _) in &gs.interactive_commands {
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

pub(crate) fn pinned_footer_lines_for_label(line_label: &str, gs: &GuiState) -> usize {
    gs.interactive_commands
        .iter()
        .find(|(name, _, _)| name == line_label)
        .map(|(_, _, pinned)| *pinned)
        .unwrap_or(0)
}

pub(crate) fn pinned_footer_lines_for_program(program: &str, gs: &GuiState) -> usize {
    pinned_footer_lines_for_specs(program, &gs.interactive_commands)
}

pub(crate) fn pinned_footer_lines_for_specs(
    program: &str,
    specs: &[InteractiveCommandSpec],
) -> usize {
    let needle = normalized_program_name(program);
    if needle.is_empty() {
        return 0;
    }
    specs
        .iter()
        .find(|(name, command, _)| {
            normalized_program_name(name) == needle
                || command
                    .split_whitespace()
                    .next()
                    .map(normalized_program_name)
                    .is_some_and(|candidate| candidate == needle)
        })
        .map(|(_, _, pinned)| *pinned)
        .unwrap_or(0)
}

pub(crate) fn launcher_program_for_label(line_label: &str, gs: &GuiState) -> Option<String> {
    gs.interactive_commands
        .iter()
        .find(|(name, _, _)| name == line_label)
        .and_then(|(_, command, _)| command.split_whitespace().next())
        .map(normalized_program_name)
        .filter(|value| !value.is_empty())
}

/// Built-in entries from seed config (same display `name` as in default_interactive_command_pairs).
pub(crate) fn is_reserved_preset_display_name(name: &str) -> bool {
    default_interactive_command_pairs()
        .iter()
        .any(|(n, _, _)| n == name)
}

pub(crate) fn normalized_program_name(text: &str) -> String {
    let trimmed = text.trim().trim_matches(|c| c == '"' || c == '\'');
    let leaf = trimmed.rsplit(['\\', '/']).next().unwrap_or(trimmed);
    leaf.strip_suffix(".exe").unwrap_or(leaf).to_ascii_lowercase()
}
