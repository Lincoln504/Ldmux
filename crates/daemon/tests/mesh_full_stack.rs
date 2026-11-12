//! The B8 full-stack gate: the **real daemon loop** and the **real phone engine**, over a simulated
//! mesh.
//!
//! `docs/RADIO-PLAN.md` §9 `B8` asks for exactly this, and until now nothing ran it. The two halves
//! were each proved against a stand-in: `crates/session/tests/client.rs` runs the phone engine
//! against a host *endpoint* with a fake dispatcher, and `crates/daemon/src/mesh.rs` runs the daemon
//! *loop* against a fake host with no transport at all. Neither is the join, and `AR-15` is what
//! that costs: the phone moved to the new transport, the host did not, and every test still passed.
//!
//! So this file drives `ldmux_daemon::mesh::LinkLoop` — the loop `session()` runs, with a real
//! `DaemonHost` over a real `LdmuxDaemon` and the in-memory keeper — against
//! `ldmux_session::client::Client`, the engine the app holds, with a `VirtualMesh` between them.
//!
//! **Nothing here waits on a clock.** The mesh, the phone and the loop all read one virtual
//! millisecond counter, and every assertion is a condition reached by stepping, not a sleep that
//! happened to be long enough.

mod common;

use std::sync::Arc;
use std::time::{Duration, Instant};

use common::rig::Rig;
use ldmux_daemon::daemon::LdmuxDaemon;
use ldmux_daemon::mesh::{LinkLoop, MeshConfig, MeshSnapshot, SessionSetup};
use ldmux_daemon::mesh_bridge::DaemonHost;
use ldmux_daemon::mesh_rpc::{MeshRpc, Outcome, PEER_LINK};
use ldmux_mesh::carrier::{Bound, Budget};
use ldmux_mesh::constants::RadioState;
use ldmux_mesh::route::Carriage;
use ldmux_mesh::sim::{NodeId, VirtualMesh};
use ldmux_protocol::mesh_term::{Message, SendItem, VERSION, closed};
use ldmux_radio_harness::{
    AIRTIME_MS, FakeRadio, SharedPort, WALL_BASE_MS, chain, info_of_width,
};
use ldmux_session::PageMirror;
use ldmux_session::client::{Client, ClientConfig, ClientEvent};
use ldmux_session::handshake::Keypair;
use ldmux_testkeeper::FakeKeeper;

/// Bytes per path hash. Two, because a one-byte path is the easy case and the gate should not be.
const PATH_WIDTH: u8 = 2;

/// The grid the phone draws with.
const COLS: u8 = 46;
const ROWS: u8 = 20;

/// A scratch directory that removes itself, so a failing run does not leave one behind.
struct Scratch(std::path::PathBuf);

impl Scratch {
    fn dir(prefix: &str) -> Self {
        let p = std::env::temp_dir().join(format!(
            "{prefix}-{}-{}",
            std::process::id(),
            rand::random::<u64>()
        ));
        std::fs::create_dir_all(&p).expect("a scratch directory");
        Self(p)
    }
}

