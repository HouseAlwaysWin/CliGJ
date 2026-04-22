//! Application entry: build window, wire timers and callbacks.

use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::mpsc;

use slint::{ComponentHandle, ModelRc, SharedString, Timer, VecModel};

#[cfg(target_os = "windows")]
use slint::winit_030::{winit, EventResult, WinitWindowAccessor};

use crate::core::config::AppConfig;

use super::font_assets::register_embedded_ui_fonts;
use super::fonts::{
    normalize_terminal_cjk_fallback_font_family, normalize_terminal_font_family,
    terminal_cjk_fallback_font_choices, terminal_font_choices,
};
use super::i18n::apply_slint_language_from_shell_setting;
use super::interactive_commands::sync_interactive_command_choices_to_ui;
use super::ipc::{IpcBridge, IpcGuiCommand};
use super::shell_profiles::sync_shell_profile_choices_to_ui;
use super::slint_ui::{AppWindow, TerminalHistoryWindow};
use super::state::{GuiState, TabState, TerminalChunk};
use super::ui_sync::{load_tab_to_ui, sync_tab_count};

mod callbacks;
mod helpers;
mod timers;

const APP_GITHUB_URL: &str = "https://github.com/HouseAlwaysWin/CliGJ";
const APP_AUTHOR: &str = "HouseAlwaysWin";
const APP_VERSION: &str = env!("CARGO_PKG_VERSION");

pub fn run_gui(inject_file: Option<PathBuf>) {
    register_embedded_ui_fonts();

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
    let history_window = Rc::new(
        TerminalHistoryWindow::new().expect("failed to build terminal history window"),
    );
    #[cfg(target_os = "windows")]
    let tray_icon: Rc<RefCell<Option<tray_icon::TrayIcon>>> = Rc::new(RefCell::new(None));
    #[cfg(target_os = "windows")]
    install_windows_tray_event_handler(&app);

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
    let terminal_font_family = normalize_terminal_font_family(
        cfg.terminal_font_family().as_deref().unwrap_or(""),
    )
    .to_string();
    let terminal_cjk_fallback_font_family = normalize_terminal_cjk_fallback_font_family(
        cfg.terminal_cjk_fallback_font_family()
            .as_deref()
            .unwrap_or(""),
    )
    .to_string();

    let titles = Rc::new(VecModel::from(vec![SharedString::from(
        super::i18n::tab_title_for_index(ui_language.as_str(), 1),
    )]));

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
        startup_terminal_font_family: terminal_font_family.clone(),
        startup_terminal_cjk_fallback_font_family: terminal_cjk_fallback_font_family.clone(),
    }));

    app.set_tab_titles(ModelRc::from(Rc::clone(&titles)));
    sync_tab_count(&app, state.borrow().tabs.len());
    sync_interactive_command_choices_to_ui(&app, &state.borrow());
    sync_shell_profile_choices_to_ui(&app, &state.borrow());
    app.set_ws_shell_startup_language(SharedString::from(ui_language.as_str()));
    app.set_ws_shell_startup_default_profile(SharedString::from(startup_profile.as_str()));
    app.set_ws_terminal_font_family(SharedString::from(terminal_font_family.as_str()));
    app.set_ws_terminal_cjk_fallback_font_family(SharedString::from(
        terminal_cjk_fallback_font_family.as_str(),
    ));
    app.set_ws_shell_startup_terminal_font_family(SharedString::from(
        terminal_font_family.as_str(),
    ));
    app.set_ws_shell_startup_terminal_cjk_fallback_font_family(SharedString::from(
        terminal_cjk_fallback_font_family.as_str(),
    ));
    app.set_ws_update_current_version(SharedString::from(APP_VERSION));
    let preferred_app_version = cfg
        .get_value("ui.preferred_app_version")
        .ok()
        .flatten()
        .unwrap_or_else(|| APP_VERSION.to_string());
    app.set_ws_update_selected_version(SharedString::from(preferred_app_version.as_str()));
    app.set_ws_update_versions(ModelRc::new(VecModel::from(vec![SharedString::from(
        APP_VERSION,
    )])));
    app.set_ws_about_version(SharedString::from(APP_VERSION));
    app.set_ws_about_author(SharedString::from(APP_AUTHOR));
    app.set_ws_about_github_url(SharedString::from(APP_GITHUB_URL));
    app.set_ws_shell_startup_terminal_font_choices(ModelRc::new(VecModel::from(
        terminal_font_choices()
            .iter()
            .map(|name| SharedString::from(*name))
            .collect::<Vec<_>>(),
    )));
    app.set_ws_shell_startup_terminal_cjk_fallback_font_choices(ModelRc::new(VecModel::from(
        terminal_cjk_fallback_font_choices()
            .iter()
            .map(|name| SharedString::from(*name))
            .collect::<Vec<_>>(),
    )));
    apply_slint_language_from_shell_setting(&app, ui_language.as_str());
    if let Err(e) = ipc_bridge.start() {
        eprintln!("CliGJ: IPC auto-start: {e}");
    }
    let ipc_snap = ipc_bridge.snapshot();
    app.set_ws_ipc_running(ipc_snap.running);
    app.set_ws_ipc_client_count(ipc_snap.client_count as i32);
    let ipc_status_text = if ipc_snap.running {
        format!("IPC ON ({})", ipc_snap.client_count)
    } else {
        "IPC OFF".to_string()
    };
    app.set_ws_ipc_status_text(SharedString::from(ipc_status_text.as_str()));

    let _terminal_stream_dispatcher =
        timers::spawn_terminal_stream_dispatcher(&app, Rc::clone(&state), rx, ipc_bridge.clone());
    callbacks::connect(
        &app,
        Rc::clone(&state),
        ipc_bridge.clone(),
        Rc::clone(&history_window),
    );

    #[cfg(target_os = "windows")]
    {
        let app_weak = app.as_weak();
        app.on_window_chrome_drag(move || {
            let Some(ui) = app_weak.upgrade() else {
                return;
            };
            let _ = ui.window().with_winit_window(|w| {
                let _ = w.drag_window();
            });
        });

        let app_weak = app.as_weak();
        app.on_window_chrome_minimize(move || {
            let Some(ui) = app_weak.upgrade() else {
                return;
            };
            let _ = ui.window().with_winit_window(|w| {
                w.set_minimized(true);
            });
        });

        let app_weak = app.as_weak();
        let tray_icon_min = Rc::clone(&tray_icon);
        app.on_window_chrome_minimize_to_tray(move || {
            let Some(ui) = app_weak.upgrade() else {
                return;
            };            
            {
                let mut tray = tray_icon_min.borrow_mut();
                if let Err(e) = super::windows_tray::ensure_tray_icon(&mut *tray) {
                    eprintln!("CliGJ: {e}");
                    return;
                }
            }
            let _ = ui.window().with_winit_window(|w| {
                w.set_visible(false);
            });
        });

        let app_weak = app.as_weak();
        app.on_window_chrome_maximize(move || {
            let Some(ui) = app_weak.upgrade() else {
                return;
            };
            if let Some(maximized) = ui.window().with_winit_window(|w| {
                let next = !w.is_maximized();
                w.set_maximized(next);
                w.is_maximized()
            }) {
                ui.set_ws_window_maximized(maximized);
            }
        });

        let app_weak = app.as_weak();
        app.on_window_chrome_title_double_click(move || {
            let Some(ui) = app_weak.upgrade() else {
                return;
            };
            if let Some(maximized) = ui.window().with_winit_window(|w| {
                let next = !w.is_maximized();
                w.set_maximized(next);
                w.is_maximized()
            }) {
                ui.set_ws_window_maximized(maximized);
            }
        });
    }
    #[cfg(not(target_os = "windows"))]
    {
        app.on_window_chrome_drag(|| {});
        app.on_window_chrome_minimize(|| {});
        app.on_window_chrome_minimize_to_tray(|| {});
        app.on_window_chrome_maximize(|| {});
        app.on_window_chrome_title_double_click(|| {});
    }

    #[cfg(target_os = "windows")]
    let tray_icon_close = Rc::clone(&tray_icon);
    app.on_window_chrome_close(move || {
        #[cfg(target_os = "windows")]
        {
            tray_icon_close.borrow_mut().take();
        }
        let _ = slint::quit_event_loop();
    });

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
fn install_windows_tray_event_handler(app: &AppWindow) {
    let app_weak = app.as_weak();
    tray_icon::TrayIconEvent::set_event_handler(Some(move |event| {
        if !super::windows_tray::should_restore_from_event(event) {
            return;
        }
        let _ = app_weak.upgrade_in_event_loop(|ui| {
            let _ = ui.window().with_winit_window(|w| {
                w.set_visible(true);
                w.set_minimized(false);
                w.focus_window();
            });
        });
    }));
}

