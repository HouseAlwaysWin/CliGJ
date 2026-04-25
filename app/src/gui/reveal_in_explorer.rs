//! Open the system file manager with a file selected (Windows: Explorer `/select,`).

use std::path::Path;

/// Reveal `path` in the OS file manager (Explorer on Windows, folder view elsewhere).
pub(crate) fn reveal_path_in_file_manager(path: &str) {
    let path = path.trim();
    if path.is_empty() {
        return;
    }
    let p = Path::new(path);
    if !p.exists() {
        eprintln!("CliGJ: reveal in explorer: path does not exist: {path}");
        return;
    }

    #[cfg(windows)]
    {
        use std::process::Command;
        let mut arg = std::ffi::OsString::from("/select,");
        arg.push(p.as_os_str());
        if let Err(e) = Command::new("explorer").arg(arg).spawn() {
            eprintln!("CliGJ: explorer: {e}");
        }
    }

    #[cfg(not(windows))]
    {
        use std::process::Command;
        let dir = p.parent().filter(|d| !d.as_os_str().is_empty()).unwrap_or_else(|| Path::new("."));
        let _ = Command::new("xdg-open").arg(dir).spawn();
    }
}

