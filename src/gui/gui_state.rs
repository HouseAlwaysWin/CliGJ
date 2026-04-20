use std::io::Write;
use std::time::Duration;

use slint::{Model, SharedString};

#[cfg(target_os = "windows")]
use crate::terminal::windows_conpty;
#[cfg(target_os = "windows")]
use crate::terminal::windows_conpty::ReaderRenderMode;

use super::composer_sync::diff_composer_to_conpty;
use super::shell_profiles::resolve_shell_command_line;
use super::slint_ui::AppWindow;
use super::state::{GuiState, TerminalChunk, TerminalMode};
use super::ui_sync::{load_tab_to_ui, sync_tab_count, tab_update_from_ui};

impl GuiState {
    pub(crate) fn prepare_current_tab_for_interactive_ai(&mut self) {
        if self.current >= self.tabs.len() {
            return;
        }
        let tab = &mut self.tabs[self.current];
        tab.terminal_mode = TerminalMode::InteractiveAi;
        tab.interactive_history_lines.clear();
        tab.interactive_frame_lines.clear();
        tab.interactive_last_archived_signature.clear();
        tab.terminal_lines.clear();
        tab.terminal_text.clear();
        tab.terminal_model_rows.clear();
        tab.terminal_model_hashes.clear();
        tab.terminal_model_dirty.clear();
        tab.terminal_physical_origin = 0;
        tab.terminal_cursor_row = None;
        tab.terminal_cursor_col = None;
        tab.last_window_first = usize::MAX;
        tab.last_window_last = usize::MAX;
        tab.last_window_total = usize::MAX;
        tab.terminal_saved_scroll_top_px = 0.0;
        tab.terminal_scroll_resync_next = true;
        tab.auto_scroll = true;
        tab.interactive_follow_output = true;
        if let Some(tx) = &tab.conpty_control_tx {
            let _ = tx.send(windows_conpty::ControlCommand::SetRenderMode(
                ReaderRenderMode::InteractiveAi,
            ));
        }
    }

    /// Drop the shell, spawn a fresh ConPTY, and reset the terminal buffer for Interactive AI.
    /// Required when switching launcher commands so input goes to a new process.
    pub(crate) fn respawn_conpty_for_interactive_command(
        &mut self,
        ui: &AppWindow,
    ) -> Result<(), &'static str> {
        if self.current >= self.tabs.len() {
            return Err("invalid current tab index");
        }
        let cmd_type = self.tabs[self.current].cmd_type.clone();
        let startup_cmd = resolve_shell_command_line(cmd_type.as_str(), self);

        #[cfg(target_os = "windows")]
        {
            self.tabs[self.current].conpty = None;
            self.tabs[self.current].conpty_control_tx = None;
        }

        {
            let tab = &mut self.tabs[self.current];
            // Force next bump_terminal_size → Resize to reach the new PTY (was matching old 120×40).
            tab.last_pty_cols = 0;
            tab.last_pty_rows = 0;
            tab.last_pushed_scroll_top = -1.0;
            tab.last_pushed_viewport_height = -1.0;
        }

        self.prepare_current_tab_for_interactive_ai();

        {
            let tab = &mut self.tabs[self.current];
            while tab.terminal_slint_model.row_count() > 0 {
                tab.terminal_slint_model.remove(0);
            }
            tab.interactive_last_archived_signature.clear();
        }

