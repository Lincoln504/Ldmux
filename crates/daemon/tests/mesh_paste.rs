//! A multi-line paste, end to end, on the radio route's own wiring: a real keeper, a real shell,
//! and the bridge the phone talks to.
//!
//! **The report this exists for** (2026-11-07): copying several lines on the phone and pasting them
//! ran the first line as a command, and the rest went to whatever that command started. The phone
//! was turning every newline of a paste into a carriage return, because it assumed no pane wants
//! bracketed paste. Only the pane knows: bash asks for it (`CSI ? 2004 h`) at every prompt, and a
//! paste given the markers is *inserted* — every line at the prompt, nothing run.
//!
//! So the assertion is the operator's own: paste two `echo` lines into a shell that asked for
//! bracketed paste, and neither of them runs.

mod common;

use std::sync::{Arc, Mutex};
use std::time::Duration;

use common::rig::Rig;
use ldmux_daemon::mesh_bridge::{Bridge, DaemonHost, Link};
use ldmux_protocol::mesh_term::Message;

/// The version these tests exercise the **frame loop** with.
///
/// `VERSION` is 15 and a version-15 phone is served by the request/answer service
/// (`crates/daemon/src/mesh_rpc.rs`), which builds no `Rows`/`Commit` at all. Everything here
/// asserts on frames, holds, echoes or a real pane's repaint — the version-14 path, which is what
/// the shipped app still uses — so these attach as 14.
const FRAME_LOOP_VERSION: u8 = 14;


#[derive(Default)]
struct Wire {
    inbound: Vec<(u8, Vec<u8>)>,
    sent: Vec<(u8, Vec<u8>)>,
}

#[derive(Clone, Default)]
struct FakeLink(Arc<Mutex<Wire>>);

impl Link for FakeLink {
    fn established(&self) -> bool {
        true
    }
    fn generation(&self) -> u32 {
        1
    }
    fn pending(&self) -> usize {
        0
    }
    fn send(&self, shell: u8, bytes: &[u8]) -> Result<(), String> {
        self.0.lock().unwrap().sent.push((shell, bytes.to_vec()));
        Ok(())
    }
    fn take_inbound(&self) -> Vec<(u8, Vec<u8>)> {
        std::mem::take(&mut self.0.lock().unwrap().inbound)
    }
}

impl FakeLink {
    fn phone_says(&self, m: &Message) {
        let bytes = m.encode().expect("encode");
        self.0.lock().unwrap().inbound.push((1, bytes));
    }
}

/// Let the pane and the bridge work, then read the pane as the host sees it.
///
/// **A fixed wait is a bug in a test.** Half a second was enough on an idle machine and not enough
/// on a loaded one — measured 2026-11-09 at load 29, where `bash` had not drawn a prompt and the
/// test read a blank pane — so the loop keeps going while the pane is entirely empty, with a
/// deadline that only a broken shell can reach.
async fn settle(bridge: &mut Bridge<DaemonHost, FakeLink>, rig: &Rig) -> Vec<String> {
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    loop {
        for _ in 0..4 {
            bridge.tick().await;
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        if all_empty(&read_pane(rig).await) {
            if std::time::Instant::now() < deadline {
                continue;
            }
            break;
        }
        break;
    }
    // **One more pass after the prompt is visible.** The bridge records the pane's paste mode from
    // the screen a pass reads, and the snapshot can show the prompt one pass before the tick that
    // sees it: the paste then travelled as ordinary text and both lines ran. Two passes make the
    // flag and the picture agree.
    for _ in 0..2 {
        bridge.tick().await;
        tokio::time::sleep(Duration::from_millis(25)).await;
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
    let link = FakeLink::default();
    let mut bridge = Bridge::new(DaemonHost(rig.arc.clone()), link.clone());
    link.phone_says(&Message::Attach {
        version: FRAME_LOOP_VERSION,
        selector: rig.session.clone(),
        cols: 0,
        rows: 0,
    });
    bridge.tick().await;
    if !bridge.attached(1) {
        link.phone_says(&Message::Takeover {
            selector: rig.session.clone(),
        });
        bridge.tick().await;
    }
    assert!(bridge.attached(1), "the bridge attached the rig's shell");

    // The prompt, and with it bash's `CSI ? 2004 h`: the mode this whole thing turns on.
    let before = settle(&mut bridge, &rig).await;
    assert!(
        before.iter().any(|l| l.contains('$') || l.contains('#')),
        "bash drew a prompt, so its paste mode is set: {before:?}"
    );

    link.phone_says(&Message::Input(b"echo ONE\necho TWO\n".to_vec()));
    let after = settle(&mut bridge, &rig).await;
    let _ = &before;

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
    let link = FakeLink::default();
    let mut bridge = Bridge::new(DaemonHost(rig.arc.clone()), link.clone());
    link.phone_says(&Message::Attach {
        version: FRAME_LOOP_VERSION,
        selector: rig.session.clone(),
        cols: 0,
        rows: 0,
    });
    bridge.tick().await;
    if !bridge.attached(1) {
        link.phone_says(&Message::Takeover {
            selector: rig.session.clone(),
        });
        bridge.tick().await;
    }
    assert!(bridge.attached(1), "the bridge attached the rig's shell");
    settle(&mut bridge, &rig).await;

    link.phone_says(&Message::Input(b"echo ONE\recho TWO\r".to_vec()));
    let after = settle(&mut bridge, &rig).await;
    assert!(
        after.iter().any(|l| l == "ONE"),
        "a carriage return is Enter, so that line runs: {after:?}"
    );
    assert!(
        after.iter().any(|l| l == "TWO"),
        "and so does the next: {after:?}"
    );
}
