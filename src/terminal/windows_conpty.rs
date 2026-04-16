use std::ffi::OsStr;
use std::io::{Read, Write};
use std::os::windows::ffi::OsStrExt;
use std::os::windows::io::FromRawHandle;
use std::thread;

use std::sync::Arc;

use wezterm_term::config::TerminalConfiguration;
use wezterm_term::Terminal;
use wezterm_term::TerminalSize;
use wezterm_term::color::ColorPalette;

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
        let mut term = Terminal::new(term_size, config, "CliGJ", "0", writer);

        let mut buf = [0u8; 8192];
        loop {
            let n = match reader.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => n,
                Err(_) => break,
            };
            // Feed into terminal emulator; it will parse VT/ANSI and maintain a screen buffer.
            term.advance_bytes(&buf[..n]);

            // Render a portion of the screen+scrollback to plain text.
            // Keep enough history so the initial cmd banner remains scrollback-visible,
            // while still bounding memory and UI update cost.
            let screen = term.screen();
            let total = screen.scrollback_rows();
            // Default scrollback size in wezterm-term is 3500 rows; keep more than that
            // so the initial cmd banner remains visible even after a couple of commands.
            const MAX_LINES: usize = 4000;
            let start = total.saturating_sub(MAX_LINES);
            let lines = screen.lines_in_phys_range(start..total);
            let mut out = String::new();
            for (i, line) in lines.iter().enumerate() {
                if i > 0 {
                    out.push('\n');
                }
                out.push_str(line.as_str().trim_end());
            }

            // Remove our init noise if it was echoed.
            let out = filter_init_noise(&out);
            let filled = total > term_rows;
            on_chunk(TerminalRender { text: out, filled });
        }
    })
}

fn filter_init_noise(s: &str) -> String {
    // Remove the UTF-8 initialization commands that may get echoed by cmd/PowerShell.
    // Use substring removal instead of dropping whole lines, because the shell prompt
    // can appear on the same line as the command echo.
    s.replace("@chcp 65001 > nul\n", "")
        .replace("chcp 65001 > nul\n", "")
        .replace("chcp 65001 > $null\n", "")
        .replace("[Console]::InputEncoding=[System.Text.UTF8Encoding]::new(); ", "")
        .replace("[Console]::OutputEncoding=[System.Text.UTF8Encoding]::new(); ", "")
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