        #[cfg(target_os = "windows")]
        {
            use std::rc::Rc;

            use slint::ModelRc;

            if let Some(startup_cmd) = startup_cmd {
                match windows_conpty::spawn_conpty_command_line(startup_cmd.as_str(), 120, 40) {
                    Ok(spawn) => {
                        let tab_id = self.tabs[self.current].id;
                        let tx = self.tx.clone();
                        let (control_tx, control_rx) = std::sync::mpsc::channel();
                        windows_conpty::start_reader_thread(
                            spawn.reader,
                            control_rx,
                            ReaderRenderMode::InteractiveAi,
                            move |render| {
                                let _ = tx.send(TerminalChunk {
                                    tab_id,
                                    terminal_mode: match render.render_mode {
                                        ReaderRenderMode::Shell => TerminalMode::Shell,
                                        ReaderRenderMode::InteractiveAi => TerminalMode::InteractiveAi,
                                    },
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
                            },
                        );
                        let tab = &mut self.tabs[self.current];
                        tab.conpty = Some(spawn.session);
                        tab.conpty_control_tx = Some(control_tx);
                    }
                    Err(e) => eprintln!("CliGJ: spawn_conpty (interactive): {e}"),
                }
            }

            let tab = &mut self.tabs[self.current];
            ui.set_ws_terminal_text(SharedString::new());
            ui.set_ws_terminal_line_offset(0);
            ui.set_ws_terminal_total_lines(0);
            ui.set_ws_terminal_lines(ModelRc::from(Rc::clone(&tab.terminal_slint_model)));
            ui.invoke_ws_scroll_terminal_to_top();
        }

        #[cfg(not(target_os = "windows"))]
        {
            use std::rc::Rc;

            use slint::ModelRc;

            let tab = &mut self.tabs[self.current];
            ui.set_ws_terminal_text(SharedString::new());
            ui.set_ws_terminal_line_offset(0);
            ui.set_ws_terminal_total_lines(0);
            ui.set_ws_terminal_lines(ModelRc::from(Rc::clone(&tab.terminal_slint_model)));
            ui.invoke_ws_scroll_terminal_to_top();
        }

        // Do not read scroll position from the UI here: after clearing, Slint may still report the
        // previous tab's offset for one frame — that would overwrite `prepare_*`'s 0 and balloon
        // the scrollbar extent until the next PTY frame.
        self.tabs[self.current].terminal_saved_scroll_top_px = 0.0;
        load_tab_to_ui(ui, &mut self.tabs[self.current]);
        Ok(())
    }

    pub(crate) fn toggle_raw_input_current(
        &mut self,
        ui: &AppWindow,
    ) -> Result<(), &'static str> {
        if self.current >= self.tabs.len() {
            return Err("invalid current tab index");
        }
        tab_update_from_ui(&mut self.tabs[self.current], ui);
        let tab = &mut self.tabs[self.current];
        tab.raw_input_mode = !tab.raw_input_mode;
        if tab.raw_input_mode {
            #[cfg(target_os = "windows")]
            {
                let prev = std::mem::take(&mut tab.composer_pty_mirror);
                if !prev.is_empty() {
                    let bytes = diff_composer_to_conpty(prev.as_str(), "");
                    if !bytes.is_empty() {
                        if let Some(session) = tab.conpty.as_mut() {
                            let _ = session.writer.write_all(&bytes);
                            let _ = session.writer.flush();
                        }
                    }
                }
            }
            tab.prompt = SharedString::new();
            tab.prompt_picked_files_abs.clear();
            tab.prompt_picked_images.clear();
        }
        tab.terminal_saved_scroll_top_px = ui.get_ws_terminal_scroll_top_px();
        load_tab_to_ui(ui, tab);
        Ok(())
    }

    pub(crate) fn switch_tab(&mut self, new_index: usize, ui: &AppWindow) -> Result<(), &'static str> {
        if new_index >= self.tabs.len() {
            return Err("invalid tab index");
        }
        if new_index == self.current {
            return Ok(());
        }

