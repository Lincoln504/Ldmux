//! The remote server: the phone-facing WebSocket endpoint (the tailnet address
//! + bearer-token auth).
//!
//! Port of `packages/daemon/src/remote.ts` (the source of record). The phone
//! connects over the tailnet address with the bearer token **at the HTTP
//! upgrade**; terminal output travels as binary OUTPUT frames (`frames.rs`);
//! control/handshake is JSON text.
//!
//! One connection = one attached session (open N connections to view N
//! sessions). On attach: subscribe first (queueing), snapshot, send CAPS +
//! SNAPSHOT, then flush — so no output is lost across the snapshot boundary.
//!
//! ## The message surface (the exact shapes the Kotlin client parses)
//!
//! - `caps` + `snapshot` (always a pair) answer `attach`, `resize`, `scrollback`.
//! - `sessions` answers `list`, `new`, `kill`, `close`.
//! - `created` (before `sessions`) answers `new`.
//! - `owned` answers a refused/taken claim; `live` answers make/close-live.
//! - `pong` answers `ping`; `error` carries a code + message.
//! - output is `FrameType::Output` (0x10) as a binary frame.
//!
//! ## Ownership (ADR-0010)
//!
//! `byDevice` maps a device id to its live connections; a claim is released
//! after a grace on disconnect (a reconnect within the grace still holds the
//! shell). A displaced device is told `owned` and cut off. Only the holder may
//! write (`input`/`key`/`mouseclick`).
//!
//! ## Flow control
//!
//! Output is coalesced into one frame per 8 ms (or 4 KiB), the input is capped
//! at 16 KiB, keys at 32, and a connection silent for 20 s is reaped.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use futures_util::stream::StreamExt;
use futures_util::SinkExt;
use ldmux_protocol::frames::{encode_frame, t as ft};
use ldmux_protocol::messages::PROTOCOL_VERSION;
use ldmux_protocol::net::is_tailnet_or_loopback;
use ldmux_protocol::rpc::{DeviceInfo, SessionOwner, SessionRecord};
use tokio::io::AsyncWriteExt;
use tokio::net::{TcpListener, TcpStream};
use tokio_tungstenite::tungstenite::Message;

use crate::daemon::now_ms;
use crate::stream::{PaneStream, StreamSubscriber};

/// The WS stream type (`accept_async` returns this).
type WsStream = tokio_tungstenite::WebSocketStream<TcpStream>;

/// The daemon handle shared with the connection tasks.
type DaemonArc = Arc<tokio::sync::Mutex<crate::daemon::LdmuxDaemon>>;

/// Messages that change what is in a shell, and so are only the holder's to
/// send (the TS `WRITES`).
const WRITES: [&str; 3] = ["input", "key", "mouseclick"];

/// Per-type caps (the TS `MAX_INPUT_BYTES` / `MAX_KEYS`).
const MAX_INPUT_BYTES: usize = 16 * 1024;
const MAX_KEYS: usize = 32;
/// Output queued between subscribe and snapshot is bounded before we resync.
const MAX_PENDING_BYTES: usize = 1024 * 1024;
/// Output coalescing window, and the size that flushes it early.
const OUTPUT_FLUSH_MS: u64 = 8;
const OUTPUT_FLUSH_BYTES: usize = 4 * 1024;
/// Unsent output on one socket past this means the link, not the terminal, is
/// behind — answer `resync` instead of building a history the client cannot show.
const WS_BACKLOG_BYTES: usize = 64 * 1024;
/// Silence that means the other end has gone.
const WS_IDLE_MS: u64 = 20_000;
/// How long a closed socket's claims wait for the same device to come back.
/// The same grace an unbacked claim gets ([`crate::daemon::CLAIM_GRACE_MS`]).
const RELEASE_GRACE_MS: u64 = crate::daemon::CLAIM_GRACE_MS;
/// How often claims are checked for a connection behind them.
const CLAIM_TICK_MS: u64 = 1_000;

/// The remote config: the hostname (the tailnet address), the port, and the
/// bearer token.
#[derive(Clone)]
pub struct RemoteConfig {
    /// The hostname to bind (the tailnet address, e.g. `100.127.119.54`).
    pub hostname: String,
    /// The port to bind (e.g. `7623`).
    pub port: u16,
    /// The bearer token (checked at the HTTP upgrade).
    pub token: String,
    /// Reap a connection that has said nothing for this long.
    ///
    /// A field rather than only the const so the keep-alive contract can be
    /// tested in milliseconds. It is a real contract: a client that does not
    /// speak on a schedule is closed, and the desktop attach client did not, so
    /// its window exited about 20 s after it opened.
    pub idle_ms: u64,
}

impl RemoteConfig {
    pub fn new(hostname: impl Into<String>, port: u16, token: impl Into<String>) -> Self {
        Self {
            hostname: hostname.into(),
            port,
            token: token.into(),
            idle_ms: WS_IDLE_MS,
        }
    }

    /// The idle deadline for tests: a real value, shortened.
    pub fn with_idle_ms(mut self, idle_ms: u64) -> Self {
        self.idle_ms = idle_ms.max(1);
        self
    }
}

/// One outbound message for a connection's writer/coalescer.
enum Out {
    /// Pane output bytes (coalesced).
    Output(Vec<u8>),
    /// A JSON control message (flushes pending output first, so it cannot
    /// overtake output it should follow).
    Json(String),
    /// Close the connection (the sweep, or `stop`).
    Close,
}

/// The connection's mutable state, shared between the reader task and the
/// output callback (which runs on the stream's poll thread).
struct ConnInner {
    disposed: bool,
    /// Whether live output is flowing (false between subscribe and the snapshot).
    live: bool,
    /// Incremented per attach; a superseded attach must not commit.
    attach_gen: u64,
    session_id: Option<String>,
    record: Option<SessionRecord>,
    stream: Option<Arc<PaneStream>>,
    unsubscribe: Option<Box<dyn Fn() + Send + Sync>>,
    /// This connection's subscriber id on `stream` (so a snapshot can leave it
    /// out of the output tail it already contains).
    sub_id: Option<usize>,
    device: Option<DeviceInfo>,
    /// Output queued before `live` (bounded by `MAX_PENDING_BYTES`).
    pending: Vec<u8>,
    pending_bytes: usize,
    resyncing: bool,
    last_seen_ms: u64,
}

/// A live connection: the outbound channel, the shared state, and the count of
/// bytes produced but not yet written to the wire (the backlog probe).
struct ConnShared {
    tx: tokio::sync::mpsc::UnboundedSender<Out>,
    inner: Mutex<ConnInner>,
    outstanding: AtomicUsize,
}

impl ConnShared {
    fn send_json(&self, value: &serde_json::Value) {
        let _ = self.tx.send(Out::Json(value.to_string()));
    }

    fn send_error(&self, code: &str, message: &str) {
        self.send_json(&serde_json::json!({
            "type": "error", "code": code, "message": message
        }));
    }
}

/// The server-shared state: the connections table + the daemon + the token.
pub(crate) struct Shared {
    conns: Mutex<HashMap<u64, Arc<ConnShared>>>,
    daemon: DaemonArc,
    token: String,
    idle_ms: u64,
    stopped: AtomicBool,
}

/// The remote server: the phone-facing WebSocket endpoint.
pub struct RemoteServer {
    cfg: RemoteConfig,
    /// The port actually bound, which is `cfg.port` unless that was 0.
    bound_port: u16,
    shared: Option<Arc<Shared>>,
    accept: Option<tokio::task::JoinHandle<()>>,
    sweep: Option<tokio::task::JoinHandle<()>>,
}

impl RemoteServer {
    pub fn new(cfg: RemoteConfig) -> Self {
        Self {
            bound_port: cfg.port,
            cfg,
            shared: None,
            accept: None,
            sweep: None,
        }
    }

