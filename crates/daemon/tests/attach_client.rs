//! The desktop attach client against a real daemon: `attach::run_with`
//! connects to the daemon's remote, attaches, streams the pane, and forwards
//! input. This is the desk-window path of plan §3.9 (a protocol client of the daemon).

mod common;

use std::path::PathBuf;
use std::sync::Arc;

use ldmux_daemon::daemon::LdmuxDaemon;

fn scratch() -> PathBuf {
    let mut p = std::env::temp_dir();
    p.push(format!("ldmux-attach-int-{}-{}", std::process::id(), rand::random::<u64>()));
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

#[tokio::test]
async fn attach_client_streams_and_types_against_a_daemon() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let dir = scratch();
    let keeper_sock = dir.join("keeper.sock");
    let _reaper = common::Reaper::new(&dir, &keeper_sock);
    let mut keeper = std::process::Command::new(keeper_bin())
        .arg(keeper_sock.to_string_lossy().as_ref())
        .spawn()
        .unwrap();
    wait_socket(&keeper_sock).await;

    let port: u16 = 49998;
    let token = "attach-token";
    let d = LdmuxDaemon::new(dir.clone(), keeper_bin())
        .with_remote("127.0.0.1".to_string(), port, token.to_string());
    let daemon_sock = d.socket_path().to_string_lossy().into_owned();
    let arc = Arc::new(tokio::sync::Mutex::new(d));
    {
        let mut d = arc.lock().await;
        d.start(&arc).await.unwrap();
    }
    // Create a shell the attach client will view.
    let cwd = std::env::current_dir().unwrap().to_string_lossy().into_owned();
    let id = arc
        .lock()
        .await
        .create_session("shell", &cwd, None, "local")
        .await
        .unwrap()
        .id;

    // The client's "terminal": a duplex the test drives.
    let (client_in, mut test_write) = tokio::io::duplex(8192);
    let (client_out, mut test_read) = tokio::io::duplex(65536);
    let task = tokio::spawn(async move {
        attach::run_with(&daemon_sock, &id, false, client_in, client_out).await
    });

    // Type a command into the window.
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    test_write
        .write_all(b"echo ATTACH_MARK_7\n")
        .await
        .unwrap();
    test_write.flush().await.unwrap();

    // The pane's output must reach the client's terminal.
    let mut seen = Vec::new();
    let mut buf = [0u8; 4096];
    let mut found = false;
    for _ in 0..200 {
        match tokio::time::timeout(std::time::Duration::from_millis(50), test_read.read(&mut buf))
            .await
        {
            Ok(Ok(0)) => break,
            Ok(Ok(n)) => {
                seen.extend_from_slice(&buf[..n]);
                if String::from_utf8_lossy(&seen).contains("ATTACH_MARK_7") {
                    found = true;
                    break;
                }
            }
            _ => {}
        }
    }
    assert!(
        found,
        "the attach client must stream the pane output; got {:?}",
        String::from_utf8_lossy(&seen)
    );

    task.abort();
    arc.lock().await.stop();
    let _ = std::process::Command::new("kill")
        .arg("-TERM")
        .arg(keeper.id().to_string())
        .output();
    let _ = keeper.wait();
    let _ = std::fs::remove_dir_all(&dir);
}
/// The desk window must survive the daemon's idle sweep.
///
/// The daemon closes a WebSocket that has said nothing for `idle_ms`
/// (`WS_IDLE_MS`, 20 s). The mobile client pings every 4 s; the desktop attach
/// client sent nothing at all, so the daemon reaped it and the window exited
/// about 20 s after it opened — reproducibly, on the live box.
///
/// The deadline is injected short and the ping short with it, so the contract is
/// proved in milliseconds: five deadlines pass with the client on the other end
/// of them.
#[tokio::test]
async fn attach_client_survives_the_idle_sweep() {
    let dir = scratch();
    let keeper_sock = dir.join("keeper.sock");
    let _reaper = common::Reaper::new(&dir, &keeper_sock);
    let mut keeper = std::process::Command::new(keeper_bin())
        .arg(keeper_sock.to_string_lossy().as_ref())
        .spawn()
        .unwrap();
    wait_socket(&keeper_sock).await;

    let port: u16 = 49997;
    let token = "idle-token";
    let idle_ms = 200u64;
    let d = LdmuxDaemon::new(dir.clone(), keeper_bin())
        .with_remote("127.0.0.1".to_string(), port, token.to_string())
        .with_remote_idle_ms(idle_ms);
    let daemon_sock = d.socket_path().to_string_lossy().into_owned();
    let arc = Arc::new(tokio::sync::Mutex::new(d));
    {
        let mut d = arc.lock().await;
        d.start(&arc).await.unwrap();
    }
    let cwd = std::env::current_dir().unwrap().to_string_lossy().into_owned();
    let id = arc
        .lock()
        .await
        .create_session("shell", &cwd, None, "local")
        .await
        .unwrap()
        .id;

    let (client_in, _test_write) = tokio::io::duplex(8192);
    let (client_out, _test_read) = tokio::io::duplex(65536);
    // Ping four times faster than the deadline: the same relationship the
    // production constants have (4 s against 20 s).
    let task = tokio::spawn(async move {
        attach::run_with_ping(&daemon_sock, &id, false, client_in, client_out, idle_ms / 4).await
    });

    // Five deadlines. Without a ping the daemon closes the socket inside the
    // first one; with it the client is still there at the end.
    tokio::time::sleep(std::time::Duration::from_millis(idle_ms * 5)).await;
    assert!(
        !task.is_finished(),
        "the attach client must outlive the idle deadline it is pinging against"
    );

    task.abort();
    arc.lock().await.stop();
    let _ = std::process::Command::new("kill")
        .arg("-TERM")
        .arg(keeper.id().to_string())
        .output();
    let _ = keeper.wait();
    let _ = std::fs::remove_dir_all(&dir);
}

