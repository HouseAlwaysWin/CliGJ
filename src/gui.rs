use std::cell::RefCell;
use std::io::Write;
use std::rc::Rc;
use std::sync::mpsc;
use std::time::Duration;

use slint::{Model, ModelRc, SharedString, Timer, VecModel};

#[cfg(target_os = "windows")]
use crate::terminal::windows_conpty;

slint::include_modules!();

pub fn run_gui() {
    let app = AppWindow::new().expect("failed to build app window");

    let titles = Rc::new(VecModel::from(vec![SharedString::from("工作階段 1")]));

    let (tx, rx) = mpsc::channel::<TerminalChunk>();

    let state = Rc::new(RefCell::new(GuiState {
        tabs: vec![TabState::new(1, tx.clone())],
        titles: Rc::clone(&titles),
        current: 0,
        next_id: 2,
        tx,
        pending_scroll: false,
    }));

    app.set_tab_titles(ModelRc::from(Rc::clone(&titles)));
    sync_tab_count(&app, state.borrow().tabs.len());
    load_tab_to_ui(&app, &state.borrow().tabs[0]);

    // Drain terminal chunks and append to the appropriate tab.
    let state_for_stream = Rc::clone(&state);
    let app_weak = app.as_weak();
    let timer = Timer::default();
    timer.start(
        slint::TimerMode::Repeated,
        Duration::from_millis(16),
        move || {
            let Some(ui) = app_weak.upgrade() else { return; };
            let mut s = state_for_stream.borrow_mut();
            // Defer scrolling by one tick so Slint has time
            // to update TextEdit's viewport metrics.
            if s.pending_scroll {
                ui.invoke_ws_scroll_terminal_to_bottom();
                s.pending_scroll = false;
            }
            while let Ok(chunk) = rx.try_recv() {
                let current_id = s.tabs.get(s.current).map(|t| t.id);
                let mut updated_current = None;
                for tab in s.tabs.iter_mut() {
                    if tab.id == chunk.tab_id {
                        if let Some(v) = chunk.set_auto_scroll {
                            tab.auto_scroll = v;
                        }
                        if chunk.replace {
                            tab.terminal_text = chunk.text.clone();
                        } else {
                            tab.append_terminal(&chunk.text);
                        }
                        if current_id == Some(chunk.tab_id) {
                            updated_current = Some(tab.terminal_text.clone());
                            if tab.auto_scroll {
                                s.pending_scroll = true;
                            }
                        }
                        break;
                    }
                }
                if let Some(text) = updated_current {
                    ui.set_ws_terminal_text(SharedString::from(text.as_str()));
                    // For full-screen replace updates, keep the viewport at the top
                    // until the user starts interacting (auto_scroll becomes true).
                    if let Some(current_tab) = s.tabs.get(s.current) {
                        if !current_tab.auto_scroll {
                            ui.invoke_ws_scroll_terminal_to_top();
                        }
                    }
                }
            }
        },
    );

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

    let state_for_hist_prev = Rc::clone(&state);
    let app_weak = app.as_weak();
    app.on_prompt_history_prev(move || {
        let Some(ui) = app_weak.upgrade() else { return; };
        let mut s = state_for_hist_prev.borrow_mut();
        if let Err(e) = s.history_prev_current_prompt(&ui) {
            eprintln!("CliGJ: history prev: {e}");
        }
    });

    let state_for_hist_next = Rc::clone(&state);
    let app_weak = app.as_weak();
    app.on_prompt_history_next(move || {
        let Some(ui) = app_weak.upgrade() else { return; };
        let mut s = state_for_hist_next.borrow_mut();
        if let Err(e) = s.history_next_current_prompt(&ui) {
            eprintln!("CliGJ: history next: {e}");
        }
    });

    let state_for_ctrl = Rc::clone(&state);
    let app_weak = app.as_weak();
    app.on_prompt_control_sequence(move |seq| {
        let Some(ui) = app_weak.upgrade() else { return; };
        let mut s = state_for_ctrl.borrow_mut();
        if let Err(e) = s.send_control_sequence_current(&ui, seq.as_str()) {
            eprintln!("CliGJ: control sequence: {e}");
        }
    });

    let state_for_raw_toggle = Rc::clone(&state);
    let app_weak = app.as_weak();
    app.on_toggle_raw_input_requested(move || {
        let Some(ui) = app_weak.upgrade() else { return; };
        let mut s = state_for_raw_toggle.borrow_mut();
        if let Err(e) = s.toggle_raw_input_current(&ui) {
            eprintln!("CliGJ: raw input toggle: {e}");
        }
    });

    let state_for_rename = Rc::clone(&state);
    let app_weak = app.as_weak();
    app.on_rename_tab_requested(move |index| {
        let Some(ui) = app_weak.upgrade() else { return; };
        let s = state_for_rename.borrow_mut();
        if index < 0 || (index as usize) >= s.tabs.len() { return; }
        let title = s.titles.row_data(index as usize).unwrap_or_else(|| SharedString::from("Tab"));
        ui.set_ws_rename_index(index);
        ui.set_ws_rename_text(title);
        ui.set_ws_rename_open(true);
    });

    let state_for_rename_commit = Rc::clone(&state);
    let app_weak = app.as_weak();
    app.on_rename_commit(move |index, text| {
        let Some(ui) = app_weak.upgrade() else { return; };
        let s = state_for_rename_commit.borrow_mut();
        if index < 0 || (index as usize) >= s.tabs.len() { return; }
        s.titles.set_row_data(index as usize, SharedString::from(text.as_str()));
        ui.set_ws_rename_open(false);
    });

    let app_weak = app.as_weak();
    app.on_rename_cancel(move || {
        let Some(ui) = app_weak.upgrade() else { return; };
        ui.set_ws_rename_open(false);
    });

    let state_for_move = Rc::clone(&state);
    let app_weak = app.as_weak();
    app.on_move_tab_requested(move |from, to| {
        let Some(ui) = app_weak.upgrade() else { return; };
        let mut s = state_for_move.borrow_mut();
        let _ = s.move_tab(from as usize, to as usize, &ui);
    });

    app.run().expect("failed to run app window");
}

