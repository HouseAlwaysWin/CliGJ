use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::mpsc;
use std::collections::{HashMap, HashSet};
use std::io::Write;
use std::time::{SystemTime, UNIX_EPOCH};

use slint::{Image, SharedString, VecModel};

use cligj_terminal::types::{RawPtyEvent, RawPtyMode, ReaderRenderMode, ControlCommand};
use cligj_terminal::replay::replay_raw_pty_events;
use cligj_terminal::render::ColoredLine;
use super::interactive_commands::InteractiveCommandSpec;
use super::slint_ui::TermLine;

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

/// Workspace root for `@` picker: explicit tab folder when set and exists; else profile workspace; else [`workspace_root_for_tab`].
pub(crate) fn workspace_root_for_tab_with_profile(tab: &TabState, gs: &GuiState) -> PathBuf {
    let t = tab.file_path.trim();
    if !t.is_empty() {
        let p = PathBuf::from(t);
        if p.is_dir() {
            return p;
        }
    }
    let cmd = tab.cmd_type.as_str();
    for (name, _, w) in &gs.shell_profiles {
        if name == cmd {
            let w = w.trim();
            if !w.is_empty() {
                let pb = PathBuf::from(w);
                if pb.is_dir() {
                    return pb;
                }
            }
            break;
        }
    }
    workspace_root_for_tab(tab)
}

/// ConPTY initial directory: tab `file_path` when it is an existing directory; else shell profile workspace (if any).
pub(crate) fn conpty_startup_cwd(tab: &TabState, gs: &GuiState) -> Option<PathBuf> {
    let t = tab.file_path.trim();
    if !t.is_empty() {
        let p = PathBuf::from(t);
        if p.is_dir() {
            return Some(p);
        }
    }
    super::shell_profiles::startup_cwd_for_shell_profile(tab.cmd_type.as_str(), gs)
}

