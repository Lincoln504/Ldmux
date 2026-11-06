//! The shell bridge: the mesh terminal profile (ADR-0025) between a live shell and the L3
//! session.
//!
//! It reads **the keeper's grid** (the `snapshot` RPC, never raw PTY bytes), sends the phone
//! plain-text rows and then only the rows that changed, and writes the phone's `Input` to the
//! shell. It holds no socket: [`Link`] is the session (the runtime in [`crate::mesh`]) and
//! [`Host`] is the daemon, so every rule here is testable without either. Its only clock is a
//! field a test can move.
//!
//! Rules that matter:
//!
//! - **Keep the pipe full, never build a backlog.** A screen is read and diffed while the window
//!   has room, so an echo does not wait for the previous record's ack; the bound is the session's
//!   own window ([`PENDING_LIMIT`]), past which the queue would only grow. An earlier design sent
//!   one update at a time and measured 16 s from keystroke to echo.
//! - **A failed send resets the diff.** If any message of an update cannot be queued, the
//!   differ forgets what the phone holds, so the next update is a full screen. The phone is
//!   never left holding half of a change the host believes it delivered.
//! - **A new handshake drops every attach.** The phone must `Attach` again; nothing carries over
//!   a session that ended.
//! - **Nothing here knows a radio.** The section 13 grep over the daemon crates stays at zero hits.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use ldmux_protocol::keeper::SnapshotResult;
use ldmux_protocol::mesh_term::{
    CWD_VERSION, DecodeError, Decoder, Differ, KEY_VERSION, MIN_VERSION, Message, ORIGIN_VERSION,
    OWNER_VERSION, RESIZE_VERSION, SHELL_CONTROL_VERSION, STREAM_VERSION, Screen, VERSION, closed,
    session_pages,
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
    /// A handle that resolves when records may be waiting to be taken.
    ///
    /// Without one the timer alone decides when a keystroke is noticed, so the idle cadence is a
    /// direct delay on every inbound record. With one, the wait is "until a record arrives **or**
    /// the timer expires", and the timer can be long without costing interactivity. `None` keeps
    /// the timer-only behaviour, which is what the test link wants.
    fn incoming(&self) -> Option<Arc<tokio::sync::Notify>> {
        None
    }
}

/// The daemon as the bridge sees it.
#[allow(async_fn_in_trait)]
pub trait Host {
    /// Ids of the running shells. `None` when the host could not be asked: unknown is not empty.
    async fn running(&mut self) -> Option<Vec<String>>;
    /// `(id, label, cwd, created_by, holder)` of the running shells, for the phone's picker. The default has
    /// no labels, no paths and no origin; a host that knows where a shell runs gives the phone the
    /// folder and who started it, as the tailnet launcher does.
    async fn listing(&mut self) -> Option<Vec<(String, String, String, String, String)>> {
        Some(
            self.running()
                .await?
                .into_iter()
                .map(|id| (id, String::new(), String::new(), String::new(), String::new()))
                .collect(),
        )
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
    /// Take the shell for this link, displacing any other viewer.
    ///
    /// A desk window that believes it holds the shell re-asserts its own grid every time a
    /// snapshot disagrees with its tty, so without a claim the phone and the window spend the
    /// link repainting the pane at two sizes in turn: the phone sees an unchanged screen, a
    /// cursor in the wrong place and a resize that does not take. Claiming is what ADR-0010
    /// says a second viewer does, and it is also what makes the window stop: a displaced
    /// window is sent `owned`, not a snapshot, so it stops resizing.
    async fn claim(&mut self, id: &str) -> Result<bool, String>;
    /// Give the shell back, so a desk window can take it again.
    /// Give the shell back to nobody.
    ///
    /// Returns whether the host accepted it. A release that silently fails is the bug that once
    /// left every claim in place for ever (a device object where a string was expected), and it was
    /// invisible because the answer was discarded — so the answer is part of the contract now.
    async fn release(&mut self, id: &str) -> bool;

    /// Start a shell in `cwd` (empty = the host's default) and answer its id.
    ///
    /// This is the verb the mesh route was missing: without it a phone could type into the shells
    /// the desk had already started and could not start one of its own, which is the whole
    /// difference between the radio route and the tailnet launcher. The shell is recorded as the
    /// phone's (`created_by = "phone"`), so it stays the phone's after the app restarts.
    async fn create(&mut self, cwd: &str, cols: u8, rows: u8) -> Result<String, String> {
        let _ = (cwd, cols, rows);
        Err("this host cannot start shells".to_owned())
    }

    /// End that shell, or let go of it when a desk window holds it (ADR-0010).
    async fn close(&mut self, id: &str) -> Result<(), String> {
        let _ = id;
        Err("this host cannot end shells".to_owned())
    }

    /// Open that shell in a desk window on the host.
    async fn make_live(&mut self, id: &str) -> Result<(), String> {
        let _ = id;
        Err("this host cannot open desk windows".to_owned())
    }
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
    fn incoming(&self) -> Option<Arc<tokio::sync::Notify>> {
        Some(Arc::clone(&self.inbound_wake))
    }
}

/// What the bridge has done, for tests and for a future status field.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
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
    /// Shells the phone started, ended, and put on the desk (version 7).
    pub created: u32,
    pub closed: u32,
    pub made_live: u32,
    /// Listing or liveness questions the host could not answer. Counted, and answered with
    /// **silence**: an empty list is a fact ("no shells are running") and a failed call is not, so
    /// sending one for the other told the phone its shells had all ended (measured 2026-11-03: the
    /// launcher emptied on a drop while every shell was alive).
    pub host_unanswered: u32,
    /// Claims the host refused to give back. A claim that cannot be released keeps the shell held
    /// by a device that is not there, which is the state that stops a desk window reclaiming it.
    pub release_failed: u32,
}

/// What a dropped attach still owes the host: the shell to give back, and the grid to put back.
struct Detached {
    session: String,
    original: Option<(u8, u8)>,
    resized: bool,
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
    /// When the last screen update for this shell went out. Pacing needs it to know how long a
    /// coalesced change has been waiting.
    last_push: Option<Instant>,
    /// Whether the previous tick also found a change. A single step after quiet is sent at once
    /// (that is a command's output appearing); a screen that keeps changing is a burst and is
    /// held to [`BURST_GAP`].
    changed_last_tick: bool,
    /// The user typed into this shell and the echo has not been read yet. Input echo never
    /// waits for the pacing: the keystroke is what the user is looking for.
    echo: bool,
    /// **The grid this phone displaced, put back when it lets go.**
    ///
    /// A phone that attaches takes the shell and fits the pty to its own screen, which is right —
    /// the shell follows the viewer. What was missing is the other half: when the phone leaves, the
    /// pty stayed at the phone's width, so a desk window 158 columns wide was left drawing a 46
    /// column shell and the reader's own shell looked broken (reported 2026-11-05, after a phone
    /// attached to `meshmore-sns` and detached). The bridge records the size the host had before
    /// the first `Resize` and restores it on detach.
    original: Option<(u8, u8)>,
}

impl Attached {
    /// Whether a change on `shell` may be sent now.
    ///
    /// Two things are deliberately not throttled. A change seen after a tick with nothing to
    /// send is immediate, so a command's output appears the moment it exists. So is the echo of
    /// what the user typed. Everything else - a build printing, `yes`, a scrolling log - is a
    /// burst, and a burst is one update per [`BURST_GAP`] rather than one per 50 ms tick: on
    /// this link the preamble and header of a frame are a third of its airtime, so coalescing
    /// is the largest single saving available, and a record that carries the present screen is
    /// worth more than several that each carry a moment that has already passed.
    fn due(&self, now: Instant) -> bool {
        if self.echo || !self.changed_last_tick {
            return true;
        }
        self.last_push
            .is_none_or(|t| now.saturating_duration_since(t) >= BURST_GAP)
    }