    /// Bind the WS server on the tailnet address and begin serving.
    pub async fn start(&mut self, daemon_arc: &DaemonArc) -> Result<(String, u16), String> {
        let addr = format!("{}:{}", self.cfg.hostname, self.cfg.port);
        // Refused here, at the one place that listens (the TS `start` check).
        if !is_tailnet_or_loopback(&self.cfg.hostname) {
            return Err(format!(
                "refusing to listen on {}: not a tailnet or loopback address",
                self.cfg.hostname
            ));
        }
        let listener = TcpListener::bind(&addr)
            .await
            .map_err(|e| format!("bind {addr}: {e}"))?;
        // The port the kernel actually gave us. A configured port of 0 means
        // "any free one", so the configured number is not the answer to where
        // this is listening — and reporting 0 to a pairing client would send the
        // phone nowhere. It is also what lets a test ask for a port nobody else
        // can have, rather than picking a number and hoping.
        self.bound_port = listener
            .local_addr()
            .map(|a| a.port())
            .unwrap_or(self.cfg.port);
        let shared = Arc::new(Shared {
            conns: Mutex::new(HashMap::new()),
            daemon: daemon_arc.clone(),
            token: self.cfg.token.clone(),
            idle_ms: self.cfg.idle_ms,
            stopped: AtomicBool::new(false),
        });
        let accept_shared = shared.clone();
        let accept = tokio::spawn(async move {
            accept_loop(listener, accept_shared).await;
        });
        let sweep_shared = shared.clone();
        let sweep = tokio::spawn(async move {
            // Two cadences, because they answer to different things. Unbacked
            // claims are checked on their own tick — a window that is on screen
            // waiting for one is the reader, and `idle_ms / 4` is five seconds of
            // it. Reaping silent connections can stay as slow as the deadline it
            // is against, and the two share a task because both walk the same
            // connection list.
            let sweep_step = (sweep_shared.idle_ms / 4).max(25);
            let mut since_sweep = 0u64;
            loop {
                tokio::time::sleep(std::time::Duration::from_millis(CLAIM_TICK_MS)).await;
                if sweep_shared.stopped.load(Ordering::Relaxed) {
                    break;
                }
                reconcile_claims(&sweep_shared).await;
                since_sweep += CLAIM_TICK_MS;
                if since_sweep >= sweep_step {
                    since_sweep = 0;
                    sweep_once(&sweep_shared).await;
                }
            }
        });
        self.shared = Some(shared);
        self.accept = Some(accept);
        self.sweep = Some(sweep);
        Ok((self.cfg.hostname.clone(), self.bound_port))
    }

    /// The port this server is listening on.
    pub fn port(&self) -> u16 {
        self.bound_port
    }

    /// Stop the server: mark stopped, tell every connection to close.
    /// The connections, for the daemon's maintenance loop to push through.
    pub(crate) fn shared(&self) -> Option<Arc<Shared>> {
        self.shared.clone()
    }

    /// The remote's live state, for tests.
    pub fn is_listening(&self) -> bool {
        self.shared.is_some()
    }

    pub fn stop(&mut self) {
        if let Some(shared) = &self.shared {
            shared.stopped.store(true, Ordering::Relaxed);
            let conns = shared.conns.lock().unwrap();
            for c in conns.values() {
                let _ = c.tx.send(Out::Close);
            }
        }
        if let Some(h) = self.accept.take() {
            h.abort();
        }
        if let Some(h) = self.sweep.take() {
            h.abort();
        }
    }
}

impl Drop for RemoteServer {
    fn drop(&mut self) {
        self.stop();
    }
}

/// The accept loop: auth at the upgrade, then one task per connection.
async fn accept_loop(listener: TcpListener, shared: Arc<Shared>) {
    let mut conn_id: u64 = 0;
    loop {
        if shared.stopped.load(Ordering::Relaxed) {
            break;
        }
        let (mut stream, peer) = match listener.accept().await {
            Ok(v) => v,
            Err(_) => {
                if shared.stopped.load(Ordering::Relaxed) {
                    break;
                }
                continue;
            }
        };
        // Before the token: a peer that is not on the tailnet is not a client,
        // however it reached this socket (the TS upgrade handler).
        if !is_tailnet_or_loopback(&peer.ip().to_string()) {
            write_http(&mut stream, 403, "forbidden\n").await;
            continue;
        }
        conn_id += 1;
        let shared = shared.clone();
        // Everything past the cheap peer check runs on its own task. The head
        // read can block for its full deadline, and doing the pre-auth work
        // inline in the accept loop let one silent peer stall every client
        // behind it for that whole deadline.
        tokio::spawn(async move {
            serve_accepted(stream, peer, shared, conn_id).await;
        });
    }
}

/// The pre-auth and upgrade half of a connection, on its own task.
///
/// Returns without ceremony when the peer is not a client: an unanswered head,
/// a plain GET, a wrong path, a bad token, or a failed upgrade.
async fn serve_accepted(
    mut stream: TcpStream,
    peer: std::net::SocketAddr,
    shared: Arc<Shared>,
    conn_id: u64,
) {
    // Peek the request head (without consuming it) so a plain HTTP GET is
    // answered rather than left to the WebSocket handshake to fail — the
    // phone's `probe()` reads the status to tell a refused token (401) from
    // a machine that is off.
    let head = match read_head(&stream).await {
        Some(h) => h,
        None => return,
    };
    let Some((_method, target, headers)) = parse_request_head(&head) else {
        write_http(&mut stream, 400, "bad request\n").await;
        return;
    };
    let path = path_of(&target);
    let authed = authorized(&headers, &target, &shared.token);
    if !is_ws_upgrade(&headers) {
        // The TS `handleRequest` statuses.
        let (status, body) = plain_response(&path, authed);
        eprintln!("ldmuxd: remote: {peer} {_method} {path} -> {status}");
        write_http(&mut stream, status, body).await;
        return;
    }
    if path != "/ws" {
        eprintln!("ldmuxd: remote: {peer} ws {path} -> 404");
        write_http(&mut stream, 404, "ldmux remote: the endpoint is /ws\n").await;
        return;
    }
    if !authed {
        eprintln!("ldmuxd: remote: {peer} ws /ws -> 401");
        write_http(&mut stream, 401, "unauthorized\n").await;
        return;
    }
    // The inbound frame cap. The TypeScript server was `new WebSocketServer({
    // maxPayload: 64 * 1024 })`, which closed the socket on anything larger; the
    // port lost it and inherited tungstenite's 64 MiB default, so a client could
    // make the daemon buffer 64 MiB in one message. The limit is on *reads* only
    // (tungstenite enforces `max_message_size` when it decodes a message, and
    // `max_frame_size` in `read_message_frame`), so server→client snapshots are
    // unaffected however large they are. 64 KiB is far above every client
    // message: `input` is capped at 16 KiB by the handler, `key` at 32 names.
    let ws_config = tokio_tungstenite::tungstenite::protocol::WebSocketConfig::default()
        .max_message_size(Some(64 * 1024))
        .max_frame_size(Some(64 * 1024));
    let ws = match tokio_tungstenite::accept_async_with_config(stream, Some(ws_config)).await {
        Ok(ws) => ws,
        Err(e) => {
            eprintln!("ldmuxd: remote: upgrade failed from {peer}: {e}");
            return;
        }
    };
    eprintln!("ldmuxd: remote: connection from {peer}");
    handle_connection(ws, shared, conn_id).await;
}

