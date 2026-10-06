//! End-to-end: a real `ldmux-keeper`, the daemon's remote WS, and a client that
//! does what the phone does — attach, receive the `caps`+`snapshot` pair, type,
//! and read back the binary OUTPUT frames.
//!
//! This is the deployment gate: it exercises the whole chain (daemon → keeper →
//! PTY → ghostty-vt → stream poll → OUTPUT frame → WS), not a fake.

mod common;

use std::path::PathBuf;
use std::sync::Arc;

use ldmux_daemon::daemon::LdmuxDaemon;

fn scratch() -> PathBuf {
    let mut p = std::env::temp_dir();
    p.push(format!("ldmux-e2e-{}-{}", std::process::id(), rand::random::<u64>()));
    std::fs::create_dir_all(&p).unwrap();
    p
}

fn keeper_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/debug/ldmux-keeper")
}

async fn wait_socket(path: &std::path::Path) {
    for _ in 0..400 {
        if path.exists() {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
    panic!("socket did not appear: {}", path.display());
}

type Ws = tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

async fn connect_authed(port: u16, token: &str) -> Ws {
    use tokio_tungstenite::tungstenite::client::IntoClientRequest;
    let uri = format!("ws://127.0.0.1:{port}/ws");
    let mut req = uri.into_client_request().unwrap();
    req.headers_mut()
        .insert("Authorization", format!("Bearer {token}").parse().unwrap());
    let (ws, _) = tokio_tungstenite::connect_async(req).await.unwrap();
    ws
}

async fn send(ws: &mut Ws, msg: &serde_json::Value) {
    use futures_util::sink::SinkExt;
    ws.send(tokio_tungstenite::tungstenite::Message::Text(
        serde_json::to_string(msg).unwrap().into(),
    ))
    .await
    .unwrap();
}

/// Read messages until a text one has `type == ty`; returns it plus any binary
/// output seen along the way.
async fn next_of_type(ws: &mut Ws, ty: &str, out: &mut Vec<u8>) -> serde_json::Value {
    use futures_util::stream::StreamExt;
    for _ in 0..200 {
        let Some(Ok(m)) = ws.next().await else {
            panic!("connection closed waiting for {ty}");
        };
        match m {
            tokio_tungstenite::tungstenite::Message::Text(t) => {
                let v: serde_json::Value = serde_json::from_str(t.as_ref()).unwrap();
                if v["type"] == ty {
                    return v;
                }
            }
            tokio_tungstenite::tungstenite::Message::Binary(b) => decode_output(&b, out),
            _ => {}
        }
    }
    panic!("no {ty} message");
}

/// Decode a binary OUTPUT frame (8-byte header, type 0x10) into `out`.
fn decode_output(b: &[u8], out: &mut Vec<u8>) {
    if b.len() < 8 {
        return;
    }
    if b[0] != 0x10 {
        return;
    }
    let len = u32::from_be_bytes([b[4], b[5], b[6], b[7]]) as usize;
    let end = (8 + len).min(b.len());
    out.extend_from_slice(&b[8..end]);
}

/// Drain binary output until `needle` appears (or a timeout).
async fn read_until(ws: &mut Ws, out: &mut Vec<u8>, needle: &str) -> bool {
    use futures_util::stream::StreamExt;
    for _ in 0..400 {
        if String::from_utf8_lossy(out).contains(needle) {
            return true;
        }
        let next = tokio::time::timeout(std::time::Duration::from_millis(25), ws.next()).await;
        match next {
            Ok(Some(Ok(tokio_tungstenite::tungstenite::Message::Binary(b)))) => {
                decode_output(&b, out)
            }
            Ok(Some(Ok(tokio_tungstenite::tungstenite::Message::Text(t)))) => {
                let _ = t;
            }
            Ok(Some(Ok(_))) | Ok(None) => {}
            Ok(Some(Err(_))) => return false,
            Err(_) => {}
        }
    }
    String::from_utf8_lossy(out).contains(needle)
}

/// The port the daemon actually bound (it is 0 in the config).
async fn remote_port(arc: &Arc<tokio::sync::Mutex<LdmuxDaemon>>) -> u16 {
    let v = arc
        .lock()
        .await
        .handle("remote_info", serde_json::json!({}))
        .await
        .unwrap();
    v["port"].as_u64().unwrap() as u16
}

/// An inbound frame larger than the cap closes the connection.
///
/// The TypeScript server was `new WebSocketServer({ maxPayload: 64 * 1024 })`
/// and the port inherited tungstenite's 64 MiB default instead, so one message
/// could make the daemon buffer 64 MiB. Nothing a client legitimately sends comes
/// near 64 KiB (`input` is capped at 16 KiB, `key` at 32 names), and the limit is
/// on reads only, so a large server→client snapshot is unaffected.
#[tokio::test]
async fn a_client_message_over_the_cap_closes_the_connection() {
    use futures_util::stream::StreamExt;

    let dir = scratch();
    let keeper_sock = dir.join("keeper.sock");
    let _reaper = common::Reaper::new(&dir, &keeper_sock);
    let mut keeper = std::process::Command::new(keeper_bin())
        .arg(keeper_sock.to_string_lossy().as_ref())
        .spawn()
        .unwrap();
    wait_socket(&keeper_sock).await;

    let token = "cap-token";
    let d = LdmuxDaemon::new(dir.clone(), keeper_bin())
        .with_remote("127.0.0.1".to_string(), 0, token.to_string());
    let arc = Arc::new(tokio::sync::Mutex::new(d));
    arc.lock().await.start(&arc).await.unwrap();
    let port = remote_port(&arc).await;

    let mut ws = connect_authed(port, token).await;
    // 1 MiB in one text frame: over the 64 KiB cap, far under tungstenite's
    // 64 MiB default, so the difference is the daemon's config and nothing else.
    let big = "x".repeat(1024 * 1024);
    let payload = serde_json::json!({ "type": "input", "data": big }).to_string();
    send(
        &mut ws,
        &serde_json::json!({ "type": "input", "data": big }),
    )
    .await;
    let _ = payload;
    // The server must close (or error) rather than accept it. Either way the
    // stream ends: nothing else is coming.
    let mut closed = false;
    for _ in 0..100 {
        match tokio::time::timeout(std::time::Duration::from_millis(50), ws.next()).await {
            Ok(Some(Ok(tokio_tungstenite::tungstenite::Message::Close(_)))) | Ok(None) => {
                closed = true;
                break;
            }
            Ok(Some(Err(_))) => {
                closed = true;
                break;
            }
            _ => {}
        }
    }
    assert!(closed, "a 1 MiB message must not be accepted");

    arc.lock().await.stop();
    let _ = std::process::Command::new("kill")
        .arg("-TERM")
        .arg(keeper.id().to_string())
        .output();
    let _ = keeper.wait();
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn remote_end_to_end_attach_and_type() {
    let dir = scratch();
    let keeper_sock = dir.join("keeper.sock");
    let _reaper = common::Reaper::new(&dir, &keeper_sock);
    let mut keeper = std::process::Command::new(keeper_bin())
        .arg(keeper_sock.to_string_lossy().as_ref())
        .spawn()
        .unwrap();
    wait_socket(&keeper_sock).await;

    let token = "e2e-token";
    // Port 0: the kernel picks, and the daemon reports what it bound. A fixed
    // port made this file and `attach_client.rs` fight over 49997 — the same
    // gate failing because two tests, or two runs of the gate, wanted one port.
    let d = LdmuxDaemon::new(dir.clone(), keeper_bin())
        .with_remote("127.0.0.1".to_string(), 0, token.to_string());
    let arc = Arc::new(tokio::sync::Mutex::new(d));
    arc.lock().await.start(&arc).await.unwrap();
    let port = remote_port(&arc).await;

    let mut ws = connect_authed(port, token).await;
    let mut out: Vec<u8> = Vec::new();

    // Identify (as the phone does) then create a shell.
    send(
        &mut ws,
        &serde_json::json!({
            "type": "hello",
            "device": { "id": "e2e-phone", "name": "E2E", "kind": "phone" }
        }),
    )
    .await;
    send(
        &mut ws,
        &serde_json::json!({ "type": "new", "kind": "shell", "cwd": dir.to_string_lossy() }),
    )
    .await;
    let created = next_of_type(&mut ws, "created", &mut out).await;
    let session_id = created["sessionId"].as_str().unwrap().to_string();

    // Attach: the answer is `caps` + `snapshot`.
    send(
        &mut ws,
        &serde_json::json!({ "type": "attach", "sessionId": session_id, "cols": 80, "rows": 24 }),
    )
    .await;
    let caps = next_of_type(&mut ws, "caps", &mut out).await;
    assert_eq!(caps["protocolVersion"], 1);
    let snap = next_of_type(&mut ws, "snapshot", &mut out).await;
    assert_eq!(snap["cols"], 80);
    assert_eq!(snap["rows"], 24);
    assert!(snap.get("screen").is_some());

    // Type a command; the shell echoes and runs it.
    send(
        &mut ws,
        &serde_json::json!({ "type": "input", "data": "echo E2E_MARKER_42\n" }),
    )
    .await;
    let saw = read_until(&mut ws, &mut out, "E2E_MARKER_42").await;
    assert!(
        saw,
        "the typed command's output must arrive as OUTPUT frames; got {:?}",
        String::from_utf8_lossy(&out)
    );

    // Kill and clean up.
    send(
        &mut ws,
        &serde_json::json!({ "type": "kill", "sessionId": session_id }),
    )
    .await;
    ws.close(None).await.ok();
    arc.lock().await.stop();
    let _ = std::process::Command::new("kill")
        .arg("-TERM")
        .arg(keeper.id().to_string())
        .output();
    let _ = keeper.wait();
    let _ = std::fs::remove_dir_all(&dir);
}