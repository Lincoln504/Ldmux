//! The desktop attach client against a real daemon: `attach::run_with`
//! connects to the daemon's remote, attaches, streams the pane, and forwards
//! input. This is the desk-window path of plan §3.9 (a protocol client, not a
//! tmux client).

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