    /// Start the screen over: the differ forgets what the phone holds, and so does the pacing.
    /// A `Resync`, an accepted `Resize` and a refused send are each "the phone's screen is not
    /// the one the last push left", so the next update is due the moment it exists - holding it
    /// would be pacing a burst that has already ended.
    fn resync(&mut self) {
        self.differ.reset();
        self.last_push = None;
        self.changed_last_tick = false;
        self.echo = false;
    }
}

pub struct Bridge<H, L> {
    host: H,
    link: L,
    shells: HashMap<u8, Attached>,
    decoders: HashMap<u8, Decoder>,
    /// The profile version of the last attach seen on a shell, refused or accepted. A `List` is
    /// answered before most attaches succeed, and its page format depends on the version the
    /// phone speaks (`CWD_VERSION`), so the version is remembered from the `Attach` that always
    /// precedes it rather than guessed.
    versions: HashMap<u8, u8>,
    generation: u32,
    /// The window was full on the previous pass. The first pass with room after a stall is a
    /// phone coming back, not a burst: its diff goes at once rather than waiting out a coalescing
    /// window that belongs to output it never saw.
    stalled: bool,
    pub stats: BridgeStats,
    /// Consecutive ticks that changed nothing. Drives [Bridge::interval_ms].
    quiet_ticks: u32,
    /// The pacing clock, a field rather than `Instant::now()` at the call site so a test can
    /// move time without sleeping.
    clock: Arc<dyn Fn() -> Instant + Send + Sync>,
}

impl<H: Host, L: Link> Bridge<H, L> {
    /// Give a claim back, and count a refusal.
    ///
    /// The bridge owns the counters, so the accounting lives here rather than in the host: a failed
    /// release is a state the operator needs to see, not a return value to drop on the floor.
    async fn release(&mut self, id: &str) {
        if !self.host.release(id).await {
            self.stats.release_failed = self.stats.release_failed.saturating_add(1);
        }
    }


    /// The counters so far. Copied, not borrowed: the caller publishes a value, never a handle.
    #[must_use]
    pub fn stats(&self) -> BridgeStats {
        self.stats
    }

    pub fn new(host: H, link: L) -> Self {
        Self {
            host,
            link,
            shells: HashMap::new(),
            decoders: HashMap::new(),
            versions: HashMap::new(),
            generation: 0,
            stalled: false,
            stats: BridgeStats::default(),
            quiet_ticks: 0,
            clock: Arc::new(Instant::now),
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
        let flush =
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
        let mut packed: usize = 0;
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
            packed += 1;
        }
        if ok {
            // **Counted after the send, not before it.** The flush can fail (a full queue, a link
            // that says no), and a counter incremented as messages were packed reported messages
            // that never left — the one number a user would read as "the phone got it".
            flush(&mut record, &mut ok, &mut self.link, &mut self.stats);
            if ok {
                self.stats.messages_sent = self.stats.messages_sent.saturating_add(packed as u32);
            }
            if timing() {
                eprintln!("ldmuxd: mesh timing: reply queued at {:?}", stamp());
            }
        }
        ok
    }

    /// How long to wait before the next pass.
    ///
    /// **Every pass costs one host round trip per attached shell**, and `DaemonHost::screen`
    /// takes the daemon-wide mutex across an await. At a fixed 50 ms that is twenty global-lock
    /// acquisitions per second per shell, paid whether or not the screen moved. A pass that
    /// changed nothing therefore earns a longer wait, and the first thing that changes puts the
    /// cadence back to the interactive one on the following tick.
    #[must_use]
    pub fn interval_ms(&self) -> u64 {
        if self.quiet_ticks < QUIET_AFTER_TICKS {
            TICK_MS
        } else {
            QUIET_TICK_MS
        }
    }

    /// One pass, counting whether it changed anything.
    pub async fn tick(&mut self) {
        let before = work(&self.stats);
        self.tick_once().await;
        if work(&self.stats) == before {
            self.quiet_ticks = self.quiet_ticks.saturating_add(1);
        } else {
            self.quiet_ticks = 0;
        }
    }

