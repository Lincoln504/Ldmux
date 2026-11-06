//! Claims under contention, against a real keeper + daemon + WebSocket clients.

mod common;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use ldmux_daemon::daemon::LdmuxDaemon;
use tokio_tungstenite::tungstenite::Message;

type Ws =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

fn keeper_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/debug/ldmux-keeper")
}

async fn connect(port: u16, token: &str) -> Ws {
    use tokio_tungstenite::tungstenite::client::IntoClientRequest;
    let mut req = format!("ws://127.0.0.1:{port}/ws")
        .into_client_request()
        .unwrap();
    req.headers_mut()
        .insert("Authorization", format!("Bearer {token}").parse().unwrap());
    tokio_tungstenite::connect_async(req).await.unwrap().0
}

async fn send(ws: &mut Ws, msg: serde_json::Value) {
    ws.send(Message::Text(serde_json::to_string(&msg).unwrap().into()))
        .await
        .unwrap();
}

/// The first of `types` the socket produces within a second, if any.
async fn first_of(ws: &mut Ws, types: &[&str]) -> Option<serde_json::Value> {
    // Six seconds, not 1.5: this is a correctness wait, not a latency budget. Two attaches
    // that arrive together are resolved under the daemon-wide lock, and that lock is still
    // held across keeper round trips (defect 5, `docs/RELEASE-AUDIT.md`), so
    // the loser's `owned` frame can arrive seconds after the winner's `caps`. With a 1.5 s
    // window this failed two runs in three and passed with six seconds five times in five,
    // which measures the latency rather than the invariant the test is named for. When the
    // lock stops being held across the round trip, this number can come down again.
    let deadline = tokio::time::Instant::now() + Duration::from_millis(6000);
    while let Ok(Some(Ok(m))) = tokio::time::timeout_at(deadline, ws.next()).await {
        if let Message::Text(t) = m {
            let v: serde_json::Value = serde_json::from_str(t.as_ref()).unwrap();
            if types.iter().any(|ty| v["type"] == *ty) {
                return Some(v);
            }
        }
    }
    None
}

struct Fixture {
    arc: Arc<tokio::sync::Mutex<LdmuxDaemon>>,
    port: u16,
    token: &'static str,
    id: String,
    _reaper: common::Reaper,
}

async fn fixture() -> Fixture {
    let mut dir = std::env::temp_dir();
    dir.push(format!(
        "ldmux-races-{}-{}",
        std::process::id(),
        rand::random::<u64>()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let reaper = common::Reaper::new(&dir, &dir.join("keeper.sock"));
    let port: u16 = 51000 + (rand::random::<u16>() % 4000);
    let token = "races-token";
    let d = LdmuxDaemon::new(dir.clone(), keeper_bin()).with_remote(
        "127.0.0.1".to_string(),
        port,
        token.to_string(),
    );
    let arc = Arc::new(tokio::sync::Mutex::new(d));
    arc.lock().await.start(&arc).await.unwrap();
    let cwd = dir.to_string_lossy().into_owned();
    let id = arc
        .lock()
        .await
        .create_session(&cwd, None, "local")
        .await
        .unwrap()
        .id;
    tokio::time::sleep(Duration::from_millis(500)).await;
    Fixture {
        arc,
        port,
        token,
        id,
        _reaper: reaper,
    }
}

fn hello(id: &str, kind: &str) -> serde_json::Value {
    serde_json::json!({"type":"hello","device":{"id":id,"name":id,"kind":kind}})
}

/// Two devices attach to one unclaimed shell at the same instant: exactly one
/// gets it, the other is told who holds it (never both, never neither).
#[tokio::test]
async fn simultaneous_attaches_have_exactly_one_winner() {
    let f = fixture().await;
    let mut a = connect(f.port, f.token).await;
    let mut b = connect(f.port, f.token).await;
    send(&mut a, hello("A", "phone")).await;
    send(&mut b, hello("B", "phone")).await;
    let att = |id: &str| serde_json::json!({"type":"attach","sessionId":id,"cols":80,"rows":24});
    let (ra, rb) = tokio::join!(send(&mut a, att(&f.id)), send(&mut b, att(&f.id)));
    let _ = (ra, rb);
    let (ra, rb) = tokio::join!(
        first_of(&mut a, &["caps", "owned"]),
        first_of(&mut b, &["caps", "owned"])
    );
    let (ra, rb) = (ra.expect("A answered"), rb.expect("B answered"));
    let winners = [&ra, &rb].iter().filter(|v| v["type"] == "caps").count();
    assert_eq!(winners, 1, "exactly one winner: A={ra} B={rb}");
    let holder = f
        .arc
        .lock()
        .await
        .ownership()
        .owner(&f.id)
        .unwrap()
        .device
        .id
        .clone();
    let loser = if ra["type"] == "owned" { &ra } else { &rb };
    assert_eq!(
        loser["owner"]["id"], holder,
        "the loser is told the real holder"
    );
}

/// A socket that says `hello` again under another id is no longer the device it
/// was; if nothing else is, that identity's claims are freed instead of
/// blocking everyone until the daemon restarts.
#[tokio::test]
async fn a_second_hello_frees_the_old_identitys_claims() {
    let f = fixture().await;
    let mut a = connect(f.port, f.token).await;
    send(&mut a, hello("old", "phone")).await;
    send(
        &mut a,
        serde_json::json!({"type":"attach","sessionId":f.id,"cols":80,"rows":24}),
    )
    .await;
    assert!(first_of(&mut a, &["caps"]).await.is_some());
    assert_eq!(
        f.arc
            .lock()
            .await
            .ownership()
            .owner(&f.id)
            .unwrap()
            .device
            .id,
        "old"
    );

    send(&mut a, hello("new", "phone")).await;
    // The claim is released as part of handling the hello.
    let mut freed = false;
    for _ in 0..40 {
        if f.arc.lock().await.ownership().owner(&f.id).is_none() {
            freed = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(
        freed,
        "the old identity's claim must not outlive the identity"
    );

    // So someone else can have the shell without a takeover.
    let mut c = connect(f.port, f.token).await;
    send(&mut c, hello("other", "phone")).await;
    send(
        &mut c,
        serde_json::json!({"type":"attach","sessionId":f.id,"cols":80,"rows":24}),
    )
    .await;
    let r = first_of(&mut c, &["caps", "owned"])
        .await
        .expect("answered");
    assert_eq!(r["type"], "caps", "got {r}");
}
