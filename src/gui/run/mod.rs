//! Application entry: build window, wire timers and callbacks.

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::mpsc;

use slint::{ComponentHandle, ModelRc, SharedString, Timer, VecModel};

#[cfg(target_os = "windows")]
use slint::winit_030::{winit, EventResult, WinitWindowAccessor};

use crate::core::config::AppConfig;

use super::interactive_commands::sync_interactive_command_choices_to_ui;
use super::ipc::{IpcBridge, IpcGuiCommand};
use super::shell_profiles::sync_shell_profile_choices_to_ui;
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
    let (ipc_gui_tx, ipc_gui_rx) = mpsc::channel::<IpcGuiCommand>();
    let ipc_bridge = IpcBridge::new(ipc_gui_tx);

    let mut cfg = AppConfig::load_or_default().unwrap_or_default();
    let (interactive_commands, persist_interactive) =
        super::interactive_commands::load_from_config(&cfg);
    let (shell_profiles, persist_shell_profiles) = super::shell_profiles::load_from_config(&cfg);
    if persist_interactive {
        cfg.set_interactive_commands(&interactive_commands);
    }
    if persist_shell_profiles {
        cfg.set_shell_profiles(&shell_profiles);
    }
    if persist_interactive || persist_shell_profiles {
        let _ = cfg.save();
    }
    let ui_language = cfg
        .ui_language()
        .unwrap_or_else(|| "預設".to_string());
    let default_shell_profile = cfg.default_shell_profile().unwrap_or_else(|| {
        shell_profiles
            .first()
            .map(|(n, _, _)| n.clone())
            .unwrap_or_else(|| "Command Prompt".to_string())
    });
    let profile_ok = shell_profiles
        .iter()
        .any(|(n, _, _)| n == &default_shell_profile);
    let startup_profile = if profile_ok {
        default_shell_profile
    } else {
        shell_profiles
            .first()
            .map(|(n, _, _)| n.clone())
            .unwrap_or_else(|| "Command Prompt".to_string())
    };

    let initial_tab_cwd = super::shell_profiles::startup_cwd_from_profiles_list(
        startup_profile.as_str(),
        &shell_profiles,
    );

    let state = Rc::new(RefCell::new(GuiState {
        tabs: vec![TabState::new(1, tx.clone(), initial_tab_cwd)],
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
        interactive_commands,
        shell_profiles,
        startup_language: ui_language.clone(),
        startup_default_shell_profile: startup_profile.clone(),
    }));

    app.set_tab_titles(ModelRc::from(Rc::clone(&titles)));
    sync_tab_count(&app, state.borrow().tabs.len());
    sync_interactive_command_choices_to_ui(&app, &state.borrow());
    sync_shell_profile_choices_to_ui(&app, &state.borrow());
    app.set_ws_shell_startup_language(SharedString::from(ui_language.as_str()));
    app.set_ws_shell_startup_default_profile(SharedString::from(startup_profile.as_str()));
    app.set_ws_ipc_status_text(SharedString::from("IPC OFF"));
    app.set_ws_ipc_running(false);
    app.set_ws_ipc_client_count(0);

    let _terminal_stream_dispatcher =
        timers::spawn_terminal_stream_dispatcher(&app, Rc::clone(&state), rx, ipc_bridge.clone());
    callbacks::connect(&app, Rc::clone(&state), ipc_bridge.clone());

    {
        let mut s = state.borrow_mut();
        load_tab_to_ui(&app, &mut s.tabs[0]);
    }

    #[cfg(target_os = "windows")]
    register_windows_file_drop(&app, Rc::clone(&state));
    let _composer_at_sync_timer = timers::spawn_composer_at_sync_timer(&app, Rc::clone(&state));
    let _ipc_bridge_timer =
        timers::spawn_ipc_bridge_timer(&app, Rc::clone(&state), ipc_bridge.clone(), ipc_gui_rx);

    let _inject_startup_timer: Option<Timer> =
        inject_file.map(|path| timers::spawn_inject_startup_timer(&app, Rc::clone(&state), path));

    app.run().expect("failed to run app window");
}

#[cfg(target_os = "windows")]
fn register_windows_file_drop(app: &AppWindow, state: Rc<RefCell<GuiState>>) {
    let app_weak = app.as_weak();
    app.window().on_winit_window_event(move |_window, event| {
        match event {
            winit::event::WindowEvent::HoveredFile(_) => {
                if let Some(ui) = app_weak.upgrade() {
                    ui.set_ws_file_drop_visible(true);
                }
                EventResult::PreventDefault
            }
            winit::event::WindowEvent::HoveredFileCancelled => {
                if let Some(ui) = app_weak.upgrade() {
                    ui.set_ws_file_drop_visible(false);
                }
                EventResult::PreventDefault
            }
            winit::event::WindowEvent::DroppedFile(path) => {
                let Some(ui) = app_weak.upgrade() else {
                    return EventResult::Propagate;
                };
                ui.set_ws_file_drop_visible(false);
                let mut s = state.borrow_mut();
                if helpers::is_probably_image_file(path.as_path()) {
                    if helpers::load_slint_image_from_path(path.as_path()).is_some() {
                        if let Err(e) =
                            helpers::push_prompt_image_from_path(&ui, &mut s, path.as_path())
                        {
                            eprintln!("CliGJ: dropped image {}: {e}", path.display());
                        }
                        return EventResult::PreventDefault;
                    }
                }
                if let Err(e) = helpers::inject_path_into_current(&ui, &mut s, path.as_path()) {
                    eprintln!("CliGJ: dropped file {}: {e}", path.display());
                }
                EventResult::PreventDefault
            }
            _ => EventResult::Propagate,
        }
    });
}
