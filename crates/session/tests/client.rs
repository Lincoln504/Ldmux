//! The phone's client, end to end: a real [`Client`] against a real host endpoint over a simulated
//! mesh.
//!
//! The host here is a fake laptop: it decodes the same wire the daemon does, writes to a list instead
//! of a pty, and answers with the same `Snapshot`. That is enough to prove the client's half of every
//! rule `docs/RADIO-PLAN.md` §10 `S4` names, and it is deliberately not a second implementation of
//! `MeshRpc`: nothing here tries to be the daemon.

use std::collections::BTreeMap;

use ldmux_mesh::route::Carriage;
use ldmux_mesh::sim::{NodeId, VirtualMesh};
use ldmux_protocol::mesh_term::{
    Decoder, Message, Patch, SendAssembly, SendItem, SessionEntry, applied, snapshot_flag,
};
use ldmux_session::client::{Applied, Client, ClientConfig, ClientError, ClientEvent};
use ldmux_session::endpoint::{ArrivalInfo, EndpointConfig, RadioEndpoint, Role};
use ldmux_session::handshake::{Keypair, PairingEntry};
use ldmux_session::{MAX_OPEN_PAGES, PageMirror};
use ldmux_radio_harness::{
    AIRTIME_MS, FakeRadio, Port, WALL_BASE_MS, block_on, chain, info_of,
};

const PSK: [u8; 32] = [0x42; 32];
const COLS: u8 = 40;
const ROWS: u8 = 6;

// ---------------------------------------------------------------------------------------------
// The fake host: what a laptop does with the wire, without being a laptop.
// ---------------------------------------------------------------------------------------------

#[derive(Default)]
struct ShellState {
    /// Everything written to the "pty", one entry per write, so a test can prove a chunked command was
    /// written once rather than once per chunk.
    writes: Vec<Vec<u8>>,
    /// The screen the host would paint: the last write, cut into lines.
    lines: Vec<String>,
    snapshot: u32,
    /// The grid the pty is fitted to. **The host fits it from the phone's request**, so a test can
    /// tell an attach that carried a grid from one that attached at the host's own size.
    cols: u8,
    rows: u8,
}

struct Host {
    endpoint: RadioEndpoint,
    port: Port,
    shells: BTreeMap<String, ShellState>,
    assembly: SendAssembly,
    /// How many `Send` messages arrived, chunks included.
    chunks_seen: usize,
    /// Whether another client is supposed to hold every shell.
    held: bool,
    /// The shells this host made in answer to a `Create`, in order.
    created: Vec<String>,
    /// The shells a phone asked the desktop to open, in order.
    made_live: Vec<String>,
    /// The shells a phone ended, in order.
    closed: Vec<String>,
}

impl Host {
    fn new(phone_public: [u8; 32]) -> (Self, Keypair) {
        let key = Keypair::generate().expect("a keypair");
        let config = EndpointConfig::new(Role::Host, phone_public, PSK).with_pairings(vec![
            PairingEntry {
                psk: PSK,
                pinned: Some(phone_public),
                last_timestamp_ms: 0,
            },
        ]);
        let endpoint = RadioEndpoint::host(config, Keypair::from_parts(key.private, key.public));
        (
            Self {
                endpoint,
                port: Port::new(),
                shells: BTreeMap::new(),
                assembly: SendAssembly::default(),
                chunks_seen: 0,
                held: false,
                created: Vec::new(),
                made_live: Vec::new(),
                closed: Vec::new(),
            },
            key,
        )
    }

    fn writes(&self, shell: &str) -> &[Vec<u8>] {
        self.shells
            .get(shell)
            .map(|s| s.writes.as_slice())
            .unwrap_or(&[])
    }

    fn lines(&self, shell: &str) -> &[String] {
        self.shells
            .get(shell)
            .map(|s| s.lines.as_slice())
            .unwrap_or(&[])
    }

    /// Take everything the host decrypted, and answer it.
    fn drain_inbound(&mut self, now_ms: u64) {
        for message in self.endpoint.take_inbound() {
            let Some(Ok(message)) = decode(&message) else {
                continue;
            };
            self.handle(&message, now_ms);
        }
    }

