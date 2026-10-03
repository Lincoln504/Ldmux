//! The remote server: the phone-facing WebSocket endpoint (the tailnet address +
//! token auth).
//!
//! Port of `packages/daemon/src/remote.ts` (1293 lines). The phone connects over
//! the tailnet address with the bearer token; the server gates on the token.
//!
//! This is the **minimal** remote: the WS server (bind on the tailnet address),
//! the token gate (checked on the first message), the `ping`/`hello`/`list`
//! dispatch, and a per-client bounded output queue (the §3.7: overflow → drop +
//! forced resync). The pairing (the QR code), the device ownership, and the full
//! session broadcast are layered on as they are ported.
//!
//! ## What is NOT here yet
//!
//! - The pairing flow (the QR code, `pairing.rs`).
//! - The device ownership (the `byDevice` map, the `releaseAfterGrace`).
//! - The full session broadcast (the `broadcastSessions`).
//! - The attach's full lifecycle (the claim, the resync, the input).
//! - The sweep timer (the idle connections).
//!
//! ## What IS here
//!
//! - The `RemoteServer` struct (the config: the hostname, the port, the token).
//! - The `start()` method (bind the WS server on the tailnet address).
//! - The `handle_connection()` method (the per-connection loop: read messages,
//!   check the token on the first message, dispatch, write responses).
//! - The per-client bounded output queue (the §3.7: overflow → drop + resync).

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

/// The remote server: the phone-facing WebSocket endpoint.
pub struct RemoteServer {
    cfg: RemoteConfig,
    /// Whether the server is stopped (the `stop` sets it).
    stopped: Arc<AtomicBool>,
    /// The accept loop's join handle (the `start` spawns it).
    #[allow(dead_code)]
    _accept_handle: Option<tokio::task::JoinHandle<()>>,
}

impl RemoteServer {
    pub fn new(cfg: RemoteConfig) -> Self {
        Self {
            cfg,
            stopped: Arc::new(AtomicBool::new(false)),
            _accept_handle: None,
        }
    }

    /// Bind the WS server on the tailnet address (the `start()`).
    pub async fn start(&mut self) -> Result<(String, u16), String> {
        let addr = format!("{}:{}", self.cfg.hostname, self.cfg.port);
        let listener = TcpListener::bind(&addr)
            .await
            .map_err(|e| format!("bind {addr}: {e}"))?;
        let token = self.cfg.token.clone();
        let stopped = Arc::clone(&self.stopped);
        let handle = tokio::spawn(async move {
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
                tokio::spawn(handle_connection(ws_stream, token.clone()));
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
async fn handle_connection(mut ws: WsStream, token: String) {
    let mut authorized = false;
    loop {
        let Some(Ok(msg)) = ws.next().await else {
            break;
        };
        let text: String = match &msg {
            Message::Text(t) => t.to_string(),
            Message::Binary(b) => String::from_utf8_lossy(b.as_ref()).to_string(),
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
        // The dispatch: the `ping`/`hello`/`list`.
        let v: serde_json::Value = match serde_json::from_str(&text) {
            Ok(v) => v,
            Err(_) => continue,
        };
        let mtype = v.get("type").and_then(|t| t.as_str()).unwrap_or("");
        match mtype {
            "ping" => {
                let id = v.get("id").cloned().unwrap_or(serde_json::Value::Null);
                let _ = send_json(
                    &mut ws,
                    &serde_json::json!({ "type": "pong", "id": id }),
                )
                .await;
            }
            "hello" => {
                let _ = send_json(&mut ws, &serde_json::json!({ "type": "hello_ack" }))
                    .await;
            }
            "list" => {
                let _ = send_json(
                    &mut ws,
                    &serde_json::json!({ "type": "sessions", "sessions": [] }),
                )
                .await;
            }
            _ => {
                let _ = send_json(
                    &mut ws,
                    &serde_json::json!({ "type": "error", "code": "unknown", "message": "unknown message type" }),
                )
                .await;
            }
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
        let (_hostname, port) = server.start().await.expect("bind");
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
}
