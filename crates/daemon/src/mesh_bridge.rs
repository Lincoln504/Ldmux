//! The shell bridge: the mesh terminal profile (ADR-0025) between a live shell and the L3
//! session.
//!
//! It reads **the keeper's grid** (the `snapshot` RPC, never raw PTY bytes), sends the phone
//! plain-text rows and then only the rows that changed, and writes the phone's `Input` to the
//! shell. It holds no socket and no clock of its own: [`Link`] is the session (the runtime in
//! [`crate::mesh`]) and [`Host`] is the daemon, so every rule here is testable without either.
//!
//! Rules that matter:
//!
//! - **One update in flight.** A screen is read and diffed only when the session has nothing
//!   queued. On a link that carries a record a minute, that yields fewer, newer updates; it
//!   never builds a queue of stale ones, and a phone that was away converges by one diff.
//! - **A failed send resets the diff.** If any message of an update cannot be queued, the
//!   differ forgets what the phone holds, so the next update is a full screen. The phone is
//!   never left holding half of a change the host believes it delivered.
//! - **A new handshake drops every attach.** The phone must `Attach` again; nothing carries over
//!   a session that ended.
//! - **Nothing here knows a radio.** The section 13 grep over the daemon crates stays at zero hits.

use std::collections::HashMap;
use std::sync::Arc;

use ldmux_protocol::keeper::SnapshotResult;
use ldmux_protocol::mesh_term::{
    DecodeError, Decoder, Differ, KEY_VERSION, MIN_VERSION, Message, RESIZE_VERSION,
    STREAM_VERSION, Screen, VERSION, closed, session_pages,
};

/// The session as the bridge sees it.
pub trait Link {
    /// An L3 session is established.
    fn established(&self) -> bool;
    /// Changes whenever a new handshake is accepted.
    fn generation(&self) -> u32;
    /// Records still queued toward the phone.
    fn pending(&self) -> usize;
    fn send(&self, shell: u8, bytes: &[u8]) -> Result<(), String>;
    fn take_inbound(&self) -> Vec<(u8, Vec<u8>)>;
}

/// The daemon as the bridge sees it.
#[allow(async_fn_in_trait)]
pub trait Host {
    /// Ids of the running shells.
    async fn running(&mut self) -> Vec<String>;
    /// `(id, label)` of the running shells, for the phone's picker. The default has no labels.
    async fn listing(&mut self) -> Vec<(String, String)> {
        self.running()
            .await
            .into_iter()
            .map(|id| (id, String::new()))
            .collect()
    }
    async fn screen(&mut self, id: &str) -> Result<Screen, String>;
    async fn input(&mut self, id: &str, bytes: &[u8]) -> Result<(), String>;
    /// Named keys for the shell, encoded for **that pane's own mode** (cursor-key mode, Kitty
    /// flags) by the daemon's one key table. The bridge never turns a name into bytes itself:
    /// a second table is a second behaviour, and this one cannot see the pane.
    async fn keys(&mut self, id: &str, names: &[String]) -> Result<(), String>;
    /// Fit the shell's pty to the phone's grid, through the keeper's own resize (the one a
    /// tailnet client's `resize` ends in), so the screen is never wider than the phone.
    async fn resize(&mut self, id: &str, cols: u8, rows: u8) -> Result<(), String>;
}

impl Link for crate::mesh::MeshRuntime {
    fn established(&self) -> bool {
        self.snapshot().session.state == "established"
    }
    fn generation(&self) -> u32 {
        self.snapshot().session.handshakes
    }
    fn pending(&self) -> usize {
        self.snapshot().session.pending
    }
    fn send(&self, shell: u8, bytes: &[u8]) -> Result<(), String> {
        Self::send(self, shell, bytes)
    }
    fn take_inbound(&self) -> Vec<(u8, Vec<u8>)> {
        Self::take_inbound(self)
    }
}

/// What the bridge has done, for tests and for a future status field.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct BridgeStats {
    pub updates: u32,
    pub messages_sent: u32,
    /// Records actually handed to the link (several messages may share one).
    pub records_sent: u32,
    pub inputs: u32,
    pub keys: u32,
    pub resizes: u32,
    /// Resizes that named the geometry already in force: no change, no repaint.
    pub resizes_unchanged: u32,
    pub malformed: u32,
    pub refused_sends: u32,
}

struct Attached {
    session: String,
    differ: Differ,
    /// The profile version the phone attached with. A phone on version 1 does not send `Key`,
    /// and one that does is not obeyed: it did not ask for the message's meaning.
    version: u8,
    /// The grid last accepted from this phone. A `Resize` to the size already set is not a
    /// change, and starting the screen over for it repaints the whole pane for nothing - which
    /// on this link is a burst of records the phone waits through, seen as a freeze while a
    /// full-screen app is open.
    geometry: Option<(u8, u8)>,
}

pub struct Bridge<H, L> {
    host: H,
    link: L,
    shells: HashMap<u8, Attached>,
    decoders: HashMap<u8, Decoder>,
    generation: u32,
    pub stats: BridgeStats,
}

