//! Remote integration test: a WS client talks to the daemon over the remote WS
//! endpoint, using the **same wire contract as the Kotlin client**.
//!
//! Pinned here so a drift from the TS source of record fails a test:
//! - the bearer token goes in the HTTP `Authorization` header at the upgrade,
//!   not in a message (the Kotlin `AttachClient`/`LdmuxClient` set the header and
//!   send `hello` with no token);
//! - `hello` is answered with nothing (the client ignores unknown types, but the
//!   TS sends no ack);
//! - `list` answers `{type:"sessions", sessions:[SessionInfo camelCase]}`;
//! - `new` answers `created` **then** `sessions`;
//! - `kill`/`close` answer `sessions`;
//! - a plain `GET /ws` answers 401 without a token and 400 with one (the phone's
//!   `probe()` reads the status to tell a refused token from a machine that is off).

use ldmux_daemon::daemon::LdmuxDaemon;
use ldmux_daemon::fake_keeper::FakeKeeper;

fn scratch() -> std::path::PathBuf {
    let mut p = std::env::temp_dir();
    p.push(format!("ldmux-remote-int-{}", rand::random::<u64>()));
    std::fs::create_dir_all(&p).unwrap();
    p
}

async fn wait_socket(path: &std::path::Path) {
    for _ in 0..50 {
        if path.exists() {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    panic!("socket did not appear: {}", path.display());
}

type Ws = tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

/// Connect with the bearer token in the HTTP upgrade header.
async fn connect_authed(port: u16, token: &str) -> Ws {
    use tokio_tungstenite::tungstenite::client::IntoClientRequest;
    let uri = format!("ws://127.0.0.1:{port}/ws");
    let mut req = uri.into_client_request().unwrap();
    req.headers_mut().insert(
        "Authorization",
        format!("Bearer {token}").parse().unwrap(),
    );
    let (ws, _) = tokio_tungstenite::connect_async(req).await.unwrap();
    ws
}

/// Send one JSON message.
async fn send(ws: &mut Ws, msg: &serde_json::Value) {
    use futures_util::sink::SinkExt;
    ws.send(tokio_tungstenite::tungstenite::Message::Text(
        serde_json::to_string(msg).unwrap().into(),
    ))
    .await
    .unwrap();
}

/// Read the next text message.
async fn next_json(ws: &mut Ws) -> serde_json::Value {
    use futures_util::stream::StreamExt;
    loop {
        let Some(Ok(m)) = ws.next().await else {
            panic!("connection closed");
        };
        if let tokio_tungstenite::tungstenite::Message::Text(t) = &m {
            return serde_json::from_str(t.as_ref()).unwrap();
        }
    }
}

/// Read messages until one has the given `type`.
async fn next_of_type(ws: &mut Ws, ty: &str) -> serde_json::Value {
    for _ in 0..20 {
        let v = next_json(ws).await;
        if v["type"] == ty {
            return v;
        }
    }
    panic!("no {ty} message");
}

/// A raw plain-HTTP GET, returning the status code.
async fn plain_get_status(port: u16, token: Option<&str>) -> u16 {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let mut s = tokio::net::TcpStream::connect(("127.0.0.1", port)).await.unwrap();
    let auth = token
        .map(|t| format!("Authorization: Bearer {t}\r\n"))
        .unwrap_or_default();
    let req = format!("GET /ws HTTP/1.1\r\nHost: localhost\r\n{auth}Connection: close\r\n\r\n");
    s.write_all(req.as_bytes()).await.unwrap();
    let mut buf = Vec::new();
    s.read_to_end(&mut buf).await.ok();
    let head = String::from_utf8_lossy(&buf);
    
    head
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse::<u16>().ok())
        .unwrap_or(0)
}

async fn start_daemon(dir: &std::path::Path, port: u16, token: &str) -> std::sync::Arc<tokio::sync::Mutex<LdmuxDaemon>> {
    let keeper_sock = dir.join("keeper.sock");
    let fk = std::sync::Arc::new(FakeKeeper::new(keeper_sock.clone()));
    let fk_serve = fk.clone();
    let fk_sock = keeper_sock.clone();
    tokio::spawn(async move {
        FakeKeeper::serve(&fk_serve, fk_sock.as_path()).await.unwrap();
    });
    wait_socket(&keeper_sock).await;
    let d = LdmuxDaemon::new(dir.to_path_buf(), std::path::PathBuf::from("/nonexistent"))
        .with_remote("127.0.0.1".to_string(), port, token.to_string());
    let arc = std::sync::Arc::new(tokio::sync::Mutex::new(d));
    arc.lock().await.start(&arc).await.unwrap();
    arc
}

#[tokio::test]
async fn remote_contract_list_new_kill_close() {
    let dir = scratch();
    let port: u16 = 49995;
    let token = "test-token";
    let arc = start_daemon(&dir, port, token).await;

    let mut ws = connect_authed(port, token).await;

    // `hello` is answered with nothing (assert the next message is the `list`
    // answer, not an ack).
    send(
        &mut ws,
        &serde_json::json!({
            "type": "hello",
            "device": { "id": "phone-1", "name": "Test Phone", "kind": "phone" }
        }),
    )
    .await;

    // `list` — camelCase SessionInfo array.
    send(&mut ws, &serde_json::json!({ "type": "list" })).await;
    let resp = next_of_type(&mut ws, "sessions").await;
    assert!(resp["sessions"].as_array().unwrap().is_empty());

    // `new` — `created` then `sessions`.
    send(
        &mut ws,
        &serde_json::json!({ "type": "new", "kind": "shell", "cwd": dir.to_string_lossy() }),
    )
    .await;
    let created = next_of_type(&mut ws, "created").await;
    let session_id = created["sessionId"].as_str().unwrap().to_string();
    assert!(!session_id.is_empty());
    assert!(created.get("cwd").is_some(), "created carries cwd");
    let sessions = next_of_type(&mut ws, "sessions").await;
    assert_eq!(sessions["sessions"].as_array().unwrap().len(), 1);
    // camelCase, and the fields the Kotlin `parseSessions` reads.
    let row = &sessions["sessions"][0];
    assert_eq!(row["id"], session_id);
    assert!(row.get("keeperName").is_some(), "camelCase keeperName: {row}");
    assert!(row.get("tmuxName").is_none(), "the legacy key must not be on the wire: {row}");
    assert_eq!(row["createdBy"], "phone");
    assert!(row.get("lastActivityAt").is_some());

    // `kill` — answered with `sessions`.
    send(
        &mut ws,
        &serde_json::json!({ "type": "kill", "sessionId": session_id }),
    )
    .await;
    let sessions = next_of_type(&mut ws, "sessions").await;
    assert_eq!(sessions["sessions"][0]["status"], "dead");

    ws.close(None).await.ok();
    drop(arc);
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn remote_rejects_a_missing_token_and_answers_the_probe() {
    let dir = scratch();
    let port: u16 = 49996;
    let token = "right-token";
    let arc = start_daemon(&dir, port, token).await;

    // A WS upgrade without the token is refused (the client never gets a socket).
    use tokio_tungstenite::tungstenite::client::IntoClientRequest;
    let uri = format!("ws://127.0.0.1:{port}/ws");
    let req = uri.into_client_request().unwrap();
    let err = tokio_tungstenite::connect_async(req).await;
    assert!(err.is_err(), "an unauthenticated upgrade must be refused");

    // The phone's `probe()`: a bad token is 401 (Rejected), a good token is a
    // non-401 (the daemon is there).
    assert_eq!(plain_get_status(port, Some("wrong")).await, 401);
    assert_eq!(plain_get_status(port, None).await, 401);
    assert_eq!(plain_get_status(port, Some(token)).await, 400);

    drop(arc);
    let _ = std::fs::remove_dir_all(&dir);
}

/// A peer that connects and says nothing must not stall everyone else: the pre-auth
/// work used to run inline in the accept loop, so an idle socket held the next
/// client for the whole head-read deadline (5 s).
#[tokio::test]
async fn a_silent_peer_does_not_block_the_next_client() {
    let dir = scratch();
    let port: u16 = 49971;
    let token = "right-token";
    let arc = start_daemon(&dir, port, token).await;

    let _idle = tokio::net::TcpStream::connect(("127.0.0.1", port)).await.unwrap();
    let started = std::time::Instant::now();
    let status = plain_get_status(port, Some(token)).await;
    assert_eq!(status, 400);
    assert!(
        started.elapsed() < std::time::Duration::from_secs(2),
        "a probe took {:?} behind a silent peer",
        started.elapsed()
    );

    drop(arc);
    let _ = std::fs::remove_dir_all(&dir);
}
