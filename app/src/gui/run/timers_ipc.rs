use std::cell::RefCell;
use std::rc::Rc;
use std::sync::mpsc;
use std::time::Duration;

use serde_json::{Value, json};
#[cfg(target_os = "windows")]
use slint::winit_030::WinitWindowAccessor;
use slint::{ComponentHandle, Model, SharedString, Timer};

use crate::gui::ipc::{IpcBridge, IpcGuiCommand, IpcGuiResponse};
use crate::gui::slint_ui::AppWindow;
use crate::gui::state::GuiState;

fn ipc_error_suggests_endpoint_occupied(err: &str) -> bool {
    let lower = err.to_ascii_lowercase();
    // Localized OS messages vary a lot; treat listener creation failure as "likely occupied"
    // and keep specific phrases as additional signals.
    lower.contains("create listener")
        || lower.contains("address in use")
        || lower.contains("already in use")
        || lower.contains("already exists")
        || lower.contains("resource busy")
        || lower.contains("access is denied")
        || lower.contains("denied")
}

fn send_ipc_response(
    response_tx: &mpsc::Sender<IpcGuiResponse>,
    id: &mut Option<Value>,
    ok: bool,
    result: Value,
    error: Option<String>,
) {
    let _ = response_tx.send(IpcGuiResponse {
        id: id.take(),
        ok,
        result,
        error,
    });
}

fn send_ipc_error(
    response_tx: &mpsc::Sender<IpcGuiResponse>,
    id: &mut Option<Value>,
    error: String,
) {
    send_ipc_response(response_tx, id, false, json!({}), Some(error));
}

fn switch_to_target_tab(
    s: &mut GuiState,
    ui: &AppWindow,
    tab_id: Option<u64>,
    response_tx: &mpsc::Sender<IpcGuiResponse>,
    out_id: &mut Option<Value>,
) -> bool {
    let Some(target_id) = tab_id else {
        return true;
    };
    if let Some(idx) = s.tabs.iter().position(|t| t.id == target_id) {
        let _ = s.switch_tab(idx, ui);
        true
    } else {
        send_ipc_error(
            response_tx,
            out_id,
            format!("sendPrompt failed: tabId {target_id} not found"),
        );
        false
    }
}

fn convert_file_origin_payloads(
    file_origin_payloads: Vec<Option<crate::gui::ipc::IpcFileOriginPayload>>,
) -> Vec<Option<crate::gui::state::PromptFileOrigin>> {
    file_origin_payloads
        .into_iter()
        .map(|origin| {
            origin.map(|o| crate::gui::state::PromptFileOrigin {
                client_id: o.client_id,
                uri_scheme: o.uri_scheme,
            })
        })
        .collect()
}

fn update_last_file_origin(
    s: &mut GuiState,
    cur: usize,
    file_origin_payloads_converted: &[Option<crate::gui::state::PromptFileOrigin>],
) {
    if let Some(origin) = file_origin_payloads_converted
        .iter()
        .rev()
        .filter_map(|origin| origin.as_ref())
        .find(|origin| !origin.client_id.trim().is_empty() || !origin.uri_scheme.trim().is_empty())
        .cloned()
    {
        s.tabs[cur].prompt_last_file_origin = Some(origin);
    }
}

