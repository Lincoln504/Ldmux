//! The desktop attach client against a real daemon: `attach::run_with`
//! connects to the daemon's remote, attaches, streams the pane, and forwards
//! input. This is the desk-window path of plan §3.9 (a protocol client, not a
//! tmux client).

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
    let sock = keeper_sock.to_string_lossy().into_owned();
    let _ = std::process::Command::new("pkill").args(["-f", &sock]).output();
    let _ = std::fs::remove_dir_all(&dir);
}