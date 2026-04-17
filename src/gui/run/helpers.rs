//! Shared helpers for `run` (clipboard, terminal selection text, inject, CJK/raw heuristics).

use std::path::Path;

use arboard::Clipboard;

use crate::terminal::key_encoding;
use crate::terminal::render::ColoredLine;

use super::super::slint_ui::AppWindow;
use super::super::state::{GuiState, TabState};

pub(crate) fn normalize_text_for_conpty(text: &str) -> Vec<u8> {
    text.replace("\r\n", "\n").replace('\n', "\r\n").into_bytes()
}

pub(crate) fn inject_path_into_current(
    ui: &AppWindow,
    s: &mut GuiState,
    path: &Path,
) -> Result<(), String> {
    let text = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
    let bytes = normalize_text_for_conpty(&text);
    s.inject_bytes_into_current(ui, &bytes)
}

pub(crate) fn auto_disable_raw_on_cjk_prompt(ui: &AppWindow, s: &mut GuiState) {
    if s.current >= s.tabs.len() {
        return;
    }
    if !s.tabs[s.current].raw_input_mode {
        return;
    }
    let prompt = ui.get_ws_prompt().to_string();
    if !contains_cjk_char(&prompt) {
        return;
    }
    if let Err(e) = s.toggle_raw_input_current(ui) {
        eprintln!("CliGJ: raw input auto-toggle (prompt CJK): {e}");
    }
}

pub(crate) fn contains_cjk_char(text: &str) -> bool {
    text.chars().any(|ch| {
        matches!(
            ch as u32,
            0x3400..=0x4DBF // CJK Unified Ideographs Extension A
                | 0x4E00..=0x9FFF // CJK Unified Ideographs
                | 0xF900..=0xFAFF // CJK Compatibility Ideographs
                | 0x20000..=0x2CEAF // CJK Unified Ideographs Extension B-E
                | 0x2EBF0..=0x2EE5F // CJK Unified Ideographs Extension I
                | 0x3000..=0x303F // CJK Symbols and Punctuation
                | 0xFF00..=0xFFEF // Halfwidth and Fullwidth Forms
        )
    })
}

pub(crate) fn is_local_prompt_edit_key(mod_mask: u32, key: &str) -> bool {
    if mod_mask & (key_encoding::MOD_CTRL | key_encoding::MOD_ALT | key_encoding::MOD_META) != 0 {
        return false;
    }
    matches!(
        key,
        "Backspace" | "Delete" | "LeftArrow" | "RightArrow" | "Home" | "End"
    )
}

fn colored_line_plain_text(line: &ColoredLine) -> String {
    line.spans.iter().fold(String::new(), |mut acc, s| {
        acc.push_str(s.text.as_str());
        acc
    })
}

/// Inclusive character slice (Unicode scalar indices, matching Slint `char-count`).
fn slice_line_chars_inclusive(line: &ColoredLine, start: usize, end_inclusive: usize) -> String {
    let s = colored_line_plain_text(line);
    let chars: Vec<char> = s.chars().collect();
    let n = chars.len();
    if n == 0 || start > end_inclusive {
        return String::new();
    }
    let start = start.min(n - 1);
    let end_inclusive = end_inclusive.min(n - 1);
    chars[start..=end_inclusive].iter().collect()
}

fn slice_line_from_char(line: &ColoredLine, start: usize) -> String {
    let s = colored_line_plain_text(line);
    let chars: Vec<char> = s.chars().collect();
    let n = chars.len();
    if start >= n {
        return String::new();
    }
    chars[start..].iter().collect()
}

fn slice_line_to_char_inclusive(line: &ColoredLine, end_inclusive: usize) -> String {
    slice_line_chars_inclusive(line, 0, end_inclusive)
}

pub(crate) fn selected_text_from_terminal_lines(
    tab: &TabState,
    sr: i32,
    sc: i32,
    er: i32,
    ec: i32,
) -> String {
    if tab.terminal_lines.is_empty() {
        return String::new();
    }
    let sr = sr.max(0) as usize;
    let sc = sc.max(0) as usize;
    let er = er.max(0) as usize;
    let ec = ec.max(0) as usize;
    let max_row = tab.terminal_lines.len() - 1;
    let sr = sr.min(max_row);
    let er = er.min(max_row);
    if sr > er {
        return String::new();
    }
    let mut out = String::new();
    if sr == er {
        let line = &tab.terminal_lines[sr];
        return slice_line_chars_inclusive(line, sc, ec);
    }
    for row_idx in sr..=er {
        if row_idx > sr {
            out.push('\n');
        }
        let line = &tab.terminal_lines[row_idx];
        if row_idx == sr {
            out.push_str(&slice_line_from_char(line, sc));
        } else if row_idx == er {
            out.push_str(&slice_line_to_char_inclusive(line, ec));
        } else {
            out.push_str(&colored_line_plain_text(line));
        }
    }
    out
}

pub(crate) fn copy_to_clipboard(text: &str) -> Result<(), String> {
    let mut clipboard = Clipboard::new().map_err(|e| e.to_string())?;
    clipboard.set_text(text.to_string()).map_err(|e| e.to_string())
}