fn apply_send_prompt_payloads(
    ui: &AppWindow,
    s: &mut GuiState,
    cur: usize,
    prompt: String,
    submit: bool,
    selection_payloads: Vec<String>,
    file_path_payloads: Vec<String>,
    file_origin_payloads_converted: Vec<Option<crate::gui::state::PromptFileOrigin>>,
) {
    if submit {
        ui.set_ws_prompt(SharedString::from(prompt.as_str()));
        s.tabs[cur].prompt = SharedString::from(prompt.as_str());
        s.tabs[cur].prompt_picked_selections = selection_payloads;
        s.tabs[cur].prompt_picked_files_abs = file_path_payloads;
        s.tabs[cur].prompt_picked_file_origins = file_origin_payloads_converted;
        while s.tabs[cur].prompt_picked_file_origins.len()
            < s.tabs[cur].prompt_picked_files_abs.len()
        {
            s.tabs[cur].prompt_picked_file_origins.push(None);
        }
        crate::gui::ui_sync::sync_prompt_file_chips_to_ui(ui, &s.tabs[cur]);
        return;
    }

    let current_prompt = s.tabs[cur].prompt.to_string();
    let merged_prompt = if prompt.trim().is_empty() {
        current_prompt
    } else {
        cligj_workspace::append_attachment_token(current_prompt.as_str(), prompt.as_str())
    };
    ui.set_ws_prompt(SharedString::from(merged_prompt.as_str()));
    s.tabs[cur].prompt = SharedString::from(merged_prompt.as_str());
    for payload in selection_payloads {
        if !s.tabs[cur]
            .prompt_picked_selections
            .iter()
            .any(|p| p == &payload)
        {
            s.tabs[cur].prompt_picked_selections.push(payload);
        }
    }
    for (path, origin) in file_path_payloads.into_iter().zip(
        file_origin_payloads_converted
            .into_iter()
            .chain(std::iter::repeat(None)),
    ) {
        if !s.tabs[cur]
            .prompt_picked_files_abs
            .iter()
            .any(|p| p == &path)
        {
            s.tabs[cur].prompt_picked_files_abs.push(path);
            s.tabs[cur].prompt_picked_file_origins.push(origin);
        }
    }
    crate::gui::ui_sync::sync_prompt_file_chips_to_ui(ui, &s.tabs[cur]);
}

fn handle_ipc_gui_command(ui: &AppWindow, s: &mut GuiState, ipc: &IpcBridge, cmd: IpcGuiCommand) {
    match cmd {
        IpcGuiCommand::OpenTab {
            id,
            profile,
            focus,
            response_tx,
        } => {
            let mut out_id = id;
            if let Err(e) = s.add_tab(ui) {
                send_ipc_error(&response_tx, &mut out_id, format!("openTab failed: {e}"));
                return;
            }
            if let Some(profile) = profile.filter(|profile| !profile.trim().is_empty()) {
                let _ = s.change_current_cmd_type(profile.as_str(), ui);
            }
            if s.current >= s.tabs.len() {
                send_ipc_error(
                    &response_tx,
                    &mut out_id,
                    "openTab failed: no current tab".to_string(),
                );
                return;
            }
            let created_index = s.current;
            let tab = &s.tabs[created_index];
            let created_id = tab.id;
            let created_cmd_type = tab.cmd_type.clone();
            let created_title = s
                .titles
                .row_data(created_index)
                .unwrap_or_else(|| SharedString::from("Tab"))
                .to_string();
            if !focus && created_index > 0 {
                let _ = s.switch_tab(created_index - 1, ui);
            }
            ipc.publish_tab_changed(
                created_id,
                created_index,
                created_title,
                created_cmd_type.clone(),
            );
            send_ipc_response(
                &response_tx,
                &mut out_id,
                true,
                json!({
                    "tabId": created_id,
                    "tabIndex": created_index,
                    "cmdType": created_cmd_type,
                }),
                None,
            );
        }
        IpcGuiCommand::FocusWindow { id, response_tx } => {
            let mut out_id = id;
            #[cfg(target_os = "windows")]
            let focused = ui
                .window()
                .with_winit_window(|w| {
                    w.set_visible(true);
                    w.set_minimized(false);
                    w.focus_window();
                    true
                })
                .unwrap_or(false);
            #[cfg(not(target_os = "windows"))]
            let focused = {
                ui.window().request_activate();
                true
            };
            send_ipc_response(
                &response_tx,
                &mut out_id,
                focused,
                json!({ "focused": focused }),
                if focused {
                    None
                } else {
                    Some("focusWindow failed".to_string())
                },
            );
        }
        IpcGuiCommand::SendPrompt {
            id,
            tab_id,
            prompt,
            submit,
            selection_payloads,
            file_path_payloads,
            file_origin_payloads,
            response_tx,
        } => {
            let mut out_id = id;
            if !switch_to_target_tab(s, ui, tab_id, &response_tx, &mut out_id) {
                return;
            }
            if s.current >= s.tabs.len() {
                send_ipc_error(
                    &response_tx,
                    &mut out_id,
                    "sendPrompt failed: no active tab".to_string(),
                );
                return;
            }
            let cur = s.current;
            let file_origin_payloads_converted = convert_file_origin_payloads(file_origin_payloads);
            update_last_file_origin(s, cur, &file_origin_payloads_converted);
            apply_send_prompt_payloads(
                ui,
                s,
                cur,
                prompt,
                submit,
                selection_payloads,
                file_path_payloads,
                file_origin_payloads_converted,
            );
            if submit {
                if let Err(e) = s.submit_current_prompt(ui) {
                    send_ipc_error(&response_tx, &mut out_id, format!("sendPrompt failed: {e}"));
                    return;
                }
            } else {
                use crate::gui::composer_sync::sync_composer_line_to_conpty;
                sync_composer_line_to_conpty(ui, s);
            }
            let tab = &s.tabs[s.current];
            send_ipc_response(
                &response_tx,
                &mut out_id,
                true,
                json!({
                    "tabId": tab.id,
                    "submitted": submit
                }),
                None,
            );
        }
    }
}

