use slint::{Color, Model, ModelRc, SharedString, VecModel};

use crate::terminal::render::ColoredLine;
use crate::terminal_v2::renderer::TerminalFrame;
use crate::terminal_v2::TerminalRow;
use crate::workspace_files;

use super::slint_ui::AppWindow;
use super::state::TabState;
use super::slint_ui::{TermLine, TermSpan};

pub(crate) fn rgb_color(rgb: [u8; 3]) -> Color {
    Color::from_rgb_u8(rgb[0], rgb[1], rgb[2])
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
                    bg: rgb_color(s.bg),
                })
                .collect();
            TermLine {
                blank: line.blank,
                spans: ModelRc::new(VecModel::from(spans)),
            }
        })
        .collect();
    ModelRc::new(VecModel::from(rows))
}

fn term_line_from_v2_row(row: &TerminalRow) -> TermLine {
    let mut spans: Vec<TermSpan> = Vec::new();
    let mut text = String::new();
    let mut cur_fg: Option<[u8; 3]> = None;
    let mut cur_bg: Option<[u8; 3]> = None;
    let mut has_visible = false;

    for cell in &row.cells {
        let fg = cell.fg;
        let bg = cell.bg;
        if !cell.ch.is_whitespace() {
            has_visible = true;
        }

        if cur_fg == Some(fg) && cur_bg == Some(bg) {
            text.push(cell.ch);
            continue;
        }

        if let (Some(prev_fg), Some(prev_bg)) = (cur_fg, cur_bg) {
            spans.push(TermSpan {
                text: SharedString::from(text.as_str()),
                fg: rgb_color(prev_fg),
                bg: rgb_color(prev_bg),
            });
        }
        text.clear();
        text.push(cell.ch);
        cur_fg = Some(fg);
        cur_bg = Some(bg);
    }

    if let (Some(prev_fg), Some(prev_bg)) = (cur_fg, cur_bg) {
        spans.push(TermSpan {
            text: SharedString::from(text.as_str()),
            fg: rgb_color(prev_fg),
            bg: rgb_color(prev_bg),
        });
    }

    TermLine {
        blank: !has_visible,
        spans: ModelRc::new(VecModel::from(spans)),
    }
}

pub(crate) fn apply_terminal_v2_frame_to_model(tab: &mut TabState, frame: &TerminalFrame) {
    let model = &tab.terminal_lines_model;
    let target_rows = frame.rows.len();
    let current_rows = model.row_count();
    if current_rows < target_rows {
        for _ in current_rows..target_rows {
            model.push(TermLine {
                blank: true,
                spans: ModelRc::new(VecModel::from(Vec::<TermSpan>::new())),
            });
        }
    } else if current_rows > target_rows {
        for _ in target_rows..current_rows {
            model.remove(model.row_count() - 1);
        }
    }

    for &row_idx in &frame.dirty_rows {
        if row_idx >= frame.rows.len() {
            continue;
        }
        model.set_row_data(row_idx, term_line_from_v2_row(&frame.rows[row_idx]));
    }
}

pub(crate) fn terminal_lines_model_rc(tab: &TabState) -> ModelRc<TermLine> {
    ModelRc::from(tab.terminal_lines_model.clone())
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
    tab.terminal_text = ui.get_ws_terminal_text().to_string();
    tab.auto_scroll = ui.get_ws_auto_scroll();
    tab.terminal_select_mode = ui.get_ws_terminal_select_mode();
    tab.raw_input_mode = ui.get_ws_raw_input();
}

pub(crate) fn load_tab_to_ui(ui: &AppWindow, tab: &TabState) {
    ui.set_ws_file_path(SharedString::from(tab.file_path.as_str()));
    ui.set_ws_has_image(tab.has_image);
    ui.set_ws_preview_image(tab.preview_image.clone());
    ui.set_ws_terminal_text(SharedString::from(tab.terminal_text.as_str()));
    ui.set_ws_terminal_lines(terminal_lines_model_rc(tab));
    ui.set_ws_auto_scroll(tab.auto_scroll);
    ui.set_ws_terminal_select_mode(tab.terminal_select_mode);
    if !tab.auto_scroll {
        ui.invoke_ws_scroll_terminal_to_top();
    }

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
}
