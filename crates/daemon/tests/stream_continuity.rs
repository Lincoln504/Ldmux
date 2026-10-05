//! What a viewer sees at the seam between a snapshot and live output.
//!
//! The keeper feeds every byte to the emulator *and* to a buffer the daemon
//! polls. A snapshot already contains whatever is still in that buffer, so
//! replaying it as live output paints it twice. A fresh stream used to do that
//! with everything an unwatched shell had printed. These tests pin the seam
//! against a real keeper + daemon + WebSocket client.

mod common;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use ldmux_daemon::daemon::LdmuxDaemon;
use tokio_tungstenite::tungstenite::Message;

type Ws = tokio_tungstenite::WebSocketStream<
    tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
>;

fn keeper_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/debug/ldmux-keeper")
}

async fn connect(port: u16, token: &str) -> Ws {
    use tokio_tungstenite::tungstenite::client::IntoClientRequest;
    let mut req = format!("ws://127.0.0.1:{port}/ws").into_client_request().unwrap();
    req.headers_mut().insert("Authorization", format!("Bearer {token}").parse().unwrap());
    tokio_tungstenite::connect_async(req).await.unwrap().0
}

async fn send(ws: &mut Ws, msg: serde_json::Value) {
    ws.send(Message::Text(serde_json::to_string(&msg).unwrap().into())).await.unwrap();
}

/// Everything the socket produces for `ms`: (json messages, output-frame bytes
/// in arrival order tagged with how many json messages preceded them).
async fn collect(ws: &mut Ws, ms: u64) -> Vec<Item> {
    let mut items = Vec::new();
    let deadline = tokio::time::Instant::now() + Duration::from_millis(ms);
    while let Ok(Some(Ok(m))) = tokio::time::timeout_at(deadline, ws.next()).await {
        match m {
            Message::Text(t) => items.push(Item::Json(serde_json::from_str(&t.to_string()).unwrap())),
            Message::Binary(b) if b.len() >= 8 && b[0] == 0x10 => {
                items.push(Item::Output(String::from_utf8_lossy(&b[8..]).into_owned()))
            }
            _ => {}
        }
    }
    items
}

enum Item {
    Json(serde_json::Value),
    Output(String),
}

fn snapshot_screen(items: &[Item]) -> String {
    items
        .iter()
        .find_map(|i| match i {
            Item::Json(v) if v["type"] == "snapshot" => v["screen"].as_str().map(str::to_string),
            _ => None,
        })
        .expect("a snapshot arrived")
}

/// Output frames that arrive AFTER the snapshot message, concatenated.
fn output_after_snapshot(items: &[Item]) -> String {
    let mut seen = false;
    let mut out = String::new();
    for i in items {
        match i {
            Item::Json(v) if v["type"] == "snapshot" => seen = true,
            Item::Output(t) if seen => out.push_str(t),
            _ => {}
        }
    }
    out
}

#[tokio::test]
async fn an_unwatched_shells_output_is_in_the_snapshot_and_not_replayed() {
    let mut dir = std::env::temp_dir();
    dir.push(format!("ldmux-continuity-{}-{}", std::process::id(), rand::random::<u64>()));
    std::fs::create_dir_all(&dir).unwrap();
    let keeper_sock = dir.join("keeper.sock");
    let _reaper = common::Reaper::new(&dir, &keeper_sock);

    let port: u16 = 51000 + (rand::random::<u16>() % 4000);
    let token = "continuity-token";
    let d = LdmuxDaemon::new(dir.clone(), keeper_bin())
        .with_remote("127.0.0.1".to_string(), port, token.to_string());
    let arc = Arc::new(tokio::sync::Mutex::new(d));
    arc.lock().await.start(&arc).await.unwrap();
    let cwd = dir.to_string_lossy().into_owned();
    let id = arc.lock().await.create_session("shell", &cwd, None, "local").await.unwrap().id;
    tokio::time::sleep(Duration::from_millis(600)).await;

    // Output while nobody is watching: no stream exists, the keeper buffers it.
    arc.lock()
        .await
        .handle("send", serde_json::json!({"id": id, "text": "printf 'BACKLOG-%s\\n' MARK", "keys": ["Enter"]}))
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(800)).await;

    let mut ws = connect(port, token).await;
    send(&mut ws, serde_json::json!({"type":"hello","device":{"id":"A","name":"a","kind":"phone"}})).await;
    send(&mut ws, serde_json::json!({"type":"attach","sessionId":id,"cols":100,"rows":30})).await;
    let items = collect(&mut ws, 1500).await;

    assert!(snapshot_screen(&items).contains("BACKLOG-MARK"), "the snapshot has what was printed");
    let live = output_after_snapshot(&items);
    assert!(
        !live.contains("BACKLOG-MARK"),
        "the backlog must not be replayed on top of the snapshot, got: {live:?}"
    );

    // ...and live output still flows after the seam.
    send(&mut ws, serde_json::json!({"type":"input","data":"printf 'LIVE-%s\\n' OK\r"})).await;
    let items = collect(&mut ws, 1500).await;
    let live: String = items.iter().filter_map(|i| if let Item::Output(t) = i { Some(t.as_str()) } else { None }).collect();
    assert!(live.contains("LIVE-OK"), "live output follows the snapshot, got: {live:?}");

    // A resize re-snapshots at the same seam: it neither loses nor repeats.
    send(&mut ws, serde_json::json!({"type":"resize","cols":90,"rows":28})).await;
    let items = collect(&mut ws, 1000).await;
    assert!(snapshot_screen(&items).contains("LIVE-OK"));
    assert!(!output_after_snapshot(&items).contains("LIVE-OK"));

    arc.lock().await.stop();
}
