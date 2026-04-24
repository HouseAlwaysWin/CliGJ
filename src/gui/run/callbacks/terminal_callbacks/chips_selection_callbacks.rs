use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use slint::{ComponentHandle, SharedString};

use crate::gui::ipc::IpcBridge;
use crate::gui::open_in_vscode::open_path_in_editor;
use crate::gui::reveal_in_explorer::reveal_path_in_file_manager;
use crate::gui::run::helpers::{
    clear_all_prompt_files, copy_to_clipboard, remove_prompt_file_at,
    selected_text_from_terminal_lines,
};
use crate::gui::slint_ui::AppWindow;
use crate::gui::state::{GuiState, TabState, workspace_root_for_tab_with_profile};

pub(super) fn connect_chips(app: &AppWindow, state: Rc<RefCell<GuiState>>, ipc: IpcBridge) {
    let st_remove = Rc::clone(&state);
    let app_weak = app.as_weak();
    app.on_prompt_chip_remove_requested(move |index| {
        let Some(ui) = app_weak.upgrade() else {
            return;
        };
        if index < 0 {
            return;
        }
        let mut s = st_remove.borrow_mut();
        remove_prompt_file_at(&ui, &mut s, index as usize);
    });

    let st_clear = Rc::clone(&state);
    let app_weak = app.as_weak();
    app.on_prompt_chip_clear_requested(move || {
        let Some(ui) = app_weak.upgrade() else {
            return;
        };
        let mut s = st_clear.borrow_mut();
        clear_all_prompt_files(&ui, &mut s);
    });

    let st_path_editor = Rc::clone(&state);
    let ipc_open_editor = ipc.clone();
    app.on_prompt_path_chip_open_editor_requested(move |index| {
        if index < 0 {
            return;
        }
        let idx = index as usize;
        let s = st_path_editor.borrow();
        if s.current >= s.tabs.len() {
            return;
        }
        let current = s.current;
        let tab = &s.tabs[current];
        let Some(raw_path) = tab.prompt_picked_files_abs.get(idx) else {
            return;
        };
        let workspace_root = workspace_root_for_tab_with_profile(tab, &s);
        let resolved_path = resolve_prompt_file_path(raw_path.as_str(), &workspace_root);
        let (start_line, end_line) = selection_lines_for_path(tab, &workspace_root, &resolved_path)
            .map(|(start, end)| (Some(start), Some(end)))
            .unwrap_or((None, None));
        let target = resolved_path.to_string_lossy().to_string();
        let origin = tab
            .prompt_picked_file_origins
            .get(idx)
            .and_then(|origin| origin.as_ref())
            .cloned()
            .or_else(|| tab.prompt_last_file_origin.clone());
        if let Some(origin) = origin {
            if !origin.client_id.trim().is_empty() {
                let snap = ipc_open_editor.snapshot();
                if snap.client_count > 0 {
                    ipc_open_editor.publish_open_editor_location(
                        origin.client_id.clone(),
                        target.clone(),
                        start_line,
                        end_line,
                    );
                    // Keep IPC for deterministic routing, and also trigger URI open
                    // to encourage the editor app window to become foreground.
                    if !origin.uri_scheme.trim().is_empty() {
                        open_path_in_editor(
                            origin.uri_scheme.as_str(),
                            target.as_str(),
                            start_line,
                            end_line,
                        );
                    }
                    return;
                }
            }
            if !origin.uri_scheme.trim().is_empty() {
                open_path_in_editor(origin.uri_scheme.as_str(), target.as_str(), start_line, end_line);
                return;
            }
        }
        reveal_path_in_file_manager(target.as_str());
    });

    let st_path_explorer = Rc::clone(&state);
    app.on_prompt_path_chip_open_explorer_requested(move |index| {
        if index < 0 {
            return;
        }
        let idx = index as usize;
        let s = st_path_explorer.borrow();
        if s.current >= s.tabs.len() {
            return;
        }
        let current = s.current;
        let tab = &s.tabs[current];
        let Some(path) = tab.prompt_picked_files_abs.get(idx) else {
            return;
        };
        let workspace_root = workspace_root_for_tab_with_profile(tab, &s);
        let resolved_path = resolve_prompt_file_path(path.as_str(), &workspace_root);
        let target = resolved_path.to_string_lossy().to_string();
        reveal_path_in_file_manager(target.as_str());
    });

    let st_img_explorer = Rc::clone(&state);
    app.on_prompt_image_open_explorer_requested(move |index| {
        if index < 0 {
            return;
        }
        let idx = index as usize;
        let s = st_img_explorer.borrow();
        if s.current >= s.tabs.len() {
            return;
        }
        let current = s.current;
        let Some(img) = s.tabs[current].prompt_picked_images.get(idx) else {
            return;
        };
        reveal_path_in_file_manager(&img.abs_path);
    });
}

