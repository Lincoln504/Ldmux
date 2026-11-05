//! permessage-deflate on the remote WebSocket (ADR-0019), against a real daemon,
//! a real keeper and a real shell.
//!
//! Two contracts are pinned here:
//!
//! 1. **Negotiation.** The daemon offers nothing unless the client does, honours
//!    what it can (`*_no_context_takeover`, `client_max_window_bits`) and declines
//!    what it cannot (`server_max_window_bits` under 15). An existing client that
//!    sends no `Sec-WebSocket-Extensions` sees today's bytes exactly.
//! 2. **The gain.** The workloads of `docs/WIRE-COST.md`, byte-counted on the wire
//!    with and without the extension. The floors are well under what is measured,
//!    so they catch the extension silently turning off or the dictionary being
//!    reset per message, not noise. Run with `--nocapture` for the table.

mod common;

use std::time::Duration;

use common::rig::Rig;
use common::wsraw::RawWs;

const OFFER: &str = "permessage-deflate; client_max_window_bits";

#[tokio::test]
async fn negotiation_follows_the_offer() {
    let rig = Rig::start().await;
    let plain = rig.connect(None).await;
    assert_eq!(plain.extension, None, "no offer, no extension");

    let ws = rig.connect(Some(OFFER)).await;
    assert_eq!(ws.extension.as_deref(), Some("permessage-deflate"));

    let ws = rig
        .connect(Some(
            "permessage-deflate; server_no_context_takeover; client_no_context_takeover",
        ))
        .await;
    assert_eq!(
        ws.extension.as_deref(),
        Some("permessage-deflate; server_no_context_takeover; client_no_context_takeover")
    );

    // A window the backend cannot honour is declined, not half-accepted.
    let ws = rig
        .connect(Some("permessage-deflate; server_max_window_bits=10"))
        .await;
    assert_eq!(ws.extension, None);
    // ... and the next offer is taken.
    let ws = rig
        .connect(Some(
            "permessage-deflate; server_max_window_bits=10, permessage-deflate",
        ))
        .await;
    assert_eq!(ws.extension.as_deref(), Some("permessage-deflate"));
    // An extension we do not know is not echoed.
    let ws = rig.connect(Some("x-webkit-deflate-frame")).await;
    assert_eq!(ws.extension, None);
}

#[tokio::test]
async fn a_compressed_session_works_end_to_end_with_one_message_per_frame() {
    let rig = Rig::start().await;
    let (mut ws, snap) = rig.attach(Some(OFFER)).await;
    assert_eq!(snap["cols"], 80);
    let mut out = Vec::new();
    ws.send_json(&serde_json::json!({"type":"input","data":"echo WIRE_$((6*7))_OK; seq 1 300\n"}))
        .await;
    ws.until_output(&mut out, "WIRE_42_OK", Duration::from_millis(150))
        .await;
    ws.until_output(&mut out, "300", Duration::from_millis(150))
        .await;
    let text = String::from_utf8_lossy(&out);
    assert!(
        text.contains("WIRE_42_OK") && text.contains("\r\n300"),
        "{text:?}"
    );
    assert!(
        ws.compressed_messages > 0,
        "a 1 KB+ output must travel compressed"
    );
    // Every message counted is a whole WS message: the reader asserts no
    // fragmentation, so reaching here is the one-frame-per-message check.
}

#[tokio::test]
async fn a_client_that_compresses_its_own_messages_is_understood() {
    // The raw client sends uncompressed frames (legal under the extension). A
    // compressed inbound message is covered by the codec unit tests; here: the
    // negotiated connection still reads plain client frames.
    let rig = Rig::start().await;
    let (mut ws, _) = rig.attach(Some(OFFER)).await;
    ws.send_json(&serde_json::json!({"type":"ping","id":7}))
        .await;
    let mut out = Vec::new();
    let pong = ws.until_type("pong", &mut out).await;
    assert_eq!(pong["id"], 7);
}

/// One workload's cost: `(messages, wire bytes, compressed messages)`.
type Cost = (u64, u64, u64);

async fn run(ws: &mut RawWs, input: &str, needle: &str) -> Cost {
    let mut out = Vec::new();
    ws.reset_counters();
    ws.send_json(&serde_json::json!({"type":"input","data":input}))
        .await;
    ws.until_output(&mut out, needle, Duration::from_millis(250))
        .await;
    (ws.messages, ws.wire_bytes, ws.compressed_messages)
}

