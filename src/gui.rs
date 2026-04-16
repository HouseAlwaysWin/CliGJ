use std::cell::RefCell;
use std::rc::Rc;

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
}

impl Default for TabState {
    fn default() -> Self {
        Self {
            file_path: String::new(),
            has_image: false,
            preview_image: slint::Image::default(),
            code_lines: Vec::new(),
            selected_line: 0,
            selected_context: SharedString::new(),
            prompt: SharedString::new(),
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

        self.tabs[self.current] = ui_to_tab_state(ui);
        self.current = new_index;
        ui.set_current_tab(new_index as i32);
        load_tab_to_ui(ui, &self.tabs[new_index]);
        Ok(())
    }

    fn add_tab(&mut self, ui: &AppWindow) -> Result<(), &'static str> {
        self.tabs[self.current] = ui_to_tab_state(ui);

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

    fn close_tab(&mut self, index: usize, ui: &AppWindow) -> Result<(), &'static str> {
        if self.tabs.len() <= 1 {
            return Ok(());
        }
        if index >= self.tabs.len() {
            return Err("invalid close index");
        }

        self.tabs[self.current] = ui_to_tab_state(ui);

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

fn ui_to_tab_state(ui: &AppWindow) -> TabState {
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

    ui.set_ws_selected_line(tab.selected_line);
    ui.set_ws_selected_context(tab.selected_context.clone());
    ui.set_ws_prompt(tab.prompt.clone());
}
