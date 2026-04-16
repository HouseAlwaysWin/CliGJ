use std::cell::RefCell;
use std::process::Command;
use std::rc::Rc;

use encoding_rs::{
    BIG5, EUC_KR, Encoding, GBK, SHIFT_JIS, UTF_8, WINDOWS_1250, WINDOWS_1251, WINDOWS_1252,
    WINDOWS_1253, WINDOWS_1254, WINDOWS_1255, WINDOWS_1256, WINDOWS_1257, WINDOWS_1258,
};
use slint::{Model, ModelRc, SharedString, VecModel};

slint::include_modules!();

pub fn run_gui() {
    let app = AppWindow::new().expect("failed to build app window");

    let titles = Rc::new(VecModel::from(vec![SharedString::from("工作階段 1")]));

    let state = Rc::new(RefCell::new(GuiState {
        tabs: vec![TabState::default()],
        titles: Rc::clone(&titles),
        current: 0,
    }));

    app.set_tab_titles(ModelRc::from(Rc::clone(&titles)));
    sync_tab_count(&app, state.borrow().tabs.len());
    load_tab_to_ui(&app, &state.borrow().tabs[0]);

    let state_for_tab = Rc::clone(&state);
    let app_weak = app.as_weak();
    app.on_tab_changed(move |new_index| {
        let Some(ui) = app_weak.upgrade() else {
            return;
        };
        let mut s = state_for_tab.borrow_mut();
        if let Err(e) = s.switch_tab(new_index as usize, &ui) {
            eprintln!("CliGJ: tab switch: {e}");
        }
    });

    let state_for_close = Rc::clone(&state);
    let app_weak = app.as_weak();
    app.on_tab_close_requested(move |index| {
        let Some(ui) = app_weak.upgrade() else {
            return;
        };
        let mut s = state_for_close.borrow_mut();
        if let Err(e) = s.close_tab(index as usize, &ui) {
            eprintln!("CliGJ: close tab: {e}");
        }
    });

    let state_for_new = Rc::clone(&state);
    let app_weak = app.as_weak();
    app.on_new_tab_requested(move || {
        let Some(ui) = app_weak.upgrade() else {
            return;
        };
        let mut s = state_for_new.borrow_mut();
        if let Err(e) = s.add_tab(&ui) {
            eprintln!("CliGJ: new tab: {e}");
        }
    });

    let state_for_cmd = Rc::clone(&state);
    let app_weak = app.as_weak();
    app.on_cmd_type_changed(move |kind| {
        let Some(ui) = app_weak.upgrade() else {
            return;
        };
        let mut s = state_for_cmd.borrow_mut();
        if let Err(e) = s.change_current_cmd_type(kind.as_str(), &ui) {
            eprintln!("CliGJ: cmd type change: {e}");
        }
    });

    let state_for_submit = Rc::clone(&state);
    let app_weak = app.as_weak();
    app.on_submit_prompt(move || {
        let Some(ui) = app_weak.upgrade() else {
            return;
        };
        let mut s = state_for_submit.borrow_mut();
        if let Err(e) = s.submit_current_prompt(&ui) {
            eprintln!("CliGJ: prompt submit: {e}");
        }
    });

    app.run().expect("failed to run app window");
}

struct TabState {
    file_path: String,
    has_image: bool,
    preview_image: slint::Image,
    code_lines: Vec<String>,
    selected_line: i32,
    selected_context: SharedString,
    prompt: SharedString,
    cmd_type: String,
    current_dir: String,
}

impl Default for TabState {
    fn default() -> Self {
        let cmd_type = default_cmd_type().to_string();
        let current_dir = default_prompt_dir();
        Self {
            file_path: String::new(),
            has_image: false,
            preview_image: slint::Image::default(),
            code_lines: initial_terminal_lines(&cmd_type, &current_dir),
            selected_line: 0,
            selected_context: SharedString::new(),
            prompt: SharedString::new(),
            cmd_type,
            current_dir,
        }
    }
}

struct GuiState {
    tabs: Vec<TabState>,
    titles: Rc<VecModel<SharedString>>,
    current: usize,
}

impl GuiState {
    fn switch_tab(&mut self, new_index: usize, ui: &AppWindow) -> Result<(), &'static str> {
        if new_index >= self.tabs.len() {
            return Err("invalid tab index");
        }
        if new_index == self.current {
            return Ok(());
        }

