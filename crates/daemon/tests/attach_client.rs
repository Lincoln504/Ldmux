//! The desktop attach client against a real daemon: `attach::run_with`
//! connects to the daemon's remote, attaches, streams the pane, and forwards
//! input. This is the desk-window path of plan §3.9 (a protocol client of the daemon).

mod common;

use std::path::PathBuf;
use std::sync::Arc;

use ldmux_daemon::daemon::LdmuxDaemon;

fn scratch() -> PathBuf {
    let mut p = std::env::temp_dir();
    p.push(format!(
        "ldmux-attach-int-{}-{}",
        std::process::id(),
        rand::random::<u64>()
    ));
    std::fs::create_dir_all(&p).unwrap();
    p
}

fn keeper_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/debug/ldmux-keeper")
}

/// The attach binary, which lives in another crate so `CARGO_BIN_EXE_` cannot name it.
///
/// The window needs a **real pty**: the client measures its terminal's size from fd 0, and a
/// test process's stdin is the harness's, not a terminal. Cargo does not necessarily build
/// another member's binary for `-p ldmux-daemon`, so a missing one skips the test out loud
/// rather than failing on the build schedule.
fn child_keeper(keeper: &mut std::process::Child) -> std::io::Result<()> {
    let _ = std::process::Command::new("kill")
        .arg("-TERM")
        .arg(keeper.id().to_string())
        .output();
    let _ = keeper.wait();
    Ok(())
}

fn attach_bin() -> Option<PathBuf> {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/debug/ldmux-attach");
    p.exists().then_some(p)
}