struct TabState {
    id: u64,
    file_path: String,
    has_image: bool,
    preview_image: slint::Image,
    selected_line: i32,
    selected_context: SharedString,
    prompt: SharedString,
    cmd_type: String,
    terminal_text: String,
    auto_scroll: bool,
    raw_input_mode: bool,
    command_history: Vec<String>,
    history_cursor: Option<usize>,
    history_draft: String,

    #[cfg(target_os = "windows")]
    conpty: Option<windows_conpty::ConptySession>,
}

impl TabState {
    fn new(id: u64, tx: mpsc::Sender<TerminalChunk>) -> Self {
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
            raw_input_mode: false,
            command_history: Vec::new(),
            history_cursor: None,
            history_draft: String::new(),
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

    fn append_terminal(&mut self, chunk: &str) {
        // Keep buffer bounded to avoid unbounded memory.
        const MAX: usize = 1_000_000;
        // When auto-scroll is enabled, switch to "tail" mode to avoid viewport jumpiness.
        const TAIL_MAX: usize = 80_000;
        self.terminal_text.push_str(chunk);
        let limit = if self.auto_scroll { TAIL_MAX } else { MAX };
        if self.terminal_text.len() > limit {
            let cut = self.terminal_text.len() - limit;
            self.terminal_text.drain(..cut);
        }
    }
}

struct GuiState {
    tabs: Vec<TabState>,
    titles: Rc<VecModel<SharedString>>,
    current: usize,
    next_id: u64,
    tx: mpsc::Sender<TerminalChunk>,
    pending_scroll: bool,
}

impl GuiState {
    fn toggle_raw_input_current(&mut self, ui: &AppWindow) -> Result<(), &'static str> {
        if self.current >= self.tabs.len() {
            return Err("invalid current tab index");
        }
        tab_update_from_ui(&mut self.tabs[self.current], ui);
        let tab = &mut self.tabs[self.current];
        tab.raw_input_mode = !tab.raw_input_mode;
        load_tab_to_ui(ui, tab);
        Ok(())
    }

