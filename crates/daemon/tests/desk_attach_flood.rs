//! The desk window's "link lost, reconnecting", end to end (real daemon, keeper and
//! shell, raw WebSocket clients).
//!
//! A client that says nothing about `inplaceResync` (the desk window did not) and
//! attaches to a shell that is flooding can have more than `MAX_PENDING_BYTES`
//! arrive between subscribe and snapshot; the daemon then answers with the legacy
//! `error`/`resync`. That notice used to bypass the connection's byte accounting,
//! the writer subtracted a length nobody had added, `queued_json` wrapped below
//! zero, and the **next** control message on the connection — the window's first
//! heartbeat — panicked on `attempt to add with overflow` and ended it.
//!
//! The panic needs the writer to have gone quiet before that next message, so each
//! attempt drains the attach and then pings, as a window does. Measured against the
//! parent of the fix: about 40% of attempts killed the connection; with the fix, none.

mod common;

use std::time::Duration;

use common::rig::{Rig, hello, hello_features};
use common::wsraw::Msg;

#[tokio::test]
async fn a_desk_style_attach_to_a_flooding_shell_never_kills_the_connection() {
    let rig = Rig::start().await;
    // The flood, from a client that is allowed to read it.
    let (mut feeder, _) = rig.attach_with(&["inplaceResync"], false).await;
    feeder
        .send_json(&serde_json::json!({
            "type": "input",
            "data": "yes 0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef | head -c 2000000000\n"
        }))
        .await;
    tokio::time::sleep(Duration::from_millis(500)).await;

    let mut notices = 0usize;
    for attempt in 0..40u32 {
        // No `features`: exactly the old desk window's hello and attach.
        let mut ws = rig.connect(None).await;
        hello(&mut ws).await;
        ws.send_json(&serde_json::json!({
            "type": "attach", "sessionId": rig.session, "cols": 158, "rows": 82
        }))
        .await;
        // Let the attach finish and the writer go quiet.
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        let mut snapshot = false;
        while !snapshot && tokio::time::Instant::now() < deadline {
            let left = deadline.saturating_duration_since(tokio::time::Instant::now());
            match ws.next_within(left).await {
                Some(Msg::Text(t)) => {
                    let v: serde_json::Value = serde_json::from_str(&t).unwrap();
                    if v["type"] == "error" && v["code"] == "resync" {
                        notices += 1;
                    }
                    snapshot |= v["type"] == "snapshot";
                }
                Some(Msg::Binary(_)) => {}
                Some(Msg::Close) | None => break,
            }
        }
        assert!(snapshot, "attempt {attempt}: no snapshot");
        tokio::time::sleep(Duration::from_millis(300)).await;

        // The window's first heartbeat. A dead connection answers nothing.
        ws.send_json(&serde_json::json!({"type": "ping", "id": attempt}))
            .await;
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        let mut ponged = false;
        while !ponged && tokio::time::Instant::now() < deadline {
            let left = deadline.saturating_duration_since(tokio::time::Instant::now());
            match ws.next_within(left).await {
                Some(Msg::Text(t)) => {
                    let v: serde_json::Value = serde_json::from_str(&t).unwrap();
                    ponged = v["type"] == "pong";
                }
                Some(Msg::Binary(_)) => {}
                Some(Msg::Close) | None => break,
            }
        }
        assert!(
            ponged,
            "attempt {attempt}: the connection died after the attach backlog notice ({notices} seen)"
        );
        // Enough to have exercised the path; stop early.
        if notices >= 3 {
            break;
        }
    }
    assert!(
        notices > 0,
        "the attach backlog path was never taken, so this test proved nothing"
    );
}

/// The path the shipped desk window takes now. It advertises `inplaceResync`, so a
/// backlog during the attach is repainted by the snapshot on the same socket and
/// the legacy notice is never sent. The connection must still survive its first
/// heartbeat.
///
/// This is a smoke test of the shipped path rather than a proof that the 1 MiB
/// backlog was crossed: with `inplaceResync` the daemon marks nothing observable,
/// so the regression itself stays pinned by the legacy-path test above and by the
/// unit test for the in-place client.
#[tokio::test]
async fn an_in_place_desk_attach_to_a_flooding_shell_keeps_its_connection() {
    let rig = Rig::start().await;
    let (mut feeder, _) = rig.attach_with(&["inplaceResync"], false).await;
    feeder
        .send_json(&serde_json::json!({
            "type": "input",
            "data": "yes 0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef | head -c 2000000000\n"
        }))
        .await;
    tokio::time::sleep(Duration::from_millis(500)).await;

    for attempt in 0..5u32 {
        let mut ws = rig.connect(None).await;
        hello_features(&mut ws, &["inplaceResync"]).await;
        ws.send_json(&serde_json::json!({
            "type": "attach", "sessionId": rig.session, "cols": 158, "rows": 82
        }))
        .await;
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        let mut snapshot = false;
        while !snapshot && tokio::time::Instant::now() < deadline {
            let left = deadline.saturating_duration_since(tokio::time::Instant::now());
            match ws.next_within(left).await {
                Some(Msg::Text(t)) => {
                    let v: serde_json::Value = serde_json::from_str(&t).unwrap();
                    assert!(
                        !(v["type"] == "error" && v["code"] == "resync"),
                        "attempt {attempt}: an in-place client must not get the legacy notice"
                    );
                    snapshot |= v["type"] == "snapshot";
                }
                Some(Msg::Binary(_)) => {}
                Some(Msg::Close) | None => break,
            }
        }
        assert!(snapshot, "attempt {attempt}: no snapshot");
        tokio::time::sleep(Duration::from_millis(200)).await;

        ws.send_json(&serde_json::json!({"type": "ping", "id": attempt}))
            .await;
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        let mut ponged = false;
        while !ponged && tokio::time::Instant::now() < deadline {
            let left = deadline.saturating_duration_since(tokio::time::Instant::now());
            match ws.next_within(left).await {
                Some(Msg::Text(t)) => {
                    let v: serde_json::Value = serde_json::from_str(&t).unwrap();
                    ponged = v["type"] == "pong";
                }
                Some(Msg::Binary(_)) => {}
                Some(Msg::Close) | None => break,
            }
        }
        assert!(
            ponged,
            "attempt {attempt}: the connection died on the in-place path"
        );
    }
}
