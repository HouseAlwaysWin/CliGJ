use std::cell::Cell;
use std::rc::Rc;
use slint::{Color, ComponentHandle, Model, ModelRc, SharedString, VecModel};
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use unicode_width::UnicodeWidthChar;

use crate::gui::prompt_attachments::{
    prune_prompt_files_not_in_prompt, prune_prompt_images_not_in_prompt,
};
use crate::terminal::render::{ColoredLine, ColoredSpan};
use crate::workspace_files;

use super::slint_ui::{AppWindow, PromptImageChip};
use super::state::{TabState, TerminalMode};
use super::slint_ui::{TermLine, TermSpan};

/// Must match `row-height` in `gj_viewer.slint`.
pub(crate) const TERMINAL_ROW_HEIGHT_PX: f32 = 18.0;
/// Extra rows above/below the visible band (matches prior Slint overscan intent).
pub(crate) const TERMINAL_ROW_OVERSCAN: usize = 8;

// 追蹤 `bump_terminal_size` 觸發時應該 resize 的 tab ID。
// `load_tab_to_ui` 在呼叫 `bump_terminal_size` 前設定此值；
// deferred resize handler 讀取此值來找到正確的 tab，
// 避免快速切換分頁時 resize 送到錯誤的 PTY。
thread_local! {
    pub(crate) static RESIZE_TARGET_TAB_ID: Cell<u64> = Cell::new(0);
    /// Monotonic token for the shared workspace layout. Increment on each tab/UI reload so
    /// deferred resize/viewport callbacks from an older frame can be ignored safely.
    pub(crate) static UI_LAYOUT_EPOCH: Cell<u64> = Cell::new(1);
}

/// Scroll offset in px (content top) matching [`GjViewer`] / PTY row math — use when Slint's
/// `terminal-scroll-top-px` getter may still reflect another tab or an older frame.
pub(crate) fn terminal_pinned_footer_start(tab: &TabState) -> Option<usize> {
    let pinned_rows = tab.terminal_pinned_footer_lines;
    if pinned_rows == 0 {
        return None;
    }
    let n = tab.terminal_lines.len();
    if n <= pinned_rows + 2 {
        return None;
    }
    Some(n.saturating_sub(pinned_rows))
}

pub(crate) fn scrollable_terminal_line_count(tab: &TabState) -> usize {
    terminal_pinned_footer_start(tab).unwrap_or(tab.terminal_lines.len())
}

pub(crate) fn terminal_scroll_top_for_tab(tab: &TabState, viewport_height_px: f32) -> f32 {
    let n = scrollable_terminal_line_count(tab);
    if n == 0 {
        return 0.0;
    }
    let vh = viewport_height_px.max(1.0);
    let content_h = n as f32 * TERMINAL_ROW_HEIGHT_PX;
    if tab.terminal_mode == TerminalMode::InteractiveAi {
        if tab.interactive_follow_output {
            return (content_h - vh).max(0.0);
        }
        return tab
            .terminal_saved_scroll_top_px
            .clamp(0.0, (content_h - vh).max(0.0));
    }
    if tab.auto_scroll {
        (content_h - vh).max(0.0)
    } else {
        0.0
    }
}

/// Clamp [`TabState::terminal_saved_scroll_top_px`] to valid range for the current line count.
pub(crate) fn clamp_saved_scroll_top(tab: &TabState, viewport_height_px: f32) -> f32 {
    let n = scrollable_terminal_line_count(tab);
    if n == 0 {
        return 0.0;
    }
    let vh = viewport_height_px.max(1.0);
    let max_s = (n as f32 * TERMINAL_ROW_HEIGHT_PX - vh).max(0.0);
    tab.terminal_saved_scroll_top_px.clamp(0.0, max_s)
}

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

fn is_emoji_char(ch: char) -> bool {
    matches!(
        ch as u32,
        0x1F000..=0x1FAFF | 0xFE0F
    )
}

fn is_symbol_char(ch: char) -> bool {
    matches!(
        ch as u32,
        0x2600..=0x27BF
            | 0x2B00..=0x2BFF
            | 0x2190..=0x21FF
            | 0x2300..=0x23FF
            | 0x2460..=0x24FF
    )
}

