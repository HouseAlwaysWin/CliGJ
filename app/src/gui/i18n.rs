//! Slint bundled translations: map settings labels → locale folder names (`translations/<tag>/LC_MESSAGES/CliGJ.po`).

use slint::ComponentHandle;

use crate::gui::slint_ui::AppWindow;

/// ComboBox labels in shell settings (`ws-shell-startup-language-choices`).
/// Localized default tab title (`Session N` vs `工作階段 N`) from the same labels as the shell language ComboBox.
pub(crate) fn tab_title_for_index(shell_language_label: &str, index_one_based: usize) -> String {
    let tag = slint_locale_tag_for_shell_setting(shell_language_label);
    if tag == "en" {
        format!("Session {index_one_based}")
    } else {
        format!("工作階段 {index_one_based}")
    }
}

pub(crate) fn slint_locale_tag_for_shell_setting(label: &str) -> &'static str {
    match label.trim() {
        "中文" => "zh_TW",
        "English" => "en",
        "預設" | _ => {
            if let Some(loc) = sys_locale::get_locale() {
                let l = loc.to_lowercase();
                if l.starts_with("zh") {
                    return "zh_TW";
                }
                if l.starts_with("en") {
                    return "en";
                }
            }
            "zh_TW"
        }
    }
}

pub(crate) fn terminal_history_title_suffix_for_shell_setting(label: &str) -> &'static str {
    if slint_locale_tag_for_shell_setting(label) == "en" {
        "Terminal History"
    } else {
        "終端歷史"
    }
}

/// Apply bundled UI strings and force a full redraw so all `@tr` bindings re-evaluate.
pub(crate) fn apply_slint_language_from_shell_setting(ui: &AppWindow, label: &str) {
    let tag = slint_locale_tag_for_shell_setting(label);
    match slint::select_bundled_translation(tag) {
        Ok(()) => {
            ui.window().request_redraw();
        }
        Err(e) => {
            eprintln!("CliGJ: UI language ({tag}): {e}");
        }
    }
}
