use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::io::{Write, Read};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};
use std::sync::mpsc::{channel, Receiver, RecvTimeoutError};

use wezterm_term::config::TerminalConfiguration;
use wezterm_term::{Line, Terminal, TerminalSize};
use wezterm_term::color::ColorPalette;

use crate::terminal::types::{
    ControlCommand, ReaderRenderMode, RawPtyEvent, TerminalRender,
};
use crate::terminal::render::{line_to_colored_spans, ColoredLine};
use crate::terminal::pty::{PtyPair, PtyProcess, PtyReader, PtyWriter};

const CONPTY_SNAPSHOT_MAX_LINES: usize = 240;
const CONPTY_RESIZE_SETTLE_MS: u64 = 120;

#[derive(Debug)]
struct SessionTermConfig;

impl TerminalConfiguration for SessionTermConfig {
    fn color_palette(&self) -> ColorPalette {
        ColorPalette::default()
    }
}

pub fn start_terminal_session(
    pty: PtyPair,
    initial_render_mode: ReaderRenderMode,
    on_chunk: impl FnMut(TerminalRender) + Send + 'static,
) -> (
    thread::JoinHandle<()>,
    std::sync::mpsc::Sender<ControlCommand>,
    Box<dyn PtyProcess>,
    Box<dyn PtyWriter>,
) {
    let (control_tx, control_rx) = channel::<ControlCommand>();
    let process = pty.process;
    let reader = pty.reader;
    let writer = pty.writer;

    // Use Arc<dyn PtyProcess> so both the session loop and GuiState can hold it.
    let process_arc: Arc<dyn PtyProcess> = process.into();

    let process_for_loop = Arc::clone(&process_arc);
    let handle = thread::spawn(move || {
        run_session_loop(reader, process_for_loop, control_rx, initial_render_mode, on_chunk);
    });
    
    // We return a proxy for PtyProcess that uses the Arc.
    struct PtyProcessProxy(Arc<dyn PtyProcess>);
    impl PtyProcess for PtyProcessProxy {
        fn resize(&self, cols: u16, rows: u16) -> Result<(), String> {
            self.0.resize(cols, rows)
        }
    }

    (handle, control_tx, Box::new(PtyProcessProxy(process_arc)), writer)
}

enum InternalEvent {
    Bytes(Vec<u8>),
    Control(ControlCommand),
}

