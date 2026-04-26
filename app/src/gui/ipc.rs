use std::io::{BufRead, BufReader, ErrorKind, Write};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread;
use std::time::Duration;

use interprocess::TryClone;
use interprocess::local_socket::{
    GenericFilePath, ListenerOptions, Stream, ToFsName,
    traits::{Listener, Stream as StreamOps},
};
use interprocess::local_socket::{GenericNamespaced, ToNsName};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

const IPC_SERVER_NAME: &str = "cligj-ipc-v1";
const IPC_REQUEST_TIMEOUT: Duration = Duration::from_secs(8);
const IPC_ACCEPT_POLL_INTERVAL: Duration = Duration::from_millis(40);

#[derive(Debug)]
pub(crate) enum IpcGuiCommand {
    OpenTab {
        id: Option<Value>,
        profile: Option<String>,
        focus: bool,
        response_tx: mpsc::Sender<IpcGuiResponse>,
    },
    FocusWindow {
        id: Option<Value>,
        response_tx: mpsc::Sender<IpcGuiResponse>,
    },
    SendPrompt {
        id: Option<Value>,
        tab_id: Option<u64>,
        prompt: String,
        submit: bool,
        selection_payloads: Vec<String>,
        file_path_payloads: Vec<String>,
        file_origin_payloads: Vec<Option<IpcFileOriginPayload>>,
        response_tx: mpsc::Sender<IpcGuiResponse>,
    },
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(crate) struct IpcFileOriginPayload {
    #[serde(default, alias = "clientId")]
    pub(crate) client_id: String,
    #[serde(default, alias = "uriScheme")]
    pub(crate) uri_scheme: String,
}

#[derive(Debug)]
pub(crate) struct IpcGuiResponse {
    pub(crate) id: Option<Value>,
    pub(crate) ok: bool,
    pub(crate) result: Value,
    pub(crate) error: Option<String>,
}

#[derive(Clone, Debug)]
pub(crate) struct IpcStatus {
    pub(crate) running: bool,
    pub(crate) client_count: usize,
    pub(crate) endpoint: String,
    pub(crate) last_error: String,
}

impl Default for IpcStatus {
    fn default() -> Self {
        Self {
            running: false,
            client_count: 0,
            endpoint: endpoint_display_string(),
            last_error: String::new(),
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) enum IpcServerEvent {
    TabChanged {
        tab_id: u64,
        tab_index: usize,
        title: String,
        cmd_type: String,
    },
    TerminalChunk {
        tab_id: u64,
        text: String,
        replace: bool,
    },
    OpenEditorLocation {
        client_id: String,
        path: String,
        start_line: Option<usize>,
        end_line: Option<usize>,
    },
}

#[derive(Clone)]
pub(crate) struct IpcBridge {
    gui_tx: mpsc::Sender<IpcGuiCommand>,
    status: Arc<Mutex<IpcStatus>>,
    running: Arc<AtomicBool>,
    client_count: Arc<AtomicUsize>,
    event_tx: Arc<Mutex<Option<mpsc::Sender<IpcServerEvent>>>>,
    stop_tx: Arc<Mutex<Option<mpsc::Sender<()>>>>,
    server_thread: Arc<Mutex<Option<thread::JoinHandle<()>>>>,
}

impl IpcBridge {
    pub(crate) fn new(gui_tx: mpsc::Sender<IpcGuiCommand>) -> Self {
        Self {
            gui_tx,
            status: Arc::new(Mutex::new(IpcStatus::default())),
            running: Arc::new(AtomicBool::new(false)),
            client_count: Arc::new(AtomicUsize::new(0)),
            event_tx: Arc::new(Mutex::new(None)),
            stop_tx: Arc::new(Mutex::new(None)),
            server_thread: Arc::new(Mutex::new(None)),
        }
    }

    pub(crate) fn start(&self) -> Result<(), String> {
        if self.running.load(Ordering::Acquire) {
            return Ok(());
        }
        let (event_tx, event_rx) = mpsc::channel::<IpcServerEvent>();
        let (stop_tx, stop_rx) = mpsc::channel::<()>();
        let gui_tx = self.gui_tx.clone();
        let status = Arc::clone(&self.status);
        let running = Arc::clone(&self.running);
        let clients = Arc::clone(&self.client_count);

        let handle = thread::Builder::new()
            .name("cligj-ipc-server".to_string())
            .spawn(move || run_server(gui_tx, event_rx, stop_rx, status, running, clients))
            .map_err(|e| format!("spawn IPC server thread: {e}"))?;

        if let Ok(mut slot) = self.event_tx.lock() {
            *slot = Some(event_tx);
        }
        if let Ok(mut slot) = self.stop_tx.lock() {
            *slot = Some(stop_tx);
        }
        if let Ok(mut slot) = self.server_thread.lock() {
            *slot = Some(handle);
        }
        self.running.store(true, Ordering::Release);
        self.update_status(|s| {
            s.running = true;
            s.client_count = 0;
            s.last_error.clear();
            s.endpoint = endpoint_display_string();
        });
        Ok(())
    }

    pub(crate) fn stop(&self) {
        self.running.store(false, Ordering::Release);

        let event_sender = self.event_tx.lock().ok().and_then(|mut slot| slot.take());
        drop(event_sender);

        let stop = self.stop_tx.lock().ok().and_then(|mut slot| slot.take());
        if let Some(tx) = stop {
            let _ = tx.send(());
        }

        let join = self
            .server_thread
            .lock()
            .ok()
            .and_then(|mut slot| slot.take());
        if let Some(handle) = join {
            let _ = handle.join();
        }

        self.running.store(false, Ordering::Release);
        self.client_count.store(0, Ordering::Release);
        self.update_status(|s| {
            s.running = false;
            s.client_count = 0;
        });
    }

    pub(crate) fn toggle(&self) -> Result<(), String> {
        if self.running.load(Ordering::Acquire) {
            self.stop();
            Ok(())
        } else {
            self.start()
        }
    }

    pub(crate) fn snapshot(&self) -> IpcStatus {
        let mut out = self
            .status
            .lock()
            .ok()
            .map(|s| s.clone())
            .unwrap_or_default();
        out.running = self.running.load(Ordering::Acquire);
        out.client_count = self.client_count.load(Ordering::Acquire);
        out
    }

    pub(crate) fn publish_tab_changed(
        &self,
        tab_id: u64,
        tab_index: usize,
        title: String,
        cmd_type: String,
    ) {
        let _ = self.send_event(IpcServerEvent::TabChanged {
            tab_id,
            tab_index,
            title,
            cmd_type,
        });
    }

    pub(crate) fn publish_terminal_chunk(&self, tab_id: u64, text: &str, replace: bool) {
        if text.is_empty() {
            return;
        }
        let mut out = text.to_string();
        const MAX: usize = 4096;
        if out.len() > MAX {
            out.truncate(MAX);
        }
        let _ = self.send_event(IpcServerEvent::TerminalChunk {
            tab_id,
            text: out,
            replace,
        });
    }

    pub(crate) fn publish_open_editor_location(
        &self,
        client_id: String,
        path: String,
        start_line: Option<usize>,
        end_line: Option<usize>,
    ) {
        if client_id.trim().is_empty() || path.trim().is_empty() {
            return;
        }
        let _ = self.send_event(IpcServerEvent::OpenEditorLocation {
            client_id,
            path,
            start_line,
            end_line,
        });
    }

    fn send_event(&self, event: IpcServerEvent) -> Result<(), ()> {
        let tx_opt = self
            .event_tx
            .lock()
            .ok()
            .and_then(|slot| slot.as_ref().cloned());
        let Some(tx) = tx_opt else {
            return Err(());
        };
        tx.send(event).map_err(|_| ())
    }

    fn update_status(&self, f: impl FnOnce(&mut IpcStatus)) {
        if let Ok(mut s) = self.status.lock() {
            f(&mut s);
        }
    }
}

impl Drop for IpcBridge {
    fn drop(&mut self) {
        self.stop();
    }
}

#[derive(Debug, Deserialize)]
struct IpcRequest {
    id: Option<Value>,
    method: String,
    #[serde(default)]
    params: Value,
}

#[derive(Debug, Serialize)]
struct IpcResponse<'a> {
    r#type: &'a str,
    id: Option<Value>,
    ok: bool,
    result: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

fn run_server(
    gui_tx: mpsc::Sender<IpcGuiCommand>,
    event_rx: mpsc::Receiver<IpcServerEvent>,
    stop_rx: mpsc::Receiver<()>,
    status: Arc<Mutex<IpcStatus>>,
    running: Arc<AtomicBool>,
    client_count: Arc<AtomicUsize>,
) {
    let listener = match create_listener() {
        Ok(v) => v,
        Err(e) => {
            running.store(false, Ordering::Release);
            if let Ok(mut s) = status.lock() {
                s.running = false;
                s.last_error = format!("create listener: {e}");
            }
            return;
        }
    };

    let peers: Arc<Mutex<Vec<mpsc::Sender<String>>>> = Arc::new(Mutex::new(Vec::new()));
    let peers_for_events = Arc::clone(&peers);
    let event_forwarder = thread::Builder::new()
        .name("cligj-ipc-event-forwarder".to_string())
        .spawn(move || {
            while let Ok(event) = event_rx.recv() {
                let payload = event_to_json_line(event);
                if let Ok(mut list) = peers_for_events.lock() {
                    list.retain(|tx| tx.send(payload.clone()).is_ok());
                }
            }
        });

    loop {
        if stop_rx.try_recv().is_ok() {
            break;
        }

        let incoming: Stream = match listener.accept() {
            Ok(s) => s,
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                thread::sleep(IPC_ACCEPT_POLL_INTERVAL);
                continue;
            }
            Err(e) => {
                if let Ok(mut s) = status.lock() {
                    s.last_error = format!("accept: {e}");
                }
                thread::sleep(IPC_ACCEPT_POLL_INTERVAL);
                continue;
            }
        };

        let writer_stream: Stream = match incoming.try_clone() {
            Ok(s) => s,
            Err(e) => {
                if let Ok(mut s) = status.lock() {
                    s.last_error = format!("clone stream: {e}");
                }
                continue;
            }
        };
        let (writer_tx, writer_rx) = mpsc::channel::<String>();
        if let Ok(mut list) = peers.lock() {
            list.push(writer_tx.clone());
        }
        let cnt = client_count.fetch_add(1, Ordering::AcqRel) + 1;
        if let Ok(mut s) = status.lock() {
            s.client_count = cnt;
        }

        let peers_on_disconnect = Arc::clone(&peers);
        let status_on_disconnect = Arc::clone(&status);
        let clients_on_disconnect = Arc::clone(&client_count);
        let gui_tx_clone = gui_tx.clone();
        let running_client = Arc::clone(&running);
        thread::Builder::new()
            .name("cligj-ipc-client".to_string())
            .spawn(move || {
                let running_for_writer = Arc::clone(&running_client);
                let writer_handle = thread::Builder::new()
                    .name("cligj-ipc-client-writer".to_string())
                    .spawn(move || {
                        let mut stream = writer_stream;
                        loop {
                            match writer_rx.recv_timeout(Duration::from_millis(40)) {
                                Ok(line) => {
                                    if stream.write_all(line.as_bytes()).is_err() {
                                        break;
                                    }
                                    if stream.write_all(b"\n").is_err() {
                                        break;
                                    }
                                    if stream.flush().is_err() {
                                        break;
                                    }
                                }
                                Err(mpsc::RecvTimeoutError::Timeout) => {
                                    if !running_for_writer.load(Ordering::Acquire) {
                                        break;
                                    }
                                }
                                Err(mpsc::RecvTimeoutError::Disconnected) => break,
                            }
                        }
                    });

                handle_client_requests(
                    incoming,
                    writer_tx.clone(),
                    &gui_tx_clone,
                    Arc::clone(&running_client),
                );
                if let Ok(mut list) = peers_on_disconnect.lock() {
                    list.retain(|s| !std::ptr::eq(s, &writer_tx));
                }
                let cnt = clients_on_disconnect
                    .fetch_sub(1, Ordering::AcqRel)
                    .saturating_sub(1);
                if let Ok(mut s) = status_on_disconnect.lock() {
                    s.client_count = cnt;
                }
                if let Ok(h) = writer_handle {
                    let _ = h.join();
                }
            })
            .ok();
    }

    running.store(false, Ordering::Release);
    client_count.store(0, Ordering::Release);
    if let Ok(mut s) = status.lock() {
        s.running = false;
        s.client_count = 0;
    }
    drop(listener);
    if let Ok(h) = event_forwarder {
        let _ = h.join();
    }
}

fn handle_client_requests(
    stream: Stream,
    writer_tx: mpsc::Sender<String>,
    gui_tx: &mpsc::Sender<IpcGuiCommand>,
    running: Arc<AtomicBool>,
) {
    let _ = stream.set_nonblocking(true);
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    loop {
        if !running.load(Ordering::Acquire) {
            break;
        }
        line.clear();
        let read = match reader.read_line(&mut line) {
            Ok(n) => n,
            Err(e) if e.kind() == ErrorKind::WouldBlock => {
                thread::sleep(Duration::from_millis(20));
                continue;
            }
            Err(_) => break,
        };
        if read == 0 {
            thread::sleep(Duration::from_millis(20));
            break;
        }
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let response = handle_request(trimmed, gui_tx);
        let json = serde_json::to_string(&response).unwrap_or_else(|_| {
            r#"{"type":"response","id":null,"ok":false,"result":{},"error":"serialize response"}"#
                .to_string()
        });
        let _ = writer_tx.send(json);
    }
}

fn handle_request(raw: &str, gui_tx: &mpsc::Sender<IpcGuiCommand>) -> IpcResponse<'static> {
    let req: IpcRequest = match serde_json::from_str(raw) {
        Ok(v) => v,
        Err(e) => {
            return IpcResponse {
                r#type: "response",
                id: None,
                ok: false,
                result: json!({}),
                error: Some(format!("invalid request JSON: {e}")),
            };
        }
    };

