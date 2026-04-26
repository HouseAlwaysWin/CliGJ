use std::fmt::Write as _;

const DEFAULT_EXTENSION_ID: &str = "local.cligj-vscode-extension";

pub(crate) fn open_path_in_editor(
    uri_scheme: &str,
    path: &str,
    start_line: Option<usize>,
    end_line: Option<usize>,
) {
    let path = path.trim();
    let uri_scheme = uri_scheme.trim();
    if path.is_empty() || uri_scheme.is_empty() {
        return;
    }

    let uri = build_editor_uri(uri_scheme, path, start_line, end_line);

    #[cfg(windows)]
    {
        use std::process::Command;

        if let Err(e) = Command::new("rundll32")
            .arg("url.dll,FileProtocolHandler")
            .arg(uri.as_str())
            .spawn()
        {
            eprintln!("CliGJ: open vscode uri: {e}");
        }
    }

    #[cfg(target_os = "macos")]
    {
        use std::process::Command;

        if let Err(e) = Command::new("open").arg(uri.as_str()).spawn() {
            eprintln!("CliGJ: open vscode uri: {e}");
        }
    }

    #[cfg(all(unix, not(target_os = "macos")))]
    {
        use std::process::Command;

        if let Err(e) = Command::new("xdg-open").arg(uri.as_str()).spawn() {
            eprintln!("CliGJ: open vscode uri: {e}");
        }
    }
}

fn build_editor_uri(
    uri_scheme: &str,
    path: &str,
    start_line: Option<usize>,
    end_line: Option<usize>,
) -> String {
    let mut uri = format!(
        "{uri_scheme}://{DEFAULT_EXTENSION_ID}/openSelection?path={}",
        encode_uri_component(path)
    );

    if let Some(start) = start_line.filter(|line| *line > 0) {
        let end = end_line
            .filter(|line| *line > 0)
            .unwrap_or(start)
            .max(start);
        uri.push_str("&startLine=");
        uri.push_str(start.to_string().as_str());
        uri.push_str("&endLine=");
        uri.push_str(end.to_string().as_str());
    }

    uri
}

fn encode_uri_component(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for b in input.as_bytes() {
        match *b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(*b as char);
            }
            _ => {
                let _ = write!(&mut out, "%{:02X}", *b);
            }
        }
    }
    out
}