        let old_dir = self.tabs[self.current].current_dir.clone();
        self.tabs[self.current] = ui_to_tab_state(ui, old_dir);
        self.current = new_index;
        ui.set_current_tab(new_index as i32);
        load_tab_to_ui(ui, &self.tabs[new_index]);
        Ok(())
    }

    fn add_tab(&mut self, ui: &AppWindow) -> Result<(), &'static str> {
        let old_dir = self.tabs[self.current].current_dir.clone();
        self.tabs[self.current] = ui_to_tab_state(ui, old_dir);

        let n = self.titles.row_count();
        let label = SharedString::from(format!("工作階段 {}", n + 1));
        self.titles.push(label);
        self.tabs.push(TabState::default());

        let new_index = self.tabs.len() - 1;
        self.current = new_index;
        ui.set_current_tab(new_index as i32);
        sync_tab_count(ui, self.tabs.len());
        load_tab_to_ui(ui, &self.tabs[new_index]);
        Ok(())
    }

    fn change_current_cmd_type(&mut self, new_cmd_type: &str, ui: &AppWindow) -> Result<(), &'static str> {
        if self.current >= self.tabs.len() {
            return Err("invalid current tab index");
        }
        let old_dir = self.tabs[self.current].current_dir.clone();
        self.tabs[self.current] = ui_to_tab_state(ui, old_dir);
        self.tabs[self.current].cmd_type = new_cmd_type.to_string();
        if self.tabs[self.current].code_lines.is_empty() {
            let tab_dir = self.tabs[self.current].current_dir.clone();
            self.tabs[self.current].code_lines = initial_terminal_lines(new_cmd_type, &tab_dir);
        } else {
            self.tabs[self.current]
                .code_lines
                .push(format!("[switched shell => {new_cmd_type}]"));
            let prompt_dir = self.tabs[self.current].current_dir.clone();
            self.tabs[self.current]
                .code_lines
                .push(prompt_line_for_shell(new_cmd_type, &prompt_dir));
        }
        ui.set_ws_cmd_type(SharedString::from(new_cmd_type));
        load_tab_to_ui(ui, &self.tabs[self.current]);
        Ok(())
    }

    fn submit_current_prompt(&mut self, ui: &AppWindow) -> Result<(), &'static str> {
        if self.current >= self.tabs.len() {
            return Err("invalid current tab index");
        }
        let old_dir = self.tabs[self.current].current_dir.clone();
        self.tabs[self.current] = ui_to_tab_state(ui, old_dir);
        let tab = &mut self.tabs[self.current];
        let command_line = tab.prompt.to_string();
        let command_line = command_line.trim().to_string();
        if command_line.is_empty() {
            return Ok(());
        }

        append_command_banner(
            &mut tab.code_lines,
            &tab.cmd_type,
            &tab.current_dir,
            &command_line,
        );
        match run_command_for_shell(&tab.cmd_type, &command_line, &tab.current_dir) {
            Ok(result) => {
                tab.current_dir = result.next_dir;
                append_stream_lines(&mut tab.code_lines, &result.stdout, false);
                append_stream_lines(&mut tab.code_lines, &result.stderr, true);
                if result.exit_code.unwrap_or(-1) != 0 {
                    tab.code_lines
                        .push(format!("[exit: {}]", result.exit_code.unwrap_or(-1)));
                }
            }
            Err(err) => {
                tab.code_lines.push(format!("[error] {err}"));
            }
        }
        tab.code_lines.push(String::new());
        tab.code_lines
            .push(prompt_line_for_shell(&tab.cmd_type, &tab.current_dir));
        tab.prompt = SharedString::new();
        load_tab_to_ui(ui, tab);
        Ok(())
    }

    fn close_tab(&mut self, index: usize, ui: &AppWindow) -> Result<(), &'static str> {
        if self.tabs.len() <= 1 {
            return Ok(());
        }
        if index >= self.tabs.len() {
            return Err("invalid close index");
        }

        let old_dir = self.tabs[self.current].current_dir.clone();
        self.tabs[self.current] = ui_to_tab_state(ui, old_dir);

        self.titles.remove(index);
        self.tabs.remove(index);

        let new_len = self.tabs.len();
        let old_current = self.current;

        let new_current = if old_current > index {
            old_current - 1
        } else if old_current == index {
            index.min(new_len - 1)
        } else {
            old_current
        };

        self.current = new_current;
        ui.set_current_tab(new_current as i32);
        sync_tab_count(ui, self.tabs.len());
        load_tab_to_ui(ui, &self.tabs[new_current]);
        Ok(())
    }
}