#[cfg(target_os = "windows")]
fn register_windows_file_drop(app: &AppWindow, state: Rc<RefCell<GuiState>>) {
    /// Same as `AppTheme.chrome_resize_border` in `theme.slint` — must stay in sync.
    const CHROME_RESIZE_BORDER_LOGICAL_PX: f64 = 8.0;

    let app_weak = app.as_weak();
    let last_cursor: Rc<Cell<Option<(f64, f64)>>> = Rc::new(Cell::new(None));

    app.window().on_winit_window_event(move |slint_window, event| {
        match event {
            winit::event::WindowEvent::CursorMoved { position, .. } => {
                last_cursor.set(Some((position.x, position.y)));
                EventResult::Propagate
            }
            winit::event::WindowEvent::MouseInput {
                state,
                button,
                ..
            } => {
                if *state == winit::event::ElementState::Pressed
                    && *button == winit::event::MouseButton::Left
                {
                    if let Some((x, y)) = last_cursor.get() {
                        let _ = slint_window.with_winit_window(|w| {
                            if !w.is_maximized() {
                                return;
                            }
                            let size = w.inner_size();
                            let width = size.width as f64;
                            let height = size.height as f64;
                            let border =
                                (CHROME_RESIZE_BORDER_LOGICAL_PX * w.scale_factor()).max(1.0);
                            let in_resize_border = x < border
                                || x > width - border
                                || y < border
                                || y > height - border;
                            if in_resize_border {
                                w.set_maximized(false);
                                if let Some(ui) = app_weak.upgrade() {
                                    ui.set_ws_window_maximized(false);
                                }
                            }
                        });
                    }
                }
                EventResult::Propagate
            }
            winit::event::WindowEvent::Resized(_) => {
                let _ = slint_window.with_winit_window(|w| {
                    if let Some(ui) = app_weak.upgrade() {
                        ui.set_ws_window_maximized(w.is_maximized());
                    }
                });
                EventResult::Propagate
            }
            winit::event::WindowEvent::Focused(true) => {
                let _ = slint_window.with_winit_window(|w| {
                    if let Some(ui) = app_weak.upgrade() {
                        ui.set_ws_window_maximized(w.is_maximized());
                    }
                });
                EventResult::Propagate
            }
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
