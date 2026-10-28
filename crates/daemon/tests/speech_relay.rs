//! Voice mode, Channel A (docs/VOICE-MODE.md sections 3.5 and 6): the daemon relays what an
//! agent wants said to the clients viewing that session, and only to those that asked.
//! A client that advertised nothing gets exactly the messages it always got.

mod common;

use std::time::Duration;

use common::rig::Rig;
use common::wsraw::{Msg, RawWs};

async fn speak(rig: &Rig, params: serde_json::Value) -> Result<serde_json::Value, String> {
    rig.arc.lock().await.handle("speech", params).await
}

/// Read messages for `ms`, returning every `speech` one.
async fn speech_within(ws: &mut RawWs, ms: u64) -> Vec<serde_json::Value> {
    let mut out = Vec::new();
    let deadline = tokio::time::Instant::now() + Duration::from_millis(ms);
    loop {
        let left = deadline.saturating_duration_since(tokio::time::Instant::now());
        if left.is_zero() {
            return out;
        }
        match ws.next_within(left).await {
            Some(Msg::Text(t)) => {
                let v: serde_json::Value = serde_json::from_str(&t).unwrap();
                if v["type"] == "speech" {
                    out.push(v);
                }
            }
            Some(_) => {}
            None => return out,
        }
    }
}

#[tokio::test]
async fn only_a_client_that_asked_for_speech_receives_it() {
    let rig = Rig::start().await;
    let (mut capable, _) = rig.attach_with(&["speechChannel"], false).await;
    let (mut plain, _) = rig.attach_with(&[], false).await;

    let r = speak(
        &rig,
        serde_json::json!({"sessionId": rig.session, "text": "Done. Two files changed.", "id": "m1", "final": true}),
    )
    .await
    .unwrap();
    assert_eq!(r["delivered"], 1, "one client advertised speechChannel");
    assert_eq!(r["seq"], 1);

    let got = speech_within(&mut capable, 800).await;
    assert_eq!(got.len(), 1);
    assert_eq!(got[0]["text"], "Done. Two files changed.");
    assert_eq!(got[0]["id"], "m1");
    assert_eq!(got[0]["final"], true);
    assert_eq!(got[0]["seq"], 1);
    assert_eq!(got[0]["sessionId"], rig.session);

    assert!(
        speech_within(&mut plain, 500).await.is_empty(),
        "a client that said nothing is never sent a speech message"
    );
}

#[tokio::test]
async fn sequence_numbers_rise_per_session_and_default_final_is_false() {
    let rig = Rig::start().await;
    let (mut ws, _) = rig.attach_with(&["speechChannel"], false).await;
    for want in 1..=3u64 {
        let r = speak(
            &rig,
            serde_json::json!({"sessionId": rig.session, "text": "x"}),
        )
        .await
        .unwrap();
        assert_eq!(r["seq"], want);
    }
    let got = speech_within(&mut ws, 800).await;
    assert_eq!(
        got.iter()
            .map(|v| v["seq"].as_u64().unwrap())
            .collect::<Vec<_>>(),
        [1, 2, 3]
    );
    assert!(got.iter().all(|v| v["final"] == false && v["id"].is_null()));
}

#[tokio::test]
async fn a_bad_request_is_refused_and_takes_no_sequence_number() {
    let rig = Rig::start().await;
    let (_ws, _) = rig.attach_with(&["speechChannel"], false).await;
    let s = &rig.session;
    assert!(
        speak(&rig, serde_json::json!({"sessionId": s, "text": "   "}))
            .await
            .unwrap_err()
            .contains("empty")
    );
    assert!(
        speak(&rig, serde_json::json!({"sessionId": s}))
            .await
            .is_err()
    );
    let big = "a".repeat(ldmux_daemon::daemon::SPEECH_MAX_BYTES + 1);
    assert!(
        speak(&rig, serde_json::json!({"sessionId": s, "text": big}))
            .await
            .unwrap_err()
            .contains("over")
    );
    assert!(
        speak(&rig, serde_json::json!({"sessionId": "nope", "text": "hi"}))
            .await
            .unwrap_err()
            .contains("no such session")
    );
    assert!(
        speak(
            &rig,
            serde_json::json!({"sessionId": s, "text": "hi", "id": ""})
        )
        .await
        .is_err()
    );
    let ok = speak(&rig, serde_json::json!({"sessionId": s, "text": "hi"}))
        .await
        .unwrap();
    assert_eq!(ok["seq"], 1, "none of the refused calls consumed a number");
}

#[tokio::test]
async fn speech_with_no_viewer_is_accepted_and_delivered_to_nobody() {
    let rig = Rig::start().await;
    let r = speak(
        &rig,
        serde_json::json!({"sessionId": rig.session, "text": "anyone?"}),
    )
    .await
    .unwrap();
    assert_eq!(r["delivered"], 0, "the caller learns nobody heard it");
}