fn sync_tab_count(ui: &AppWindow, n: usize) {
    ui.set_tab_count(n as i32);
}

fn ui_to_tab_state(ui: &AppWindow, current_dir: String) -> TabState {
    let lines: Vec<String> = ui
        .get_ws_code_lines()
        .iter()
        .map(|s| s.to_string())
        .collect();

    TabState {
        file_path: ui.get_ws_file_path().to_string(),
        has_image: ui.get_ws_has_image(),
        preview_image: ui.get_ws_preview_image(),
        code_lines: lines,
        selected_line: ui.get_ws_selected_line(),
        selected_context: ui.get_ws_selected_context(),
        prompt: ui.get_ws_prompt(),
        cmd_type: ui.get_ws_cmd_type().to_string(),
        current_dir,
    }
}

fn load_tab_to_ui(ui: &AppWindow, tab: &TabState) {
    ui.set_ws_file_path(SharedString::from(tab.file_path.as_str()));
    ui.set_ws_has_image(tab.has_image);
    ui.set_ws_preview_image(tab.preview_image.clone());

    let model_data: Vec<SharedString> = tab
        .code_lines
        .iter()
        .map(|s| SharedString::from(s.as_str()))
        .collect();
    let model = ModelRc::new(VecModel::from(model_data));
    ui.set_ws_code_lines(model);
    ui.set_ws_terminal_text(SharedString::from(lines_to_terminal_text(&tab.code_lines)));

    ui.set_ws_selected_line(tab.selected_line);
    ui.set_ws_selected_context(tab.selected_context.clone());
    ui.set_ws_prompt(tab.prompt.clone());
    ui.set_ws_cmd_type(SharedString::from(tab.cmd_type.as_str()));
}

fn default_cmd_type() -> &'static str {
    if cfg!(target_os = "windows") {
        "Command Prompt"
    } else {
        "Shell"
    }
}

struct CmdExecutionResult {
    exit_code: Option<i32>,
    stdout: String,
    stderr: String,
    next_dir: String,
}

fn run_command_for_shell(
    shell_kind: &str,
    command_line: &str,
    current_dir: &str,
) -> Result<CmdExecutionResult, String> {
    let mut command = if cfg!(target_os = "windows") {
        match shell_kind {
            "PowerShell" => {
                let mut c = Command::new("powershell");
                c.args(["-NoProfile", "-Command", command_line]);
                c
            }
            "Git Bash" => {
                let git_bash = r"C:\Program Files\Git\bin\bash.exe";
                let mut c = if std::path::Path::new(git_bash).exists() {
                    Command::new(git_bash)
                } else {
                    Command::new("bash")
                };
                c.args(["-lc", command_line]);
                c
            }
            _ => {
                let mut c = Command::new("cmd");
                let wrapped = format!("{command_line} & cd");
                c.args(["/D", "/C", &wrapped]);
                c
            }
        }
    } else {
        match shell_kind {
            "PowerShell" => {
                let mut c = Command::new("pwsh");
                c.args(["-NoProfile", "-Command", command_line]);
                c
            }
            "Git Bash" => {
                let mut c = Command::new("bash");
                c.args(["-lc", command_line]);
                c
            }
            _ => {
                let mut c = Command::new("sh");
                c.args(["-lc", command_line]);
                c
            }
        }
    };

    command.current_dir(current_dir);
    let output = command.output().map_err(|e| e.to_string())?;
    let cmd_code_page = if cfg!(target_os = "windows") && shell_kind == "Command Prompt" {
        detect_windows_console_code_page()
    } else {
        None
    };
    let decoded_stdout = decode_output_text_with_cp(shell_kind, &output.stdout, cmd_code_page);
    let (stdout, next_dir) = if cfg!(target_os = "windows") && shell_kind == "Command Prompt" {
        let (cleaned, detected_dir) = split_cmd_stdout_and_dir(&decoded_stdout);
        (cleaned, detected_dir.unwrap_or_else(|| current_dir.to_string()))
    } else {
        (decoded_stdout, current_dir.to_string())
    };

    Ok(CmdExecutionResult {
        exit_code: output.status.code(),
        stdout,
        stderr: decode_output_text_with_cp(shell_kind, &output.stderr, cmd_code_page),
        next_dir,
    })
}

fn append_command_banner(
    lines: &mut Vec<String>,
    shell_kind: &str,
    current_dir: &str,
    command_line: &str,
) {
    lines.push(format!(
        "{} {command_line}",
        prompt_line_for_shell(shell_kind, current_dir)
    ));
}