    match req.method.as_str() {
        "ping" => IpcResponse {
            r#type: "response",
            id: req.id,
            ok: true,
            result: json!({ "pong": true }),
            error: None,
        },
        "subscribe" => IpcResponse {
            r#type: "response",
            id: req.id,
            ok: true,
            result: json!({ "subscribed": true }),
            error: None,
        },
        "openTab" => {
            let profile = req
                .params
                .get("profile")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned);
            let focus = req
                .params
                .get("focus")
                .and_then(Value::as_bool)
                .unwrap_or(true);
            let (tx, rx) = mpsc::channel();
            let send = gui_tx.send(IpcGuiCommand::OpenTab {
                id: req.id.clone(),
                profile,
                focus,
                response_tx: tx,
            });
            if send.is_err() {
                return IpcResponse {
                    r#type: "response",
                    id: req.id,
                    ok: false,
                    result: json!({}),
                    error: Some("GUI command channel unavailable".to_string()),
                };
            }
            gui_response_to_wire(req.id, rx.recv_timeout(IPC_REQUEST_TIMEOUT))
        }
        "focusWindow" => {
            let (tx, rx) = mpsc::channel();
            let send = gui_tx.send(IpcGuiCommand::FocusWindow {
                id: req.id.clone(),
                response_tx: tx,
            });
            if send.is_err() {
                return IpcResponse {
                    r#type: "response",
                    id: req.id,
                    ok: false,
                    result: json!({}),
                    error: Some("GUI command channel unavailable".to_string()),
                };
            }
            gui_response_to_wire(req.id, rx.recv_timeout(IPC_REQUEST_TIMEOUT))
        }
        "sendPrompt" => {
            let prompt = req
                .params
                .get("prompt")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            let tab_id = req.params.get("tabId").and_then(Value::as_u64);
            let submit = req
                .params
                .get("submit")
                .and_then(Value::as_bool)
                .unwrap_or(true);
            let selection_payloads = req
                .params
                .get("selectionPayloads")
                .and_then(Value::as_array)
                .map(|arr| {
                    arr.iter()
                        .filter_map(Value::as_str)
                        .map(ToOwned::to_owned)
                        .collect::<Vec<String>>()
                })
                .unwrap_or_default();
            let file_path_payloads = req
                .params
                .get("filePathPayloads")
                .and_then(Value::as_array)
                .map(|arr| {
                    arr.iter()
                        .filter_map(Value::as_str)
                        .map(ToOwned::to_owned)
                        .collect::<Vec<String>>()
                })
                .unwrap_or_default();
            let file_origin_payloads = req
                .params
                .get("fileOriginPayloads")
                .and_then(Value::as_array)
                .map(|arr| {
                    arr.iter()
                        .map(|value| {
                            if value.is_null() {
                                None
                            } else {
                                serde_json::from_value::<IpcFileOriginPayload>(value.clone()).ok()
                            }
                        })
                        .collect::<Vec<Option<IpcFileOriginPayload>>>()
                })
                .unwrap_or_default();
            let (tx, rx) = mpsc::channel();
            let send = gui_tx.send(IpcGuiCommand::SendPrompt {
                id: req.id.clone(),
                tab_id,
                prompt,
                submit,
                selection_payloads,
                file_path_payloads,
                file_origin_payloads,
                response_tx: tx,
            });
            if send.is_err() {
                return IpcResponse {
                    r#type: "response",
                    id: req.id,
                    ok: false,
                    result: json!({}),
                    error: Some("GUI command channel unavailable".to_string()),
                };
            }
            gui_response_to_wire(req.id, rx.recv_timeout(IPC_REQUEST_TIMEOUT))
        }
        _ => IpcResponse {
            r#type: "response",
            id: req.id,
            ok: false,
            result: json!({}),
            error: Some(format!("unknown method '{}'", req.method)),
        },
    }
}