async fn workloads(rig: &Rig, offer: Option<&str>) -> Vec<(&'static str, Cost)> {
    let (mut ws, _) = rig.attach(offer).await;
    let mut rows = Vec::new();
    // Settle the prompt.
    let _ = run(&mut ws, "echo READY_$((1+1))\n", "READY_2").await;
    rows.push(("one keystroke", run(&mut ws, "x", "x").await));
    let _ = run(&mut ws, "\u{15}", "").await; // C-u: clear the line
    rows.push((
        "ls -la",
        run(
            &mut ws,
            "ls -la --color=always /usr/lib | head -n 40; echo D_$((6*7))\n",
            "D_42",
        )
        .await,
    ));
    rows.push((
        "seq 1 100",
        run(&mut ws, "seq 1 100; echo D_$((6*8))\n", "D_48").await,
    ));
    rows.push((
        "ls -laR (8000 lines)",
        run(
            &mut ws,
            "ls -laR /usr/lib 2>/dev/null | head -n 8000; echo D_$((6*9))\n",
            "D_54",
        )
        .await,
    ));
    rows.push((
        "yes flood (4 MB)",
        run(&mut ws, "yes | head -c 4000000; echo D_$((7*7))\n", "D_49").await,
    ));
    // The attach snapshot of a screen with content on it.
    let _ = run(
        &mut ws,
        "clear; ls -la --color=always /usr/lib | head -n 20; echo D_$((7*8))\n",
        "D_56",
    )
    .await;
    let (ws2, snap) = rig.attach(offer).await;
    assert!(snap["screen"].as_str().unwrap().contains("D_56"));
    rows.push((
        "attach snapshot",
        (ws2.messages, ws2.wire_bytes, ws2.compressed_messages),
    ));
    rows
}

/// The measured gain, pinned. Numbers are printed with `--nocapture`.
#[tokio::test]
async fn compression_cuts_the_wire_cost_of_every_real_workload() {
    let rig = Rig::start().await;
    let plain = workloads(&rig, None).await;
    let rig2 = Rig::start().await;
    let comp = workloads(&rig2, Some(OFFER)).await;

    println!(
        "\n{:<24} {:>10} {:>12} {:>12} {:>8}",
        "workload", "msgs", "plain B", "deflate B", "ratio"
    );
    for ((name, p), (_, c)) in plain.iter().zip(comp.iter()) {
        println!(
            "{:<24} {:>10} {:>12} {:>12} {:>7.1}x",
            name,
            format!("{}/{}", p.0, c.0),
            p.1,
            c.1,
            p.1 as f64 / c.1 as f64
        );
    }
    println!();

    let get = |rows: &Vec<(&'static str, Cost)>, n: &str| rows.iter().find(|r| r.0 == n).unwrap().1;
    // The ratio floors: under the measured, far over 1.
    //
    // `seq 1 100` is 1.5, not 2.0, because its ratio depends on how many messages the
    // 100-line burst is batched into: three plain messages against two compressed ones
    // measures 1.78x, while a run that batches into two and two measures over 2x. Both are
    // correct behaviour and which one happens depends on timing, so a 2.0 floor failed on a
    // loaded machine and passed on an idle one. With the extension off the ratio is 1.0 and
    // with a per-message dictionary it is also ~1.0, so 1.5 still catches exactly what this
    // test exists to catch.
    for (name, floor) in [
        ("ls -la", 3.0),
        ("seq 1 100", 1.5),
        ("ls -laR (8000 lines)", 5.0),
        ("yes flood (4 MB)", 100.0),
        ("attach snapshot", 2.5),
    ] {
        let (p, c) = (get(&plain, name), get(&comp, name));
        let ratio = p.1 as f64 / c.1 as f64;
        assert!(
            ratio >= floor,
            "{name}: {} B plain vs {} B compressed is {ratio:.2}x, floor {floor}x",
            p.1,
            c.1
        );
        assert!(c.2 > 0, "{name}: nothing was compressed");
    }
    // A keystroke echo is not worth compressing: it must not be made larger by
    // more than the cost of nothing (it is sent plain).
    let (p, c) = (get(&plain, "one keystroke"), get(&comp, "one keystroke"));
    assert_eq!(c.2, 0, "a tiny frame goes plain");
    assert!(
        c.1 <= p.1 + 8,
        "a keystroke cost {} B compressed vs {} B plain",
        c.1,
        p.1
    );
}

/// Serve a daemon for an *independent* client to talk to (Python `websockets`,
/// a browser): prints `INTEROP port token session` and runs until the file named
/// by `LDMUX_INTEROP_STOP` exists. Not part of the gate.
/// `LDMUX_INTEROP_STOP=/tmp/stop cargo test -p ldmux-daemon --test wire_compression -- --ignored --nocapture serve_for_interop`
#[tokio::test]
#[ignore]
async fn serve_for_interop() {
    let rig = Rig::start().await;
    println!("INTEROP {} {} {}", rig.port, rig.token, rig.session);
    let stop = std::env::var("LDMUX_INTEROP_STOP").expect("LDMUX_INTEROP_STOP");
    for _ in 0..600 {
        if std::path::Path::new(&stop).exists() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}