/// The per-connection loop: read messages, enqueue responses, coalesce output.
async fn handle_connection(ws: WsStream, shared: Arc<Shared>, conn_id: u64) {
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<Out>();
    let conn = Arc::new(ConnShared {
        tx,
        inner: Mutex::new(ConnInner {
            disposed: false,
            live: false,
            attach_gen: 0,
            session_id: None,
            record: None,
            stream: None,
            unsubscribe: None,
            sub_id: None,
            device: None,
            pending: Vec::new(),
            pending_bytes: 0,
            resyncing: false,
            last_seen_ms: now_ms(),
        }),
        outstanding: AtomicUsize::new(0),
    });
    shared
        .conns
        .lock()
        .unwrap()
        .insert(conn_id, conn.clone());

    // The connection loop owns the socket; outbound goes through `tx` so the
    // coalescer and every control message share one ordering.
    let mut ws = ws;
    let mut pending: Vec<u8> = Vec::new();
    loop {
        // Arm the coalescing timer only when there is output to flush.
        let mut timer: std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>> =
            if pending.is_empty() {
                Box::pin(std::future::pending())
            } else {
                Box::pin(tokio::time::sleep(std::time::Duration::from_millis(
                    OUTPUT_FLUSH_MS,
                )))
            };
        tokio::select! {
            maybe = ws.next() => {
                match maybe {
                    None => break,
                    Some(Err(_)) => break,
                    Some(Ok(msg)) => {
                        // Flush buffered output before acting on the message, so a
                        // reply cannot overtake the output it should follow.
                        flush_output(&mut ws, &mut pending, &conn).await;
                        if let Some(text) = message_text(&msg) {
                            touch(&conn);
                            handle_message(&text, &conn, &shared).await;
                        }
                    }
                }
            }
            out = rx.recv() => {
                match out {
                    None => break,
                    Some(Out::Close) => break,
                    Some(Out::Output(b)) => {
                        pending.extend_from_slice(&b);
                        if pending.len() >= OUTPUT_FLUSH_BYTES {
                            flush_output(&mut ws, &mut pending, &conn).await;
                        }
                    }
                    Some(Out::Json(s)) => {
                        flush_output(&mut ws, &mut pending, &conn).await;
                        let _ = ws.send(Message::Text(s.into())).await;
                    }
                }
            }
            _ = &mut timer => {
                flush_output(&mut ws, &mut pending, &conn).await;
            }
        }
    }

    // The connection ended. Clean up: unsubscribe, release the stream, and
    // release the device's claims after the grace.
    disconnect(conn, shared).await;
}

/// Send every buffered output frame, then release those bytes from the
/// connection's `outstanding` count (they have left the daemon).
async fn flush_output(ws: &mut WsStream, pending: &mut Vec<u8>, conn: &ConnShared) {
    if pending.is_empty() {
        return;
    }
    let data = std::mem::take(pending);
    let n = data.len();
    match encode_frame(ft::OUTPUT, &data, 0) {
        Ok(frame) => {
            let _ = ws.send(Message::Binary(frame.into())).await;
        }
        Err(_) => {
            let _ = ws
                .send(Message::Text(
                    serde_json::json!({"type":"error","code":"resync","message":"output frame too large"})
                        .to_string()
                        .into(),
                ))
                .await;
        }
    }
    // Only now are the bytes off the daemon: a client that is not reading holds
    // this await open, and `outstanding` must keep counting them meanwhile.
    conn.outstanding.fetch_sub(n, Ordering::Relaxed);
}

/// Mark the connection seen (any inbound message counts).
fn touch(conn: &ConnShared) {
    conn.inner.lock().unwrap().last_seen_ms = now_ms();
}

/// Hand pane output to the connection's writer, or — when the link is behind —
/// refuse it and ask the client to resync (the TS `queueOutput` backlog rule).
///
/// `outstanding` is the bytes queued for this socket and not yet written. Past
/// `WS_BACKLOG_BYTES` the client is not keeping up, and building a history it
/// cannot display only grows memory: the queue is unbounded, so without this a
/// phone on a bad link made the daemon buffer a busy shell's whole output. It is
/// said once per backlog (`resyncing`), because a client that is already
/// re-attaching must not be told again on every write; a fresh `attach` clears
/// the flag and its snapshot is the recovery.
fn queue_output(conn: &ConnShared, inner: &mut ConnInner, bytes: &[u8]) {
    if inner.resyncing {
        return;
    }
    if conn.outstanding.load(Ordering::Relaxed) > WS_BACKLOG_BYTES {
        inner.resyncing = true;
        let _ = conn.tx.send(Out::Json(
            serde_json::json!({"type":"error","code":"resync","message":"client is behind the link"})
                .to_string(),
        ));
        return;
    }
    conn.outstanding.fetch_add(bytes.len(), Ordering::Relaxed);
    if conn.tx.send(Out::Output(bytes.to_vec())).is_err() {
        // The writer is gone: nothing will ever drain this.
        conn.outstanding.fetch_sub(bytes.len(), Ordering::Relaxed);
    }
}

