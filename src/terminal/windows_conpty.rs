use std::collections::hash_map::DefaultHasher;
use std::ffi::OsStr;
use std::hash::{Hash, Hasher};
use std::io::{Read, Write};
use std::os::windows::ffi::OsStrExt;
use std::os::windows::io::FromRawHandle;
use std::thread;

use std::sync::Arc;

use wezterm_term::config::TerminalConfiguration;
use wezterm_term::Line;
use wezterm_term::Terminal;
use wezterm_term::TerminalSize;
use wezterm_term::color::ColorPalette;

use crate::terminal::render::{line_to_colored_spans, ColoredLine};

use windows::core::{PCWSTR, PWSTR};
use windows::Win32::Foundation::{CloseHandle, HANDLE};
use windows::Win32::System::Console::{ClosePseudoConsole, CreatePseudoConsole, COORD, HPCON};
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
    pub _hpc: HPCON,
    pub _attr_list_ptr: *mut std::ffi::c_void,
    pub _attr_list_buf: Box<[u8]>,
}

pub struct ConptySpawn {
    pub session: ConptySession,
    pub reader: std::fs::File,
}

pub struct TerminalRender {
    pub text: String,
    /// ONLY lines that changed (matches changed_indices length).
    pub lines: Vec<ColoredLine>,
    pub full_len: usize,
    pub first_line_idx: usize,
    pub cursor_row: Option<usize>,
    pub cursor_col: Option<usize>,
    pub filled: bool,
    /// Indices of lines that changed since last render (for downstream diff).
    pub changed_indices: Vec<usize>,
}

pub fn spawn_conpty(shell: &str, cols: i16, rows: i16) -> Result<ConptySpawn, String> {
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

        let (app, cmdline) = build_shell_command(shell);
        let mut cmdline_w = to_wide_null(&cmdline);

        let mut pi: PROCESS_INFORMATION = std::mem::zeroed();
        CreateProcessW(
            PCWSTR(app.as_ptr()),
            Some(PWSTR(cmdline_w.as_mut_ptr())),
            None,
            None,
            false,
            EXTENDED_STARTUPINFO_PRESENT,
            None,
            None,
            &siex.StartupInfo,
            &mut pi,
        )
        .map_err(|e| format!("CreateProcessW: {e}"))?;

        let mut writer = std::fs::File::from_raw_handle(in_write.0 as *mut _);
        let reader = std::fs::File::from_raw_handle(out_read.0 as *mut _);

        let _ = init_shell_utf8(shell, &mut writer);

        Ok(ConptySpawn {
            session: ConptySession {
                writer,
                _child_process: pi.hProcess,
                _child_thread: pi.hThread,
                _hpc: hpc,
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
            ClosePseudoConsole(self._hpc);
            if !self._attr_list_ptr.is_null() {
                DeleteProcThreadAttributeList(LPPROC_THREAD_ATTRIBUTE_LIST(self._attr_list_ptr as *mut _));
            }
        }
    }
}

const CONPTY_SNAPSHOT_MAX_LINES: usize = 240;

fn snapshot_content_fingerprint(
    total_rows: usize,
    lines: &[&Line],
    cursor_local_row: Option<usize>,
    cursor_col: Option<usize>,
) -> u64 {
    let mut h = DefaultHasher::new();
    total_rows.hash(&mut h);
    lines.len().hash(&mut h);
    cursor_local_row.hash(&mut h);
    cursor_col.hash(&mut h);
    for line in lines {
        line.as_str().hash(&mut h);
    }
    h.finish()
}

fn line_fingerprint_raw(line: &Line, cursor_col: Option<usize>) -> u64 {
    let mut h = DefaultHasher::new();
    line.as_str().hash(&mut h);
    cursor_col.hash(&mut h);
    h.finish()
}

fn terminal_render_from_lines_cached(
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

    // 確保 cache 長度足夠
    if cache.len() < start_phys_idx + num_lines {
        cache.resize(start_phys_idx + num_lines, (0, ColoredLine::default()));
    }

    for i in 0..num_lines {
        let global_idx = start_phys_idx + i;
        let active_cursor_col = if cursor_local_row == Some(i) {
            cursor_col
        } else {
            None
        };
        let fp = line_fingerprint_raw(lines[i], active_cursor_col);
        if cache[global_idx].0 != fp {
            let built = line_to_colored_spans(lines[i], palette, None);
            cache[global_idx] = (fp, built);
            changed_indices.push(i);
        }
    }

    let changed_lines: Vec<ColoredLine> = changed_indices
        .iter()
        .map(|&i| cache[start_phys_idx + i].1.clone())
        .collect();

    TerminalRender {
        text: String::new(),
        lines: changed_lines,
        full_len: num_lines,
        first_line_idx: start_phys_idx,
        cursor_row: cursor_local_row.map(|row| start_phys_idx + row),
        cursor_col,
        filled: total_scrollback_rows > term_screen_rows,
        changed_indices,
    }
}

pub fn start_reader_thread(
    mut reader: std::fs::File,
    mut on_chunk: impl FnMut(TerminalRender) + Send + 'static,
) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        let config: Arc<dyn TerminalConfiguration> = Arc::new(CliGjTermConfig);
        let term_rows = 60usize;
        let term_cols = 200usize;
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

        let mut last_snapshot_fp: Option<u64> = None;
        let mut line_cache: Vec<(u64, ColoredLine)> = Vec::new();
        let mut buf = [0u8; 65536];
        loop {
            let n = match reader.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => n,
                Err(_) => break,
            };
            term.advance_bytes(&buf[..n]);

            let screen = term.screen();
            let total = screen.scrollback_rows();
            let start = total.saturating_sub(CONPTY_SNAPSHOT_MAX_LINES);
            let lines = screen.lines_in_phys_range(start..total);
            let line_refs: Vec<&Line> = lines.iter().collect();
            let cursor = term.cursor_pos();
            let cursor_phys_row = screen.phys_row(cursor.y);
            let cursor_local_row =
                cursor_phys_row.checked_sub(start).filter(|row| *row < line_refs.len());
            let cursor_col = Some(cursor.x);

            let fp = snapshot_content_fingerprint(total, &line_refs, cursor_local_row, cursor_col);
            if last_snapshot_fp == Some(fp) {
                continue;
            }
            last_snapshot_fp = Some(fp);

            on_chunk({
                terminal_render_from_lines_cached(
                    &line_refs,
                    start,
                    total,
                    term_rows,
                    &palette,
                    cursor_local_row,
                    cursor_col,
                    &mut line_cache,
                )
            });
        }
    })
}

fn init_shell_utf8(shell: &str, writer: &mut std::fs::File) -> std::io::Result<()> {
    let cmd = if shell == "PowerShell" {
        "[Console]::InputEncoding=[System.Text.UTF8Encoding]::new(); \
[Console]::OutputEncoding=[System.Text.UTF8Encoding]::new(); \
chcp 65001 > $null\r\n"
            .to_string()
    } else {
        "@chcp 65001 > nul\r\n".to_string()
    };
    writer.write_all(cmd.as_bytes())?;
    writer.flush()?;
    Ok(())
}
