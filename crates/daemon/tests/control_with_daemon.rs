use ldmux_daemon::daemon::LdmuxDaemon;
use ldmux_daemon::fake_keeper::FakeKeeper;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

#[tokio::test]
async fn control_socket_with_daemon_handle() {
    let dir = std::env::temp_dir().join(format!("ldmux-ctrl-test-{}", rand::random::<u64>()));
    std::fs::create_dir_all(&dir).unwrap();
    let keeper_sock = dir.join("keeper.sock");
    let fk = std::sync::Arc::new(FakeKeeper::new(keeper_sock.clone()));
    let fk_serve = fk.clone();
    let fk_sock = keeper_sock.clone();
    tokio::spawn(async move {
        FakeKeeper::serve(&fk_serve, fk_sock.as_path())
            .await
            .unwrap();
    });
    for _ in 0..50 {
        if keeper_sock.exists() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }

    let d = LdmuxDaemon::new(dir.clone(), std::path::PathBuf::from("/nonexistent"));
    let arc: std::sync::Arc<tokio::sync::Mutex<LdmuxDaemon>> =
        std::sync::Arc::new(tokio::sync::Mutex::new(d));
    {
        let mut d = arc.lock().await;
        d.start(&arc).await.unwrap();
    }
    let sock_path = arc.lock().await.socket_path();

    // Connect directly to the control socket.
    let mut client = tokio::net::UnixStream::connect(sock_path).await.unwrap();

    // Send a ping.
    let req = serde_json::json!({"id": 1, "method": "ping"});
    let mut out = req.to_string();
    out.push('\n');
    client.write_all(out.as_bytes()).await.unwrap();
    client.flush().await.unwrap();

    // Read the response.
    let mut buf = Vec::new();
    let mut byte = [0u8; 1];
    loop {
        let n = tokio::time::timeout(std::time::Duration::from_secs(5), client.read(&mut byte))
            .await
            .unwrap_or_else(|_| panic!("timeout waiting for response"))
            .unwrap();
        if n == 0 || byte[0] == b'\n' {
            break;
        }
        buf.push(byte[0]);
    }
    let resp: serde_json::Value = serde_json::from_slice(&buf).unwrap();
    eprintln!("response: {resp}");
    assert_eq!(resp["ok"], true);
    assert_eq!(resp["result"]["pong"], true);

    drop(fk);
    let _ = std::fs::remove_dir_all(&dir);
}
