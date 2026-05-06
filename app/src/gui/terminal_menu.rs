use std::borrow::Cow;

use cligj_terminal::key_encoding;
use cligj_terminal::render::ColoredLine;

use super::state::{TabState, TerminalMode};

const MENU_MAX_LABEL_CHARS: usize = 120;
const MENU_MAX_WORDS: usize = 16;
const PLAIN_MENU_MAX_LABEL_CHARS: usize = 48;
const PLAIN_MENU_MAX_WORDS: usize = 6;
const COMMAND_LABEL_MAX_CHARS: usize = 48;
const COMMAND_LABEL_MAX_WORDS: usize = 5;
const COMMAND_DESCRIPTION_MAX_CHARS: usize = 240;
const DEFAULT_BG_COLORS: &[[u8; 3]] = &[[0, 0, 0], [10, 10, 15], [18, 18, 18]];
// Some Windows/codepage fallback paths degrade the selection chevron into literal question marks.
const ARROW_MARKERS: &[&str] = &[
    "??", "\u{276f}", "\u{203a}", ">", "\u{25b6}", "\u{25b8}", "\u{2192}", "\u{00bb}",
];
const RADIO_SELECTED_MARKERS: &[&str] = &["\u{25cf}", "\u{25c9}"];
const RADIO_UNSELECTED_MARKERS: &[&str] = &["\u{25cb}", "\u{25ef}"];
const CHECKBOX_SELECTED_MARKERS: &[&str] = &[
    "[x]",
    "[X]",
    "[*]",
    "[\u{221a}]",
    "[\u{2713}]",
    "[\u{2714}]",
    "(x)",
    "(X)",
    "(*)",
    "(\u{221a})",
    "(\u{2713})",
    "(\u{2714})",
];
const CHECKBOX_UNSELECTED_MARKERS: &[&str] = &["[ ]", "( )"];
const FRAME_MARKERS: &[char] = &['│', '┃', '║', '|'];

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
    slash_command: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct MenuCandidate {
    rows: Vec<usize>,
    active_row: Option<usize>,
    score: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DescribedMenuRowKind {
    Selectable,
    Detail,
    Blank,
    Indicator,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum HighlightMenuRowKind {
    Selectable,
    Detail,
    Blank,
    Indicator,
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
    let active_idx = tab
        .terminal_menu_rows
        .iter()
        .position(|&r| r == active_row)?;
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

pub(crate) fn move_menu_edge_bytes(tab: &TabState, direction: i32) -> Option<(usize, Vec<u8>)> {
    if !has_terminal_menu(tab) || tab.terminal_menu_rows.len() < 2 {
        return None;
    }
    if direction < 0 {
        let target_row = *tab.terminal_menu_rows.first()?;
        let bytes = if effective_menu_row(tab) == Some(target_row) {
            plain_key_bytes("UpArrow")?
        } else {
            move_menu_row_bytes(tab, target_row)?
        };
        return Some((target_row, bytes));
    }
    if direction > 0 {
        let target_row = *tab.terminal_menu_rows.last()?;
        let bytes = if effective_menu_row(tab) == Some(target_row) {
            plain_key_bytes("DownArrow")?
        } else {
            move_menu_row_bytes(tab, target_row)?
        };
        return Some((target_row, bytes));
    }
    None
}

pub(crate) fn mark_menu_pending_row(tab: &mut TabState, row: usize) {
    if tab.terminal_menu_rows.contains(&row) {
        tab.terminal_menu_pending_row = Some(row);
    }
}

pub(crate) fn effective_menu_row(tab: &TabState) -> Option<usize> {
    if let Some(row) = tab.terminal_menu_pending_row {
        if tab.terminal_menu_rows.contains(&row) {
            return Some(row);
        }
    }
    tab.terminal_menu_active_row
}

pub(crate) fn menu_hit_cols(line: &ColoredLine) -> Option<(i32, i32)> {
    let mut first = None;
    let mut last = None;
    let mut col = 0i32;
    for span in &line.spans {
        for ch in span.text.chars() {
            if !ch.is_whitespace() {
                first.get_or_insert(col);
                last = Some(col);
            }
            col += 1;
        }
    }
    Some((first?, last?))
}

pub(crate) fn refresh_terminal_menu_state(
    tab: &mut TabState,
    visible_first: usize,
    visible_last: usize,
) {
    if tab.terminal_mode != TerminalMode::InteractiveAi
        || tab.terminal_lines.is_empty()
        || visible_first > visible_last
        || visible_first >= tab.terminal_lines.len()
    {
        tab.terminal_menu_rows.clear();
        tab.terminal_menu_active_row = None;
        tab.terminal_menu_pending_row = None;
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
        let active_row = cursor_row
            .filter(|cursor| rows.contains(cursor))
            .or(Some(row));
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
        let slash_like = command_block_has_slash_label(&tab.terminal_lines, &rows);
        let effective_rows = if slash_like {
            filter_command_rows(&tab.terminal_lines, &rows, true)
        } else {
            rows.clone()
        };
        if effective_rows.len() >= 2 {
            let highlighted_row = find_highlighted_row(&tab.terminal_lines, &effective_rows);
            // Plain two-column status blocks (for example Codex model/cwd banners) are not menus.
            if highlighted_row.is_none() && !slash_like {
                row = rows.last().copied().unwrap_or(row) + 1;
                continue;
            }
            let active_row = highlighted_row
                .or_else(|| cursor_row.filter(|cursor| effective_rows.contains(cursor)))
                .or_else(|| effective_rows.first().copied());
            let score = effective_rows.len() * 14
                + usize::from(active_row.is_some()) * 3
                + usize::from(highlighted_row.is_some()) * 6;
            if score > best.score {
                best = MenuCandidate {
                    rows: effective_rows,
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
        let mut selected_rows = if parsed.selected {
            vec![row]
        } else {
            Vec::new()
        };
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

    row = visible_first;
    while row <= last {
        let text = line_plain_text(&tab.terminal_lines[row]);
        let Some(parsed) = parse_explicit_family_line(text.as_str()) else {
            row += 1;
            continue;
        };
        let (rows, highlighted_row, end) = build_described_explicit_menu_block(
            &tab.terminal_lines,
            row,
            visible_first,
            last,
            parsed.family,
            parsed.indent_bytes,
        );
        if rows.len() >= 2 {
            let active_row = highlighted_row
                .or_else(|| cursor_row.filter(|cursor| rows.contains(cursor)))
                .or_else(|| rows.first().copied());
            let score = rows.len() * 13
                + usize::from(active_row == cursor_row) * 4
                + usize::from(highlighted_row.is_some()) * 5;
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

    row = visible_first;
    while row <= last {
        if !line_has_non_default_bg(&tab.terminal_lines[row]) {
            row += 1;
            continue;
        }
        let (rows, active_row, end) =
            build_highlighted_single_column_menu_block(&tab.terminal_lines, row, last);
        if rows.len() >= 2 {
            let score = rows.len() * 11
                + usize::from(active_row == cursor_row) * 4
                + usize::from(active_row.is_some()) * 5;
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
            if highlighted {
                let score = rows.len() * 7 + 4;
                if score > best.score {
                    best = MenuCandidate {
                        rows,
                        active_row: Some(cursor),
                        score,
                    };
                }
            }
        }
    }

    tab.terminal_menu_rows = best.rows;
    tab.terminal_menu_active_row = best.active_row;
    if tab.terminal_menu_pending_row.is_some_and(|row| {
        !tab.terminal_menu_rows.contains(&row) || tab.terminal_menu_active_row == Some(row)
    }) {
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

fn build_described_explicit_menu_block(
    lines: &[ColoredLine],
    anchor_row: usize,
    _visible_first: usize,
    visible_last: usize,
    family: MenuFamily,
    indent_bytes: usize,
) -> (Vec<usize>, Option<usize>, usize) {
    let mut rows = Vec::new();
    let mut highlighted_row = None;
    let mut last_selectable = anchor_row;
    let mut blank_run = 0usize;
    let mut row = anchor_row;

    while row <= visible_last {
        let text = line_plain_text(&lines[row]);
        let kind = classify_described_menu_row(text.as_str(), family, indent_bytes);
        match kind {
            Some(DescribedMenuRowKind::Selectable) => {
                rows.push(row);
                last_selectable = row;
                blank_run = 0;
                if line_has_non_default_bg(&lines[row]) {
                    highlighted_row = Some(row);
                }
                row += 1;
            }
            Some(DescribedMenuRowKind::Detail) | Some(DescribedMenuRowKind::Indicator) => {
                blank_run = 0;
                row += 1;
            }
            Some(DescribedMenuRowKind::Blank) => {
                if rows.is_empty() {
                    break;
                }
                blank_run += 1;
                if blank_run > 1 {
                    break;
                }
                row += 1;
            }
            None => break,
        }
    }

    (rows, highlighted_row, last_selectable)
}

fn build_highlighted_single_column_menu_block(
    lines: &[ColoredLine],
    anchor_row: usize,
    visible_last: usize,
) -> (Vec<usize>, Option<usize>, usize) {
    let base_text = line_plain_text(&lines[anchor_row]);
    let normalized = normalize_optional_side_frame(base_text.as_str());
    let base_indent = leading_whitespace_bytes(normalized.as_ref());

    let mut rows = vec![anchor_row];
    let mut last_selectable = anchor_row;
    let mut blank_run = 0usize;
    let mut row = anchor_row + 1;
    while row <= visible_last {
        let text = line_plain_text(&lines[row]);
        match classify_highlight_menu_row(&lines[row], text.as_str(), base_indent) {
            Some(HighlightMenuRowKind::Selectable) => {
                rows.push(row);
                last_selectable = row;
                blank_run = 0;
                row += 1;
            }
            Some(HighlightMenuRowKind::Detail) | Some(HighlightMenuRowKind::Indicator) => {
                blank_run = 0;
                row += 1;
            }
            Some(HighlightMenuRowKind::Blank) => {
                blank_run += 1;
                if blank_run > 1 {
                    break;
                }
                row += 1;
            }
            None => break,
        }
    }

    let mut row = anchor_row;
    while row > 0 {
        let prev_row = row - 1;
        let text = line_plain_text(&lines[prev_row]);
        match classify_highlight_menu_row(&lines[prev_row], text.as_str(), base_indent) {
            Some(HighlightMenuRowKind::Selectable) => {
                rows.insert(0, prev_row);
                row = prev_row;
            }
            _ => break,
        }
    }

    (rows, Some(anchor_row), last_selectable)
}

fn parse_arrow_selected_line(text: &str) -> Option<ParsedMenuLine> {
    let normalized = normalize_optional_side_frame(text);
    let trimmed = normalized.as_ref();
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
    let normalized = normalize_optional_side_frame(text);
    let trimmed = normalized.as_ref();
    let line_indent = leading_whitespace_bytes(trimmed);
    if line_indent != indent_bytes && line_indent != indent_bytes.saturating_add(2) {
        return None;
    }
    // `indent_bytes` comes from another row in the same candidate block, so it may not be a
    // valid UTF-8 boundary for this row. Treat that as "not the same menu block", not a panic.
    let rest = trimmed.get(indent_bytes..)?;
    if let Some(label) = strip_prefixed_label(rest, ARROW_MARKERS) {
        if line_indent == indent_bytes && is_menuish_label(label) {
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

fn classify_described_menu_row(
    text: &str,
    family: MenuFamily,
    indent_bytes: usize,
) -> Option<DescribedMenuRowKind> {
    if text.trim().is_empty() {
        return Some(DescribedMenuRowKind::Blank);
    }

    let normalized = normalize_optional_side_frame(text);
    let trimmed = normalized.as_ref();
    let line_indent = leading_whitespace_bytes(trimmed);

    if let Some(parsed) = parse_explicit_family_line(trimmed) {
        if parsed.family == family && line_indent.abs_diff(indent_bytes) <= 2 {
            return Some(DescribedMenuRowKind::Selectable);
        }
    }

    if parse_arrow_selected_line(trimmed).is_some()
        || (line_indent == indent_bytes && parse_arrow_block_line(trimmed, indent_bytes).is_some())
    {
        return Some(DescribedMenuRowKind::Selectable);
    }

    if is_menu_scroll_indicator(trimmed) {
        return Some(DescribedMenuRowKind::Indicator);
    }

    if line_indent > indent_bytes && line_has_visible_text_from_text(trimmed) {
        return Some(DescribedMenuRowKind::Detail);
    }

    None
}

fn classify_highlight_menu_row(
    line: &ColoredLine,
    text: &str,
    indent_bytes: usize,
) -> Option<HighlightMenuRowKind> {
    if text.trim().is_empty() {
        return Some(HighlightMenuRowKind::Blank);
    }

    let normalized = normalize_optional_side_frame(text);
    let trimmed = normalized.as_ref();
    let line_indent = leading_whitespace_bytes(trimmed);

    if is_menu_scroll_indicator(trimmed) {
        return Some(HighlightMenuRowKind::Indicator);
    }

    if is_highlight_menu_header(line, trimmed, indent_bytes) {
        return None;
    }

    if line_indent > indent_bytes && line_has_visible_text_from_text(trimmed) {
        return Some(HighlightMenuRowKind::Detail);
    }

    if is_highlight_menu_selectable_text(trimmed, indent_bytes) {
        return Some(HighlightMenuRowKind::Selectable);
    }

    None
}

fn parse_command_menu_line(text: &str) -> Option<ParsedCommandMenuLine> {
    let normalized = normalize_optional_side_frame(text);
    let trimmed = normalized.as_ref();
    let indent_bytes = leading_whitespace_bytes(trimmed);
    let rest = &trimmed[indent_bytes..];
    let (label, desc) = split_gap_columns(rest)?;
    if !is_command_label(label) || !is_command_description(desc) {
        return None;
    }
    Some(ParsedCommandMenuLine {
        indent_bytes,
        slash_command: label.trim_start().starts_with('/'),
    })
}

fn parse_prefixed_family_line(
    text: &str,
    family: MenuFamily,
    selected_prefixes: &[&str],
    unselected_prefixes: &[&str],
) -> Option<ParsedMenuLine> {
    let normalized = normalize_optional_side_frame(text);
    let trimmed = normalized.as_ref();
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

fn is_menu_scroll_indicator(text: &str) -> bool {
    matches!(
        text.trim(),
        "\u{25b2}" | "\u{25bc}" | "\u{25b4}" | "\u{25be}" | "^" | "v"
    )
}

fn is_highlight_menu_selectable_text(text: &str, indent_bytes: usize) -> bool {
    let normalized = normalize_optional_side_frame(text);
    let trimmed = normalized.as_ref().trim();
    if trimmed.is_empty() {
        return false;
    }
    let line_indent = leading_whitespace_bytes(normalized.as_ref());
    let char_len = trimmed.chars().count();
    line_indent.abs_diff(indent_bytes) <= 2
        && char_len <= 80
        && trimmed.split_whitespace().count() <= 12
        && !trimmed.ends_with(':')
}

fn is_highlight_menu_header(line: &ColoredLine, text: &str, indent_bytes: usize) -> bool {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return false;
    }
    let line_indent = leading_whitespace_bytes(text);
    line_indent.abs_diff(indent_bytes) <= 2
        && !line_has_non_default_bg(line)
        && line_accented_fg_visible_chars(line) >= 3
        && trimmed.split_whitespace().count() <= 3
}

fn is_plain_menu_candidate(line: &ColoredLine, base_indent: usize) -> bool {
    if !line_has_visible_text(line) {
        return false;
    }
    let text = normalize_optional_side_frame(line_plain_text(line).as_str()).into_owned();
    let trimmed = text.trim();
    if trimmed.is_empty()
        || trimmed.chars().count() > PLAIN_MENU_MAX_LABEL_CHARS
        || trimmed.split_whitespace().count() > PLAIN_MENU_MAX_WORDS
    {
        return false;
    }
    let indent = leading_whitespace_bytes(text.as_str());
    indent.abs_diff(base_indent) <= 2
}

fn normalize_optional_side_frame(text: &str) -> Cow<'_, str> {
    let trimmed = text.trim_end();
    let indent_bytes = leading_whitespace_bytes(trimmed);
    let Some(mut rest) = trimmed.get(indent_bytes..) else {
        return Cow::Borrowed(trimmed);
    };

    if let Some(ch) = rest.chars().next() {
        if FRAME_MARKERS.contains(&ch) {
            rest = &rest[ch.len_utf8()..];
            if let Some(next) = rest.chars().next() {
                if next.is_whitespace() {
                    rest = &rest[next.len_utf8()..];
                }
            }
        } else {
            return Cow::Borrowed(trimmed);
        }
    } else {
        return Cow::Borrowed(trimmed);
    }

    rest = rest.trim_end_matches(char::is_whitespace);
    if let Some(ch) = rest.chars().last() {
        if FRAME_MARKERS.contains(&ch) {
            rest = &rest[..rest.len() - ch.len_utf8()];
            rest = rest.trim_end_matches(char::is_whitespace);
        }
    }

    let mut normalized = String::with_capacity(indent_bytes + rest.len());
    normalized.push_str(&trimmed[..indent_bytes]);
    normalized.push_str(rest);
    Cow::Owned(normalized)
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
    if trimmed.chars().count() > COMMAND_LABEL_MAX_CHARS
        || trimmed.split_whitespace().count() > COMMAND_LABEL_MAX_WORDS
        || trimmed.ends_with(':')
    {
        return false;
    }
    if !(first == '/' || first.is_ascii_alphanumeric()) {
        return false;
    }
    trimmed.chars().all(|ch| {
        ch.is_ascii_alphanumeric()
            || ch.is_ascii_whitespace()
            || matches!(
                ch,
                '/' | '-'
                    | '_'
                    | '.'
                    | ':'
                    | '+'
                    | '#'
                    | '?'
                    | '@'
                    | '&'
                    | '\''
                    | '"'
                    | ','
                    | '('
                    | ')'
            )
    })
}

fn is_command_description(text: &str) -> bool {
    let trimmed = text.trim();
    !trimmed.is_empty()
        && trimmed.chars().count() <= COMMAND_DESCRIPTION_MAX_CHARS
        && trimmed.chars().any(|ch| ch.is_ascii_alphabetic())
}

fn command_block_has_slash_label(lines: &[ColoredLine], rows: &[usize]) -> bool {
    rows.iter().copied().any(|row| {
        let Some(line) = lines.get(row) else {
            return false;
        };
        parse_command_menu_line(line_plain_text(line).as_str())
            .is_some_and(|parsed| parsed.slash_command)
    })
}

fn filter_command_rows(lines: &[ColoredLine], rows: &[usize], slash_command: bool) -> Vec<usize> {
    rows.iter()
        .copied()
        .filter(|&row| {
            let Some(line) = lines.get(row) else {
                return false;
            };
            parse_command_menu_line(line_plain_text(line).as_str())
                .is_some_and(|parsed| parsed.slash_command == slash_command)
        })
        .collect()
}

fn find_highlighted_row(lines: &[ColoredLine], rows: &[usize]) -> Option<usize> {
    rows.iter()
        .copied()
        .find(|&row| lines.get(row).is_some_and(line_has_non_default_bg))
        .or_else(|| find_accented_fg_row(lines, rows))
}

fn find_accented_fg_row(lines: &[ColoredLine], rows: &[usize]) -> Option<usize> {
    let mut best: Option<(usize, usize)> = None;
    let mut tie = false;
    for &row in rows {
        let Some(line) = lines.get(row) else {
            continue;
        };
        let score = line_accented_fg_visible_chars(line);
        if score < 4 {
            continue;
        }
        match best {
            None => {
                best = Some((row, score));
                tie = false;
            }
            Some((_, best_score)) if score > best_score => {
                best = Some((row, score));
                tie = false;
            }
            Some((_, best_score)) if score == best_score => {
                tie = true;
            }
            _ => {}
        }
    }
    if tie {
        return None;
    }
    best.map(|(row, _)| row)
}

fn line_accented_fg_visible_chars(line: &ColoredLine) -> usize {
    line.spans
        .iter()
        .filter(|span| is_accent_color(span.fg))
        .map(|span| span.text.chars().filter(|ch| !ch.is_whitespace()).count())
        .sum()
}

fn is_accent_color(rgb: [u8; 3]) -> bool {
    rgb[0].abs_diff(rgb[1]) > 12 || rgb[1].abs_diff(rgb[2]) > 12 || rgb[0].abs_diff(rgb[2]) > 12
}

fn line_has_visible_text(line: &ColoredLine) -> bool {
    line.spans
        .iter()
        .any(|span| span.text.chars().any(|ch| !ch.is_whitespace()))
}

fn line_has_visible_text_from_text(text: &str) -> bool {
    text.chars().any(|ch| !ch.is_whitespace())
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

    fn accented_line(text: &str) -> ColoredLine {
        ColoredLine {
            blank: false,
            spans: vec![ColoredSpan {
                text: text.to_string(),
                fg: [90, 220, 230],
                bg: [18, 18, 18],
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
            line(
                "Long paragraph lines should not become a menu because they exceed the label guard",
            ),
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
    fn edge_hover_moves_past_visible_boundary_when_requested() {
        let mut tab = TabState::new_for_test();
        tab.terminal_mode = TerminalMode::InteractiveAi;
        tab.terminal_menu_rows = vec![4, 5, 6];

        tab.terminal_menu_active_row = Some(4);
        let (target_row, bytes) = move_menu_edge_bytes(&tab, -1).unwrap();
        assert_eq!(target_row, 4);
        assert_eq!(bytes, plain_key_bytes("UpArrow").unwrap());

        tab.terminal_menu_active_row = Some(6);
        let (target_row, bytes) = move_menu_edge_bytes(&tab, 1).unwrap();
        assert_eq!(target_row, 6);
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
    fn detects_command_menu_while_raw_input_mode_is_enabled() {
        let mut tab = TabState::new_for_test();
        tab.terminal_mode = TerminalMode::InteractiveAi;
        tab.raw_input_mode = true;
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

    #[test]
    fn detects_multi_word_gemini_submenu_items() {
        let mut tab = TabState::new_for_test();
        tab.terminal_mode = TerminalMode::InteractiveAi;
        tab.terminal_lines = vec![
            highlighted_line("add directory      Add a workspace directory"),
            line("remove directory   Remove a workspace directory"),
            line("list directories   Show configured directories"),
        ];
        refresh_terminal_menu_state(&mut tab, 0, 2);
        assert_eq!(tab.terminal_menu_rows, vec![0, 1, 2]);
        assert_eq!(tab.terminal_menu_active_row, Some(0));
    }

    #[test]
    fn detects_checkbox_menu_with_descriptions_and_action_row() {
        let mut tab = TabState::new_for_test();
        tab.terminal_mode = TerminalMode::InteractiveAi;
        tab.terminal_lines = vec![
            line("[\u{2713}] auth"),
            line("    Current authentication info"),
            line("[\u{2713}] code-changes"),
            line("    Lines added/removed in the session (not shown when zero)"),
            line("[\u{2713}] token-count"),
            line("    Total tokens used in the session (not shown when zero)"),
            line("[\u{2713}] Show footer labels"),
            line(""),
            highlighted_line("> Reset to default footer"),
            line(""),
            line("\u{25bc}"),
            line("Enter to select · ↑/↓ to navigate · ←/→ to reorder · Esc to close"),
        ];
        refresh_terminal_menu_state(&mut tab, 0, 11);
        assert_eq!(tab.terminal_menu_rows, vec![0, 2, 4, 6, 8]);
        assert_eq!(tab.terminal_menu_active_row, Some(8));
    }

    #[test]
    fn detects_codex_permissions_menu_with_long_descriptions() {
        let mut tab = TabState::new_for_test();
        tab.terminal_mode = TerminalMode::InteractiveAi;
        tab.terminal_lines = vec![
            line(
                "1. Read Only                     Codex can read files in the current workspace. Approval is required to edit files or access the internet.",
            ),
            accented_line(
                "2. Default (non-admin sandbox)  Codex can read and edit files in the current workspace, and run commands. Approval is required to access the internet or edit other files.",
            ),
            line(
                "3. Auto-review (current)        Same workspace-write permissions as Default, but eligible `on-request` approvals are routed through the auto-reviewer subagent.",
            ),
            line(
                "4. Full Access                  Codex can edit files outside this workspace and access the internet without asking for approval. Exercise caution when using.",
            ),
        ];
        refresh_terminal_menu_state(&mut tab, 0, 3);
        assert_eq!(tab.terminal_menu_rows, vec![0, 1, 2, 3]);
        assert_eq!(tab.terminal_menu_active_row, Some(1));
    }

    #[test]
    fn detects_highlighted_single_column_provider_menu() {
        let mut tab = TabState::new_for_test();
        tab.terminal_mode = TerminalMode::InteractiveAi;
        tab.terminal_lines = vec![
            accented_line("Popular"),
            highlighted_line("OpenCode Zen (Recommended)"),
            line("OpenCode Go  Low cost subscription for everyone"),
            line("OpenAI (ChatGPT Plus/Pro or API key)"),
            line("GitHub Copilot"),
            line("Anthropic (API key)"),
            line("Google"),
        ];
        refresh_terminal_menu_state(&mut tab, 0, 6);
        assert_eq!(tab.terminal_menu_rows, vec![1, 2, 3, 4, 5, 6]);
        assert_eq!(tab.terminal_menu_active_row, Some(1));
    }

    #[test]
    fn detects_framed_command_menu() {
        let mut tab = TabState::new_for_test();
        tab.terminal_mode = TerminalMode::InteractiveAi;
        tab.terminal_lines = vec![
            highlighted_line("│ /agents    Switch agent │"),
            line("│ /connect   Connect provider │"),
            line("│ /editor    Open editor │"),
        ];
        refresh_terminal_menu_state(&mut tab, 0, 2);
        assert_eq!(tab.terminal_menu_rows, vec![0, 1, 2]);
        assert_eq!(tab.terminal_menu_active_row, Some(0));
    }

    #[test]
    fn detects_fg_accented_command_menu_active_row() {
        let mut tab = TabState::new_for_test();
        tab.terminal_mode = TerminalMode::InteractiveAi;
        tab.terminal_lines = vec![
            line("/fast                    toggle Fast mode"),
            accented_line("/permissions             choose what Codex is allowed to do"),
            line("/keymap                  remap TUI shortcuts"),
        ];
        refresh_terminal_menu_state(&mut tab, 0, 2);
        assert_eq!(tab.terminal_menu_rows, vec![0, 1, 2]);
        assert_eq!(tab.terminal_menu_active_row, Some(1));
    }

    #[test]
    fn does_not_detect_unhighlighted_prompt_footer_as_menu() {
        let mut tab = TabState::new_for_test();
        tab.terminal_mode = TerminalMode::InteractiveAi;
        tab.terminal_cursor_row = Some(1);
        tab.terminal_lines = vec![
            line("Tip: Try /model to change model"),
            line("> /skills"),
            line("Use Enter to submit"),
        ];
        refresh_terminal_menu_state(&mut tab, 0, 2);
        assert!(tab.terminal_menu_rows.is_empty());
        assert_eq!(tab.terminal_menu_active_row, None);
    }

    #[test]
    fn does_not_detect_codex_status_block_as_command_menu() {
        let mut tab = TabState::new_for_test();
        tab.terminal_mode = TerminalMode::InteractiveAi;
        tab.terminal_lines = vec![
            line("model      gpt-5.4 xhigh /model to change"),
            line("cwd        D:\\Projects\\CliGJ"),
            line("approval   never"),
        ];
        refresh_terminal_menu_state(&mut tab, 0, 2);
        assert!(tab.terminal_menu_rows.is_empty());
        assert_eq!(tab.terminal_menu_active_row, None);
    }

    #[test]
    fn slash_command_menu_ignores_non_slash_helper_rows() {
        let mut tab = TabState::new_for_test();
        tab.terminal_mode = TerminalMode::InteractiveAi;
        tab.terminal_lines = vec![
            line("help       Use arrows to pick a command"),
            highlighted_line("/logout    log out of Codex"),
            line("/exit      exit Codex"),
            line("/feedback  send logs to maintainers"),
        ];
        refresh_terminal_menu_state(&mut tab, 0, 3);
        assert_eq!(tab.terminal_menu_rows, vec![1, 2, 3]);
        assert_eq!(tab.terminal_menu_active_row, Some(1));
    }

    #[test]
    fn arrow_block_parser_ignores_non_boundary_indent_on_unicode_rows() {
        assert_eq!(parse_arrow_block_line("▄▄▄▄▄▄", 1), None);
    }
}
