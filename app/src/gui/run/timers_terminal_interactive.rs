use crate::gui::state::TabState;
use cligj_terminal::render::ColoredLine;

use super::timers_terminal::{
    append_interactive_history_block, archive_dropped_interactive_prefix, compose_interactive_terminal_lines,
    line_has_visible_text, maybe_archive_repainted_frame_before_replace, reset_terminal_model_cache,
    snapshot_starts_mid_interactive_frame, trim_or_drop_shell_preamble_snapshot,
};

const INTERACTIVE_TRAILING_BLANK_KEEP: usize = 1;

pub(super) fn apply_interactive_replace(
    tab: &mut TabState,
    changed_indices: &[usize],
    reset_terminal_buffer: bool,
    cursor_row: Option<usize>,
    cursor_col: Option<usize>,
    chunk_first_idx: usize,
    phys_end: usize,
    mut new_lines: Vec<ColoredLine>,
) {
    if changed_indices.is_empty() && !new_lines.is_empty() {
        let previous_terminal_lines = Some(tab.terminal_lines.clone());
        let previous_cursor_row = tab.terminal_cursor_row;
        let previous_cursor_col = tab.terminal_cursor_col;
        let previous_origin = tab.terminal_physical_origin;
        let mut snapshot_lines = std::mem::take(&mut new_lines);
        let drop_shell_preamble_snapshot =
            trim_or_drop_shell_preamble_snapshot(&mut snapshot_lines, &tab.interactive_markers);
        let frame_end = snapshot_lines
            .iter()
            .rposition(line_has_visible_text)
            .map(|idx| (idx + 1 + INTERACTIVE_TRAILING_BLANK_KEEP).min(snapshot_lines.len()))
            .unwrap_or(0);
        snapshot_lines.truncate(frame_end);
        let restore_previous_resize_frame = reset_terminal_buffer
            && snapshot_starts_mid_interactive_frame(&snapshot_lines, &tab.interactive_markers)
            && previous_terminal_lines
                .as_ref()
                .is_some_and(|previous| !previous.is_empty());

        if drop_shell_preamble_snapshot {
            tab.interactive_frame_lines.clear();
            tab.terminal_lines.clear();
            reset_terminal_model_cache(tab);
        } else if restore_previous_resize_frame {
            if let Some(previous_terminal_lines) = previous_terminal_lines {
                tab.terminal_lines = previous_terminal_lines;
            }
            tab.terminal_cursor_row = previous_cursor_row;
            tab.terminal_cursor_col = previous_cursor_col;
            tab.terminal_physical_origin = previous_origin;
            reset_terminal_model_cache(tab);
        } else if snapshot_lines.is_empty() {
            if let Some(previous_terminal_lines) = previous_terminal_lines {
                if !previous_terminal_lines.is_empty() {
                    tab.terminal_lines = previous_terminal_lines;
                }
            }
        } else {
            if reset_terminal_buffer {
                if !tab.interactive_archive_repainted_frames {
                    tab.interactive_history_lines.clear();
                    tab.interactive_last_archived_signature.clear();
                }
                tab.interactive_frame_lines.clear();
                tab.terminal_lines.clear();
            } else {
                maybe_archive_repainted_frame_before_replace(tab, &snapshot_lines);
                archive_dropped_interactive_prefix(tab, chunk_first_idx);
            }
            tab.interactive_frame_lines = snapshot_lines;
            compose_interactive_terminal_lines(tab);
        }

        if !restore_previous_resize_frame {
            tab.terminal_physical_origin = chunk_first_idx;
            tab.terminal_cursor_row = cursor_row
                .and_then(|phys| phys.checked_sub(chunk_first_idx))
                .map(|row| tab.interactive_history_lines.len() + row);
            tab.terminal_cursor_col = cursor_col;
            if let Some(row) = tab.terminal_cursor_row {
                if row >= tab.terminal_lines.len() {
                    tab.terminal_cursor_row = None;
                }
            }
        }
        reset_terminal_model_cache(tab);
        tab.terminal_text.clear();
        return;
    }

    let previous_terminal_lines = if reset_terminal_buffer {
        Some(tab.terminal_lines.clone())
    } else {
        None
    };
    if reset_terminal_buffer {
        if !tab.interactive_archive_repainted_frames {
            tab.interactive_history_lines.clear();
            tab.interactive_last_archived_signature.clear();
        }
        tab.interactive_frame_lines.clear();
        tab.terminal_lines.clear();
        tab.terminal_physical_origin = chunk_first_idx;
        tab.terminal_cursor_row = None;
        tab.terminal_cursor_col = None;
        reset_terminal_model_cache(tab);
    }

    if chunk_first_idx < tab.terminal_physical_origin {
        let prepend = tab.terminal_physical_origin - chunk_first_idx;
        let mut rebased = vec![ColoredLine::default(); prepend];
        rebased.append(&mut tab.interactive_frame_lines);
        tab.interactive_frame_lines = rebased;
        tab.terminal_physical_origin = chunk_first_idx;
        reset_terminal_model_cache(tab);
    }

    let origin = tab.terminal_physical_origin;
    let leading = chunk_first_idx.saturating_sub(origin);
    if leading > 0 {
        let drop_leading = leading.min(tab.interactive_frame_lines.len());
        if drop_leading > 0 && !reset_terminal_buffer {
            let dropped: Vec<ColoredLine> = tab
                .interactive_frame_lines
                .iter()
                .take(drop_leading)
                .cloned()
                .collect();
            append_interactive_history_block(tab, &dropped);
        }
        tab.interactive_frame_lines.drain(0..drop_leading);
        tab.terminal_physical_origin = tab.terminal_physical_origin.saturating_add(drop_leading);
        if leading > drop_leading {
            tab.terminal_physical_origin = chunk_first_idx;
        }
    }

    let origin = tab.terminal_physical_origin;
    let local_len = phys_end.saturating_sub(origin);
    if local_len > tab.interactive_frame_lines.len() {
        tab.interactive_frame_lines
            .resize(local_len, ColoredLine::default());
    }

    if changed_indices.is_empty() && !new_lines.is_empty() {
        for i in 0..new_lines.len() {
            let phys = chunk_first_idx + i;
            let Some(local) = phys.checked_sub(origin) else {
                continue;
            };
            if local < tab.interactive_frame_lines.len() {
                tab.interactive_frame_lines[local] = std::mem::take(&mut new_lines[i]);
            }
        }
    } else {
        for (delta_idx, &snapshot_idx) in changed_indices.iter().enumerate() {
            let phys = chunk_first_idx + snapshot_idx;
            let Some(local) = phys.checked_sub(origin) else {
                continue;
            };
            if local < tab.interactive_frame_lines.len() && delta_idx < new_lines.len() {
                tab.interactive_frame_lines[local] = std::mem::take(&mut new_lines[delta_idx]);
            }
        }
    }

    let tail_keep = phys_end.saturating_sub(origin);
    if tab.interactive_frame_lines.len() > tail_keep {
        tab.interactive_frame_lines.truncate(tail_keep);
    }

    let history_len = tab.interactive_history_lines.len();
    tab.terminal_cursor_row = cursor_row
        .and_then(|phys| phys.checked_sub(tab.terminal_physical_origin))
        .map(|row| history_len + row);
    tab.terminal_cursor_col = cursor_col;
    compose_interactive_terminal_lines(tab);
    if tab.terminal_lines.is_empty() {
        if let Some(previous_terminal_lines) = previous_terminal_lines {
            tab.terminal_lines = previous_terminal_lines;
            reset_terminal_model_cache(tab);
        }
    }
    if let Some(row) = tab.terminal_cursor_row {
        if row >= tab.terminal_lines.len() {
            tab.terminal_cursor_row = None;
        }
    }
    tab.terminal_text.clear();
}
