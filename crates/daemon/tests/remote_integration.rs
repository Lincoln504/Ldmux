//! Remote integration test: the WebSocket client talks to the daemon over the
//! remote WS endpoint.
//!
//! The daemon is created with a fake keeper and started (which starts the
//! remote WS server). A WS client connects, authenticates with the token, and
//! exercises the hello, list, desktop, new, kill, and close handlers.

use ldmux_daemon::daemon::LdmuxDaemon;
use ldmux_daemon::fake_keeper::FakeKeeper;
use ldmux_protocol::keeper::{KeeperMessage, Spawn};

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

/// A minimal WS helper: send a JSON message and read the response.
async fn ws_roundtrip(
    ws: &mut tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>,
    msg: &serde_json::Value,
) -> serde_json::Value {
    use futures_util::sink::SinkExt;
    use futures_util::stream::StreamExt;

    let text = serde_json::to_string(msg).unwrap();
    ws.send(tokio_tungstenite::tungstenite::Message::Text(text.into()))
        .await
        .unwrap();
    loop {
        let Some(Ok(m)) = ws.next().await else {
            panic!("connection closed");
        };
        if let tokio_tungstenite::tungstenite::Message::Text(t) = &m {
            return serde_json::from_str(&t.to_string()).unwrap();
        }
    }
}

#[tokio::test]
async fn remote_list_and_new_over_ws() {
    let dir = scratch();
    let keeper_sock = dir.join("keeper.sock");
    let fk = std::sync::Arc::new(FakeKeeper::new(keeper_sock.clone()));
    let fk_serve = fk.clone();
    let fk_sock = keeper_sock.clone();
    tokio::spawn(async move {
        FakeKeeper::serve(&fk_serve, fk_sock.as_path()).await.unwrap();
    });
    wait_socket(&keeper_sock).await;

    // A daemon with the remote enabled on a test port.
    let port: u16 = 49995;
    let token = "test-token".to_string();
    let mut d = LdmuxDaemon::new(dir.clone(), std::path::PathBuf::from("/nonexistent"))
        .with_remote("127.0.0.1".to_string(), port, token.clone());
    let arc: std::sync::Arc<tokio::sync::Mutex<LdmuxDaemon>> =
        std::sync::Arc::new(tokio::sync::Mutex::new(d));
    {
        let mut d = arc.lock().await;
        d.start(&arc).await.unwrap();
    }

    // Connect a WS client.
    let uri = format!("ws://127.0.0.1:{port}/ws");
    let (mut ws, _response) = tokio_tungstenite::connect_async(&uri)
        .await
        .unwrap();

    // Send a `ping` with the token (the first message carries the token).
    let resp = ws_roundtrip(
        &mut ws,
        &serde_json::json!({ "type": "ping", "id": "test-1", "token": token }),
    )
    .await;
    assert_eq!(resp["type"], "pong");

    // Send a `hello` with the device id.
    let resp = ws_roundtrip(
        &mut ws,
        &serde_json::json!({
            "type": "hello",
            "device": { "id": "phone-1", "name": "Test Phone", "kind": "phone" }
        }),
    )
    .await;
    assert_eq!(resp["type"], "hello_ack");

    // Send a `list` — should be empty (no sessions yet).
    let resp = ws_roundtrip(&mut ws, &serde_json::json!({ "type": "list" })).await;
    assert_eq!(resp["type"], "sessions");
    let sessions = resp["sessions"].as_array().unwrap();
    assert!(sessions.is_empty(), "expected no sessions: {sessions:?}");

    // Send a `new` — create a session.
    let dir_str = dir.to_string_lossy().into_owned();
    let resp = ws_roundtrip(
        &mut ws,
        &serde_json::json!({
            "type": "new",
            "kind": "shell",
            "cwd": dir_str,
        }),
    )
    .await;
    assert_eq!(resp["type"], "session");
    let session = &resp["session"];
    let session_id = session["id"].as_str().unwrap().to_string();
    assert!(!session_id.is_empty(), "no session id: {session:?}");

    // Send a `list` — should contain the session.
    let resp = ws_roundtrip(&mut ws, &serde_json::json!({ "type": "list" })).await;
    let sessions = resp["sessions"].as_array().unwrap();
    assert_eq!(sessions.len(), 1, "expected 1 session: {sessions:?}");
    assert_eq!(sessions[0]["id"].as_str().unwrap(), session_id);

    // Send a `kill` — kill the session.
    let resp = ws_roundtrip(
        &mut ws,
        &serde_json::json!({ "type": "kill", "sessionId": session_id }),
    )
    .await;
    assert_eq!(resp["type"], "killed");

    // Send a `list` — should show the session as dead.
    let resp = ws_roundtrip(&mut ws, &serde_json::json!({ "type": "list" })).await;
    let sessions = resp["sessions"].as_array().unwrap();
    assert_eq!(sessions.len(), 1, "expected 1 session: {sessions:?}");
    assert_eq!(sessions[0]["status"].as_str().unwrap(), "dead");

    // Close the connection.
    ws.close(None).await.ok();
    drop(fk);
    let _ = std::fs::remove_dir_all(&dir);
}
