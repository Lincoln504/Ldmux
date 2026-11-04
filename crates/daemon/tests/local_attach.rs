//! The local attach socket: the desk attach client's way in with no tailnet.
//!
//! The same WebSocket protocol and connection loop as the remote endpoint, over a
//! Unix socket (`<control>-attach.sock`) that is 0600 and checks the peer's uid.
//! The daemon here has **no remote configured at all**, which is the point: with
//! tailscaled down the window still attaches.

mod common;

use std::os::unix::fs::PermissionsExt;
use std::sync::Arc;
use std::time::Duration;

use common::wsraw::RawWs;
use ldmux_daemon::daemon::LdmuxDaemon;

#[tokio::test]
async fn a_window_attaches_over_the_local_socket_with_no_remote_configured() {
    let dir = std::env::temp_dir().join(format!(
        "ldmux-local-{}-{}",
        std::process::id(),
        rand::random::<u64>()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let _reaper = common::Reaper::new(&dir, &dir.join("keeper.sock"));
    // The claim grace is shortened: this test waits past it on purpose, and three
    // seconds is not a test's business. The sweep's cadence comes with it, or the
    // grace is invisible for up to a tick.
    let d = LdmuxDaemon::new(dir.clone(), common::rig::keeper_bin())
        .with_claim_grace_ms(150)
        .with_claim_tick_ms(50);
    let arc = Arc::new(tokio::sync::Mutex::new(d));
    arc.lock().await.start(&arc).await.unwrap();

    let control = dir.join("ldmuxd.sock");
    let local = std::path::PathBuf::from(ldmux_protocol::paths::attach_socket_path_from(
        &control.to_string_lossy(),
    ));
    assert!(local.exists(), "the attach socket is bound");
    let mode = std::fs::metadata(&local).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600, "owner-only");
    let info = arc
        .lock()
        .await
        .handle("remote_info", serde_json::json!({}))
        .await
        .unwrap();
    assert_eq!(info["listening"], false, "no tailnet endpoint is involved");

    let mut ws = RawWs::connect_unix(&local).await;
    ws.send_json(&serde_json::json!({"type":"hello","device":{"id":"desk","name":"desktop","kind":"desktop"}})).await;
    ws.send_json(&serde_json::json!({"type":"new","kind":"shell","cwd":"/tmp"}))
        .await;
    let mut out = Vec::new();
    let created = ws.until_type("created", &mut out).await;
    let sid = created["sessionId"].as_str().unwrap().to_string();
    ws.send_json(&serde_json::json!({"type":"attach","sessionId":sid,"cols":80,"rows":24}))
        .await;
    ws.until_type("caps", &mut out).await;
    ws.until_type("snapshot", &mut out).await;
    ws.send_json(&serde_json::json!({"type":"input","data":"echo LOCAL_$((6*7))\n"}))
        .await;
    ws.until_output(&mut out, "LOCAL_42", Duration::from_millis(100))
        .await;

    // The claim the window holds is backed by this connection, so the sweep that
    // frees unbacked claims leaves it alone (the table is shared with the tailnet
    // listener's).
    // Four times the grace and eight times the tick: long enough that a sweep has
    // run several times, short enough to be a test.
    tokio::time::sleep(Duration::from_millis(600)).await;
    let list = arc.lock().await.describe_sessions().await;
    let mine = list.iter().find(|s| s.id == sid).unwrap();
    assert_eq!(
        mine.owner.as_ref().map(|o| o.id.as_str()),
        Some("desk"),
        "{mine:?}"
    );

    arc.lock().await.stop();
    assert!(!local.exists(), "stop removes the attach socket");
}