pub(crate) fn spawn_ipc_bridge_timer(
    app: &AppWindow,
    state: Rc<RefCell<GuiState>>,
    ipc: IpcBridge,
    ipc_rx: mpsc::Receiver<IpcGuiCommand>,
) -> Timer {
    let app_weak = app.as_weak();
    let timer = Timer::default();
    let mut startup_retry_pending = true;
    timer.start(slint::TimerMode::Repeated, Duration::from_millis(40), move || {
        let Some(ui) = app_weak.upgrade() else {
            return;
        };

        if startup_retry_pending {
            let snap0 = ipc.snapshot();
            if !snap0.running {
                let _ = ipc.start();
            }
            startup_retry_pending = false;
        }

        let snap = ipc.snapshot();
        ui.set_ws_ipc_running(snap.running);
        ui.set_ws_ipc_client_count(snap.client_count as i32);
        let status_text = if snap.running {
            format!("IPC ON ({})", snap.client_count)
        } else if !snap.last_error.trim().is_empty() {
            format!("IPC OFF ({})", snap.last_error)
        } else {
            "IPC OFF".to_string()
        };
        ui.set_ws_ipc_status_text(SharedString::from(status_text.as_str()));

        if snap.running {
            state.borrow_mut().ipc_last_occupied_error_notified.clear();
        } else if !snap.last_error.trim().is_empty()
            && ipc_error_suggests_endpoint_occupied(snap.last_error.as_str())
        {
            let mut s = state.borrow_mut();
            if s.ipc_last_occupied_error_notified != snap.last_error {
                s.ipc_last_occupied_error_notified = snap.last_error.clone();
                drop(s);
                let detail = format!(
                    "偵測到 IPC 端點已被占用。\n\n可能原因：已開啟另一個 CliGJ 視窗。\n建議：關閉其他 CliGJ 實例後再重試。\n\n詳細資訊：{}",
                    snap.last_error
                );
                ui.set_ws_ipc_warning_title(SharedString::from("CliGJ IPC 警告"));
                ui.set_ws_ipc_warning_message(SharedString::from(detail.as_str()));
                ui.set_ws_ipc_warning_open(true);
            }
        }

        let mut pending: Vec<IpcGuiCommand> = Vec::new();
        while let Ok(cmd) = ipc_rx.try_recv() {
            pending.push(cmd);
            if pending.len() >= 16 {
                break;
            }
        }
        if pending.is_empty() {
            return;
        }
        let mut s = state.borrow_mut();
        for cmd in pending {
            handle_ipc_gui_command(&ui, &mut s, &ipc, cmd);
        }
    });
    timer
}