/// Handle one control message (the TS `onMessage` dispatch).
async fn handle_message(text: &str, conn: &Arc<ConnShared>, shared: &Arc<Shared>) {
    let v: serde_json::Value = match serde_json::from_str(text) {
        Ok(v) => v,
        Err(_) => {
            conn.send_error("bad_json", "invalid JSON");
            return;
        }
    };
    let mtype = v.get("type").and_then(|t| t.as_str()).unwrap_or("").to_string();

    // Writing to a shell is the holder's (the TS `WRITES` / `mayWrite`).
    if WRITES.contains(&mtype.as_str()) {
        let (device, session) = {
            let inner = conn.inner.lock().unwrap();
            (inner.device.clone(), inner.session_id.clone())
        };
        if let (Some(device), Some(session)) = (device, session) {
            let holds = {
                let d = shared.daemon.lock().await;
                d.holds(&session, &device.id)
            };
            if !holds {
                conn.send_error("not_holder", "this device does not hold that shell");
                return;
            }
        }
    }

    match mtype.as_str() {
        "ping" => {
            let id = v.get("id").cloned().unwrap_or(serde_json::Value::Null);
            conn.send_json(&serde_json::json!({"type":"pong","id":id}));
        }
        "hello" => {
            // Identity, not authentication (ADR-0010).
            let dev = v.get("device");
            if let Some(dev) = dev {
                let id = dev.get("id").and_then(|x| x.as_str()).unwrap_or("");
                if !id.is_empty() {
                    let id = id.chars().take(64).collect::<String>();
                    let name = dev
                        .get("name")
                        .and_then(|x| x.as_str())
                        .unwrap_or("device")
                        .chars()
                        .take(32)
                        .collect::<String>();
                    let kind = if dev.get("kind").and_then(|x| x.as_str()) == Some("desktop") {
                        "desktop"
                    } else {
                        "phone"
                    };
                    let previous = conn.inner.lock().unwrap().device.replace(DeviceInfo {
                        id: id.clone(),
                        name: if name.is_empty() { "device".into() } else { name },
                        kind: kind.into(),
                    });
                    // A second `hello` under a different id means this socket is
                    // no longer the device it was. If no other socket still is,
                    // that identity's claims have no holder and nothing else
                    // would free them (they would block everyone else until the
                    // daemon restarts).
                    if let Some(prev) = previous.filter(|p| p.id != id) {
                        let still_held = shared.conns.lock().unwrap().values().any(|c| {
                            c.inner.lock().unwrap().device.as_ref().map(|d| d.id == prev.id).unwrap_or(false)
                        });
                        if !still_held {
                            let freed = !shared.daemon.lock().await.release_device(&prev.id).is_empty();
                            if freed {
                                broadcast_sessions(shared).await;
                            }
                        }
                    }
                }
            }
        }
        "list" => {
            let sessions = {
                let mut d = shared.daemon.lock().await;
                d.describe_sessions().await
            };
            conn.send_json(&sessions_message(&sessions));
        }
        "desktop" => {
            let inv = {
                let mut d = shared.daemon.lock().await;
                d.desktop_inventory().await
            };
            conn.send_json(&serde_json::json!({"type":"desktop","inventory":inv}));
        }
        "new" => {
            let kind = v.get("kind").and_then(|x| x.as_str()).unwrap_or("shell");
            let kind = if kind == "pi" { "pi" } else { "shell" };
            let cwd = v.get("cwd").and_then(|x| x.as_str());
            let title = v.get("title").and_then(|x| x.as_str());
            // Who opened it: what the client says, else who the client is.
            //
            // This was the literal `"phone"` for **every** remote client, so a
            // desktop that connected over the wire had its shells recorded as the
            // phone's — and `createdBy` is not decoration: the launcher shows its
            // per-shell menu (close, send to desktop) only for a shell it opened,
            // and `seizing_closes_the_desk_window` is true only for `"phone"`. A
            // desktop client's shells were therefore offered to the phone as its
            // own. The connection already said what it was (`hello`), so the
            // record now follows the connection: a client that sends `createdBy`
            // still wins, and everything else falls back to its device kind.
            let origin = v
                .get("createdBy")
                .and_then(|x| x.as_str())
                .filter(|s| !s.is_empty())
                .map(|s| s.to_string())
                .or_else(|| {
                    conn.inner
                        .lock()
                        .unwrap()
                        .device
                        .as_ref()
                        .map(|d| d.kind.clone())
                        .filter(|k| !k.is_empty())
                })
                .unwrap_or_else(|| "local".to_string());
            let record = {
                let mut d = shared.daemon.lock().await;
                let fallback = std::env::var("HOME").unwrap_or_else(|_| ".".into());
                d.create_session(kind, cwd.unwrap_or(&fallback), title, &origin)
                    .await
            };
            match record {
                Ok(record) => {
                    // Before the list, so a client that opens what it asked for
                    // has the id by the time the list arrives.
                    conn.send_json(&serde_json::json!({
                        "type": "created", "sessionId": record.id, "cwd": record.cwd
                    }));
                    // The list is news for every client, not just the one that
                    // asked: a shell opened from the desk CLI has to appear on the
                    // phone, and a phone that opened one has to appear on the
                    // other phone. This used to go to the asking connection
                    // alone, so everyone else waited for the next maintenance tick
                    // (2 s) — and only then if some mutator had remembered to set
                    // `sessions_dirty`.
                    broadcast_sessions(shared).await;
                }
                Err(e) => conn.send_error("handler_error", &e),
            }
        }
        "makelive" => {
            let id = v.get("sessionId").and_then(|x| x.as_str()).unwrap_or("").to_string();
            let workspace = v.get("workspace").and_then(|x| x.as_str()).unwrap_or("");
            let result = {
                let mut d = shared.daemon.lock().await;
                d.handle("makeLive", serde_json::json!({"id": id, "workspace": workspace}))
                    .await
            };
            let (ok, detail) = live_result(result);
            conn.send_json(&serde_json::json!({
                "type": "live", "sessionId": id, "ok": ok, "detail": detail
            }));
            if ok {
                broadcast_sessions(shared).await;
            }
        }
        "closelive" => {
            let id = v.get("sessionId").and_then(|x| x.as_str()).unwrap_or("").to_string();
            let result = {
                let mut d = shared.daemon.lock().await;
                d.handle("closeLive", serde_json::json!({"id": id})).await
            };
            let (ok, detail) = live_result(result);
            conn.send_json(&serde_json::json!({
                "type": "live", "sessionId": id, "ok": ok, "detail": detail
            }));
            broadcast_sessions(shared).await;
        }
        "kill" => {
            let id = v.get("sessionId").and_then(|x| x.as_str()).unwrap_or("");
            let reason = ended_by("killed", device_of(conn).as_ref());
            {
                let mut d = shared.daemon.lock().await;
                let _ = d.kill_session(id, &reason).await;
            }
            broadcast_sessions(shared).await;
        }        "close" => {
            let id = v.get("sessionId").and_then(|x| x.as_str()).unwrap_or("").to_string();
            let device = conn.inner.lock().unwrap().device.clone();
            // A shell opened at the desk belongs to the desk: closing it from a
            // phone is putting it down, not destroying it (ADR-0010).
            let record = {
                let d = shared.daemon.lock().await;
                d.get_session(&id)
            };
            let phone_owned_closed = device.as_ref().map(|dev| dev.kind == "phone").unwrap_or(false)
                && record.as_ref().map(|r| r.created_by != "phone").unwrap_or(false);
            if phone_owned_closed {
                if let Some(dev) = &device {
                    let mut d = shared.daemon.lock().await;
                    d.release_claim(&id, &dev.id);
                }
                stop_viewing(conn, shared).await;
                broadcast_sessions(shared).await;
                return;
            }
            let reason = ended_by("closed", device_of(conn).as_ref());
            let (ended, _) = {
                let mut d = shared.daemon.lock().await;
                d.close_session(&id, &reason).await.unwrap_or((true, default_record(&id)))
            };
            if let Some(dev) = &device
                && !ended {
                    let mut d = shared.daemon.lock().await;
                    d.release_claim(&id, &dev.id);
                }
            stop_viewing(conn, shared).await;
            broadcast_sessions(shared).await;
        }
        "attach" => {
            let session_id = v.get("sessionId").and_then(|x| x.as_str()).unwrap_or("").to_string();
            let takeover = v.get("takeover").and_then(|x| x.as_bool()).unwrap_or(false);
            let cols = v.get("cols").and_then(|x| x.as_u64());
            let rows = v.get("rows").and_then(|x| x.as_u64());
            attach(conn, shared, session_id, takeover, cols, rows).await;
        }
        "resize" => {
            let (cols, rows, gen_id) = {
                let inner = conn.inner.lock().unwrap();
                (
                    v.get("cols").and_then(|x| x.as_u64()),
                    v.get("rows").and_then(|x| x.as_u64()),
                    inner.attach_gen,
                )
            };
            let (Some(cols), Some(rows)) = (cols, rows) else {
                conn.send_error("bad_size", "invalid size");
                return;
            };
            let stream = active_stream(conn);
            let Some(stream) = stream else {
                conn.send_error("not_attached", "attach before resizing");
                return;
            };
            if on_stream(&stream, move |s| s.resize(cols, rows))
                .await
                .unwrap_or_else(|| Err("resize task failed".to_string()))
                .is_err()
            {
                conn.send_error("resize_failed", "resize failed");
                return;
            }
            // The pane geometry changed: answer with a fresh snapshot.
            let sid = conn.inner.lock().unwrap().session_id.clone().unwrap_or_default();
            let sub = conn.inner.lock().unwrap().sub_id;
            if let Err(e) = snapshot_and_go_live(conn, &stream, sub, None, gen_id, &sid, false).await {
                conn.send_error("resize_failed", &e);
            }
        }
        "mouseclick" => {
            let col = v.get("col").and_then(|x| x.as_u64());
            let row = v.get("row").and_then(|x| x.as_u64());
            let (Some(col), Some(row)) = (col, row) else {
                conn.send_error("bad_cell", "invalid cell");
                return;
            };
            let stream = active_stream(conn);
            let Some(stream) = stream else {
                conn.send_error("not_attached", "attach before clicking");
                return;
            };
            match on_stream(&stream, move |s| s.snapshot_full()).await.unwrap_or_else(|| Err("snapshot task failed".to_string())) {
                Ok(state) => {
                    if !(state.mouse && state.mouse_sgr) {
                        conn.send_error("no_mouse", "pane has not enabled SGR mouse reporting");
                        return;
                    }
                    if crate::desktop::is_shell_foreground(&state.foreground) {
                        conn.send_error(
                            "shell_foreground",
                            &format!("pane is at a shell prompt ({}), not a mouse-aware program", state.foreground),
                        );
                        return;
                    }
                    if col >= state.cols || row >= state.rows {
                        conn.send_error("bad_cell", "cell outside the pane");
                        return;
                    }
                    let _ = on_stream(&stream, move |s| s.mouse_click(col, row)).await;
                    conn.send_json(&serde_json::json!({
                        "type": "mousedelivered", "col": col, "row": row
                    }));
                }
                Err(e) => conn.send_error("handler_error", &e),
            }
        }
        "input" => {
            let text = v.get("data").and_then(|x| x.as_str()).unwrap_or("").to_string();
            if text.len() > MAX_INPUT_BYTES {
                conn.send_error("input_too_large", &format!("input exceeds {MAX_INPUT_BYTES} bytes"));
                return;
            }
            let stream = active_stream(conn);
            let Some(stream) = stream else {
                conn.send_error("not_attached", "attach before typing");
                return;
            };
            let _ = on_stream(&stream, move |s| s.write(&text)).await;
        }
        "key" => {
            let keys: Vec<String> = v
                .get("keys")
                .and_then(|x| x.as_array())
                .map(|a| {
                    a.iter()
                        .filter_map(|k| k.as_str())
                        .filter(|k| !k.is_empty() && !k.starts_with('-'))
                        .take(MAX_KEYS)
                        .map(|k| k.to_string())
                        .collect()
                })
                .unwrap_or_default();
            let stream = active_stream(conn);
            let Some(stream) = stream else {
                conn.send_error("not_attached", "attach before sending keys");
                return;
            };
            // A name the table refuses is reported, not swallowed: the client
            // logs the code, and nothing reaches the shell.
            let sent = on_stream(&stream, move |s| {
                let refs: Vec<&str> = keys.iter().map(|k| k.as_str()).collect();
                s.send_key(&refs)
            })
            .await;
            if let Some(Err(e)) = sent {
                conn.send_error("bad_key", &e);
            }
        }
        "follow" => {
            // Client-owned scroll (ADR-0004): a no-op.
        }
        "scrollback" => {
            let lines = v.get("lines").and_then(|x| x.as_u64()).unwrap_or(0);
            let wants_history = lines > 0;
            let (gen_id, sid) = {
                let inner = conn.inner.lock().unwrap();
                (inner.attach_gen, inner.session_id.clone().unwrap_or_default())
            };
            let stream = active_stream(conn);
            let Some(stream) = stream else {
                conn.send_error("not_attached", "attach before requesting scrollback");
                return;
            };
            let hist = if wants_history { Some(lines) } else { None };
            let sub = conn.inner.lock().unwrap().sub_id;
            if let Err(e) = snapshot_and_go_live(conn, &stream, sub, hist, gen_id, &sid, wants_history).await {
                conn.send_error("scrollback_failed", &e);
            }
        }
        other => {
            conn.send_error("bad_message", &format!("unknown type: {other}"));
        }
    }
}

