//! Ownership handover, size-on-attach, and close-≠-detach (plan §6, the desktop
//! mirror). A real keeper + daemon + two remote-WS clients, no X server: the
//! i3 placement itself is covered by `desktop_e2e.rs`; this covers what an
//! attached window *means* — who holds the shell, what the pane size becomes,
//! and that closing a window is not killing the session.
//!
//! The attach client's own loop is covered by `attach_client.rs`.

mod common;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use ldmux_daemon::daemon::LdmuxDaemon;

type Ws =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

fn scratch() -> PathBuf {
    let mut p = std::env::temp_dir();
    p.push(format!(
        "ldmux-handover-{}-{}",
        std::process::id(),
        rand::random::<u64>()
    ));
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
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("socket did not appear: {}", path.display());
}

async fn connect(port: u16, token: &str) -> Ws {
    use tokio_tungstenite::tungstenite::client::IntoClientRequest;
    let mut req = format!("ws://127.0.0.1:{port}/ws")
        .into_client_request()
        .unwrap();
    req.headers_mut()
        .insert("Authorization", format!("Bearer {token}").parse().unwrap());
    let (ws, _) = tokio_tungstenite::connect_async(req).await.unwrap();
    ws
}

async fn send(ws: &mut Ws, msg: serde_json::Value) {
    use futures_util::sink::SinkExt;
    ws.send(tokio_tungstenite::tungstenite::Message::Text(
        serde_json::to_string(&msg).unwrap().into(),
    ))
    .await
    .unwrap();
}

/// Read until a text message of type `ty` arrives (decoding binary output).
async fn next_of_type(ws: &mut Ws, ty: &str) -> serde_json::Value {
    use futures_util::stream::StreamExt;
    for _ in 0..200 {
        let Some(Ok(m)) = ws.next().await else {
            panic!("connection closed waiting for {ty}");
        };
        if let tokio_tungstenite::tungstenite::Message::Text(t) = m {
            let v: serde_json::Value = serde_json::from_str(t.as_ref()).unwrap();
            if v["type"] == ty {
                return v;
            }
        }
    }
    panic!("no {ty} message");
}

