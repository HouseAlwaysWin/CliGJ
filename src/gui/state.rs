use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::mpsc;

use slint::{SharedString, VecModel};

use crate::terminal::render::ColoredLine;

#[cfg(target_os = "windows")]
use crate::terminal::windows_conpty;

pub(crate) fn workspace_root_for_tab(tab: &TabState) -> PathBuf {
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    if tab.file_path.is_empty() {
        return cwd;
    }
    let p = Path::new(&tab.file_path);
    if p.is_file() {
        p.parent().map(Path::to_path_buf).unwrap_or(cwd)
    } else if p.is_dir() {
        p.to_path_buf()
    } else {
        cwd
    }
}

pub(crate) fn default_cmd_type() -> &'static str {
    if cfg!(target_os = "windows") {
        "Command Prompt"
    } else {
        "Shell"
    }
}

pub struct TabState {
    pub(crate) id: u64,
    pub(crate) file_path: String,
    pub(crate) has_image: bool,
    pub(crate) preview_image: slint::Image,
    pub(crate) selected_line: i32,
    pub(crate) selected_context: SharedString,
    pub(crate) prompt: SharedString,
    pub(crate) cmd_type: String,
    pub(crate) terminal_text: String,
    pub(crate) auto_scroll: bool,
    pub(crate) terminal_select_mode: bool,
    pub(crate) raw_input_mode: bool,
    pub(crate) command_history: Vec<String>,
    pub(crate) history_cursor: Option<usize>,
    pub(crate) history_draft: String,
    /// Absolute paths selected from `@` picker; shown as chips, appended on submit.
    pub(crate) prompt_picked_files_abs: Vec<String>,
    /// VT-colored screen lines (ConPTY + wezterm-term); empty => plain `TextEdit` fallback.
    pub(crate) terminal_lines: Vec<ColoredLine>,
    /// Last `prompt` string written to ConPTY while `@` is active (composer → shell line sync).
    pub(crate) composer_pty_mirror: String,

    #[cfg(target_os = "windows")]
    pub(crate) conpty: Option<windows_conpty::ConptySession>,
}

impl TabState {
    pub fn new(id: u64, tx: mpsc::Sender<TerminalChunk>) -> Self {
        let cmd_type = default_cmd_type().to_string();
        let mut me = Self {
            id,
            file_path: String::new(),
            has_image: false,
            preview_image: slint::Image::default(),
            selected_line: 0,
            selected_context: SharedString::new(),
            prompt: SharedString::new(),
            cmd_type,
            terminal_text: String::new(),
            auto_scroll: false,
            terminal_select_mode: false,
            raw_input_mode: false,
            command_history: Vec::new(),
            history_cursor: None,
            history_draft: String::new(),
            prompt_picked_files_abs: Vec::new(),
            terminal_lines: Vec::new(),
            composer_pty_mirror: String::new(),
            #[cfg(target_os = "windows")]
            conpty: None,
        };

        #[cfg(target_os = "windows")]
        {
            if me.cmd_type == "Command Prompt" || me.cmd_type == "PowerShell" {
                if let Ok(spawn) = windows_conpty::spawn_conpty(&me.cmd_type, 120, 40) {
                    let tab_id = me.id;
                    windows_conpty::start_reader_thread(spawn.reader, move |render| {
                        let _ = tx.send(TerminalChunk {
                            tab_id,
                            text: render.text,
                            lines: render.lines,
                            replace: true,
                            set_auto_scroll: if render.filled { Some(true) } else { None },
                        });
                    });
                    me.conpty = Some(spawn.session);
                }
            }
        }

        me
    }

    pub fn append_terminal(&mut self, chunk: &str) {
        const MAX: usize = 1_000_000;
        const TAIL_MAX: usize = 80_000;
        self.terminal_text.push_str(chunk);
        let limit = if self.auto_scroll { TAIL_MAX } else { MAX };
        if self.terminal_text.len() > limit {
            let cut = self.terminal_text.len() - limit;
            self.terminal_text.drain(..cut);
        }
        self.terminal_lines.clear();
    }
}

pub struct GuiState {
    pub(crate) tabs: Vec<TabState>,
    pub(crate) titles: Rc<VecModel<SharedString>>,
    pub(crate) current: usize,
    pub(crate) next_id: u64,
    pub(crate) tx: mpsc::Sender<TerminalChunk>,
    pub(crate) pending_scroll: bool,
    pub(crate) workspace_file_cache: Vec<String>,
    pub(crate) workspace_file_cache_root: Option<PathBuf>,
    pub(crate) at_picker_query_snapshot: String,
    pub(crate) at_picker_open_snapshot: bool,
    /// When unchanged, skip composer + `@` picker timer work (avoids heavy UI reads each tick).
    pub(crate) timer_prompt_snapshot: Option<(usize, String, bool)>,
}

#[derive(Debug)]
pub struct TerminalChunk {
    pub(crate) tab_id: u64,
    pub(crate) text: String,
    pub(crate) lines: Vec<ColoredLine>,
    pub(crate) replace: bool,
    pub(crate) set_auto_scroll: Option<bool>,
}