/// The `attach` handler: claim, subscribe, snapshot, CAPS + SNAPSHOT, flush.
async fn attach(
    conn: &Arc<ConnShared>,
    shared: &Arc<Shared>,
    session_id: String,
    takeover: bool,
    cols: Option<u64>,
    rows: Option<u64>,
) {
    // Resolve the record + claim under the daemon lock; remember any displaced
    // device to notify after the lock is released.
    let (record, displaced, stream, device) = {
        let mut d = shared.daemon.lock().await;
        let Some(record) = d.get_session(&session_id) else {
            drop(d);
            conn.send_error("unknown_session", &format!("no terminal-backed session: {session_id}"));
            return;
        };
        if record.keeper_name.is_empty() {
            drop(d);
            conn.send_error("unknown_session", &format!("no terminal-backed session: {session_id}"));
            return;
        }
        if record.status != "running" {
            drop(d);
            conn.send_error("session_dead", &format!("session is {}: {session_id}", record.status));
            return;
        }
        let device = conn.inner.lock().unwrap().device.clone();
        let mut displaced = None;
        if let Some(device) = &device {
            match d.claim(&session_id, device, takeover) {
                crate::ownership::ClaimResult::Acquired { previous, .. } => {
                    displaced = previous;
                }
                crate::ownership::ClaimResult::HeldBy { device: holder } => {
                    drop(d);
                    conn.send_json(&serde_json::json!({
                        "type": "owned",
                        "sessionId": session_id,
                        "owner": SessionOwner {
                            id: holder.id, name: holder.name, kind: holder.kind,
                            since: now_ms(),
                        }
                    }));
                    return;
                }
            }
        }
        let keeper_name = record.keeper_name.clone();
        let stream = d.stream_for(&keeper_name).clone();
        (record, displaced, stream, device)
    };

    // A connection that attaches somewhere else has stopped watching where it
    // was, so its claim on that shell goes with it. Otherwise a client that moves
    // between shells over one socket (nothing in the protocol forbids it — the
    // phone happens to use one socket per shell) holds its last one for good:
    // nobody can take it without a takeover, and a desk window it displaced sits
    // on "Press r to take it back" for a shell no device is actually looking at.
    //
    // Per *device*, not per connection: a device with two live sockets on the
    // same shell keeps it, because one of them is still watching.
    {
        let previous = conn.inner.lock().unwrap().session_id.clone();
        if let (Some(device), Some(previous)) = (&device, previous)
            && previous != session_id
        {
            let still_watched = {
                let conns: Vec<Arc<ConnShared>> = {
                    let map = shared.conns.lock().unwrap();
                    map.values().cloned().collect()
                };
                conns.iter().any(|c| {
                    // Not this one: it is the connection that is moving, and its
                    // `session_id` is only updated below.
                    if Arc::ptr_eq(c, conn) {
                        return false;
                    }
                    let inner = c.inner.lock().unwrap();
                    inner.session_id.as_deref() == Some(previous.as_str())
                        && inner
                            .device
                            .as_ref()
                            .map(|d| d.id == device.id)
                            .unwrap_or(false)
                })
            };
            if !still_watched {
                let freed = {
                    let mut d = shared.daemon.lock().await;
                    d.release_claim(&previous, &device.id)
                };
                if freed {
                    broadcast_sessions(shared).await;
                }
            }
        }
    }

    // Bump the generation, clear the old subscription.
    let gen_id = {
        let mut inner = conn.inner.lock().unwrap();
        inner.attach_gen += 1;
        let gen_id = inner.attach_gen;
        if let Some(unsub) = inner.unsubscribe.take() {
            unsub();
        }
        inner.stream = None;
        inner.session_id = Some(session_id.clone());
        inner.record = Some(record.clone());
        inner.live = false;
        inner.resyncing = false;
        inner.pending.clear();
        inner.pending_bytes = 0;
        gen_id
    };

    // Subscribe (queueing output until `live`).
    let tx = conn.tx.clone();
    let inner_for_cb = conn.clone();
    let cb_gen = gen_id;
    let sub = StreamSubscriber {
        on_output: Box::new(move |bytes: &[u8]| {
            let mut inner = inner_for_cb.inner.lock().unwrap();
            if inner.attach_gen != cb_gen {
                return;
            }
            if inner.live {
                queue_output(&inner_for_cb, &mut inner, bytes);
            } else {
                inner.pending.extend_from_slice(bytes);
                inner.pending_bytes += bytes.len();
                if inner.pending_bytes > MAX_PENDING_BYTES {
                    inner.pending.clear();
                    inner.pending_bytes = 0;
                    let _ = tx.send(Out::Json(
                        serde_json::json!({
                            "type":"error","code":"resync",
                            "message":"output backlog exceeded while attaching"
                        })
                        .to_string(),
                    ));
                }
            }
        }),
        on_reset: Box::new({
            let conn2 = conn.clone();
            move |reason: &str| {
                // Not while attaching: the snapshot that is about to be sent
                // repaints from scratch, which is all a resync asks for.
                let ok = {
                    let inner = conn2.inner.lock().unwrap();
                    inner.attach_gen == cb_gen && inner.live
                };
                if ok {
                    conn2.send_error("resync", reason);
                }
            }
        }),
    };
    let (sub_id, unsubscribe) = stream.subscribe_owned(sub);
    {
        let mut inner = conn.inner.lock().unwrap();
        inner.unsubscribe = Some(unsubscribe);
        inner.sub_id = Some(sub_id);
        inner.stream = Some(stream.clone());
    }

    // Resize before the snapshot so the screen we send already matches the grid
    // the client is about to paint.
    if let (Some(c), Some(r)) = (cols, rows) {
        let _ = on_stream(&stream, move |s| s.resize(c, r)).await;
    }
    // The snapshot, cut from live output at one byte boundary. The fence runs
    // with the stream's poll held off: whatever was queued while attaching is
    // already in the snapshot (dropped), the snapshot goes out first, and
    // everything after it is new output.
    let fenced = snapshot_and_go_live(conn, &stream, Some(sub_id), None, gen_id, &session_id, false).await;

    // A second attach (or a disconnect) may have superseded us while we awaited.
    if conn.inner.lock().unwrap().attach_gen != gen_id {
        return;
    }

    if let Err(e) = fenced {
        // Tear the half-made attach down and give the claim back: left alone,
        // this connection holds a shell it is not looking at and the next attach
        // to it (by this very device) is the one refused — the Stale State
        // lockout ADR-0010 exists to prevent.
        stop_viewing(conn, shared).await;
        let freed = match &device {
            Some(dev) => shared.daemon.lock().await.release_claim(&session_id, &dev.id),
            None => false,
        };
        conn.send_error("attach_failed", &e);
        if freed {
            broadcast_sessions(shared).await;
        }
        return;
    }

    // Tell a displaced device (and cut it off).
    if let Some(prev) = displaced {
        notify_displaced(shared, &prev.id, &session_id).await;
    }
}