/// Kill the keeper when the test ends, however it ends.
struct KeeperGuard(std::process::Child);
impl Drop for KeeperGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[tokio::test]
async fn handover_size_and_close_does_not_detach() {
    let dir = scratch();
    let keeper_sock = dir.join("keeper.sock");
    let _reaper = common::Reaper::new(&dir, &keeper_sock);
    let keeper = std::process::Command::new(keeper_bin())
        .arg(keeper_sock.to_string_lossy().as_ref())
        .spawn()
        .unwrap();
    let _keeper_guard = KeeperGuard(keeper);
    wait_socket(&keeper_sock).await;

    let port: u16 = 51000 + (rand::random::<u16>() % 4000);
    let token = "handover-token";
    let d = LdmuxDaemon::new(dir.clone(), keeper_bin()).with_remote(
        "127.0.0.1".to_string(),
        port,
        token.to_string(),
    );
    let arc = Arc::new(tokio::sync::Mutex::new(d));
    {
        let mut d = arc.lock().await;
        d.start(&arc).await.unwrap();
    }
    let cwd = dir.to_string_lossy().into_owned();
    let id = arc
        .lock()
        .await
        .create_session("shell", &cwd, None, "local")
        .await
        .unwrap()
        .id;
    tokio::time::sleep(Duration::from_millis(500)).await;

    // A attaches with a size: the pane takes it (size-on-attach).
    let mut a = connect(port, token).await;
    send(
        &mut a,
        serde_json::json!({"type":"hello","device":{"id":"A","name":"desk","kind":"desktop"}}),
    )
    .await;
    send(
        &mut a,
        serde_json::json!({"type":"attach","sessionId":id,"cols":100,"rows":30}),
    )
    .await;
    let caps = next_of_type(&mut a, "caps").await;
    assert_eq!(caps["cols"], 100, "attach resizes the pane: {caps}");
    assert_eq!(caps["rows"], 30);
    let snap = next_of_type(&mut a, "snapshot").await;
    assert_eq!(snap["cols"], 100, "the snapshot matches the new grid");
    assert_eq!(snap["rows"], 30);
    assert_eq!(
        arc.lock().await.ownership().owner(&id).unwrap().device.id,
        "A"
    );

    // B attaches with no takeover: refused, told who holds it, and A keeps it.
    let mut b = connect(port, token).await;
    send(
        &mut b,
        serde_json::json!({"type":"hello","device":{"id":"B","name":"phone","kind":"phone"}}),
    )
    .await;
    send(
        &mut b,
        serde_json::json!({"type":"attach","sessionId":id,"cols":80,"rows":24}),
    )
    .await;
    let owned = next_of_type(&mut b, "owned").await;
    assert_eq!(owned["owner"]["id"], "A", "B is told A holds it: {owned}");
    assert_eq!(
        arc.lock().await.ownership().owner(&id).unwrap().device.id,
        "A"
    );

    // B takes over (the phone's reclaim / the window's r): A is cut off.
    send(
        &mut b,
        serde_json::json!({"type":"attach","sessionId":id,"takeover":true,"cols":80,"rows":24}),
    )
    .await;
    let caps_b = next_of_type(&mut b, "caps").await;
    assert_eq!(caps_b["cols"], 80);
    let owned_a = next_of_type(&mut a, "owned").await;
    assert!(
        owned_a["owner"].is_null(),
        "A is told it lost the shell: {owned_a}"
    );
    assert_eq!(
        arc.lock().await.ownership().owner(&id).unwrap().device.id,
        "B"
    );

    // A takes it back — the desk window's r, which is the one reclaim the spec
    // allows. B is told it lost the shell, and A owns it again.
    send(
        &mut a,
        serde_json::json!({"type":"attach","sessionId":id,"takeover":true,"cols":100,"rows":30}),
    )
    .await;
    let caps_a2 = next_of_type(&mut a, "caps").await;
    assert_eq!(caps_a2["cols"], 100);
    let owned_b = next_of_type(&mut b, "owned").await;
    assert!(
        owned_b["owner"].is_null(),
        "B is told it lost the shell: {owned_b}"
    );
    assert_eq!(
        arc.lock().await.ownership().owner(&id).unwrap().device.id,
        "A",
        "a displaced window reclaims with takeover"
    );

    // B takes it once more, so the rest of this test still describes B as the
    // holder (the close/grace assertions below).
    send(
        &mut b,
        serde_json::json!({"type":"attach","sessionId":id,"takeover":true,"cols":80,"rows":24}),
    )
    .await;
    let _ = next_of_type(&mut b, "caps").await;
    let _ = next_of_type(&mut a, "owned").await;
    assert_eq!(
        arc.lock().await.ownership().owner(&id).unwrap().device.id,
        "B"
    );

    // Closing B is not detaching: the claim survives the socket for the grace.
    drop(b);
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(
        arc.lock().await.ownership().owner(&id).unwrap().device.id,
        "B",
        "a closed window does not drop the claim immediately"
    );
    assert_eq!(arc.lock().await.get_session(&id).unwrap().status, "running");

    // After the release grace the claim is freed — and the session still lives.
    tokio::time::sleep(Duration::from_millis(3500)).await;
    assert!(
        arc.lock().await.ownership().owner(&id).is_none(),
        "the grace releases the claim"
    );
    assert_eq!(
        arc.lock().await.get_session(&id).unwrap().status,
        "running",
        "closing the window never killed the shell"
    );

    // A fresh attach still finds a live shell (it was never killed).
    let mut c = connect(port, token).await;
    send(
        &mut c,
        serde_json::json!({"type":"hello","device":{"id":"C","name":"desk","kind":"desktop"}}),
    )
    .await;
    send(
        &mut c,
        serde_json::json!({"type":"attach","sessionId":id,"cols":90,"rows":20}),
    )
    .await;
    let caps_c = next_of_type(&mut c, "caps").await;
    assert_eq!(caps_c["cols"], 90);

    drop(a);
    drop(c);
    {
        let mut d = arc.lock().await;
        d.stop();
    }
}
/// A daemon with a real keeper and a few shells to play with.
///
/// The port is asked for as 0 — whatever the kernel has free — and read back
/// from `remote_info`, so no two tests in this workspace can collide on a
/// fixed number.
struct Harness {
    port: u16,
    token: String,
    arc: Arc<tokio::sync::Mutex<LdmuxDaemon>>,
    _keeper: KeeperGuard,
    /// Last field: dropped last, on success and on panic.
    _reaper: common::Reaper,
}

