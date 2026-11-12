//! A key name over the radio route reaches a REAL pane as the bytes **that pane's mode** asks for
//! (ADR-0025).
//!
//! The mesh terminal used to send raw bytes built on the phone, which cannot know the pane's
//! cursor-key mode. Now the phone sends a name and the daemon's one key table encodes it. The
//! program here is `cat -v` in raw mode, so what it prints is exactly what the daemon wrote.
//! Two panes, same key: with application cursor keys on (`ESC [ ? 1 h`, which vim, less and
//! htop set) `Up` must be `ESC O A`, without it `ESC [ A`. A host that ignored the mode would
//! send the same bytes to both and fail one of these.
//!
//! **Driven through the version-15 service, not the frame loop.** The loop is gone
//! (`docs/RADIO-PLAN.md` §10a); a `Send` carrying `SendItem::Keys` is what a phone sends now, and
//! `MeshRpc` is what turns it into the daemon's `send` with `keys`.

mod common;

use std::time::Duration;

use common::rig::Rig;
use ldmux_daemon::mesh_bridge::DaemonHost;
use ldmux_daemon::mesh_rpc::{MeshRpc, PEER_LINK};
use ldmux_protocol::mesh_term::{Message, SendItem, VERSION};

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

/// Attach to the rig's shell through the service, taking it over, and press `Up`.
///
/// **The rig's own WebSocket client holds this shell**, so a plain attach is refused with `HELD` —
/// by design: a phone does not steal a shell another device is watching. Taking it over is what the
/// phone does when the user answers `take it back`.
async fn press_up(rig: &Rig) -> MeshRpc {
    let mut rpc = MeshRpc::new();
    let mut host = DaemonHost(rig.arc.clone());
    let attach = Message::Attach {
        version: VERSION,
        selector: rig.session.clone(),
        cols: 0,
        rows: 0,
    };
    rpc.on_message(0, PEER_LINK, &attach, &mut host, std::time::Instant::now())
        .await
        .expect("an attach is not refused");
    let takeover = Message::Takeover {
        selector: rig.session.clone(),
    };
    rpc.on_message(0, PEER_LINK, &takeover, &mut host, std::time::Instant::now())
        .await
        .expect("a takeover is not refused");
    assert_eq!(
        rpc.attached_to(0),
        Some(rig.session.as_str()),
        "the service attached the rig's shell"
    );
    let send = Message::Send {
        op_id: 1,
        chunk_idx: 0,
        chunk_count: 1,
        have_snapshot_id: 0,
        cols: 0,
        rows: 0,
        flags: 0,
        shell: rig.session.clone(),
        items: vec![SendItem::Keys(vec!["Up".to_owned()])],
    };
    rpc.on_message(0, PEER_LINK, &send, &mut host, std::time::Instant::now())
        .await
        .expect("a key is not refused");
    rpc
}

/// Wait until the **pane** shows [want], by asking the daemon what the keeper's emulator has.
///
/// The viewer's own stream is not the thing to read here. The service takes the shell over, and a
/// takeover tells the displaced viewer to stop, so its stream can end at any moment. The pane's
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
    let _rpc = press_up(&rig).await;
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
    let _rpc = press_up(&rig).await;
    pane_shows(&rig, "^[[A").await;
}

/// **An empty selector with one shell running attaches to it, and the answer names it.** The phone
/// learns which shell it got from the `Snapshot`, not by guessing from a list another client may
/// have changed in between.
#[tokio::test]
async fn an_empty_selector_attaches_to_the_only_shell_and_the_answer_names_it() {
    let rig = Rig::start().await;
    let mut rpc = MeshRpc::new();
    let mut host = DaemonHost(rig.arc.clone());
    let attach = Message::Attach {
        version: VERSION,
        selector: String::new(),
        cols: 0,
        rows: 0,
    };
    let outcome = rpc
        .on_message(0, PEER_LINK, &attach, &mut host, std::time::Instant::now())
        .await
        .expect("an attach is not refused");
    let ldmux_daemon::mesh_rpc::Outcome::Answer(messages) = outcome else {
        panic!("an attach is answered with a screen");
    };
    let named = messages.iter().find_map(|m| match m {
        Message::Snapshot { shell, .. } => Some(shell.clone()),
        _ => None,
    });
    assert_eq!(
        named.as_deref(),
        Some(rig.session.as_str()),
        "the answer did not name the shell the host chose: {messages:?}"
    );
}

/// **A grid stated with the attach reaches the REAL keeper**, so the pane's own size changes — the
/// thing a tailnet client's resize does — and the answer carries that grid.
#[tokio::test]
async fn a_stated_grid_changes_the_real_pane() {
    let rig = Rig::start().await;
    let mut rpc = MeshRpc::new();
    let mut host = DaemonHost(rig.arc.clone());
    let attach = Message::Attach {
        version: VERSION,
        selector: rig.session.clone(),
        cols: 41,
        rows: 17,
    };
    rpc.on_message(0, PEER_LINK, &attach, &mut host, std::time::Instant::now())
        .await
        .expect("an attach is not refused");
    let snap = rig
        .arc
        .lock()
        .await
        .handle("snapshot", serde_json::json!({ "id": rig.session }))
        .await
        .unwrap();
    assert_eq!(
        (snap["cols"].as_u64(), snap["rows"].as_u64()),
        (Some(41), Some(17)),
        "the pane was not fitted to the grid the phone stated"
    );
}