    fn switch_tab(&mut self, new_index: usize, ui: &AppWindow) -> Result<(), &'static str> {
        if new_index >= self.tabs.len() {
            return Err("invalid tab index");
        }
        if new_index == self.current {
            return Ok(());
        }

        tab_update_from_ui(&mut self.tabs[self.current], ui);
        self.current = new_index;
        ui.set_current_tab(new_index as i32);
        load_tab_to_ui(ui, &self.tabs[new_index]);
        Ok(())
    }

    fn add_tab(&mut self, ui: &AppWindow) -> Result<(), &'static str> {
        tab_update_from_ui(&mut self.tabs[self.current], ui);

        let n = self.titles.row_count();
        let label = SharedString::from(format!("工作階段 {}", n + 1));
        self.titles.push(label);
        let id = self.next_id;
        self.next_id += 1;
        self.tabs.push(TabState::new(id, self.tx.clone()));

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
        tab_update_from_ui(&mut self.tabs[self.current], ui);
        self.tabs[self.current].cmd_type = new_cmd_type.to_string();

        #[cfg(target_os = "windows")]
        {
            // Restart ConPTY session for interactive shells.
            self.tabs[self.current].conpty = None;
            self.tabs[self.current].terminal_text.clear();
            self.tabs[self.current].auto_scroll = false;
            if new_cmd_type == "Command Prompt" || new_cmd_type == "PowerShell" {
                if let Ok(spawn) = windows_conpty::spawn_conpty(new_cmd_type, 120, 40) {
                    let tab_id = self.tabs[self.current].id;
                    let tx = self.tx.clone();
                    windows_conpty::start_reader_thread(spawn.reader, move |render| {
                        let _ = tx.send(TerminalChunk {
                            tab_id,
                            text: render.text,
                            replace: true,
                            set_auto_scroll: if render.filled { Some(true) } else { None },
                        });
                    });
                    self.tabs[self.current].conpty = Some(spawn.session);
                }
            }
        }

        ui.set_ws_cmd_type(SharedString::from(new_cmd_type));
        load_tab_to_ui(ui, &self.tabs[self.current]);
        Ok(())
    }

    fn submit_current_prompt(&mut self, ui: &AppWindow) -> Result<(), &'static str> {
        if self.current >= self.tabs.len() {
            return Err("invalid current tab index");
        }
        tab_update_from_ui(&mut self.tabs[self.current], ui);
        let tab = &mut self.tabs[self.current];
        let command_line = tab.prompt.to_string();
        let command_line = command_line.trim().to_string();
        if command_line.is_empty() {
            return Ok(());
        }

        {
            let history = &mut tab.command_history;
            if history.last().map(|s| s.as_str()) != Some(command_line.as_str()) {
                history.push(command_line.clone());
            }
            tab.history_cursor = None;
            tab.history_draft.clear();
        }

        #[cfg(target_os = "windows")]
        {
            if let Some(session) = tab.conpty.as_mut() {
                let mut to_send = command_line.clone();
                to_send.push_str("\r\n");
                let _ = session.writer.write_all(to_send.as_bytes());
                let _ = session.writer.flush();
            } else {
                // No interactive shell: show the line in the UI buffer.
                tab.append_terminal(&format!("{command_line}\n"));
            }
        }

        #[cfg(not(target_os = "windows"))]
        {
            tab.append_terminal(&format!("{command_line}\n"));
        }

        tab.prompt = SharedString::new();
        // Auto-scroll is enabled once output fills the visible terminal height.
        load_tab_to_ui(ui, tab);
        Ok(())
    }

    fn history_prev_current_prompt(&mut self, ui: &AppWindow) -> Result<(), &'static str> {
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

    fn history_next_current_prompt(&mut self, ui: &AppWindow) -> Result<(), &'static str> {
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

    fn send_control_sequence_current(&mut self, ui: &AppWindow, seq: &str) -> Result<(), &'static str> {
        if self.current >= self.tabs.len() {
            return Err("invalid current tab index");
        }
        tab_update_from_ui(&mut self.tabs[self.current], ui);
        let tab = &mut self.tabs[self.current];

        #[cfg(target_os = "windows")]
        {
            if let Some(session) = tab.conpty.as_mut() {
                let _ = session.writer.write_all(seq.as_bytes());
                let _ = session.writer.flush();
                return Ok(());
            }
        }

        Ok(())
    }

    fn close_tab(&mut self, index: usize, ui: &AppWindow) -> Result<(), &'static str> {
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
        load_tab_to_ui(ui, &self.tabs[new_current]);
        Ok(())
    }

    fn move_tab(&mut self, from: usize, to: usize, ui: &AppWindow) -> Result<(), &'static str> {
        if from >= self.tabs.len() || to >= self.tabs.len() {
            return Err("invalid move index");
        }
        if from == to {
            return Ok(());
        }

        // Persist current UI state into current tab before reordering.
        tab_update_from_ui(&mut self.tabs[self.current], ui);

        // Reorder titles model.
        let title = self.titles.remove(from);
        self.titles.insert(to, title);

        // Reorder tab states.
        let tab = self.tabs.remove(from);
        self.tabs.insert(to, tab);

        // Fix current index.
        if self.current == from {
            self.current = to;
        } else if from < self.current && to >= self.current {
            self.current -= 1;
        } else if from > self.current && to <= self.current {
            self.current += 1;
        }

        ui.set_current_tab(self.current as i32);
        sync_tab_count(ui, self.tabs.len());
        load_tab_to_ui(ui, &self.tabs[self.current]);
        Ok(())
    }
}