impl<H: Host, L: Link> Bridge<H, L> {
    pub fn new(host: H, link: L) -> Self {
        Self {
            host,
            link,
            shells: HashMap::new(),
            decoders: HashMap::new(),
            generation: 0,
            stats: BridgeStats::default(),
        }
    }

    /// Whether `shell` is currently attached.
    #[must_use]
    pub fn attached(&self, shell: u8) -> bool {
        self.shells.contains_key(&shell)
    }

    fn reply(&mut self, shell: u8, messages: &[Message]) -> bool {
        // **Messages share a record when they fit.** The phone's decoder is a stream decoder
        // (`TermDecoder` in MeshTerm.kt), so a record may carry several messages, and the
        // protocol's own `RECORD_PAYLOAD` is the only limit. Sending one record per message
        // cost a whole 176-byte frame per message: a one-row echo paid two frames (`Rows` and
        // its `Commit`) where the pair is 30-40 bytes. Packed, the same echo is one frame.
        //
        // A phone that attached below [STREAM_VERSION] is not assumed to decode a record with
        // more than one message, so it keeps one record per message.
        let streamed = self
            .shells
            .get(&shell)
            .map(|s| s.version >= STREAM_VERSION)
            .unwrap_or(false);
        let mut ok = true;
        let mut record: Vec<u8> = Vec::with_capacity(ldmux_protocol::mesh_term::RECORD_PAYLOAD);
        let mut flush =
            |record: &mut Vec<u8>, ok: &mut bool, link: &mut L, stats: &mut BridgeStats| {
                if record.is_empty() {
                    return;
                }
                if link.send(shell, record).is_ok() {
                    stats.records_sent += 1;
                } else {
                    stats.refused_sends += 1;
                    *ok = false;
                }
                record.clear();
            };
        for m in messages {
            let Ok(bytes) = m.encode() else {
                ok = false;
                continue;
            };
            if !streamed {
                if self.link.send(shell, &bytes).is_ok() {
                    self.stats.messages_sent += 1;
                } else {
                    self.stats.refused_sends += 1;
                    ok = false;
                    break;
                }
                continue;
            }
            if !record.is_empty()
                && record.len() + bytes.len() > ldmux_protocol::mesh_term::RECORD_PAYLOAD
            {
                flush(&mut record, &mut ok, &mut self.link, &mut self.stats);
                if !ok {
                    break;
                }
            }
            record.extend_from_slice(&bytes);
            self.stats.messages_sent += 1;
        }
        if ok {
            flush(&mut record, &mut ok, &mut self.link, &mut self.stats);
            if timing() {
                eprintln!("ldmuxd: mesh timing: reply queued at {:?}", stamp());
            }
        }
        ok
    }

    /// One pass: take what the phone sent, then push what changed on screen.
    pub async fn tick(&mut self) {
        if !self.link.established() {
            self.shells.clear();
            self.decoders.clear();
            return;
        }
        let generation = self.link.generation();
        if generation != self.generation {
            self.generation = generation;
            self.shells.clear();
            self.decoders.clear();
        }

        for (shell, bytes) in self.link.take_inbound() {
            let decoded = self.decoders.entry(shell).or_default().push(&bytes);
            let items = match decoded {
                Ok(items) => items,
                Err(
                    DecodeError::BodyTooLong | DecodeError::Overflow | DecodeError::Malformed(_),
                ) => {
                    // The stream is unusable: drop the attach, the phone starts again.
                    self.stats.malformed += 1;
                    self.shells.remove(&shell);
                    self.decoders.remove(&shell);
                    continue;
                }
            };
            for item in items {
                match item {
                    Ok(m) => self.handle(shell, m).await,
                    Err(_) => self.stats.malformed += 1,
                }
            }
        }

        let ids: Vec<u8> = self.shells.keys().copied().collect();
        for shell in ids {
            // **Keep the pipe full, do not build a backlog.** This used to stop looking at the
            // screen while ANY record was unacked, which cost a whole round trip per update: the
            // echo you were waiting for could not even be *built* until the previous record's ack
            // came back, and on this link an ack only rides on the phone's next record or its pure
            // ack timer. Measured on the device as a 16 s gap between a keystroke and its echo.
            // The bound that belongs here is the window: past it the queue would only grow.
            let room = PENDING_LIMIT.saturating_sub(self.link.pending());
            if room == 0 {
                break;
            }
            let Some(session) = self.shells.get(&shell).map(|a| a.session.clone()) else {
                continue;
            };
            let screen = match self.host.screen(&session).await {
                Ok(s) => s,
                Err(_) => {
                    self.reply(
                        shell,
                        &[Message::Closed {
                            reason: closed::UNAVAILABLE,
                        }],
                    );
                    self.shells.remove(&shell);
                    continue;
                }
            };
            // **Send only what the window can take, and mean it.** The differ bounds itself to
            // `room` messages and advances its record of the phone's screen only over the rows
            // that actually went out, so a frame too big for the window is spread over ticks
            // instead of being queued whole: one full repaint used to sit in front of a
            // keystroke as eight records, which the device showed as a freeze.
            let messages = match self.shells.get_mut(&shell) {
                Some(a) => a.differ.update_within(&screen, room),
                None => continue,
            };
            if messages.is_empty() {
                continue;
            }
            self.stats.updates += 1;
            if !self.reply(shell, &messages) {
                if let Some(a) = self.shells.get_mut(&shell) {
                    a.differ.reset();
                }
                continue;
            }
            if screen.dead {
                self.reply(
                    shell,
                    &[Message::Closed {
                        reason: closed::EXITED,
                    }],
                );
                self.shells.remove(&shell);
            }
        }
    }

