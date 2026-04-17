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
        return;
    }
    let prompt = ui.get_ws_prompt().to_string();
    if !prompt.contains('@') {
        ui.set_ws_at_picker_open(false);
        s.at_picker_query_snapshot.clear();
        return;
    }
    let query = prompt
        .rsplit_once('@')
        .map(|(_, q)| q.split(['\r', '\n']).next().unwrap_or(""))
        .unwrap_or("")
        .to_string();
    let tab = &s.tabs[s.current];
    let root = workspace_root_for_tab(tab);
    if s.workspace_file_cache_root.as_ref() != Some(&root) {
        s.workspace_file_cache = workspace_files::scan_workspace_files(&root);
        s.workspace_file_cache_root = Some(root.clone());
    }
    if s.at_picker_query_snapshot != query {
        s.at_picker_query_snapshot = query.clone();
        ui.set_ws_at_selected(0);
    }
    let choices = workspace_files::filter_paths(
        &s.workspace_file_cache,
        &query,
        workspace_files::CHOICES_DISPLAY,
    );
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
    let new_p = workspace_files::apply_at_file_pick(&prompt, picked.as_str(), &root);
    ui.set_ws_prompt(SharedString::from(new_p.as_str()));
    ui.set_ws_at_picker_open(false);
    tab_update_from_ui(&mut s.tabs[s.current], ui);
    sync_composer_line_to_conpty(ui, s);
}