/// Composer-attached images: absolute path (sent on submit) + thumbnail for UI.
pub(crate) struct PromptImageAttach {
    pub abs_path: String,
    pub preview: Image,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct PromptFileOrigin {
    pub client_id: String,
    pub uri_scheme: String,
}

pub(crate) fn default_cmd_type() -> &'static str {
    if cfg!(target_os = "windows") {
        "Command Prompt"
    } else {
        "Shell"
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TerminalMode {
    Shell,
    InteractiveAi,
}

pub struct TabState {
    pub(crate) id: u64,
    pub(crate) file_path: String,
    pub(crate) prompt_picked_images: Vec<PromptImageAttach>,
    pub(crate) selected_line: i32,
    pub(crate) selected_context: SharedString,
    pub(crate) prompt: SharedString,
    /// Composer-only undo/redo stack (kept outside Slint TextInput to avoid byte-boundary panic).
    pub(crate) prompt_undo_stack: Vec<String>,
    pub(crate) prompt_redo_stack: Vec<String>,
    pub(crate) cmd_type: String,
    pub(crate) terminal_mode: TerminalMode,
    pub(crate) terminal_text: String,
    pub(crate) auto_scroll: bool,
    pub(crate) terminal_select_mode: bool,
    pub(crate) raw_input_mode: bool,
    pub(crate) command_history: Vec<String>,
    pub(crate) history_cursor: Option<usize>,
    pub(crate) history_draft: String,
    /// Absolute paths selected from `@` picker; shown as chips, appended on submit.
    pub(crate) prompt_picked_files_abs: Vec<String>,
    /// Per-file origin metadata for jump-back to the editor instance that attached the file.
    pub(crate) prompt_picked_file_origins: Vec<Option<PromptFileOrigin>>,
    /// Last known editor origin seen from IPC payloads on this tab (fallback for local-only file chips).
    pub(crate) prompt_last_file_origin: Option<PromptFileOrigin>,
    /// Hidden payload blocks from IPC tokens like `[[sel1]]`, expanded on submit.
    pub(crate) prompt_picked_selections: Vec<String>,
    /// VT-colored screen lines (ConPTY + wezterm-term); empty => plain `TextEdit` fallback.
    pub(crate) terminal_lines: Vec<ColoredLine>,
    /// Archived prior screens for Interactive AI tabs.
    pub(crate) interactive_history_lines: Vec<ColoredLine>,
    /// Current visible frame for Interactive AI tabs; `terminal_lines` stores scrollback + frame.
    pub(crate) interactive_frame_lines: Vec<ColoredLine>,
    /// Deduplicate archived Interactive AI frames so redraws do not keep appending the same screen.
    pub(crate) interactive_last_archived_signature: String,
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
    /// After `load_tab_to_ui`, the next PTY-driven push should sync scroll with computed px (Slint may
    /// still hold the previous tab's viewport until the first terminal frame).
    pub(crate) terminal_scroll_resync_next: bool,
    /// Scroll offset (px, content top) when this tab was last shown; restored on tab switch. All tabs
    /// share one Slint `ScrollView`, so we must persist this per tab.
    pub(crate) terminal_saved_scroll_top_px: f32,
    /// Interactive AI tabs follow the newest frame until the user manually scrolls up.
    pub(crate) interactive_follow_output: bool,
    /// Manually configured count of terminal rows to keep fixed at the bottom.
    pub(crate) terminal_pinned_footer_lines: usize,
    /// Per-tab override from the prompt `Pin` input; when present it takes precedence over launcher defaults.
    pub(crate) terminal_pinned_footer_override: Option<usize>,
    /// Normalized launcher program name for the current interactive tab, used for live config updates.
    pub(crate) interactive_launcher_program: String,
    /// Case-insensitive visible-text markers that identify the active Interactive AI screen.
    pub(crate) interactive_markers: Vec<String>,
    /// Archive rewritten viewport frames for TUIs that redraw instead of emitting scrollback.
    pub(crate) interactive_archive_repainted_frames: bool,
    /// Append-only raw PTY event log for replay/debugging. This records bytes/control events before
    /// GUI-level interpretation, so it survives render-mode filtering and resize heuristics.
    pub(crate) raw_pty_events: Vec<RawPtyEvent>,
    pub(crate) raw_pty_event_bytes: usize,
    /// Last PTY grid size actually sent to this tab. Avoid same-size resize on tab switch because
    /// many CLIs/TUIs treat it as a redraw and pollute scrollback with duplicate frames.
    pub(crate) last_pty_cols: u16,
    pub(crate) last_pty_rows: u16,

    pub(crate) pty_process: Option<Box<dyn cligj_terminal::pty::PtyProcess>>,
    pub(crate) pty_writer: Option<Box<dyn cligj_terminal::pty::PtyWriter>>,
    pub(crate) pty_control_tx: Option<mpsc::Sender<ControlCommand>>,
}

/// Hard cap on VT rows kept client-side; oldest lines are discarded first (see `enforce_scrollback_cap`).
pub(crate) const TERMINAL_SCROLLBACK_CAP: usize = 1200;
pub(crate) const RAW_PTY_EVENT_CAP: usize = 8192;
pub(crate) const RAW_PTY_BYTE_CAP: usize = 4 * 1024 * 1024;

pub(crate) struct RawPtyDumpResult {
    pub(crate) dir: PathBuf,
    pub(crate) raw_path: PathBuf,
    pub(crate) index_path: PathBuf,
    pub(crate) escaped_path: PathBuf,
    pub(crate) live_visible_path: PathBuf,
    pub(crate) live_full_path: PathBuf,
    pub(crate) meta_path: PathBuf,
    pub(crate) event_count: usize,
    pub(crate) byte_count: usize,
}

pub(crate) struct RawPtyReplayDumpResult {
    pub(crate) dir: PathBuf,
    pub(crate) live_visible_path: PathBuf,
    pub(crate) live_full_path: PathBuf,
    pub(crate) replay_visible_path: PathBuf,
    pub(crate) replay_tail_2x_path: PathBuf,
    pub(crate) replay_active_viewport_path: PathBuf,
    pub(crate) replay_full_path: PathBuf,
    pub(crate) meta_path: PathBuf,
    pub(crate) event_count: usize,
    pub(crate) byte_count: usize,
}

fn escaped_bytes_for_debug(bytes: &[u8]) -> String {
    let mut out = String::new();
    for &byte in bytes {
        match byte {
            b'\n' => out.push_str("\\n\n"),
            b'\r' => out.push_str("\\r"),
            b'\t' => out.push_str("\\t"),
            0x1b => out.push_str("\\x1b"),
            0x20..=0x7e => out.push(byte as char),
            _ => out.push_str(format!("\\x{byte:02x}").as_str()),
        }
    }
    out
}

fn colored_lines_plain_text(lines: &[ColoredLine]) -> String {
    let mut out = String::new();
    for line in lines {
        for span in &line.spans {
            out.push_str(span.text.as_str());
        }
        out.push('\n');
    }
    out
}

fn live_visible_plain_text(tab: &TabState) -> String {
    if tab.terminal_lines.is_empty() {
        return tab.terminal_text.clone();
    }
    if tab.last_window_first == usize::MAX || tab.last_window_last == usize::MAX {
        return colored_lines_plain_text(&tab.terminal_lines);
    }
    let first = tab.last_window_first.min(tab.terminal_lines.len());
    let last = tab.last_window_last.min(tab.terminal_lines.len().saturating_sub(1));
    if first > last || first >= tab.terminal_lines.len() {
        return String::new();
    }
    colored_lines_plain_text(&tab.terminal_lines[first..=last])
}

impl TabState {
    pub fn new(id: u64, tx: mpsc::Sender<TerminalChunk>, startup_cwd: Option<PathBuf>) -> Self {
        let cmd_type = default_cmd_type().to_string();
        let mut me = Self {
            id,
            file_path: String::new(),
            prompt_picked_images: Vec::new(),
            selected_line: 0,
            selected_context: SharedString::new(),
            prompt: SharedString::new(),
            prompt_undo_stack: Vec::new(),
            prompt_redo_stack: Vec::new(),
            cmd_type,
            terminal_mode: TerminalMode::Shell,
            terminal_text: String::new(),
            auto_scroll: false,
            terminal_select_mode: false,
            raw_input_mode: false,
            command_history: Vec::new(),
            history_cursor: None,
            history_draft: String::new(),
            prompt_picked_files_abs: Vec::new(),
            prompt_picked_file_origins: Vec::new(),
            prompt_last_file_origin: None,
            prompt_picked_selections: Vec::new(),
            terminal_lines: Vec::new(),
            interactive_history_lines: Vec::new(),
            interactive_frame_lines: Vec::new(),
            interactive_last_archived_signature: String::new(),
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
            terminal_scroll_resync_next: false,
            terminal_saved_scroll_top_px: 0.0,
            interactive_follow_output: true,
            terminal_pinned_footer_lines: 0,
            terminal_pinned_footer_override: None,
            interactive_launcher_program: String::new(),
            interactive_markers: Vec::new(),
            interactive_archive_repainted_frames: false,
            raw_pty_events: Vec::new(),
            raw_pty_event_bytes: 0,
            last_pty_cols: 120,
            last_pty_rows: 40,
            pty_process: None,
            pty_writer: None,
            pty_control_tx: None,
        };

        #[cfg(target_os = "windows")]
        {
            use cligj_terminal::windows_conpty;
            use cligj_terminal::session;

            if me.cmd_type == "Command Prompt" || me.cmd_type == "PowerShell" {
                if let Ok(pty) = windows_conpty::spawn_conpty(
                    &me.cmd_type,
                    120,
                    40,
                    startup_cwd.as_ref().map(|p| p.as_path()),
                ) {
                    let tab_id = me.id;
                    let (handle, control_tx, process, writer) = session::start_terminal_session(
                        pty,
                        ReaderRenderMode::Shell,
                        move |render| {
                        let _ = tx.send(TerminalChunk {
                            tab_id,
                            terminal_mode: match render.render_mode {
                                ReaderRenderMode::Shell => TerminalMode::Shell,
                                ReaderRenderMode::InteractiveAi => TerminalMode::InteractiveAi,
                            },
                            raw_pty_events: render.raw_pty_events,
                            text: render.text,
                            lines: render.lines,
                            snapshot_len: render.snapshot_len,
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
                    let _ = handle; 
                    me.pty_process = Some(process);
                    me.pty_writer = Some(writer);
                    me.pty_control_tx = Some(control_tx);
                }
            }
        }

        me
    }

    pub fn enforce_scrollback_cap(&mut self) {
        if self.terminal_lines.len() <= TERMINAL_SCROLLBACK_CAP {
            return;
        }
        let excess = self.terminal_lines.len() - TERMINAL_SCROLLBACK_CAP;
        self.terminal_lines.drain(0..excess);
        self.terminal_physical_origin = self.terminal_physical_origin.saturating_add(excess);
        let hist_excess = excess.min(self.interactive_history_lines.len());
        if hist_excess > 0 {
            self.interactive_history_lines.drain(0..hist_excess);
        }
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
        self.interactive_history_lines.clear();
        self.interactive_frame_lines.clear();
        self.interactive_last_archived_signature.clear();
        self.interactive_markers.clear();
        self.interactive_archive_repainted_frames = false;
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

    pub fn append_raw_pty_events(&mut self, events: Vec<RawPtyEvent>) {
        if events.is_empty() {
            return;
        }
        for event in events {
            self.raw_pty_event_bytes = self.raw_pty_event_bytes.saturating_add(event.byte_len());
            self.raw_pty_events.push(event);
        }
        while self.raw_pty_events.len() > RAW_PTY_EVENT_CAP
            || self.raw_pty_event_bytes > RAW_PTY_BYTE_CAP
        {
            if self.raw_pty_events.is_empty() {
                self.raw_pty_event_bytes = 0;
                break;
            }
            let removed = self.raw_pty_events.remove(0);
            self.raw_pty_event_bytes =
                self.raw_pty_event_bytes.saturating_sub(removed.byte_len());
        }
    }

    pub fn dump_raw_pty_events(&self, dir: Option<PathBuf>) -> Result<RawPtyDumpResult, String> {
        let dir = dir.unwrap_or_else(|| {
            std::env::current_dir()
                .unwrap_or_else(|_| PathBuf::from("."))
                .join("raw_pty_dumps")
        });
        std::fs::create_dir_all(&dir).map_err(|e| format!("create dump dir: {e}"))?;

        let timestamp_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|e| format!("system time before unix epoch: {e}"))?
            .as_millis();
        let stem = format!("cligj-raw-pty-tab-{}-{timestamp_ms}", self.id);
        let raw_path = dir.join(format!("{stem}.raw.bin"));
        let index_path = dir.join(format!("{stem}.events.jsonl"));
        let escaped_path = dir.join(format!("{stem}.raw_escaped.txt"));
        let live_visible_path = dir.join(format!("{stem}.live_visible.txt"));
        let live_full_path = dir.join(format!("{stem}.live_full.txt"));
        let meta_path = dir.join(format!("{stem}.meta.json"));

        let mut raw_file =
            std::fs::File::create(&raw_path).map_err(|e| format!("create raw dump: {e}"))?;
        let mut index_file =
            std::fs::File::create(&index_path).map_err(|e| format!("create index dump: {e}"))?;
        let mut escaped_file = std::fs::File::create(&escaped_path)
            .map_err(|e| format!("create escaped dump: {e}"))?;

        let mut offset = 0usize;
        for (idx, event) in self.raw_pty_events.iter().enumerate() {
            match event {
                RawPtyEvent::Bytes(bytes) => {
                    raw_file
                        .write_all(bytes)
                        .map_err(|e| format!("write raw bytes: {e}"))?;
                    let line = serde_json::json!({
                        "event_index": idx,
                        "kind": "bytes",
                        "offset": offset,
                        "len": bytes.len(),
                    });
                    writeln!(index_file, "{line}")
                        .map_err(|e| format!("write index bytes event: {e}"))?;
                    writeln!(
                        escaped_file,
                        "\n--- event {idx}: bytes offset={offset} len={} ---\n{}",
                        bytes.len(),
                        escaped_bytes_for_debug(bytes)
                    )
                    .map_err(|e| format!("write escaped bytes event: {e}"))?;
                    offset = offset.saturating_add(bytes.len());
                }
                RawPtyEvent::Resize { cols, rows } => {
                    let line = serde_json::json!({
                        "event_index": idx,
                        "kind": "resize",
                        "cols": cols,
                        "rows": rows,
                    });
                    writeln!(index_file, "{line}")
                        .map_err(|e| format!("write index resize event: {e}"))?;
                    writeln!(escaped_file, "\n--- event {idx}: resize cols={cols} rows={rows} ---")
                        .map_err(|e| format!("write escaped resize event: {e}"))?;
                }
                RawPtyEvent::RenderMode { mode } => {
                    let mode = match mode {
                        RawPtyMode::Shell => "shell",
                        RawPtyMode::InteractiveAi => "interactive_ai",
                    };
                    let line = serde_json::json!({
                        "event_index": idx,
                        "kind": "render_mode",
                        "mode": mode,
                    });
                    writeln!(index_file, "{line}")
                        .map_err(|e| format!("write index render mode event: {e}"))?;
                    writeln!(escaped_file, "\n--- event {idx}: render_mode mode={mode} ---")
                        .map_err(|e| format!("write escaped render mode event: {e}"))?;
                }
            }
        }

        let live_visible_text = live_visible_plain_text(self);
        std::fs::write(&live_visible_path, live_visible_text)
            .map_err(|e| format!("write live visible dump: {e}"))?;

        let live_full_text = if self.terminal_lines.is_empty() {
            self.terminal_text.clone()
        } else {
            colored_lines_plain_text(&self.terminal_lines)
        };
        std::fs::write(&live_full_path, live_full_text)
            .map_err(|e| format!("write live full dump: {e}"))?;

        let meta = serde_json::json!({
            "tabId": self.id,
            "eventCount": self.raw_pty_events.len(),
            "byteCount": offset,
            "rawPath": raw_path,
            "indexPath": index_path,
            "escapedPath": escaped_path,
            "liveVisiblePath": live_visible_path,
            "liveFullPath": live_full_path,
        });
        std::fs::write(
            &meta_path,
            serde_json::to_string_pretty(&meta)
                .map_err(|e| format!("serialize raw dump meta: {e}"))?,
        )
        .map_err(|e| format!("write raw dump meta: {e}"))?;

        Ok(RawPtyDumpResult {
            dir,
            raw_path,
            index_path,
            escaped_path,
            live_visible_path,
            live_full_path,
            meta_path,
            event_count: self.raw_pty_events.len(),
            byte_count: offset,
        })
    }

    pub fn dump_debug_replay(&self, dir: Option<PathBuf>) -> Result<RawPtyReplayDumpResult, String> {
        let dir = dir.unwrap_or_else(|| {
            std::env::current_dir()
                .unwrap_or_else(|_| PathBuf::from("."))
                .join("raw_pty_replays")
        });
        std::fs::create_dir_all(&dir).map_err(|e| format!("create replay dir: {e}"))?;

        let timestamp_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|e| format!("system time before unix epoch: {e}"))?
            .as_millis();
        let stem = format!("cligj-replay-tab-{}-{timestamp_ms}", self.id);
        let live_visible_path = dir.join(format!("{stem}.live_visible.txt"));
        let live_full_path = dir.join(format!("{stem}.live_full.txt"));
        let replay_visible_path = dir.join(format!("{stem}.replay_visible.txt"));
        let replay_tail_2x_path = dir.join(format!("{stem}.replay_tail_2x.txt"));
        let replay_active_viewport_path = dir.join(format!("{stem}.replay_active_viewport.txt"));
        let replay_full_path = dir.join(format!("{stem}.replay_full.txt"));
        let meta_path = dir.join(format!("{stem}.meta.json"));

        let live_visible_text = live_visible_plain_text(self);
        std::fs::write(&live_visible_path, live_visible_text)
            .map_err(|e| format!("write live visible dump: {e}"))?;

        let live_full_text = if self.terminal_lines.is_empty() {
            self.terminal_text.clone()
        } else {
            colored_lines_plain_text(&self.terminal_lines)
        };
        std::fs::write(&live_full_path, live_full_text)
            .map_err(|e| format!("write live full dump: {e}"))?;

        let replay = replay_raw_pty_events(&self.raw_pty_events)?;
        std::fs::write(&replay_visible_path, replay.visible_text.as_str())
            .map_err(|e| format!("write replay visible dump: {e}"))?;
        std::fs::write(&replay_tail_2x_path, replay.tail_2x_text.as_str())
            .map_err(|e| format!("write replay tail 2x dump: {e}"))?;
        std::fs::write(&replay_active_viewport_path, replay.active_viewport_text.as_str())
            .map_err(|e| format!("write replay active viewport dump: {e}"))?;
        std::fs::write(&replay_full_path, replay.full_text.as_str())
            .map_err(|e| format!("write replay full dump: {e}"))?;

        let final_mode = match replay.final_mode {
            RawPtyMode::Shell => "shell",
            RawPtyMode::InteractiveAi => "interactive_ai",
        };
        let meta = serde_json::json!({
            "tabId": self.id,
            "eventCount": self.raw_pty_events.len(),
            "byteCount": self.raw_pty_event_bytes,
            "finalMode": final_mode,
            "cols": replay.cols,
            "rows": replay.rows,
            "totalRows": replay.total_rows,
            "visibleStart": replay.visible_start,
            "tail2xStart": replay.tail_2x_start,
            "renderStart": replay.render_start,
            "liveVisiblePath": live_visible_path,
            "liveFullPath": live_full_path,
            "replayVisiblePath": replay_visible_path,
            "replayTail2xPath": replay_tail_2x_path,
            "replayActiveViewportPath": replay_active_viewport_path,
            "replayFullPath": replay_full_path,
        });
        std::fs::write(
            &meta_path,
            serde_json::to_string_pretty(&meta)
                .map_err(|e| format!("serialize replay meta: {e}"))?,
        )
        .map_err(|e| format!("write replay meta: {e}"))?;

        Ok(RawPtyReplayDumpResult {
            dir,
            live_visible_path,
            live_full_path,
            replay_visible_path,
            replay_tail_2x_path,
            replay_active_viewport_path,
            replay_full_path,
            meta_path,
            event_count: self.raw_pty_events.len(),
            byte_count: self.raw_pty_event_bytes,
        })
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
    pub(crate) timer_snapshot: Option<(usize, String, bool)>,
    /// From config `[[ui.interactive_commands]]`.
    pub(crate) interactive_commands: Vec<InteractiveCommandSpec>,
    /// Top-right terminal picker profiles from `[[ui.shell_profiles]]`: name, command, optional workspace root.
    pub(crate) shell_profiles: Vec<(String, String, String)>,
    /// Startup page setting: preferred UI language label (currently persisted only).
    pub(crate) startup_language: String,
    /// Startup page setting: default shell profile for newly created tabs.
    pub(crate) startup_default_shell_profile: String,
    /// Startup page setting: terminal font family used by the in-app terminal viewer.
    pub(crate) startup_terminal_font_family: String,
    /// Startup page setting: CJK fallback font used when the main terminal font lacks glyphs.
    pub(crate) startup_terminal_cjk_fallback_font_family: String,
}

#[derive(Debug)]
pub struct TerminalChunk {
    pub(crate) tab_id: u64,
    pub(crate) terminal_mode: TerminalMode,
    pub(crate) raw_pty_events: Vec<RawPtyEvent>,
    pub(crate) text: String,
    pub(crate) lines: Vec<ColoredLine>,
    /// Number of physical rows covered by this snapshot window.
    pub(crate) snapshot_len: usize,
    /// Total physical rows known by the terminal emulator.
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