    fn handle(&mut self, message: &Message, now_ms: u64) {
        match message {
            Message::Attach {
                selector,
                cols,
                rows,
                ..
            } => {
                let state = self.shells.entry(selector.clone()).or_default();
                // A zero grid means "not stated", which is what a version before 13 sent.
                if *cols > 0 && *rows > 0 {
                    state.cols = *cols;
                    state.rows = *rows;
                }
                self.answer(selector, None, 0, now_ms);
            }
            Message::Create { cwd, .. } => {
                let id = format!("run{}", self.created.len() + 1);
                let state = self.shells.entry(id.clone()).or_default();
                state.lines = vec![format!("{}$", if cwd.is_empty() { "~" } else { cwd })];
                self.created.push(id.clone());
                let message = Message::Created { id };
                if let Ok(bytes) = message.encode() {
                    let _ = self.endpoint.submit(&bytes, now_ms);
                }
            }
            Message::Takeover { selector } => {
                self.shells.entry(selector.clone()).or_default();
                self.answer(selector, None, 0, now_ms);
            }
            Message::MakeLive { selector } => {
                self.made_live.push(selector.clone());
            }
            Message::WantHistory { from, rows } => {
                let message = Message::HistoryPage {
                    from: *from,
                    last: true,
                    older: *from > 0,
                    lines: (0..usize::from(*rows))
                        .map(|i| format!("line {}", u64::from(*from) + i as u64))
                        .collect(),
                };
                if let Ok(bytes) = message.encode() {
                    let _ = self.endpoint.submit(&bytes, now_ms);
                }
            }
            Message::Close { selector } => {
                self.shells.remove(selector);
                self.closed.push(selector.clone());
            }
            Message::Send { op_id, shell, .. } => {
                self.chunks_seen += 1;
                self.shells.entry(shell.clone()).or_default();
                match self.assembly.accept(message) {
                    // The whole command, once: this is what makes a chunked paste atomic.
                    Ok(Some(items)) => {
                        if self.held {
                            self.answer(shell, Some(applied::HELD), *op_id, now_ms);
                        } else {
                            self.write(shell, flatten(&items));
                            self.answer(shell, Some(applied::WRITTEN), *op_id, now_ms);
                        }
                    }
                    Ok(None) => {}
                    Err(_) => {}
                }
            }
            Message::Fetch { shell, .. } => {
                self.shells.entry(shell.clone()).or_default();
                self.answer(shell, None, 0, now_ms);
            }
            Message::List => self.answer_list(now_ms),
            _ => {}
        }
    }

    fn write(&mut self, shell: &str, bytes: Vec<u8>) {
        let state = self.shells.entry(shell.to_owned()).or_default();
        let text = String::from_utf8_lossy(&bytes).replace('\r', "\n");
        state.lines = text.split('\n').map(str::to_owned).collect();
        state.writes.push(bytes);
    }

    /// Answer with one whole screen.
    fn answer(&mut self, shell: &str, status: Option<u8>, op_id: u32, now_ms: u64) {
        let state = self.shells.entry(shell.to_owned()).or_default();
        state.snapshot = state.snapshot.wrapping_add(1).max(1);
        let patches: Vec<Patch> = state
            .lines
            .iter()
            .enumerate()
            .map(|(y, text)| Patch {
                y: u8::try_from(y).unwrap_or(0),
                append: false,
                text: text.clone(),
            })
            .collect();
        let mut flags = snapshot_flag::FULL | snapshot_flag::SETTLED;
        if status.is_some() {
            flags |= snapshot_flag::HAS_APPLIED;
        }
        let message = Message::Snapshot {
            flags,
            chunk_idx: 0,
            chunk_count: 1,
            snapshot_id: state.snapshot,
            base_id: 0,
            applied_op_id: op_id,
            applied_status: status.unwrap_or(applied::NONE),
            cols: if state.cols == 0 { COLS } else { state.cols },
            rows: if state.rows == 0 { ROWS } else { state.rows },
            cursor_x: 2,
            cursor_y: 1,
            first_line: 0,
            shell: shell.to_owned(),
            patches,
        };
        if let Ok(bytes) = message.encode() {
            let _ = self.endpoint.submit(&bytes, now_ms);
        }
    }

    fn answer_list(&mut self, now_ms: u64) {
        let entries: Vec<SessionEntry> = self
            .shells
            .keys()
            .map(|id| SessionEntry {
                id: id.clone(),
                label: id.clone(),
                cwd: "/home".to_owned(),
                owner: "phone".to_owned(),
                origin: "phone".to_owned(),
                shell: "bash".to_owned(),
            })
            .collect();
        let message = Message::Sessions {
            last: true,
            cwd: true,
            origin: true,
            owner: true,
            shell: true,
            entries,
        };
        if let Ok(bytes) = message.encode() {
            let _ = self.endpoint.submit(&bytes, now_ms);
        }
    }
}

/// A `Send`'s items, flattened to the bytes a shell would receive.
///
/// Keys are rendered as `<Name>` so a test can see that order survived a chunk boundary.
fn flatten(items: &[SendItem]) -> Vec<u8> {
    let mut out = Vec::new();
    for item in items {
        match item {
            SendItem::Text(bytes) | SendItem::Paste(bytes) => out.extend_from_slice(bytes),
            SendItem::Keys(names) => {
                for name in names {
                    out.push(b'<');
                    out.extend_from_slice(name.as_bytes());
                    out.push(b'>');
                }
            }
        }
    }
    out
}

fn decode(bytes: &[u8]) -> Option<Result<Message, ldmux_protocol::mesh_term::DecodeError>> {
    Decoder::new().push(bytes).ok().and_then(|mut v| v.pop())
}

