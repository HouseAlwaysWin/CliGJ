//! Shell profile rows for top-right terminal picker (`name` + startup `command` + optional workspace root).

use slint::{ModelRc, SharedString, VecModel};

use crate::core::config::AppConfig;
use crate::gui::slint_ui::{AppWindow, InteractiveCmdEditorRow};
use crate::gui::state::GuiState;

pub(crate) fn default_shell_profiles() -> Vec<(String, String, String)> {
    vec![
        (
            "Command Prompt".to_string(),
            r#""C:\Windows\System32\cmd.exe""#.to_string(),
            String::new(),
        ),
        (
            "PowerShell".to_string(),
            r#""C:\Windows\System32\WindowsPowerShell\v1.0\powershell.exe" -NoLogo"#.to_string(),
            String::new(),
        ),
    ]
}

/// Convert known external-launcher commands into ConPTY-friendly startup commands.
pub(crate) fn normalize_shell_profile_command(command: &str) -> (String, bool) {
    let trimmed = command.trim();
    if trimmed.is_empty() {
        return (String::new(), false);
    }

    #[cfg(target_os = "windows")]
    {
        let lower = trimmed.to_ascii_lowercase();

        // "git-bash.exe" launches mintty (new window). Use bash.exe directly for in-app terminal.
        if lower == "git bash" || lower == "git-bash" || lower == "git-bash.exe" {
            return (
                r#""C:\Program Files\Git\bin\bash.exe" --login -i"#.to_string(),
                true,
            );
        }

        if let Some(i) = lower.find("git-bash.exe") {
            let mut out = trimmed.to_string();
            out.replace_range(i..i + "git-bash.exe".len(), r#"bin\bash.exe"#);
            let out_lower = out.to_ascii_lowercase();
            if !out_lower.contains("--login") {
                out.push_str(" --login -i");
            }
            return (out, true);
        }
    }

    (trimmed.to_string(), false)
}

/// Returns `(rows, should_persist)`.
pub(crate) fn load_from_config(cfg: &AppConfig) -> (Vec<(String, String, String)>, bool) {
    let profiles = cfg.shell_profiles();
    if profiles.is_empty() {
        return (default_shell_profiles(), true);
    }
    let defaults = default_shell_profiles();
    let mut out = Vec::new();
    let mut changed = false;

    for (name, cmd_default, w_default) in &defaults {
        if let Some((_, c, w)) = profiles.iter().find(|(n, _, _)| n == name) {
            let (norm, fixed) = normalize_shell_profile_command(c);
            if fixed {
                changed = true;
            }
            out.push((name.clone(), norm, w.clone()));
        } else {
            out.push((name.clone(), cmd_default.clone(), w_default.clone()));
            changed = true;
        }
    }

    for (name, cmd, w) in profiles {
        if out.iter().all(|(n, _, _)| n != &name) {
            let (norm, fixed) = normalize_shell_profile_command(&cmd);
            if fixed {
                changed = true;
            }
            out.push((name, norm, w));
        }
    }

    (out, changed)
}

pub(crate) fn is_reserved_shell_profile(name: &str) -> bool {
    default_shell_profiles().iter().any(|(n, _, _)| n == name)
}

pub(crate) fn resolve_shell_command_line(name: &str, gs: &GuiState) -> Option<String> {
    gs.shell_profiles
        .iter()
        .find(|(n, _, _)| n == name)
        .map(|(_, c, _)| c.clone())
}

pub(crate) fn default_shell_profile_name(gs: &GuiState) -> String {
    if let Some((name, _, _)) = gs.shell_profiles.first() {
        return name.clone();
    }
    "Command Prompt".to_string()
}

pub(crate) fn sync_shell_profile_choices_to_ui(ui: &AppWindow, gs: &GuiState) {
    let rows: Vec<SharedString> = gs
        .shell_profiles
        .iter()
        .map(|(n, _, _)| SharedString::from(n.as_str()))
        .collect();
    ui.set_ws_cmd_type_choices(ModelRc::new(VecModel::from(rows)));
}

pub(crate) fn sync_shell_manage_editor_to_ui(ui: &AppWindow, gs: &GuiState) {
    let rows: Vec<InteractiveCmdEditorRow> = gs
        .shell_profiles
        .iter()
        .map(|(n, c, w)| InteractiveCmdEditorRow {
            name: SharedString::from(n.as_str()),
            line: SharedString::from(c.as_str()),
            key_locked: is_reserved_shell_profile(n),
            expanded: false,
            workspace_path: SharedString::from(w.as_str()),
        })
        .collect();
    ui.set_ws_shell_manage_rows(ModelRc::new(VecModel::from(rows)));
}