fn run_session_loop(
    mut reader: Box<dyn PtyReader>,
    process: Arc<dyn PtyProcess>,
    control_rx: Receiver<ControlCommand>,
    initial_render_mode: ReaderRenderMode,
    mut on_chunk: impl FnMut(TerminalRender) + Send + 'static,
) {

    let (event_tx, event_rx) = channel::<InternalEvent>();

    // 1. Byte reading thread
    let event_tx_bytes = event_tx.clone();
    thread::spawn(move || {
        let mut buf = [0u8; 65536];
        loop {
            match reader.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    if event_tx_bytes.send(InternalEvent::Bytes(buf[..n].to_vec())).is_err() {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
    });

    // 2. Control command proxy thread
    let event_tx_control = event_tx.clone();
    thread::spawn(move || {
        while let Ok(cmd) = control_rx.recv() {
            if event_tx_control.send(InternalEvent::Control(cmd)).is_err() {
                break;
            }
        }
    });

    // 3. Main emulation loop
    let config: Arc<dyn TerminalConfiguration> = Arc::new(SessionTermConfig);
    let mut term_rows = 40usize;
    let mut term_cols = 120usize;
    let term_size = TerminalSize {
        rows: term_rows,
        cols: term_cols,
        pixel_width: 0,
        pixel_height: 0,
        dpi: 0,
    };
    let writer: Box<dyn Write + Send> = Box::new(std::io::sink());
    let palette = config.color_palette();
    let mut term = Terminal::new(term_size, config, "CliGJ", "0", writer);
    term.enable_conpty_quirks();

    let mut last_snapshot_fp: Option<u64> = None;
    let mut line_cache: Vec<(u64, ColoredLine)> = Vec::new();
    let mut pending_reset = false;
    let mut render_mode = initial_render_mode;
    let mut last_alt_screen_active = false;
    let mut interactive_snapshot_floor = 0usize;
    
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    enum InteractiveFloorReset { ModeStart, Viewport }
    let mut pending_interactive_floor_reset =
        (initial_render_mode == ReaderRenderMode::InteractiveAi)
            .then_some(InteractiveFloorReset::ModeStart);

    let mut resize_settle_deadline: Option<Instant> = None;
    let mut pending_raw_pty_events = vec![
        RawPtyEvent::RenderMode { mode: initial_render_mode.into() },
        RawPtyEvent::Resize { cols: term_cols as u16, rows: term_rows as u16 },
    ];

    loop {
        let event = match resize_settle_deadline {
            Some(deadline) => {
                let now = Instant::now();
                if deadline > now {
                    match event_rx.recv_timeout(deadline.duration_since(now)) {
                        Ok(event) => Some(event),
                        Err(RecvTimeoutError::Timeout) => None,
                        Err(RecvTimeoutError::Disconnected) => break,
                    }
                } else {
                    None
                }
            }
            None => match event_rx.recv() {
                Ok(event) => Some(event),
                Err(_) => break,
            },
        };

        if let Some(event) = event {
            match event {
                InternalEvent::Bytes(bytes) => {
                    term.advance_bytes(&bytes);
                    pending_raw_pty_events.push(RawPtyEvent::Bytes(bytes));
                    if resize_settle_deadline.is_some() {
                        resize_settle_deadline = Some(
                            Instant::now() + Duration::from_millis(CONPTY_RESIZE_SETTLE_MS),
                        );
                    }
                }
                InternalEvent::Control(ControlCommand::Resize { cols, rows }) => {
                    let _ = process.resize(cols, rows);
                    pending_raw_pty_events.push(RawPtyEvent::Resize { cols, rows });
                    let new_cols = cols as usize;
                    let new_rows = rows as usize;
                    let size_changed = term_cols != new_cols || term_rows != new_rows;
                    term_cols = new_cols;
                    term_rows = new_rows;
                    term.resize(TerminalSize {
                        rows: term_rows,
                        cols: term_cols,
                        pixel_width: 0,
                        pixel_height: 0,
                        dpi: 0,
                    });
                    line_cache.clear();
                    last_snapshot_fp = None;
                    if size_changed {
                        pending_reset = true;
                        if render_mode == ReaderRenderMode::InteractiveAi
                            && pending_interactive_floor_reset != Some(InteractiveFloorReset::ModeStart)
                        {
                            pending_interactive_floor_reset = Some(InteractiveFloorReset::Viewport);
                        }
                        resize_settle_deadline = Some(Instant::now() + Duration::from_millis(CONPTY_RESIZE_SETTLE_MS));
                    }
                }
                InternalEvent::Control(ControlCommand::SetRenderMode(new_mode)) => {
                    pending_raw_pty_events.push(RawPtyEvent::RenderMode { mode: new_mode.into() });
                    if render_mode != new_mode {
                        render_mode = new_mode;
                        line_cache.clear();
                        last_snapshot_fp = None;
                        pending_reset = true;
                        pending_interactive_floor_reset = (render_mode == ReaderRenderMode::InteractiveAi)
                            .then_some(InteractiveFloorReset::ModeStart);
                    }
                }
            }
        } else {
            resize_settle_deadline = None;
        }

        if resize_settle_deadline.is_some() {
            continue;
        }

        let alt_screen_active = term.is_alt_screen_active();
        if alt_screen_active != last_alt_screen_active {
            line_cache.clear();
            last_snapshot_fp = None;
            pending_reset = true;
            last_alt_screen_active = alt_screen_active;
        }

        let screen = term.screen();
        let total = screen.scrollback_rows();
        let snapshot_cap = term_rows.saturating_mul(4).min(CONPTY_SNAPSHOT_MAX_LINES).max(term_rows);

        if let Some(reset) = pending_interactive_floor_reset.take() {
            if render_mode == ReaderRenderMode::InteractiveAi {
                interactive_snapshot_floor = match reset {
                    InteractiveFloorReset::ModeStart => {
                        let cursor = term.cursor_pos();
                        screen.phys_row(cursor.y)
                    }
                    InteractiveFloorReset::Viewport => total.saturating_sub(term_rows),
                };
            }
        }

        let (start, end, total_for_render, filled) = match render_mode {
            ReaderRenderMode::InteractiveAi => {
                let snapshot_row_count = snapshot_cap.min(total.max(1));
                let floor = interactive_snapshot_floor.min(total);
                let start = total.saturating_sub(snapshot_row_count).max(floor);
                (start, total, total, total > term_rows)
            }
            ReaderRenderMode::Shell => {
                let snapshot_row_count = snapshot_cap.min(total.max(1));
                let start = total.saturating_sub(snapshot_row_count);
                (start, total, total, total > term_rows)
            }
        };

        let lines = screen.lines_in_phys_range(start..end);
        let line_refs: Vec<&Line> = lines.iter().collect();
        let cursor = term.cursor_pos();
        let cursor_phys_row = screen.phys_row(cursor.y);
        let cursor_local_row = cursor_phys_row.checked_sub(start).filter(|row| *row < line_refs.len());
        let cursor_col = Some(cursor.x);

        let fp = snapshot_content_fingerprint(total_for_render, &line_refs, &palette, cursor_local_row, cursor_col);
        if last_snapshot_fp == Some(fp) && pending_raw_pty_events.is_empty() {
            continue;
        }
        last_snapshot_fp = Some(fp);

        let mut render = match render_mode {
            ReaderRenderMode::Shell => terminal_render_from_lines_cached(
                ReaderRenderMode::Shell,
                &line_refs,
                start,
                total_for_render,
                term_rows,
                &palette,
                cursor_local_row,
                cursor_col,
                &mut line_cache,
            ),
            ReaderRenderMode::InteractiveAi => terminal_render_from_lines_full(
                ReaderRenderMode::InteractiveAi,
                &line_refs,
                start,
                total_for_render,
                term_rows,
                &palette,
                cursor_local_row,
                cursor_col,
            ),
        };
        render.raw_pty_events = std::mem::take(&mut pending_raw_pty_events);
        render.filled = filled;
        render.reset_terminal_buffer = pending_reset;
        pending_reset = false;
        on_chunk(render);
    }
}

fn snapshot_content_fingerprint(
    total_rows: usize,
    lines: &[&Line],
    palette: &ColorPalette,
    cursor_local_row: Option<usize>,
    cursor_col: Option<usize>,
) -> u64 {
    let mut h = DefaultHasher::new();
    total_rows.hash(&mut h);
    lines.len().hash(&mut h);
    cursor_local_row.hash(&mut h);
    cursor_col.hash(&mut h);
    for (i, line) in lines.iter().enumerate() {
        let active_cursor_col = if cursor_local_row == Some(i) { cursor_col } else { None };
        let built = line_to_colored_spans(line, palette, active_cursor_col);
        built.blank.hash(&mut h);
        built.spans.len().hash(&mut h);
        for span in &built.spans {
            span.text.hash(&mut h);
            span.fg.hash(&mut h);
            span.bg.hash(&mut h);
        }
    }
    h.finish()
}

fn colored_line_fingerprint(line: &ColoredLine, cursor_col: Option<usize>) -> u64 {
    let mut h = DefaultHasher::new();
    line.blank.hash(&mut h);
    line.spans.len().hash(&mut h);
    for span in &line.spans {
        span.text.hash(&mut h);
        span.fg.hash(&mut h);
        span.bg.hash(&mut h);
    }
    cursor_col.hash(&mut h);
    h.finish()
}

fn terminal_render_from_lines_cached(
    render_mode: ReaderRenderMode,
    lines: &[&Line],
    start_phys_idx: usize,
    total_scrollback_rows: usize,
    term_screen_rows: usize,
    palette: &ColorPalette,
    cursor_local_row: Option<usize>,
    cursor_col: Option<usize>,
    cache: &mut Vec<(u64, ColoredLine)>,
) -> TerminalRender {
    let mut changed_indices = Vec::new();
    let num_lines = lines.len();
    let cache_base_idx = start_phys_idx;

    if cache.len() < cache_base_idx + num_lines {
        cache.resize(cache_base_idx + num_lines, (0, ColoredLine::default()));
    }

    for i in 0..num_lines {
        let global_idx = cache_base_idx + i;
        let active_cursor_col = if cursor_local_row == Some(i) { cursor_col } else { None };
        let built = line_to_colored_spans(lines[i], palette, None);
        let fp = colored_line_fingerprint(&built, active_cursor_col);
        if cache[global_idx].0 != fp {
            cache[global_idx] = (fp, built);
            changed_indices.push(i);
        }
    }

    let changed_lines: Vec<ColoredLine> = changed_indices.iter().map(|&i| cache[cache_base_idx + i].1.clone()).collect();

    TerminalRender {
        render_mode,
        raw_pty_events: Vec::new(),
        text: String::new(),
        lines: changed_lines,
        snapshot_len: num_lines,
        full_len: total_scrollback_rows,
        first_line_idx: start_phys_idx,
        cursor_row: cursor_local_row.map(|row| start_phys_idx + row),
        cursor_col,
        filled: num_lines > term_screen_rows,
        changed_indices,
        reset_terminal_buffer: false,
    }
}

fn terminal_render_from_lines_full(
    render_mode: ReaderRenderMode,
    lines: &[&Line],
    start_phys_idx: usize,
    total_scrollback_rows: usize,
    term_screen_rows: usize,
    palette: &ColorPalette,
    cursor_local_row: Option<usize>,
    cursor_col: Option<usize>,
) -> TerminalRender {
    let full_lines: Vec<ColoredLine> = lines.iter().map(|line| line_to_colored_spans(line, palette, None)).collect();
    let render_window_len = full_lines.len();

    TerminalRender {
        render_mode,
        raw_pty_events: Vec::new(),
        text: String::new(),
        lines: full_lines,
        snapshot_len: render_window_len,
        full_len: total_scrollback_rows,
        first_line_idx: start_phys_idx,
        cursor_row: cursor_local_row.map(|row| start_phys_idx + row),
        cursor_col,
        filled: render_window_len > term_screen_rows,
        changed_indices: Vec::new(),
        reset_terminal_buffer: false,
    }
}