    async fn handle(&mut self, shell: u8, m: Message) {
        match m {
            Message::Attach { version, selector } => {
                if !(MIN_VERSION..=VERSION).contains(&version) {
                    self.reply(
                        shell,
                        &[Message::Closed {
                            reason: closed::VERSION,
                        }],
                    );
                    return;
                }
                let running = self.host.running().await;
                let matches: Vec<&String> = running
                    .iter()
                    .filter(|id| selector.is_empty() || id.starts_with(&selector))
                    .collect();
                match matches.as_slice() {
                    [] => {
                        self.reply(
                            shell,
                            &[Message::Closed {
                                reason: closed::NOT_FOUND,
                            }],
                        );
                    }
                    [one] => {
                        self.shells.insert(
                            shell,
                            Attached {
                                session: (*one).clone(),
                                // Answered in the version asked for, naming the shell the host
                                // chose, so a phone that sent an empty selector learns which.
                                differ: Differ::for_attach(version, one),
                                version,
                                geometry: None,
                            },
                        );
                    }
                    _ => {
                        self.reply(
                            shell,
                            &[Message::Closed {
                                reason: closed::AMBIGUOUS,
                            }],
                        );
                    }
                }
            }
            Message::List => {
                let listing = self.host.listing().await;
                self.reply(shell, &session_pages(&listing));
            }
            Message::Input(bytes) => {
                let Some(session) = self.shells.get(&shell).map(|a| a.session.clone()) else {
                    return;
                };
                self.stats.inputs += 1;
                // Timing probe, off unless asked for (`LDMUX_MESH_TIMING=1`): the phone's own
                // screen is a native surface with no text to poll, so the only honest way to
                // measure a keystroke's turnaround is from inside the host. This marks the
                // moment input arrived; `reply` marks when its echo was queued.
                if timing() {
                    eprintln!(
                        "ldmuxd: mesh timing: input {} bytes at {:?}",
                        bytes.len(),
                        stamp()
                    );
                }
                let _ = self.host.input(&session, &bytes).await;
            }
            Message::Key(names) => {
                let Some(a) = self.shells.get(&shell) else {
                    return;
                };
                if a.version < KEY_VERSION {
                    return;
                }
                let session = a.session.clone();
                self.stats.keys += 1;
                // A name the table refuses is the daemon's refusal: nothing reaches the shell.
                let _ = self.host.keys(&session, &names).await;
            }
            Message::Resize { cols, rows } => {
                let Some(a) = self.shells.get(&shell) else {
                    return;
                };
                // A phone that attached below version 3 did not ask for this message's meaning.
                if a.version < RESIZE_VERSION {
                    return;
                }
                let session = a.session.clone();
                self.stats.resizes += 1;
                // A phone repeats its geometry on every relayout, and the keeper answers the
                // same size happily, so without this an unchanged size would throw away the
                // differ's record and send the whole pane again - twice over for a keyboard
                // that opens and closes.
                if a.geometry == Some((cols, rows)) {
                    self.stats.resizes_unchanged += 1;
                    return;
                }
                // Only a resize the pane accepted starts the screen over: a refused one leaves
                // the grid as it was and the phone keeps what it has.
                if self.host.resize(&session, cols, rows).await.is_ok() {
                    if let Some(a) = self.shells.get_mut(&shell) {
                        a.geometry = Some((cols, rows));
                        a.differ.reset();
                    }
                }
            }
            Message::Resync => {
                if let Some(a) = self.shells.get_mut(&shell) {
                    a.differ.reset();
                }
            }
            Message::Detach => {
                self.shells.remove(&shell);
            }
            // Host-to-phone messages arriving from the phone are ignored.
            _ => {}
        }
    }
}

/// The daemon as a [`Host`]: every call is an RPC the control socket already serves.
pub struct DaemonHost(pub Arc<tokio::sync::Mutex<crate::daemon::LdmuxDaemon>>);

