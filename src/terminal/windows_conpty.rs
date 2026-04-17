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
    /// Per-screen-line spans with ANSI-resolved colors (fg + bg).
    pub lines: Vec<ColoredLine>,
    pub filled: bool,
}

pub fn spawn_conpty(shell: &str, cols: i16, rows: i16) -> Result<ConptySpawn, String> {
    unsafe {
        // Pipe for pseudo console input: we write -> console reads.
        let mut in_read = HANDLE::default();
        let mut in_write = HANDLE::default();
        CreatePipe(&mut in_read, &mut in_write, None, 0)
            .map_err(|e| format!("CreatePipe(in): {e}"))?;

        // Pipe for pseudo console output: console writes -> we read.
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

        // The ConPTY now owns these ends.
        let _ = CloseHandle(in_read);
        let _ = CloseHandle(out_write);

        // Setup attribute list with PSEUDOCONSOLE.
        let mut attr_size: usize = 0;
        // This will fail with INSUFFICIENT_BUFFER and set attr_size.
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

        // Best-effort: switch shells to UTF-8 to avoid mojibake on localized Windows installs.
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
            // Best-effort cleanup.
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

/// Screen + scrollback window pulled into Slint (matches UI row windowing budget).
const CONPTY_SNAPSHOT_MAX_LINES: usize = 240;

/// Raw-line fingerprint before ANSI→span work; skips rebuild on no-op ConPTY reads.
fn snapshot_content_fingerprint(total_rows: usize, collapsed: &[&Line]) -> u64 {
    let mut h = DefaultHasher::new();
    total_rows.hash(&mut h);
    collapsed.len().hash(&mut h);
    for line in collapsed {
        line.as_str().hash(&mut h);
    }
    h.finish()
}

/// Per-line fingerprint for a raw wezterm `Line`.
fn line_fingerprint_raw(line: &Line) -> u64 {
    let mut h = DefaultHasher::new();
    line.as_str().hash(&mut h);
    h.finish()
}

/// Cached version: only rebuild ColoredLine for lines whose content changed.
fn terminal_render_from_collapsed_cached(
    collapsed: &[&Line],
    total_scrollback_rows: usize,
    term_screen_rows: usize,
    palette: &ColorPalette,
    cache: &mut Vec<(u64, ColoredLine)>,
) -> TerminalRender {
    let mut lines = Vec::with_capacity(collapsed.len());
    for (i, line) in collapsed.iter().enumerate() {
        let fp = line_fingerprint_raw(line);
        if i < cache.len() && cache[i].0 == fp {
            // 行內容未變化 → 直接 clone 快取
            lines.push(cache[i].1.clone());
        } else {
            // 行內容有變化 → 重新建立 spans
            lines.push(line_to_colored_spans(line, palette));
        }
    }
    // 更新快取
    cache.clear();
    cache.reserve(lines.len());
    for (i, line) in collapsed.iter().enumerate() {
        let fp = line_fingerprint_raw(line);
        cache.push((fp, lines[i].clone()));
    }
    TerminalRender {
        text: String::new(),
        lines,
        filled: total_scrollback_rows > term_screen_rows,
    }
}

pub fn start_reader_thread(
    mut reader: std::fs::File,
    mut on_chunk: impl FnMut(TerminalRender) + Send + 'static,
) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        let config: Arc<dyn TerminalConfiguration> = Arc::new(CliGjTermConfig);
        let term_rows = 40usize;
        let term_cols = 120usize;
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
        let mut buf = [0u8; 32768];
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
            let collapsed = collapse_adjacent_empty_phys_lines(&line_refs);
            let collapsed = trim_trailing_empty_phys_lines(collapsed);

            let fp = snapshot_content_fingerprint(total, &collapsed);
            if last_snapshot_fp == Some(fp) {
                continue;
            }
            last_snapshot_fp = Some(fp);

            on_chunk(terminal_render_from_collapsed_cached(
                &collapsed,
                total,
                term_rows,
                &palette,
                &mut line_cache,
            ));
        }
    })
}

fn phys_line_is_effectively_empty(line: &Line) -> bool {
    line.as_str().trim_end().is_empty()
}

/// Keep at most one blank row per run of blank physical lines from the emulator buffer.
fn collapse_adjacent_empty_phys_lines<'a>(lines: &[&'a Line]) -> Vec<&'a Line> {
    let mut out = Vec::with_capacity(lines.len());
    let mut prev_empty = false;
    for &line in lines {
        let empty = phys_line_is_effectively_empty(line);
        if empty {
            if !prev_empty {
                out.push(line);
            }
            prev_empty = true;
        } else {
            prev_empty = false;
            out.push(line);
        }
    }
    out
}

/// Drop blank rows at the end of the snapshot (except leave a single line if everything is blank).
fn trim_trailing_empty_phys_lines(mut lines: Vec<&Line>) -> Vec<&Line> {
    while lines.len() > 1 && lines.last().is_some_and(|l| phys_line_is_effectively_empty(l)) {
        lines.pop();
    }
    lines
}

fn init_shell_utf8(shell: &str, writer: &mut std::fs::File) -> std::io::Result<()> {
    let cmd = if shell == "PowerShell" {
        "[Console]::InputEncoding=[System.Text.UTF8Encoding]::new(); \
[Console]::OutputEncoding=[System.Text.UTF8Encoding]::new(); \
chcp 65001 > $null\r\n"
            .to_string()
    } else {
        // Prefix with '@' to suppress cmd's command echo for this initialization line.
        "@chcp 65001 > nul\r\n".to_string()
    };
    writer.write_all(cmd.as_bytes())?;
    writer.flush()?;
    Ok(())
}

