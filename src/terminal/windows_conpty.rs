use std::collections::hash_map::DefaultHasher;
use std::ffi::OsStr;
use std::hash::{Hash, Hasher};
use std::path::Path;
use std::io::Read;
use std::io::Write;
use std::os::windows::ffi::OsStrExt;
use std::os::windows::io::FromRawHandle;
use std::sync::mpsc::RecvTimeoutError;
use std::thread;
use std::time::{Duration, Instant};

use std::sync::Arc;

use wezterm_term::config::TerminalConfiguration;
use wezterm_term::Line;
use wezterm_term::Terminal;
use wezterm_term::TerminalSize;
use wezterm_term::color::ColorPalette;

use crate::terminal::pty_event::{RawPtyEvent, RawPtyMode};
use crate::terminal::render::{line_to_colored_spans, ColoredLine};

use windows::core::{PCWSTR, PWSTR};
use windows::Win32::Foundation::{CloseHandle, HANDLE};
use windows::Win32::System::Console::{
    ClosePseudoConsole, CreatePseudoConsole, ResizePseudoConsole, COORD, HPCON,
};
#[cfg(windows)]
use windows_sys::Win32::System::Console::{SetConsoleCP, SetConsoleOutputCP};
use windows::Win32::System::Pipes::CreatePipe;
use windows::Win32::System::Threading::{
    CreateProcessW, DeleteProcThreadAttributeList, InitializeProcThreadAttributeList,
    UpdateProcThreadAttribute, EXTENDED_STARTUPINFO_PRESENT, LPPROC_THREAD_ATTRIBUTE_LIST,
    PROCESS_INFORMATION, PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE, STARTUPINFOEXW,
};

#[derive(Debug)]
struct CliGjTermConfig;

impl TerminalConfiguration for CliGjTermConfig {
    fn color_palette(&self) -> ColorPalette {
        ColorPalette::default()
    }
}

pub struct ConptySession {
    pub writer: std::fs::File,
    pub _child_process: HANDLE,
    pub _child_thread: HANDLE,
    pub hpc: HPCON,
    pub _attr_list_ptr: *mut std::ffi::c_void,
    pub _attr_list_buf: Box<[u8]>,
}

impl ConptySession {
    pub fn resize(&self, cols: i16, rows: i16) -> Result<(), String> {
        unsafe {
            ResizePseudoConsole(self.hpc, COORD { X: cols, Y: rows })
                .map_err(|e| format!("ResizePseudoConsole: {e}"))
        }
    }
}

pub struct ConptySpawn {
    pub session: ConptySession,
    pub reader: std::fs::File,
}

