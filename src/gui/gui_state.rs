use std::io::Write;

use slint::{Model, SharedString};

#[cfg(target_os = "windows")]
use crate::terminal::windows_conpty;

use super::composer_sync::diff_composer_to_conpty;
use super::slint_ui::AppWindow;
use super::state::{GuiState, TerminalChunk};
use super::ui_sync::{load_tab_to_ui, sync_tab_count, tab_update_from_ui};

impl GuiState {
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
        self.current = new_index;
        ui.set_current_tab(new_index as i32);
        load_tab_to_ui(ui, &mut self.tabs[new_index]);
        Ok(())
    }

    pub(crate) fn add_tab(&mut self, ui: &AppWindow) -> Result<(), &'static str> {
        tab_update_from_ui(&mut self.tabs[self.current], ui);

        let n = self.titles.row_count();
        let label = SharedString::from(format!("工作階段 {}", n + 1));
        self.titles.push(label);
        let id = self.next_id;
        self.next_id += 1;
        self.tabs.push(super::state::TabState::new(id, self.tx.clone()));

        let new_index = self.tabs.len() - 1;
        self.current = new_index;
        ui.set_current_tab(new_index as i32);
        sync_tab_count(ui, self.tabs.len());
        load_tab_to_ui(ui, &mut self.tabs[new_index]);
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

        #[cfg(target_os = "windows")]
        {
            self.tabs[self.current].conpty = None;
            self.tabs[self.current].terminal_text.clear();
            self.tabs[self.current].auto_scroll = false;
            self.tabs[self.current].composer_pty_mirror.clear();
            if new_cmd_type == "Command Prompt" || new_cmd_type == "PowerShell" {
                if let Ok(spawn) = windows_conpty::spawn_conpty(new_cmd_type, 120, 40) {
                    let tab_id = self.tabs[self.current].id;
                    let tx = self.tx.clone();
                    windows_conpty::start_reader_thread(spawn.reader, move |render| {
                        let _ = tx.send(TerminalChunk {
                            tab_id,
                            text: render.text,
                            lines: render.lines,
                            full_len: render.full_len,
                            first_line_idx: render.first_line_idx,
                            replace: true,
                            set_auto_scroll: if render.filled { Some(true) } else { None },
                            changed_indices: render.changed_indices,
                        });
                    });
                    self.tabs[self.current].conpty = Some(spawn.session);
                }
            }
        }

        ui.set_ws_cmd_type(SharedString::from(new_cmd_type));
        load_tab_to_ui(ui, &mut self.tabs[self.current]);
        Ok(())
    }

    pub(crate) fn submit_current_prompt(&mut self, ui: &AppWindow) -> Result<(), &'static str> {
        if self.current >= self.tabs.len() {
            return Err("invalid current tab index");
        }
        tab_update_from_ui(&mut self.tabs[self.current], ui);
        let tab = &mut self.tabs[self.current];
        
        // 1. Prepare additional attachments (files/images) to be appended to command line
        let mut extra_payload = String::new();
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

        let full_command = format!("{}{}", tab.prompt, extra_payload).trim().to_string();

        // 2. Add to history if not empty
        if !full_command.is_empty() {
            let history = &mut tab.command_history;
            if history.last().map(|s| s.as_str()) != Some(full_command.as_str()) {
                history.push(full_command.clone());
            }
        }
        tab.history_cursor = None;
        tab.history_draft.clear();

        // 3. Send to PTY
        #[cfg(target_os = "windows")]
        {
            if let Some(session) = tab.conpty.as_mut() {
                use std::io::Write;
                use crate::gui::composer_sync::diff_composer_to_conpty;

                // Send whatever is left in the prompt (not mirrored yet) + extras
                let cur_prompt = tab.prompt.to_string();
                let diff = diff_composer_to_conpty(&tab.composer_pty_mirror, &cur_prompt);
                let _ = session.writer.write_all(&diff);
                let _ = session.writer.write_all(extra_payload.as_bytes());
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
            if !full_command.is_empty() {
                tab.append_terminal(&format!("{full_command}\n"));
            } else {
                tab.append_terminal("\n");
            }
        }

        // 4. Reset composer state
        tab.prompt = SharedString::new();
        tab.prompt_picked_files_abs.clear();
        tab.prompt_picked_images.clear();
        tab.composer_pty_mirror.clear();
        // Update snapshot to prevent timer from thinking it needs to delete the prompt we just submitted
        self.timer_prompt_snapshot = Some((self.current, String::new(), ui.get_ws_raw_input()));
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