// ── helpers for the handover test ──

type Ws = tokio_tungstenite::WebSocketStream<
    tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
>;

async fn connect_authed(port: u16, token: &str) -> Ws {
    use tokio_tungstenite::tungstenite::client::IntoClientRequest;
    let uri = format!("ws://127.0.0.1:{port}/ws");
    let mut req = uri.into_client_request().unwrap();
    req.headers_mut()
        .insert("Authorization", format!("Bearer {token}").parse().unwrap());
    let (ws, _) = tokio_tungstenite::connect_async(req).await.unwrap();
    ws
}

async fn ws_send(ws: &mut Ws, msg: &serde_json::Value) {
    use futures_util::sink::SinkExt;
    ws.send(tokio_tungstenite::tungstenite::Message::Text(
        serde_json::to_string(msg).unwrap().into(),
    ))
    .await
    .unwrap();
}

/// Read until `needle` shows up, or give up after `ms`.
async fn wait_for<R: tokio::io::AsyncReadExt + Unpin>(reader: &mut R, needle: &str) -> bool {
    wait_for_ms(reader, needle, 15_000).await
}

async fn wait_for_ms<R: tokio::io::AsyncReadExt + Unpin>(
    reader: &mut R,
    needle: &str,
    ms: u64,
) -> bool {
    let mut seen = Vec::new();
    let mut buf = [0u8; 8192];
    for _ in 0..(ms / 50).max(1) {
        match tokio::time::timeout(std::time::Duration::from_millis(50), reader.read(&mut buf)).await
        {
            Ok(Ok(0)) => break,
            Ok(Ok(n)) => {
                seen.extend_from_slice(&buf[..n]);
                if String::from_utf8_lossy(&seen).contains(needle) {
                    return true;
                }
            }
            _ => {}
        }
    }
    false
}

