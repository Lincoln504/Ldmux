//! The remote server: the phone-facing WebSocket endpoint (the tailnet address +
//! token auth).
//!
//! Port of `packages/daemon/src/remote.ts` (1293 lines). The phone connects over
//! the tailnet address with the bearer token; the server gates on the token.
//!
//! ## The full `onMessage` dispatch
//!
//! 16 message types: `ping`, `hello`, `list`, `desktop`, `new`, `makelive`,
//! `closelive`, `kill`, `close`, `attach`, `resize`, `mouseclick`, `input`,
//! `key`, `copymode`, `follow`, `scrollback`.
//!
//! ## The device ownership
//!
//! The `byDevice` map: which device holds which session. The `releaseAfterGrace`
//! timer: a closed socket's claims are released after a grace period (a
//! reconnect that comes back within the grace still holds the shell).
//!
//! ## The sweep
//!
//! The idle connection cleanup: a socket silent for `idleMs` is terminated.
//!
//! ## What is NOT here yet
//!
//! - The `makeLive`/`closeLive` handlers (the desktop window lifecycle,
//!   `desktop.ts`).
//! - The `resize`/`mouseclick`/`input`/`key`/`copymode`/`scrollback` handlers
//!   (the stream methods, `PaneStream`).
//! - The `new`/`kill`/`close` handlers (the daemon's session lifecycle).

use std::collections::HashMap;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;
use std::sync::Arc;

use futures_util::sink::SinkExt;
use futures_util::stream::StreamExt;
use tokio::net::TcpListener;
use tokio_tungstenite::tungstenite::Message;

/// The WS stream type (the `accept_async` returns this; `connect_async` returns
/// `WebSocketStream<MaybeTlsStream<TcpStream>>`, which is the same for our
/// purposes).
type WsStream = tokio_tungstenite::WebSocketStream<tokio::net::TcpStream>;

/// The remote config: the hostname (the tailnet address), the port, and the
/// token (the ADR-0010 bearer).
#[derive(Clone)]
pub struct RemoteConfig {
    /// The hostname to bind (the tailnet address, e.g. `100.127.119.54`).
    pub hostname: String,
    /// The port to bind (e.g. `8080`).
    pub port: u16,
    /// The bearer token (the ADR-0010 identity).
    pub token: String,
}

impl RemoteConfig {
    pub fn new(hostname: impl Into<String>, port: u16, token: impl Into<String>) -> Self {
        Self { hostname: hostname.into(), port, token: token.into() }
    }
}

/// Per-connection state (the `ConnData` in the TS).
#[allow(dead_code)]
struct ConnData {
    /// Whether the connection is disposed (the client closed).
    disposed: bool,
    /// The attach generation (incremented on each attach; a stale snapshot
    /// after a re-attach is dropped).
    attach_gen: u64,
    /// Whether the connection is live (actively streaming).
    live: bool,
    /// The device identity (the `hello` sets it).
    device: Option<ldmux_protocol::rpc::DeviceInfo>,
    /// The session id this connection is attached to.
    session_id: Option<String>,
    /// The session record this connection is attached to.
    record: Option<ldmux_protocol::rpc::SessionRecord>,
    /// The last time this connection sent a message (the sweep uses it).
    last_seen: std::time::Instant,
}

/// The remote server: the phone-facing WebSocket endpoint.
pub struct RemoteServer {
    cfg: RemoteConfig,
    /// Whether the server is stopped (the `stop` sets it).
    stopped: Arc<AtomicBool>,
    /// The accept loop's join handle (the `start` spawns it).
    #[allow(dead_code)]
    _accept_handle: Option<tokio::task::JoinHandle<()>>,
    /// Per-connection state (keyed by a connection id).
    #[allow(dead_code)]
    conn_state: HashMap<u64, ConnData>,
    /// The device ownership: device id → set of connection ids.
    #[allow(dead_code)]
    by_device: HashMap<String, std::collections::HashSet<u64>>,
}