fn gui_response_to_wire(
    fallback_id: Option<Value>,
    recv: Result<IpcGuiResponse, mpsc::RecvTimeoutError>,
) -> IpcResponse<'static> {
    match recv {
        Ok(msg) => IpcResponse {
            r#type: "response",
            id: msg.id.or(fallback_id),
            ok: msg.ok,
            result: msg.result,
            error: msg.error,
        },
        Err(mpsc::RecvTimeoutError::Timeout) => IpcResponse {
            r#type: "response",
            id: fallback_id,
            ok: false,
            result: json!({}),
            error: Some("request timeout waiting for UI".to_string()),
        },
        Err(mpsc::RecvTimeoutError::Disconnected) => IpcResponse {
            r#type: "response",
            id: fallback_id,
            ok: false,
            result: json!({}),
            error: Some("UI channel disconnected".to_string()),
        },
    }
}

fn event_to_json_line(event: IpcServerEvent) -> String {
    match event {
        IpcServerEvent::TabChanged {
            tab_id,
            tab_index,
            title,
            cmd_type,
        } => json!({
            "type": "event",
            "event": "tabChanged",
            "data": {
                "tabId": tab_id,
                "tabIndex": tab_index,
                "title": title,
                "cmdType": cmd_type
            }
        })
        .to_string(),
        IpcServerEvent::TerminalChunk {
            tab_id,
            text,
            replace,
        } => json!({
            "type": "event",
            "event": "terminalChunk",
            "data": {
                "tabId": tab_id,
                "text": text,
                "replace": replace
            }
        })
        .to_string(),
        IpcServerEvent::OpenEditorLocation {
            client_id,
            path,
            start_line,
            end_line,
        } => json!({
            "type": "event",
            "event": "openEditorLocation",
            "data": {
                "clientId": client_id,
                "path": path,
                "startLine": start_line,
                "endLine": end_line,
            }
        })
        .to_string(),
    }
}