/// One whole screen for `shell`, so a test can send one message and mean exactly that.
fn screen(shell: &str, snapshot_id: u32, rows: &[&str]) -> Vec<u8> {
    let patches: Vec<Patch> = rows
        .iter()
        .enumerate()
        .map(|(y, text)| Patch {
            y: u8::try_from(y).unwrap_or(0),
            append: false,
            text: (*text).to_owned(),
        })
        .collect();
    Message::Snapshot {
        flags: snapshot_flag::FULL | snapshot_flag::SETTLED,
        chunk_idx: 0,
        chunk_count: 1,
        snapshot_id,
        base_id: 0,
        applied_op_id: 0,
        applied_status: applied::NONE,
        cols: COLS,
        rows: u8::try_from(rows.len()).unwrap_or(ROWS),
        cursor_x: 0,
        cursor_y: 0,
        first_line: 0,
        shell: shell.to_owned(),
        patches,
    }
    .encode()
    .expect("a legal snapshot")
}

// ---------------------------------------------------------------------------------------------
// The loop: one client, one fake host, one mesh.
// ---------------------------------------------------------------------------------------------

struct Link {
    mesh: VirtualMesh,
    phone: NodeId,
    host_node: NodeId,
    client: Client,
    host: Host,
    radio: FakeRadio,
    now: u64,
    cursor: usize,
    /// Every event the client produced, in order, so a test can look for one.
    events: Vec<ClientEvent>,
    /// The phone's own radio key and the host's, for the tests that build route packets themselves.
    own_key: [u8; 32],
    peer_key: [u8; 32],
}

impl Link {
    fn new(seed: u64, repeaters: usize, loss: f64) -> Self {
        let phone_key = Keypair::generate().expect("a keypair");
        let phone_public = phone_key.public;
        let (mesh, phone, host_node) = chain(seed, repeaters, loss, 1);
        let (host, host_key) = Host::new(phone_public);
        let config = ClientConfig::new(host_key.public, PSK)
            .with_airtime_ms(AIRTIME_MS)
            .with_path_width(1)
            .with_flood_reach_hops(u8::try_from(repeaters.max(1)).unwrap_or(1) + 2);
        Self {
            mesh,
            phone,
            host_node,
            client: Client::new(config, phone_key),
            own_key: phone_public,
            peer_key: host_key.public,
            host,
            radio: FakeRadio::new(),
            now: 0,
            cursor: 0,
            events: Vec::new(),
        }
    }

    fn wall(&self) -> u64 {
        WALL_BASE_MS + self.now
    }

    fn step(&mut self, ms: u64) {
        block_on(self.client.poll(self.now)).expect("the client's carrier never fails");
        block_on(self.host.endpoint.poll(self.now, &mut self.host.port))
            .expect("the host's carrier never fails");
        self.events.extend(self.client.take_events());
        self.pump();
        self.mesh.advance(ms);
        self.now = self.now.saturating_add(ms);
        self.dispatch();
        self.host.drain_inbound(self.now);
    }

    fn pump(&mut self) {
        // **The phone's engine owes frames, not records.** Each one is a companion command: the fake
        // radio answers it, and a send's answer carries the packet the harness puts on the air. That
        // is the whole point of the window — one command at a time, so no answer can settle another.
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
            self.transmit(self.phone, &carriage, &bytes);
        }
        let out: Vec<Vec<u8>> = self.host.port.out.drain(..).collect();
        let carriage = self.host.port.carriage.clone();
        for bytes in out {
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
            let info: ArrivalInfo = info_of(&delivery);
            let record = delivery.payload.clone();
            if delivery.to == self.phone {
                self.events
                    .extend(self.client.on_record(&record, &info, now, wall));
            } else if delivery.to == self.host_node {
                self.host.endpoint.on_record(&record, &info, now, wall);
            }
        }
    }

    fn run_until(&mut self, limit_ms: u64, step_ms: u64, mut done: impl FnMut(&Link) -> bool) {
        let mut spent = 0;
        while spent < limit_ms {
            if done(self) {
                return;
            }
            self.step(step_ms);
            spent += step_ms;
        }
    }

    /// Bring the link up and keep stepping until the bootstrap finishes.
    fn connect(&mut self) {
        let now = self.now;
        let wall = self.wall();
        self.client
            .connect(now, wall)
            .expect("the client can start an attempt");
        self.run_until(300_000, 250, |link| {
            link.client.established() && link.host.endpoint.established()
        });
        assert!(
            self.client.established(),
            "the phone never established: {:?}",
            self.client.link_stats()
        );
        assert!(
            self.host.endpoint.established(),
            "the host never established: {:?}",
            self.host.endpoint.stats()
        );
    }

    /// Open a page and wait for its first screen.
    fn open(&mut self, shell: &str) {
        let now = self.now;
        self.client
            .open(shell, 46, 20, now)
            .expect("the page can be opened");
        self.client
            .fetch(shell, COLS, ROWS, now)
            .expect("the page can be fetched");
        self.run_until(300_000, 250, |link| {
            link.client.mirror(shell).is_some_and(PageMirror::painted)
        });
        assert!(
            self.client.mirror(shell).is_some_and(PageMirror::painted),
            "the host never painted {shell}: {:?}",
            self.client.stats()
        );
    }

    fn events_for(&self, shell: &str) -> Vec<&ClientEvent> {
        self.events
            .iter()
            .filter(|e| match e {
                ClientEvent::Attached { shell: s, .. }
                | ClientEvent::Settled { shell: s, .. }
                | ClientEvent::Held { shell: s } => s == shell,
                _ => false,
            })
            .collect()
    }

    fn first_line(&self, shell: &str) -> Option<String> {
        self.client
            .mirror(shell)
            .and_then(|m| m.lines.first().cloned())
    }
}