/// Take a snapshot for this connection and, at the fence, send it and switch the
/// connection to live output. The superseded check happens under the fence too,
/// so a stale attach sends nothing. `history` is `Some(n)` for a scrollback page.
async fn snapshot_and_go_live(
    conn: &Arc<ConnShared>,
    stream: &Arc<PaneStream>,
    requester: Option<usize>,
    history: Option<u64>,
    gen_id: u64,
    session_id: &str,
    history_flag: bool,
) -> Result<(), String> {
    let conn2 = conn.clone();
    let sid = session_id.to_string();
    on_stream(stream, move |s| {
        s.snapshot_fenced(requester, history, |snap, epoch| {
            {
                let mut inner = conn2.inner.lock().unwrap();
                if inner.disposed || inner.attach_gen != gen_id {
                    return;
                }
                inner.pending.clear();
                inner.pending_bytes = 0;
                inner.live = true;
            }
            send_state(&conn2, &sid, snap, epoch, history_flag);
        })
    })
    .await
    .unwrap_or_else(|| Err("snapshot task failed".to_string()))
    .map(|_| ())
}

/// Notify every connection of `device_id` that it no longer holds `session_id`,
/// and cut it off (the TS `notifyDisplaced`).
async fn notify_displaced(shared: &Arc<Shared>, device_id: &str, session_id: &str) {
    let conns: Vec<Arc<ConnShared>> = {
        let map = shared.conns.lock().unwrap();
        map.values().cloned().collect()
    };
    for c in conns {
        let (is_holder, _sid) = {
            let inner = c.inner.lock().unwrap();
            (
                inner.device.as_ref().map(|d| d.id == device_id).unwrap_or(false),
                inner.session_id.clone(),
            )
        };
        if !is_holder {
            continue;
        }
        let viewing = { c.inner.lock().unwrap().session_id.as_deref() == Some(session_id) };
        if !viewing {
            continue;
        }
        c.send_json(&serde_json::json!({"type":"owned","sessionId":session_id,"owner":null}));
        // Cut it off: unsubscribe and forget the stream.
        let keeper_name = {
            let mut inner = c.inner.lock().unwrap();
            if let Some(unsub) = inner.unsubscribe.take() {
                unsub();
            }
            inner.stream = None;
            inner.session_id = None;
            inner.live = false;
            inner.record.as_ref().map(|r| r.keeper_name.clone())
        };
        if let Some(keeper_name) = keeper_name {
            let mut d = shared.daemon.lock().await;
            d.release_stream_if_idle(&keeper_name);
        }
    }
}

/// Send the session list to one connection.
/// Push the session list to every connected client.
pub(crate) async fn broadcast_sessions(shared: &Arc<Shared>) {
    let sessions = {
        let mut d = shared.daemon.lock().await;
        d.describe_sessions().await
    };
    let msg = sessions_message(&sessions).to_string();
    let conns: Vec<Arc<ConnShared>> = {
        let map = shared.conns.lock().unwrap();
        map.values().cloned().collect()
    };
    for c in conns {
        let _ = c.tx.send(Out::Json(msg.clone()));
    }
}

fn sessions_message(sessions: &[ldmux_protocol::rpc::SessionInfo]) -> serde_json::Value {
    serde_json::json!({"type":"sessions","sessions": sessions})
}

/// The `caps` + `snapshot` pair (the TS `sendState`).
fn send_state(
    conn: &Arc<ConnShared>,
    session_id: &str,
    snap: &ldmux_protocol::keeper::SnapshotResult,
    epoch: u64,
    history: bool,
) {
    conn.send_json(&serde_json::json!({
        "type": "caps",
        "protocolVersion": PROTOCOL_VERSION,
        "sessionId": session_id,
        "cols": snap.cols,
        "rows": snap.rows,
        "snapshotEpoch": epoch,
        "capabilities": {"scrollback": true},
    }));
    let mut snapshot = serde_json::json!({
        "type": "snapshot",
        "sessionId": session_id,
        "cols": snap.cols,
        "rows": snap.rows,
        "screen": snap.text,
        "cursorX": snap.cursor_x,
        "cursorY": snap.cursor_y,
        "alternateOn": snap.alternate_on,
        "bracketedPaste": snap.bracketed_paste,
        "mouse": snap.mouse,
        "mouseSgr": snap.mouse_sgr,
        "appCursor": snap.app_cursor,
        "historySize": snap.history_size,
        "snapshotEpoch": epoch,
    });
    if history {
        snapshot["history"] = serde_json::Value::Bool(true);
    }
    conn.send_json(&snapshot);
}

/// Run a `PaneStream` call on the blocking pool. The stream's keeper connection
/// is a **blocking** socket with no read timeout, so calling it directly from an
/// async task would let a slow or wedged keeper stall the executor (and, under
/// the daemon lock, every other RPC).
async fn on_stream<T, F>(stream: &Arc<PaneStream>, f: F) -> Option<T>
where
    T: Send + 'static,
    F: FnOnce(Arc<PaneStream>) -> T + Send + 'static,
{
    let s = Arc::clone(stream);
    tokio::task::spawn_blocking(move || f(s)).await.ok()
}

/// The connection's active stream (cloned).
fn active_stream(conn: &Arc<ConnShared>) -> Option<Arc<PaneStream>> {
    conn.inner.lock().unwrap().stream.clone()
}

/// Stop viewing the currently-attached session (a `close`/`kill` path).
async fn stop_viewing(conn: &Arc<ConnShared>, shared: &Arc<Shared>) {
    let keeper_name = {
        let mut inner = conn.inner.lock().unwrap();
        if let Some(unsub) = inner.unsubscribe.take() {
            unsub();
        }
        inner.stream = None;
        inner.session_id = None;
        inner.live = false;
        inner.record.take().map(|r| r.keeper_name)
    };
    if let Some(keeper_name) = keeper_name {
        let mut d = shared.daemon.lock().await;
        d.release_stream_if_idle(&keeper_name);
    }
}

/// The connection ended: unsubscribe, release the stream, release claims.
async fn disconnect(conn: Arc<ConnShared>, shared: Arc<Shared>) {
    let (keeper_name, device, session) = {
        let mut inner = conn.inner.lock().unwrap();
        inner.disposed = true;
        if let Some(unsub) = inner.unsubscribe.take() {
            unsub();
        }
        (
            inner.record.take().map(|r| r.keeper_name),
            inner.device.clone(),
            inner.session_id.clone(),
        )
    };
    shared.conns.lock().unwrap().remove_by_ptr(&conn);
    if let Some(keeper_name) = keeper_name {
        let mut d = shared.daemon.lock().await;
        d.release_stream_if_idle(&keeper_name);
    }
    if let Some(device) = device {
        release_after_grace(&shared, device.id, session).await;
    }
}

/// Release a closed socket's claims after the grace (a reconnect within it
/// still holds; one that does not lets go).
async fn release_after_grace(shared: &Arc<Shared>, device_id: String, session_id: Option<String>) {
    let shared = shared.clone();
    tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(RELEASE_GRACE_MS)).await;
        let (device_has_conns, still_viewing) = {
            let map = shared.conns.lock().unwrap();
            let has = map.values().any(|c| {
                c.inner
                    .lock()
                    .unwrap()
                    .device
                    .as_ref()
                    .map(|d| d.id == device_id)
                    .unwrap_or(false)
            });
            let viewing = map.values().any(|c| {
                let inner = c.inner.lock().unwrap();
                inner
                    .device
                    .as_ref()
                    .map(|d| d.id == device_id)
                    .unwrap_or(false)
                    && session_id.is_some()
                    && inner.session_id == session_id
            });
            (has, viewing)
        };
        let mut freed = false;
        if !device_has_conns {
            let mut d = shared.daemon.lock().await;
            freed = !d.release_device(&device_id).is_empty();
        } else if let Some(sid) = &session_id
            && !still_viewing {
                let mut d = shared.daemon.lock().await;
                freed = d.release_claim(sid, &device_id);
            }
        if freed {
            broadcast_sessions(&shared).await;
        }
    });
}

