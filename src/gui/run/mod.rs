//! Application entry: build window, wire timers and callbacks.

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::mpsc;

use slint::{ComponentHandle, ModelRc, SharedString, Timer, VecModel};

#[cfg(target_os = "windows")]
use slint::winit_030::{winit, EventResult, WinitWindowAccessor};

use super::slint_ui::AppWindow;
use super::state::{GuiState, TabState, TerminalChunk};
use super::ui_sync::{load_tab_to_ui, sync_tab_count};

mod callbacks;
mod helpers;
mod timers;

pub fn run_gui(inject_file: Option<PathBuf>) {
    #[cfg(target_os = "windows")]
    {
        if let Err(e) = slint::BackendSelector::new()
            .backend_name("winit".into())
            .select()
        {
            eprintln!("CliGJ: select winit backend failed: {e}");
        }
    }

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
        workspace_file_cache: Vec::new(),
        workspace_file_cache_root: None,
        at_picker_query_snapshot: String::new(),
        at_picker_open_snapshot: false,
        timer_prompt_snapshot: None,
    }));

    app.set_tab_titles(ModelRc::from(Rc::clone(&titles)));
    sync_tab_count(&app, state.borrow().tabs.len());
    {
        let mut s = state.borrow_mut();
        load_tab_to_ui(&app, &mut s.tabs[0]);
    }

    #[cfg(target_os = "windows")]
    register_windows_file_drop(&app, Rc::clone(&state));

    let _terminal_stream_timer = timers::spawn_terminal_stream_timer(&app, Rc::clone(&state), rx);
    callbacks::connect(&app, Rc::clone(&state));
    let _composer_at_sync_timer = timers::spawn_composer_at_sync_timer(&app, Rc::clone(&state));

    let _inject_startup_timer: Option<Timer> =
        inject_file.map(|path| timers::spawn_inject_startup_timer(&app, Rc::clone(&state), path));

    app.run().expect("failed to run app window");
}

#[cfg(target_os = "windows")]
fn register_windows_file_drop(app: &AppWindow, state: Rc<RefCell<GuiState>>) {
    let app_weak = app.as_weak();
    app.window().on_winit_window_event(move |_window, event| {
        match event {
            winit::event::WindowEvent::DroppedFile(path) => {
                let Some(ui) = app_weak.upgrade() else {
                    return EventResult::Propagate;
                };
                let mut s = state.borrow_mut();
                if let Err(e) = helpers::inject_path_into_current(&ui, &mut s, path.as_path()) {
                    eprintln!("CliGJ: dropped file {}: {e}", path.display());
                }
                EventResult::PreventDefault
            }
            _ => EventResult::Propagate,
        }
    });
}