pub(super) fn connect_terminal_selection(app: &AppWindow, state: Rc<RefCell<GuiState>>) {
    let st_sel = Rc::clone(&state);
    let app_weak = app.as_weak();
    app.on_terminal_selection_committed(move |sr, sc, er, ec| {
        let Some(ui) = app_weak.upgrade() else {
            return;
        };
        let mut s = st_sel.borrow_mut();
        if s.current >= s.tabs.len() {
            return;
        }
        let selected = selected_text_from_terminal_lines(&s.tabs[s.current], sr, sc, er, ec);
        if selected.is_empty() {
            return;
        }
        if let Err(e) = copy_to_clipboard(selected.as_str()) {
            eprintln!("CliGJ: copy selection: {e}");
        }
        let current = s.current;
        s.tabs[current].selected_context = SharedString::from(selected.as_str());
        ui.set_ws_selected_context(SharedString::from(selected.as_str()));
    });
}

fn selection_lines_for_path(
    tab: &TabState,
    workspace_root: &Path,
    target_path: &Path,
) -> Option<(usize, usize)> {
    let target_key = normalize_path_key(target_path);
    for payload in &tab.prompt_picked_selections {
        let Some(header) = payload.lines().next() else {
            continue;
        };
        let Some(raw_file) = extract_selection_attr(header, "file") else {
            continue;
        };
        let Some(raw_range) = extract_selection_attr(header, "range") else {
            continue;
        };
        let candidate = resolve_prompt_file_path(raw_file.as_str(), workspace_root);
        if normalize_path_key(candidate.as_path()) != target_key {
            continue;
        }
        if let Some(lines) = parse_selection_range(raw_range.as_str()) {
            return Some(lines);
        }
    }
    None
}

fn extract_selection_attr(header: &str, key: &str) -> Option<String> {
    let needle = format!(r#"{key}=""#);
    let start = header.find(needle.as_str())? + needle.len();
    let tail = &header[start..];
    let end = tail.find('"')?;
    Some(tail[..end].to_string())
}

fn parse_selection_range(raw: &str) -> Option<(usize, usize)> {
    let trimmed = raw.trim();
    let single = trimmed.strip_prefix('L').and_then(|v| v.parse::<usize>().ok());
    if let Some(line) = single {
        return Some((line, line));
    }
    let body = trimmed.strip_prefix('L')?;
    let (start, end) = body.split_once("-L")?;
    let start_line = start.parse::<usize>().ok()?;
    let end_line = end.parse::<usize>().ok()?;
    Some((start_line, end_line.max(start_line)))
}

fn resolve_prompt_file_path(raw_path: &str, workspace_root: &Path) -> PathBuf {
    let candidate = PathBuf::from(raw_path);
    if candidate.is_absolute() {
        candidate
    } else {
        workspace_root.join(candidate)
    }
}

fn normalize_path_key(path: &Path) -> String {
    let mut normalized = path.to_string_lossy().replace('\\', "/");
    #[cfg(target_os = "windows")]
    {
        normalized.make_ascii_lowercase();
    }
    normalized
}