/// Free claims with no connection behind them (see
/// [`crate::daemon::LdmuxDaemon::release_unbacked_claims`]), and tell every
/// client, because who holds a shell is on the wire.
///
/// It runs on the sweep's own tick rather than the maintenance pass: it is
/// about connections, and the sweep already walks them.
async fn reconcile_claims(shared: &Arc<Shared>) {
    let attached: std::collections::HashSet<(String, String)> = {
        let map = shared.conns.lock().unwrap();
        map.values()
            .filter_map(|c| {
                let inner = c.inner.lock().unwrap();
                let device = inner.device.as_ref()?.id.clone();
                let session = inner.session_id.clone()?;
                Some((session, device))
            })
            .collect()
    };
    let freed = {
        let mut d = shared.daemon.lock().await;
        d.release_unbacked_claims(&attached, now_ms())
    };
    if !freed.is_empty() {
        broadcast_sessions(shared).await;
    }
}

/// Reap connections silent for the idle deadline (the TS `sweep`).
async fn sweep_once(shared: &Arc<Shared>) {
    let now = now_ms();
    let conns: Vec<Arc<ConnShared>> = {
        let map = shared.conns.lock().unwrap();
        map.values().cloned().collect()
    };
    for c in conns {
        let (idle, disposed) = {
            let inner = c.inner.lock().unwrap();
            (now.saturating_sub(inner.last_seen_ms), inner.disposed)
        };
        if disposed {
            continue;
        }
        if idle > shared.idle_ms {
            let _ = c.tx.send(Out::Close);
        }
    }
}

/// The `live` result fields (ok + detail) out of a `handle` JSON value.
fn live_result(result: Result<serde_json::Value, String>) -> (bool, Option<String>) {
    match result {
        Ok(v) => (
            v.get("ok").and_then(|x| x.as_bool()).unwrap_or(false),
            v.get("detail").and_then(|x| x.as_str()).map(|s| s.to_string()),
        ),
        Err(e) => (false, Some(e)),
    }
}

fn default_record(id: &str) -> SessionRecord {
    SessionRecord {
        id: id.into(),
        keeper_name: String::new(),
        kind: "shell".into(),
        cwd: String::new(),
        title: String::new(),
        created_at: 0,
        updated_at: 0,
        created_by: String::new(),
        status: "dead".into(),
        exit_code: None,
        exited_at: None,
        live_workspace: None,
        live_pid: None,
        ended_reason: None,
    }
}

/// How an ending reads when the device behind it is known: `killed from motorola
/// edge 2024`. A connection that announced no device (a bare socket, the CLI over
/// RPC) leaves the bare verb — a reason that guessed at a device would be worse
/// than one that does not name it.
fn ended_by(verb: &str, device: Option<&ldmux_protocol::rpc::DeviceInfo>) -> String {
    match device {
        Some(dev) if !dev.name.is_empty() => format!("{verb} from {}", dev.name),
        _ => verb.to_string(),
    }
}

/// The device behind a connection, for [`ended_by`], without holding the lock.
fn device_of(conn: &ConnShared) -> Option<ldmux_protocol::rpc::DeviceInfo> {
    conn.inner.lock().unwrap().device.clone()
}

// ── HTTP head parsing / responses ───────────────────────────────────────────

/// A parsed request head: `(method, path, headers)`.
type ParsedHead = (String, String, Vec<(String, String)>);

/// The text of a WS message, if it is text or binary (a control message).
fn message_text(msg: &Message) -> Option<String> {
    match msg {
        Message::Text(t) => Some(t.to_string()),
        Message::Binary(b) => Some(String::from_utf8_lossy(b.as_ref()).to_string()),
        _ => None,
    }
}

/// Read the HTTP request head from the socket without consuming it (a
/// non-destructive peek, so the WebSocket handshake still sees the request).
async fn read_head(stream: &TcpStream) -> Option<String> {
    let mut buf = vec![0u8; 16 * 1024];
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        match stream.peek(&mut buf).await {
            Ok(0) => return None,
            Ok(n) => {
                if buf[..n].windows(4).any(|w| w == b"\r\n\r\n") || n >= buf.len() {
                    return Some(String::from_utf8_lossy(&buf[..n]).into_owned());
                }
            }
            Err(_) => return None,
        }
        if tokio::time::Instant::now() > deadline {
            return None;
        }
        tokio::time::sleep(std::time::Duration::from_millis(2)).await;
    }
}

/// Parse the request line + headers from a head. Header names are lowercased.
fn parse_request_head(head: &str) -> Option<ParsedHead> {
    let mut lines = head.split("\r\n");
    let request = lines.next()?;
    let mut parts = request.split_whitespace();
    let method = parts.next()?.to_string();
    let target = parts.next()?.to_string();
    let mut headers = Vec::new();
    for line in lines {
        if line.is_empty() {
            break;
        }
        if let Some((k, v)) = line.split_once(':') {
            headers.push((k.trim().to_ascii_lowercase(), v.trim().to_string()));
        }
    }
    Some((method, target, headers))
}

/// The path of a request target (query removed).
fn path_of(target: &str) -> String {
    target.split('?').next().unwrap_or("/").to_string()
}

/// Whether the request is a WebSocket upgrade.
fn is_ws_upgrade(headers: &[(String, String)]) -> bool {
    let connection = headers
        .iter()
        .filter(|(k, _)| k == "connection")
        .map(|(_, v)| v.to_ascii_lowercase())
        .collect::<Vec<_>>()
        .join(",");
    let upgrade = headers
        .iter()
        .find(|(k, _)| k == "upgrade")
        .map(|(_, v)| v.eq_ignore_ascii_case("websocket"))
        .unwrap_or(false);
    connection
        .split(',')
        .any(|p| p.trim() == "upgrade")
        && upgrade
}

/// Whether the request carries the bearer token (header or `?token=`) —
/// the TS `authorized`.
fn authorized(headers: &[(String, String)], target: &str, token: &str) -> bool {
    let provided = headers
        .iter()
        .find(|(k, _)| k == "authorization")
        .map(|(_, v)| {
            if let Some(rest) = v.strip_prefix("Bearer ") {
                rest.trim().to_string()
            } else {
                String::new()
            }
        })
        .filter(|s| !s.is_empty())
        .or_else(|| query_param(target, "token"))
        .unwrap_or_default();
    crate::auth::token_matches(token, &provided)
}

/// The value of a query parameter in a request target.
fn query_param(target: &str, name: &str) -> Option<String> {
    let query = target.split_once('?')?.1;
    for pair in query.split('&') {
        let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
        if k == name {
            return Some(percent_decode(v));
        }
    }
    None
}