        tab_update_from_ui(&mut self.tabs[self.current], ui);
        self.tabs[self.current].terminal_saved_scroll_top_px = ui.get_ws_terminal_scroll_top_px();
        self.current = new_index;
        ui.set_current_tab(new_index as i32);
        load_tab_to_ui(ui, &mut self.tabs[new_index]);
        Ok(())
    }

    pub(crate) fn add_tab(&mut self, ui: &AppWindow) -> Result<(), &'static str> {
        tab_update_from_ui(&mut self.tabs[self.current], ui);
        self.tabs[self.current].terminal_saved_scroll_top_px = ui.get_ws_terminal_scroll_top_px();
        let startup_profile = self.startup_default_shell_profile.clone();

        let n = self.titles.row_count();
        let label = SharedString::from(format!("Session {}", n + 1));
        self.titles.push(label);
        let id = self.next_id;
        self.next_id += 1;
        self.tabs.push(super::state::TabState::new(id, self.tx.clone()));

        let new_index = self.tabs.len() - 1;
        self.current = new_index;
        ui.set_current_tab(new_index as i32);
        sync_tab_count(ui, self.tabs.len());
        if !startup_profile.trim().is_empty() {
            self.tabs[new_index].cmd_type = startup_profile.clone();
        }
        load_tab_to_ui(ui, &mut self.tabs[new_index]);
        if self.tabs[new_index].cmd_type != "Command Prompt" {
            let _ = self.change_current_cmd_type(startup_profile.as_str(), ui);
        }
        Ok(())
    }

    pub(crate) fn change_current_cmd_type(
        &mut self,
        new_cmd_type: &str,
        ui: &AppWindow,
    ) -> Result<(), &'static str> {
        if self.current >= self.tabs.len() {
            return Err("invalid current tab index");
        }
        tab_update_from_ui(&mut self.tabs[self.current], ui);
        self.tabs[self.current].cmd_type = new_cmd_type.to_string();
        self.tabs[self.current].terminal_mode = TerminalMode::Shell;

        #[cfg(target_os = "windows")]
        {
            self.tabs[self.current].conpty = None;
            self.tabs[self.current].conpty_control_tx = None;
            self.tabs[self.current].terminal_text.clear();
            self.tabs[self.current].interactive_frame_lines.clear();
            self.tabs[self.current].auto_scroll = false;
            self.tabs[self.current].composer_pty_mirror.clear();
            if let Some(startup_cmd) = resolve_shell_command_line(new_cmd_type, self) {
                if let Ok(spawn) = windows_conpty::spawn_conpty_command_line(startup_cmd.as_str(), 120, 40) {
                    let tab_id = self.tabs[self.current].id;
                    let tx = self.tx.clone();
                    let (control_tx, control_rx) = std::sync::mpsc::channel();
                    windows_conpty::start_reader_thread(
                        spawn.reader,
                        control_rx,
                        ReaderRenderMode::Shell,
                        move |render| {
                        let _ = tx.send(TerminalChunk {
                            tab_id,
                            terminal_mode: match render.render_mode {
                                ReaderRenderMode::Shell => TerminalMode::Shell,
                                ReaderRenderMode::InteractiveAi => TerminalMode::InteractiveAi,
                            },
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
                    self.tabs[self.current].conpty = Some(spawn.session);
                    self.tabs[self.current].conpty_control_tx = Some(control_tx);
                }
            }
        }

        ui.set_ws_cmd_type(SharedString::from(new_cmd_type));
        self.tabs[self.current].terminal_saved_scroll_top_px = ui.get_ws_terminal_scroll_top_px();
        load_tab_to_ui(ui, &mut self.tabs[self.current]);
        Ok(())
    }

    pub(crate) fn submit_current_prompt(&mut self, ui: &AppWindow) -> Result<(), &'static str> {
        if self.current >= self.tabs.len() {
            return Err("invalid current tab index");
        }
        tab_update_from_ui(&mut self.tabs[self.current], ui);
        
        let mut extra_payload = String::new();
        {
            let tab = &self.tabs[self.current];
            for path in &tab.prompt_picked_files_abs {
                if !path.is_empty() && !tab.prompt.contains(path) {
                    extra_payload.push(' ');
                    extra_payload.push_str(path);
                }
            }
            for img in &tab.prompt_picked_images {
                if !img.abs_path.is_empty() && !tab.prompt.contains(&img.abs_path) {
                    extra_payload.push(' ');
                    extra_payload.push_str(&img.abs_path);
                }
            }
        }

        let full_command = {
            let tab = &self.tabs[self.current];
            format!("{}{}", tab.prompt, extra_payload).trim().to_string()
        };
        let is_interactive_ai_launch = matches!(
            full_command.split_whitespace().next(),
            Some("gemini" | "codex" | "claude" | "copilot")
        );

        if !full_command.is_empty() {
            let tab = &mut self.tabs[self.current];
            let history = &mut tab.command_history;
            if history.last().map(|s| s.as_str()) != Some(full_command.as_str()) {
                history.push(full_command.clone());
            }
        }

        if is_interactive_ai_launch {
            self.prepare_current_tab_for_interactive_ai();
            ui.set_ws_terminal_text(SharedString::new());
            ui.set_ws_terminal_total_lines(0);
        }

        #[cfg(target_os = "windows")]
        {
            use crate::gui::composer_sync::sync_composer_line_to_conpty;
            // 提交前補齊鏡像同步
            sync_composer_line_to_conpty(ui, self);
            
            let tab = &mut self.tabs[self.current];
            tab.history_cursor = None;
            tab.history_draft.clear();

            if let Some(session) = tab.conpty.as_mut() {
                use std::io::Write;
                if !extra_payload.is_empty() {
                    let _ = session.writer.write_all(extra_payload.as_bytes());
                    let _ = session.writer.flush();
                }
                // Some interactive CLIs on Windows (including Gemini CLI) debounce
                // rapid input to detect paste. If Enter arrives in the same burst as
                // the mirrored prompt text, it can be treated like another pasted
                // newline instead of a submit. Give the TUI a moment, then send CR.
                if !full_command.is_empty() {
                    std::thread::sleep(Duration::from_millis(40));
                }
                let _ = session.writer.write_all(b"\r");
                let _ = session.writer.flush();
            } else if !full_command.is_empty() {
                tab.append_terminal(&format!("{full_command}\n"));
            } else {
                tab.append_terminal("\n");
            }
        }

        #[cfg(not(target_os = "windows"))]
        {
            let tab = &mut self.tabs[self.current];
            tab.history_cursor = None;
            tab.history_draft.clear();
            if !full_command.is_empty() {
                tab.append_terminal(&format!("{full_command}\n"));
            } else {
                tab.append_terminal("\n");
            }
        }

        let tab = &mut self.tabs[self.current];
        tab.prompt = SharedString::new();
        tab.prompt_picked_files_abs.clear();
        tab.prompt_picked_images.clear();
        tab.composer_pty_mirror.clear();
        // 立即更新快照，阻止計時器在下一毫秒發送退格鍵
        self.timer_prompt_snapshot = Some((self.current, String::new(), ui.get_ws_raw_input()));
        tab.terminal_saved_scroll_top_px = ui.get_ws_terminal_scroll_top_px();
        load_tab_to_ui(ui, tab);
        Ok(())
    }

    pub(crate) fn history_prev_current_prompt(
        &mut self,
        ui: &AppWindow,
    ) -> Result<(), &'static str> {
        if self.current >= self.tabs.len() {
            return Err("invalid current tab index");
        }
        tab_update_from_ui(&mut self.tabs[self.current], ui);
        let tab = &mut self.tabs[self.current];
        if tab.command_history.is_empty() {
            return Ok(());
        }

        if tab.history_cursor.is_none() {
            tab.history_draft = tab.prompt.to_string();
            tab.history_cursor = Some(tab.command_history.len());
        }

        if let Some(cur) = tab.history_cursor {
            if cur > 0 {
                let next = cur - 1;
                tab.history_cursor = Some(next);
                tab.prompt = SharedString::from(tab.command_history[next].as_str());
                tab.terminal_saved_scroll_top_px = ui.get_ws_terminal_scroll_top_px();
                load_tab_to_ui(ui, tab);
            }
        }
        Ok(())
    }

    pub(crate) fn history_next_current_prompt(
        &mut self,
        ui: &AppWindow,
    ) -> Result<(), &'static str> {
        if self.current >= self.tabs.len() {
            return Err("invalid current tab index");
        }
        tab_update_from_ui(&mut self.tabs[self.current], ui);
        let tab = &mut self.tabs[self.current];
        if tab.command_history.is_empty() {
            return Ok(());
        }

        let Some(cur) = tab.history_cursor else {
            return Ok(());
        };

        if cur + 1 < tab.command_history.len() {
            let next = cur + 1;
            tab.history_cursor = Some(next);
            tab.prompt = SharedString::from(tab.command_history[next].as_str());
        } else {
            tab.history_cursor = None;
            tab.prompt = SharedString::from(tab.history_draft.as_str());
            tab.history_draft.clear();
        }
        tab.terminal_saved_scroll_top_px = ui.get_ws_terminal_scroll_top_px();
        load_tab_to_ui(ui, tab);
        Ok(())
    }

    pub(crate) fn inject_bytes_into_current(
        &mut self,
        ui: &AppWindow,
        data: &[u8],
    ) -> Result<(), String> {
        if self.current >= self.tabs.len() {
            return Err("invalid current tab index".into());
        }
        let _ = ui;
        let tab = &mut self.tabs[self.current];

        #[cfg(target_os = "windows")]
        {
            if let Some(session) = tab.conpty.as_mut() {
                session
                    .writer
                    .write_all(data)
                    .map_err(|e| e.to_string())?;
                session.writer.flush().map_err(|e| e.to_string())?;
                return Ok(());
            }
        }

        let preview = String::from_utf8_lossy(data);
        tab.append_terminal(&format!("\n[inject]\n{preview}"));
        tab.terminal_saved_scroll_top_px = ui.get_ws_terminal_scroll_top_px();
        load_tab_to_ui(ui, tab);
        Ok(())
    }

    pub(crate) fn close_tab(&mut self, index: usize, ui: &AppWindow) -> Result<(), &'static str> {
        if self.tabs.len() <= 1 {
            return Ok(());
        }
        if index >= self.tabs.len() {
            return Err("invalid close index");
        }

        tab_update_from_ui(&mut self.tabs[self.current], ui);
        if self.current < self.tabs.len() {
            self.tabs[self.current].terminal_saved_scroll_top_px = ui.get_ws_terminal_scroll_top_px();
        }

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
        load_tab_to_ui(ui, &mut self.tabs[new_current]);
        Ok(())
    }

    pub(crate) fn move_tab(&mut self, from: usize, to: usize, ui: &AppWindow) -> Result<(), &'static str> {
        if from >= self.tabs.len() || to >= self.tabs.len() {
            return Err("invalid move index");
        }
        if from == to {
            return Ok(());
        }

        tab_update_from_ui(&mut self.tabs[self.current], ui);
        self.tabs[self.current].terminal_saved_scroll_top_px = ui.get_ws_terminal_scroll_top_px();

        let title = self.titles.remove(from);
        self.titles.insert(to, title);

        let tab = self.tabs.remove(from);
        self.tabs.insert(to, tab);

        if self.current == from {
            self.current = to;
        } else if from < self.current && to >= self.current {
            self.current -= 1;
        } else if from > self.current && to <= self.current {
            self.current += 1;
        }

        ui.set_current_tab(self.current as i32);
        sync_tab_count(ui, self.tabs.len());
        load_tab_to_ui(ui, &mut self.tabs[self.current]);
        Ok(())
    }
}
