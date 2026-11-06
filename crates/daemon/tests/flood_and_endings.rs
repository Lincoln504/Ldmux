//! The flood path and the ends of sessions, end to end (real daemon, keeper and
//! shell, raw WebSocket clients).
//!
//! - A flood is delivered in full, in bounded chunks, with no resync.
//! - A client that stops reading is repaired **in place** when it said it can be
//!   (`hello.features: ["inplaceResync"]`): a fresh `caps`+`snapshot` on the same
//!   socket. A client that said nothing gets the legacy `error`/`resync`, exactly as
//!   before.
//! - A session ending is `error`/`resync` with the message `session ended`, for every
//!   client: there is no separate ending message to understand, and the record in the
//!   list carries the exit status and how it went.

mod common;

use std::time::Duration;

use common::rig::{Rig, hello_features};
use common::wsraw::{Msg, RawWs};

const INPLACE: &[&str] = &["inplaceResync"];

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
            // One mebibyte, not twenty-four. The condition under test is
            // `backlog_bytes() > WS_BACKLOG_BYTES`, and that constant is 64 KiB
            // (`remote.rs:206`), so 24000000 bytes was 375 times more than the test needs.
            // It cost minutes of emulator work per attempt, four attempts deep, which is
            // why this binary took 160 to 320 seconds under a pool and failed its own
            // deadline on a busy machine. 1 MiB is sixteen times the threshold and leaves
            // the property, and the timing, intact.
            "data": format!("yes | head -c 4194304; echo {marker}\n"),
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
            if flood_ended {
                "the flood finished"
            } else {
                "30 s passed"
            }
        );
    }
    panic!("no backlog notice in four attempts");
}

/// A flood the client keeps up with is delivered whole: volume alone is not a resync.
#[tokio::test]
async fn a_flood_is_delivered_whole_in_bounded_frames_without_a_resync() {
    let rig = Rig::start().await;
    let (mut ws, _) = rig.attach_with(INPLACE, false).await;
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
                if v["type"] == "error" && v["code"] == "resync" {
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
    let (mut ws, _) = rig.attach_with(INPLACE, true).await;
    let mut legacy = false;
    let snap = flood_until_backlog_notice(&mut ws, "D_48", |v| {
        if v["type"] == "error" && v["code"] == "resync" {
            legacy = true;
        }
        v["type"] == "snapshot" && v["screen"].is_string()
    })
    .await;
    assert!(
        !legacy,
        "the capable client is never sent the legacy resync"
    );
    assert_eq!(snap["cols"], 80);
    assert!(snap["screen"].is_string());
    // **Liveness is proved by a marker sent after the resync, not by the flood's own.**
    // `D_48` was the wrong witness: the 4 MiB flood can finish while the snapshot is being
    // taken, so the marker is either already inside `snap["screen"]` or was discarded with
    // the screen the snapshot replaced. The wait then found nothing and burned its whole
    // 300 s ceiling (`wsraw.rs:281`) on a daemon that was perfectly alive — measured
    // 316 s before failing, on a test whose property is ordering, not speed.
    let live = "LIVE_AFTER_RESYNC";
    ws.send_json(&serde_json::json!({
        "type": "input",
        "data": format!("echo {live}\n"),
    }))
    .await;
    let mut out = Vec::new();
    ws.until_output(&mut out, live, Duration::from_millis(200))
        .await;
    assert!(
        String::from_utf8_lossy(&out).contains(live),
        "the shell was not live after the in-place resync"
    );
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

/// **How a shell ended is in the record, for every client.** The live notice is
/// `error`/`resync` (`session ended`); the list carries the status, the signal and the
/// kind, which is how the app shows "exit 3" after a reconnect. This is the coverage the
/// deleted `session_ended` message used to carry, moved onto the surface clients actually
/// read.
#[tokio::test]
async fn a_shell_that_exits_records_its_status_and_signal_in_the_list() {
    for (cmd, code, sig) in [("exit 3\n", Some(3), None), ("kill -9 $$\n", None, Some(9))] {
        let rig = Rig::start().await;
        let (mut ws, _) = rig.attach_with(INPLACE, false).await;
        ws.send_json(&serde_json::json!({"type":"input","data":cmd}))
            .await;
        let v = until_json(&mut ws, 30, |v| {
            v["type"] == "error" && v["code"] == "resync"
        })
        .await;
        assert_eq!(v["message"], "session ended");

        let mut old = rig.connect(None).await;
        hello_features(&mut old, &[]).await;
        old.send_json(&serde_json::json!({"type":"list"})).await;
        let list = until_json(&mut old, 10, |v| v["type"] == "sessions").await;
        let s = list["sessions"]
            .as_array()
            .unwrap()
            .iter()
            .find(|s| s["id"] == rig.session.as_str())
            .expect("the ended session is still listed");
        assert_eq!(s["status"], "dead");
        assert_eq!(s["endedKind"], "exited");
        assert_eq!(s["exitCode"].as_i64(), code, "{s}");
        assert_eq!(s["exitSignal"].as_i64(), sig, "{s}");

        // A client that attaches to the ended session is refused, naming it.
        let mut late = rig.connect(None).await;
        hello_features(&mut late, &[]).await;
        late.send_json(&serde_json::json!({"type":"attach","sessionId":rig.session}))
            .await;
        let e = until_json(&mut late, 10, |v| v["type"] == "error").await;
        assert_eq!(e["code"], "session_dead");
    }
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