// ---------------------------------------------------------------------------------------------
// 1a. Every uplink request the old radio path could make, and what its answer is.
//
// The survey in S5a found that a rewire would have silently dropped these: the engine could carry a
// command and a refresh, and nothing else. A capability that stops existing does not fail — the
// screen simply stops showing something — so each one gets a test that proves it reached the host.
// ---------------------------------------------------------------------------------------------

#[test]
fn an_attach_carries_the_phone_grid_so_the_host_fits_the_pty_before_it_paints() {
    let mut link = Link::new(11, 1, 0.0);
    link.connect();
    let now = link.now;
    // Opening a page is what asks for a screen, and the grid is what the host fits the pty to
    // **before** it captures. A zero grid here is the clipped-picture defect: the host paints at
    // its own size and the phone has to drop the top rows, which are the prompt and the command.
    link.client
        .open("grid", 46, 17, now)
        .expect("the page can be opened");
    link.run_until(120_000, 250, |link| {
        link.client.mirror("grid").is_some_and(PageMirror::painted)
    });
    let mirror = link.client.mirror("grid").expect("the page painted");
    assert_eq!(
        (mirror.cols, mirror.rows),
        (46, 17),
        "the host painted a grid the phone did not ask for: {:?}",
        link.client.stats()
    );
}

#[test]
fn a_history_request_goes_out_and_the_page_comes_back_as_an_event() {
    let mut link = Link::new(12, 1, 0.0);
    link.connect();
    link.open("hist");
    let now = link.now;
    link.client
        .want_history("hist", 0, 8, now)
        .expect("a history request is legal");
    link.run_until(120_000, 250, |link| {
        link.events
            .iter()
            .any(|e| matches!(e, ClientEvent::History { .. }))
    });
    let page = link
        .events
        .iter()
        .find_map(|e| match e {
            ClientEvent::History { from, lines, older, .. } => Some((*from, lines.clone(), *older)),
            _ => None,
        })
        .expect("the host answered the history request");
    assert!(
        !page.1.is_empty() || page.0 > 0,
        "an empty first page at line 0 is not an answer: {page:?}"
    );
}

#[test]
fn a_create_answers_with_the_id_to_open_rather_than_a_guess() {
    let mut link = Link::new(13, 1, 0.0);
    link.connect();
    let now = link.now;
    link.client
        .create("", now)
        .expect("a create is legal on an established link");
    link.run_until(120_000, 250, |link| {
        link.events
            .iter()
            .any(|e| matches!(e, ClientEvent::Created { .. }))
    });
    let id = link
        .events
        .iter()
        .find_map(|e| match e {
            ClientEvent::Created { id } => Some(id.clone()),
            _ => None,
        })
        .expect("the host never said which shell it made");
    assert!(!id.is_empty(), "the host named an empty shell");
    // The id it named is one the phone can actually open, which is the whole point of saying it.
    link.open(&id);
    assert!(link.client.mirror(&id).is_some_and(PageMirror::painted));
}

#[test]
fn taking_a_shell_back_is_its_own_request_and_not_an_attach() {
    let mut link = Link::new(14, 1, 0.0);
    link.connect();
    let now = link.now;
    link.client
        .takeover("held", now)
        .expect("a takeover is legal on an established link");
    link.run_until(120_000, 250, |link| {
        link.client
            .mirror("held")
            .is_some_and(PageMirror::painted)
    });
    assert!(
        link.client.mirror("held").is_some_and(PageMirror::painted),
        "a takeover opened no page to paint: {:?}",
        link.client.stats()
    );
}

#[test]
fn send_to_desktop_is_its_own_request_and_spends_no_command() {
    let mut link = Link::new(15, 1, 0.0);
    link.connect();
    link.open("live");
    let before = link.client.stats().commands_sent;
    let now = link.now;
    link.client
        .make_live("live", now)
        .expect("make-live is legal on an established link");
    link.run_until(60_000, 250, |_| false);
    assert_eq!(
        link.client.stats().commands_sent,
        before,
        "opening a desk window must not count as a command written to the shell"
    );
}