fn append_stream_lines(lines: &mut Vec<String>, stream_text: &str, is_stderr: bool) {
    for line in stream_text.lines() {
        if is_stderr {
            lines.push(format!("ERR: {line}"));
        } else {
            lines.push(line.to_string());
        }
    }
}

fn initial_terminal_lines(shell_kind: &str, current_dir: &str) -> Vec<String> {
    match shell_kind {
        "Command Prompt" => vec![
            command_prompt_version_line(),
            command_prompt_copyright_line(),
            String::new(),
            prompt_line_for_shell(shell_kind, current_dir),
        ],
        "PowerShell" => vec![
            "Windows PowerShell".to_string(),
            "Copyright (C) Microsoft Corporation. All rights reserved.".to_string(),
            String::new(),
            prompt_line_for_shell(shell_kind, current_dir),
        ],
        "Git Bash" => vec![
            "GNU bash terminal".to_string(),
            String::new(),
            prompt_line_for_shell(shell_kind, current_dir),
        ],
        _ => vec![prompt_line_for_shell(shell_kind, current_dir)],
    }
}

fn current_dir_display() -> String {
    std::env::current_dir()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| ".".to_string())
}

fn prompt_line_for_shell(shell_kind: &str, cwd: &str) -> String {
    match shell_kind {
        "PowerShell" => format!("PS {cwd}>"),
        "Git Bash" => format!("{cwd}$"),
        _ => format!("{cwd}>"),
    }
}

fn default_prompt_dir() -> String {
    if cfg!(target_os = "windows") {
        std::env::var("USERPROFILE").unwrap_or_else(|_| current_dir_display())
    } else {
        std::env::var("HOME").unwrap_or_else(|_| current_dir_display())
    }
}

fn command_prompt_version_line() -> String {
    if cfg!(target_os = "windows") {
        "Microsoft Windows [Version 10.0]".to_string()
    } else {
        "Microsoft Windows [Version Unknown]".to_string()
    }
}

fn command_prompt_copyright_line() -> String {
    "(c) Microsoft Corporation. All rights reserved.".to_string()
}

fn lines_to_terminal_text(lines: &[String]) -> String {
    lines.join("\n")
}

fn split_cmd_stdout_and_dir(stdout: &str) -> (String, Option<String>) {
    let mut lines: Vec<String> = stdout
        .lines()
        .map(|line| line.trim_end_matches('\r').to_string())
        .collect();

    while lines.last().is_some_and(|line| line.trim().is_empty()) {
        lines.pop();
    }

    let Some(last) = lines.last() else {
        return (String::new(), None);
    };
    let maybe_dir = last.trim();
    if !looks_like_windows_dir(maybe_dir) {
        return (stdout.to_string(), None);
    }

    let next_dir = maybe_dir.to_string();
    lines.pop();
    let cleaned = lines.join("\n");
    (cleaned, Some(next_dir))
}

fn looks_like_windows_dir(value: &str) -> bool {
    if value.len() < 3 {
        return false;
    }
    let bytes = value.as_bytes();
    if !bytes[0].is_ascii_alphabetic() || bytes[1] != b':' {
        return false;
    }
    bytes[2] == b'\\' || bytes[2] == b'/'
}

fn decode_output_text_with_cp(shell_kind: &str, bytes: &[u8], code_page: Option<u16>) -> String {
    if cfg!(target_os = "windows") && shell_kind == "Command Prompt" {
        decode_windows_cmd_text(bytes, code_page)
    } else {
        String::from_utf8_lossy(bytes).to_string()
    }
}

fn decode_windows_cmd_text(bytes: &[u8], code_page: Option<u16>) -> String {
    if bytes.is_empty() {
        return String::new();
    }

    if bytes.len() >= 2 {
        if bytes[0] == 0xff && bytes[1] == 0xfe {
            return decode_utf16_le(bytes);
        }
        if bytes[0] == 0xfe && bytes[1] == 0xff {
            return decode_utf16_be(bytes);
        }
    }

    let zero_count = bytes.iter().filter(|b| **b == 0).count();
    let looks_utf16_no_bom = bytes.len() % 2 == 0 && zero_count * 100 / bytes.len() >= 20;
    if looks_utf16_no_bom {
        return decode_utf16_le(bytes);
    }

    let mut candidates: Vec<String> = vec![String::from_utf8_lossy(bytes).to_string()];

    if let Some(decoded) = decode_with_code_page(bytes, code_page) {
        candidates.push(decoded);
    }

    // Common Windows East-Asia fallbacks, useful when tool output encoding
    // differs from current console code page.
    for enc in [BIG5, GBK, SHIFT_JIS, EUC_KR] {
        if let Some(decoded) = decode_with_encoding(bytes, enc) {
            candidates.push(decoded);
        }
    }

    choose_best_decoding(&candidates, code_page)
}