/// A minimal percent-decoder for the query token.
fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hi = (bytes[i + 1] as char).to_digit(16);
            let lo = (bytes[i + 2] as char).to_digit(16);
            if let (Some(hi), Some(lo)) = (hi, lo) {
                out.push((hi * 16 + lo) as u8);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// The plain-HTTP status + body for a non-upgrade request (the TS `handleRequest`).
fn plain_response(path: &str, authed: bool) -> (u16, &'static str) {
    if path == "/ws" {
        if !authed {
            (401, "unauthorized\n")
        } else {
            (400, "upgrade required\n")
        }
    } else {
        (404, "ldmux remote: the endpoint is /ws\n")
    }
}

/// Write a minimal HTTP response and close the connection.
async fn write_http(stream: &mut TcpStream, status: u16, body: &str) {
    let reason = match status {
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        _ => "OK",
    };
    let text = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: text/plain; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let _ = stream.write_all(text.as_bytes()).await;
    let _ = stream.flush().await;
    let _ = stream.shutdown().await;
}

/// Remove a connection from the table by pointer identity (the reader task
/// still holds the `Arc`, so `Arc::ptr_eq` is the key).
trait RemoveByPtr {
    fn remove_by_ptr(&mut self, needle: &Arc<ConnShared>);
}
impl RemoveByPtr for HashMap<u64, Arc<ConnShared>> {
    fn remove_by_ptr(&mut self, needle: &Arc<ConnShared>) {
        let key = self
            .iter()
            .find(|(_, v)| Arc::ptr_eq(v, needle))
            .map(|(k, _)| *k);
        if let Some(k) = key {
            self.remove(&k);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_request_head() {
        let head = "GET /ws?token=abc HTTP/1.1\r\nHost: x\r\nAuthorization: Bearer t\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n\r\n";
        let (m, t, h) = parse_request_head(head).unwrap();
        assert_eq!(m, "GET");
        assert_eq!(path_of(&t), "/ws");
        assert!(is_ws_upgrade(&h));
        assert!(authorized(&h, &t, "t"));
    }

    #[test]
    fn authorized_accepts_the_query_token_and_rejects_others() {
        let (_, target, headers) = parse_request_head(
            "GET /ws?token=abc HTTP/1.1\r\nHost: x\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n\r\n",
        )
        .unwrap();
        assert!(authorized(&headers, &target, "abc"));
        assert!(!authorized(&headers, &target, "zzz"));
    }

    #[test]
    fn plain_get_answers_401_without_a_token() {
        assert_eq!(plain_response("/ws", false), (401, "unauthorized\n"));
        assert_eq!(plain_response("/ws", true).0, 400);
        assert_eq!(plain_response("/", true).0, 404);
    }

    #[test]
    fn a_non_upgrade_request_is_not_an_upgrade() {
        let (_, _, headers) =
            parse_request_head("GET /ws HTTP/1.1\r\nHost: x\r\n\r\n").unwrap();
        assert!(!is_ws_upgrade(&headers));
    }

    /// A connection whose writer never drains (a phone on a dead link).
    fn undrained_conn() -> (Arc<ConnShared>, tokio::sync::mpsc::UnboundedReceiver<Out>) {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<Out>();
        let conn = Arc::new(ConnShared {
            tx,
            inner: Mutex::new(ConnInner {
                disposed: false,
                live: true,
                attach_gen: 1,
                session_id: None,
                record: None,
                stream: None,
                unsubscribe: None,
                sub_id: None,
                device: None,
                pending: Vec::new(),
                pending_bytes: 0,
                resyncing: false,
                last_seen_ms: 0,
            }),
            outstanding: AtomicUsize::new(0),
        });
        (conn, rx)
    }

    /// A write from a connection that is not attached says so.
    ///
    /// All three writes go to the pane, and only `mouseclick` answered: `key` and
    /// `input` did nothing at all, on the wire, in either direction. A tap test on
    /// the phone looked like "the app sends nothing" because of it — the app's key
    /// batches were dropped by a daemon that said nothing back.
    #[tokio::test]
    async fn a_write_without_an_attach_is_refused_loudly() {
        let (conn, mut rx) = undrained_conn();
        let dir = std::env::temp_dir().join(format!("ldmux-remote-write-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let daemon = Arc::new(tokio::sync::Mutex::new(crate::daemon::LdmuxDaemon::new(
            dir.clone(),
            std::path::PathBuf::from("/nonexistent"),
        )));
        let shared = Arc::new(Shared {
            conns: Mutex::new(HashMap::new()),
            daemon,
            token: "t".into(),
            idle_ms: 20_000,
            stopped: AtomicBool::new(false),
        });
        for msg in [
            r#"{"type":"key","keys":["Enter"]}"#,
            r#"{"type":"input","data":"ls\n"}"#,
            r#"{"type":"mouseclick","col":1,"row":1}"#,
        ] {
            handle_message(msg, &conn, &shared).await;
            let out = rx.try_recv().unwrap_or_else(|_| panic!("no answer to {msg}"));
            let Out::Json(text) = out else { panic!("expected json for {msg}") };
            let v: serde_json::Value = serde_json::from_str(&text).unwrap();
            assert_eq!(v["type"], "error", "{msg} -> {v}");
            assert_eq!(v["code"], "not_attached", "{msg} -> {v}");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The backlog rule: a client that is not reading costs the daemon a bounded
    /// amount, is told to resync exactly once, and is not buffered for after that.
    /// (`outstanding` was never incremented, so this guard did nothing and the
    /// writer's queue grew with the shell's whole output.)
    #[test]
    fn a_client_that_does_not_read_is_resynced_once_and_not_buffered() {
        let (conn, mut rx) = undrained_conn();
        let chunk = vec![b'x'; 4096];
        for _ in 0..10_000 {
            let mut inner = conn.inner.lock().unwrap();
            queue_output(&conn, &mut inner, &chunk);
        }
        let mut output = 0usize;
        let mut resyncs = 0usize;
        while let Ok(m) = rx.try_recv() {
            match m {
                Out::Output(b) => output += b.len(),
                Out::Json(j) => {
                    assert!(j.contains("\"resync\""));
                    resyncs += 1;
                }
                Out::Close => panic!("no close"),
            }
        }
        assert_eq!(resyncs, 1, "told once per backlog");
        assert!(
            output <= WS_BACKLOG_BYTES + 4096,
            "queued {output} bytes against a {WS_BACKLOG_BYTES} byte bound"
        );
        assert_eq!(conn.outstanding.load(Ordering::Relaxed), output);

        // A fresh attach clears the flag; once the writer has caught up output flows again.
        conn.outstanding.store(0, Ordering::Relaxed);
        conn.inner.lock().unwrap().resyncing = false;
        let mut inner = conn.inner.lock().unwrap();
        queue_output(&conn, &mut inner, b"hi");
        drop(inner);
        assert!(matches!(rx.try_recv(), Ok(Out::Output(b)) if b == b"hi"));
    }

    #[test]
    fn the_caps_and_snapshot_pair_has_the_client_fields() {
        // Build a fake connection with a channel, then assert the two shapes.
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<Out>();
        let conn = Arc::new(ConnShared {
            tx,
            inner: Mutex::new(ConnInner {
                disposed: false,
                live: false,
                attach_gen: 0,
                session_id: None,
                record: None,
                stream: None,
                unsubscribe: None,
                sub_id: None,
                device: None,
                pending: Vec::new(),
                pending_bytes: 0,
                resyncing: false,
                last_seen_ms: 0,
            }),
            outstanding: AtomicUsize::new(0),
        });
        let snap = ldmux_protocol::keeper::SnapshotResult {
            session_id: "s".into(),
            cols: 80,
            rows: 24,
            history_lines: 0,
            text: "screen".into(),
            cursor_x: 1,
            cursor_y: 2,
            alternate_on: false,
            mouse: true,
            mouse_sgr: true,
            app_cursor: false,
            bracketed_paste: true,
            history_size: 0,
            dead: false,
            foreground: "bash".into(),
            cwd: "/".into(),
                tail: Vec::new(),
            };
        send_state(&conn, "s1", &snap, 7, false);
        let a = rx.try_recv().unwrap();
        let b = rx.try_recv().unwrap();
        let (Out::Json(a), Out::Json(b)) = (a, b) else {
            panic!("expected json");
        };
        let a: serde_json::Value = serde_json::from_str(&a).unwrap();
        let b: serde_json::Value = serde_json::from_str(&b).unwrap();
        assert_eq!(a["type"], "caps");
        assert_eq!(a["protocolVersion"], 1);
        assert_eq!(a["snapshotEpoch"], 7);
        assert_eq!(b["type"], "snapshot");
        assert_eq!(b["screen"], "screen");
        assert_eq!(b["mouseSgr"], true);
        assert!(b.get("history").is_none());
    }

    /// A reason names the device when the connection announced one, and does not
    /// invent one when it did not. The device is how "why did my shell die"
    /// answers "because the phone closed it" rather than "closed".
    #[test]
    fn a_reason_comes_from_the_device_that_ended_it() {
        let phone = ldmux_protocol::rpc::DeviceInfo {
            id: "d1".into(),
            name: "motorola edge 2024".into(),
            kind: "phone".into(),
        };
        assert_eq!(ended_by("killed", Some(&phone)), "killed from motorola edge 2024");
        assert_eq!(ended_by("closed", Some(&phone)), "closed from motorola edge 2024");
        // A connection with no device (a bare socket, the CLI over RPC), and one
        // that announced itself without a name, both leave the bare verb.
        assert_eq!(ended_by("killed", None), "killed");
        let unnamed = ldmux_protocol::rpc::DeviceInfo {
            id: "d2".into(),
            name: String::new(),
            kind: "phone".into(),
        };
        assert_eq!(ended_by("killed", Some(&unnamed)), "killed");
    }
}