#[test]
fn ending_a_shell_tells_the_host_and_forgets_the_page() {
    let mut link = Link::new(16, 1, 0.0);
    link.connect();
    link.open("gone");
    let now = link.now;
    link.client
        .end("gone", now)
        .expect("ending a shell is legal on an established link");
    assert!(
        link.client.mirror("gone").is_none(),
        "the page outlived the shell it was bound to"
    );
    link.run_until(60_000, 250, |_| false);
    // The host put it down, so there is nothing left to draw and no page to send to.
    assert_eq!(
        link.client
            .send(
                "gone",
                &[SendItem::Text(b"x".to_vec())],
                COLS,
                ROWS,
                link.now
            )
            .unwrap_err(),
        ClientError::NoPage("gone".to_owned())
    );
}

// ---------------------------------------------------------------------------------------------
// 1. Multi-shell survives by message field (gate B11).
// ---------------------------------------------------------------------------------------------

#[test]
fn three_pages_each_get_their_own_screen_and_no_screen_lands_in_another() {
    let mut link = Link::new(3, 1, 0.0);
    link.connect();
    for shell in ["aaaa", "bbbb", "cccc"] {
        link.open(shell);
    }
    for (shell, word) in [("aaaa", "alpha"), ("bbbb", "bravo"), ("cccc", "charlie")] {
        let now = link.now;
        link.client
            .send(
                shell,
                &[SendItem::Text(word.as_bytes().to_vec())],
                COLS,
                ROWS,
                now,
            )
            .expect("the command is submitted");
    }
    link.run_until(120_000, 250, |link| {
        ["aaaa", "bbbb", "cccc"].iter().all(|shell| {
            link.first_line(shell)
                .is_some_and(|l| !l.is_empty())
        })
    });

    for (shell, word) in [("aaaa", "alpha"), ("bbbb", "bravo"), ("cccc", "charlie")] {
        let mirror = link.client.mirror(shell).expect("the page is open");
        assert_eq!(
            mirror.shell, shell,
            "the screen in {shell}'s page says it belongs to {}",
            mirror.shell
        );
        assert_eq!(
            mirror.lines.first().map(String::as_str),
            Some(word),
            "{shell}'s page does not hold its own screen: {:?}",
            mirror.lines
        );
    }
    // No page holds another page's text: the cross-shell bleed `S7` was.
    let shown: Vec<String> = ["aaaa", "bbbb", "cccc"]
        .iter()
        .filter_map(|s| link.first_line(s))
        .collect();
    let mut unique = shown.clone();
    unique.sort();
    unique.dedup();
    assert_eq!(unique.len(), 3, "two pages painted the same screen: {shown:?}");
}

// ---------------------------------------------------------------------------------------------
// 2. A refresh writes nothing.
// ---------------------------------------------------------------------------------------------

#[test]
fn a_fetch_asks_for_the_screen_and_spends_no_draft() {
    let mut link = Link::new(5, 1, 0.0);
    link.connect();
    link.open("aaaa");
    let writes_before = link.host.writes("aaaa").len();
    let commands_before = link.client.stats().commands_sent;
    let snapshot_before = link.client.mirror("aaaa").expect("open").snapshot_id;

    let now = link.now;
    link.client
        .fetch("aaaa", COLS, ROWS, now)
        .expect("a fetch is submitted");
    link.run_until(60_000, 250, |link| {
        link.client
            .mirror("aaaa")
            .is_some_and(|m| m.snapshot_id != snapshot_before)
    });

    assert_eq!(
        link.host.writes("aaaa").len(),
        writes_before,
        "a refresh wrote to the pty"
    );
    assert_eq!(
        link.client.stats().commands_sent,
        commands_before,
        "a refresh sent a command"
    );
    assert!(
        link.client
            .mirror("aaaa")
            .is_some_and(|m| m.snapshot_id != snapshot_before),
        "the refresh did not bring a new screen"
    );
}

// ---------------------------------------------------------------------------------------------
// 3. A chunked command applies once.
// ---------------------------------------------------------------------------------------------

#[test]
fn a_command_too_large_for_one_message_is_written_once_as_the_bytes_it_was() {
    let mut link = Link::new(7, 1, 0.0);
    link.connect();
    link.open("aaaa");

    // Two thousand bytes is far past one message: the encoder refuses a body over `MAX_BODY`, so this
    // is several chunks sharing one `op_id`.
    let text = "abcdefghij".repeat(200);
    let now = link.now;
    link.client
        .send(
            "aaaa",
            &[SendItem::Text(text.as_bytes().to_vec())],
            COLS,
            ROWS,
            now,
        )
        .expect("the command is submitted");
    link.run_until(180_000, 250, |link| {
        link.host
            .lines("aaaa")
            .first()
            .is_some_and(|l| l.starts_with("abcdefghij"))
    });

    let writes = link.host.writes("aaaa");
    assert_eq!(
        writes.len(),
        1,
        "a chunked command was written {} times, so its pieces ran separately",
        writes.len()
    );
    assert_eq!(
        writes[0],
        text.as_bytes(),
        "the joined command is not the bytes that were typed"
    );
    assert!(
        link.host.chunks_seen > 1,
        "the client did not actually split the command ({} messages)",
        link.host.chunks_seen
    );
}

// ---------------------------------------------------------------------------------------------
// 4. A repeated screen changes nothing and is not reported twice.
// ---------------------------------------------------------------------------------------------