/// The desk window takes the shell back **by itself** when the device holding it
/// lets go.
///
/// A device that puts a shell down is not using it, so there is nothing to
/// consent to and the window shows the shell again. The TypeScript shim did
/// exactly this — its attach loop re-attached when the daemon cleared the
/// session's "held elsewhere" note — and the port's client instead waited for a
/// key, so a desk window stayed on the notice after the phone in its pocket had
/// long since let go. Only the *free* case comes back on its own: a shell another
/// device holds still waits for `r`, which is the consent.
#[tokio::test]
async fn the_desk_window_takes_the_shell_back_when_the_holder_lets_go() {
    use tokio::io::AsyncWriteExt;

    let dir = scratch();
    let keeper_sock = dir.join("keeper.sock");
    let _reaper = common::Reaper::new(&dir, &keeper_sock);
    let mut keeper = std::process::Command::new(keeper_bin())
        .arg(keeper_sock.to_string_lossy().as_ref())
        .spawn()
        .unwrap();
    wait_socket(&keeper_sock).await;

    let port: u16 = 49993;
    let token = "holder-let-go-token";
    let d = LdmuxDaemon::new(dir.clone(), keeper_bin())
        .with_remote("127.0.0.1".to_string(), port, token.to_string());
    let daemon_sock = d.socket_path().to_string_lossy().into_owned();
    let arc = Arc::new(tokio::sync::Mutex::new(d));
    {
        let mut d = arc.lock().await;
        d.start(&arc).await.unwrap();
    }
    let cwd = std::env::current_dir().unwrap().to_string_lossy().into_owned();
    let id = arc
        .lock()
        .await
        .create_session("shell", &cwd, None, "local")
        .await
        .unwrap()
        .id;

    // The desk window.
    let (desk_in, mut desk_typing) = tokio::io::duplex(8192);
    let (desk_out, mut desk_view) = tokio::io::duplex(65536);
    let desk = {
        let daemon_sock = daemon_sock.clone();
        let id = id.clone();
        tokio::spawn(async move {
            attach::run_with(&daemon_sock, &id, false, desk_in, desk_out).await
        })
    };
    tokio::time::sleep(std::time::Duration::from_millis(700)).await;

    // A phone takes the shell.
    let mut phone = connect_authed(port, token).await;
    ws_send(
        &mut phone,
        &serde_json::json!({
            "type": "hello",
            "device": { "id": "phone-1", "name": "phone", "kind": "phone" }
        }),
    )
    .await;
    ws_send(
        &mut phone,
        &serde_json::json!({
            "type": "attach", "sessionId": id, "cols": 80, "rows": 24, "takeover": true
        }),
    )
    .await;

    assert!(
        wait_for(&mut desk_view, "open on another device").await,
        "the desk window must be told the shell is held elsewhere"
    );

    // The phone puts the shell down. The daemon releases the claim after its
    // grace (3 s) and broadcasts the list; the window re-attaches on its own.
    //
    // Polled rather than slept through: the whole workspace gate runs on one
    // machine, and a fixed wait sized for an idle box is a test that fails when
    // the box is busy. While the window is detached its input is swallowed, so
    // typing into it repeatedly is harmless and the first echo that lands is the
    // proof it is live again.
    drop(phone);
    let mut reattached = false;
    for attempt in 0..30 {
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        if attempt < 6 {
            continue; // before the release grace has expired
        }
        desk_typing.write_all(b"echo LANDED_9\n").await.unwrap();
        desk_typing.flush().await.unwrap();
        if wait_for_ms(&mut desk_view, "LANDED_9", 700).await {
            reattached = true;
            break;
        }
    }
    assert!(
        reattached,
        "the desk window must have re-attached without a key"
    );

    desk.abort();
    arc.lock().await.stop();
    let _ = std::process::Command::new("kill")
        .arg("-TERM")
        .arg(keeper.id().to_string())
        .output();
    let _ = keeper.wait();
    let _ = std::fs::remove_dir_all(&dir);
}
