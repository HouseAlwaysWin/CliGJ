//! Ctrl+P workspace quick-open picker: list sync, dismiss, and commit.

use slint::{Model, ModelRc, SharedString, VecModel};

use cligj_workspace as workspace_files;

use super::composer_sync::sync_composer_line_to_conpty;
use super::slint_ui::AppWindow;
use super::state::GuiState;
use super::state::workspace_root_for_tab_with_profile;
use super::ui_sync::{sync_prompt_file_chips_to_ui, tab_update_from_ui};

/// Open the workspace quick-open popup with the full file list.
pub(crate) fn open_file_picker(ui: &AppWindow, s: &mut GuiState) {
    if s.current >= s.tabs.len() {
        return;
    }
    s.at_picker_filter.clear();
    s.at_picker_query_snapshot.clear();
    s.at_picker_open_snapshot = false;
    ui.set_ws_at_selected(0);
    refresh_file_list(ui, s, "");
    ui.set_ws_at_picker_open(true);
    ui.set_ws_at_picker_filter(SharedString::new());
    s.at_picker_open_snapshot = true;
}

/// Close the workspace quick-open popup and clear transient filter state.
pub(crate) fn close_file_picker(ui: &AppWindow, s: &mut GuiState) {
    ui.set_ws_at_picker_open(false);
    ui.set_ws_at_selected(0);
    ui.set_ws_at_picker_filter(SharedString::new());
    s.at_picker_query_snapshot.clear();
    s.at_picker_open_snapshot = false;
    s.at_picker_filter.clear();
}

/// Refresh the file list using the current filter text (called on every keystroke while picker is open).
pub(crate) fn refresh_file_picker_from_filter(ui: &AppWindow, s: &mut GuiState) {
    if s.current >= s.tabs.len() {
        return;
    }
    let query = s.at_picker_filter.clone();
    ui.set_ws_at_picker_filter(SharedString::from(query.as_str()));
    ui.set_ws_at_selected(0);
    refresh_file_list(ui, s, &query);
}

fn refresh_file_list(ui: &AppWindow, s: &mut GuiState, query: &str) {
    let tab = &s.tabs[s.current];
    let root = workspace_root_for_tab_with_profile(tab, s);
    let root_changed = s.workspace_file_cache_root.as_ref() != Some(&root);
    if root_changed {
        s.workspace_file_cache = workspace_files::scan_workspace_files(&root);
        s.workspace_file_cache_root = Some(root.clone());
    }

    let choices = workspace_files::filter_paths(
        &s.workspace_file_cache,
        query,
        workspace_files::CHOICES_DISPLAY,
    );
    let model: Vec<SharedString> = choices
        .iter()
        .map(|x| SharedString::from(x.as_str()))
        .collect();
    let count = model.len() as i32;

    ui.set_ws_at_choices(ModelRc::new(VecModel::from(model)));
    ui.set_ws_at_picker_open(true);

    if count <= 0 {
        ui.set_ws_at_selected(0);
    } else {
        let sel = ui.get_ws_at_selected();
        let clamped = sel.max(0).min(count - 1);
        ui.set_ws_at_selected(clamped);
        ui.invoke_ws_scroll_at_picker_into_view();
    }

    let total_in_tree = s.workspace_file_cache.len();
    let label = format!(
        "Workspace: {}  Matches: {}/{} files",
        root.display(),
        choices.len(),
        total_in_tree
    );
    ui.set_ws_workspace_root_label(SharedString::from(label.as_str()));
    s.at_picker_open_snapshot = true;
}

pub(crate) fn commit_at_file_pick(ui: &AppWindow, s: &mut GuiState, index: usize) {
    let m = ui.get_ws_at_choices();
    let n = m.row_count();
    if n == 0 || index >= n {
        return;
    }
    let Some(picked) = m.row_data(index) else {
        return;
    };
    let picked_str = picked.to_string();
    close_file_picker(ui, s);
    let root = workspace_root_for_tab_with_profile(&s.tabs[s.current], s);
    let abs_path = workspace_files::absolute_path_from_pick(&picked_str, &root);
    let current = s.current;
    let tab = &mut s.tabs[current];
    if !tab.prompt_picked_files_abs.iter().any(|p| p == &abs_path) {
        tab.prompt_picked_files_abs.push(abs_path.clone());
        tab.prompt_picked_file_origins.push(None);
    }
    let file_name = workspace_files::file_name_label(abs_path.as_str());
    let occurrence = tab
        .prompt_picked_files_abs
        .iter()
        .filter(|p| workspace_files::file_name_label(p) == file_name)
        .count()
        + tab
            .prompt_picked_images
            .iter()
            .filter(|img| workspace_files::file_name_label(img.abs_path.as_str()) == file_name)
            .count();
    let token = workspace_files::filepath_hint_token(file_name.as_str(), occurrence.max(1));
    let next_prompt = workspace_files::append_attachment_token(tab.prompt.as_str(), token.as_str());
    tab.prompt = SharedString::from(next_prompt.as_str());
    ui.set_ws_prompt(tab.prompt.clone());
    sync_prompt_file_chips_to_ui(ui, tab);
    tab_update_from_ui(tab, ui);
    sync_composer_line_to_conpty(ui, s);
}