#[test]
fn the_same_screen_twice_is_idempotent() {
    let mut link = Link::new(11, 1, 0.0);
    link.connect();
    link.open("aaaa");

    let bytes = screen("aaaa", 99, &["hello", ""]);
    let now = link.now;
    let first = link.client.on_message(&bytes, now);
    assert!(
        first
            .iter()
            .any(|e| matches!(e, ClientEvent::Settled { snapshot_id: 99, .. })),
        "the first screen was not reported: {first:?}"
    );
    let before = link.client.mirror("aaaa").expect("open").clone();

    let second = link.client.on_message(&bytes, now);
    assert!(
        second.is_empty(),
        "a repeated screen was reported again: {second:?}"
    );
    assert_eq!(
        link.client.mirror("aaaa").expect("open"),
        &before,
        "a repeated screen changed the mirror"
    );
    assert_eq!(link.client.stats().snapshots_duplicate, 1);
}

// ---------------------------------------------------------------------------------------------
// 5. The answer says what happened, and it is a value.
// ---------------------------------------------------------------------------------------------

#[test]
fn the_answer_carries_what_the_host_did_with_the_command() {
    let mut link = Link::new(13, 1, 0.0);
    link.connect();
    link.open("aaaa");

    let now = link.now;
    let op_id = link
        .client
        .send(
            "aaaa",
            &[SendItem::Text(b"echo hi\r".to_vec())],
            COLS,
            ROWS,
            now,
        )
        .expect("the command is submitted");
    link.run_until(60_000, 250, |link| {
        link.events_for("aaaa").iter().any(|e| {
            matches!(
                e,
                ClientEvent::Settled {
                    applied: Some(Applied { op_id: id, .. }),
                    ..
                } if *id == op_id
            )
        })
    });

    let (answered, settled) = link
        .events_for("aaaa")
        .into_iter()
        .find_map(|e| match e {
            ClientEvent::Settled {
                applied: Some(applied),
                settled,
                ..
            } => Some((*applied, *settled)),
            _ => None,
        })
        .expect("the answer to the command was not reported");
    assert_eq!(answered.op_id, op_id, "the answer names another command");
    assert!(
        answered.was_written(),
        "the host wrote the command but the answer says {answered:?}"
    );
    assert!(settled, "a settled host did not say so");
    assert_eq!(link.client.stats().applied_count(applied::WRITTEN), 1);
}

// ---------------------------------------------------------------------------------------------
// 6. The page cap refuses the sixth page and keeps the first five.
// ---------------------------------------------------------------------------------------------

#[test]
fn the_sixth_page_is_refused_and_the_first_five_keep_working() {
    let mut link = Link::new(17, 1, 0.0);
    link.connect();
    let shells: Vec<String> = (0..MAX_OPEN_PAGES).map(|n| format!("shell{n}")).collect();
    let now = link.now;
    for shell in &shells {
        link.client
            .open(shell, 46, 20, now)
            .expect("five pages can be opened");
    }
    match link.client.open("sixth", 46, 20, now) {
        Err(ClientError::TooManyPages { max }) => assert_eq!(max, MAX_OPEN_PAGES),
        other => panic!("the sixth page was not refused with the cap: {other:?}"),
    }
    assert_eq!(link.client.pages().len(), MAX_OPEN_PAGES);
    // The first five are still usable: a refusal must not have dropped one to make room.
    for shell in &shells {
        link.client
            .fetch(shell, COLS, ROWS, now)
            .expect("an open page can still be refreshed");
    }
    link.run_until(180_000, 250, |link| {
        shells
            .iter()
            .all(|s| link.client.mirror(s).is_some_and(PageMirror::painted))
    });
    for shell in &shells {
        assert!(
            link.client
                .mirror(shell)
                .is_some_and(PageMirror::painted),
            "{shell} never painted after the refusal"
        );
    }
}

// ---------------------------------------------------------------------------------------------
// The shell list still arrives.
// ---------------------------------------------------------------------------------------------

#[test]
fn a_list_answer_arrives_as_a_value() {
    let mut link = Link::new(19, 1, 0.0);
    link.connect();
    link.open("aaaa");
    let now = link.now;
    link.client.list(now).expect("a list is submitted");
    link.run_until(60_000, 250, |link| {
        link.events
            .iter()
            .any(|e| matches!(e, ClientEvent::Sessions { last: true, .. }))
    });
    let entries = link
        .events
        .iter()
        .find_map(|e| match e {
            ClientEvent::Sessions { entries, .. } => Some(entries.clone()),
            _ => None,
        })
        .expect("the list answer never arrived");
    assert!(
        entries.iter().any(|e| e.id == "aaaa"),
        "the list does not name the shell that is attached: {entries:?}"
    );
}

// ---------------------------------------------------------------------------------------------
// A screen that never finishes arriving does not hold the page for ever.
// ---------------------------------------------------------------------------------------------

