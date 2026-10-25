//! The flood path and the ends of sessions, end to end (real daemon, keeper and
//! shell, raw WebSocket clients).
//!
//! - A flood is delivered in full, in bounded chunks, with no resync.
//! - A client that stops reading is repaired **in place** when it said it can be
//!   (`hello.features: ["inplaceResync"]`): a fresh `caps`+`snapshot` marked
//!   `"resync": true` on the same socket. A client that said nothing gets the
//!   legacy `error`/`resync`, exactly as before.
//! - A session ending is a `session_ended` message for a client that asked
//!   (`"sessionEnded"`), with the shell's exit status; the final screen can be
//!   read by a client that reconnects. A client that said nothing sees today's
//!   `resync` / `session_dead`.

mod common;

use std::time::Duration;

use common::rig::{Rig, hello_features};
use common::wsraw::{Msg, RawWs};

const BOTH: &[&str] = &["inplaceResync", "sessionEnded"];

/// Read until a message satisfies `pick`, within `secs`.
///
/// **A client that only reads is "idle" to the daemon**, which closes a connection
/// it has not heard from in `WS_IDLE_MS` (20 s). That is right for a phone and wrong
/// for this test: 24 MB of flood through a 4 KiB receive buffer takes longer than
/// 20 s when the whole suite runs beside it, and the connection was then closed in
/// the middle of the resync it was waiting to observe — a red gate with nothing
/// wrong behind it (measured 2026-10-25: 2 of 6 tests in this file, alone, on an
/// idle machine, 0 of 6). A ping every few seconds is the same thing the phone does.
async fn until_json(
    ws: &mut RawWs,
    secs: u64,
    mut pick: impl FnMut(&serde_json::Value) -> bool,
) -> serde_json::Value {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(secs);
    // **A client that only reads is "idle" to the daemon**, which closes a
    // connection it has not heard from in `WS_IDLE_MS` (20 s). Its own 24 MB of
    // flood does not count as hearing from us, and while it is blocked writing that
    // flood it cannot read a ping either — so the ping has to be sent on a clock,
    // not when the stream goes quiet. (Measured 2026-10-25: without this, 2 of 6
    // tests in this file failed repeatedly under `cargo test` running them in
    // parallel; the daemon closed the connection and the test reported "connection
    // ended" with nothing wrong behind it.)
    let mut last_keepalive = tokio::time::Instant::now();
    loop {
        let left = deadline.saturating_duration_since(tokio::time::Instant::now());
        assert!(!left.is_zero(), "timed out waiting for a matching message");
        if last_keepalive.elapsed() >= Duration::from_secs(4) {
            ws.send_json(&serde_json::json!({"type":"ping"})).await;
            last_keepalive = tokio::time::Instant::now();
        }
        match ws.next_within(left.min(Duration::from_secs(2))).await {
            Some(Msg::Text(t)) => {
                let v: serde_json::Value = serde_json::from_str(&t).unwrap();
                if pick(&v) {
                    return v;
                }
            }
            Some(Msg::Binary(_)) => {}
            Some(Msg::Close) => panic!("connection closed by the daemon"),
            None => {}
        }
    }
}