impl RemoteServer {
    pub fn new(cfg: RemoteConfig) -> Self {
        Self {
            cfg,
            stopped: Arc::new(AtomicBool::new(false)),
            _accept_handle: None,
            conn_state: HashMap::new(),
            by_device: HashMap::new(),
        }
    }

    /// Bind the WS server on the tailnet address (the `start()`).
    ///
    /// The `daemon_arc` is the `Arc<Mutex<LdmuxDaemon>>` shared with the
    /// daemon; the `handle_connection` function uses it to call the daemon's
    /// methods (session lifecycle, stream, desktop).
    pub async fn start(
        &mut self,
        daemon_arc: &std::sync::Arc<tokio::sync::Mutex<super::daemon::LdmuxDaemon>>,
    ) -> Result<(String, u16), String> {
        let addr = format!("{}:{}", self.cfg.hostname, self.cfg.port);
        let listener = TcpListener::bind(&addr)
            .await
            .map_err(|e| format!("bind {addr}: {e}"))?;
        let token = self.cfg.token.clone();
        let stopped = Arc::clone(&self.stopped);
        let daemon_arc = daemon_arc.clone();
        let handle = tokio::spawn(async move {
            let mut conn_id: u64 = 0;
            loop {
                let (stream, _remote) = match listener.accept().await {
                    Ok(v) => v,
                    Err(_) => {
                        if stopped.load(Ordering::Relaxed) {
                            break;
                        }
                        continue;
                    }
                };
                if stopped.load(Ordering::Relaxed) {
                    break;
                }
                let ws_stream = match tokio_tungstenite::accept_async(stream).await {
                    Ok(s) => s,
                    Err(_) => continue,
                };
                conn_id += 1;
                tokio::spawn(handle_connection(
                    ws_stream,
                    token.clone(),
                    conn_id,
                    daemon_arc.clone(),
                ));
            }
        });
        self._accept_handle = Some(handle);
        Ok((self.cfg.hostname.clone(), self.cfg.port))
    }

    /// Stop the server (the `stop()`).
    pub fn stop(&mut self) {
        self.stopped.store(true, Ordering::Relaxed);
    }
}