impl std::ops::Deref for Scratch {
    type Target = std::path::Path;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// One shell, as the two names it has: the daemon's id and the keeper's.
///
/// **They are not the same string.** The daemon keys its registry by its own id and tells the keeper
/// a name; a test that peeked the daemon's id at the keeper would be told "no such session", which
/// is what the first run of this gate said.
struct Shell {
    /// The daemon's id: what a version-15 message names.
    id: String,
    /// The keeper's name: what `Peek` and `set_snapshot` take.
    keeper: String,
}

/// Make a shell the way the daemon makes one.
async fn create_shell(arc: &Arc<tokio::sync::Mutex<LdmuxDaemon>>) -> Shell {
    let created = arc
        .lock()
        .await
        .handle("new", serde_json::json!({ "kind": "shell", "cwd": "/tmp" }))
        .await
        .expect("the daemon makes a shell");
    let record = &created["session"];
    Shell {
        id: record["id"]
            .as_str()
            .expect("a created shell has an id")
            .to_owned(),
        keeper: record["keeperName"]
            .as_str()
            .expect("a created shell has a keeper name")
            .to_owned(),
    }
}

/// A daemon over the in-memory keeper, with one shell already running.
async fn daemon_with_shell(
    dir: &std::path::Path,
) -> (Arc<tokio::sync::Mutex<LdmuxDaemon>>, Arc<FakeKeeper>, Shell) {
    let keeper_sock = dir.join("keeper.sock");
    let fk = Arc::new(FakeKeeper::new(keeper_sock.clone()));
    let serve = Arc::clone(&fk);
    let sock = keeper_sock.clone();
    tokio::spawn(async move {
        let _ = FakeKeeper::serve(&serve, sock.as_path()).await;
    });
    for _ in 0..200 {
        if keeper_sock.exists() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    let d = LdmuxDaemon::new(dir.to_path_buf(), std::path::PathBuf::from("/nonexistent"));
    let arc = Arc::new(tokio::sync::Mutex::new(d));
    arc.lock().await.start(&arc).await.expect("the daemon starts");
    let shell = create_shell(&arc).await;
    (arc, fk, shell)
}

/// The whole stack: one mesh, one phone engine, one daemon loop.
struct Stack {
    mesh: VirtualMesh,
    phone_node: NodeId,
    host_node: NodeId,
    client: Client,
    radio: FakeRadio,
    link: LinkLoop<SharedPort, DaemonHost>,
    port: SharedPort,
    /// The virtual clock, in milliseconds. The mesh, the phone and the loop all read it.
    now: u64,
    /// The `Instant` the loop's settle engine measures against, so a settle window is a fact this
    /// test controls rather than a wall-clock wait.
    base: Instant,
    /// How far into `mesh.deliveries()` the harness has dispatched.
    cursor: usize,
    /// Every event the phone produced, in order.
    events: Vec<ClientEvent>,
}

impl Stack {
    fn new(seed: u64, repeaters: usize, loss: f64, setup: &SessionSetup, host: DaemonHost) -> Self {
        let phone_key = Keypair::generate().expect("a keypair");
        let phone_public = phone_key.public;
        let (mesh, phone_node, host_node) = chain(seed, repeaters, loss, PATH_WIDTH);
        let keys = setup.pairing_keys();
        let config = ClientConfig::new(keys.host_key, keys.psk)
            .with_airtime_ms(AIRTIME_MS)
            .with_path_width(PATH_WIDTH)
            .with_flood_reach_hops(u8::try_from(repeaters.max(1)).unwrap_or(1) + 2);
        let client = Client::new(config, phone_key);

        let cfg = MeshConfig {
            serial: "/dev/null".to_owned(),
            airtime_record: None,
            peer_record: None,
        };
        let snapshot = Arc::new(std::sync::Mutex::new(MeshSnapshot::new(&cfg)));
        let port = SharedPort::new();
        let promise = Bound {
            uplink_record_max: 172,
            radio_state: RadioState::Connected,
            budget: Budget::RadioGoverned,
            airtime_ms: Some(AIRTIME_MS),
        };
        let preset = ldmux_daemon::mesh_serial::RadioPreset {
            freq_mhz: 910.525,
            bw_khz: 62.5,
            sf: 7,
            cr: 5,
            tx_power_dbm: 22,
            hash_size: PATH_WIDTH,
            public_key: [0u8; 32],
        };
        let link = LinkLoop::new(
            SharedPort {
                carriage: Carriage::Flood,
                out: Arc::clone(&port.out),
            },
            setup,
            promise,
            &preset,
            host,
            snapshot,
        );
        // The phone's static key is what the host pins; the pairing starts unpinned, so the first
        // handshake is what pins it. That is the shipped behaviour, not a test shortcut.
        let _ = phone_public;
        Self {
            mesh,
            phone_node,
            host_node,
            client,
            radio: FakeRadio::new(),
            link,
            port,
            now: 0,
            base: Instant::now(),
            cursor: 0,
            events: Vec::new(),
        }
    }

    fn wall(&self) -> u64 {
        WALL_BASE_MS + self.now
    }

    fn instant(&self) -> Instant {
        self.base + Duration::from_millis(self.now)
    }

    /// One pass of the whole stack: the phone, the loop, the air, and what arrived.
    async fn step(&mut self, ms: u64) {
        let now = self.now;
        let instant = self.instant();

        self.client
            .poll(now)
            .await
            .expect("the phone's carrier never fails");
        self.link
            .step(now, instant)
            .await
            .expect("the loop's carrier never fails");
        self.events.extend(self.client.take_events());
        self.pump();
        self.mesh.advance(ms);
        self.now = self.now.saturating_add(ms);
        self.dispatch();
    }

    /// Move every record the two ends produced onto the air.
    fn pump(&mut self) {
        // **The phone's engine owes frames, not records.** Each is a companion command; the fake
        // radio answers it, and a send's answer carries the packet the harness puts on the air.
        let now = self.now;
        let wall = self.wall();
        let mut produced = 0;
        while let Some(frame) = self.client.take_frame() {
            for answer in self.radio.answer(&frame) {
                self.client.on_frame(&answer, now, wall);
            }
            produced += 1;
            assert!(
                produced < 256,
                "the engine never stopped producing frames; the window is not advancing"
            );
        }
        let out: Vec<(Carriage, Vec<u8>)> = self.radio.transmissions.drain(..).collect();
        for (carriage, bytes) in out {
            self.transmit(self.phone_node, &carriage, &bytes);
        }
        let carriage = self.port.carriage.clone();
        for bytes in self.port.drain() {
            self.transmit(self.host_node, &carriage, &bytes);
        }
    }

    fn transmit(&mut self, from: NodeId, carriage: &Carriage, bytes: &[u8]) {
        match carriage {
            Carriage::Direct { path } => {
                self.mesh.send_direct(from, path, bytes);
            }
            Carriage::Flood => {
                self.mesh.send_flood(from, bytes);
            }
        }
    }

    /// Hand every delivery the mesh has produced since the last pass to the end it reached.
    fn dispatch(&mut self) {
        let deliveries: Vec<_> = self.mesh.deliveries().to_vec();
        let batch = deliveries
            .get(self.cursor..)
            .map(<[_]>::to_vec)
            .unwrap_or_default();
        self.cursor = deliveries.len();
        let now = self.now;
        let wall = self.wall();
        for delivery in batch {
            let info = info_of_width(&delivery, PATH_WIDTH);
            let record = delivery.payload.clone();
            if delivery.to == self.phone_node {
                self.events
                    .extend(self.client.on_record(&record, &info, now, wall));
            } else if delivery.to == self.host_node {
                self.link.on_record(&record, &info, now, wall);
            }
        }
    }

    /// Step until `done`, or fail with what the stack was doing.
    async fn run_until(
        &mut self,
        limit_ms: u64,
        step_ms: u64,
        mut done: impl FnMut(&Stack) -> bool,
    ) {
        let mut spent = 0;
        while spent < limit_ms {
            if done(self) {
                return;
            }
            self.step(step_ms).await;
            spent += step_ms;
        }
    }

    /// Bring the link up: the phone starts an attempt and both ends finish the handshake.
    async fn connect(&mut self) {
        let now = self.now;
        let wall = self.wall();
        self.client
            .connect(now, wall)
            .expect("the phone can start an attempt");
        self.run_until(300_000, 250, |s| {
            s.client.established() && s.link.established()
        })
        .await;
        assert!(
            self.client.established(),
            "the phone never established: {:?}",
            self.client.link_stats()
        );
        assert!(
            self.link.established(),
            "the daemon's loop never established: {:?}",
            self.link.stats()
        );
    }

    /// Open a page and wait for its first screen.
    async fn open(&mut self, shell: &str) {
        let now = self.now;
        self.client
            .open(shell, COLS, ROWS, now)
            .expect("the page can be opened");
        self.client
            .fetch(shell, COLS, ROWS, now)
            .expect("the page can be fetched");
        self.run_until(300_000, 250, |s| {
            s.client.mirror(shell).is_some_and(PageMirror::painted)
        })
        .await;
        assert!(
            self.client.mirror(shell).is_some_and(PageMirror::painted),
            "the daemon never painted {shell}: {:?}",
            self.client.stats()
        );
    }

    /// How many settled answers have arrived for `shell`, so a second one is visible.
    fn settles(&self, shell: &str) -> usize {
        self.events
            .iter()
            .filter(|e| matches!(e, ClientEvent::Settled { shell: s, .. } if s == shell))
            .count()
    }

    /// Whether the host has told the phone that another device holds the shell it wrote to.
    fn was_held(&self) -> bool {
        self.events
            .iter()
            .any(|e| matches!(e, ClientEvent::Closed { reason } if *reason == closed::HELD))
    }

    /// The status of the last settled answer for `shell`, if one arrived.
    fn settled(&self, shell: &str) -> Option<u8> {
        self.events.iter().rev().find_map(|e| match e {
            ClientEvent::Settled {
                shell: s, applied, ..
            } if s == shell => applied.map(|a| a.status),
            _ => None,
        })
    }
}

/// **The join, proved.** A phone engine and the daemon's own loop complete a handshake over a
/// two-repeater mesh with loss, and the phone gets a screen for a shell the daemon is running.
///
/// This is the assertion `AR-15` says was missing: both halves were green while they spoke
/// different transports, and only a test that runs them against each other can tell.
#[tokio::test]
async fn a_phone_and_the_daemons_loop_handshake_and_paint_a_shell() {
    let dir = Scratch::dir("ldmux-b8-connect");
    let (arc, fk, shell) = daemon_with_shell(&dir).await;
    let setup = SessionSetup::new(&dir, [0x5au8; 32]).expect("a session setup");
    let host = DaemonHost(Arc::clone(&arc));
    fk.set_snapshot(&shell.keeper, "prompt$ ");

    let mut stack = Stack::new(7, 2, 0.15, &setup, host);
    stack.connect().await;
    stack.open(&shell.id).await;

    let mirror = stack.client.mirror(&shell.id).expect("the page painted");
    assert_eq!(
        (mirror.cols, mirror.rows),
        (COLS, ROWS),
        "the daemon painted a grid the phone did not ask for"
    );
    assert!(
        mirror.lines.iter().any(|l| l.contains("prompt$")),
        "the screen the daemon sent is not the pane's: {:?}",
        mirror.lines
    );
}

/// **A command runs once and its settled answer comes back.**
///
/// The at-most-once rule is the one a lossy link makes load-bearing: a retransmitted `Send` must not
/// type the command twice. The phone sends one `Send` carrying text and `Enter`; the daemon writes
/// it once and answers with a settled `Snapshot`.
#[tokio::test]
async fn a_command_reaches_the_shell_once_and_settles() {
    let dir = Scratch::dir("ldmux-b8-send");
    let (arc, fk, shell) = daemon_with_shell(&dir).await;
    let setup = SessionSetup::new(&dir, [0x5au8; 32]).expect("a session setup");
    let host = DaemonHost(Arc::clone(&arc));
    fk.set_snapshot(&shell.keeper, "prompt$ ");

    let mut stack = Stack::new(8, 1, 0.0, &setup, host);
    stack.connect().await;
    stack.open(&shell.id).await;

    let now = stack.now;
    stack
        .client
        .send(
            &shell.id,
            &[
                SendItem::Text(b"echo hi".to_vec()),
                SendItem::Keys(vec!["Enter".to_owned()]),
            ],
            COLS,
            ROWS,
            now,
        )
        .expect("the command is legal");

    stack
        .run_until(300_000, 250, |s| s.settled(&shell.id).is_some())
        .await;
    let status = stack
        .settled(&shell.id)
        .expect("the daemon answered the command");
    assert_eq!(
        status,
        ldmux_protocol::mesh_term::applied::WRITTEN,
        "the command was not written: {:?}",
        stack.client.stats()
    );

    // **Once.** The keeper's input buffer holds what the daemon wrote; a retransmission that typed
    // the command again would show it twice.
    let peek = fk
        .handle(&ldmux_protocol::keeper::KeeperMessage::Peek(
            ldmux_protocol::keeper::Peek {
                session_id: shell.keeper.clone(),
                max_bytes: None,
                wait_ms: None,
            },
        ))
        .expect("the keeper answers a peek");
    let ldmux_protocol::keeper::KeeperMessage::PeekResult(result) = peek else {
        panic!("a peek is answered with a peek result");
    };
    let written = String::from_utf8_lossy(&result.data).to_string();
    assert_eq!(
        written.matches("echo hi").count(),
        1,
        "the command was typed more than once: {written:?}"
    );
}

/// **Two shells are two pages, and each command reaches the shell it names.**
///
/// Gate `B11`. The old transport carried the page in a record header, so a command could only mean
/// "the shell this record came from"; a version-15 message names its shell. This is the same rule
/// the daemon's unit test pins, now proved through the real transport.
#[tokio::test]
async fn two_shells_are_two_pages_and_each_command_reaches_its_own() {
    let dir = Scratch::dir("ldmux-b8-two");
    let (arc, fk, first) = daemon_with_shell(&dir).await;
    let setup = SessionSetup::new(&dir, [0x5au8; 32]).expect("a session setup");
    let host = DaemonHost(Arc::clone(&arc));

    // A second shell, made the way the daemon makes one.
    let second = create_shell(&arc).await;
    fk.set_snapshot(&first.keeper, "first$ ");
    fk.set_snapshot(&second.keeper, "second$ ");

    let mut stack = Stack::new(9, 1, 0.0, &setup, host);
    stack.connect().await;
    stack.open(&first.id).await;
    stack.open(&second.id).await;

    for (shell, text) in [(&first, "one"), (&second, "two")] {
        let now = stack.now;
        stack
            .client
            .send(
                &shell.id,
                &[SendItem::Text(text.as_bytes().to_vec())],
                COLS,
                ROWS,
                now,
            )
            .expect("the command is legal");
        stack
            .run_until(300_000, 250, |s| s.settled(&shell.id).is_some())
            .await;
        assert!(
            stack.settled(&shell.id).is_some(),
            "the daemon never answered the command for {}",
            shell.id
        );
    }

    // Each shell's input buffer holds its own command and not the other's.
    for (shell, mine, theirs) in [(&first, "one", "two"), (&second, "two", "one")] {
        let peek = fk
            .handle(&ldmux_protocol::keeper::KeeperMessage::Peek(
                ldmux_protocol::keeper::Peek {
                    session_id: shell.keeper.clone(),
                    max_bytes: None,
                    wait_ms: None,
                },
            ))
            .expect("the keeper answers a peek");
        let ldmux_protocol::keeper::KeeperMessage::PeekResult(result) = peek else {
            panic!("a peek is answered with a peek result");
        };
        let written = String::from_utf8_lossy(&result.data).to_string();
        assert!(
            written.contains(mine),
            "{} did not receive its own command: {written:?}",
            shell.id
        );
        assert!(
            !written.contains(theirs),
            "{} received the other shell's command: {written:?}",
            shell.id
        );
    }
}

/// **A multiline paste is inserted, not executed.**
///
/// The report this exists for (2026-11-07): copying several lines on the phone and pasting them ran
/// the first line as a command. The daemon's half of the fix is to wrap a `Paste` in the pane's own
/// bracketed-paste markers when the pane asked for them — `mesh_paste.rs` proves bash then inserts
/// it, and this proves the daemon's encoder does its part through the real transport.
#[tokio::test]
async fn a_multiline_paste_is_wrapped_in_the_panes_paste_markers() {
    let dir = Scratch::dir("ldmux-b8-paste");
    let (arc, fk, shell) = daemon_with_shell(&dir).await;
    let setup = SessionSetup::new(&dir, [0x5au8; 32]).expect("a session setup");
    let host = DaemonHost(Arc::clone(&arc));
    // The pane asked for bracketed paste, which is what bash does at every prompt.
    fk.set_paste_mode(&shell.keeper, true);
    fk.set_snapshot(&shell.keeper, "prompt$ ");

    let mut stack = Stack::new(13, 1, 0.0, &setup, host);
    stack.connect().await;
    stack.open(&shell.id).await;

    let now = stack.now;
    stack
        .client
        .send(
            &shell.id,
            &[SendItem::Paste(b"echo ONE\necho TWO\n".to_vec())],
            COLS,
            ROWS,
            now,
        )
        .expect("the paste is legal");
    stack
        .run_until(300_000, 250, |s| s.settled(&shell.id).is_some())
        .await;
    assert!(
        stack.settled(&shell.id).is_some(),
        "the daemon never answered the paste: {:?}",
        stack.client.stats()
    );

    let peek = fk
        .handle(&ldmux_protocol::keeper::KeeperMessage::Peek(
            ldmux_protocol::keeper::Peek {
                session_id: shell.keeper.clone(),
                max_bytes: None,
                wait_ms: None,
            },
        ))
        .expect("the keeper answers a peek");
    let ldmux_protocol::keeper::KeeperMessage::PeekResult(result) = peek else {
        panic!("a peek is answered with a peek result");
    };
    let written = String::from_utf8_lossy(&result.data).to_string();
    assert!(
        written.contains("\u{1b}[200~") && written.contains("\u{1b}[201~"),
        "the paste was not wrapped in the pane's markers, so a shell would run its first line: {written:?}"
    );
    assert!(
        written.contains("echo ONE") && written.contains("echo TWO"),
        "the paste lost a line: {written:?}"
    );
}

/// **A `Fetch` answers without spending the draft.**
///
/// The refresh button is the one control an operator presses while a command is half-typed, so a
/// refresh that cleared the draft would be a control nobody could use.
#[tokio::test]
async fn a_fetch_answers_and_leaves_the_draft_alone() {
    let dir = Scratch::dir("ldmux-b8-fetch");
    let (arc, fk, shell) = daemon_with_shell(&dir).await;
    let setup = SessionSetup::new(&dir, [0x5au8; 32]).expect("a session setup");
    let host = DaemonHost(Arc::clone(&arc));
    fk.set_snapshot(&shell.keeper, "prompt$ ");

    let mut stack = Stack::new(10, 1, 0.0, &setup, host);
    stack.connect().await;
    stack.open(&shell.id).await;

    let before = stack.client.mirror(&shell.id).expect("painted").snapshot_id;
    let now = stack.now;
    stack
        .client
        .fetch(&shell.id, COLS, ROWS, now)
        .expect("a fetch is legal");
    stack
        .run_until(300_000, 250, |s| {
            s.client
                .mirror(&shell.id)
                .is_some_and(|m| m.snapshot_id != before)
        })
        .await;
    let after = stack.client.mirror(&shell.id).expect("painted").snapshot_id;
    assert_ne!(after, before, "the fetch was not answered with a new screen");
    // A fetch is not a command: nothing was written to the pane.
    let peek = fk
        .handle(&ldmux_protocol::keeper::KeeperMessage::Peek(
            ldmux_protocol::keeper::Peek {
                session_id: shell.keeper.clone(),
                max_bytes: None,
                wait_ms: None,
            },
        ))
        .expect("the keeper answers a peek");
    let ldmux_protocol::keeper::KeeperMessage::PeekResult(result) = peek else {
        panic!("a peek is answered with a peek result");
    };
    assert!(
        result.data.is_empty(),
        "a fetch typed into the pane: {:?}",
        String::from_utf8_lossy(&result.data)
    );
}

/// **A blackout does not cost the session.**
///
/// The retry ladder is the phone's, and a link that goes quiet for longer than one attempt must
/// recover without a new handshake: a re-handshake is a new ephemeral, a new pin check and a new
/// round trip, and on a mountain it is the difference between a screen that comes back and one that
/// does not.
#[tokio::test]
async fn a_radio_blackout_does_not_require_a_new_handshake() {
    let dir = Scratch::dir("ldmux-b8-blackout");
    let (arc, fk, shell) = daemon_with_shell(&dir).await;
    let setup = SessionSetup::new(&dir, [0x5au8; 32]).expect("a session setup");
    let host = DaemonHost(Arc::clone(&arc));
    fk.set_snapshot(&shell.keeper, "prompt$ ");

    let mut stack = Stack::new(11, 1, 0.0, &setup, host);
    stack.connect().await;
    stack.open(&shell.id).await;
    let handshakes = stack.client.link_stats().sessions_established;

    // **The air goes dead.** Every delivery is dropped for longer than one retry attempt, which is
    // what a ridge between two radios does.
    let blackout_until = stack.now + 20_000;
    while stack.now < blackout_until {
        let now = stack.now;
        let instant = stack.instant();
        stack
            .client
            .poll(now)
            .await
            .expect("the phone's carrier never fails");
        stack
            .link
            .step(now, instant)
            .await
            .expect("the loop's carrier never fails");
        stack.events.extend(stack.client.take_events());
        // The frames still move — the radio still transmits — but nothing reaches the other end.
        stack.pump();
        stack.mesh.advance(250);
        stack.now = stack.now.saturating_add(250);
        // Deliberately no `dispatch`: the air carried nothing.
        stack.cursor = stack.mesh.deliveries().len();
    }

    // The air comes back, and the next command works.
    let now = stack.now;
    stack
        .client
        .send(
            &shell.id,
            &[SendItem::Text(b"after".to_vec())],
            COLS,
            ROWS,
            now,
        )
        .expect("the command is legal");
    stack
        .run_until(300_000, 250, |s| s.settled(&shell.id).is_some())
        .await;
    assert!(
        stack.settled(&shell.id).is_some(),
        "the link did not recover after a blackout: {:?}",
        stack.client.stats()
    );
    assert_eq!(
        stack.client.link_stats().sessions_established,
        handshakes,
        "the blackout cost a handshake, which is the round trip a mountain cannot afford"
    );
}

/// **A restarted host is recovered by `KIND_NEED_HANDSHAKE`, not by a new pairing.**
///
/// The daemon restarting loses its sessions, not its key. The phone still holds its session and
/// sends a record the new host cannot read; the host answers `KIND_NEED_HANDSHAKE`, and the phone
/// re-handshakes against the same pinned key. A pairing that had to be redone would mean a QR code
/// on a mountain.
#[tokio::test]
async fn a_restarted_host_is_recovered_without_a_new_pairing() {
    let dir = Scratch::dir("ldmux-b8-restart");
    let (arc, fk, shell) = daemon_with_shell(&dir).await;
    let setup = SessionSetup::new(&dir, [0x5au8; 32]).expect("a session setup");
    let host = DaemonHost(Arc::clone(&arc));
    fk.set_snapshot(&shell.keeper, "prompt$ ");

    let mut stack = Stack::new(12, 1, 0.0, &setup, host);
    stack.connect().await;
    stack.open(&shell.id).await;
    let pinned = setup.pairing_keys().host_key;

    // **The host restarts.** A new loop, the same keypair and the same pairing: exactly what a
    // daemon restart leaves behind.
    let host = DaemonHost(Arc::clone(&arc));
    let restarted = LinkLoop::new(
        SharedPort {
            carriage: Carriage::Flood,
            out: Arc::clone(&stack.port.out),
        },
        &setup,
        Bound {
            uplink_record_max: 172,
            radio_state: RadioState::Connected,
            budget: Budget::RadioGoverned,
            airtime_ms: Some(AIRTIME_MS),
        },
        &ldmux_daemon::mesh_serial::RadioPreset {
            freq_mhz: 910.525,
            bw_khz: 62.5,
            sf: 7,
            cr: 5,
            tx_power_dbm: 22,
            hash_size: PATH_WIDTH,
            public_key: [0u8; 32],
        },
        host,
        Arc::new(std::sync::Mutex::new(MeshSnapshot::new(&MeshConfig {
            serial: "/dev/null".to_owned(),
            airtime_record: None,
            peer_record: None,
        }))),
    );
    stack.link = restarted;

    // **The phone asks for its screen, which is what the app does when a page goes stale.** The
    // new host has no session, so it answers `KIND_NEED_HANDSHAKE`; the phone re-handshakes against
    // the key it already pinned, and the fetch is answered by the new session.
    let now = stack.now;
    stack
        .client
        .fetch(&shell.id, COLS, ROWS, now)
        .expect("a fetch is legal");
    stack
        .run_until(300_000, 250, |s| s.client.established() && s.link.established())
        .await;
    assert!(
        stack.client.established() && stack.link.established(),
        "the phone did not re-handshake after a host restart: {:?}",
        stack.client.stats()
    );

    // **The page re-attaches, which is what the app does when a session comes back.** The new host
    // has no attachment for this shell, so a command sent into it would be refused with `NO_SHELL`
    // — correctly, because the host has no idea which pane the phone means. The page is closed and
    // opened again because a page that is already open locally cannot be opened twice.
    stack.client.close(&shell.id).expect("the page can be closed");
    stack.open(&shell.id).await;

    // And the recovered session carries a command.
    let now = stack.now;
    stack
        .client
        .send(
            &shell.id,
            &[SendItem::Text(b"again".to_vec())],
            COLS,
            ROWS,
            now,
        )
        .expect("the command is legal");
    stack
        .run_until(300_000, 250, |s| s.settled(&shell.id).is_some())
        .await;
    assert!(
        stack.settled(&shell.id).is_some(),
        "the recovered session did not carry a command: {:?}",
        stack.client.stats()
    );
    assert_eq!(
        setup.pairing_keys().host_key,
        pinned,
        "the host's key changed, which is a new pairing and not a restart"
    );
    // The phone never had to be told a new key: its config still names the one it pinned.
    assert!(
        stack.client.established(),
        "the phone is not established after the restart"
    );
}

// ---------------------------------------------------------------------------------------------
// The two behaviours `docs/RELEASE-AUDIT.md` Appendix C listed as having no test.
// ---------------------------------------------------------------------------------------------

/// **A desk window displaced by a mesh claim is told, and the phone's next command is refused.**
///
/// `RELEASE-AUDIT.md` Appendix C: "the daemon sends the notice and the same message is proven for a
/// WebSocket claim, but the mesh variant has no test." A mesh claim does not pass through
/// `remote.rs`'s attach handler, so the notice comes from `displace_viewers` on the claim tick —
/// a path that had no test at all.
///
/// The second half is the rule `RADIO-PLAN.md` §2 states for a claim: it is "immediately
/// displaceable by local desktop typing or `ldmux attach`, answering `Closed { reason: HELD }` on
/// the phone's next `Send`/`Fetch`". So the desk takes the shell back, the phone's command must be
/// refused **without the pty being written**, and the phone's takeover must still work afterwards —
/// the way back is never closed.
#[tokio::test]
async fn a_desk_window_displaced_by_a_mesh_claim_is_told_and_the_phones_write_is_refused() {
    let rig = Rig::start().await;
    let shell = rig.session.clone();
    let setup = SessionSetup::new(&rig.dir, [0x5au8; 32]).expect("a session setup");

    // The desk window holds the shell, as a desktop: the kind is what the notice names, and what
    // tells the phone's own claim apart from somebody else's.
    let mut desk = rig.connect(None).await;
    desk.send_json(&serde_json::json!({
        "type": "hello",
        "device": { "id": "desk-1", "name": "Desk", "kind": "desktop" },
    }))
    .await;
    desk.send_json(&serde_json::json!({
        "type": "attach", "sessionId": shell, "cols": 80, "rows": 24,
    }))
    .await;
    let _ = desk.until_type("snapshot", &mut Vec::new()).await;

    let host = DaemonHost(Arc::clone(&rig.arc));
    let mut stack = Stack::new(21, 1, 0.0, &setup, host);
    stack.connect().await;

    // **The phone takes it over the radio** — the "take it back" press, from the phone's side.
    let now = stack.now;
    stack
        .client
        .takeover(&shell, now)
        .expect("a takeover is legal");
    stack
        .run_until(300_000, 250, |s| {
            s.client.mirror(&shell).is_some_and(PageMirror::painted)
        })
        .await;
    assert!(
        stack.client.mirror(&shell).is_some_and(PageMirror::painted),
        "the phone's takeover never painted: {:?}",
        stack.client.stats()
    );

    // **The desk is told it lost the shell, and to whom.** The claim was made over the radio, so
    // this notice comes from the claim sweep; waiting for it is waiting on the message, not on a
    // clock.
    let owned = desk.until_type("owned", &mut Vec::new()).await;
    assert_eq!(owned["sessionId"], shell.as_str());
    assert_eq!(
        owned["owner"]["kind"], "mesh",
        "the desk was told it lost the shell without being told who has it: {owned}"
    );

    // The desk takes it back — the "press r" path, an attach with a takeover.
    desk.send_json(&serde_json::json!({
        "type": "attach", "sessionId": shell, "cols": 80, "rows": 24, "takeover": true,
    }))
    .await;
    let _ = desk.until_type("snapshot", &mut Vec::new()).await;

    // The reclaim is what makes the refusal meaningful, so the phone must not be holding it yet:
    // a `HELD` that was already in the log would prove nothing about this command.
    assert!(
        !stack.was_held(),
        "the phone had already been refused before the desk took the shell back"
    );

    // **The phone's next command is refused, and nothing is typed.**
    let now = stack.now;
    stack
        .client
        .send(
            &shell,
            &[
                SendItem::Text(b"echo hi".to_vec()),
                SendItem::Keys(vec!["Enter".to_owned()]),
            ],
            COLS,
            ROWS,
            now,
        )
        .expect("the command is legal to ask");
    stack
        .run_until(300_000, 250, Stack::was_held)
        .await;
    assert!(
        stack.was_held(),
        "the phone's command was not refused while the desk held the shell: {:?}",
        stack.client.stats()
    );
    let pane = rig
        .arc
        .lock()
        .await
        .handle("snapshot", serde_json::json!({ "id": shell }))
        .await
        .expect("the pane reads");
    let text = pane["text"].as_str().unwrap_or_default();
    assert!(
        !text.contains("echo hi"),
        "a refused command was typed into the pty anyway: {text:?}"
    );
    // And it did not run either: a command that reached the shell would print its own line.
    assert!(
        !text.lines().any(|l| l.trim() == "hi"),
        "a refused command ran in the pty: {text:?}"
    );

    // **And the way back is open.** The refusal detached the phone; a takeover attaches it again.
    let before = stack.settles(&shell);
    let now = stack.now;
    stack
        .client
        .takeover(&shell, now)
        .expect("a takeover is legal");
    stack
        .run_until(300_000, 250, |s| s.settles(&shell) > before)
        .await;
    assert!(
        stack.settles(&shell) > before,
        "the phone could not take the shell back after being displaced: {:?}",
        stack.client.stats()
    );
}

/// **A shell created from a phone comes back with its phone origin, after an app restart.**
///
/// `RELEASE-AUDIT.md` Appendix C: "A shell created from a phone is recorded with `createdBy` set to
/// `phone`, and rows keep their actions across an app restart because of it. There is no test that
/// asserts that round trip."
///
/// The daemon writes `createdBy: "phone"` (`mesh_bridge.rs`, `DaemonHost::create`) and the launcher
/// keys a row's actions off the origin `List` reports, so the round trip is the thing to pin: ask
/// for a shell, then ask again **through a service with no state**, which is exactly what an app
/// restart leaves behind. A `MeshRpc` that remembered nothing is the honest stand-in for a
/// restarted app, because the record is the daemon's and nothing about it is the phone's to keep.
#[tokio::test]
async fn a_shell_created_from_a_phone_keeps_its_phone_origin_across_an_restart() {
    let dir = Scratch::dir("ldmux-b8-origin");
    let (arc, _fk, local) = daemon_with_shell(&dir).await;
    let mut host = DaemonHost(Arc::clone(&arc));

    let mut rpc = MeshRpc::new();
    let create = Message::Create {
        version: VERSION,
        cwd: "/tmp".to_owned(),
    };
    let Outcome::Answer(reply) = rpc
        .on_message(0, PEER_LINK, &create, &mut host, Instant::now())
        .await
        .expect("a create is not refused")
    else {
        panic!("a create is answered");
    };
    let id = reply
        .iter()
        .find_map(|m| match m {
            Message::Created { id } => Some(id.clone()),
            _ => None,
        })
        .expect("the host named the shell it made");

    // **The app restarts.** No attachment, no ring, no page — only the daemon's record survives.
    let mut restarted = MeshRpc::new();
    let Outcome::Answer(listing) = restarted
        .on_message(0, PEER_LINK, &Message::List, &mut host, Instant::now())
        .await
        .expect("a list is not refused")
    else {
        panic!("a list is answered");
    };
    let mut rows = Vec::new();
    for message in listing {
        if let Message::Sessions { entries, .. } = message {
            rows.extend(entries);
        }
    }
    let row = |id: &str| {
        rows.iter()
            .find(|r| r.id == id)
            .unwrap_or_else(|| panic!("{id} is not in the list: {rows:?}"))
            .origin
            .clone()
    };
    assert_eq!(
        row(&id),
        "phone",
        "the phone's own shell came back as somebody else's, so its actions would be drawn for the \
         wrong route: {rows:?}"
    );
    // **The field discriminates**, or the assertion above would pass for every shell on the host:
    // the one the rig started through the daemon's own handle has a different origin.
    assert_ne!(
        row(&local.id),
        "phone",
        "every shell reports the phone's origin, so the assertion above proves nothing: {rows:?}"
    );
}