impl Harness {
    async fn new(sessions: usize) -> (Self, Vec<String>) {
        let dir = scratch();
        let keeper_sock = dir.join("keeper.sock");
        let _reaper = common::Reaper::new(&dir, &keeper_sock);
        let keeper = std::process::Command::new(keeper_bin())
            .arg(keeper_sock.to_string_lossy().as_ref())
            .spawn()
            .unwrap();
        let _keeper = KeeperGuard(keeper);
        wait_socket(&keeper_sock).await;

        let token = format!("handover-{}", rand::random::<u32>());
        let d = LdmuxDaemon::new(dir.clone(), keeper_bin()).with_remote(
            "127.0.0.1".to_string(),
            0,
            token.clone(),
        );
        let arc = Arc::new(tokio::sync::Mutex::new(d));
        {
            let mut d = arc.lock().await;
            d.start(&arc).await.unwrap();
        }
        let port = arc
            .lock()
            .await
            .handle("remote_info", serde_json::json!({}))
            .await
            .unwrap()["port"]
            .as_u64()
            .unwrap() as u16;
        let cwd = std::env::current_dir()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        let mut ids = Vec::new();
        for _ in 0..sessions {
            ids.push(
                arc.lock()
                    .await
                    .create_session("shell", &cwd, None, "local")
                    .await
                    .unwrap()
                    .id,
            );
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
        (
            Self {
                port,
                token,
                arc,
                _keeper,
                _reaper,
            },
            ids,
        )
    }
}

/// Read until any `sessions` message arrives, or give up.
async fn wait_for_any_sessions(ws: &mut Ws) -> bool {
    use futures_util::stream::StreamExt;
    for _ in 0..300 {
        let Some(Ok(m)) = ws.next().await else {
            return false;
        };
        if let tokio_tungstenite::tungstenite::Message::Text(t) = m
            && let Ok(v) = serde_json::from_str::<serde_json::Value>(t.as_ref())
            && v["type"] == "sessions"
        {
            return true;
        }
    }
    false
}

/// Read until a `sessions` message says `id` has no owner, or give up.
async fn wait_owner_cleared(ws: &mut Ws, id: &str) -> bool {
    use futures_util::stream::StreamExt;
    for _ in 0..300 {
        let Some(Ok(m)) = ws.next().await else {
            return false;
        };
        if let tokio_tungstenite::tungstenite::Message::Text(t) = m {
            let v: serde_json::Value = serde_json::from_str(t.as_ref()).unwrap();
            if v["type"] == "sessions"
                && let Some(s) = v["sessions"]
                    .as_array()
                    .and_then(|a| a.iter().find(|s| s["id"].as_str() == Some(id)))
                && s.get("owner").map(|o| o.is_null()).unwrap_or(true)
            {
                return true;
            }
        }
    }
    false
}

/// A connection that moves to another shell lets the first one go.
///
/// A claim means "this device is watching this shell". Nothing released it when
/// the connection attached somewhere else, so a client that moves between shells
/// over one socket — which the protocol allows, and which is how the desktop
/// shim worked — held its last shell for good. Nobody could take it without a
/// takeover, and a desk window it had displaced sat on "Press r to take it back"
/// for a shell no device was actually looking at.
#[tokio::test]
async fn a_connection_that_moves_to_another_shell_lets_the_first_one_go() {
    let (h, ids) = Harness::new(2).await;
    let (s1, s2) = (ids[0].clone(), ids[1].clone());

    let mut c = connect(h.port, &h.token).await;
    send(
        &mut c,
        serde_json::json!({"type":"hello","device":{"id":"mover","name":"desk","kind":"desktop"}}),
    )
    .await;
    send(
        &mut c,
        serde_json::json!({"type":"attach","sessionId":s1,"cols":100,"rows":30}),
    )
    .await;
    let _ = next_of_type(&mut c, "caps").await;
    assert_eq!(
        h.arc.lock().await.ownership().owner(&s1).unwrap().device.id,
        "mover"
    );

    // The same connection attaches to the other shell.
    send(
        &mut c,
        serde_json::json!({"type":"attach","sessionId":s2,"cols":100,"rows":30}),
    )
    .await;
    let _ = next_of_type(&mut c, "caps").await;

    assert_eq!(
        h.arc.lock().await.ownership().owner(&s2).unwrap().device.id,
        "mover",
        "the shell it moved to is the one it holds"
    );
    assert!(
        h.arc.lock().await.ownership().owner(&s1).is_none(),
        "the shell it moved away from must not stay held by a device that is not watching it"
    );
}

/// A phone putting a desk shell down is news for every client, not only the
/// phone that did it.
///
/// `close` sent the session list back to the connection that asked, so the
/// displaced desk window — the one client that needs to hear it — heard nothing
/// directly. It usually heard it a couple of seconds later from the maintenance
/// tick's "something changed" broadcast, which is a coincidence rather than a
/// contract, and a client that is not in that broadcast's path waits forever.
#[tokio::test]
async fn putting_a_desk_shell_down_reaches_the_other_clients() {
    let (h, ids) = Harness::new(1).await;
    let id = ids[0].clone();

    // The desk window holds its shell.
    let mut desk = connect(h.port, &h.token).await;
    send(
        &mut desk,
        serde_json::json!({"type":"hello","device":{"id":"desk-1","name":"desk","kind":"desktop"}}),
    )
    .await;
    send(
        &mut desk,
        serde_json::json!({"type":"attach","sessionId":id,"cols":100,"rows":30}),
    )
    .await;
    let _ = next_of_type(&mut desk, "caps").await;

    // A phone takes it, so the desk window is displaced and waiting.
    let mut phone = connect(h.port, &h.token).await;
    send(
        &mut phone,
        serde_json::json!({"type":"hello","device":{"id":"phone-1","name":"phone","kind":"phone"}}),
    )
    .await;
    send(
        &mut phone,
        serde_json::json!({"type":"attach","sessionId":id,"cols":80,"rows":24,"takeover":true}),
    )
    .await;
    let _ = next_of_type(&mut phone, "caps").await;
    let owned = next_of_type(&mut desk, "owned").await;
    assert!(
        owned["owner"].is_null(),
        "the desk window is told it lost the shell"
    );

    // The phone puts it down. That is a release, and the desk window has to be
    // told — it is the client that acts on it.
    // Anchor against the maintenance pass first, because it is what makes this
    // assertion hard to write honestly: it runs every 2 s and broadcasts when
    // anything has set `sessions_dirty` — and a release sets it — so a release
    // tested on its own is announced by the next pass whether or not it is
    // announced when it happens. A change made through the daemon's own handle
    // (no socket, so no immediate broadcast) is announced by the pass, and seeing
    // that means the next pass is about two seconds away. A release that reaches
    // the desk inside a second was therefore broadcast when it happened.
    let cwd = std::env::current_dir()
        .unwrap()
        .to_string_lossy()
        .into_owned();
    h.arc
        .lock()
        .await
        .create_session("shell", &cwd, None, "local")
        .await
        .unwrap();
    assert!(
        wait_for_any_sessions(&mut desk).await,
        "the maintenance pass announces a new shell"
    );

    send(
        &mut phone,
        serde_json::json!({"type":"close","sessionId":id}),
    )
    .await;
    let told = tokio::time::timeout(
        Duration::from_millis(1000),
        wait_owner_cleared(&mut desk, &id),
    )
    .await;
    assert!(
        matches!(told, Ok(true)),
        "releasing a shell must reach the client waiting for it, not only the one that released it"
    );
    // Putting it down is not ending it: the shell is still there to be picked up.
    assert_eq!(
        h.arc.lock().await.get_session(&id).unwrap().status,
        "running"
    );
}