fn sync_tab_count(ui: &AppWindow, n: usize) {
    ui.set_tab_count(n as i32);
}

fn tab_update_from_ui(tab: &mut TabState, ui: &AppWindow) {
    tab.file_path = ui.get_ws_file_path().to_string();
    tab.has_image = ui.get_ws_has_image();
    tab.preview_image = ui.get_ws_preview_image();
    tab.selected_line = ui.get_ws_selected_line();
    tab.selected_context = ui.get_ws_selected_context();
    tab.prompt = ui.get_ws_prompt();
    tab.cmd_type = ui.get_ws_cmd_type().to_string();
    tab.terminal_text = ui.get_ws_terminal_text().to_string();
    tab.auto_scroll = ui.get_ws_auto_scroll();
    tab.raw_input_mode = ui.get_ws_raw_input();
}

fn load_tab_to_ui(ui: &AppWindow, tab: &TabState) {
    ui.set_ws_file_path(SharedString::from(tab.file_path.as_str()));
    ui.set_ws_has_image(tab.has_image);
    ui.set_ws_preview_image(tab.preview_image.clone());
    ui.set_ws_terminal_text(SharedString::from(tab.terminal_text.as_str()));
    ui.set_ws_auto_scroll(tab.auto_scroll);
    if !tab.auto_scroll {
        ui.invoke_ws_scroll_terminal_to_top();
    }

    ui.set_ws_selected_line(tab.selected_line);
    ui.set_ws_selected_context(tab.selected_context.clone());
    ui.set_ws_prompt(tab.prompt.clone());
    ui.set_ws_cmd_type(SharedString::from(tab.cmd_type.as_str()));
    ui.set_ws_raw_input(tab.raw_input_mode);
}

fn default_cmd_type() -> &'static str {
    if cfg!(target_os = "windows") {
        "Command Prompt"
    } else {
        "Shell"
    }
}

#[derive(Debug)]
struct TerminalChunk {
    tab_id: u64,
    text: String,
    replace: bool,
    set_auto_scroll: Option<bool>,
}
