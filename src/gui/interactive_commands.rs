//! Interactive launcher rows: loaded from config `[[ui.interactive_commands]]`.

use slint::{ModelRc, SharedString, VecModel};

use crate::core::config::{AppConfig, InteractiveCommandConfig};
use crate::gui::slint_ui::{AppWindow, InteractiveCmdEditorRow};
use crate::gui::state::GuiState;

pub(crate) type InteractiveCommandSpec = InteractiveCommandConfig;

/// Seed when config has no `interactive_commands` (and no legacy customs).
pub(crate) fn default_interactive_command_pairs() -> Vec<InteractiveCommandSpec> {
    vec![
        InteractiveCommandConfig::with_defaults("Gemini".into(), "gemini".into(), 8),
        InteractiveCommandConfig::with_defaults("Codex".into(), "codex".into(), 0),
        InteractiveCommandConfig::with_defaults("Claude".into(), "claude".into(), 0),
        InteractiveCommandConfig::with_defaults("Copilot".into(), "copilot".into(), 0),
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
        for spec in legacy {
            if out.iter().all(|existing| existing.name != spec.name) {
                out.push(spec);
            }
        }
        return (out, true);
    }
    (default_interactive_command_pairs(), true)
}

pub(crate) fn build_interactive_command_labels(gs: &GuiState) -> Vec<SharedString> {
    gs.interactive_commands
        .iter()
        .map(|spec| SharedString::from(spec.name.as_str()))
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
        .map(|spec| InteractiveCmdEditorRow {
            name: SharedString::from(spec.name.as_str()),
            line: SharedString::from(spec.command.as_str()),
            interactive_cli: spec.interactive_cli,
            pinned_footer_lines: SharedString::from(
                spec.pinned_footer_lines.to_string().as_str(),
            ),
            markers: SharedString::from(spec.markers.join(", ").as_str()),
            archive_repainted_frames: spec.archive_repainted_frames,
            key_locked: is_reserved_preset_display_name(spec.name.as_str()),
            expanded: false,
            workspace_path: SharedString::new(),
        })
        .collect();
    ui.set_ws_interactive_manage_rows(ModelRc::new(VecModel::from(rows)));
}

/// Full PTY payload including line ending(s).
pub(crate) fn resolve_interactive_launch(line_label: &str, gs: &GuiState) -> Option<String> {
    for spec in &gs.interactive_commands {
        if spec.name == line_label {
            let c = spec.command.trim();
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

pub(crate) fn pinned_footer_lines_for_specs(
    program: &str,
    specs: &[InteractiveCommandSpec],
) -> usize {
    spec_for_program_in_specs(program, specs)
        .map(|spec| spec.pinned_footer_lines)
        .unwrap_or(0)
}

pub(crate) fn spec_for_program_in_specs<'a>(
    program: &str,
    specs: &'a [InteractiveCommandSpec],
) -> Option<&'a InteractiveCommandSpec> {
    let needle = normalized_program_name(program);
    if needle.is_empty() {
        return None;
    }
    specs
        .iter()
        .find(|spec| {
            normalized_program_name(spec.name.as_str()) == needle
                || spec
                    .command
                    .split_whitespace()
                    .next()
                    .map(normalized_program_name)
                    .is_some_and(|candidate| candidate == needle)
        })
}

pub(crate) fn spec_for_label(line_label: &str, gs: &GuiState) -> Option<InteractiveCommandSpec> {
    gs.interactive_commands
        .iter()
        .find(|spec| spec.name == line_label)
        .cloned()
}

pub(crate) fn spec_for_program(program: &str, gs: &GuiState) -> Option<InteractiveCommandSpec> {
    spec_for_program_in_specs(program, &gs.interactive_commands).cloned()
}

/// Built-in entries from seed config (same display `name` as in default_interactive_command_pairs).
pub(crate) fn is_reserved_preset_display_name(name: &str) -> bool {
    default_interactive_command_pairs()
        .iter()
        .any(|spec| spec.name == name)
}

pub(crate) fn normalized_program_name(text: &str) -> String {
    let trimmed = text.trim().trim_matches(|c| c == '"' || c == '\'');
    let leaf = trimmed.rsplit(['\\', '/']).next().unwrap_or(trimmed);
    leaf.strip_suffix(".exe").unwrap_or(leaf).to_ascii_lowercase()
}