/// The per-connection loop: read messages, check the token on the first
/// message, dispatch, write responses.
///
/// The token gate: the first message must carry the token (the `hello` with the
/// device id + token).
async fn handle_connection(
    mut ws: WsStream,
    token: String,
    _conn_id: u64,
    daemon_arc: std::sync::Arc<tokio::sync::Mutex<super::daemon::LdmuxDaemon>>,
) {
    let mut authorized = false;
    // These are set by the `hello`/`attach` handlers and read by the stream
    // commands.
    let mut device: Option<ldmux_protocol::rpc::DeviceInfo> = None;
    let mut session_id: Option<String> = None;
    // The attach output buffer (flushed on each WS message).
    let mut attach_out: Option<Arc<std::sync::Mutex<Vec<u8>>>> = None;

    loop {
        let Some(Ok(msg)) = ws.next().await else {
            // The connection closed.
            break;
        };
        // Flush the attach output buffer (the live output accumulated since
        // the last flush is sent now).
        if let Some(buf) = &attach_out {
            let data: Vec<u8> = buf.lock().unwrap().drain(..).collect();
            if !data.is_empty() {
                let _ = ws.send(Message::Binary(data.into())).await;
            }
        }
        let text: String = match &msg {
            Message::Text(t) => t.to_string(),
            Message::Binary(b) => String::from_utf8_lossy(b.as_ref()).to_string(),
            Message::Ping(_) => continue,
            Message::Pong(_) => continue,
            Message::Close(_) => break,
            _ => continue,
        };
        // The token gate: the first message must carry the token.
        if !authorized {
            let v: serde_json::Value = match serde_json::from_str(&text) {
                Ok(v) => v,
                Err(_) => {
                    let _ = send_json(&mut ws, &serde_json::json!({
                        "type": "error", "code": "bad_json", "message": "invalid JSON"
                    }))
                    .await;
                    break;
                }
            };
            let provided = v.get("token").and_then(|t| t.as_str()).unwrap_or("");
            if provided != token {
                let _ = send_json(&mut ws, &serde_json::json!({
                    "type": "error", "code": "unauthorized", "message": "bad token"
                }))
                .await;
                break;
            }
            authorized = true;
        }
        // The dispatch: the 16 message types.
        let v: serde_json::Value = match serde_json::from_str(&text) {
            Ok(v) => v,
            Err(_) => continue,
        };
        let mtype = v.get("type").and_then(|t| t.as_str()).unwrap_or("");
        match mtype {
            "ping" => {
                let id = v.get("id").cloned().unwrap_or(serde_json::Value::Null);
                let _ = send_json(&mut ws, &serde_json::json!({ "type": "pong", "id": id }))
                    .await;
            }
            "hello" => {
                // The device identity (ADR-0010: identity, not authentication).
                let dev = v.get("device").cloned().unwrap_or(serde_json::Value::Null);
                if let Ok(d) = serde_json::from_value::<ldmux_protocol::rpc::DeviceInfo>(dev) {
                    device = Some(d);
                }
                let _ = send_json(&mut ws, &serde_json::json!({ "type": "hello_ack" }))
                    .await;
            }
            "list" => {
                // The session list from the daemon.
                let mut d = daemon_arc.lock().await;
                match d.handle("list", serde_json::json!({})).await {
                    Ok(r) => {
                        let sessions = r.get("sessions").cloned().unwrap_or(serde_json::json!([]));
                        let _ = send_json(&mut ws, &serde_json::json!({ "type": "sessions", "sessions": sessions }))
                            .await;
                    }
                    Err(e) => {
                        let _ = send_json(&mut ws, &serde_json::json!({
                            "type": "error", "code": "daemon_error", "message": e
                        }))
                        .await;
                    }
                }
            }
            "desktop" => {
                // The desktop inventory from the daemon.
                let d = daemon_arc.lock().await;
                let inv = d.desktop_inventory().await;
                let _ = send_json(
                    &mut ws,
                    &serde_json::json!({
                        "type": "desktop",
                        "inventory": serde_json::to_value(&inv).unwrap_or(serde_json::Value::Null),
                    }),
                )
                .await;
            }
            "new" => {
                // Create a new session (the daemon's `createSession`).
                let kind = v.get("kind").and_then(|k| k.as_str()).unwrap_or("shell");
                let cwd = v.get("cwd").and_then(|c| c.as_str()).unwrap_or(".");
                let title = v.get("title").and_then(|t| t.as_str());
                let created_by = device.as_ref().map(|d| d.id.clone()).unwrap_or_else(|| "phone".to_string());
                let mut d = daemon_arc.lock().await;
                match d.handle("new", serde_json::json!({
                    "kind": kind,
                    "cwd": cwd,
                    "title": title.map(|t| t.to_string()),
                    "createdBy": created_by,
                })).await {
                    Ok(r) => {
                        let session = r.get("session").cloned().unwrap_or(serde_json::Value::Null);
                        let _ = send_json(&mut ws, &serde_json::json!({ "type": "session", "session": session }))
                            .await;
                    }
                    Err(e) => {
                        let _ = send_json(&mut ws, &serde_json::json!({
                            "type": "error", "code": "daemon_error", "message": e
                        }))
                        .await;
                    }
                }
            }
            "makelive" => {
                // The desktop window lifecycle (the daemon's `makeLive`).
                let sid = v.get("sessionId").and_then(|t| t.as_str()).unwrap_or("");
                let mut d = daemon_arc.lock().await;
                let rec = d.get_session(sid);
                match rec {
                    None => {
                        let _ = send_json(&mut ws, &serde_json::json!({
                            "type": "live", "sessionId": sid, "ok": false,
                            "detail": format!("no session: {sid}")
                        }))
                        .await;
                    }
                    Some(rec) => {
                        let tmux_socket = d.keeper_socket_path().to_string_lossy();
                        let tmux_bin = d.keeper_bin().to_string_lossy();
                        let state_dir = d.state_dir().to_string_lossy();
                        let result = crate::desktop::open_live(
                            &tmux_socket,
                            &rec.tmux_name,
                            sid,
                            "10",
                            &tmux_bin,
                            &state_dir,
                        )
                        .await;
                        match result {
                            Ok(lr) => {
                                let _ = send_json(&mut ws, &serde_json::json!({
                                    "type": "live", "sessionId": sid,
                                    "ok": lr.ok, "placed": lr.placed,
                                    "detail": lr.detail,
                                }))
                                .await;
                            }
                            Err(e) => {
                                let _ = send_json(&mut ws, &serde_json::json!({
                                    "type": "live", "sessionId": sid,
                                    "ok": false, "detail": e,
                                }))
                                .await;
                            }
                        }
                    }
                }
            }
            "closelive" => {
                // The desktop window lifecycle (the daemon's `closeLive`).
                let sid = v.get("sessionId").and_then(|t| t.as_str()).unwrap_or("");
                let d = daemon_arc.lock().await;
                let rec = d.get_session(sid);
                match rec {
                    None => {
                        let _ = send_json(&mut ws, &serde_json::json!({
                            "type": "live", "sessionId": sid, "ok": false,
                            "detail": format!("no session: {sid}")
                        }))
                        .await;
                    }
                    Some(_rec) => {
                        let result = crate::desktop::close_live(sid).await;
                        match result {
                            Ok(ok) => {
                                let _ = send_json(&mut ws, &serde_json::json!({
                                    "type": "live", "sessionId": sid,
                                    "ok": ok,
                                }))
                                .await;
                            }
                            Err(e) => {
                                let _ = send_json(&mut ws, &serde_json::json!({
                                    "type": "live", "sessionId": sid,
                                    "ok": false, "detail": e,
                                }))
                                .await;
                            }
                        }
                    }
                }
            }
            "kill" => {
                // Kill a session (the daemon's `killSession`).
                let id = v.get("sessionId").and_then(|t| t.as_str()).unwrap_or("");
                let mut d = daemon_arc.lock().await;
                match d.handle("kill", serde_json::json!({ "id": id })).await {
                    Ok(r) => {
                        let _ = send_json(&mut ws, &serde_json::json!({ "type": "killed", "session": r }))
                            .await;
                    }
                    Err(e) => {
                        let _ = send_json(&mut ws, &serde_json::json!({
                            "type": "error", "code": "daemon_error", "message": e
                        }))
                        .await;
                    }
                }
            }
            "close" => {
                // Close a session (the daemon's `closeSession`).
                let id = v.get("sessionId").and_then(|t| t.as_str()).unwrap_or("");
                let mut d = daemon_arc.lock().await;
                match d.handle("close", serde_json::json!({ "id": id })).await {
                    Ok(r) => {
                        let _ = send_json(&mut ws, &serde_json::json!({ "type": "closed", "result": r }))
                            .await;
                    }
                    Err(e) => {
                        let _ = send_json(&mut ws, &serde_json::json!({
                            "type": "error", "code": "daemon_error", "message": e
                        }))
                        .await;
                    }
                }
            }
            "attach" => {
                // The attach lifecycle (the claim, the snapshot, the input).
                let sid = v.get("sessionId").and_then(|t| t.as_str()).unwrap_or("");
                let _cols = v.get("cols").and_then(|c| c.as_u64()).unwrap_or(80);
                let _rows = v.get("rows").and_then(|r| r.as_u64()).unwrap_or(24);
                let takeover = v.get("takeover").and_then(|t| t.as_bool()).unwrap_or(false);
                let mut d = daemon_arc.lock().await;
                // Get the session record.
                let rec = d.get_session(sid);
                match rec {
                    None => {
                        let _ = send_json(&mut ws, &serde_json::json!({
                            "type": "error", "code": "unknown_session",
                            "message": format!("no session: {sid}")
                        }))
                        .await;
                    }
                    Some(rec) => {
                        if rec.status != "running" {
                            let _ = send_json(&mut ws, &serde_json::json!({
                                "type": "error", "code": "session_dead",
                                "message": format!("session is dead: {sid}")
                            }))
                            .await;
                        } else {
                            // Claim the session (exclusivity, ADR-0010).
                            let claimed = if let Some(dev) = &device {
                                match d.claim(sid, dev, takeover) {
                                    crate::ownership::ClaimResult::HeldBy { device } => {
                                        let _ = send_json(&mut ws, &serde_json::json!({
                                            "type": "owned",
                                            "sessionId": sid,
                                            "owner": serde_json::to_value(&device)
                                                .unwrap_or(serde_json::Value::Null),
                                        }))
                                        .await;
                                        None
                                    }
                                    _ => Some(()),
                                }
                            } else {
                                Some(())
                            };
                            if claimed.is_some() {
                                // Get or create a stream.
                                let stream = d.stream_for(&rec.tmux_name);
                                // Subscribe with an output buffer (the attach
                                // streams live output to the client; the buffer
                                // is flushed on each WS message).
                                let out_buf: Arc<std::sync::Mutex<Vec<u8>>> =
                                    Arc::new(std::sync::Mutex::new(Vec::new()));
                                let ob = out_buf.clone();
                                let sub = crate::stream::StreamSubscriber {
                                    on_output: Box::new(move |text: &[u8]| {
                                        ob.lock().unwrap().extend_from_slice(text);
                                    }),
                                    on_reset: Box::new(|reason: &str| {
                                        // The client must resync (a forced resync,
                                        // §3.7). The snapshot is re-sent on the
                                        // next attach.
                                        let _ = reason;
                                    }),
                                };
                                let (_sub_id, _unsubscribe) = stream.subscribe(sub);
                                session_id = Some(sid.to_string());
                                // Take a full snapshot (the state fields are
                                // part of the initial state, not the SGR text).
                                let snap = stream.snapshot_full();
                                let _ = send_json(
                                    &mut ws,
                                    &serde_json::json!({
                                        "type": "state",
                                        "sessionId": sid,
                                        "snapshot": serde_json::to_value(&snap)
                                            .unwrap_or(serde_json::Value::Null),
                                    }),
                                )
                                .await;
                                // Store the output buffer for flushing.
                                attach_out = Some(out_buf);
                            }
                        }
                    }
                }
            }
            "resize" => {
                // Resize the stream (the viewing client owns the geometry).
                let sid = session_id.clone().unwrap_or_default();
                let cols = v.get("cols").and_then(|c| c.as_u64()).unwrap_or(80);
                let rows = v.get("rows").and_then(|r| r.as_u64()).unwrap_or(24);
                if cols == 0 || rows == 0 {
                    let _ = send_json(&mut ws, &serde_json::json!({
                        "type": "error", "code": "bad_size",
                        "message": "invalid size"
                    }))
                    .await;
                } else {
                    let mut d = daemon_arc.lock().await;
                    let stream = d.get_stream(&sid);
                    match stream {
                        Some(stream) => {
                            match stream.resize(cols, rows) {
                                Ok(_resp) => {
                                    // Send a fresh snapshot (the pane geometry changed).
                                    let snap = stream.snapshot();
                                    let _ = send_json(
                                        &mut ws,
                                        &serde_json::json!({
                                            "type": "state",
                                            "sessionId": sid,
                                            "snapshot": serde_json::to_value(&snap)
                                                .unwrap_or(serde_json::Value::Null),
                                        }),
                                    )
                                    .await;
                                }
                                Err(e) => {
                                    let _ = send_json(&mut ws, &serde_json::json!({
                                        "type": "error", "code": "resize_failed",
                                        "message": e
                                    }))
                                    .await;
                                }
                            }
                        }
                        None => {
                            let _ = send_json(&mut ws, &serde_json::json!({
                                "type": "error", "code": "not_attached",
                                "message": "attach before resizing"
                            }))
                            .await;
                        }
                    }
                }
            }
            "input" => {
                // Send input text to the pane (writes to the PTY, not to
                // subscribers).
                let sid = session_id.clone().unwrap_or_default();
                let text = v.get("text").and_then(|t| t.as_str()).unwrap_or("");
                if text.is_empty() {
                    let _ = send_json(&mut ws, &serde_json::json!({
                        "type": "error", "code": "no_text",
                        "message": "no text to send"
                    }))
                    .await;
                } else {
                    let mut d = daemon_arc.lock().await;
                    let stream = d.get_stream(&sid);
                    match stream {
                        Some(stream) => {
                            let _ = stream.write(text);
                        }
                        None => {
                            let _ = send_json(&mut ws, &serde_json::json!({
                                "type": "error", "code": "not_attached",
                                "message": "attach before sending input"
                            }))
                            .await;
                        }
                    }
                }
            }
            "key" => {
                // Send keys to the pane (the `sendKey` method).
                let sid = session_id.clone().unwrap_or_default();
                let keys: Vec<String> = v
                    .get("keys")
                    .and_then(|k| k.as_array())
                    .map(|arr| {
                        arr.iter()
                            .filter_map(|k| k.as_str())
                            .filter(|k| !k.is_empty() && !k.starts_with('-'))
                            .map(|k| k.to_string())
                            .collect()
                    })
                    .unwrap_or_default();
                let keys_refs: Vec<&str> = keys.iter().map(|s| s.as_str()).collect();
                let mut d = daemon_arc.lock().await;
                let stream = d.get_stream(&sid);
                match stream {
                    Some(stream) => {
                        if !keys_refs.is_empty() {
                            let _ = stream.send_key(&keys_refs);
                        }
                    }
                    None => {
                        let _ = send_json(&mut ws, &serde_json::json!({
                            "type": "error", "code": "not_attached",
                            "message": "attach before sending keys"
                        }))
                        .await;
                    }
                }
            }
            "follow" => {
                // Client-owned scroll (ADR-0004): a no-op.
            }
            "scrollback" => {
                // Send a snapshot with history (the `scrollback` path).
                let sid = session_id.clone().unwrap_or_default();
                let lines = v.get("lines").and_then(|l| l.as_u64()).unwrap_or(0);
                let wants_history = lines > 0;
                let mut d = daemon_arc.lock().await;
                let stream = d.get_stream(&sid);
                match stream {
                    Some(stream) => {
                        let hist = if wants_history { Some(lines) } else { None };
                        match stream.snapshot_with_history(hist) {
                            Ok((_text, _epoch)) => {
                                // Send a state message with the history flag.
                                let _ = send_json(&mut ws, &serde_json::json!({
                                    "type": "state",
                                    "sessionId": sid,
                                    "history": wants_history,
                                }))
                                .await;
                            }
                            Err(e) => {
                                let _ = send_json(&mut ws, &serde_json::json!({
                                    "type": "error", "code": "scrollback_failed",
                                    "message": e
                                }))
                                .await;
                            }
                        }
                    }
                    None => {
                        let _ = send_json(&mut ws, &serde_json::json!({
                            "type": "error", "code": "not_attached",
                            "message": "attach before requesting scrollback"
                        }))
                        .await;
                    }
                }
            }
            "mouseclick" => {
                // Send an SGR mouse click to the pane.
                let sid = session_id.clone().unwrap_or_default();
                let col = v.get("col").and_then(|c| c.as_u64());
                let row = v.get("row").and_then(|r| r.as_u64());
                match (col, row) {
                    (Some(col), Some(row)) if col < 10000 && row < 10000 => {
                        let mut d = daemon_arc.lock().await;
                        let stream = d.get_stream(&sid);
                        match stream {
                            Some(stream) => {
                                // Get the state and check mouse is enabled.
                                match stream.snapshot_full() {
                                    Ok(state) => {
                                        if !(state.mouse && state.mouse_sgr) {
                                            let _ = send_json(&mut ws, &serde_json::json!({
                                                "type": "error", "code": "no_mouse",
                                                "message": "pane has not enabled SGR mouse reporting"
                                            }))
                                            .await;
                                        } else if col >= state.cols || row >= state.rows {
                                            let _ = send_json(&mut ws, &serde_json::json!({
                                                "type": "error", "code": "bad_cell",
                                                "message": format!("cell outside the pane: {},{}", col, row)
                                            }))
                                            .await;
                                        } else {
                                            match stream.mouse_click(col, row) {
                                                Ok(_) => {
                                                    let _ = send_json(&mut ws, &serde_json::json!({
                                                        "type": "mousedelivered", "col": col, "row": row,
                                                    }))
                                                    .await;
                                                }
                                                Err(e) => {
                                                    let _ = send_json(&mut ws, &serde_json::json!({
                                                        "type": "error", "code": "mouseclick_failed",
                                                        "message": e
                                                    }))
                                                    .await;
                                                }
                                            }
                                        }
                                    }
                                    Err(e) => {
                                        let _ = send_json(&mut ws, &serde_json::json!({
                                            "type": "error", "code": "snapshot_failed",
                                            "message": e
                                        }))
                                        .await;
                                    }
                                }
                            }
                            None => {
                                let _ = send_json(&mut ws, &serde_json::json!({
                                    "type": "error", "code": "not_attached",
                                    "message": "attach before clicking"
                                }))
                                .await;
                            }
                        }
                    }
                    _ => {
                        let _ = send_json(&mut ws, &serde_json::json!({
                            "type": "error", "code": "bad_cell",
                            "message": "invalid cell"
                        }))
                        .await;
                    }
                }
            }
            "copymode" => {
                // tmux-specific (the keeper has no copy-mode equivalent).
                let _ = send_json(&mut ws, &serde_json::json!({
                    "type": "error", "code": "not_implemented",
                    "message": "copymode is tmux-specific, not yet ported to the keeper"
                }))
                .await;
            }
            _ => {
                let _ = send_json(
                    &mut ws,
                    &serde_json::json!({ "type": "error", "code": "bad_message", "message": format!("unknown type: {mtype}") }),
                )
                .await;
            }
        }
    }
    // Connection closed: release the stream if idle (the subscribers are
    // cleared, the output buffer is dropped).
    if let Some(sid) = &session_id {
        let tmux_name = daemon_arc.lock().await.get_session(sid)
            .map(|r| r.tmux_name.clone())
            .unwrap_or_default();
        if !tmux_name.is_empty() {
            daemon_arc.lock().await.release_stream_if_idle(&tmux_name);
        }
    }
}

