//! A multi-line paste, end to end, on the radio route's own wiring: a real keeper, a real shell,
//! and the version-15 service the phone talks to.
//!
//! **The report this exists for** (2026-11-07): copying several lines on the phone and pasting them
//! ran the first line as a command, and the rest went to whatever that command started. The phone
//! was turning every newline of a paste into a carriage return, because it assumed no pane wants
//! bracketed paste. Only the pane knows: bash asks for it (`CSI ? 2004 h`) at every prompt, and a
//! paste given the markers is *inserted* — every line at the prompt, nothing run.
//!
//! So the assertion is the operator's own: paste two `echo` lines into a shell that asked for
//! bracketed paste, and neither of them runs.
//!
//! **Driven through the service, not the frame loop.** A `Send` carrying `SendItem::Paste` is what a
//! phone sends now; `MeshRpc` reads the pane's paste mode from the screen it attached with and
//! `mesh_apply` encodes the bytes.

mod common;

use std::time::Duration;

use common::rig::Rig;
use ldmux_daemon::mesh_bridge::DaemonHost;
use ldmux_daemon::mesh_rpc::{MeshRpc, PEER_LINK};
use ldmux_protocol::mesh_term::{Message, SendItem, VERSION};

/// Attach to the rig's shell through the service, taking it over if the rig's own viewer holds it.
async fn attach(rig: &Rig) -> (MeshRpc, DaemonHost) {
    let mut rpc = MeshRpc::new();
    let mut host = DaemonHost(rig.arc.clone());
    let message = Message::Attach {
        version: VERSION,
        selector: rig.session.clone(),
        cols: 0,
        rows: 0,
    };
    rpc.on_message(0, PEER_LINK, &message, &mut host, std::time::Instant::now())
        .await
        .expect("an attach is not refused");
    if rpc.attached_to(0).is_none() {
        let takeover = Message::Takeover {
            selector: rig.session.clone(),
        };
        rpc.on_message(0, PEER_LINK, &takeover, &mut host, std::time::Instant::now())
            .await
            .expect("a takeover is not refused");
    }
    assert_eq!(
        rpc.attached_to(0),
        Some(rig.session.as_str()),
        "the service attached the rig's shell"
    );
    (rpc, host)
}

/// Send one item to the attached shell.
async fn send(rpc: &mut MeshRpc, host: &mut DaemonHost, rig: &Rig, item: SendItem) {
    let message = Message::Send {
        op_id: 1,
        chunk_idx: 0,
        chunk_count: 1,
        have_snapshot_id: 0,
        cols: 0,
        rows: 0,
        flags: 0,
        shell: rig.session.clone(),
        items: vec![item],
    };
    rpc.on_message(0, PEER_LINK, &message, host, std::time::Instant::now())
        .await
        .expect("a send is not refused");
}

/// Let the pane work, then read it as the host sees it.
///
/// **A fixed wait is a bug in a test.** Half a second was enough on an idle machine and not enough
/// on a loaded one — measured 2026-11-09 at load 29, where `bash` had not drawn a prompt and the
/// test read a blank pane — so the loop keeps going while the pane is entirely empty, with a
/// deadline that only a broken shell can reach.
async fn settle(rig: &Rig) -> Vec<String> {
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    loop {
        tokio::time::sleep(Duration::from_millis(100)).await;
        if !all_empty(&read_pane(rig).await) {
            break;
        }
        if std::time::Instant::now() >= deadline {
            break;
        }
    }
    read_pane(rig).await
}

fn all_empty(lines: &[String]) -> bool {
    lines.iter().all(|l| l.trim().is_empty())
}

/// The pane as the host sees it.
async fn read_pane(rig: &Rig) -> Vec<String> {
    let v = rig
        .arc
        .lock()
        .await
        .handle("snapshot", serde_json::json!({ "id": rig.session }))
        .await
        .expect("snapshot");
    let text = v["text"].as_str().unwrap_or_default().to_owned();
    text.lines().map(|l| l.trim_end().to_owned()).collect()
}

#[tokio::test]
async fn a_pasted_pair_of_commands_is_inserted_and_does_not_run() {
    // A shell that negotiates bracketed paste. The keeper picks `$SHELL`, and this is the one
    // behaviour the test depends on, so it is set rather than assumed.
    // SAFETY: this test binary runs one test, and the keeper it starts inherits the value.
    unsafe { std::env::set_var("SHELL", "/bin/bash") };
    let rig = Rig::start().await;
    let (mut rpc, mut host) = attach(&rig).await;

    // The prompt, and with it bash's `CSI ? 2004 h`: the mode this whole thing turns on.
    let before = settle(&rig).await;
    assert!(
        before.iter().any(|l| l.contains('$') || l.contains('#')),
        "bash drew a prompt, so its paste mode is set: {before:?}"
    );

    send(
        &mut rpc,
        &mut host,
        &rig,
        SendItem::Paste(b"echo ONE\necho TWO\n".to_vec()),
    )
    .await;
    let after = settle(&rig).await;

    // Neither line ran. If either had, its output would be a line of its own.
    assert!(
        !after.iter().any(|l| l == "ONE" || l == "TWO"),
        "a pasted line must be inserted, not executed: {after:?}"
    );
    // And both are there, as text, which is what "inserted" means to a reader.
    assert!(
        after.iter().any(|l| l.contains("echo ONE")),
        "the first pasted line is on the prompt: {after:?}"
    );
    assert!(
        after.iter().any(|l| l.contains("echo TWO")),
        "and so is the second: {after:?}"
    );
}

#[tokio::test]
async fn the_same_text_with_carriage_returns_runs_each_line() {
    // **The other half of the proof.** The test above asserts that nothing ran; this one sends the
    // same text the way the phone used to (a carriage return per line) and asserts that both lines
    // *do* run. Without it, "nothing ran" could pass for any reason — a paste that never arrived,
    // a pane that ignored input altogether — and the difference the fix makes would be untested.
    // SAFETY: this test binary's keeper inherits the value; see the test above.
    unsafe { std::env::set_var("SHELL", "/bin/bash") };
    let rig = Rig::start().await;
    let (mut rpc, mut host) = attach(&rig).await;
    settle(&rig).await;

    send(
        &mut rpc,
        &mut host,
        &rig,
        SendItem::Text(b"echo ONE\recho TWO\r".to_vec()),
    )
    .await;
    let after = settle(&rig).await;
    assert!(
        after.iter().any(|l| l == "ONE"),
        "a carriage return is Enter, so that line runs: {after:?}"
    );
    assert!(
        after.iter().any(|l| l == "TWO"),
        "and so does the next: {after:?}"
    );
}