    /// One pass: take what the phone sent, then push what changed on screen.
    async fn tick_once(&mut self) {
        if !self.link.established() {
            self.drop_all().await;
            return;
        }
        let generation = self.link.generation();
        if generation != self.generation {
            self.generation = generation;
            self.drop_all().await;
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
                    if let Some(a) = self.shells.remove(&shell) {
                        self.release(&a.session).await;
                    }
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
                self.stalled = true;
                break;
            }
            if self.stalled {
                self.stalled = false;
                // The phone is back after a full window. Nothing it missed is worth pacing
                // against: send the present screen now.
                for a in self.shells.values_mut() {
                    a.last_push = None;
                    a.changed_last_tick = false;
                }
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
            // Before this phone has fitted anything, the pty's size is whatever the desk left it
            // at: that is the size to put back. Recorded once, and only before our own `Resize`
            // has been applied, so a later screen cannot overwrite it with the phone's grid.
            if let Some(a) = self.shells.get_mut(&shell)
                && a.original.is_none()
                && a.geometry.is_none()
            {
                a.original = Some((screen.cols, screen.rows));
            }
            if self.shells.get(&shell).is_some_and(|a| a.differ.up_to_date(&screen)) {
                if let Some(a) = self.shells.get_mut(&shell) {
                    a.changed_last_tick = false;
                }
                continue;
            }
            // **Pacing is for bursts, not for every change.** The echo of input and the first
            // change after a quiet tick go at once; a screen that keeps changing is held to
            // [`BURST_GAP`], because on this link a record carrying the present screen is worth
            // more than several carrying moments that have already passed.
            let now = (self.clock)();
            if !self.shells.get(&shell).is_some_and(|a| a.due(now)) {
                if let Some(a) = self.shells.get_mut(&shell) {
                    a.changed_last_tick = true;
                }
                continue;
            }
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
                if let Some(a) = self.shells.get_mut(&shell) {
                    a.changed_last_tick = false;
                }
                continue;
            }
            self.stats.updates += 1;
            if !self.reply(shell, &messages) {
                if let Some(a) = self.shells.get_mut(&shell) {
                    a.resync();
                }
                continue;
            }
            if let Some(a) = self.shells.get_mut(&shell) {
                a.last_push = Some(now);
                a.changed_last_tick = true;
                a.echo = false;
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

    /// Forget every attach, handing each shell back to whoever held it before.
    ///
    /// A link that re-handshakes, or a stream that cannot be decoded, ends the attach as
    /// surely as `Detach` does, and a claim left behind would lock a desk window out of a
    /// shell nobody is watching.
    async fn drop_all(&mut self) {
        let held: Vec<Detached> = self
            .shells
            .values()
            .map(|a| Detached {
                session: a.session.clone(),
                original: a.original,
                resized: a.geometry.is_some(),
            })
            .collect();
        self.shells.clear();
        self.decoders.clear();
        self.versions.clear();
        for a in &held {
            self.release(&a.session).await;
        }
        for a in &held {
            if let Some((cols, rows)) = a.original.filter(|_| a.resized) {
                let _ = self.host.resize(&a.session, cols, rows).await;
            }
        }
    }

    /// `Attach`, with or without the consent that displaces whoever holds the shell.
    async fn attach_with(&mut self, shell: u8, version: u8, selector: &str, takeover: bool) {
                if !(MIN_VERSION..=VERSION).contains(&version) {
                    self.reply(
                        shell,
                        &[Message::Closed {
                            reason: closed::VERSION,
                        }],
                    );
                    return;
                }
                // Remembered even when the attach itself is refused: the phone answers a refusal
                // with a `List`, and that answer must be in the version this phone speaks.
                self.versions.insert(shell, version);
                // The holder matters as much as the id: a shell a desk window is watching is
                // refused rather than stolen, which is what the tailnet route has always done
                // (`HeldBy`, then a takeover the user chooses). Silence would be worse — the phone
                // would sit on a screen nobody is feeding.
                let listing = self.host.listing().await;
                let Some(running) = listing.as_ref().map(|rows| {
                    rows.iter().map(|r| r.0.clone()).collect::<Vec<_>>()
                }) else {
                    // Unknown is not "not found": answering `Closed` here told the phone the
                    // shell had ended when the host had merely failed to answer. Stay silent and
                    // let the phone ask again.
                    self.stats.host_unanswered += 1;
                    return;
                };
                let matches: Vec<&String> = running
                    .iter()
                    .filter(|id| selector.is_empty() || id.starts_with(selector))
                    .collect();
                // A shell another device holds, and this is a plain Attach: ask first.
                if let Some(held) = matches.first() {
                    let holder = listing
                        .as_ref()
                        .and_then(|rows| rows.iter().find(|r| r.0 == ***held))
                        .map(|r| r.4.as_str())
                        .unwrap_or("");
                    if !takeover && !holder.is_empty() && holder != "mesh" {
                        self.reply(shell, &[Message::Closed { reason: closed::HELD }]);
                        return;
                    }
                }
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
                        if self.host.claim(one).await.is_err() {
                            self.reply(
                                shell,
                                &[Message::Closed {
                                    reason: closed::UNAVAILABLE,
                                }],
                            );
                            return;
                        }
                        // **A re-attach replaces the claim; it must also release the old one.**
                        // A phone that attaches twice on one channel number (a reconnect that
                        // raced its own `Detach`) used to overwrite the map entry and leave the
                        // previous session claimed by a viewer that no longer exists — a desk
                        // window then could not take the shell back, and nothing would release it
                        // short of the link dropping. Releasing first is what makes the claim
                        // count match the viewers.
                        if let Some(old) = self.shells.remove(&shell)
                            && old.session != **one
                        {
                            self.release(&old.session).await;
                        }
                        // **The size to put back is read here, before the phone can fit.**
                        // Recorded on the first tick instead, it was already the phone's grid when
                        // the phone's `Resize` won the race, so the restore either put back the
                        // phone's own size or (when the fit had set `geometry`) was skipped
                        // entirely — measured 2026-11-05: a desk shell left at 46x35 after the
                        // phone returned to its list.
                        let original = self
                            .host
                            .screen(one)
                            .await
                            .ok()
                            .map(|s| (s.cols, s.rows));
                        self.shells.insert(
                            shell,
                            Attached {
                                session: (*one).clone(),
                                // Answered in the version asked for, naming the shell the host
                                // chose, so a phone that sent an empty selector learns which.
                                differ: Differ::for_attach(version, one),
                                version,
                                geometry: None,
                                last_push: None,
                                changed_last_tick: false,
                                echo: false,
                                original,
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

    async fn handle(&mut self, shell: u8, m: Message) {
        match m {
            Message::Attach { version, selector } => {
                self.attach_with(shell, version, &selector, false).await;
            }
            // **Taking a shell from the desk is the user's decision, so it is a message of its
            // own.** The version is the one this phone attached with: a takeover is not a place to
            // renegotiate the protocol.
            Message::Takeover { selector } => {
                let version = self.versions.get(&shell).copied().unwrap_or(0);
                if version < OWNER_VERSION {
                    self.reply(shell, &[Message::Closed { reason: closed::VERSION }]);
                    return;
                }
                self.attach_with(shell, version, &selector, true).await;
            }
            Message::List => {
                let Some(listing) = self.host.listing().await else {
                    // An empty page means "no shells are running", which is a fact. A failed call
                    // is not that fact, and sending it emptied the launcher while every shell was
                    // alive.
                    self.stats.host_unanswered += 1;
                    return;
                };
                // A version-5 phone decodes the version-5 form; only a version-6 phone is given
                // the folder as well. No attach means no version, and no version means the old
                // form: a page the phone cannot read is worse than one without a path.
                let version = self.versions.get(&shell).copied();
                let with_cwd = version.is_some_and(|v| v >= CWD_VERSION);
                // The origin rides the same flag-byte mechanism as the path, for the same reason:
                // a list is answered before any attach has a profile to gate on.
                let with_origin = version.is_some_and(|v| v >= ORIGIN_VERSION);
                let with_owner = version.is_some_and(|v| v >= OWNER_VERSION);
                self.reply(
                    shell,
                    &session_pages(&listing, with_cwd, with_origin, with_owner),
                );
            }
            // ── version 7: the phone starts and ends shells, as the tailnet client does ──
            Message::Create { version, cwd } => {
                self.versions.insert(shell, version);
                if version < SHELL_CONTROL_VERSION {
                    self.reply(shell, &[Message::Closed { reason: closed::VERSION }]);
                    return;
                }
                // The phone's own grid, when it has said one, so the shell is born the size the
                // phone will draw rather than 80x24 and reflowed on the first attach.
                let (cols, rows) = self
                    .shells
                    .get(&shell)
                    .and_then(|a| a.geometry)
                    .unwrap_or((80, 24));
                match self.host.create(&cwd, cols, rows).await {
                    Ok(id) => {
                        self.stats.created += 1;
                        self.reply(shell, &[Message::Created { id }]);
                    }
                    // A host that cannot start one says so; the phone must not sit waiting.
                    Err(_) => {
                        self.stats.host_unanswered += 1;
                        self.reply(shell, &[Message::Closed { reason: closed::UNAVAILABLE }]);
                    }
                }
            }
            Message::Close { selector } => {
                let version = self.versions.get(&shell).copied().unwrap_or(0);
                if version < SHELL_CONTROL_VERSION {
                    self.reply(shell, &[Message::Closed { reason: closed::VERSION }]);
                    return;
                }
                let Some(id) = self.resolve(shell, &selector).await else {
                    return;
                };
                // **A shell another device holds is not this phone's to end.** The tailnet route
                // refuses the same press (`remote.rs`, `not_holder`) and offers a takeover; the
                // radio route called the daemon's bare `close`, which has no holder rule, so the
                // same button that was refused over the network destroyed a shell a desk window was
                // working in. Two routes doing one thing is the whole point of having two routes.
                let holder = self
                    .host
                    .listing()
                    .await
                    .and_then(|rows| rows.into_iter().find(|r| r.0 == id).map(|r| r.4))
                    .unwrap_or_default();
                if !holder.is_empty() && holder != "mesh" {
                    self.reply(shell, &[Message::Closed { reason: closed::HELD }]);
                    return;
                }
                if self.host.close(&id).await.is_err() {
                    self.stats.host_unanswered += 1;
                    return;
                }
                self.stats.closed += 1;
                // The list the phone shows is now wrong. **Release only the attach that named
                // this shell**: `shells` is keyed by channel, so removing it unconditionally
                // detached whatever else that channel was attached to and released *its* claim,
                // leaving a live attachment with no owner.
                if let Some(a) = self.shells.get(&shell)
                    && a.session == id
                {
                    let session = a.session.clone();
                    self.shells.remove(&shell);
                    self.release(&session).await;
                }
                if let Some(listing) = self.host.listing().await {
                    let with_cwd = version >= CWD_VERSION;
                    let with_origin = version >= ORIGIN_VERSION;
                    let with_owner = version >= OWNER_VERSION;
                    self.reply(
                        shell,
                        &session_pages(&listing, with_cwd, with_origin, with_owner),
                    );
                }
            }
            Message::MakeLive { selector } => {
                let version = self.versions.get(&shell).copied().unwrap_or(0);
                if version < SHELL_CONTROL_VERSION {
                    self.reply(shell, &[Message::Closed { reason: closed::VERSION }]);
                    return;
                }
                let Some(id) = self.resolve(shell, &selector).await else {
                    return;
                };
                if self.host.make_live(&id).await.is_ok() {
                    self.stats.made_live += 1;
                } else {
                    self.stats.host_unanswered += 1;
                }
            }
            Message::Input(bytes) => {
                let Some(session) = self.shells.get(&shell).map(|a| a.session.clone()) else {
                    return;
                };
                self.stats.inputs += 1;
                if let Some(a) = self.shells.get_mut(&shell) {
                    a.echo = true;
                }
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
                if let Some(a) = self.shells.get_mut(&shell) {
                    a.echo = true;
                }
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
                let accepted = self.host.resize(&session, cols, rows).await.is_ok();
                if accepted && let Some(a) = self.shells.get_mut(&shell) {
                    a.geometry = Some((cols, rows));
                    a.resync();
                }
            }
            Message::Resync => {
                if let Some(a) = self.shells.get_mut(&shell) {
                    a.resync();
                }
            }
            Message::Detach => {
                if let Some(a) = self.shells.remove(&shell) {
                    self.release(&a.session).await;
                    self.restore_grid(&a).await;
                }
            }
            // Host-to-phone messages arriving from the phone are ignored.
            _ => {}
        }
    }
}

impl<H: Host, L: Link> Bridge<H, L> {
    /// The session a selector names, or `None` when it names nothing running.
    ///
    /// The same rule `Attach` uses: an empty selector means the shell this link is attached to,
    /// and anything else is an id prefix. Nothing is guessed: a selector that matches no running
    /// shell answers nothing, because a close aimed at the wrong shell is not recoverable.
    async fn resolve(&mut self, shell: u8, selector: &str) -> Option<String> {
        if selector.is_empty() {
            return self.shells.get(&shell).map(|a| a.session.clone());
        }
        let running = self.host.running().await?;
        running
            .into_iter()
            .find(|id| id.starts_with(selector))
    }
}

impl<H: Host, L: Link> Bridge<H, L> {
    /// Put the shell's pty back where the phone found it, when the phone leaves.
    ///
    /// Only after a fit of ours: a shell the phone merely looked at must not be resized on the way
    /// out, and a call that changes nothing is still a keeper round trip.
    async fn restore_grid(&mut self, attached: &Attached) {
        if attached.geometry.is_none() {
            return;
        }
        if let Some((cols, rows)) = attached.original {
            let _ = self.host.resize(&attached.session, cols, rows).await;
        }
    }
}

/// The identity the mesh link claims shells under. One mesh link is one viewer: the session
/// carries one phone key, and a second phone means a new pairing.
const MESH_DEVICE: &str = "mesh-link";

/// The daemon as a [`Host`]: every call is an RPC the control socket already serves.
pub struct DaemonHost(pub Arc<tokio::sync::Mutex<crate::daemon::LdmuxDaemon>>);

impl Host for DaemonHost {
    async fn running(&mut self) -> Option<Vec<String>> {
        let v = self
            .0
            .lock()
            .await
            .handle("list", serde_json::json!({}))
            .await
            .ok()?;
        let ids = v
            .get("sessions")
            .and_then(|s| s.as_array())
            .map(|a| {
                a.iter()
                    .filter(|s| s.get("status").and_then(|x| x.as_str()) == Some("running"))
                    .filter_map(|s| s.get("id").and_then(|x| x.as_str()).map(str::to_owned))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        Some(ids)
    }

    async fn listing(&mut self) -> Option<Vec<(String, String, String, String, String)>> {
        let v = self
            .0
            .lock()
            .await
            .handle("list", serde_json::json!({}))
            .await
            .ok()?;
        let rows = v
            .get("sessions")
            .and_then(|s| s.as_array())
            .map(|a| {
                a.iter()
                    .filter(|s| s.get("status").and_then(|x| x.as_str()) == Some("running"))
                    .filter_map(|s| {
                        let id = s.get("id").and_then(|x| x.as_str())?;
                        let label = s.get("title").and_then(|x| x.as_str()).unwrap_or("");
                        // The daemon's `list` already describes the pane's live directory (the
                        // keeper's fact, not the directory the session was created in).
                        let cwd = s.get("cwd").and_then(|x| x.as_str()).unwrap_or("");
                        // Who started it (`SessionInfo.created_by`): "phone", "desktop", "local".
                        // The mesh launcher shows the phone's own shells' actions, so it needs the
                        // same fact the tailnet launcher has always had.
                        let origin = s.get("createdBy").and_then(|x| x.as_str()).unwrap_or("");
                        // Who holds it now (`owner.kind`): `desktop`, `phone`, `mesh`, or absent.
                        // A shell with a holder is one this phone must ask to take.
                        let holder = s
                            .get("owner")
                            .and_then(|o| o.get("kind"))
                            .and_then(|x| x.as_str())
                            .unwrap_or("");
                        Some((
                            id.to_owned(),
                            label.to_owned(),
                            cwd.to_owned(),
                            origin.to_owned(),
                            holder.to_owned(),
                        ))
                    })
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        Some(rows)
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
            // The keeper numbers its lines: history first, then the live screen. A keeper too
            // old to say reports nothing, and a phone treats that as "no line identity".
            s.total_rows.saturating_sub(s.rows).min(u64::from(u32::MAX)) as u32,
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

    async fn claim(&mut self, id: &str) -> Result<bool, String> {
        let v = self
            .0
            .lock()
            .await
            .handle(
                "reclaim",
                serde_json::json!({
                    "id": id,
                    "device": { "id": MESH_DEVICE, "name": "the mesh link", "kind": "mesh" },
                }),
            )
            .await?;
        Ok(v.get("ok").and_then(|b| b.as_bool()).unwrap_or(false))
    }

    async fn release(&mut self, id: &str) -> bool {
        let answer = self
            .0
            .lock()
            .await
            // A bare device id, not the device object `reclaim` takes: the daemon's `release`
            // reads `device` as a string, so the object the bridge used to send parsed as `""` and
            // every release returned `{"ok":false}` — the phone let go and the claim was kept for
            // ever, which is why the desk window never got its shell (and its size) back.
            .handle("release", serde_json::json!({ "id": id, "device": MESH_DEVICE }))
            .await;
        match answer {
            Ok(_) => true,
            Err(e) => {
                // Said out loud here; counted by the bridge, which owns the counters. A claim that
                // cannot be given back keeps a desk window from ever reclaiming the shell, and the
                // bug that caused it once (a device object where a string was expected) was silent
                // because this answer was discarded.
                eprintln!("ldmuxd: mesh: release {id} refused by the host: {e}");
                false
            }
        }
    }

    /// The same `new` the tailnet's WebSocket handler calls, with `createdBy` set to the phone so the
    /// record says who started it. That field is what the launcher reads to keep a phone-opened
    /// shell's actions (close, open on the desktop) after the app restarts, instead of letting the
    /// row pass for a desk shell.
    async fn create(&mut self, cwd: &str, cols: u8, rows: u8) -> Result<String, String> {
        let cwd = if cwd.is_empty() { "." } else { cwd };
        let v = self
            .0
            .lock()
            .await
            .handle(
                "new",
                serde_json::json!({
                    "kind": "shell",
                    "cwd": cwd,
                    "createdBy": "phone",
                    "cols": cols,
                    "rows": rows,
                }),
            )
            .await?;
        v.get("session")
            .and_then(|s| s.get("id"))
            .and_then(|x| x.as_str())
            .map(str::to_owned)
            .ok_or_else(|| "the host did not say which shell it made".to_owned())
    }

    /// The daemon's `close`: a shell a desk window holds is put down, one nobody holds is ended.
    /// Destroying a desk shell from a phone is not this app's decision to make (ADR-0010).
    async fn close(&mut self, id: &str) -> Result<(), String> {
        self.0
            .lock()
            .await
            .handle("close", serde_json::json!({ "id": id }))
            .await
            .map(|_| ())
    }

    async fn make_live(&mut self, id: &str) -> Result<(), String> {
        self.0
            .lock()
            .await
            .handle("makeLive", serde_json::json!({ "id": id, "workspace": "" }))
            .await
            .map(|_| ())
    }
}

/// How often the bridge looks at the screen. Interactive echo wants this small, the airtime
/// bound does not care: an update waits for an empty queue regardless.
pub const TICK_MS: u64 = 50;

/// The wait after a pass that changed nothing. A moved screen resets the cadence to
/// [`TICK_MS`] on the next tick, so interactive work is never paced by this.
///
/// Three times the structural cost removed for a first-keystroke echo delayed by at most 150 ms
/// instead of 50, measured from the moment the bridge notices the input rather than from the
/// keystroke, which is what the phone already waits for.
pub const QUIET_TICK_MS: u64 = 150;

/// Passes with no change before the cadence relaxes. At 50 ms this is 400 ms of quiet, which is
/// longer than one keystroke interval, so typing keeps the interactive cadence throughout.
pub const QUIET_AFTER_TICKS: u32 = 8;

/// Everything a pass can bump. Any of them moving means the pass did work.
fn work(stats: &BridgeStats) -> u64 {
    u64::from(stats.updates)
        + u64::from(stats.messages_sent)
        + u64::from(stats.records_sent)
        + u64::from(stats.inputs)
        + u64::from(stats.keys)
        + u64::from(stats.resizes)
        + u64::from(stats.malformed)
        + u64::from(stats.refused_sends)
        + u64::from(stats.host_unanswered)
}

/// How many unacked records may be in flight before the bridge stops looking at the screen. It is
/// the session's own window, so the queue can never grow past what the link can carry.
pub const PENDING_LIMIT: usize = 8;

/// The most often a screen that keeps changing is sent, unless the change is the echo of input.
///
/// This is a coalescing window, not a delay added to every update: a change that arrives after a
/// quiet tick goes at once, so a command's output is as fast as the link allows. Continuous
/// output (a build, `yes`, a log) is one update a second instead of twenty, which on a link
/// where a short frame's preamble and header are a third of its airtime is the difference
/// between the screen being usable and the queue never draining. It is also the hard bound on
/// staleness: nothing coalesced waits longer than this.
pub const BURST_GAP: Duration = Duration::from_millis(1000);

/// Wait until either the link has something to say or the cadence expires.
///
/// **The timer is the fallback, not the mechanism.** A record the phone sent wakes the bridge the
/// moment the session queues it, so the cadence only decides how soon a *screen* change is noticed.
/// That is what lets the idle cadence be long without putting its length on the path of a
/// keystroke, which is the one latency the user feels directly.
async fn wait_for_work<H: Host, L: Link>(bridge: &Bridge<H, L>) {
    let ms = bridge.interval_ms();
    match bridge.link.incoming() {
        Some(wake) => {
            tokio::select! {
                () = tokio::time::sleep(std::time::Duration::from_millis(ms)) => {}
                () = wake.notified() => {}
            }
        }
        None => tokio::time::sleep(std::time::Duration::from_millis(ms)).await,
    }
}

/// Run the bridge for the life of the process.
pub async fn run<H: Host, L: Link>(mut bridge: Bridge<H, L>) {
    loop {
        wait_for_work(&bridge).await;
        bridge.tick().await;
    }
}

/// As [`run`], and report the counters after every tick.
///
/// The report is a copy of ten `u32`s, once per tick: cheap enough not to be worth a lock
/// discipline, and it is what puts the bridge's numbers in `mesh status`.
pub async fn run_publishing<H: Host, L: Link>(
    mut bridge: Bridge<H, L>,
    publish: impl Fn(BridgeStats),
) {
    loop {
        wait_for_work(&bridge).await;
        bridge.tick().await;
        publish(bridge.stats());
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
        /// Every claim, in order: `("claim" | "release", session)`.
        claimed: Vec<(&'static str, String)>,
        refuse_resize: bool,
        /// The host refuses to give a claim back, as a daemon that no longer knows the device would.
        refuse_release: bool,
        fail_screen: bool,
        /// The host cannot answer a listing or liveness question at all.
        fail_list: bool,
        /// What `create` was asked for, and what it answers.
        created: Vec<(String, u8, u8)>,
        closed: Vec<String>,
        made_live: Vec<String>,
        /// When set, `create` fails the way a host with no room would.
        fail_create: bool,
        /// Who holds a shell, as `owner.kind` would say it: `desktop`, `phone`, or absent.
        holders: HashMap<String, String>,
    }
    #[derive(Clone, Default)]
    struct FakeHost(Arc<Mutex<HostState>>);
    impl Host for FakeHost {
        async fn running(&mut self) -> Option<Vec<String>> {
            let s = self.0.lock().unwrap();
            if s.fail_list {
                return None;
            }
            Some(s.running.clone())
        }
        async fn listing(&mut self) -> Option<Vec<(String, String, String, String, String)>> {
            let s = self.0.lock().unwrap();
            if s.fail_list {
                return None;
            }
            Some(
                s.running
                    .iter()
                    .map(|id| {
                        (
                            id.clone(),
                            format!("label of {id}"),
                            format!("/work/{id}"),
                            "phone".to_string(),
                            s.holders.get(id).cloned().unwrap_or_default(),
                        )
                    })
                    .collect(),
            )
        }
        async fn create(&mut self, cwd: &str, cols: u8, rows: u8) -> Result<String, String> {
            let mut s = self.0.lock().unwrap();
            if s.fail_create {
                return Err("no room".into());
            }
            s.created.push((cwd.to_owned(), cols, rows));
            let id = format!("new-{}", s.created.len());
            s.running.push(id.clone());
            Ok(id)
        }

        async fn close(&mut self, id: &str) -> Result<(), String> {
            let mut s = self.0.lock().unwrap();
            s.closed.push(id.to_owned());
            s.running.retain(|r| r != id);
            Ok(())
        }

        async fn make_live(&mut self, id: &str) -> Result<(), String> {
            self.0.lock().unwrap().made_live.push(id.to_owned());
            Ok(())
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
        async fn claim(&mut self, id: &str) -> Result<bool, String> {
            self.0
                .lock()
                .unwrap()
                .claimed
                .push(("claim", id.to_owned()));
            Ok(true)
        }
        async fn release(&mut self, id: &str) -> bool {
            let mut s = self.0.lock().unwrap();
            s.claimed.push(("release", id.to_owned()));
            // A double that cannot fail cannot be used to test the failure, and the failure is the
            // one that once left every claim in place for ever without a word.
            !s.refuse_release
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
            first_line: 0,
            lines: lines.iter().map(|s| (*s).to_owned()).collect(),
        }
    }

    /// A clock that is always well past any coalescing window, so a legacy test's "next tick"
    /// is a tick that sends.
    fn unpaced_now() -> Instant {
        static BASE: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
        static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        *BASE.get_or_init(Instant::now)
            + Duration::from_millis(N.fetch_add(2_000, std::sync::atomic::Ordering::Relaxed))
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
        let mut b = Bridge::new(host.clone(), link.clone());
        // The tests here predate pacing and each asserts "this change goes on the next tick".
        // A clock that steps past the coalescing window on every read keeps them about what
        // they are about; the pacing tests install a clock they control.
        b.clock = Arc::new(unpaced_now);
        (b, host, link)
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

    /// A desk window that believes it holds a shell re-asserts its own grid whenever a
    /// snapshot disagrees, so an attaching phone that does not take the shell fights it for
    /// ever: every resize is undone and every undo repaints the pane. The claim is what ends
    /// that, and a detach must hand the shell back or a window is locked out of a shell
    /// nobody is watching.
    #[tokio::test]
    async fn an_attach_takes_the_shell_and_a_detach_gives_it_back() {
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
        assert_eq!(
            *host.0.lock().unwrap().claimed,
            [("claim", "abcd-1".to_string())],
            "the phone took the shell before it was given a screen"
        );
        phone_says(&link, 3, &Message::Detach);
        b.tick().await;
        assert_eq!(
            *host.0.lock().unwrap().claimed,
            [
                ("claim", "abcd-1".to_string()),
                ("release", "abcd-1".to_string())
            ],
            "the detached shell went back to whoever held it"
        );
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

    /// A host that could not answer is not a host with no shells.
    ///
    /// The phone draws `Sessions` as the whole list, so an empty page from a failed keeper call
    /// emptied the launcher while every shell was alive (measured 2026-11-03, on a link drop). The
    /// distinction the protocol can carry is silence: no page, so the phone keeps what it has.
    #[tokio::test]
    async fn a_host_that_cannot_answer_says_nothing_rather_than_none() {
        let (mut b, host, link) = setup();
        host.0.lock().unwrap().fail_list = true;
        phone_says(&link, 4, &Message::List);
        b.tick().await;
        assert!(
            sessions_seen(&link, 4).is_empty(),
            "an unanswerable listing must not be sent as an empty list"
        );
        assert_eq!(b.stats.host_unanswered, 1);

        // An attach against the same failure must not pretend the shell is gone either: no
        // `Closed`, so the phone retries instead of telling the user it ended.
        phone_says(
            &link,
            5,
            &Message::Attach {
                version: 6,
                selector: "ab".into(),
            },
        );
        b.tick().await;
        let mut d = Decoder::new();
        let replies: Vec<_> = link
            .0
            .lock()
            .unwrap()
            .sent
            .iter()
            .filter(|(s, _)| *s == 5)
            .flat_map(|(_, bytes)| d.push(bytes).unwrap())
            .flatten()
            .collect();
        assert!(
            !replies
                .iter()
                .any(|m| matches!(m, Message::Closed { .. })),
            "unknown is not not-found: {replies:?}"
        );
        assert_eq!(b.stats.host_unanswered, 2);
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
                let Message::Sessions {
                    last,
                    cwd,
                    origin: _,
                    owner: _,
                    entries,
                } = m
                else {
                    unreachable!()
                };
                assert_eq!(*last, i + 1 == pages.len());
                // No attach was seen on this shell, so the host has no version to trust and
                // answers in the oldest form it speaks.
                assert!(!*cwd, "n={n}: an unattached shell gets no path");
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

    /// A list is answered before an attach can succeed, so its page carries its own format flag.
    /// A version-5 phone keeps the version-5 form (no path on the wire) and a version-6 phone is
    /// given the folder the shell runs in, which is what its picker shows instead of the id.
    #[tokio::test]
    async fn a_list_carries_the_folder_only_to_a_phone_that_speaks_version_six() {
        for (version, expect_cwd) in [(5u8, false), (CWD_VERSION, true)] {
            let (mut b, _host, link) = setup();
            // Any attach records the version, refused or not: a list whose shell was not known
            // is exactly the ambiguous case that made the phone ask.
            phone_says(
                &link,
                4,
                &Message::Attach {
                    version,
                    selector: "nope".into(),
                },
            );
            b.tick().await;
            phone_says(&link, 4, &Message::List);
            b.tick().await;
            let Message::Sessions {
                last,
                cwd,
                origin: _,
                owner: _,
                entries,
            } = &sessions_seen(&link, 4)[0]
            else {
                panic!("a list must answer with a page")
            };
            assert!(*last);
            assert_eq!(*cwd, expect_cwd, "version {version}");
            assert_eq!(entries[0].id, "abcd-1");
            assert_eq!(
                entries[0].cwd,
                if expect_cwd { "/work/abcd-1" } else { "" },
                "version {version}: the path is on the wire only for a version-6 phone"
            );
        }
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

    // ── pacing ────────────────────────────────────────────────────────────────────────────

    /// A clock a test can move. `Instant` cannot be constructed, so the fake is an origin plus
    /// whatever the test has advanced the counter to, and it is per-test so the suite can run
    /// these in parallel.
    #[derive(Clone)]
    struct FakeClock(Arc<(std::sync::atomic::AtomicU64, Instant)>);

    impl FakeClock {
        fn new() -> Self {
            Self(Arc::new((std::sync::atomic::AtomicU64::new(0), Instant::now())))
        }

        fn advance(&self, ms: u64) {
            self.0 .0.fetch_add(ms, std::sync::atomic::Ordering::Relaxed);
        }

        fn reader(&self) -> Arc<dyn Fn() -> Instant + Send + Sync> {
            let c = self.clone();
            Arc::new(move || c.0 .1 + Duration::from_millis(c.0 .0.load(std::sync::atomic::Ordering::Relaxed)))
        }
    }

    fn paced(host: &FakeHost, lines: &[&str]) {
        host.0
            .lock()
            .unwrap()
            .screens
            .insert("abcd-1".into(), screen(lines));
    }

    async fn attached_paced() -> (Bridge<FakeHost, FakeLink>, FakeHost, FakeLink, FakeClock) {
        let (mut b, host, link) = setup();
        let clock = FakeClock::new();
        b.clock = clock.reader();
        b.tick().await;
        phone_says(
            &link,
            1,
            &Message::Attach {
                version: VERSION,
                selector: String::new(),
            },
        );
        b.tick().await;
        // The attach's full screen is a push like any other; start the test after the window it
        // opened has closed, so "a step after quiet" is what the first change really is.
        clock.advance(2_000);
        b.tick().await;
        (b, host, link, clock)
    }

    fn sent_records(link: &FakeLink) -> usize {
        link.0.lock().unwrap().sent.len()
    }

    #[tokio::test]
    async fn a_burst_is_coalesced_to_one_update_a_second() {
        let (mut b, host, link, clock) = attached_paced().await;
        let base = sent_records(&link);

        clock.advance(50);
        paced(&host, &["one", ""]);
        b.tick().await;
        assert_eq!(sent_records(&link) - base, 1, "a step after quiet is immediate");

        // Inside the window that push opened, further changes are held. Nothing is lost: the
        // differ still holds the present screen, so the next allowed update carries it.
        clock.advance(100);
        paced(&host, &["two", ""]);
        b.tick().await;
        assert_eq!(sent_records(&link) - base, 1, "held inside the burst window");

        clock.advance(100);
        paced(&host, &["three", ""]);
        b.tick().await;
        assert_eq!(sent_records(&link) - base, 1, "still held");

        // A second after the last push: whatever the screen says now goes, once.
        clock.advance(800);
        paced(&host, &["four", ""]);
        b.tick().await;
        assert_eq!(sent_records(&link) - base, 2, "one update for the whole burst");
        assert_eq!(
            phone_view(&link, 1).lines,
            ["four", ""],
            "the held burst coalesces into the present screen, not a replay"
        );
    }

    #[tokio::test]
    async fn input_echo_is_never_paced() {
        let (mut b, host, link, clock) = attached_paced().await;
        let base = sent_records(&link);

        // Put the link into a burst: a change just went out.
        clock.advance(50);
        paced(&host, &["before", ""]);
        b.tick().await;
        assert_eq!(sent_records(&link) - base, 1);

        // The user types; the shell answers inside the same window.
        clock.advance(60);
        phone_says(&link, 1, &Message::Input(b"x".to_vec()));
        paced(&host, &["beforex", ""]);
        b.tick().await;
        assert_eq!(
            sent_records(&link) - base,
            2,
            "the echo goes now, not at the next window boundary"
        );
    }

    #[tokio::test]
    async fn a_quiet_tick_ends_the_burst() {
        let (mut b, host, link, clock) = attached_paced().await;
        let base = sent_records(&link);

        clock.advance(50);
        paced(&host, &["a", ""]);
        b.tick().await;
        b.tick().await; // the phone holds the screen: the burst is over
        assert_eq!(sent_records(&link) - base, 1);

        clock.advance(60);
        paced(&host, &["abc", ""]);
        b.tick().await;
        assert_eq!(
            sent_records(&link) - base,
            2,
            "a discrete change after quiet is immediate even inside the window"
        );
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
    /// The cadence relaxes only after a pass that changed nothing, and the next thing that
    /// changes puts it back. On an idle link this removes two of every three host round trips
    /// without pacing interactive work: an inbound record wakes the bridge on its own.
    #[tokio::test]
    async fn a_quiet_link_relaxes_the_cadence_and_work_restores_it() {
        let (mut b, _host, link) = setup();
        assert_eq!(b.interval_ms(), TICK_MS);
        b.quiet_ticks = QUIET_AFTER_TICKS;
        assert_eq!(b.interval_ms(), QUIET_TICK_MS);
        // A tick that delivers something resets the counter, which restores the fast cadence.
        phone_says(
            &link,
            1,
            &Message::Attach {
                version: VERSION,
                selector: String::new(),
            },
        );
        b.tick().await;
        assert_eq!(b.quiet_ticks, 0);
        assert_eq!(b.interval_ms(), TICK_MS);
    }

    /// A pass with nothing to do must not reset the counter, or the cadence could never relax.
    #[tokio::test]
    async fn an_empty_pass_counts_as_quiet() {
        let (mut b, _host, _link) = setup();
        b.tick().await;
        assert_eq!(b.quiet_ticks, 1);
    }
    /// A second attach on the same channel number must release the claim the first one made.
    /// Without that, the map entry was overwritten and the old session stayed claimed by a viewer
    /// that no longer existed: a desk window could not take the shell back and nothing released it
    /// until the link dropped.
    #[tokio::test]
    async fn a_re_attach_releases_the_shell_it_replaces() {
        let (mut b, host, link) = setup();
        host.0.lock().unwrap().running.push("abcd-2".into());
        phone_says(
            &link,
            1,
            &Message::Attach {
                version: VERSION,
                selector: "abcd-1".into(),
            },
        );
        b.tick().await;
        assert!(b.attached(1));
        phone_says(
            &link,
            1,
            &Message::Attach {
                version: VERSION,
                selector: "abcd-2".into(),
            },
        );
        b.tick().await;
        let claimed = host.0.lock().unwrap().claimed.clone();
        assert!(
            claimed.contains(&("release", "abcd-1".to_owned())),
            "the replaced shell was never released: {claimed:?}"
        );
        assert!(claimed.contains(&("claim", "abcd-2".to_owned())));
    }

    /// Re-attaching to the shell already attached releases nothing: the claim is still the same
    /// viewer's, and releasing it would hand the shell back to a desk window mid-session.
    #[tokio::test]
    async fn a_re_attach_to_the_same_shell_keeps_its_claim() {
        let (mut b, host, link) = setup();
        phone_says(
            &link,
            1,
            &Message::Attach {
                version: VERSION,
                selector: "abcd-1".into(),
            },
        );
        b.tick().await;
        phone_says(
            &link,
            1,
            &Message::Attach {
                version: VERSION,
                selector: "abcd-1".into(),
            },
        );
        b.tick().await;
        let claimed = host.0.lock().unwrap().claimed.clone();
        assert_eq!(
            claimed.iter().filter(|(k, _)| *k == "release").count(),
            0,
            "{claimed:?}"
        );
    }

    /// Every message the host sent on `shell`, decoded in order.
    fn sent(link: &FakeLink, shell: u8) -> Vec<Message> {
        let mut d = Decoder::new();
        let mut out = Vec::new();
        for (s, bytes) in link.0.lock().unwrap().sent.iter() {
            if *s == shell {
                for item in d.push(bytes).unwrap() {
                    out.push(item.unwrap());
                }
            }
        }
        out
    }

    /// **The gap that made the radio route a viewer and the tailnet route a client.** A version-7
    /// phone asks for a shell, and the host answers with the id it made so the phone can attach to
    /// exactly that one rather than guessing from a list another client may have changed.
    #[tokio::test]
    async fn a_version_7_phone_starts_a_shell_and_is_told_which_one() {
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
        phone_says(
            &link,
            3,
            &Message::Create {
                version: SHELL_CONTROL_VERSION,
                cwd: "~/Documents".into(),
            },
        );
        b.tick().await;
        assert_eq!(
            host.0.lock().unwrap().created,
            [("~/Documents".to_string(), 80, 24)],
            "the host was asked for a shell in the phone's own directory"
        );
        assert!(
            sent(&link, 3).iter().any(|m| matches!(m, Message::Created { id } if id == "new-1")),
            "the phone must be told the id it should attach to"
        );
    }

    /// A host that speaks version 6 has no `Create`, and saying so is better than waiting: the
    /// phone reads `Closed { VERSION }` and stops asking.
    #[tokio::test]
    async fn a_version_6_phone_is_told_the_host_cannot_start_shells() {
        let (mut b, host, link) = setup();
        phone_says(
            &link,
            3,
            &Message::Attach {
                version: CWD_VERSION,
                selector: String::new(),
            },
        );
        b.tick().await;
        // The gate is the message's own version field, not the profile's: a phone that speaks
        // version 6 says so in the command it sends, and a host that answered it would be
        // starting a shell for a client that cannot read the answer.
        phone_says(
            &link,
            3,
            &Message::Create {
                version: CWD_VERSION,
                cwd: String::new(),
            },
        );
        b.tick().await;
        assert!(host.0.lock().unwrap().created.is_empty(), "nothing was started");
        assert!(
            sent(&link, 3)
                .iter()
                .any(|m| matches!(m, Message::Closed { reason } if *reason == closed::VERSION)),
            "the refusal has to name the version, not be silence"
        );
    }

    /// Closing is the tailnet's `close`, not a destroy: the bridge asks the host, and the list the
    /// phone is looking at is replaced with the one that is now true.
    #[tokio::test]
    async fn a_version_7_phone_ends_a_shell_and_gets_the_new_list() {
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
        phone_says(
            &link,
            3,
            &Message::Close {
                selector: "abcd-1".into(),
            },
        );
        b.tick().await;
        assert_eq!(host.0.lock().unwrap().closed, ["abcd-1"]);
        let pages: Vec<_> = sent(&link, 3)
            .into_iter()
            .filter(|m| matches!(m, Message::Sessions { .. }))
            .collect();
        assert!(!pages.is_empty(), "the phone must not keep showing a shell that is gone");
        assert!(
            host.0.lock().unwrap().claimed.contains(&("release", "abcd-1".to_owned())),
            "ending the shell gives it back rather than leaving a claim behind"
        );
    }

    /// **A release the host refuses is counted, not swallowed.** The bug that once left every claim
    /// in place for ever — a device object where the daemon expected a string — was invisible
    /// because the answer was thrown away. A claim that cannot be given back is exactly the state
    /// that stops a desk window reclaiming its shell, so it gets a counter.
    #[tokio::test]
    async fn a_refused_release_is_counted() {
        let (mut b, host, link) = setup();
        host.0.lock().unwrap().refuse_release = true;
        phone_says(
            &link,
            3,
            &Message::Attach {
                version: VERSION,
                selector: "abcd-1".into(),
            },
        );
        b.tick().await;
        phone_says(&link, 3, &Message::Detach);
        b.tick().await;
        assert_eq!(
            b.stats().release_failed, 1,
            "a refused release must be visible in the counters"
        );
    }

    /// **A shell a desk window holds is not ended from the phone.** The tailnet route refuses this
    /// exact press (`not_holder`) and offers a takeover; the radio route called the daemon's bare
    /// `close`, which has no holder rule, so the same button destroyed a shell a desk window was
    /// working in. Two routes are supposed to do one thing.
    #[tokio::test]
    async fn closing_a_shell_another_device_holds_is_refused() {
        let (mut b, host, link) = setup();
        host.0
            .lock()
            .unwrap()
            .holders
            .insert("abcd-1".into(), "desktop".into());
        phone_says(
            &link,
            3,
            &Message::Attach {
                version: VERSION,
                selector: String::new(),
            },
        );
        b.tick().await;
        phone_says(
            &link,
            3,
            &Message::Close {
                selector: "abcd-1".into(),
            },
        );
        b.tick().await;
        assert!(
            host.0.lock().unwrap().closed.is_empty(),
            "a shell the desk holds must not be ended from the phone"
        );
        assert!(
            sent(&link, 3)
                .iter()
                .any(|m| matches!(m, Message::Closed { reason } if *reason == closed::HELD)),
            "the phone must be told it is held, not left guessing"
        );
    }

    /// **Closing one shell must not detach another.** `shells` is keyed by channel, so the close
    /// handler that removed the channel's attach unconditionally released whatever session that
    /// channel happened to be attached to — a shell the user still had open, left with no claim.
    #[tokio::test]
    async fn closing_another_shell_leaves_the_attached_one_claimed() {
        let (mut b, host, link) = setup();
        phone_says(
            &link,
            3,
            &Message::Attach {
                version: VERSION,
                selector: "abcd-1".into(),
            },
        );
        b.tick().await;
        phone_says(
            &link,
            3,
            &Message::Close {
                selector: "abcd-2".into(),
            },
        );
        b.tick().await;
        let claimed = host.0.lock().unwrap().claimed.clone();
        assert!(
            !claimed.contains(&("release", "abcd-1".to_owned())),
            "the shell still attached on this channel must keep its claim: {claimed:?}"
        );
    }

    /// The origin rides the page's flag byte, like the path: a version-6 phone is never sent a
    /// field it does not decode, and a version-7 phone learns that a shell is its own.
    #[tokio::test]
    async fn the_list_says_who_started_each_shell_only_to_a_phone_that_asked() {
        for (version, expect_origin) in [(VERSION, true), (CWD_VERSION, false)] {
            let (mut b, _host, link) = setup();
            phone_says(
                &link,
                3,
                &Message::Attach {
                    version,
                    selector: String::new(),
                },
            );
            b.tick().await;
            phone_says(&link, 3, &Message::List);
            b.tick().await;
            let page = sent(&link, 3)
                .into_iter()
                .find(|m| matches!(m, Message::Sessions { .. }))
                .expect("a list must be answered");
            let Message::Sessions { origin, entries, .. } = page else {
                unreachable!()
            };
            assert_eq!(origin, expect_origin, "version {version}");
            assert_eq!(entries[0].origin.is_empty(), !expect_origin, "version {version}");
        }
    }

    /// **A phone that lets go puts the pty back.** Taking the shell is right — the shell follows the
    /// viewer — but leaving it at the phone's width left a 158-column desk window drawing a 46-column
    /// shell, which reads as the reader's own shell being broken (reported 2026-11-05 after a phone
    /// attached to `meshmore-sns` and detached). The bridge records the size the host had before the
    /// first fit and restores it on detach.
    #[tokio::test]
    async fn a_detach_restores_the_grid_the_phone_displaced() {
        let (mut b, host, link) = setup();
        let original = {
            let s = host.0.lock().unwrap();
            let screen = s.screens.get("abcd-1").cloned().expect("the fixture screen");
            (screen.cols, screen.rows)
        };
        phone_says(
            &link,
            1,
            &Message::Attach {
                version: VERSION,
                selector: String::new(),
            },
        );
        b.tick().await;
        // The phone fits its own pane.
        phone_says(&link, 1, &Message::Resize { cols: 46, rows: 19 });
        b.tick().await;
        assert!(
            host.0.lock().unwrap().resized.contains(&("abcd-1".to_string(), 46, 19)),
            "the phone's fit reached the host: {:?}",
            host.0.lock().unwrap().resized
        );
        // It leaves.
        phone_says(&link, 1, &Message::Detach);
        b.tick().await;
        let resized = host.0.lock().unwrap().resized.clone();
        assert_eq!(
            resized.last(),
            Some(&("abcd-1".to_string(), original.0, original.1)),
            "the detach must put the pty back where it was: {resized:?}"
        );
    }

    /// A shell the phone only looked at is not resized on the way out: no fit of ours, nothing to
    /// undo, and a keeper round trip that changes nothing is still a round trip.
    #[tokio::test]
    async fn a_detach_after_only_looking_does_not_resize() {
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
        phone_says(&link, 1, &Message::Detach);
        b.tick().await;
        assert!(
            host.0.lock().unwrap().resized.is_empty(),
            "nothing was fitted, so nothing is restored: {:?}",
            host.0.lock().unwrap().resized
        );
    }

    /// **A shell a desk window is watching is refused, not stolen.** The phone is told
    /// `Closed { HELD }` and nothing is claimed, so the desk window keeps the shell and the phone
    /// can show the same choice the tailnet route shows: open it on the desktop, or take it back.
    #[tokio::test]
    async fn an_attach_on_a_shell_the_desk_holds_asks_first() {
        let (mut b, host, link) = setup();
        host.0
            .lock()
            .unwrap()
            .holders
            .insert("abcd-1".into(), "desktop".into());
        phone_says(
            &link,
            1,
            &Message::Attach {
                version: VERSION,
                selector: "abcd-1".into(),
            },
        );
        b.tick().await;
        assert!(
            sent(&link, 1)
                .iter()
                .any(|m| matches!(m, Message::Closed { reason } if *reason == closed::HELD)),
            "the refusal has to name the holder, not be silence"
        );
        assert!(
            host.0.lock().unwrap().claimed.is_empty(),
            "nothing may be claimed before the user says so: {:?}",
            host.0.lock().unwrap().claimed
        );
    }

    /// And `Takeover` is that consent: it claims, and the screen follows.
    #[tokio::test]
    async fn a_takeover_takes_the_shell_the_desk_held() {
        let (mut b, host, link) = setup();
        host.0
            .lock()
            .unwrap()
            .holders
            .insert("abcd-1".into(), "desktop".into());
        phone_says(
            &link,
            1,
            &Message::Attach {
                version: VERSION,
                selector: "abcd-1".into(),
            },
        );
        b.tick().await;
        phone_says(
            &link,
            1,
            &Message::Takeover {
                selector: "abcd-1".into(),
            },
        );
        b.tick().await;
        assert_eq!(
            *host.0.lock().unwrap().claimed,
            [("claim", "abcd-1".to_string())],
            "the takeover is what claims it"
        );
        assert!(
            sent(&link, 1)
                .iter()
                .any(|m| matches!(m, Message::Profile { shell, .. } if shell == "abcd-1")),
            "and the phone is given the screen it asked for"
        );
    }
}