/// The port the daemon's remote actually bound.
///
/// The tests ask for port 0 — any free port the kernel has — so two test
/// binaries running at once cannot collide on a fixed number. Two of them did
/// (this file and `remote_e2e.rs` both chose 49997), which is a gate that fails
/// on the schedule of the machine rather than on the code.
async fn remote_port(arc: &Arc<tokio::sync::Mutex<LdmuxDaemon>>) -> u16 {
    let v = arc
        .lock()
        .await
        .handle("remote_info", serde_json::json!({}))
        .await
        .unwrap();
    v["port"].as_u64().unwrap() as u16
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

    let token = "attach-token";
    let d = LdmuxDaemon::new(dir.clone(), keeper_bin()).with_remote(
        "127.0.0.1".to_string(),
        0,
        token.to_string(),
    );
    let daemon_sock = d.socket_path().to_string_lossy().into_owned();
    let arc = Arc::new(tokio::sync::Mutex::new(d));
    {
        let mut d = arc.lock().await;
        d.start(&arc).await.unwrap();
    }
    // Create a shell the attach client will view.
    let cwd = std::env::current_dir()
        .unwrap()
        .to_string_lossy()
        .into_owned();
    let id = arc
        .lock()
        .await
        .create_session(&cwd, None, "local")
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
    test_write.write_all(b"echo ATTACH_MARK_7\n").await.unwrap();
    test_write.flush().await.unwrap();

    // The pane's output must reach the client's terminal.
    let mut seen = Vec::new();
    let mut buf = [0u8; 4096];
    let mut found = false;
    for _ in 0..200 {
        match tokio::time::timeout(
            std::time::Duration::from_millis(50),
            test_read.read(&mut buf),
        )
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
/// **The desk window opens on the shell's own history, not on a blank screen.**
///
/// Without a `scrollback` request the window is a screen mirror: `render_snapshot` clears and
/// paints the current grid, so the terminal's scrollback holds repeated snapshots rather than the
/// shell's history. The daemon has always been able to serve it and the mobile tailnet client asks;
/// the desk client never did. This prints a marker, pushes it off the visible screen, and requires
/// the window to show it anyway.
#[tokio::test]
async fn the_desk_window_opens_on_the_shells_history() {
    use tokio::io::AsyncReadExt;

    let dir = scratch();
    let keeper_sock = dir.join("keeper.sock");
    let _reaper = common::Reaper::new(&dir, &keeper_sock);
    let mut keeper = std::process::Command::new(keeper_bin())
        .arg(keeper_sock.to_string_lossy().as_ref())
        .spawn()
        .unwrap();
    wait_socket(&keeper_sock).await;

    let token = "attach-token";
    let d = LdmuxDaemon::new(dir.clone(), keeper_bin()).with_remote(
        "127.0.0.1".to_string(),
        0,
        token.to_string(),
    );
    let daemon_sock = d.socket_path().to_string_lossy().into_owned();
    let arc = Arc::new(tokio::sync::Mutex::new(d));
    {
        let mut d = arc.lock().await;
        d.start(&arc).await.unwrap();
    }
    let cwd = std::env::current_dir()
        .unwrap()
        .to_string_lossy()
        .into_owned();
    let id = arc
        .lock()
        .await
        .create_session(&cwd, None, "local")
        .await
        .unwrap()
        .id;

    // A marker, then enough output to push it off a 24-row screen. Typed through a plain
    // WebSocket, because the client under test is the one that must *read* the history.
    let port = remote_port(&arc).await;
    let mut ws = connect_authed(port, token).await;
    // A writer says hello first; the daemon refuses input from a viewer that has not.
    ws_send(
        &mut ws,
        &serde_json::json!({
            "type": "hello",
            "device": { "id": "test-desk", "name": "test", "kind": "desktop" }
        }),
    )
    .await;
    ws_send(
        &mut ws,
        &serde_json::json!({ "type": "attach", "sessionId": id, "cols": 80, "rows": 24 }),
    )
    .await;
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    ws_send(
        &mut ws,
        &serde_json::json!({ "type": "input", "data": "echo HISTORY_MARK_9\n" }),
    )
    .await;
    for i in 0..120 {
        ws_send(
            &mut ws,
            &serde_json::json!({ "type": "input", "data": format!("echo filler-{i}\n") }),
        )
        .await;
    }
    // Let the pane print and the keeper's history fill.
    tokio::time::sleep(std::time::Duration::from_millis(2_000)).await;
    drop(ws);

    let (client_in, _test_write) = tokio::io::duplex(8192);
    let (client_out, mut test_read) = tokio::io::duplex(1 << 20);
    let task = tokio::spawn(async move {
        attach::run_with(&daemon_sock, &id, false, client_in, client_out).await
    });

    let mut seen = Vec::new();
    let mut buf = [0u8; 8192];
    let mut found = false;
    for _ in 0..400 {
        match tokio::time::timeout(
            std::time::Duration::from_millis(50),
            test_read.read(&mut buf),
        )
        .await
        {
            Ok(Ok(0)) => break,
            Ok(Ok(n)) => {
                seen.extend_from_slice(&buf[..n]);
                if String::from_utf8_lossy(&seen).contains("HISTORY_MARK_9") {
                    found = true;
                    break;
                }
            }
            _ => {}
        }
    }
    assert!(
        found,
        "the desk window never showed the shell's history; got {} bytes",
        seen.len()
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
/// ≈20 s after it opened — reproducibly, on the live box.
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

    let token = "idle-token";
    let idle_ms = 200u64;
    let d = LdmuxDaemon::new(dir.clone(), keeper_bin())
        .with_remote("127.0.0.1".to_string(), 0, token.to_string())
        .with_remote_idle_ms(idle_ms);
    let daemon_sock = d.socket_path().to_string_lossy().into_owned();
    let arc = Arc::new(tokio::sync::Mutex::new(d));
    {
        let mut d = arc.lock().await;
        d.start(&arc).await.unwrap();
    }
    let cwd = std::env::current_dir()
        .unwrap()
        .to_string_lossy()
        .into_owned();
    let id = arc
        .lock()
        .await
        .create_session(&cwd, None, "local")
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

type Ws =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

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
        match tokio::time::timeout(std::time::Duration::from_millis(50), reader.read(&mut buf))
            .await
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

    let token = "holder-let-go-token";
    // **The graces are shortened, because they are what this test waits for.**
    // A phone's claim outlives its connection for the reconnect grace (15 s in
    // production, sized against the phone's own 2.5 s probe plus 400 ms retry and
    // its backoff), and the desk window only takes a *free* shell. So the whole
    // test used to be one 15 s wait with a 15 s `wait_for` around it. The rule
    // under test is "the claim is given back after the grace and the window comes
    // back by itself", which is the same rule at 300 ms.
    let d = LdmuxDaemon::new(dir.clone(), keeper_bin())
        .with_remote("127.0.0.1".to_string(), 0, token.to_string())
        .with_reconnect_grace_ms(300)
        .with_claim_tick_ms(50);
    let daemon_sock = d.socket_path().to_string_lossy().into_owned();
    let arc = Arc::new(tokio::sync::Mutex::new(d));
    {
        let mut d = arc.lock().await;
        d.start(&arc).await.unwrap();
    }
    let port = remote_port(&arc).await;
    let cwd = std::env::current_dir()
        .unwrap()
        .to_string_lossy()
        .into_owned();
    let id = arc
        .lock()
        .await
        .create_session(&cwd, None, "local")
        .await
        .unwrap()
        .id;

    // The desk window.
    let (desk_in, mut desk_typing) = tokio::io::duplex(8192);
    let (desk_out, mut desk_view) = tokio::io::duplex(65536);
    let desk = {
        let daemon_sock = daemon_sock.clone();
        let id = id.clone();
        tokio::spawn(
            async move { attach::run_with(&daemon_sock, &id, false, desk_in, desk_out).await },
        )
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
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
    while tokio::time::Instant::now() < deadline {
        desk_typing.write_all(b"echo LANDED_9\n").await.unwrap();
        desk_typing.flush().await.unwrap();
        if wait_for_ms(&mut desk_view, "LANDED_9", 200).await {
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

/// A desk window that was displaced ends when its shell does.
///
/// The notice ("open on another device, press r") is only ever replaced by an
/// answer from the daemon, and a displaced window has stopped asking: it was cut
/// off, so it saw neither the pane's exit nor the session's death. Live: the
/// phone ended the shell and the desk window sat on that notice for the shell's
/// whole afterlife — a window telling its user to press a key for a shell that
/// no longer exists.
///
/// So the window asks. If the sessions it is told about no longer contain a
/// running copy of its shell, it re-attaches and takes the daemon's verdict:
/// `session_dead`, which prints "shell ended" and ends the window.
#[tokio::test]
async fn a_displaced_desk_window_ends_when_its_shell_does() {
    let dir = scratch();
    let keeper_sock = dir.join("keeper.sock");
    let _reaper = common::Reaper::new(&dir, &keeper_sock);
    let mut keeper = std::process::Command::new(keeper_bin())
        .arg(keeper_sock.to_string_lossy().as_ref())
        .spawn()
        .unwrap();
    wait_socket(&keeper_sock).await;

    let token = "displaced-death-token";
    let d = LdmuxDaemon::new(dir.clone(), keeper_bin()).with_remote(
        "127.0.0.1".to_string(),
        0,
        token.to_string(),
    );
    let daemon_sock = d.socket_path().to_string_lossy().into_owned();
    let arc = Arc::new(tokio::sync::Mutex::new(d));
    {
        let mut d = arc.lock().await;
        d.start(&arc).await.unwrap();
    }
    let port = remote_port(&arc).await;
    let cwd = std::env::current_dir()
        .unwrap()
        .to_string_lossy()
        .into_owned();
    let id = arc
        .lock()
        .await
        .create_session(&cwd, None, "local")
        .await
        .unwrap()
        .id;

    let (desk_in, _desk_typing) = tokio::io::duplex(8192);
    let (desk_out, mut desk_view) = tokio::io::duplex(65536);
    let desk = {
        let daemon_sock = daemon_sock.clone();
        let id = id.clone();
        tokio::spawn(
            async move { attach::run_with(&daemon_sock, &id, false, desk_in, desk_out).await },
        )
    };
    tokio::time::sleep(std::time::Duration::from_millis(700)).await;

    let mut phone = connect_authed(port, token).await;
    ws_send(
        &mut phone,
        &serde_json::json!({
            "type": "hello",
            "device": { "id": "phone-death", "name": "phone", "kind": "phone" }
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

    // The phone ends the shell. Nothing about that reaches a displaced window
    // directly: it hears it in the session list, or not at all.
    ws_send(
        &mut phone,
        &serde_json::json!({"type": "kill", "sessionId": id}),
    )
    .await;

    assert!(
        wait_for(&mut desk_view, "shell ended").await,
        "a window whose shell is gone must say so rather than keep the notice"
    );
    for _ in 0..100 {
        if desk.is_finished() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    assert!(
        desk.is_finished(),
        "the attach client must end with its shell"
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

/// A claim with no connection behind it is given back, and the desk window takes
/// the shell again by itself.
///
/// Found in use, 2026-10-05: a desk window sat on "Press r to take it back"
/// while the phone had nothing open at all. The claim was the *desk's own* —
/// the daemon takes one on a window's behalf (`makeLive`/`deskWindow`) before the
/// window has attached — and the window it was taken for had been displaced and
/// never come back, so no connection was behind the claim and nothing would ever
/// release it: the window only takes a shell that is *free*, `list` said
/// `desktop` held it, and the only way out was pressing `r`.
///
/// The claim here is made through the control socket with the desktop device,
/// which is exactly what `makeLive` and `deskWindow` do (all three call
/// `LdmuxDaemon::claim`), so the same state is reached without a window manager.
#[tokio::test]
async fn a_claim_with_nothing_behind_it_does_not_strand_the_window() {
    let dir = scratch();
    let keeper_sock = dir.join("keeper.sock");
    let _reaper = common::Reaper::new(&dir, &keeper_sock);
    let mut keeper = std::process::Command::new(keeper_bin())
        .arg(keeper_sock.to_string_lossy().as_ref())
        .spawn()
        .unwrap();
    wait_socket(&keeper_sock).await;

    let token = "unbacked-claim-token";
    // The idle deadline is left at its default on purpose. Shortening it to make
    // the sweep run sooner also reaps the *desk window's* connection, and the
    // client reconnects and re-attaches — which recovers the shell by itself and
    // hides the thing under test. It pings every 4 s, so a 20 s deadline leaves it
    // alone.
    //
    // **The claim grace is shortened.** This test used to take 16 s, and almost
    // all of it was one grace: the desk window's unbacked claim was treated as a
    // dropped link (15 s) because the same device had attached to the shell
    // earlier, so `ever_backed` still held its key. `LdmuxDaemon::claim` now drops
    // that key when a claim changes hands, so this claim gets the short grace — and
    // the short grace here is 200 ms, because the rule under test is "an unbacked
    // claim is given back and the window returns by itself", not "…after three
    // seconds".
    let d = LdmuxDaemon::new(dir.clone(), keeper_bin())
        .with_remote("127.0.0.1".to_string(), 0, token.to_string())
        .with_claim_grace_ms(200)
        .with_claim_tick_ms(50);
    let daemon_sock = d.socket_path().to_string_lossy().into_owned();
    let arc = Arc::new(tokio::sync::Mutex::new(d));
    {
        let mut d = arc.lock().await;
        d.start(&arc).await.unwrap();
    }
    let port = remote_port(&arc).await;
    let cwd = std::env::current_dir()
        .unwrap()
        .to_string_lossy()
        .into_owned();
    let id = arc
        .lock()
        .await
        .create_session(&cwd, None, "local")
        .await
        .unwrap()
        .id;

    // The desk window: the real attach client, on a duplex the test drives.
    let (desk_in, _desk_typing) = tokio::io::duplex(8192);
    let (desk_out, mut desk_view) = tokio::io::duplex(65536);
    let desk = {
        let daemon_sock = daemon_sock.clone();
        let id = id.clone();
        tokio::spawn(
            async move { attach::run_with(&daemon_sock, &id, false, desk_in, desk_out).await },
        )
    };
    tokio::time::sleep(std::time::Duration::from_millis(700)).await;

    let mut phone = connect_authed(port, token).await;
    ws_send(
        &mut phone,
        &serde_json::json!({
            "type": "hello",
            "device": { "id": "phone-unbacked", "name": "phone", "kind": "phone" }
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
        "the desk window must be displaced before its claim is taken away"
    );

    // The daemon claims the shell to the desk for a window that never attaches —
    // the state the sweep exists to end.
    arc.lock()
        .await
        .handle(
            "reclaim",
            serde_json::json!({
                "id": id,
                "device": { "id": "desktop", "name": "desktop", "kind": "desktop" }
            }),
        )
        .await
        .unwrap();
    assert!(
        arc.lock().await.holds(&id, "desktop"),
        "the claim is the desk's"
    );

    // The window must come back on its own — no key pressed. What proves it is
    // pane output that the window could only see while it is attached: the marker
    // is printed *after* the claim is gone, so it reaches this duplex only if the
    // window re-attached and is subscribed again. It is printed once a second, so
    // the check does not depend on landing after the sweep.
    //
    // (Typing into the window would not prove anything here: input from a
    // displaced window is dropped, which is the point of the notice.)
    let mut recovered = false;
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
    while tokio::time::Instant::now() < deadline {
        arc.lock()
            .await
            .handle(
                "send",
                serde_json::json!({ "id": id, "text": "echo UNBACKED_MARK_4\n" }),
            )
            .await
            .unwrap();
        if wait_for_ms(&mut desk_view, "UNBACKED_MARK_4", 200).await {
            recovered = true;
            break;
        }
    }
    assert!(
        recovered,
        "the desk window must take the shell back once the unbacked claim is given up"
    );
    assert!(
        arc.lock().await.holds(&id, "desktop"),
        "and the desk holds it, with the window behind the claim"
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

/// **A desk window opens at the size of the window it is drawn in.**
///
/// The reported fault: a window opened at the wrong grid and only a manual resize cured it. The
/// client reads its terminal's size once at startup and sends it with the attach, so the first
/// snapshot already matches the pane — which means a wrong reading at startup is never corrected
/// by the mismatch check afterwards. This test gives the client a pty of exactly 100x30 and asks
/// the shell inside the pane what size it has: `stty size` must say `30 100`.
#[cfg(unix)]
// **Multi-threaded, because this test blocks.** The pty is read with `std::fs` and the deadline
// is `std::thread::sleep`; on the default single-threaded runtime that stalls the daemon itself,
// which then never accepts the window and the test fails with an empty screen.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_window_opens_at_the_size_of_the_terminal_it_is_drawn_in() {
    use std::io::{Read, Write};
    use std::os::fd::AsRawFd;

    let dir = scratch();
    let keeper_sock = dir.join("keeper.sock");
    let _reaper = common::Reaper::new(&dir, &keeper_sock);
    let mut keeper = std::process::Command::new(keeper_bin())
        .arg(keeper_sock.to_string_lossy().as_ref())
        .spawn()
        .unwrap();
    wait_socket(&keeper_sock).await;

    let d = LdmuxDaemon::new(dir.clone(), keeper_bin())
        .with_remote("127.0.0.1".to_string(), 0, "size-token".to_string());
    let arc = Arc::new(tokio::sync::Mutex::new(d));
    {
        let mut d = arc.lock().await;
        d.start(&arc).await.unwrap();
    }
    let cwd = std::env::current_dir().unwrap().to_string_lossy().into_owned();
    let id = arc
        .lock()
        .await
        .create_session(&cwd, None, "local")
        .await
        .unwrap()
        .id;

    // The window: a pty of exactly 100x30, which is what the client will read.
    let size = nix::pty::Winsize {
        ws_row: 30,
        ws_col: 100,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    let Some(attach_bin) = attach_bin() else {
        eprintln!("skipping: target/debug/ldmux-attach is not built; run `cargo build -p ldmux-attach`");
        arc.lock().await.stop();
        let _ = child_keeper(&mut keeper);
        let _ = std::fs::remove_dir_all(&dir);
        return;
    };
    let pty = nix::pty::openpty(Some(&size), None).unwrap();
    let slave = std::fs::File::from(pty.slave);
    let slave_out = slave.try_clone().unwrap();
    let mut child = std::process::Command::new(attach_bin)
        .arg(&id)
        .env("LDMUX_STATE_DIR", &dir)
        .stdin(std::process::Stdio::from(slave))
        .stdout(std::process::Stdio::from(slave_out))
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let mut master = std::fs::File::from(pty.master);
    // Non-blocking reads inside a deadline, so a client that never answers fails the test
    // rather than hanging it.
    nix::fcntl::fcntl(master.as_raw_fd(), nix::fcntl::FcntlArg::F_SETFL(nix::fcntl::OFlag::O_NONBLOCK))
        .unwrap();

    // Wait for the first screen, then ask the shell what size it thinks it has.
    let mut seen = String::new();
    let mut asked = false;
    let mut buf = [0u8; 8192];
    let mut status = None;
    for _ in 0..400 {
        std::thread::sleep(std::time::Duration::from_millis(25));
        if let Ok(Some(st)) = child.try_wait() {
            status = Some(st);
            break;
        }
        match master.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => seen.push_str(&String::from_utf8_lossy(&buf[..n])),
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
            Err(_) => break,
        }
        if !asked && !seen.is_empty() {
            master.write_all(b"stty size\n").unwrap();
            asked = true;
        }
        if seen.contains("30 100") {
            break;
        }
    }
    let ok = seen.contains("30 100");
    let _ = child.kill();
    let err = child
        .stderr
        .take()
        .and_then(|mut e| {
            let mut s = String::new();
            std::io::Read::read_to_string(&mut e, &mut s).ok().map(|_| s)
        })
        .unwrap_or_default();
    let _ = child.wait();
    if !ok {
        eprintln!("attach exit: {:?} stderr: {err}", status);
    }
    arc.lock().await.stop();
    let _ = std::process::Command::new("kill")
        .arg("-TERM")
        .arg(keeper.id().to_string())
        .output();
    let _ = keeper.wait();
    let _ = std::fs::remove_dir_all(&dir);
    assert!(
        ok,
        "the pane must be the window's own grid (30 100); terminal said {:?}",
        seen.chars().rev().take(300).collect::<String>().chars().rev().collect::<String>()
    );
}
