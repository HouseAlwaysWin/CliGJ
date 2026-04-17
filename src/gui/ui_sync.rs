use slint::{Color, ModelRc, SharedString, VecModel};
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

use crate::terminal::render::ColoredLine;
use crate::workspace_files;

use super::slint_ui::AppWindow;
use super::state::TabState;
use super::slint_ui::{TermLine, TermSpan};

/// Must match `row-height` in `gj_viewer.slint`.
pub(crate) const TERMINAL_ROW_HEIGHT_PX: f32 = 18.0;
/// Extra rows above/below the visible band (matches prior Slint overscan intent).
pub(crate) const TERMINAL_ROW_OVERSCAN: usize = 8;

pub(crate) fn rgb_color(rgb: [u8; 3]) -> Color {
    Color::from_rgb_u8(rgb[0], rgb[1], rgb[2])
}

/// `AppTheme.bg` in `theme.slint` (#0a0a0f). Map ANSI default bg to this so short lines do not
/// show as darker strips than the padded terminal area.
fn term_bg_for_ui(bg: [u8; 3]) -> Color {
    const APP_BG: [u8; 3] = [10, 10, 15];
    const TERM_DEFAULTS: &[[u8; 3]] = &[[0, 0, 0], [18, 18, 18]];
    let rgb = if TERM_DEFAULTS.contains(&bg) {
        APP_BG
    } else {
        bg
    };
    rgb_color(rgb)
}

#[allow(dead_code)]
pub(crate) fn colored_lines_to_model(lines: &[ColoredLine]) -> ModelRc<TermLine> {
    let rows: Vec<TermLine> = lines
        .iter()
        .map(|line| {
            let spans: Vec<TermSpan> = line
                .spans
                .iter()
                .map(|s| TermSpan {
                    text: SharedString::from(s.text.as_str()),
                    fg: rgb_color(s.fg),
                    bg: term_bg_for_ui(s.bg),
                })
                .collect();
            let char_count: i32 = line
                .spans
                .iter()
                .map(|s| s.text.chars().count() as i32)
                .sum();
            TermLine {
                blank: line.blank,
                char_count,
                spans: ModelRc::new(VecModel::from(spans)),
            }
        })
        .collect();
    ModelRc::new(VecModel::from(rows))
}

fn line_fingerprint(line: &ColoredLine) -> u64 {
    let mut h = DefaultHasher::new();
    line.blank.hash(&mut h);
    line.spans.len().hash(&mut h);
    for span in &line.spans {
        span.text.hash(&mut h);
        span.fg.hash(&mut h);
        span.bg.hash(&mut h);
    }
    h.finish()
}

fn build_term_line(line: &ColoredLine) -> TermLine {
    let spans: Vec<TermSpan> = line
        .spans
        .iter()
        .map(|s| TermSpan {
            text: SharedString::from(s.text.as_str()),
            fg: rgb_color(s.fg),
            bg: term_bg_for_ui(s.bg),
        })
        .collect();
    let char_count: i32 = line
        .spans
        .iter()
        .map(|s| s.text.chars().count() as i32)
        .sum();
    TermLine {
        blank: line.blank,
        char_count,
        spans: ModelRc::new(VecModel::from(spans)),
    }
}

fn empty_term_line() -> TermLine {
    TermLine {
        blank: true,
        char_count: 0,
        spans: ModelRc::new(VecModel::from(Vec::<TermSpan>::new())),
    }
}

/// Incremental cache: only rebuild rows inside [first, last] whose VT content changed.
/// Returns true when cached rows in this window changed.
pub(crate) fn sync_terminal_model_cache_range(tab: &mut TabState, first: usize, last: usize) -> bool {
    let n = tab.terminal_lines.len();
    let mut changed = false;
    if n == 0 || first > last {
        changed = !tab.terminal_model_rows.is_empty() || !tab.terminal_model_hashes.is_empty();
        tab.terminal_model_rows.clear();
        tab.terminal_model_hashes.clear();
        return changed;
    }
    if tab.terminal_model_rows.len() > n {
        tab.terminal_model_rows.truncate(n);
        tab.terminal_model_hashes.truncate(n);
        changed = true;
    } else if tab.terminal_model_rows.len() < n {
        tab.terminal_model_rows.resize_with(n, empty_term_line);
        tab.terminal_model_hashes.resize(n, u64::MAX);
        changed = true;
    }
    let first = first.min(n - 1);
    let last = last.min(n - 1);
    if first > last {
        return changed;
    }
    for idx in first..=last {
        let line = &tab.terminal_lines[idx];
        let fp = line_fingerprint(line);
        let unchanged = tab.terminal_model_hashes[idx] == fp;
        if unchanged {
            continue;
        }
        let built = build_term_line(line);
        tab.terminal_model_rows[idx] = built;
        tab.terminal_model_hashes[idx] = fp;
        changed = true;
    }
    changed
}

