use std::borrow::Cow;
use std::io::Write;
use std::sync::Arc;

use wezterm_term::color::ColorPalette;
use wezterm_term::config::TerminalConfiguration;
use wezterm_term::{Line, Terminal, TerminalSize};

use crate::ansi::bytes_include_clear_screen_sequence_for_rows;
use crate::types::{RawPtyEvent, RawPtyMode};

#[derive(Debug)]
struct ReplayTermConfig;

impl TerminalConfiguration for ReplayTermConfig {
    fn color_palette(&self) -> ColorPalette {
        ColorPalette::default()
    }
}

#[derive(Debug, Clone)]
pub struct ReplaySnapshot {
    pub final_mode: RawPtyMode,
    pub cols: usize,
    pub rows: usize,
    pub total_rows: usize,
    pub visible_start: usize,
    pub tail_2x_start: usize,
    pub render_start: usize,
    pub full_text: String,
    pub visible_text: String,
    pub tail_2x_text: String,
    pub active_viewport_text: String,
}

fn lines_plain_text(lines: &[Line]) -> String {
    let mut out = String::new();
    for line in lines {
        let text: Cow<'_, str> = line.as_str();
        out.push_str(text.as_ref());
        out.push('\n');
    }
    out
}

pub fn replay_raw_pty_events(events: &[RawPtyEvent]) -> Result<ReplaySnapshot, String> {
    const REPLAY_SNAPSHOT_MAX_LINES: usize = 240;

    let mut rows = 40usize;
    let mut cols = 120usize;
    let mut final_mode = RawPtyMode::Shell;
    let mut render_mode = RawPtyMode::Shell;
    let mut interactive_snapshot_floor = 0usize;

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    enum InteractiveFloorReset {
        ModeStart,
        Viewport,
    }
    let mut pending_interactive_floor_reset: Option<InteractiveFloorReset> = None;

    let config: Arc<dyn TerminalConfiguration + Send + Sync> = Arc::new(ReplayTermConfig);
    let writer: Box<dyn Write + Send> = Box::new(std::io::sink());
    let mut term = Terminal::new(
        TerminalSize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
            dpi: 0,
        },
        Arc::clone(&config),
        "CliGJ-Replay",
        "0",
        writer,
    );
    term.enable_conpty_quirks();

    for event in events {
        match event {
            RawPtyEvent::Bytes(bytes) => {
                term.advance_bytes(bytes);
                if render_mode == RawPtyMode::InteractiveAi
                    && bytes_include_clear_screen_sequence_for_rows(bytes, rows)
                    && pending_interactive_floor_reset != Some(InteractiveFloorReset::ModeStart)
                {
                    pending_interactive_floor_reset = Some(InteractiveFloorReset::Viewport);
                }
            }
            RawPtyEvent::Resize {
                cols: new_cols,
                rows: new_rows,
            } => {
                cols = (*new_cols).max(1) as usize;
                rows = (*new_rows).max(1) as usize;
                term.resize(TerminalSize {
                    rows,
                    cols,
                    pixel_width: 0,
                    pixel_height: 0,
                    dpi: 0,
                });
                if render_mode == RawPtyMode::InteractiveAi {
                    pending_interactive_floor_reset = Some(InteractiveFloorReset::Viewport);
                }
            }
            RawPtyEvent::RenderMode { mode } => {
                render_mode = *mode;
                final_mode = *mode;
                if render_mode == RawPtyMode::InteractiveAi {
                    pending_interactive_floor_reset = Some(InteractiveFloorReset::ModeStart);
                }
            }
        }

        if let Some(reset) = pending_interactive_floor_reset.take() {
            if render_mode == RawPtyMode::InteractiveAi {
                let screen = term.screen();
                let total = screen.scrollback_rows();
                interactive_snapshot_floor = match reset {
                    InteractiveFloorReset::ModeStart => {
                        let cursor = term.cursor_pos();
                        screen.phys_row(cursor.y)
                    }
                    InteractiveFloorReset::Viewport => total.saturating_sub(rows),
                };
            }
        }
    }

    let screen = term.screen();
    let total_rows = screen.scrollback_rows();
    let visible_start = total_rows.saturating_sub(rows);
    let tail_2x_start = total_rows.saturating_sub(rows.saturating_mul(2));
    let render_cap = rows
        .saturating_mul(4)
        .min(REPLAY_SNAPSHOT_MAX_LINES)
        .max(rows);
    let render_start = match final_mode {
        RawPtyMode::InteractiveAi => total_rows
            .saturating_sub(render_cap)
            .max(interactive_snapshot_floor.min(total_rows)),
        RawPtyMode::Shell => visible_start,
    };
    let full_lines = screen.lines_in_phys_range(0..total_rows);
    let visible_lines = screen.lines_in_phys_range(visible_start..total_rows);
    let tail_2x_lines = screen.lines_in_phys_range(tail_2x_start..total_rows);
    let render_lines = screen.lines_in_phys_range(render_start..total_rows);

    Ok(ReplaySnapshot {
        final_mode,
        cols,
        rows,
        total_rows,
        visible_start,
        tail_2x_start,
        render_start,
        full_text: lines_plain_text(&full_lines),
        visible_text: lines_plain_text(&render_lines),
        tail_2x_text: lines_plain_text(&tail_2x_lines),
        active_viewport_text: lines_plain_text(&visible_lines),
    })
}

#[cfg(test)]
mod tests {
    use super::replay_raw_pty_events;
    use crate::types::{RawPtyEvent, RawPtyMode};

    #[test]
    fn interactive_clear_resets_render_floor_to_viewport() {
        let events = vec![
            RawPtyEvent::RenderMode {
                mode: RawPtyMode::InteractiveAi,
            },
            RawPtyEvent::Resize { cols: 20, rows: 6 },
            RawPtyEvent::Bytes(b"one\r\ntwo\r\nthree\r\nfour\r\nfive\r\nsix\r\n".to_vec()),
            RawPtyEvent::Bytes(
                b"\x1b[H\x1b[K\r\n\x1b[K\r\n\x1b[K\r\n\x1b[K\r\n\x1b[K\r\n\x1b[K\r\n".to_vec(),
            ),
            RawPtyEvent::Bytes(b"\x1b[Hafter clear\r\nprompt".to_vec()),
        ];

        let snapshot = replay_raw_pty_events(&events).expect("replay should succeed");
        assert!(!snapshot.visible_text.contains("one"));
        assert!(snapshot.visible_text.contains("after clear"));
        assert!(snapshot.active_viewport_text.contains("after clear"));
    }
}
