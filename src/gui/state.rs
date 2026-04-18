use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::mpsc;
use std::collections::{HashMap, HashSet};

use slint::{Image, SharedString, VecModel};

use crate::terminal::render::ColoredLine;
use super::slint_ui::TermLine;

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

/// Composer-attached images: absolute path (sent on submit) + thumbnail for UI.
pub(crate) struct PromptImageAttach {
    pub abs_path: String,
    pub preview: Image,
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
    pub(crate) prompt_picked_images: Vec<PromptImageAttach>,
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
    /// Cached converted Slint rows + fingerprints to avoid rebuilding unchanged lines.
    pub(crate) terminal_model_rows: HashMap<usize, TermLine>,
    pub(crate) terminal_model_hashes: HashMap<usize, u64>,
    /// 哪些行在上次 push 之後有變化，需要 set_row_data
    pub(crate) terminal_model_dirty: HashSet<usize>,
    /// 持久 Slint terminal model — 用 set_row_data 差異更新
    pub(crate) terminal_slint_model: Rc<VecModel<TermLine>>,
    /// Last scroll position used for terminal windowing (px, content top).
    pub(crate) terminal_scroll_top_px: f32,
    /// Viewport height in px (for row windowing).
    pub(crate) terminal_view_height_px: f32,
    /// Last scroll top pushed to Slint (for detecting user scroll without terminal output).
    pub(crate) last_pushed_scroll_top: f32,
    /// Last viewport height used when pushing the terminal window.
    pub(crate) last_pushed_viewport_height: f32,
    /// Last pushed global first row index for sliced terminal model.
    pub(crate) last_window_first: usize,
    /// Last pushed global last row index for sliced terminal model.
    pub(crate) last_window_last: usize,
    /// Last pushed full terminal line count.
    pub(crate) last_window_total: usize,
    /// Last `prompt` string written to ConPTY while `@` is active (composer → shell line sync).
    pub(crate) composer_pty_mirror: String,
    /// Local row index into `terminal_lines` for the cell cursor (matches overlay in `ui_sync`).
    pub(crate) terminal_cursor_row: Option<usize>,
    pub(crate) terminal_cursor_col: Option<usize>,
    /// PTY physical line index of `terminal_lines[0]`; rows below were dropped for scrollback cap.
    pub(crate) terminal_physical_origin: usize,

    #[cfg(target_os = "windows")]
    pub(crate) conpty: Option<windows_conpty::ConptySession>,
    #[cfg(target_os = "windows")]
    pub(crate) conpty_control_tx: Option<mpsc::Sender<windows_conpty::ControlCommand>>,
}

/// Hard cap on VT rows kept client-side; oldest lines are discarded first (see `enforce_scrollback_cap`).
pub(crate) const TERMINAL_SCROLLBACK_CAP: usize = 1200;

impl TabState {
    pub fn new(id: u64, tx: mpsc::Sender<TerminalChunk>) -> Self {
        let cmd_type = default_cmd_type().to_string();
        let mut me = Self {
            id,
            file_path: String::new(),
            prompt_picked_images: Vec::new(),
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
            terminal_model_rows: HashMap::new(),
            terminal_model_hashes: HashMap::new(),
            terminal_model_dirty: HashSet::new(),
            terminal_slint_model: Rc::new(VecModel::default()),
            terminal_scroll_top_px: 0.0,
            terminal_view_height_px: 600.0,
            last_pushed_scroll_top: -1.0,
            last_pushed_viewport_height: -1.0,
            last_window_first: usize::MAX,
            last_window_last: usize::MAX,
            last_window_total: usize::MAX,
            composer_pty_mirror: String::new(),
            terminal_cursor_row: None,
            terminal_cursor_col: None,
            terminal_physical_origin: 0,
            #[cfg(target_os = "windows")]
            conpty: None,
            #[cfg(target_os = "windows")]
            conpty_control_tx: None,
        };

        #[cfg(target_os = "windows")]
        {
            if me.cmd_type == "Command Prompt" || me.cmd_type == "PowerShell" {
                if let Ok(spawn) = windows_conpty::spawn_conpty(&me.cmd_type, 120, 40) {
                    let tab_id = me.id;
                    let (control_tx, control_rx) = mpsc::channel();
                    windows_conpty::start_reader_thread(spawn.reader, control_rx, move |render| {
                        let _ = tx.send(TerminalChunk {
                            tab_id,
                            text: render.text,
                            lines: render.lines,
                            full_len: render.full_len,
                            first_line_idx: render.first_line_idx,
                            cursor_row: render.cursor_row,
                            cursor_col: render.cursor_col,
                            replace: true,
                            set_auto_scroll: if render.filled { Some(true) } else { None },
                            changed_indices: render.changed_indices,
                            reset_terminal_buffer: render.reset_terminal_buffer,
                        });
                    });
                    me.conpty = Some(spawn.session);
                    me.conpty_control_tx = Some(control_tx);
                }
            }
        }

        me
    }

    /// Drop oldest lines when the buffer exceeds `TERMINAL_SCROLLBACK_CAP`.
    pub fn enforce_scrollback_cap(&mut self) {
        if self.terminal_lines.len() <= TERMINAL_SCROLLBACK_CAP {
            return;
        }
        let excess = self.terminal_lines.len() - TERMINAL_SCROLLBACK_CAP;
        self.terminal_lines.drain(0..excess);
        self.terminal_physical_origin = self.terminal_physical_origin.saturating_add(excess);
        self.terminal_model_rows.clear();
        self.terminal_model_hashes.clear();
        self.terminal_model_dirty.clear();
        self.last_window_first = usize::MAX;
        self.last_window_last = usize::MAX;
        self.last_window_total = usize::MAX;
        self.terminal_cursor_row = self.terminal_cursor_row.and_then(|c| {
            if c >= excess {
                Some(c - excess)
            } else {
                None
            }
        });
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
        self.terminal_model_rows.clear();
        self.terminal_model_hashes.clear();
        self.terminal_model_dirty.clear();
        self.last_window_first = usize::MAX;
        self.last_window_last = usize::MAX;
        self.last_window_total = usize::MAX;
        self.terminal_cursor_row = None;
        self.terminal_cursor_col = None;
        self.terminal_physical_origin = 0;
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
    pub(crate) full_len: usize,
    pub(crate) first_line_idx: usize,
    pub(crate) cursor_row: Option<usize>,
    pub(crate) cursor_col: Option<usize>,
    pub(crate) replace: bool,
    pub(crate) set_auto_scroll: Option<bool>,
    /// Which line indices changed (from reader thread diff); empty = treat all as changed.
    pub(crate) changed_indices: Vec<usize>,
    /// Drop GUI scrollback and re-apply the next snapshot (e.g. after PTY resize).
    pub(crate) reset_terminal_buffer: bool,
}