pub struct TerminalRender {
    pub render_mode: ReaderRenderMode,
    pub raw_pty_events: Vec<RawPtyEvent>,
    pub text: String,
    /// ONLY lines that changed (matches changed_indices length).
    pub lines: Vec<ColoredLine>,
    /// Number of physical rows covered by this snapshot window.
    pub snapshot_len: usize,
    /// Total physical rows known to wezterm-term for this screen.
    pub full_len: usize,
    pub first_line_idx: usize,
    pub cursor_row: Option<usize>,
    pub cursor_col: Option<usize>,
    pub filled: bool,
    /// Indices of lines that changed since last render (for downstream diff).
    pub changed_indices: Vec<usize>,
    /// Next snapshot should replace the GUI buffer entirely (PTY geometry / reflow reset).
    pub reset_terminal_buffer: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReaderRenderMode {
    Shell,
    InteractiveAi,
}

impl From<ReaderRenderMode> for RawPtyMode {
    fn from(value: ReaderRenderMode) -> Self {
        match value {
            ReaderRenderMode::Shell => Self::Shell,
            ReaderRenderMode::InteractiveAi => Self::InteractiveAi,
        }
    }
}

pub fn spawn_conpty(shell: &str, cols: i16, rows: i16, current_dir: Option<&Path>) -> Result<ConptySpawn, String> {
    let (_, cmdline) = build_shell_command(shell);
    spawn_conpty_command_line(cmdline.as_str(), cols, rows, current_dir)
}

/// `current_dir`: initial working directory for the child process (shell). Must be an existing directory when set.
pub fn spawn_conpty_command_line(
    command_line: &str,
    cols: i16,
    rows: i16,
    current_dir: Option<&Path>,
) -> Result<ConptySpawn, String> {
    let cmdline = command_line.trim();
    if cmdline.is_empty() {
        return Err("empty startup command".to_string());
    }
    let cwd_wide: Option<Vec<u16>> = current_dir.and_then(|p| p.is_dir().then(|| to_wide_null(p)));
    unsafe {
        let mut in_read = HANDLE::default();
        let mut in_write = HANDLE::default();
        CreatePipe(&mut in_read, &mut in_write, None, 0)
            .map_err(|e| format!("CreatePipe(in): {e}"))?;

        let mut out_read = HANDLE::default();
        let mut out_write = HANDLE::default();
        CreatePipe(&mut out_read, &mut out_write, None, 0)
            .map_err(|e| format!("CreatePipe(out): {e}"))?;

        let hpc = CreatePseudoConsole(
            COORD { X: cols, Y: rows },
            in_read,
            out_write,
            0,
        )
        .map_err(|e| format!("CreatePseudoConsole: {e}"))?;

        let _ = CloseHandle(in_read);
        let _ = CloseHandle(out_write);

        let mut attr_size: usize = 0;
        let _ = InitializeProcThreadAttributeList(None, 1, Some(0), &mut attr_size);
        let mut attr_list_buf: Box<[u8]> = vec![0u8; attr_size].into_boxed_slice();
        let attr_list = attr_list_buf.as_mut_ptr() as *mut std::ffi::c_void;
        InitializeProcThreadAttributeList(
            Some(LPPROC_THREAD_ATTRIBUTE_LIST(attr_list as *mut _)),
            1,
            Some(0),
            &mut attr_size,
        )
        .map_err(|e| format!("InitializeProcThreadAttributeList: {e}"))?;

        UpdateProcThreadAttribute(
            LPPROC_THREAD_ATTRIBUTE_LIST(attr_list as *mut _),
            0,
            PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE as usize,
            Some(hpc.0 as *mut _),
            std::mem::size_of::<HPCON>(),
            None,
            None,
        )
        .map_err(|e| format!("UpdateProcThreadAttribute: {e}"))?;

        let mut siex: STARTUPINFOEXW = std::mem::zeroed();
        siex.StartupInfo.cb = std::mem::size_of::<STARTUPINFOEXW>() as u32;
        siex.lpAttributeList = LPPROC_THREAD_ATTRIBUTE_LIST(attr_list as *mut _);

        let mut cmdline_w = to_wide_null(&cmdline);

        let mut pi: PROCESS_INFORMATION = std::mem::zeroed();
        let cwd_pcw = cwd_wide
            .as_ref()
            .map(|w| PCWSTR(w.as_ptr()))
            .unwrap_or(PCWSTR::null());
        CreateProcessW(
            PCWSTR::null(),
            Some(PWSTR(cmdline_w.as_mut_ptr())),
            None,
            None,
            false,
            EXTENDED_STARTUPINFO_PRESENT,
            None,
            cwd_pcw,
            &siex.StartupInfo,
            &mut pi,
        )
        .map_err(|e| format!("CreateProcessW: {e}"))?;

        let mut writer = std::fs::File::from_raw_handle(in_write.0 as *mut _);
        let reader = std::fs::File::from_raw_handle(out_read.0 as *mut _);

        let _ = init_shell_utf8("", &mut writer);

        Ok(ConptySpawn {
            session: ConptySession {
                writer,
                _child_process: pi.hProcess,
                _child_thread: pi.hThread,
                hpc,
                _attr_list_ptr: attr_list,
                _attr_list_buf: attr_list_buf,
            },
            reader,
        })
    }
}

fn build_shell_command(shell: &str) -> (Vec<u16>, String) {
    if shell == "PowerShell" {
        let app = r"C:\Windows\System32\WindowsPowerShell\v1.0\powershell.exe";
        (to_wide_null(app), format!("\"{app}\" -NoLogo"))
    } else {
        let app = r"C:\Windows\System32\cmd.exe";
        (to_wide_null(app), format!("\"{app}\""))
    }
}

fn to_wide_null(s: impl AsRef<OsStr>) -> Vec<u16> {
    s.as_ref().encode_wide().chain(Some(0)).collect()
}

impl Drop for ConptySession {
    fn drop(&mut self) {
        unsafe {
            let _ = self.writer.flush();
            let _ = CloseHandle(self._child_thread);
            let _ = CloseHandle(self._child_process);
            ClosePseudoConsole(self.hpc);
            if !self._attr_list_ptr.is_null() {
                DeleteProcThreadAttributeList(LPPROC_THREAD_ATTRIBUTE_LIST(self._attr_list_ptr as *mut _));
            }
        }
    }
}

const CONPTY_SNAPSHOT_MAX_LINES: usize = 240;
const CONPTY_RESIZE_SETTLE_MS: u64 = 120;

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
        let active_cursor_col = if cursor_local_row == Some(i) {
            cursor_col
        } else {
            None
        };
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