/// Flood until the daemon's backlog rule fires, and answer with the matching message.
///
/// The premise is a race, and it is the test's premise, not the daemon's contract:
/// the client stops reading for 2.5 s while the shell floods, and the daemon's queue
/// has to pass `WS_BACKLOG_BYTES` inside that window. On a loaded machine (`cargo
/// test` runs every test binary at once, and six tests here each flood 24 MB) the
/// shell may not even have started `yes` inside 2.5 s, the flood then arrives while
/// the client is reading, and nothing is ever owed a resync — a red gate with
/// nothing wrong behind it (measured 2026-10-25: 2 of 6 tests in this file, three
/// runs in a row, while a single test alone passed in 3.9 s). A retry re-arms the
/// race; a daemon that never resyncs still fails, three attempts over.
async fn flood_until_backlog_notice(
    ws: &mut RawWs,
    marker: &str,
    mut pick: impl FnMut(&serde_json::Value) -> bool,
) -> serde_json::Value {
    for attempt in 1..=4 {
        ws.send_json(&serde_json::json!({
            "type":"input",
            "data": format!("yes | head -c 24000000; echo {marker}\n"),
        }))
        .await;
        // Do not read: this is the client that stops reading while the daemon's
        // writes back up behind the 4 KiB window.
        tokio::time::sleep(Duration::from_millis(2500)).await;

        let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
        let mut last_keepalive = tokio::time::Instant::now();
        let mut out: Vec<u8> = Vec::new();
        let mut flood_ended = false;
        while tokio::time::Instant::now() < deadline {
            if last_keepalive.elapsed() >= Duration::from_secs(4) {
                ws.send_json(&serde_json::json!({"type":"ping"})).await;
                last_keepalive = tokio::time::Instant::now();
            }
            match ws.next_within(Duration::from_secs(2)).await {
                Some(Msg::Text(t)) => {
                    let v: serde_json::Value = serde_json::from_str(&t).unwrap();
                    if pick(&v) {
                        return v;
                    }
                }
                Some(Msg::Binary(b)) => {
                    common::wsraw::decode_output(&b, &mut out);
                    if out.len() > 4096 {
                        let from = out.len() - 4096;
                        if String::from_utf8_lossy(&out[from..]).contains(marker) {
                            flood_ended = true;
                            break;
                        }
                    }
                }
                Some(Msg::Close) => panic!("the daemon closed the connection"),
                None => {}
            }
        }
        // Neither: the attempt neither lined up nor proved anything. Re-arm it.
        eprintln!(
            "attempt {attempt}: {} without a backlog notice; retrying",
            if flood_ended { "the flood finished" } else { "30 s passed" }
        );
    }
    panic!("no backlog notice in four attempts");
}

/// A flood the client keeps up with is delivered whole: volume alone is not a resync.
#[tokio::test]
async fn a_flood_is_delivered_whole_in_bounded_frames_without_a_resync() {
    let rig = Rig::start().await;
    let (mut ws, _) = rig.attach_with(BOTH, false).await;
    ws.send_json(
        &serde_json::json!({"type":"input","data":"yes | head -c 3000000; echo D_$((6*7))\n"}),
    )
    .await;
    let mut out = Vec::new();
    let mut resyncs = 0;
    let mut max_frame = 0usize;
    let mut saw = false;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(60);
    while !saw && tokio::time::Instant::now() < deadline {
        match ws.next_within(Duration::from_secs(10)).await {
            Some(Msg::Binary(b)) => {
                max_frame = max_frame.max(b.len());
                common::wsraw::decode_output(&b, &mut out);
                let from = out.len().saturating_sub(4096);
                saw = String::from_utf8_lossy(&out[from..]).contains("D_42");
            }
            Some(Msg::Text(t)) => {
                let v: serde_json::Value = serde_json::from_str(&t).unwrap();
                if v["type"] == "error" && v["code"] == "resync"
                    || v["type"] == "snapshot" && v["resync"] == true
                {
                    resyncs += 1;
                }
            }
            _ => break,
        }
    }
    assert!(saw, "the end of the flood never arrived");
    assert_eq!(resyncs, 0, "a client that keeps up is not resynced");
    assert!(
        max_frame <= 80 * 1024,
        "output frames are bounded by the peek chunk: {max_frame}"
    );
    assert!(
        out.len() >= 4_500_000,
        "3 MB of `y\\n` is 6 MB once the tty adds CR: got {}",
        out.len()
    );
}

/// A client that stops reading is repaired on the same socket.
#[tokio::test]
async fn a_client_that_fell_behind_gets_an_in_place_snapshot_not_a_forced_reconnect() {
    let rig = Rig::start().await;
    let (mut ws, _) = rig.attach_with(BOTH, true).await;
    let mut legacy = false;
    let snap = flood_until_backlog_notice(&mut ws, "D_48", |v| {
        if v["type"] == "error" && v["code"] == "resync" {
            legacy = true;
        }
        v["type"] == "snapshot" && v["resync"] == true
    })
    .await;
    assert!(
        !legacy,
        "the capable client is never sent the legacy resync"
    );
    assert_eq!(snap["cols"], 80);
    assert!(snap["screen"].is_string());
    // And it is live again afterwards: the end of the flood turns up.
    let mut out = Vec::new();
    ws.until_output(&mut out, "D_48", Duration::from_millis(200))
        .await;
}

