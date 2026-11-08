//! A `Key` message over the mesh bridge reaches a REAL pane as the bytes **that pane's mode**
//! asks for (ADR-0025, version 2).
//!
//! The mesh terminal used to send raw bytes built on the phone, which cannot know the pane's
//! cursor-key mode. Now the phone sends a name and the daemon's one key table encodes it. The
//! program here is `cat -v` in raw mode, so what it prints is exactly what the daemon wrote.
//! Two panes, same key: with application cursor keys on (`ESC [ ? 1 h`, which vim, less and
//! htop set) `Up` must be `ESC O A`, without it `ESC [ A`. A bridge that ignored the mode would
//! send the same bytes to both and fail one of these.

mod common;

use std::sync::{Arc, Mutex};
use std::time::Duration;

use common::rig::Rig;
use ldmux_daemon::mesh_bridge::{Bridge, DaemonHost, Link};
use ldmux_protocol::mesh_term::{Decoder, Message, VERSION};

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

fn phone_says(link: &FakeLink, m: &Message) {
    link.0
        .lock()
        .unwrap()
        .inbound
        .push((1, m.encode().unwrap()));
}

/// Start a program in the rig's pane that echoes what it reads, in the mode asked for, and wait
/// until the keeper's emulator really is in that mode: the daemon reads the mode from there.
async fn pane_in_mode(rig: &Rig, app_cursor: bool) -> common::wsraw::RawWs {
    let (mut ws, _) = rig.attach_with(&[], false).await;
    let set = if app_cursor {
        "printf '\\033[?1h'; "
    } else {
        ""
    };
    ws.send_json(&serde_json::json!({
        "type":"input",
        "data": format!("{set}stty raw -echo; exec cat -v\n"),
    }))
    .await;
    let mut seen = Vec::new();
    for _ in 0..100 {
        let snap = rig
            .arc
            .lock()
            .await
            .handle("snapshot", serde_json::json!({ "id": rig.session }))
            .await
            .unwrap();
        if snap["appCursor"].as_bool() == Some(app_cursor) {
            // Mode set; also make sure the program is reading raw (the prompt is gone).
            ws.until_output(&mut seen, "", Duration::from_millis(400))
                .await;
            return ws;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("the pane never reached application cursor mode {app_cursor}");
}

async fn press_up_through_the_bridge(rig: &Rig) -> (Bridge<DaemonHost, FakeLink>, FakeLink) {
    let link = FakeLink::default();
    let mut bridge = Bridge::new(DaemonHost(rig.arc.clone()), link.clone());
    // **The rig's own WebSocket client holds this shell**, so a plain attach is refused with
    // `HELD` (version 8) — by design: a phone does not steal a shell another device is watching.
    // Taking it over is what the phone does when the user answers `take it back`.
    phone_says(
        &link,
        &Message::Attach {
            version: VERSION,
            selector: rig.session.clone(),
        },
    );
    bridge.tick().await;
    phone_says(
        &link,
        &Message::Takeover {
            selector: rig.session.clone(),
        },
    );
    bridge.tick().await;
    assert!(bridge.attached(1), "the bridge attached the rig's shell");
    phone_says(&link, &Message::Key(vec!["Up".into()]));
    bridge.tick().await;
    (bridge, link)
}

/// Wait until the **pane** shows [want], by asking the daemon what the keeper's emulator has.
///
/// The viewer's own stream is not the thing to read here. The bridge takes the shell over, and a
/// takeover tells the displaced viewer to stop (`notify_displaced`), so its stream can end at any
/// moment — the first version of these two tests read it and failed under the test pool with
/// "never saw ...; tail \"\"" whenever the takeover won a race it is allowed to win. The pane's
/// content is what the tests are about: which bytes the daemon wrote.
async fn pane_shows(rig: &Rig, want: &str) {
    let deadline = std::time::Instant::now() + Duration::from_secs(20);
    loop {
        let snap = rig
            .arc
            .lock()
            .await
            .handle("capture", serde_json::json!({ "id": rig.session }))
            .await
            .unwrap();
        let screen = snap.get("screen").and_then(|s| s.as_str()).unwrap_or("");
        if screen.contains(want) {
            return;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the pane never showed {want:?}; it reads {screen:?}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

#[tokio::test]
async fn up_is_ss3_when_the_pane_asked_for_application_cursor_keys() {
    let rig = Rig::start().await;
    let _ws = pane_in_mode(&rig, true).await;
    let (bridge, _link) = press_up_through_the_bridge(&rig).await;
    assert_eq!(bridge.stats.keys, 1);
    // `cat -v` renders ESC as `^[`: `ESC O A` is `^[OA`.
    pane_shows(&rig, "^[OA").await;
    // ...and the normal-mode form is not what the pane got.
    let snap = rig
        .arc
        .lock()
        .await
        .handle("capture", serde_json::json!({ "id": rig.session }))
        .await
        .unwrap();
    let screen = snap.get("screen").and_then(|s| s.as_str()).unwrap_or("");
    assert!(
        !screen.contains("^[[A"),
        "the normal-mode arrow must not be sent to a pane in application mode"
    );
}

#[tokio::test]
async fn up_is_csi_in_a_pane_that_did_not() {
    let rig = Rig::start().await;
    let _ws = pane_in_mode(&rig, false).await;
    let (_bridge, _link) = press_up_through_the_bridge(&rig).await;
    pane_shows(&rig, "^[[A").await;
}

#[tokio::test]
async fn the_profile_names_the_shell_the_host_chose() {
    let rig = Rig::start().await;
    let link = FakeLink::default();
    let mut bridge = Bridge::new(DaemonHost(rig.arc.clone()), link.clone());
    // An empty selector with exactly one shell running: the host picks it and says which.
    phone_says(
        &link,
        &Message::Attach {
            version: VERSION,
            selector: String::new(),
        },
    );
    bridge.tick().await;
    // **An attach holds its first frame briefly** so the phone's own grid can arrive before the pane
    // is drawn at whatever geometry a desk window left behind (`ATTACH_GRACE`). This rig is real
    // time, so the wait is real: without it the attach is answered and the screen is not yet due.
    tokio::time::sleep(std::time::Duration::from_millis(1_600)).await;
    bridge.tick().await;
    let sent = link.0.lock().unwrap().sent.clone();
    // The profile, not "the first record": a version-9 phone is also sent the mark table, and that
    // page is a record of its own.
    let profile = sent
        .iter()
        .flat_map(|(_, bytes)| Decoder::new().push(bytes).unwrap())
        .find_map(|m| match m {
            Ok(p @ Message::Profile { .. }) => Some(p),
            _ => None,
        })
        .expect("the host answered with a profile");
    match profile {
        Message::Profile { version, shell, .. } => {
            assert_eq!(version, VERSION);
            assert_eq!(shell, rig.session);
        }
        _ => unreachable!(),
    }
}

/// Version 3: a `Resize` reaches the REAL keeper, so the pane's own grid changes (the thing a
/// tailnet client's resize does), and the next screen the phone gets carries a `Profile` with
/// exactly that grid. A bridge that only reset its differ would answer with the old 80 columns.
#[tokio::test]
async fn a_resize_changes_the_real_pane_and_the_next_profile_says_so() {
    let rig = Rig::start().await;
    let link = FakeLink::default();
    let mut bridge = Bridge::new(DaemonHost(rig.arc.clone()), link.clone());
    phone_says(
        &link,
        &Message::Attach {
            version: VERSION,
            selector: rig.session.clone(),
        },
    );
    bridge.tick().await;
    link.0.lock().unwrap().sent.clear();
    phone_says(&link, &Message::Resize { cols: 41, rows: 17 });
    bridge.tick().await;
    assert_eq!(bridge.stats.resizes, 1);
    let snap = rig
        .arc
        .lock()
        .await
        .handle("snapshot", serde_json::json!({ "id": rig.session }))
        .await
        .unwrap();
    assert_eq!(
        (snap["cols"].as_u64(), snap["rows"].as_u64()),
        (Some(41), Some(17))
    );
    let mut d = Decoder::new();
    let mut messages = Vec::new();
    for (_, bytes) in link.0.lock().unwrap().sent.iter() {
        messages.extend(d.push(bytes).unwrap().into_iter().map(Result::unwrap));
    }
    assert!(
        matches!(
            messages.first(),
            Some(Message::Profile {
                cols: 41,
                rows: 17,
                ..
            })
        ),
        "the first message after a resize is a Profile with the new grid: {messages:?}"
    );
}
