use cligj_terminal::key_encoding;
use cligj_terminal::render::ColoredLine;

use super::state::{TabState, TerminalMode};

const MENU_MAX_LABEL_CHARS: usize = 120;
const MENU_MAX_WORDS: usize = 16;
const DEFAULT_BG_COLORS: &[[u8; 3]] = &[[0, 0, 0], [10, 10, 15], [18, 18, 18]];
const ARROW_MARKERS: &[&str] = &[
    "\u{276f}",
    "\u{203a}",
    ">",
    "\u{25b6}",
    "\u{25b8}",
    "\u{2192}",
    "\u{00bb}",
];
const RADIO_SELECTED_MARKERS: &[&str] = &["\u{25cf}", "\u{25c9}"];
const RADIO_UNSELECTED_MARKERS: &[&str] = &["\u{25cb}", "\u{25ef}"];
const CHECKBOX_SELECTED_MARKERS: &[&str] = &["[x]", "[X]", "[*]", "(x)", "(X)", "(*)"];
const CHECKBOX_UNSELECTED_MARKERS: &[&str] = &["[ ]", "( )"];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum MenuFamily {
    Arrow,
    Radio,
    Checkbox,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ParsedMenuLine {
    family: MenuFamily,
    indent_bytes: usize,
    selected: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ParsedCommandMenuLine {
    indent_bytes: usize,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct MenuCandidate {
    rows: Vec<usize>,
    active_row: Option<usize>,
    score: usize,
}

pub(crate) fn has_terminal_menu(tab: &TabState) -> bool {
    tab.terminal_menu_active_row.is_some() && tab.terminal_menu_rows.len() >= 2
}

pub(crate) fn plain_key_bytes(key: &str) -> Option<Vec<u8>> {
    key_encoding::encode_for_pty(0, key)
}

pub(crate) fn move_menu_row_bytes(tab: &TabState, row: usize) -> Option<Vec<u8>> {
    if !has_terminal_menu(tab) {
        return None;
    }
    let active_row = effective_menu_row(tab)?;
    let active_idx = tab.terminal_menu_rows.iter().position(|&r| r == active_row)?;
    let target_idx = tab.terminal_menu_rows.iter().position(|&r| r == row)?;

    let mut out = Vec::new();
    if target_idx > active_idx {
        let step = plain_key_bytes("DownArrow")?;
        for _ in 0..(target_idx - active_idx) {
            out.extend_from_slice(step.as_slice());
        }
    } else if target_idx < active_idx {
        let step = plain_key_bytes("UpArrow")?;
        for _ in 0..(active_idx - target_idx) {
            out.extend_from_slice(step.as_slice());
        }
    }
    Some(out)
}

pub(crate) fn activate_menu_row_bytes(tab: &TabState, row: usize) -> Option<Vec<u8>> {
    let mut out = move_menu_row_bytes(tab, row)?;
    out.extend_from_slice(plain_key_bytes("Return")?.as_slice());
    Some(out)
}

pub(crate) fn mark_menu_pending_row(tab: &mut TabState, row: usize) {
    if tab.terminal_menu_rows.contains(&row) {
        tab.terminal_menu_pending_row = Some(row);
    }
}

fn effective_menu_row(tab: &TabState) -> Option<usize> {
    if let Some(row) = tab.terminal_menu_pending_row {
        if tab.terminal_menu_rows.contains(&row) {
            return Some(row);
        }
    }
    tab.terminal_menu_active_row
}

pub(crate) fn refresh_terminal_menu_state(
    tab: &mut TabState,
    visible_first: usize,
    visible_last: usize,
) {
    if tab.terminal_mode != TerminalMode::InteractiveAi
        || tab.raw_input_mode
        || tab.terminal_lines.is_empty()
        || visible_first > visible_last
        || visible_first >= tab.terminal_lines.len()
    {
        tab.terminal_menu_rows.clear();
        tab.terminal_menu_active_row = None;
        return;
    }

    let last = visible_last.min(tab.terminal_lines.len().saturating_sub(1));
    let cursor_row = tab
        .terminal_cursor_row
        .filter(|row| *row >= visible_first && *row <= last);

    let mut best = MenuCandidate::default();

    for row in visible_first..=last {
        let text = line_plain_text(&tab.terminal_lines[row]);
        let Some(parsed) = parse_arrow_selected_line(text.as_str()) else {
            continue;
        };
        let rows = build_arrow_block(
            &tab.terminal_lines,
            row,
            visible_first,
            last,
            parsed.indent_bytes,
        );
        if rows.len() < 2 {
            continue;
        }
        let active_row = cursor_row.filter(|cursor| rows.contains(cursor)).or(Some(row));
        let score = rows.len() * 12 + usize::from(active_row == cursor_row) * 4;
        if score > best.score {
            best = MenuCandidate {
                rows,
                active_row,
                score,
            };
        }
    }

    let mut row = visible_first;
    while row <= last {
        let text = line_plain_text(&tab.terminal_lines[row]);
        let Some(parsed) = parse_command_menu_line(text.as_str()) else {
            row += 1;
            continue;
        };
        let rows = build_command_menu_block(
            &tab.terminal_lines,
            row,
            visible_first,
            last,
            parsed.indent_bytes,
        );
        if rows.len() >= 2 {
            let active_row = find_highlighted_row(&tab.terminal_lines, &rows)
                .or_else(|| cursor_row.filter(|cursor| rows.contains(cursor)))
                .or_else(|| rows.first().copied());
            let score = rows.len() * 14
                + usize::from(active_row.is_some()) * 3
                + usize::from(find_highlighted_row(&tab.terminal_lines, &rows).is_some()) * 6;
            if score > best.score {
                best = MenuCandidate {
                    rows: rows.clone(),
                    active_row,
                    score,
                };
            }
        }
        row = rows.last().copied().unwrap_or(row) + 1;
    }

    row = visible_first;
    while row <= last {
        let text = line_plain_text(&tab.terminal_lines[row]);
        let Some(parsed) = parse_explicit_family_line(text.as_str()) else {
            row += 1;
            continue;
        };
        if parsed.family == MenuFamily::Arrow {
            row += 1;
            continue;
        }
        let mut rows = vec![row];
        let mut selected_rows = if parsed.selected { vec![row] } else { Vec::new() };
        let mut end = row;
        while end < last {
            let next_text = line_plain_text(&tab.terminal_lines[end + 1]);
            let Some(next) = parse_explicit_family_line(next_text.as_str()) else {
                break;
            };
            if next.family != parsed.family || next.indent_bytes != parsed.indent_bytes {
                break;
            }
            end += 1;
            rows.push(end);
            if next.selected {
                selected_rows.push(end);
            }
        }
        if rows.len() >= 2 {
            let active_row = cursor_row
                .filter(|cursor| rows.contains(cursor))
                .or_else(|| selected_rows.first().copied())
                .or_else(|| rows.first().copied());
            let score = rows.len() * 10 + usize::from(active_row == cursor_row) * 4;
            if score > best.score {
                best = MenuCandidate {
                    rows,
                    active_row,
                    score,
                };
            }
        }
        row = end + 1;
    }

    if let Some(cursor) = cursor_row {
        let rows = build_highlight_block(&tab.terminal_lines, cursor, visible_first, last);
        if rows.len() >= 2 {
            let highlighted = line_has_non_default_bg(&tab.terminal_lines[cursor]);
            let score = rows.len() * if highlighted { 7 } else { 5 } + if highlighted { 4 } else { 1 };
            if score > best.score {
                best = MenuCandidate {
                    rows,
                    active_row: Some(cursor),
                    score,
                };
            }
        }
    }

    tab.terminal_menu_rows = best.rows;
    tab.terminal_menu_active_row = best.active_row;
    if tab
        .terminal_menu_pending_row
        .is_some_and(|row| !tab.terminal_menu_rows.contains(&row) || tab.terminal_menu_active_row == Some(row))
    {
        tab.terminal_menu_pending_row = None;
    }
}

fn build_arrow_block(
    lines: &[ColoredLine],
    selected_row: usize,
    visible_first: usize,
    visible_last: usize,
    indent_bytes: usize,
) -> Vec<usize> {
    let mut start = selected_row;
    while start > visible_first {
        let text = line_plain_text(&lines[start - 1]);
        if parse_arrow_block_line(text.as_str(), indent_bytes).is_some() {
            start -= 1;
        } else {
            break;
        }
    }

    let mut end = selected_row;
    while end < visible_last {
        let text = line_plain_text(&lines[end + 1]);
        if parse_arrow_block_line(text.as_str(), indent_bytes).is_some() {
            end += 1;
        } else {
            break;
        }
    }

    (start..=end).collect()
}

fn build_highlight_block(
    lines: &[ColoredLine],
    cursor_row: usize,
    visible_first: usize,
    visible_last: usize,
) -> Vec<usize> {
    let cursor_text = line_plain_text(&lines[cursor_row]);
    let base_indent = leading_whitespace_bytes(cursor_text.as_str());
    let mut start = cursor_row;
    while start > visible_first {
        if is_plain_menu_candidate(&lines[start - 1], base_indent) {
            start -= 1;
        } else {
            break;
        }
    }

    let mut end = cursor_row;
    while end < visible_last {
        if is_plain_menu_candidate(&lines[end + 1], base_indent) {
            end += 1;
        } else {
            break;
        }
    }

    (start..=end).collect()
}

fn build_command_menu_block(
    lines: &[ColoredLine],
    anchor_row: usize,
    visible_first: usize,
    visible_last: usize,
    indent_bytes: usize,
) -> Vec<usize> {
    let mut start = anchor_row;
    while start > visible_first {
        let text = line_plain_text(&lines[start - 1]);
        let Some(parsed) = parse_command_menu_line(text.as_str()) else {
            break;
        };
        if parsed.indent_bytes != indent_bytes {
            break;
        }
        start -= 1;
    }

    let mut end = anchor_row;
    while end < visible_last {
        let text = line_plain_text(&lines[end + 1]);
        let Some(parsed) = parse_command_menu_line(text.as_str()) else {
            break;
        };
        if parsed.indent_bytes != indent_bytes {
            break;
        }
        end += 1;
    }

    (start..=end).collect()
}

fn parse_arrow_selected_line(text: &str) -> Option<ParsedMenuLine> {
    let trimmed = text.trim_end();
    let indent_bytes = leading_whitespace_bytes(trimmed);
    let rest = &trimmed[indent_bytes..];
    let label = strip_prefixed_label(rest, ARROW_MARKERS)?;
    is_menuish_label(label).then_some(ParsedMenuLine {
        family: MenuFamily::Arrow,
        indent_bytes,
        selected: true,
    })
}

fn parse_arrow_block_line(text: &str, indent_bytes: usize) -> Option<ParsedMenuLine> {
    let trimmed = text.trim_end();
    let line_indent = leading_whitespace_bytes(trimmed);
    if line_indent != indent_bytes {
        return None;
    }
    let rest = &trimmed[indent_bytes..];
    if let Some(label) = strip_prefixed_label(rest, ARROW_MARKERS) {
        if is_menuish_label(label) {
            return Some(ParsedMenuLine {
                family: MenuFamily::Arrow,
                indent_bytes,
                selected: true,
            });
        }
    }
    let pad_bytes = leading_whitespace_bytes(rest);
    if pad_bytes < 2 {
        return None;
    }
    let label = rest[pad_bytes..].trim();
    is_menuish_label(label).then_some(ParsedMenuLine {
        family: MenuFamily::Arrow,
        indent_bytes,
        selected: false,
    })
}

fn parse_explicit_family_line(text: &str) -> Option<ParsedMenuLine> {
    parse_prefixed_family_line(
        text,
        MenuFamily::Radio,
        RADIO_SELECTED_MARKERS,
        RADIO_UNSELECTED_MARKERS,
    )
    .or_else(|| {
        parse_prefixed_family_line(
            text,
            MenuFamily::Checkbox,
            CHECKBOX_SELECTED_MARKERS,
            CHECKBOX_UNSELECTED_MARKERS,
        )
    })
}

fn parse_command_menu_line(text: &str) -> Option<ParsedCommandMenuLine> {
    let trimmed = text.trim_end();
    let indent_bytes = leading_whitespace_bytes(trimmed);
    let rest = &trimmed[indent_bytes..];
    let (label, desc) = split_gap_columns(rest)?;
    if !is_command_label(label) || !is_command_description(desc) {
        return None;
    }
    Some(ParsedCommandMenuLine { indent_bytes })
}

fn parse_prefixed_family_line(
    text: &str,
    family: MenuFamily,
    selected_prefixes: &[&str],
    unselected_prefixes: &[&str],
) -> Option<ParsedMenuLine> {
    let trimmed = text.trim_end();
    let indent_bytes = leading_whitespace_bytes(trimmed);
    let rest = &trimmed[indent_bytes..];
    if let Some(label) = strip_prefixed_label(rest, selected_prefixes) {
        if is_menuish_label(label) {
            return Some(ParsedMenuLine {
                family,
                indent_bytes,
                selected: true,
            });
        }
    }
    if let Some(label) = strip_prefixed_label(rest, unselected_prefixes) {
        if is_menuish_label(label) {
            return Some(ParsedMenuLine {
                family,
                indent_bytes,
                selected: false,
            });
        }
    }
    None
}

fn strip_prefixed_label<'a>(text: &'a str, prefixes: &[&str]) -> Option<&'a str> {
    for prefix in prefixes {
        if let Some(rest) = text.strip_prefix(prefix) {
            let label = rest.trim_start();
            if !label.is_empty() {
                return Some(label);
            }
        }
    }
    None
}

fn is_menuish_label(label: &str) -> bool {
    let trimmed = label.trim();
    if trimmed.is_empty() {
        return false;
    }
    let char_len = trimmed.chars().count();
    char_len > 0
        && char_len <= MENU_MAX_LABEL_CHARS
        && trimmed.split_whitespace().count() <= MENU_MAX_WORDS
}

fn is_plain_menu_candidate(line: &ColoredLine, base_indent: usize) -> bool {
    if !line_has_visible_text(line) {
        return false;
    }
    let text = line_plain_text(line);
    let trimmed = text.trim();
    if !is_menuish_label(trimmed) {
        return false;
    }
    let indent = leading_whitespace_bytes(text.as_str());
    indent.abs_diff(base_indent) <= 2
}

fn split_gap_columns(text: &str) -> Option<(&str, &str)> {
    let mut gap_start: Option<usize> = None;
    let mut gap_width = 0usize;
    let mut gap_has_tab = false;
    let mut saw_non_ws = false;

    for (idx, ch) in text.char_indices() {
        if ch.is_whitespace() {
            if saw_non_ws {
                if gap_start.is_none() {
                    gap_start = Some(idx);
                }
                gap_width += 1;
                if ch == '\t' {
                    gap_has_tab = true;
                }
            }
            continue;
        }

        if let Some(start) = gap_start {
            if gap_width >= 2 || gap_has_tab {
                let left = text[..start].trim_end();
                let right = text[idx..].trim_start();
                if !left.is_empty() && !right.is_empty() {
                    return Some((left, right));
                }
            }
            gap_start = None;
            gap_width = 0;
            gap_has_tab = false;
        }

        saw_non_ws = true;
    }

    None
}

fn is_command_label(text: &str) -> bool {
    let trimmed = text.trim();
    let mut chars = trimmed.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    if trimmed.chars().count() > 32 || trimmed.chars().any(|ch| ch.is_whitespace()) {
        return false;
    }
    if !(first == '/' || first.is_ascii_alphanumeric()) {
        return false;
    }
    trimmed.chars().all(|ch| {
        ch.is_ascii_alphanumeric()
            || matches!(ch, '/' | '-' | '_' | '.' | ':' | '+' | '#' | '?' | '@')
    })
}

fn is_command_description(text: &str) -> bool {
    let trimmed = text.trim();
    !trimmed.is_empty()
        && trimmed.chars().count() <= 120
        && trimmed.chars().any(|ch| ch.is_ascii_alphabetic())
}

fn find_highlighted_row(lines: &[ColoredLine], rows: &[usize]) -> Option<usize> {
    rows.iter()
        .copied()
        .find(|&row| lines.get(row).is_some_and(line_has_non_default_bg))
}

fn line_has_visible_text(line: &ColoredLine) -> bool {
    line.spans
        .iter()
        .any(|span| span.text.chars().any(|ch| !ch.is_whitespace()))
}

fn line_has_non_default_bg(line: &ColoredLine) -> bool {
    line.spans.iter().any(|span| {
        span.text.chars().any(|ch| !ch.is_whitespace()) && !DEFAULT_BG_COLORS.contains(&span.bg)
    })
}

fn line_plain_text(line: &ColoredLine) -> String {
    let mut text = String::new();
    for span in &line.spans {
        text.push_str(span.text.as_str());
    }
    text
}

fn leading_whitespace_bytes(text: &str) -> usize {
    text.len() - text.trim_start_matches(char::is_whitespace).len()
}

#[cfg(test)]
mod tests {
    use super::*;
    use cligj_terminal::render::ColoredSpan;

    fn line(text: &str) -> ColoredLine {
        ColoredLine {
            blank: text.trim().is_empty(),
            spans: if text.is_empty() {
                Vec::new()
            } else {
                vec![ColoredSpan {
                    text: text.to_string(),
                    fg: [240, 240, 240],
                    bg: [18, 18, 18],
                }]
            },
        }
    }

    fn highlighted_line(text: &str) -> ColoredLine {
        ColoredLine {
            blank: false,
            spans: vec![ColoredSpan {
                text: text.to_string(),
                fg: [18, 18, 18],
                bg: [40, 120, 210],
            }],
        }
    }

    #[test]
    fn detects_arrow_menu_block() {
        let mut tab = TabState::new_for_test();
        tab.terminal_mode = TerminalMode::InteractiveAi;
        tab.terminal_lines = vec![line("  Alpha"), line("❯ Beta"), line("  Gamma")];
        refresh_terminal_menu_state(&mut tab, 0, 2);
        assert_eq!(tab.terminal_menu_rows, vec![0, 1, 2]);
        assert_eq!(tab.terminal_menu_active_row, Some(1));
    }

    #[test]
    fn detects_highlighted_plain_menu_block() {
        let mut tab = TabState::new_for_test();
        tab.terminal_mode = TerminalMode::InteractiveAi;
        tab.terminal_cursor_row = Some(1);
        tab.terminal_lines = vec![
            line("Edit"),
            highlighted_line("Delete"),
            line("Cancel"),
            line("Long paragraph lines should not become a menu because they exceed the label guard"),
        ];
        refresh_terminal_menu_state(&mut tab, 0, 3);
        assert_eq!(tab.terminal_menu_rows, vec![0, 1, 2]);
        assert_eq!(tab.terminal_menu_active_row, Some(1));
    }

    #[test]
    fn click_bytes_move_and_confirm() {
        let mut tab = TabState::new_for_test();
        tab.terminal_mode = TerminalMode::InteractiveAi;
        tab.terminal_menu_rows = vec![4, 5, 6];
        tab.terminal_menu_active_row = Some(4);
        let bytes = activate_menu_row_bytes(&tab, 6).unwrap();
        let mut expected = plain_key_bytes("DownArrow").unwrap();
        expected.extend_from_slice(plain_key_bytes("DownArrow").unwrap().as_slice());
        expected.extend_from_slice(plain_key_bytes("Return").unwrap().as_slice());
        assert_eq!(bytes, expected);
    }

    #[test]
    fn hover_bytes_use_pending_row_as_effective_active() {
        let mut tab = TabState::new_for_test();
        tab.terminal_mode = TerminalMode::InteractiveAi;
        tab.terminal_menu_rows = vec![4, 5, 6];
        tab.terminal_menu_active_row = Some(4);
        tab.terminal_menu_pending_row = Some(5);
        let bytes = move_menu_row_bytes(&tab, 6).unwrap();
        assert_eq!(bytes, plain_key_bytes("DownArrow").unwrap());
    }

    #[test]
    fn detects_plain_slash_command_menu() {
        let mut tab = TabState::new_for_test();
        tab.terminal_mode = TerminalMode::InteractiveAi;
        tab.terminal_lines = vec![
            line("/help    Show help"),
            highlighted_line("/model   Choose model"),
            line("/review  Review changes"),
        ];
        refresh_terminal_menu_state(&mut tab, 0, 2);
        assert_eq!(tab.terminal_menu_rows, vec![0, 1, 2]);
        assert_eq!(tab.terminal_menu_active_row, Some(1));
    }

    #[test]
    fn detects_gemini_style_command_menu() {
        let mut tab = TabState::new_for_test();
        tab.terminal_mode = TerminalMode::InteractiveAi;
        tab.terminal_lines = vec![
            highlighted_line("clear      Clear the screen and start a new session"),
            line("compress   Compresses the context by replacing it with a summary"),
            line("directory  Manage workspace directories"),
            line("(1/41)"),
        ];
        refresh_terminal_menu_state(&mut tab, 0, 3);
        assert_eq!(tab.terminal_menu_rows, vec![0, 1, 2]);
        assert_eq!(tab.terminal_menu_active_row, Some(0));
    }
}
