use crate::gui::state::{TabState, TerminalMode};
use cligj_terminal::render::ColoredLine;

use super::timers_terminal::{
    compact_terminal_lines_after_snapshot, line_has_visible_text,
};

const INTERACTIVE_TRAILING_BLANK_KEEP: usize = 1;

pub(super) fn apply_shell_replace(
    tab: &mut TabState,
    changed_indices: &[usize],
    reset_terminal_buffer: bool,
    cursor_row: Option<usize>,
    cursor_col: Option<usize>,
    chunk_first_idx: usize,
    phys_end: usize,
    mut new_lines: Vec<ColoredLine>,
) {
    if reset_terminal_buffer {
        tab.terminal_lines.clear();
        tab.terminal_physical_origin = chunk_first_idx;
        tab.terminal_cursor_row = None;
        tab.terminal_cursor_col = None;
        tab.terminal_model_rows.clear();
        tab.terminal_model_hashes.clear();
        tab.terminal_model_dirty.clear();
        tab.last_window_first = usize::MAX;
        tab.last_window_last = usize::MAX;
        tab.last_window_total = usize::MAX;
    }

    if chunk_first_idx < tab.terminal_physical_origin {
        let prepend = tab.terminal_physical_origin - chunk_first_idx;
        let mut rebased = vec![ColoredLine::default(); prepend];
        rebased.append(&mut tab.terminal_lines);
        tab.terminal_lines = rebased;
        tab.terminal_physical_origin = chunk_first_idx;
        tab.terminal_model_rows.clear();
        tab.terminal_model_hashes.clear();
        tab.terminal_model_dirty.clear();
        tab.last_window_first = usize::MAX;
        tab.last_window_last = usize::MAX;
        tab.last_window_total = usize::MAX;
    }

    let origin = tab.terminal_physical_origin;
    let local_len = phys_end.saturating_sub(origin);
    if local_len > tab.terminal_lines.len() {
        tab.terminal_lines.resize(local_len, ColoredLine::default());
    }

    if changed_indices.is_empty() && !new_lines.is_empty() {
        for i in 0..new_lines.len() {
            let phys = chunk_first_idx + i;
            let Some(local) = phys.checked_sub(origin) else {
                continue;
            };
            if local < tab.terminal_lines.len() {
                tab.terminal_lines[local] = std::mem::take(&mut new_lines[i]);
                tab.terminal_model_rows.remove(&local);
                tab.terminal_model_hashes.remove(&local);
                tab.terminal_model_dirty.insert(local);
            }
        }
    } else {
        for (delta_idx, &snapshot_idx) in changed_indices.iter().enumerate() {
            let phys = chunk_first_idx + snapshot_idx;
            let Some(local) = phys.checked_sub(origin) else {
                continue;
            };
            if local < tab.terminal_lines.len() && delta_idx < new_lines.len() {
                tab.terminal_lines[local] = std::mem::take(&mut new_lines[delta_idx]);
                tab.terminal_model_rows.remove(&local);
                tab.terminal_model_hashes.remove(&local);
                tab.terminal_model_dirty.insert(local);
            }
        }
    }

    let tail_keep = phys_end.saturating_sub(origin);
    if tab.terminal_lines.len() > tail_keep {
        tab.terminal_lines.truncate(tail_keep);
        tab.terminal_model_rows.retain(|k, _| *k < tail_keep);
        tab.terminal_model_hashes.retain(|k, _| *k < tail_keep);
        tab.terminal_model_dirty.retain(|k| *k < tail_keep);
    }

    let leading = chunk_first_idx.saturating_sub(origin);
    let drop_snapshot_prefix = reset_terminal_buffer && tab.terminal_mode == TerminalMode::InteractiveAi;
    compact_terminal_lines_after_snapshot(tab, leading, drop_snapshot_prefix);

    tab.terminal_cursor_row = cursor_row
        .and_then(|phys| phys.checked_sub(tab.terminal_physical_origin));
    tab.terminal_cursor_col = cursor_col;

    let cursor_local_end = tab.terminal_cursor_row.map(|r| r + 1).unwrap_or(0);
    let interactive_tail_end = tab
        .terminal_lines
        .iter()
        .rposition(line_has_visible_text)
        .map(|idx| (idx + 1 + INTERACTIVE_TRAILING_BLANK_KEEP).min(tab.terminal_lines.len()))
        .unwrap_or(0);
    let trim_floor = if tab.terminal_mode == TerminalMode::InteractiveAi {
        interactive_tail_end
    } else {
        cursor_local_end
    };
    while tab.terminal_lines.len() > trim_floor {
        if tab
            .terminal_lines
            .last()
            .is_some_and(|line| !line_has_visible_text(line))
        {
            tab.terminal_lines.pop();
        } else {
            break;
        }
    }
    if let Some(row) = tab.terminal_cursor_row {
        if row >= tab.terminal_lines.len() {
            tab.terminal_cursor_row = None;
        }
    }

    tab.enforce_scrollback_cap();
    tab.terminal_text.clear();
}