impl Host for DaemonHost {
    async fn running(&mut self) -> Vec<String> {
        let Ok(v) = self
            .0
            .lock()
            .await
            .handle("list", serde_json::json!({}))
            .await
        else {
            return Vec::new();
        };
        v.get("sessions")
            .and_then(|s| s.as_array())
            .map(|a| {
                a.iter()
                    .filter(|s| s.get("status").and_then(|x| x.as_str()) == Some("running"))
                    .filter_map(|s| s.get("id").and_then(|x| x.as_str()).map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default()
    }

    async fn listing(&mut self) -> Vec<(String, String)> {
        let Ok(v) = self
            .0
            .lock()
            .await
            .handle("list", serde_json::json!({}))
            .await
        else {
            return Vec::new();
        };
        v.get("sessions")
            .and_then(|s| s.as_array())
            .map(|a| {
                a.iter()
                    .filter(|s| s.get("status").and_then(|x| x.as_str()) == Some("running"))
                    .filter_map(|s| {
                        let id = s.get("id").and_then(|x| x.as_str())?;
                        let label = s.get("title").and_then(|x| x.as_str()).unwrap_or("");
                        Some((id.to_owned(), label.to_owned()))
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    async fn screen(&mut self, id: &str) -> Result<Screen, String> {
        let v = self
            .0
            .lock()
            .await
            .handle("snapshot", serde_json::json!({ "id": id }))
            .await?;
        let s: SnapshotResult = serde_json::from_value(v).map_err(|e| e.to_string())?;
        Ok(Screen::from_keeper(
            &s.text,
            s.cols,
            s.rows,
            (s.cursor_x, s.cursor_y),
            s.alternate_on,
            s.dead,
        ))
    }

    async fn input(&mut self, id: &str, bytes: &[u8]) -> Result<(), String> {
        let text = std::str::from_utf8(bytes).map_err(|_| "input is not UTF-8".to_owned())?;
        self.0
            .lock()
            .await
            .handle("send", serde_json::json!({ "id": id, "text": text }))
            .await
            .map(|_| ())
    }

    /// The same `send` the CLI and the tailnet clients use: it reads the pane's cursor-key mode
    /// and Kitty flags from the keeper and encodes each name with `key_name_to_bytes_in_mode`,
    /// refusing a name it does not know instead of typing it into the shell.
    async fn keys(&mut self, id: &str, names: &[String]) -> Result<(), String> {
        self.0
            .lock()
            .await
            .handle("send", serde_json::json!({ "id": id, "keys": names }))
            .await
            .map(|_| ())
    }

    async fn resize(&mut self, id: &str, cols: u8, rows: u8) -> Result<(), String> {
        self.0
            .lock()
            .await
            .handle(
                "resizePane",
                serde_json::json!({ "id": id, "cols": cols, "rows": rows }),
            )
            .await
            .map(|_| ())
    }
}

/// How often the bridge looks at the screen. Interactive echo wants this small, the airtime
/// bound does not care: an update waits for an empty queue regardless.
pub const TICK_MS: u64 = 50;

/// How many unacked records may be in flight before the bridge stops looking at the screen. It is
/// the session's own window, so the queue can never grow past what the link can carry.
pub const PENDING_LIMIT: usize = 8;

/// Run the bridge for the life of the process.
pub async fn run<H: Host, L: Link>(mut bridge: Bridge<H, L>) {
    let mut interval = tokio::time::interval(std::time::Duration::from_millis(TICK_MS));
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        interval.tick().await;
        bridge.tick().await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ldmux_protocol::mesh_term::{MAX_MESSAGE, Mirror};
    use std::sync::Mutex;

    #[derive(Default)]
    struct LinkState {
        established: bool,
        generation: u32,
        pending: usize,
        sent: Vec<(u8, Vec<u8>)>,
        inbound: Vec<(u8, Vec<u8>)>,
        refuse: bool,
    }
    #[derive(Clone, Default)]
    struct FakeLink(Arc<Mutex<LinkState>>);
    impl Link for FakeLink {
        fn established(&self) -> bool {
            self.0.lock().unwrap().established
        }
        fn generation(&self) -> u32 {
            self.0.lock().unwrap().generation
        }
        fn pending(&self) -> usize {
            self.0.lock().unwrap().pending
        }
        fn send(&self, shell: u8, bytes: &[u8]) -> Result<(), String> {
            let mut s = self.0.lock().unwrap();
            if s.refuse {
                return Err("full".into());
            }
            s.sent.push((shell, bytes.to_vec()));
            Ok(())
        }
        fn take_inbound(&self) -> Vec<(u8, Vec<u8>)> {
            std::mem::take(&mut self.0.lock().unwrap().inbound)
        }
    }

    #[derive(Default)]
    struct HostState {
        running: Vec<String>,
        screens: HashMap<String, Screen>,
        typed: Vec<(String, Vec<u8>)>,
        keyed: Vec<(String, Vec<String>)>,
        resized: Vec<(String, u8, u8)>,
        refuse_resize: bool,
        fail_screen: bool,
    }
    #[derive(Clone, Default)]
    struct FakeHost(Arc<Mutex<HostState>>);
    impl Host for FakeHost {
        async fn running(&mut self) -> Vec<String> {
            self.0.lock().unwrap().running.clone()
        }
        async fn listing(&mut self) -> Vec<(String, String)> {
            let s = self.0.lock().unwrap();
            s.running
                .iter()
                .map(|id| (id.clone(), format!("label of {id}")))
                .collect()
        }
        async fn screen(&mut self, id: &str) -> Result<Screen, String> {
            let s = self.0.lock().unwrap();
            if s.fail_screen {
                return Err("keeper gone".into());
            }
            s.screens.get(id).cloned().ok_or_else(|| "none".into())
        }
        async fn input(&mut self, id: &str, bytes: &[u8]) -> Result<(), String> {
            self.0
                .lock()
                .unwrap()
                .typed
                .push((id.to_owned(), bytes.to_vec()));
            Ok(())
        }
        async fn keys(&mut self, id: &str, names: &[String]) -> Result<(), String> {
            self.0
                .lock()
                .unwrap()
                .keyed
                .push((id.to_owned(), names.to_vec()));
            Ok(())
        }
        async fn resize(&mut self, id: &str, cols: u8, rows: u8) -> Result<(), String> {
            let mut s = self.0.lock().unwrap();
            if s.refuse_resize {
                return Err("no live session".into());
            }
            s.resized.push((id.to_owned(), cols, rows));
            // The pane's grid really changes, as the keeper's does.
            if let Some(sc) = s.screens.get_mut(id) {
                sc.cols = cols;
                sc.rows = rows;
                sc.lines.resize(usize::from(rows), String::new());
            }
            Ok(())
        }
    }

    fn screen(lines: &[&str]) -> Screen {
        Screen {
            cols: 80,
            rows: lines.len() as u8,
            cursor_x: 0,
            cursor_y: 0,
            alternate: false,
            dead: false,
            lines: lines.iter().map(|s| (*s).to_owned()).collect(),
        }
    }

    fn setup() -> (Bridge<FakeHost, FakeLink>, FakeHost, FakeLink) {
        let host = FakeHost::default();
        host.0.lock().unwrap().running = vec!["abcd-1".into()];
        host.0
            .lock()
            .unwrap()
            .screens
            .insert("abcd-1".into(), screen(&["$", ""]));
        let link = FakeLink::default();
        {
            let mut l = link.0.lock().unwrap();
            l.established = true;
            l.generation = 1;
        }
        (Bridge::new(host.clone(), link.clone()), host, link)
    }

    fn phone_says(link: &FakeLink, shell: u8, m: &Message) {
        link.0
            .lock()
            .unwrap()
            .inbound
            .push((shell, m.encode().unwrap()));
    }

    /// What the phone would hold after everything the host sent.
    fn phone_view(link: &FakeLink, shell: u8) -> Mirror {
        let mut d = Decoder::new();
        let mut m = Mirror::new();
        for (s, bytes) in link.0.lock().unwrap().sent.iter() {
            if *s == shell {
                for item in d.push(bytes).unwrap() {
                    m.apply(&item.unwrap());
                }
            }
        }
        m
    }

    #[tokio::test]
    async fn attach_then_snapshot_then_keystroke_then_changed_rows() {
        let (mut b, host, link) = setup();
        phone_says(
            &link,
            3,
            &Message::Attach {
                version: VERSION,
                selector: String::new(),
            },
        );
        b.tick().await;
        assert!(b.attached(3));
        assert_eq!(
            phone_view(&link, 3).lines,
            ["$", ""],
            "the whole screen, as plain rows"
        );
        assert!(
            link.0
                .lock()
                .unwrap()
                .sent
                .iter()
                .all(|(_, m)| m.len() <= MAX_MESSAGE)
        );

        // A keystroke goes to the shell as typed.
        phone_says(&link, 3, &Message::Input(b"ls\r".to_vec()));
        b.tick().await;
        assert_eq!(
            host.0.lock().unwrap().typed,
            vec![("abcd-1".to_owned(), b"ls\r".to_vec())]
        );

        // The shell answers; only the changed row crosses.
        let sent_before = link.0.lock().unwrap().sent.len();
        host.0
            .lock()
            .unwrap()
            .screens
            .insert("abcd-1".into(), screen(&["$ ls", "file.txt"]));
        b.tick().await;
        assert_eq!(phone_view(&link, 3).lines, ["$ ls", "file.txt"]);
        let new = link.0.lock().unwrap().sent[sent_before..].to_vec();
        assert_eq!(
            new.len(),
            1,
            "one record carrying the Rows and its Commit: no Profile, no full screen"
        );
    }

    #[tokio::test]
    async fn nothing_is_sent_when_nothing_changed_or_the_queue_is_busy() {
        let (mut b, host, link) = setup();
        phone_says(
            &link,
            0,
            &Message::Attach {
                version: VERSION,
                selector: "abcd".into(),
            },
        );
        b.tick().await;
        let n = link.0.lock().unwrap().sent.len();
        b.tick().await;
        assert_eq!(link.0.lock().unwrap().sent.len(), n, "unchanged");

        host.0
            .lock()
            .unwrap()
            .screens
            .insert("abcd-1".into(), screen(&["changed", ""]));
        // Busy means the window is full, not "one record is in flight": waiting for the first
        // ack cost a whole round trip per update (measured on the device as sixteen seconds).
        link.0.lock().unwrap().pending = PENDING_LIMIT;
        b.tick().await;
        assert_eq!(
            link.0.lock().unwrap().sent.len(),
            n,
            "a full window gets no new update"
        );
        link.0.lock().unwrap().pending = 0;
        b.tick().await;
        assert!(link.0.lock().unwrap().sent.len() > n);
    }

    #[tokio::test]
    async fn a_phone_that_was_away_gets_one_diff_of_the_present() {
        let (mut b, host, link) = setup();
        phone_says(
            &link,
            1,
            &Message::Attach {
                version: VERSION,
                selector: String::new(),
            },
        );
        b.tick().await;
        link.0.lock().unwrap().pending = PENDING_LIMIT; // unreachable for a while
        for i in 0..30 {
            host.0
                .lock()
                .unwrap()
                .screens
                .insert("abcd-1".into(), screen(&[&format!("tick {i}"), "x"]));
            b.tick().await;
        }
        let n = link.0.lock().unwrap().sent.len();
        link.0.lock().unwrap().pending = 0;
        b.tick().await;
        assert_eq!(
            link.0.lock().unwrap().sent.len() - n,
            1,
            "one record: the Rows and its Commit share it"
        );
        assert_eq!(phone_view(&link, 1).lines, ["tick 29", "x"]);
    }

    #[tokio::test]
    async fn a_refused_send_resets_so_the_phone_is_never_left_half_updated() {
        let (mut b, host, link) = setup();
        phone_says(
            &link,
            1,
            &Message::Attach {
                version: VERSION,
                selector: String::new(),
            },
        );
        b.tick().await;
        host.0
            .lock()
            .unwrap()
            .screens
            .insert("abcd-1".into(), screen(&["A", "B"]));
        link.0.lock().unwrap().refuse = true;
        b.tick().await;
        assert!(b.stats.refused_sends >= 1);
        link.0.lock().unwrap().refuse = false;
        b.tick().await;
        assert_eq!(
            phone_view(&link, 1).lines,
            ["A", "B"],
            "converged after the reset"
        );
    }

    #[tokio::test]
    async fn selectors_are_exact_about_not_found_and_ambiguous() {
        let (mut b, host, link) = setup();
        host.0.lock().unwrap().running = vec!["abcd-1".into(), "abcd-2".into()];
        phone_says(
            &link,
            0,
            &Message::Attach {
                version: VERSION,
                selector: "zzzz".into(),
            },
        );
        phone_says(
            &link,
            1,
            &Message::Attach {
                version: VERSION,
                selector: "abcd".into(),
            },
        );
        phone_says(
            &link,
            2,
            &Message::Attach {
                version: VERSION,
                selector: String::new(),
            },
        );
        phone_says(
            &link,
            3,
            &Message::Attach {
                version: 9,
                selector: String::new(),
            },
        );
        b.tick().await;
        let sent = link.0.lock().unwrap().sent.clone();
        let reason = |shell: u8| {
            let mut d = Decoder::new();
            let (_, bytes) = sent.iter().find(|(s, _)| *s == shell).unwrap();
            match d.push(bytes).unwrap().remove(0).unwrap() {
                Message::Closed { reason } => reason,
                m => panic!("{m:?}"),
            }
        };
        assert_eq!(reason(0), closed::NOT_FOUND);
        assert_eq!(reason(1), closed::AMBIGUOUS);
        assert_eq!(reason(2), closed::AMBIGUOUS);
        assert_eq!(reason(3), closed::VERSION);
        assert!(!b.attached(0) && !b.attached(1) && !b.attached(2) && !b.attached(3));
    }

    /// Every `Sessions` page the host sent on `shell`.
    fn sessions_seen(link: &FakeLink, shell: u8) -> Vec<Message> {
        let mut d = Decoder::new();
        let mut out = Vec::new();
        for (s, bytes) in link.0.lock().unwrap().sent.iter() {
            if *s == shell {
                for item in d.push(bytes).unwrap() {
                    if let m @ Message::Sessions { .. } = item.unwrap() {
                        out.push(m);
                    }
                }
            }
        }
        out
    }

    #[tokio::test]
    async fn list_answers_with_every_running_shell_and_never_guesses() {
        for (n, expect_pages_min) in [(0usize, 1usize), (1, 1), (6, 2)] {
            let (mut b, host, link) = setup();
            host.0.lock().unwrap().running = (0..n)
                .map(|i| format!("{i:02}{}", "e".repeat(30)))
                .collect();
            phone_says(&link, 4, &Message::List);
            b.tick().await;
            let pages = sessions_seen(&link, 4);
            assert!(
                pages.len() >= expect_pages_min,
                "n={n}: {} pages",
                pages.len()
            );
            let mut ids = Vec::new();
            for (i, m) in pages.iter().enumerate() {
                let Message::Sessions { last, entries } = m else {
                    unreachable!()
                };
                assert_eq!(*last, i + 1 == pages.len());
                ids.extend(entries.iter().map(|e| e.id.clone()));
            }
            assert_eq!(ids.len(), n, "n={n}");
            // Listing attaches nothing: the phone must choose.
            assert!(!b.attached(4), "n={n}: a list is not an attach");
        }
    }

    #[tokio::test]
    async fn the_chosen_id_attaches_what_was_listed() {
        let (mut b, host, link) = setup();
        host.0.lock().unwrap().running = vec!["abcd-1".into(), "abcd-2".into()];
        host.0
            .lock()
            .unwrap()
            .screens
            .insert("abcd-2".into(), screen(&["two"]));
        phone_says(&link, 4, &Message::List);
        b.tick().await;
        let Message::Sessions { entries, .. } = &sessions_seen(&link, 4)[0] else {
            panic!()
        };
        assert_eq!(entries[1].id, "abcd-2");
        phone_says(
            &link,
            4,
            &Message::Attach {
                version: 1,
                selector: entries[1].id.clone(),
            },
        );
        b.tick().await;
        assert!(b.attached(4));
    }

    #[tokio::test]
    async fn a_key_message_reaches_the_attached_shell_as_names_never_as_bytes() {
        let (mut b, host, link) = setup();
        phone_says(
            &link,
            1,
            &Message::Attach {
                version: VERSION,
                selector: String::new(),
            },
        );
        b.tick().await;
        phone_says(&link, 1, &Message::Key(vec!["Up".into(), "S-Tab".into()]));
        b.tick().await;
        let h = host.0.lock().unwrap();
        assert_eq!(
            h.keyed,
            vec![(
                "abcd-1".to_owned(),
                vec!["Up".to_owned(), "S-Tab".to_owned()]
            )],
            "the host's own key table does the encoding, per the pane's mode"
        );
        assert!(h.typed.is_empty(), "no bytes were typed by the bridge");
        assert_eq!(b.stats.keys, 1);
    }

    /// Every host→phone message sent on `shell`, decoded in order.
    fn sent_messages(link: &FakeLink, shell: u8) -> Vec<Message> {
        let mut d = Decoder::new();
        let mut out = Vec::new();
        for (s, bytes) in link.0.lock().unwrap().sent.iter() {
            if *s == shell {
                out.extend(d.push(bytes).unwrap().into_iter().map(Result::unwrap));
            }
        }
        out
    }

    #[tokio::test]
    async fn a_resize_fits_the_pane_and_starts_the_screen_over_with_the_new_grid() {
        let (mut b, host, link) = setup();
        phone_says(
            &link,
            1,
            &Message::Attach {
                version: VERSION,
                selector: String::new(),
            },
        );
        b.tick().await;
        let before = sent_messages(&link, 1).len();
        phone_says(&link, 1, &Message::Resize { cols: 45, rows: 30 });
        b.tick().await;
        assert_eq!(
            host.0.lock().unwrap().resized,
            vec![("abcd-1".to_owned(), 45, 30)]
        );
        assert_eq!(b.stats.resizes, 1);
        let after = sent_messages(&link, 1);
        assert!(
            matches!(
                &after[before],
                Message::Profile { cols: 45, rows: 30, shell, .. } if shell == "abcd-1"
            ),
            "a fresh Profile with the new grid leads the answer: {:?}",
            &after[before..]
        );
        assert!(
            matches!(&after[before + 1], Message::Rows { full: true, .. }),
            "and the screen follows in full"
        );
        let m = phone_view(&link, 1);
        assert_eq!(m.profile.map(|p| (p.0, p.1)), Some((45, 30)));
        assert_eq!(m.lines.len(), 30);
    }

    #[tokio::test]
    async fn a_resize_before_attach_from_an_older_phone_or_a_refused_one_changes_nothing() {
        let (mut b, host, link) = setup();
        phone_says(&link, 1, &Message::Resize { cols: 45, rows: 30 });
        b.tick().await;
        assert!(host.0.lock().unwrap().resized.is_empty(), "not attached");
        phone_says(
            &link,
            2,
            &Message::Attach {
                version: 2,
                selector: String::new(),
            },
        );
        b.tick().await;
        phone_says(&link, 2, &Message::Resize { cols: 45, rows: 30 });
        b.tick().await;
        assert!(
            host.0.lock().unwrap().resized.is_empty(),
            "a phone that attached as version 2 did not ask for Resize"
        );
        // A resize the pane refuses leaves the grid, and sends no second Profile.
        phone_says(
            &link,
            3,
            &Message::Attach {
                version: VERSION,
                selector: String::new(),
            },
        );
        b.tick().await;
        let before = sent_messages(&link, 3).len();
        host.0.lock().unwrap().refuse_resize = true;
        phone_says(&link, 3, &Message::Resize { cols: 45, rows: 30 });
        b.tick().await;
        assert_eq!(sent_messages(&link, 3).len(), before);
    }

    #[tokio::test]
    async fn a_key_before_attach_or_from_a_version_one_phone_is_ignored() {
        let (mut b, host, link) = setup();
        phone_says(&link, 1, &Message::Key(vec!["Enter".into()]));
        b.tick().await;
        assert!(host.0.lock().unwrap().keyed.is_empty(), "not attached");
        phone_says(
            &link,
            2,
            &Message::Attach {
                version: 1,
                selector: String::new(),
            },
        );
        b.tick().await;
        phone_says(&link, 2, &Message::Key(vec!["Enter".into()]));
        b.tick().await;
        assert!(
            host.0.lock().unwrap().keyed.is_empty(),
            "a phone that attached as version 1 did not ask for Key"
        );
    }

    #[tokio::test]
    async fn the_profile_names_the_attached_shell_in_the_version_asked_for() {
        let (mut b, host, link) = setup();
        host.0.lock().unwrap().running = vec!["abcd-1".into()];
        phone_says(
            &link,
            1,
            &Message::Attach {
                version: VERSION,
                selector: String::new(),
            },
        );
        phone_says(
            &link,
            2,
            &Message::Attach {
                version: 1,
                selector: String::new(),
            },
        );
        b.tick().await;
        let sent = link.0.lock().unwrap().sent.clone();
        let first = |shell: u8| {
            let (_, bytes) = sent.iter().find(|(s, _)| *s == shell).unwrap();
            Decoder::new().push(bytes).unwrap().remove(0).unwrap()
        };
        // An empty selector: the host picked the shell, and says which.
        assert!(
            matches!(first(1), Message::Profile { version, shell, .. } if version == VERSION && shell == "abcd-1")
        );
        // A version-1 phone gets the five-byte Profile it can decode, with no shell.
        assert!(matches!(first(2), Message::Profile { version: 1, shell, .. } if shell.is_empty()));
        let (_, bytes) = sent.iter().find(|(s, _)| *s == 2).unwrap();
        assert_eq!(bytes.len(), 7, "tag, len, and a five-byte body");
    }

    #[tokio::test]
    async fn input_before_attach_is_ignored_and_never_reaches_a_shell() {
        let (mut b, host, link) = setup();
        phone_says(&link, 5, &Message::Input(b"rm -rf /\r".to_vec()));
        b.tick().await;
        assert!(host.0.lock().unwrap().typed.is_empty());
    }

    #[tokio::test]
    async fn a_new_handshake_drops_every_attach() {
        let (mut b, _host, link) = setup();
        phone_says(
            &link,
            1,
            &Message::Attach {
                version: VERSION,
                selector: String::new(),
            },
        );
        b.tick().await;
        assert!(b.attached(1));
        link.0.lock().unwrap().generation = 2;
        b.tick().await;
        assert!(!b.attached(1), "the phone must attach again");
    }

    #[tokio::test]
    async fn a_corrupt_stream_drops_the_attach_and_a_bad_message_does_not() {
        let (mut b, _host, link) = setup();
        phone_says(
            &link,
            1,
            &Message::Attach {
                version: VERSION,
                selector: String::new(),
            },
        );
        b.tick().await;
        // One malformed body (Closed with 2 bytes) costs one message only.
        link.0
            .lock()
            .unwrap()
            .inbound
            .push((1, vec![0x84, 0x02, 0, 0]));
        b.tick().await;
        assert!(b.attached(1));
        assert_eq!(b.stats.malformed, 1);
        // A length over the cap ends the attach.
        link.0
            .lock()
            .unwrap()
            .inbound
            .push((1, vec![0x02, 0xAD, 0x02]));
        b.tick().await;
        assert!(!b.attached(1));
    }

    #[tokio::test]
    async fn resync_resends_the_whole_screen() {
        let (mut b, _host, link) = setup();
        phone_says(
            &link,
            1,
            &Message::Attach {
                version: VERSION,
                selector: String::new(),
            },
        );
        b.tick().await;
        let n = link.0.lock().unwrap().sent.len();
        phone_says(&link, 1, &Message::Resync);
        b.tick().await;
        let after = link.0.lock().unwrap().sent[n..].to_vec();
        let mut d = Decoder::new();
        let first = d.push(&after[0].1).unwrap().remove(0).unwrap();
        assert!(
            matches!(first, Message::Profile { .. }),
            "a full screen starts with Profile"
        );
    }

    #[tokio::test]
    async fn a_dead_shell_is_reported_and_detached_and_an_unreadable_one_too() {
        let (mut b, host, link) = setup();
        phone_says(
            &link,
            1,
            &Message::Attach {
                version: VERSION,
                selector: String::new(),
            },
        );
        b.tick().await;
        let mut s = screen(&["bye", ""]);
        s.dead = true;
        host.0.lock().unwrap().screens.insert("abcd-1".into(), s);
        b.tick().await;
        assert!(!b.attached(1));
        assert_eq!(phone_view(&link, 1).closed, Some(closed::EXITED));

        let (mut b, host, link) = setup();
        phone_says(
            &link,
            1,
            &Message::Attach {
                version: VERSION,
                selector: String::new(),
            },
        );
        b.tick().await;
        host.0.lock().unwrap().fail_screen = true;
        b.tick().await;
        assert_eq!(phone_view(&link, 1).closed, Some(closed::UNAVAILABLE));
    }

    #[tokio::test]
    async fn no_session_means_no_attaches() {
        let (mut b, _host, link) = setup();
        phone_says(
            &link,
            1,
            &Message::Attach {
                version: VERSION,
                selector: String::new(),
            },
        );
        b.tick().await;
        link.0.lock().unwrap().established = false;
        b.tick().await;
        assert!(!b.attached(1));
    }
}

/// `LDMUX_MESH_TIMING=1` turns on the two timing lines in this file. Off by default: a log line
/// per keystroke is noise on a link that carries a few dozen records an hour.
fn timing() -> bool {
    std::env::var_os("LDMUX_MESH_TIMING").is_some_and(|v| v == "1")
}

/// The daemon's one timing clock, so a line here can be subtracted from a line in `mesh.rs`.
fn stamp() -> u128 {
    crate::mesh::runtime_stamp()
}