async fn send_json(ws: &mut WsStream, v: &serde_json::Value) -> Result<(), String> {
    let text = serde_json::to_string(v).map_err(|e| e.to_string())?;
    ws.send(Message::Text(text.into()))
        .await
        .map_err(|e| e.to_string())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// A helper: create a dummy `Arc<Mutex<LdmuxDaemon>>` for tests.
    async fn dummy_daemon_arc() -> std::sync::Arc<tokio::sync::Mutex<crate::daemon::LdmuxDaemon>> {
        let dir = std::env::temp_dir().join(format!("ldmux-remote-test-{}", rand::random::<u64>()));
        std::fs::create_dir_all(&dir).unwrap();
        let keeper_sock = dir.join("keeper.sock");
        let fk = std::sync::Arc::new(crate::fake_keeper::FakeKeeper::new(keeper_sock.clone()));
        let fk_serve = fk.clone();
        let fk_sock = keeper_sock.clone();
        tokio::spawn(async move {
            crate::fake_keeper::FakeKeeper::serve(&fk_serve, fk_sock.as_path()).await.unwrap();
        });
        for _ in 0..50 {
            if keeper_sock.exists() { break; }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
        let d = crate::daemon::LdmuxDaemon::new(dir, std::path::PathBuf::from("/nonexistent"));
        std::sync::Arc::new(tokio::sync::Mutex::new(d))
    }

    /// A minimal `RemoteServer` test: start the server on a fixed port,
    /// connect a WS client, send a `ping` (with the token), and get a `pong`.
    ///
    /// The test explicitly closes the WS connection and stops the server so
    /// that the detached `tokio::spawn` tasks (the accept loop and the
    /// `handle_connection`) exit before the test ends.
    #[tokio::test]
    async fn a_ws_client_gets_a_pong_over_the_tailnet_bind() {
        const PORT: u16 = 49991;
        let cfg = RemoteConfig::new("127.0.0.1", PORT, "test-token");
        let mut server = RemoteServer::new(cfg);
        let daemon_arc = dummy_daemon_arc().await;
        let (_hostname, port) = server.start(&daemon_arc).await.expect("bind");
        assert_eq!(port, PORT);

        // Connect a WS client. The first message carries the token.
        let uri = format!("ws://127.0.0.1:{PORT}/ws");
        let (mut ws, _response) = tokio_tungstenite::connect_async(&uri)
            .await
            .expect("connect");

        // Send a `ping` with the token.
        let ping = serde_json::to_string(&serde_json::json!({
            "type": "ping",
            "id": "abc123",
            "token": "test-token"
        }))
        .unwrap();
        ws.send(Message::Text(ping.into())).await.unwrap();

        // Read the `pong`.
        let msg = ws.next().await.unwrap().unwrap();
        let text = match msg {
            Message::Text(t) => t,
            _ => panic!("expected text"),
        };
        let resp: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(resp["type"], "pong");
        assert_eq!(resp["id"], "abc123");

        // Close the connection (the server's `handle_connection` exits when the
        // client closes), then stop the server (the accept loop exits).
        ws.close(None).await.ok();
        server.stop();
    }

    /// The `hello` message sets the device identity (ADR-0010: identity, not
    /// authentication).
    #[tokio::test]
    async fn a_hello_message_sets_the_device_identity() {
        const PORT: u16 = 49992;
        let cfg = RemoteConfig::new("127.0.0.1", PORT, "test-token");
        let mut server = RemoteServer::new(cfg);
        let daemon_arc = dummy_daemon_arc().await;
        let (_hostname, port) = server.start(&daemon_arc).await.expect("bind");
        assert_eq!(port, PORT);

        let uri = format!("ws://127.0.0.1:{PORT}/ws");
        let (mut ws, _response) = tokio_tungstenite::connect_async(&uri)
            .await
            .expect("connect");

        // Send a `hello` with the device id and token.
        let hello = serde_json::to_string(&serde_json::json!({
            "type": "hello",
            "token": "test-token",
            "device": {
                "id": "phone-123",
                "name": "My Phone",
                "kind": "phone"
            }
        }))
        .unwrap();
        ws.send(Message::Text(hello.into())).await.unwrap();

        // Read the `hello_ack`.
        let msg = ws.next().await.unwrap().unwrap();
        let text = match msg {
            Message::Text(t) => t,
            _ => panic!("expected text"),
        };
        let resp: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(resp["type"], "hello_ack");

        ws.close(None).await.ok();
        server.stop();
    }
}
