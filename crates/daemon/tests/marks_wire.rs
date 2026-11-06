//! Marks over the tailnet route: the table a device is handed when it says hello, and the push that
//! follows a new one.
//!
//! The radio route replays on attach (`docs/ARCHITECTURE.md` §11); this is the other half, where the
//! phone's presence connection is alive and a mark should reach it while no shell page is open. Both
//! ends of it matter: a client that did **not** ask for buzzes must never be sent one (the wire's
//! additive rule), and a client that did must be handed the table it missed as well as the live push.

mod common;

use std::time::Duration;

use common::rig::Rig;
use common::wsraw::{Msg, RawWs};

/// Read frames until a JSON message of `ty` arrives, or the deadline passes.
async fn until_type(ws: &mut RawWs, ty: &str, wait: Duration) -> Option<serde_json::Value> {
    let deadline = std::time::Instant::now() + wait;
    while std::time::Instant::now() < deadline {
        let left = deadline.saturating_duration_since(std::time::Instant::now());
        match ws.next_within(left).await {
            Some(Msg::Text(t)) => {
                if let Ok(v) = serde_json::from_str::<serde_json::Value>(&t)
                    && v.get("type").and_then(|x| x.as_str()) == Some(ty)
                {
                    return Some(v);
                }
            }
            Some(_) => {}
            None => return None,
        }
    }
    None
}

#[tokio::test]
async fn a_client_that_asks_for_buzzes_is_handed_the_table_and_the_next_push() {
    let rig = Rig::start().await;
    // A mark made before the phone connects: this is "while you were away".
    let made = rig
        .arc
        .lock()
        .await
        .handle(
            "buzz",
            serde_json::json!({"session": rig.session, "kind": "attention", "note": "over here"}),
        )
        .await
        .unwrap();
    assert_eq!(made["ok"], serde_json::Value::Bool(true), "{made}");

    let mut ws = rig.connect(None).await;
    ws.send_json(&serde_json::json!({
        "type":"hello",
        "device":{"id":"wire-presence","name":"Presence","kind":"phone"},
        "features":["buzz"],
    }))
    .await;

    // The replay: the whole table, not a stream of events.
    let table = until_type(&mut ws, "marks", Duration::from_secs(5))
        .await
        .expect("the marks table on hello");
    let marks = table["marks"].as_array().expect("marks array");
    let mine = marks
        .iter()
        .find(|m| m["session"].as_str() == Some(rig.session.as_str()))
        .expect("the mark made before the connection");
    assert_eq!(mine["kind"], serde_json::Value::String("attention".into()));
    assert_eq!(mine["note"], serde_json::Value::String("over here".into()));

    // The live push, on the next maintenance tick. A *second* shell, because a repeat mark for the
    // same one inside the coalescing window is refused on purpose ("a mark is not a progress bar"),
    // and a test that sleeps that window out would be testing the sleep.
    let second = {
        let mut d = rig.arc.lock().await;
        let created = d
            .handle("new", serde_json::json!({"cwd": "/tmp", "createdBy": "local"}))
            .await
            .unwrap();
        created["session"]["id"].as_str().expect("a new id").to_string()
    };
    let pushed = rig
        .arc
        .lock()
        .await
        .handle(
            "buzz",
            serde_json::json!({"session": second, "kind": "done"}),
        )
        .await
        .unwrap();
    assert_eq!(pushed["ok"], serde_json::Value::Bool(true), "{pushed}");
    // The first mark was made before this client existed, so it can still be in the push queue:
    // read pushes until the one for the second shell arrives.
    let deadline = std::time::Instant::now() + Duration::from_secs(6);
    let mut seen = Vec::new();
    let mut hit = None;
    while std::time::Instant::now() < deadline && hit.is_none() {
        if let Some(push) = until_type(&mut ws, "buzz", Duration::from_millis(1500)).await {
            seen.push(push["mark"]["session"].as_str().unwrap_or("").to_string());
            if push["mark"]["session"].as_str() == Some(second.as_str()) {
                assert_eq!(push["mark"]["kind"], serde_json::Value::String("done".into()));
                hit = Some(push);
            }
        }
    }
    assert!(hit.is_some(), "no push for the new mark, saw {seen:?}");
}

#[tokio::test]
async fn a_client_that_did_not_ask_is_not_sent_the_table_or_a_push() {
    let rig = Rig::start().await;
    let mut ws = rig.connect(None).await;
    common::rig::hello(&mut ws).await;
    // Nothing about marks reaches a client that said nothing, which is the whole additive rule.
    assert!(
        until_type(&mut ws, "marks", Duration::from_secs(2))
            .await
            .is_none(),
        "a client without the feature is not sent the table"
    );
    let made = rig
        .arc
        .lock()
        .await
        .handle(
            "buzz",
            serde_json::json!({"session": rig.session, "kind": "done"}),
        )
        .await
        .unwrap();
    assert_eq!(made["ok"], serde_json::Value::Bool(true), "{made}");
    assert!(
        until_type(&mut ws, "buzz", Duration::from_secs(4))
            .await
            .is_none(),
        "a client without the feature is not sent a push"
    );
}

/// A mark for a shell that does not exist is refused rather than recorded: it would be a
/// notification about nothing, holding a slot in a bounded table.
#[tokio::test]
async fn a_mark_for_an_unknown_shell_is_refused() {
    let rig = Rig::start().await;
    let answer = rig
        .arc
        .lock()
        .await
        .handle(
            "buzz",
            serde_json::json!({"session": "nope-0000", "kind": "done"}),
        )
        .await
        .unwrap();
    assert_eq!(answer["ok"], serde_json::Value::Bool(false), "{answer}");
    let table = rig
        .arc
        .lock()
        .await
        .handle("marks", serde_json::json!({}))
        .await
        .unwrap();
    assert_eq!(table["marks"].as_array().map(Vec::len), Some(0), "{table}");
}