pub(crate) fn terminal_model_window(tab: &TabState, first: usize, last: usize) -> ModelRc<TermLine> {
    if tab.terminal_model_rows.is_empty() || first > last {
        return ModelRc::new(VecModel::from(Vec::<TermLine>::new()));
    }
    let n = tab.terminal_model_rows.len();
    let first = first.min(n.saturating_sub(1));
    let last = last.min(n - 1);
    if first > last {
        return ModelRc::new(VecModel::from(Vec::new()));
    }
    let slice: Vec<TermLine> = tab.terminal_model_rows[first..=last].to_vec();
    ModelRc::new(VecModel::from(slice))
}

/// Push only the visible (+ overscan) slice into Slint; set global offset and total line count.
pub(crate) fn push_terminal_view_to_ui(ui: &AppWindow, tab: &mut TabState) {
    let scroll_top = ui.get_ws_terminal_scroll_top_px();
    let vh = ui.get_ws_terminal_viewport_height_px().max(1.0);
    tab.terminal_scroll_top_px = scroll_top;
    tab.terminal_view_height_px = vh;

    let n = tab.terminal_lines.len();
    if n == 0 {
        if tab.last_window_total != 0 {
            ui.set_ws_terminal_line_offset(0);
            ui.set_ws_terminal_total_lines(0);
            ui.set_ws_terminal_lines(ModelRc::new(VecModel::from(Vec::<TermLine>::new())));
        }
        tab.last_window_first = 0;
        tab.last_window_last = 0;
        tab.last_window_total = 0;
        tab.last_pushed_scroll_top = scroll_top;
        tab.last_pushed_viewport_height = vh;
        return;
    }

    let first_f = (scroll_top / TERMINAL_ROW_HEIGHT_PX).floor() as isize;
    let first = first_f
        .saturating_sub(TERMINAL_ROW_OVERSCAN as isize)
        .max(0) as usize;
    let last_visible_bottom = scroll_top + vh;
    let last_visible = (last_visible_bottom / TERMINAL_ROW_HEIGHT_PX).ceil() as isize;
    let last = (last_visible + TERMINAL_ROW_OVERSCAN as isize).clamp(0, n as isize - 1) as usize;
    let first = first.min(last);
    let window_changed = tab.last_window_first != first || tab.last_window_last != last;
    let total_changed = tab.last_window_total != n;
    let content_changed = sync_terminal_model_cache_range(tab, first, last);
    if window_changed {
        ui.set_ws_terminal_line_offset(first as i32);
    }
    if total_changed {
        ui.set_ws_terminal_total_lines(n as i32);
    }
    if window_changed || content_changed {
        ui.set_ws_terminal_lines(terminal_model_window(tab, first, last));
    }

    tab.last_window_first = first;
    tab.last_window_last = last;
    tab.last_window_total = n;
    tab.last_pushed_scroll_top = scroll_top;
    tab.last_pushed_viewport_height = vh;
}

pub(crate) fn sync_tab_count(ui: &AppWindow, n: usize) {
    ui.set_tab_count(n as i32);
}

pub(crate) fn tab_update_from_ui(tab: &mut TabState, ui: &AppWindow) {
    tab.file_path = ui.get_ws_file_path().to_string();
    tab.has_image = ui.get_ws_has_image();
    tab.preview_image = ui.get_ws_preview_image();
    tab.selected_line = ui.get_ws_selected_line();
    tab.selected_context = ui.get_ws_selected_context();
    tab.prompt = ui.get_ws_prompt();
    tab.cmd_type = ui.get_ws_cmd_type().to_string();
    if tab.terminal_lines.is_empty() {
        tab.terminal_text = ui.get_ws_terminal_text().to_string();
    }
    tab.auto_scroll = ui.get_ws_auto_scroll();
    tab.terminal_select_mode = ui.get_ws_terminal_select_mode();
    tab.raw_input_mode = ui.get_ws_raw_input();
}

pub(crate) fn load_tab_to_ui(ui: &AppWindow, tab: &mut TabState) {
    ui.set_ws_file_path(SharedString::from(tab.file_path.as_str()));
    ui.set_ws_has_image(tab.has_image);
    ui.set_ws_preview_image(tab.preview_image.clone());
    if tab.terminal_lines.is_empty() {
        ui.set_ws_terminal_text(SharedString::from(tab.terminal_text.as_str()));
    } else {
        ui.set_ws_terminal_text(SharedString::new());
    }
    ui.set_ws_auto_scroll(tab.auto_scroll);
    ui.set_ws_terminal_select_mode(tab.terminal_select_mode);

    ui.set_ws_selected_line(tab.selected_line);
    ui.set_ws_selected_context(tab.selected_context.clone());
    ui.set_ws_prompt(tab.prompt.clone());
    let chips: Vec<SharedString> = tab
        .prompt_picked_files_abs
        .iter()
        .map(|p| SharedString::from(workspace_files::file_name_label(p).as_str()))
        .collect();
    ui.set_ws_prompt_path_chips(ModelRc::new(VecModel::from(chips)));
    ui.set_ws_cmd_type(SharedString::from(tab.cmd_type.as_str()));
    ui.set_ws_raw_input(tab.raw_input_mode);

    let n = tab.terminal_lines.len();
    ui.set_ws_terminal_total_lines(n as i32);
    if !tab.auto_scroll {
        ui.invoke_ws_scroll_terminal_to_top();
    }
    push_terminal_view_to_ui(ui, tab);
}