fn is_cjk_or_bopomofo_char(ch: char) -> bool {
    matches!(
        ch as u32,
        0x2E80..=0x2EFF
            | 0x2F00..=0x2FDF
            | 0x3000..=0x303F
            | 0x3100..=0x312F
            | 0x31A0..=0x31BF
            | 0x3400..=0x4DBF
            | 0x4E00..=0x9FFF
            | 0xF900..=0xFAFF
            | 0xFE30..=0xFE4F
            | 0xFF00..=0xFFEF
            | 0x20000..=0x2A6DF
            | 0x2A700..=0x2B73F
            | 0x2B740..=0x2B81F
            | 0x2B820..=0x2CEAF
            | 0x2CEB0..=0x2EBEF
            | 0x2EBF0..=0x2EE5F
    )
}

fn term_span_font_family(ch: char, cjk_fallback_font_family: &str) -> String {
    if is_emoji_char(ch) {
        "Segoe UI Emoji".to_string()
    } else if is_symbol_char(ch) {
        "Segoe UI Symbol".to_string()
    } else if is_cjk_or_bopomofo_char(ch) {
        cjk_fallback_font_family.to_string()
    } else {
        String::new()
    }
}

fn split_term_span_by_font(span: &ColoredSpan, cjk_fallback_font_family: &str) -> Vec<TermSpan> {
    let mut out: Vec<TermSpan> = Vec::new();
    let mut current_text = String::new();
    let mut current_font = String::new();

    for ch in span.text.chars() {
        let font = term_span_font_family(ch, cjk_fallback_font_family);
        if !current_text.is_empty() && font != current_font {
            out.push(TermSpan {
                text: SharedString::from(current_text.as_str()),
                fg: rgb_color(span.fg),
                bg: term_bg_for_ui(span.bg),
                font_family: SharedString::from(current_font.as_str()),
            });
            current_text.clear();
        }
        current_font = font;
        current_text.push(ch);
    }

    if !current_text.is_empty() {
        out.push(TermSpan {
            text: SharedString::from(current_text.as_str()),
            fg: rgb_color(span.fg),
            bg: term_bg_for_ui(span.bg),
            font_family: SharedString::from(current_font.as_str()),
        });
    }

    if out.is_empty() {
        out.push(TermSpan {
            text: SharedString::from(span.text.as_str()),
            fg: rgb_color(span.fg),
            bg: term_bg_for_ui(span.bg),
            font_family: SharedString::new(),
        });
    }

    out
}