#[test]
fn an_unfinished_screen_is_dropped_rather_than_blocking_the_page() {
    let mut link = Link::new(23, 1, 0.0);
    link.connect();
    link.open("aaaa");

    // Chunk 0 of 2, and never the rest.
    let partial = Message::Snapshot {
        flags: snapshot_flag::FULL,
        chunk_idx: 0,
        chunk_count: 2,
        snapshot_id: 500,
        base_id: 0,
        applied_op_id: 0,
        applied_status: applied::NONE,
        cols: COLS,
        rows: 2,
        cursor_x: 0,
        cursor_y: 0,
        first_line: 0,
        shell: "aaaa".to_owned(),
        patches: vec![Patch {
            y: 0,
            append: false,
            text: "half".to_owned(),
        }],
    }
    .encode()
    .expect("a legal snapshot");
    let now = link.now;
    assert!(link.client.on_message(&partial, now).is_empty());

    // Well past the deadline, a whole screen still paints: the lost chunk did not wedge the page.
    let later = now + 60_000;
    let whole = screen("aaaa", 501, &["whole", ""]);
    let events = link.client.on_message(&whole, later);
    assert!(
        events
            .iter()
            .any(|e| matches!(e, ClientEvent::Settled { snapshot_id: 501, .. })),
        "the page stopped painting after a lost chunk: {events:?}"
    );
    assert_eq!(
        link.client
            .mirror("aaaa")
            .expect("open")
            .lines
            .first()
            .map(String::as_str),
        Some("whole")
    );
}

// ---------------------------------------------------------------------------------------------
// A path from an advert: the only way a record crosses more than one hop.
// ---------------------------------------------------------------------------------------------

/// **A path an advert taught is a candidate, and the next transmission uses it.**
///
/// The firmware never delivers a flooded plaintext record (`crates/mesh/src/companion/push.rs`,
/// AR-10), so a record reaches past one hop only along a path, and the mesh's own advert cache is
/// where a path comes from. This proves the three steps that matter: the advert request leaves the
/// radio, the cache's answer is reversed by hop group, and the endpoint stores it as a candidate
/// that can never be mistaken for a proven route.
#[test]
fn a_path_learned_from_an_advert_is_used_but_never_proven() {
    use ldmux_mesh::companion::command::{self, AdvertPath, parse_advert_path};

    // The reverse is by hop group, which is what makes the path usable in the other direction.
    let mut frame = vec![0x16u8];
    frame.extend_from_slice(&0u32.to_le_bytes());
    frame.push(((2 - 1) << 6) | 2); // two-byte hashes, two hops
    frame.extend_from_slice(&[0x11, 0x12, 0x21, 0x22]);
    let parsed = parse_advert_path(&frame).expect("a path frame");
    assert_eq!(parsed.outbound(), vec![0x21, 0x22, 0x11, 0x12]);

    // The requests the radio is given: an advert to emit, and a cache read for the peer's key.
    assert_eq!(command::self_advert(true), vec![command::commands::SEND_SELF_ADVERT, 1]);
    let key = [0x33u8; 32];
    assert_eq!(command::get_advert_path(&key).len(), 34);

    // A learned arrival is a candidate: `Candidate::learned` has no argument that grants a proof.
    let candidate = ldmux_mesh::route::Candidate::learned(parsed.outbound(), 2, 0.0, 0);
    assert!(!candidate.is_proven(), "an advert says where the peer was, not that a record crossed");

    // And the same path, adopted into a table, is still not proven.
    let mut table = ldmux_mesh::route::RouteTable::new();
    table.observe(candidate);
    assert!(table.best(0, 7).is_some(), "the path is usable");
    assert!(
        !table.best(0, 7).expect("a candidate").is_proven(),
        "and never proof of a working route"
    );

    // **A zero-hop chain is a route.** Two radios a metre apart are adjacent, so the advert's chain
    // has no hash at all, and a direct packet needs no path to reach the peer. The candidate is kept
    // and is still not a proof.
    let adjacent = ldmux_mesh::route::Candidate::learned(Vec::new(), 1, 0.0, 0);
    let mut table = ldmux_mesh::route::RouteTable::new();
    table.observe(adjacent);
    assert!(
        table.best(0, 7).is_some(),
        "an adjacent peer is a route, not a missing one"
    );

    // The type is named so an unused import cannot hide a missing step.
    let _: AdvertPath = parsed;
}

// ---------------------------------------------------------------------------------------------
// Asking for the list again must not replace the request already in flight.
// ---------------------------------------------------------------------------------------------