fn decode_utf16_le(bytes: &[u8]) -> String {
    let mut units = Vec::with_capacity(bytes.len() / 2);
    for chunk in bytes.chunks_exact(2) {
        units.push(u16::from_le_bytes([chunk[0], chunk[1]]));
    }
    String::from_utf16_lossy(&units)
        .trim_start_matches('\u{feff}')
        .to_string()
}

fn decode_utf16_be(bytes: &[u8]) -> String {
    let mut units = Vec::with_capacity(bytes.len() / 2);
    for chunk in bytes.chunks_exact(2) {
        units.push(u16::from_be_bytes([chunk[0], chunk[1]]));
    }
    String::from_utf16_lossy(&units)
        .trim_start_matches('\u{feff}')
        .to_string()
}

fn detect_windows_console_code_page() -> Option<u16> {
    if !cfg!(target_os = "windows") {
        return None;
    }

    let output = Command::new("cmd")
        .args(["/D", "/C", "chcp"])
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&output.stdout);
    let mut digits = String::new();
    for ch in text.chars() {
        if ch.is_ascii_digit() {
            digits.push(ch);
        } else if !digits.is_empty() {
            break;
        }
    }
    digits.parse::<u16>().ok()
}

fn decode_with_code_page(bytes: &[u8], code_page: Option<u16>) -> Option<String> {
    let cp = code_page?;
    let encoding = match cp {
        65001 => UTF_8,
        950 => BIG5,
        936 => GBK,
        932 => SHIFT_JIS,
        949 => EUC_KR,
        1250 => WINDOWS_1250,
        1251 => WINDOWS_1251,
        1252 => WINDOWS_1252,
        1253 => WINDOWS_1253,
        1254 => WINDOWS_1254,
        1255 => WINDOWS_1255,
        1256 => WINDOWS_1256,
        1257 => WINDOWS_1257,
        1258 => WINDOWS_1258,
        _ => return None,
    };
    let (decoded, _, had_errors) = encoding.decode(bytes);
    if had_errors {
        None
    } else {
        Some(decoded.into_owned())
    }
}

fn decode_with_encoding(bytes: &[u8], encoding: &'static Encoding) -> Option<String> {
    let (decoded, _, had_errors) = encoding.decode(bytes);
    if had_errors {
        None
    } else {
        Some(decoded.into_owned())
    }
}

fn choose_best_decoding(candidates: &[String], code_page: Option<u16>) -> String {
    if candidates.is_empty() {
        return String::new();
    }
    let mut best = &candidates[0];
    let mut best_score = quality_score(best, code_page);
    for text in &candidates[1..] {
        let score = quality_score(text, code_page);
        if score > best_score {
            best = text;
            best_score = score;
        }
    }
    best.clone()
}

fn quality_score(text: &str, code_page: Option<u16>) -> i64 {
    let prefer_cjk = matches!(code_page, Some(950 | 936));
    let mut score = 0_i64;
    for ch in text.chars() {
        if ch == '\u{fffd}' || ch == '□' {
            score -= 20;
        } else if ch == '\0' {
            score -= 10;
        } else if ch.is_control() && ch != '\n' && ch != '\r' && ch != '\t' {
            score -= 6;
        } else if ('\u{ff61}'..='\u{ff9f}').contains(&ch) {
            // Half-width katakana often appears in mojibake for Big5/GBK text.
            score -= if prefer_cjk { 8 } else { 2 };
        } else if ('\u{3040}'..='\u{30ff}').contains(&ch) {
            // Hiragana/Katakana are unlikely under CP950/CP936 terminals.
            score -= if prefer_cjk { 4 } else { 0 };
        } else if ('\u{4e00}'..='\u{9fff}').contains(&ch) {
            score += if prefer_cjk { 6 } else { 3 };
        } else if ch.is_ascii_alphanumeric() || ch.is_ascii_whitespace() {
            score += 2;
        } else if ch.is_ascii_punctuation() {
            score += 1;
        } else {
            score += 3;
        }
    }
    score
}