#[allow(dead_code)]
pub(crate) fn colored_lines_to_model(lines: &[ColoredLine]) -> ModelRc<TermLine> {
    let cjk_fallback_font_family = "MingLiU";
    let rows: Vec<TermLine> = lines
        .iter()
        .map(|line| {
            let spans: Vec<TermSpan> = line
                .spans
                .iter()
                .flat_map(|span| split_term_span_by_font(span, cjk_fallback_font_family))
                .collect();
            let char_count: i32 = line
                .spans
                .iter()
                .map(|s| {
                    s.text
                        .chars()
                        .map(|ch| UnicodeWidthChar::width(ch).unwrap_or(1).max(1) as i32)
                        .sum::<i32>()
                })
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

fn build_term_line(line: &ColoredLine, cjk_fallback_font_family: &str) -> TermLine {
    let spans: Vec<TermSpan> = line
        .spans
        .iter()
        .flat_map(|span| split_term_span_by_font(span, cjk_fallback_font_family))
        .collect();
    let char_count: i32 = line
        .spans
        .iter()
        .map(|s| {
            s.text
                .chars()
                .map(|ch| UnicodeWidthChar::width(ch).unwrap_or(1).max(1) as i32)
                .sum::<i32>()
        })
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
            let w = UnicodeWidthChar::width(ch).unwrap_or(1).max(1);
            if cursor_col >= col && cursor_col < col + w {
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
            col += w;
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

fn rendered_term_line_for_index(
    tab: &TabState,
    idx: usize,
    cjk_fallback_font_family: &str,
) -> TermLine {
    let base_line = &tab.terminal_lines[idx];
    let rendered = if tab.terminal_cursor_row == Some(idx) {
        overlay_cursor_line(base_line, tab.terminal_cursor_col.unwrap_or(0))
    } else {
        base_line.clone()
    };
    build_term_line(&rendered, cjk_fallback_font_family)
}

/// Incremental cache: only rebuild rows inside [first, last] whose VT content changed.
/// Returns true when cached rows in this window changed.
pub(crate) fn sync_terminal_model_cache_range(
    tab: &mut TabState,
    first: usize,
    last: usize,
    cjk_fallback_font_family: &str,
) -> bool {
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
        
        let built = build_term_line(&rendered, cjk_fallback_font_family);
        tab.terminal_model_rows.insert(idx, built);
        tab.terminal_model_hashes.insert(idx, fp);
        tab.terminal_model_dirty.insert(idx);
        changed = true;
    }
    changed
}

/// Push only the visible (+ overscan) slice into Slint; set global offset and total line count.
///
/// `scroll_top_override`: when `Some`, use this instead of [`AppWindow::get_ws_terminal_scroll_top_px`]
/// for windowing. Use when switching tabs: the ScrollView may still report the **previous** tab's
/// scroll offset until the next frame, so the visible row range would be wrong (blank terminal).
pub(crate) fn push_terminal_view_to_ui(
    ui: &AppWindow,
    tab: &mut TabState,
    scroll_top_override: Option<f32>,
) {
    let cjk_fallback_font_family = ui.get_ws_terminal_cjk_fallback_font_family().to_string();
    let scroll_top = scroll_top_override.unwrap_or_else(|| ui.get_ws_terminal_scroll_top_px());
    let vh = ui.get_ws_terminal_viewport_height_px().max(1.0);
    tab.terminal_scroll_top_px = scroll_top;
    tab.terminal_view_height_px = vh;

    let footer_start = terminal_pinned_footer_start(tab);
    let body_n = scrollable_terminal_line_count(tab);
    let footer_rows: Vec<TermLine> = footer_start
        .map(|start| {
            (start..tab.terminal_lines.len())
                .map(|idx| {
                    rendered_term_line_for_index(tab, idx, cjk_fallback_font_family.as_str())
                })
                .collect()
        })
        .unwrap_or_default();
    ui.set_ws_terminal_pinned_lines(ModelRc::new(VecModel::from(footer_rows)));

    if body_n == 0 {
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
        ui.window().request_redraw();
        return;
    }

    let first_f = (scroll_top / TERMINAL_ROW_HEIGHT_PX).floor() as isize;
    let first = first_f
        .saturating_sub(TERMINAL_ROW_OVERSCAN as isize)
        .max(0) as usize;
    let last_visible_bottom = scroll_top + vh;
    let last_visible = (last_visible_bottom / TERMINAL_ROW_HEIGHT_PX).ceil() as isize;
    let last =
        (last_visible + TERMINAL_ROW_OVERSCAN as isize).clamp(0, body_n as isize - 1) as usize;
    let first = first.min(last);
    let window_changed = tab.last_window_first != first || tab.last_window_last != last;
    let total_changed = tab.last_window_total != body_n;
    let content_changed = sync_terminal_model_cache_range(
        tab,
        first,
        last,
        cjk_fallback_font_family.as_str(),
    );

    if window_changed {
        ui.set_ws_terminal_line_offset(first as i32);
    }
    if total_changed {
        ui.set_ws_terminal_total_lines(body_n as i32);
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
                    let row = tab.terminal_model_rows.get(&line_idx).cloned()
                        .unwrap_or_else(empty_term_line);
                    model.set_row_data(model_idx, row);
                }
            }
        } else {
            // 長度不一致，執行最小化增減
            let current_count = model.row_count();
            if window_len > current_count {
                // 需要增加行
                for model_idx in 0..current_count {
                    let line_idx = first + model_idx;
                    let row = tab.terminal_model_rows.get(&line_idx).cloned()
                        .unwrap_or_else(empty_term_line);
                    model.set_row_data(model_idx, row);
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
                    let row = tab.terminal_model_rows.get(&line_idx).cloned()
                        .unwrap_or_else(empty_term_line);
                    model.set_row_data(model_idx, row);
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
    tab.last_window_total = body_n;
    tab.last_pushed_scroll_top = scroll_top;
    tab.last_pushed_viewport_height = vh;

    ui.window().request_redraw();
}

pub(crate) fn sync_tab_count(ui: &AppWindow, n: usize) {
    ui.set_tab_count(n as i32);
}

pub(crate) fn sync_prompt_image_chips_to_ui(ui: &AppWindow, tab: &TabState) {
    let img_chips: Vec<PromptImageChip> = tab
        .prompt_picked_images
        .iter()
        .map(|p| PromptImageChip {
            label: SharedString::from(workspace_files::file_name_label(&p.abs_path).as_str()),
            thumb: p.preview.clone(),
            abs_path: SharedString::from(p.abs_path.as_str()),
        })
        .collect();
    ui.set_ws_prompt_images(ModelRc::new(VecModel::from(img_chips)));
}

pub(crate) fn tab_update_from_ui(tab: &mut TabState, ui: &AppWindow) {
    tab.file_path = ui.get_ws_file_path().to_string();
    tab.selected_line = ui.get_ws_selected_line();
    tab.selected_context = ui.get_ws_selected_context();
    tab.prompt = ui.get_ws_prompt();
    if prune_prompt_files_not_in_prompt(tab) {
        let chips: Vec<SharedString> = tab
            .prompt_picked_files_abs
            .iter()
            .map(|p| SharedString::from(workspace_files::file_name_label(p).as_str()))
            .collect();
        ui.set_ws_prompt_path_chips(ModelRc::new(VecModel::from(chips)));
    }
    if prune_prompt_images_not_in_prompt(tab) {
        sync_prompt_image_chips_to_ui(ui, tab);
    }
    tab.cmd_type = ui.get_ws_cmd_type().to_string();
    if tab.terminal_lines.is_empty() {
        tab.terminal_text = ui.get_ws_terminal_text().to_string();
    }
    tab.auto_scroll = ui.get_ws_auto_scroll();
    tab.terminal_select_mode = ui.get_ws_terminal_select_mode();
    tab.raw_input_mode = ui.get_ws_raw_input();
}

pub(crate) fn load_tab_to_ui(ui: &AppWindow, tab: &mut TabState) {
    UI_LAYOUT_EPOCH.with(|c| c.set(c.get().wrapping_add(1)));
    ui.set_ws_image_zoom_index(-1);
    ui.set_ws_file_path(SharedString::from(tab.file_path.as_str()));
    sync_prompt_image_chips_to_ui(ui, tab);
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
    ui.set_ws_terminal_pin_lines(SharedString::from(
        tab.terminal_pinned_footer_lines.to_string().as_str(),
    ));

    let n = scrollable_terminal_line_count(tab);
    ui.set_ws_terminal_total_lines(n as i32);
    ui.set_ws_terminal_pinned_lines(ModelRc::new(VecModel::from(Vec::<TermLine>::new())));
    // 清空舊 model 資料，強制 push_terminal_view_to_ui 完整重建
    {
        let model = &tab.terminal_slint_model;
        while model.row_count() > 0 {
            model.remove(0);
        }
    }
    tab.last_window_first = usize::MAX;
    tab.last_window_last = usize::MAX;
    tab.last_window_total = usize::MAX;
    // 綁定持久 model
    ui.set_ws_terminal_lines(ModelRc::from(Rc::clone(&tab.terminal_slint_model)));

    // One shared Slint ScrollView for all tabs: resize PTY first, then apply this tab's saved scroll
    // in px (see `terminal_saved_scroll_top_px` + `gui_state` before tab switch).
    // 設定 resize target tab ID，確保 deferred resize handler
    // 能找到正確的 tab（而非依賴 s.current）。
    RESIZE_TARGET_TAB_ID.with(|c| c.set(tab.id));
    ui.invoke_ws_bump_terminal_size();
    let vh = ui.get_ws_terminal_viewport_height_px().max(1.0);
    let scroll = clamp_saved_scroll_top(tab, vh);
    ui.invoke_ws_apply_terminal_scroll_top_px(scroll);
    push_terminal_view_to_ui(ui, tab, Some(scroll));
    tab.terminal_scroll_resync_next = true;
}