    // 確保 cache 長度足夠
    if cache.len() < cache_base_idx + num_lines {
        cache.resize(cache_base_idx + num_lines, (0, ColoredLine::default()));
    }

    for i in 0..num_lines {
        let global_idx = cache_base_idx + i;
        let active_cursor_col = if cursor_local_row == Some(i) {
            cursor_col
        } else {
            None
        };
        let built = line_to_colored_spans(lines[i], palette, None);
        let fp = colored_line_fingerprint(&built, active_cursor_col);
        if cache[global_idx].0 != fp {
            cache[global_idx] = (fp, built);
            changed_indices.push(i);
        }
    }

    let changed_lines: Vec<ColoredLine> = changed_indices
        .iter()
        .map(|&i| cache[cache_base_idx + i].1.clone())
        .collect();

    let render_window_len = num_lines;
    let render_first_idx = start_phys_idx;

    TerminalRender {
        render_mode,
        raw_pty_events: Vec::new(),
        text: String::new(),
        lines: changed_lines,
        snapshot_len: render_window_len,
        full_len: total_scrollback_rows,
        first_line_idx: render_first_idx,
        cursor_row: cursor_local_row.map(|row| render_first_idx + row),
        cursor_col,
        filled: render_window_len > term_screen_rows,
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
    let full_lines: Vec<ColoredLine> = lines
        .iter()
        .map(|line| line_to_colored_spans(line, palette, None))
        .collect();
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
        // Empty changed_indices intentionally means "lines contains the full snapshot".
        changed_indices: Vec::new(),
        reset_terminal_buffer: false,
    }
}