#[tokio::test]
async fn a_client_that_said_nothing_still_gets_the_legacy_resync() {
    let rig = Rig::start().await;
    let (mut ws, _) = rig.attach_with(&[], true).await;
    let v = flood_until_backlog_notice(&mut ws, "D_54", |v| {
        v["type"] == "error" && v["code"] == "resync"
    })
    .await;
    assert!(v["message"].is_string());
}

/// The ending of a shell, for a client that asked, with how it ended.
#[tokio::test]
async fn a_shell_that_exits_is_a_session_ended_with_its_status() {
    for (cmd, kind, code, sig) in [
        ("exit 3\n", "exited", Some(3), None),
        ("kill -9 $$\n", "exited", None, Some(9)),
    ] {
        let rig = Rig::start().await;
        let (mut ws, _) = rig.attach_with(BOTH, false).await;
        ws.send_json(&serde_json::json!({"type":"input","data":cmd}))
            .await;
        let ended = until_json(&mut ws, 30, |v| v["type"] == "session_ended").await;
        assert_eq!(ended["sessionId"], rig.session.as_str());
        assert_eq!(ended["kind"], kind, "{ended}");
        assert_eq!(ended["exitCode"].as_i64(), code, "{ended}");
        assert_eq!(ended["exitSignal"].as_i64(), sig, "{ended}");
        assert!(ended["reason"].is_string());
    }
}

/// A client that reconnects after the shell ended can still read its last screen.
#[tokio::test]
async fn a_reconnecting_client_reads_the_final_screen_then_the_ending() {
    let rig = Rig::start().await;
    let (mut ws, _) = rig.attach_with(BOTH, false).await;
    ws.send_json(&serde_json::json!({"type":"input","data":"echo LAST_WORDS_$((6*7)); exit 7\n"}))
        .await;
    until_json(&mut ws, 30, |v| v["type"] == "session_ended").await;
    drop(ws);

    // Capable client: caps + snapshot (with the last screen) + session_ended.
    let mut ws = rig.connect(None).await;
    hello_features(&mut ws, BOTH).await;
    ws.send_json(&serde_json::json!({"type":"attach","sessionId":rig.session}))
        .await;
    let mut out = Vec::new();
    ws.until_type("caps", &mut out).await;
    let snap = ws.until_type("snapshot", &mut out).await;
    assert!(
        snap["screen"].as_str().unwrap().contains("LAST_WORDS_42"),
        "{snap}"
    );
    let ended = ws.until_type("session_ended", &mut out).await;
    assert_eq!(ended["exitCode"], 7);

    // A client that said nothing is refused as it always was.
    let mut old = rig.connect(None).await;
    hello_features(&mut old, &[]).await;
    old.send_json(&serde_json::json!({"type":"attach","sessionId":rig.session}))
        .await;
    let e = until_json(&mut old, 10, |v| v["type"] == "error").await;
    assert_eq!(e["code"], "session_dead");

    // The list carries the structured ending.
    old.send_json(&serde_json::json!({"type":"list"})).await;
    let list = until_json(&mut old, 10, |v| v["type"] == "sessions").await;
    let s = list["sessions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["id"] == rig.session.as_str())
        .unwrap();
    assert_eq!(s["status"], "dead");
    assert_eq!(s["endedKind"], "exited");
    assert_eq!(s["exitCode"], 7);
}

/// A client that did not ask for endings sees the legacy `resync` when its shell ends.
#[tokio::test]
async fn a_legacy_client_is_still_told_resync_when_its_shell_ends() {
    let rig = Rig::start().await;
    let (mut ws, _) = rig.attach_with(&[], false).await;
    ws.send_json(&serde_json::json!({"type":"input","data":"exit\n"}))
        .await;
    let v = until_json(&mut ws, 30, |v| {
        v["type"] == "error" && v["code"] == "resync"
    })
    .await;
    assert_eq!(v["message"], "session ended");
}
