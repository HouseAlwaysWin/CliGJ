use std::rc::Rc;
use slint::{Color, Model, ModelRc, SharedString, VecModel};
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

use crate::terminal::render::{ColoredLine, ColoredSpan};
use crate::workspace_files;

use super::slint_ui::{AppWindow, PromptImageChip};
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

fn overlay_cursor_line(line: &ColoredLine, cursor_col: usize) -> ColoredLine {
    let mut out = ColoredLine {
        blank: line.blank,
        spans: Vec::new(),
    };
    let mut col = 0usize;
    let mut painted = false;

    for span in &line.spans {
        let chars: Vec<char> = span.text.chars().collect();
        if chars.is_empty() {
            continue;
        }
        let mut plain = String::new();
        for ch in chars {
            if col == cursor_col {
                if !plain.is_empty() {
                    out.spans.push(ColoredSpan {
                        text: std::mem::take(&mut plain),
                        fg: span.fg,
                        bg: span.bg,
                    });
                }
                out.spans.push(ColoredSpan {
                    text: ch.to_string(),
                    fg: span.bg,
                    bg: span.fg,
                });
                painted = true;
            } else {
                plain.push(ch);
            }
            col += 1;
        }
        if !plain.is_empty() {
            out.spans.push(ColoredSpan {
                text: plain,
                fg: span.fg,
                bg: span.bg,
            });
        }
    }

    if !painted && col == cursor_col {
        out.blank = false;
        out.spans.push(ColoredSpan {
            text: " ".to_string(),
            fg: [18u8, 18, 18],
            bg: [240u8, 240, 240],
        });
    }
    out
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
        tab.terminal_model_dirty.clear();
        return changed;
    }
    let first = first.min(n - 1);
    let last = last.min(n - 1);
    
    for idx in first..=last {
        // 如果該行已知為 dirty，或快取中不存在，則必須重建
        let needs_rebuild = tab.terminal_model_dirty.contains(&idx) 
            || !tab.terminal_model_hashes.contains_key(&idx);
            
        if !needs_rebuild {
            continue;
        }

        let base_line = &tab.terminal_lines[idx];
        let rendered = if tab.terminal_cursor_row == Some(idx) {
            overlay_cursor_line(base_line, tab.terminal_cursor_col.unwrap_or(0))
        } else {
            base_line.clone()
        };
        let fp = line_fingerprint(&rendered);
        
        // 再次確認指紋，避免不必要的 UI 更新（例如 dirty 標記了但內容其實回滾到跟快取一致）
        let unchanged = tab
            .terminal_model_hashes
            .get(&idx)
            .is_some_and(|cached| *cached == fp);
            
        if unchanged {
            tab.terminal_model_dirty.remove(&idx);
            continue;
        }
        
        let built = build_term_line(&rendered);
        tab.terminal_model_rows.insert(idx, built);
        tab.terminal_model_hashes.insert(idx, fp);
        tab.terminal_model_dirty.insert(idx);
        changed = true;
    }
    changed
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
            // 清空持久 model
            let model = &tab.terminal_slint_model;
            while model.row_count() > 0 {
                model.remove(0);
            }
            ui.set_ws_terminal_lines(ModelRc::from(Rc::clone(&tab.terminal_slint_model)));
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

    let model = &tab.terminal_slint_model;
    let window_len = last - first + 1;

    // 視窗範圍或總長度改變，或者是髒行需要更新
    if window_changed || total_changed || content_changed || model.row_count() != window_len {
        if model.row_count() == window_len {
            // 長度一致，逐行檢查並更新（比對索引避免全量 set_row_data）
            for model_idx in 0..window_len {
                let line_idx = first + model_idx;
                let needs_update = window_changed || tab.terminal_model_dirty.contains(&line_idx);
                
                if needs_update {
                    if let Some(row) = tab.terminal_model_rows.get(&line_idx) {
                        model.set_row_data(model_idx, row.clone());
                    }
                }
            }
        } else {
            // 長度不一致，執行最小化增減
            let current_count = model.row_count();
            if window_len > current_count {
                // 需要增加行
                for model_idx in 0..current_count {
                    let line_idx = first + model_idx;
                    if let Some(row) = tab.terminal_model_rows.get(&line_idx) {
                        model.set_row_data(model_idx, row.clone());
                    }
                }
                for model_idx in current_count..window_len {
                    let line_idx = first + model_idx;
                    let row = tab.terminal_model_rows.get(&line_idx).cloned().unwrap_or_else(empty_term_line);
                    model.push(row);
                }
            } else {
                // 需要減少行
                for model_idx in 0..window_len {
                    let line_idx = first + model_idx;
                    if let Some(row) = tab.terminal_model_rows.get(&line_idx) {
                        model.set_row_data(model_idx, row.clone());
                    }
                }
                for _ in window_len..current_count {
                    model.remove(window_len);
                }
            }
        }
        
        if window_changed || total_changed || model.row_count() != window_len {
            ui.set_ws_terminal_lines(ModelRc::from(Rc::clone(&tab.terminal_slint_model)));
        }
        tab.terminal_model_dirty.clear();
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
    ui.set_ws_image_zoom_index(-1);
    ui.set_ws_file_path(SharedString::from(tab.file_path.as_str()));
    let img_chips: Vec<PromptImageChip> = tab
        .prompt_picked_images
        .iter()
        .map(|p| PromptImageChip {
            label: SharedString::from(workspace_files::file_name_label(&p.abs_path).as_str()),
            thumb: p.preview.clone(),
        })
        .collect();
    ui.set_ws_prompt_images(ModelRc::new(VecModel::from(img_chips)));
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
    // 綁定持久 model
    ui.set_ws_terminal_lines(ModelRc::from(Rc::clone(&tab.terminal_slint_model)));
    if !tab.auto_scroll {
        ui.invoke_ws_scroll_terminal_to_top();
    }
    push_terminal_view_to_ui(ui, tab);
}