fn create_listener() -> std::io::Result<interprocess::local_socket::Listener> {
    #[cfg(target_os = "windows")]
    {
        let mut last_err: Option<std::io::Error> = None;
        // Retry a few times to survive rapid stop->start races.
        for _ in 0..6 {
            match IPC_SERVER_NAME
                .to_ns_name::<GenericNamespaced>()
                .and_then(|name| {
                    ListenerOptions::new()
                        .name(name)
                        .nonblocking(interprocess::local_socket::ListenerNonblockingMode::Both)
                        .create_sync()
                }) {
                Ok(listener) => return Ok(listener),
                Err(e) => {
                    last_err = Some(e);
                    thread::sleep(Duration::from_millis(40));
                }
            }
        }
        for _ in 0..6 {
            match endpoint_display_string()
                .to_fs_name::<GenericFilePath>()
                .and_then(|name| {
                    ListenerOptions::new()
                        .name(name)
                        .nonblocking(interprocess::local_socket::ListenerNonblockingMode::Both)
                        .create_sync()
                }) {
                Ok(listener) => return Ok(listener),
                Err(e) => {
                    last_err = Some(e);
                    thread::sleep(Duration::from_millis(40));
                }
            }
        }
        return Err(last_err.unwrap_or_else(|| {
            std::io::Error::other("IPC listener failed for both namespace styles")
        }));
    }
    #[cfg(not(target_os = "windows"))]
    {
        let name = if GenericNamespaced::is_supported() {
            IPC_SERVER_NAME.to_ns_name::<GenericNamespaced>()?
        } else {
            endpoint_display_string().to_fs_name::<GenericFilePath>()?
        };
        return ListenerOptions::new()
            .name(name)
            .nonblocking(interprocess::local_socket::ListenerNonblockingMode::Both)
            .create_sync();
    }
}

fn endpoint_display_string() -> String {
    #[cfg(target_os = "windows")]
    {
        return format!(r"\\.\pipe\{IPC_SERVER_NAME}");
    }
    #[cfg(not(target_os = "windows"))]
    {
        format!("/tmp/{IPC_SERVER_NAME}.sock")
    }
}
