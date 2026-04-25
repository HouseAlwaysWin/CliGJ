use std::ffi::OsStr;
use std::path::Path;
use std::os::windows::ffi::OsStrExt;
use std::os::windows::io::FromRawHandle;

use crate::terminal::pty::{PtyPair, PtyProcess, PtyReader, PtyWriter};

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

pub struct WindowsPtyProcess {
    pub _child_process: HANDLE,
    pub _child_thread: HANDLE,
    pub hpc: HPCON,
    pub _attr_list_ptr: *mut std::ffi::c_void,
    pub _attr_list_buf: Box<[u8]>,
}

unsafe impl Send for WindowsPtyProcess {}
unsafe impl Sync for WindowsPtyProcess {}

impl PtyProcess for WindowsPtyProcess {
    fn resize(&self, cols: u16, rows: u16) -> Result<(), String> {
        unsafe {
            ResizePseudoConsole(self.hpc, COORD { X: cols as i16, Y: rows as i16 })
                .map_err(|e| format!("ResizePseudoConsole: {e}"))
        }
    }
}

impl Drop for WindowsPtyProcess {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseHandle(self._child_thread);
            let _ = CloseHandle(self._child_process);
            ClosePseudoConsole(self.hpc);
            if !self._attr_list_ptr.is_null() {
                DeleteProcThreadAttributeList(LPPROC_THREAD_ATTRIBUTE_LIST(self._attr_list_ptr as *mut _));
            }
        }
    }
}

pub struct WindowsPtyReader(pub std::fs::File);
impl std::io::Read for WindowsPtyReader {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        self.0.read(buf)
    }
}
impl PtyReader for WindowsPtyReader {}

pub struct WindowsPtyWriter(pub std::fs::File);
impl std::io::Write for WindowsPtyWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.write(buf)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.0.flush()
    }
}
impl PtyWriter for WindowsPtyWriter {}

pub fn spawn_conpty(shell: &str, cols: u16, rows: u16, current_dir: Option<&Path>) -> Result<PtyPair, String> {
    let (_, cmdline) = build_shell_command(shell);
    spawn_conpty_command_line(cmdline.as_str(), cols, rows, current_dir)
}

pub fn spawn_conpty_command_line(
    command_line: &str,
    cols: u16,
    rows: u16,
    current_dir: Option<&Path>,
) -> Result<PtyPair, String> {
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
            COORD { X: cols as i16, Y: rows as i16 },
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

        let mut writer_file = std::fs::File::from_raw_handle(in_write.0 as *mut _);
        let reader_file = std::fs::File::from_raw_handle(out_read.0 as *mut _);

        let _ = init_shell_utf8(&mut writer_file);

        Ok(PtyPair {
            process: Box::new(WindowsPtyProcess {
                _child_process: pi.hProcess,
                _child_thread: pi.hThread,
                hpc,
                _attr_list_ptr: attr_list,
                _attr_list_buf: attr_list_buf,
            }),
            reader: Box::new(WindowsPtyReader(reader_file)),
            writer: Box::new(WindowsPtyWriter(writer_file)),
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

fn init_shell_utf8(_writer: &mut std::fs::File) -> std::io::Result<()> {
    #[cfg(windows)]
    {
        unsafe {
            if SetConsoleOutputCP(65001) == 0 || SetConsoleCP(65001) == 0 {
                return Err(std::io::Error::last_os_error());
            }
        }
    }
    Ok(())
}