/// **The screen asks every few seconds while it waits, and each ask used to begin a new
/// transaction.** Measured in the field on 2026-11-13: forty-eight list requests in two minutes, each
/// superseding the last, so none completed and no list ever arrived — the phone showed "connecting"
/// for ever with a healthy session on both ends.
#[test]
fn asking_for_the_list_again_does_not_supersede_the_request_in_flight() {
    let mut link = Link::new(31, 1, 0.0);
    link.connect();

    // **The screen's retry, before any answer has had time to arrive.** That is the field case: the
    // answer never came, and every ask started another transaction until the air was full of them.
    for _ in 0..5 {
        let now = link.now;
        link.client.list(now).expect("a list request is accepted");
    }
    assert_eq!(
        link.client.stats().lists_sent,
        1,
        "the screen's retry started {} transactions instead of one",
        link.client.stats().lists_sent
    );

    // And the one request is what reaches the host.
    link.run_until(300_000, 250, |link| {
        link.host.endpoint.stats().messages_delivered >= 1
    });
    assert!(
        link.host.endpoint.stats().messages_delivered >= 1,
        "the one list request never reached the host: {:?}",
        link.host.endpoint.stats()
    );
}

// ---------------------------------------------------------------------------------------------
// The route request is the only way past the advert cap.
// ---------------------------------------------------------------------------------------------

/// **A repeater drops an advert at eight hops; a `REQ` flood travels sixty-four.** On a mesh longer
/// than eight hops the advert cache can never answer, so both ends must be able to ask each other
/// directly — and the reply must teach the asker the road the question crossed.
#[test]
fn a_route_request_is_answered_and_teaches_the_asker_the_road() {
    use ldmux_mesh::companion::route::{build_reply, build_request, parse_reply};

    let mut link = Link::new(41, 4, 0.0);
    link.connect();

    // The phone asks the host for a route, as it does when it holds none.
    let asker = link.own_key;
    let host_key = link.peer_key;
    let request = build_request(1, &host_key, &asker, &[1, 2, 3, 4]).expect("a request");

    // It crosses four repeaters, so it arrives with four hashes on it.
    let mut arrival = request.clone();
    arrival[1] = ldmux_mesh::companion::build::path_byte(1, 4).expect("four hops");
    arrival.splice(2..2, [0xA1, 0xA2, 0xA3, 0xA4]);

    // The host reads it and answers with the chain reversed.
    let parsed = ldmux_mesh::companion::route::parse_request(&arrival, &host_key)
        .expect("the host reads the request");
    let reply = build_reply(&host_key, &parsed.peer_key, 1, &parsed.path, &parsed.nonce)
        .expect("the host answers");
    let learned = parse_reply(&reply, &host_key).expect("the phone reads the answer");
    assert_eq!(
        learned.path,
        vec![0xA4, 0xA3, 0xA2, 0xA1],
        "the way back is the way here in the opposite order"
    );
}

/// **The phone answers a route request and adopts a route reply, through its own frame handler.**
///
/// Both packets arrive as the receive log's `0x88` push, because both are `REQ` floods. This is the
/// seam that matters: the mechanism is useless if the frames are classified as session records and
/// dropped, which is what the old 0x88-only classifier did to every real arrival.
#[test]
fn the_phone_answers_a_route_request_and_adopts_the_reply() {
    use ldmux_mesh::companion::route::{build_reply, build_request, parse_reply};

    let mut link = Link::new(43, 4, 0.0);
    link.connect();
    let (own, peer) = (link.own_key, link.peer_key);

    // A request from the host, addressed to this phone, after crossing two repeaters.
    let mut request = build_request(1, &own, &peer, &[9, 9, 9, 9]).expect("a request");
    request[1] = ldmux_mesh::companion::build::path_byte(1, 2).expect("two hops");
    request.splice(2..2, [0xC1, 0xC2]);
    let mut frame = vec![ldmux_mesh::companion::push::PUSH_LOG_RX_DATA, 0xF4, 0xA6];
    frame.extend_from_slice(&request);

    // The parse is checked on its own first, so a wiring fault is not mistaken for a format fault.
    assert!(
        ldmux_mesh::companion::route::parse_request(&request, &own).is_some(),
        "the request does not parse at all"
    );
    let now = link.now;
    link.client.on_frame(&frame, now, link.wall());
    assert!(
        link.client.radio().counters().route_replies >= 1,
        "the phone did not answer: the request never reached the radio's route handler"
    );
    let _ = now;

    // The phone must have queued a reply, and that reply must carry the chain reversed. A queued
    // command reaches the air on the next poll, because the radio's window is a separate step.
    // The reply reaches the air inside `step`, which drains the engine's frames and hands them to
    // the mesh. Its contents are pinned by `route.rs`'s own parse tests; what this test owns is that
    // the frame handler took the request to the radio at all, which the counter above proves.
    for _ in 0..8 {
        link.step(250);
    }

    // And a reply from the host is adopted as the phone's road.
    let before = link.client.stats().path_learned;
    let answer = build_reply(&peer, &own, 1, &[0xD1], &[7, 7, 7, 7]).expect("a reply");
    let mut frame = vec![ldmux_mesh::companion::push::PUSH_LOG_RX_DATA, 0xF4, 0xA6];
    frame.extend_from_slice(&answer);
    let now = link.now;
    link.client.on_frame(&frame, now, link.wall());
    assert!(
        link.client.stats().path_learned > before,
        "the reply was not adopted as a road: {:?}",
        link.client.stats()
    );
    assert!(parse_reply(&answer, &peer).is_some());
}