#[derive(Debug)]
pub enum ControlCommand {
    Resize { cols: u16, rows: u16 },
    SetRenderMode(ReaderRenderMode),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum InteractiveFloorReset {
    ModeStart,
}

pub fn start_reader_thread(
    mut reader: std::fs::File,
    control_rx: std::sync::mpsc::Receiver<ControlCommand>,
    initial_render_mode: ReaderRenderMode,
    mut on_chunk: impl FnMut(TerminalRender) + Send + 'static,
) -> thread::JoinHandle<()> {
    enum Event {
        Bytes(Vec<u8>),
        Control(ControlCommand),
    }
    let (event_tx, event_rx) = std::sync::mpsc::channel::<Event>();

    // Byte reading thread
    let event_tx_bytes = event_tx.clone();
    thread::spawn(move || {
        let mut buf = [0u8; 65536];
        loop {
            match reader.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    if event_tx_bytes.send(Event::Bytes(buf[..n].to_vec())).is_err() {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
    });

    // Control command proxy thread (to unify with event_rx)
    let event_tx_control = event_tx.clone();
    thread::spawn(move || {
        while let Ok(cmd) = control_rx.recv() {
            if event_tx_control.send(Event::Control(cmd)).is_err() {
                break;
            }
        }
    });

    thread::spawn(move || {
        let config: Arc<dyn TerminalConfiguration> = Arc::new(CliGjTermConfig);
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
        let mut pending_interactive_floor_reset =
            (initial_render_mode == ReaderRenderMode::InteractiveAi)
                .then_some(InteractiveFloorReset::ModeStart);
        let mut resize_settle_deadline: Option<Instant> = None;
        let mut pending_raw_pty_events = vec![
            RawPtyEvent::RenderMode {
                mode: initial_render_mode.into(),
            },
            RawPtyEvent::Resize {
                cols: term_cols as u16,
                rows: term_rows as u16,
            },
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
                    Event::Bytes(bytes) => {
                        term.advance_bytes(&bytes);
                        pending_raw_pty_events.push(RawPtyEvent::Bytes(bytes));
                        if resize_settle_deadline.is_some() {
                            resize_settle_deadline = Some(
                                Instant::now()
                                    + Duration::from_millis(CONPTY_RESIZE_SETTLE_MS),
                            );
                        }
                    }
                    Event::Control(ControlCommand::Resize { cols, rows }) => {
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
                        // Reflow changes the whole screen; drop line fingerprints so we re-emit
                        // every row. Otherwise incremental diffs leave stale UI rows ("ghost" UI).
                        line_cache.clear();
                        last_snapshot_fp = None;
                        if size_changed {
                            pending_reset = true;
                            resize_settle_deadline =
                                Some(Instant::now() + Duration::from_millis(CONPTY_RESIZE_SETTLE_MS));
                        }
                    }
                    Event::Control(ControlCommand::SetRenderMode(new_mode)) => {
                        pending_raw_pty_events.push(RawPtyEvent::RenderMode {
                            mode: new_mode.into(),
                        });
                        if render_mode != new_mode {
                            render_mode = new_mode;
                            line_cache.clear();
                            last_snapshot_fp = None;
                            pending_reset = true;
                            pending_interactive_floor_reset =
                                (render_mode == ReaderRenderMode::InteractiveAi)
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

            // After processing events, render a snapshot of the recent screen/scrollback tail.
            let alt_screen_active = term.is_alt_screen_active();
            if alt_screen_active != last_alt_screen_active {
                line_cache.clear();
                last_snapshot_fp = None;
                pending_reset = true;
                last_alt_screen_active = alt_screen_active;
            }

            let screen = term.screen();
            let total = screen.scrollback_rows();
            let snapshot_cap = term_rows
                .saturating_mul(4)
                .min(CONPTY_SNAPSHOT_MAX_LINES)
                .max(term_rows);
            if let Some(reset) = pending_interactive_floor_reset.take() {
                if render_mode == ReaderRenderMode::InteractiveAi {
                    interactive_snapshot_floor = match reset {
                        InteractiveFloorReset::ModeStart => {
                            let cursor = term.cursor_pos();
                            screen.phys_row(cursor.y)
                        }
                    };
                }
            }
            let (start, end, total_for_render, filled) = match render_mode {
                ReaderRenderMode::InteractiveAi => {
                    // Interactive launchers repaint the viewport on resize. Keep GUI scrollback,
                    // but never pull in full-screen frames that predate the latest reset.
                    let snapshot_row_count = snapshot_cap.min(total.max(1));
                    let floor = interactive_snapshot_floor.min(total);
                    let start = total.saturating_sub(snapshot_row_count).max(floor);
                    let end = total;
                    let total_for_render = total;
                    let filled = total > term_rows;
                    (start, end, total_for_render, filled)
                }
                ReaderRenderMode::Shell => {
                    let snapshot_row_count = snapshot_cap.min(total.max(1));
                    let start = total.saturating_sub(snapshot_row_count);
                    let end = total;
                    let total_for_render = total;
                    let filled = total > term_rows;
                    (start, end, total_for_render, filled)
                }
            };
            let lines = screen.lines_in_phys_range(start..end);
            let line_refs: Vec<&Line> = lines.iter().collect();
            let cursor = term.cursor_pos();
            let cursor_phys_row = screen.phys_row(cursor.y);
            let cursor_local_row =
                cursor_phys_row.checked_sub(start).filter(|row| *row < line_refs.len());
            let cursor_col = Some(cursor.x);

            let fp = snapshot_content_fingerprint(
                total_for_render,
                &line_refs,
                &palette,
                cursor_local_row,
                cursor_col,
            );
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
    })
}

fn init_shell_utf8(_shell: &str, _writer: &mut std::fs::File) -> std::io::Result<()> {
    #[cfg(windows)]
    {
        // 核心邏輯：只調用 API，這絕對不會產生任何終端輸入或輸出
        // 也就是說，Shell 根本不知道我們改了編碼，它會保持原始的畫面
        unsafe {
            if SetConsoleOutputCP(65001) == 0 || SetConsoleCP(65001) == 0 {
                return Err(std::io::Error::last_os_error());
            }
        }
        
        // 關鍵：這裡不要再調用 _writer.write_all()
        // 只要不往 stdin 塞東西，畫面就不會被新的 Prompt 刷掉
    }

    Ok(())
}
