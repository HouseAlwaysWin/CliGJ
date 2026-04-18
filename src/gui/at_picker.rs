//! `@` workspace file picker: list sync and commit.

use slint::{Model, ModelRc, SharedString, VecModel};

use crate::workspace_files;

use super::composer_sync::sync_composer_line_to_conpty;
use super::slint_ui::AppWindow;
use super::state::GuiState;
use super::state::workspace_root_for_tab;
use super::ui_sync::tab_update_from_ui;

pub(crate) fn sync_at_file_picker(ui: &AppWindow, s: &mut GuiState) {
    if ui.get_ws_raw_input() {
        ui.set_ws_at_picker_open(false);
        s.at_picker_open_snapshot = false;
        return;
    }
    let prompt = ui.get_ws_prompt().to_string();
    if !prompt.contains('@') {
        ui.set_ws_at_picker_open(false);
        s.at_picker_query_snapshot.clear();
        s.at_picker_open_snapshot = false;
        return;
    }
    let query = prompt
        .rsplit_once('@')
        .map(|(_, q)| q.split(['\r', '\n']).next().unwrap_or(""))
        .unwrap_or("")
        .to_string();
    let tab = &s.tabs[s.current];
    let root = workspace_root_for_tab(tab);
    let root_changed = s.workspace_file_cache_root.as_ref() != Some(&root);
    if root_changed {
        s.workspace_file_cache = workspace_files::scan_workspace_files(&root);
        s.workspace_file_cache_root = Some(root.clone());
    }
    let query_changed = s.at_picker_query_snapshot != query;
    if query_changed {
        s.at_picker_query_snapshot = query.clone();
        ui.set_ws_at_selected(0);
    }
    if !root_changed && !query_changed && s.at_picker_open_snapshot && ui.get_ws_at_picker_open() {
        return;
    }
    let choices = workspace_files::filter_paths(
        &s.workspace_file_cache,
        &query,
        workspace_files::CHOICES_DISPLAY,
    );
    if choices.is_empty() {
        ui.set_ws_at_picker_open(false);
        s.at_picker_open_snapshot = false;
        return;
    }
    let model: Vec<SharedString> = choices
        .iter()
        .map(|x| SharedString::from(x.as_str()))
        .collect();
    let n = model.len() as i32;
    ui.set_ws_at_choices(ModelRc::new(VecModel::from(model)));
    ui.set_ws_at_picker_open(true);
    let sel = ui.get_ws_at_selected();
    let clamped = if n <= 0 {
        0
    } else {
        sel.max(0).min(n - 1)
    };
    ui.set_ws_at_selected(clamped);
    ui.invoke_ws_scroll_at_picker_into_view();
    let total_in_tree = s.workspace_file_cache.len();
    let label = format!(
        "@ 檔案 · {} · {}/{} 筆（可捲動）",
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
    let prompt = ui.get_ws_prompt().to_string();
    let root = workspace_root_for_tab(&s.tabs[s.current]);
    let (new_p, abs_path) = workspace_files::apply_at_file_pick_hidden(&prompt, picked.as_str(), &root);
    ui.set_ws_prompt(SharedString::from(new_p.as_str()));
    if !s.tabs[s.current].prompt_picked_files_abs.iter().any(|p| p == &abs_path) {
        s.tabs[s.current].prompt_picked_files_abs.push(abs_path);
    }
    let chips: Vec<SharedString> = s.tabs[s.current]
        .prompt_picked_files_abs
        .iter()
        .map(|p| SharedString::from(workspace_files::file_name_label(p).as_str()))
        .collect();
    ui.set_ws_prompt_path_chips(ModelRc::new(VecModel::from(chips)));
    ui.set_ws_at_picker_open(false);
    tab_update_from_ui(&mut s.tabs[s.current], ui);
    sync_composer_line_to_conpty(ui, s);
}
