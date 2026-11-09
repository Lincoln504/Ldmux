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
//! - **A failed send keeps exactly what did not go, and nothing is rebuilt.** `reply_upto` says how
//!   many messages reached the link, so the retry re-offers the tail and the differ is not reset:
//!   the phone is never left holding half a change, and the other half costs what it costs rather
//!   than a whole repaint. (It used to reset the differ, which made a full queue cost a full
//!   screen every ten seconds.)
//! - **A frozen view is not drawn.** A phone in batch mode sends `Hold`; the host builds no frames
//!   for that shell until it is released, and the release is the difference from the screen the
//!   phone holds.
//! - **A new handshake drops every attach.** The phone must `Attach` again; nothing carries over
//!   a session that ended.
//! - **Nothing here knows a radio.** The section 13 grep over the daemon crates stays at zero hits.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use ldmux_protocol::keeper::SnapshotResult;
use ldmux_protocol::mesh_term::{
    CWD_VERSION, DecodeError, Decoder, Differ, KEY_VERSION, MARKS_VERSION, MIN_VERSION, MarkEntry,
    Message, ORIGIN_VERSION, OWNER_VERSION, RESIZE_VERSION, SHELL_CONTROL_VERSION, SHELL_VERSION,
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
    /// The largest payload one record can carry **right now**.
    ///
    /// Not a constant: a record is 172 bytes including the ARQ header and the path, so the payload
    /// shrinks with the number of repeats the record will cross. Packing to the empty-path figure
    /// (143) is safe but wasteful at range - a 20-hop record still carries 147 - and it costs a whole
    /// extra record on a compressed screen, which is 0.6 s of air.
    fn record_payload(&self) -> usize {
        ldmux_protocol::mesh_term::RECORD_PAYLOAD
    }
    /// How long one full record takes on the air, when the carrier knows its tuple.
    ///
    /// **A pace is only worth keeping when it is shorter than the record it saves.** On the public
    /// 62.5 kHz preset a record is 600 ms, so deferring a synced line's echo by one airtime is free
    /// in felt latency (the phone draws the line itself) and saves a whole record. On the bench
    /// 500 kHz tuple a record is 75 ms, and the same rule defers for 75 ms rather than half a
    /// second — which is why this is asked of the link instead of being a constant.
    fn airtime_ms(&self) -> Option<u32> {
        None
    }
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
    async fn listing(&mut self) -> Option<Vec<ldmux_protocol::mesh_term::SessionSummary>> {
        Some(
            self.running()
                .await?
                .into_iter()
                .map(|id| ldmux_protocol::mesh_term::SessionSummary {
                    id,
                    ..Default::default()
                })
                .collect(),
        )
    }
    async fn screen(&mut self, id: &str) -> Result<Screen, String>;
    /// **The shell's own scrollback**, in the keeper's line numbering.
    ///
    /// `from` is an absolute line number and `rows` how many rows the caller wants. The answer is
    /// `(start, older, lines)`:
    /// - `start` the absolute line of `lines[0]`, which is `from` whenever the pane still has that
    ///   line, and the oldest line it kept when the caller asked for more than exists — so a caller
    ///   never receives a page it cannot place;
    /// - `older` whether the pane holds history **above** that page, which is how a caller knows
    ///   whether pressing up again can fetch anything;
    /// - `lines` oldest first, with no live-screen rows ever included (the caller already has them).
    ///
    /// `None` means the host could not answer — a failed read, which is a different fact from
    /// "nothing above this" and must not be drawn as one.
    ///
    /// Called only when the operator scrolls up, so a link that never scrolls pays nothing.
    async fn history(&mut self, id: &str, from: u32, rows: u8) -> Option<(u32, bool, Vec<String>)> {
        let _ = (id, from, rows);
        None
    }
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
    /// Claim a shell for the radio link, or learn who holds it.
    ///
    /// **The host decides, not this bridge.** Whether a holder is "in use" (and may refuse) or
    /// merely holds an idle window (and is displaced) is a rule of the claim table, and a second
    /// copy of it here would drift from it — the drift being exactly the bug where nine idle desk
    /// windows refused every attach.
    async fn claim(&mut self, id: &str, takeover: bool) -> Result<ClaimOutcome, String>;
    /// Give the shell back, so a desk window can take it again.
    /// Give the shell back to nobody.
    ///
    /// Returns whether the host accepted it. A release that silently fails is the bug that once
    /// left every claim in place for ever (a device object where a string was expected), and it was
    /// invisible because the answer was discarded — so the answer is part of the contract now.
    async fn release(&mut self, id: &str) -> bool;

    /// The marks the phone should draw beside its rows: `(shell id, kind)`.
    ///
    /// Read once per attach, because that is the radio route's whole delivery model: the host
    /// cannot reach a phone that is not there, and the table it sends is what the phone missed.
    async fn marks(&mut self) -> Vec<(String, u8)> {
        Vec::new()
    }

    /// Who holds `id` now, as the daemon's claim table says: `"mesh"`, `"desktop"`, `"phone"`,
    /// or `""` for nobody. `None` when the host cannot say.
    ///
    /// A separate question from [`Self::listing`] because it is asked on the **write path**,
    /// where a `list` (which asks the keeper about every shell) would put a round trip in front
    /// of every keystroke. The claim table is in the daemon's memory and answers for nothing.
    async fn holder(&mut self, id: &str) -> Option<String> {
        let _ = id;
        None
    }

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
    fn airtime_ms(&self) -> Option<u32> {
        self.snapshot().airtime_ms
    }
    fn record_payload(&self) -> usize {
        // **The record is not the payload.** `uplink_record_max` is what the peer said a *record*
        // may be, and a record carries a 24-byte session overhead (6 clear header + 2 inner header
        // + 16 tag) before the ARQ's own 5-byte header, so the application bytes are what is left.
        // Packing to `max - 5` instead made every record of more than 142 bytes become two: the
        // ARQ re-chunks with its own ceiling, so the bridge's figure has to be the same one or the
        // two layers disagree about what a record is.
        let limit = ldmux_protocol::mesh_term::RECORD_PAYLOAD;
        self.snapshot()
            .uplink_record_max
            .map_or(limit, |m| {
                ldmux_session::record::application_bytes(usize::from(m))
                    .saturating_sub(ldmux_session::arq::HEADER)
            })
            .clamp(1, limit)
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
    /// Writes refused because another device had taken the shell. Counted because it is the
    /// one visible sign of a takeover from the mesh side: the phone sees `HELD`, the desk sees
    /// its own claim, and this is what says the rule fired.
    pub held_writes: u32,
    /// Claims the host refused to give back. A claim that cannot be released keeps the shell held
    /// by a device that is not there, which is the state that stops a desk window reclaiming it.
    pub release_failed: u32,
    /// Holds the phone asked for (batch mode's frozen view). Not frames withheld: a hold that saves
    /// nothing costs one small record, and this is what says whether the feature is being used.
    pub holds: u32,
}

/// What a dropped attach still owes the host: the shell to give back, and the grid to put back.
struct Detached {
    session: String,
    original: Option<(u8, u8)>,
    resized: bool,
}

struct Attached {
    session: String,
    /// The shell id this attach names: the `Profile` says it, and the marks that ride the attach
    /// are looked up by it.
    shell: String,
    differ: Differ,
    /// The profile version the phone attached with. A phone on version 1 does not send `Key`,
    /// and one that does is not obeyed: it did not ask for the message's meaning.
    version: u8,
    /// Messages a refused send left unsent, re-offered before anything new is built.
    ///
    /// **A refusal is a moment, not a fault, and it must not cost a whole screen.** The differ
    /// advances its record of the phone's screen as it hands rows out, so a record the radio refused
    /// is a row the phone never sees unless it is re-offered *as the same message*. Resyncing instead
    /// reset that record, so the next frame was a full screen - measured 2026-11-09: a full screen
    /// plus a profile every ten seconds on an idle link, ~34 records and 20 s of air a minute.
    retry: Vec<Message>,
    /// The grid last accepted from this phone. A `Resize` to the size already set is not a
    /// change, and starting the screen over for it repaints the whole pane for nothing - which
    /// on this link is a burst of records the phone waits through, seen as a freeze while a
    /// full-screen app is open.
    geometry: Option<(u8, u8)>,
    /// When the last screen update for this shell went out. Pacing needs it to know how long a
    /// coalesced change has been waiting.
    last_push: Option<Instant>,
    /// A frame held until this instant: set once, on attach, so the phone's own grid can arrive
    /// before the pane is drawn at whatever geometry the desk left behind. See [`Attached::due`].
    hold: Option<Instant>,
    /// The phone typed into this shell and has not yet been told the pane settled.
    ///
    /// The marker exists for a wait on an *answer*, and an answer follows input: a repaint nobody
    /// asked for (an attach, a resize, a scroll) would otherwise spend a record saying "nothing
    /// more" about a screen nobody was waiting on.
    answer_owed: bool,
    /// How many frames the current answer has taken, counted from when the phone typed.
    ///
    /// **A one-frame answer needs no marker.** The marker's job is to end the phone's wait for an
    /// answer, and the frame that *carries* the answer already ends it: a keystroke's echo is one
    /// frame, and spending a second record (0.6 s of air, one whole record for a 5-byte body) to say
    /// "and nothing more" was a third of the airtime of typing. Only an answer that took two or more
    /// frames has a "more is coming" the phone cannot see for itself.
    pushes_since_input: u32,
    /// Whether the "pane has stopped changing" marker went out for the burst that just ended.
    ///
    /// One marker per burst: the phone ends its wait for an answer on it, and a marker per tick
    /// would be a record every 150 ms for nothing.
    idle_sent: bool,
    /// Whether the previous tick also found a change. A single step after quiet is sent at once
    /// (that is a command's output appearing); a screen that keeps changing is a burst and is
    /// held to [`BURST_GAP`].
    changed_last_tick: bool,
    /// The user typed into this shell and the echo has not been read yet.
    ///
    /// **The first echo of a burst goes at once; the rest wait for the coalescing window.** The
    /// keystroke is what the user is looking for, so the first one must not be paced — but on a
    /// radio a person typing continuously produces an input record every few hundred milliseconds,
    /// and pushing an echo for every one of them is one record down per record up, at 0.6 s each.
    /// `pushes_since_input` already counts the frames since the input arrived, so "the first echo"
    /// is that counter at zero.
    echo: bool,
    /// The earliest instant a frame for this shell may be built: a synced line's echo waits one
    /// record's airtime for its answer, so the two are one frame.
    defer_until: Option<Instant>,
    /// What one record costs on this link, learned when the shell was attached.
    ///
    /// The pace for an echo. **A keystroke's echo cannot leave sooner than a record**, so pushing
    /// one per keystroke spends records on frames that lose the race against the next keystroke:
    /// measured on the airtime harness, a 40-character line cost **38 records down** at one echo per
    /// key, where the same line at one echo per airtime is two or three. Nothing about the typing
    /// changes — the echoes carry every character typed so far, which is the same picture.
    airtime: Option<Duration>,
    /// The shell was held (batch mode's frozen view) and has just been released, so this echo is
    /// the echo of a *synced line*.
    ///
    /// **A synced line's echo is not worth a record of its own.** The phone already draws the line
    /// it sent (its local echo), and the answer's frame carries the echoed row anyway because the
    /// differ has not sent it yet — so the echo goes with the next change instead of costing a
    /// record first. Measured on the airtime harness: one record per command, on a link where a
    /// command's whole answer is six or seven.
    released_by_sync: bool,
    /// When the phone asked the host to stop drawing this shell, if it has.
    ///
    /// **A frozen view is a screen nobody reads.** Batch mode holds the picture on the phone until
    /// the operator presses sync, and the host kept sending a frame a second to a phone that kept
    /// the newest and dropped the rest: on a shell printing a build log that is the hour's allowance
    /// spent on a picture in a drawer. `Hold` stops the building, not the looking — a dead shell and
    /// every control message still arrive — and the release sends the difference from the screen the
    /// phone holds, so nothing is repainted.
    held_since: Option<Instant>,
    /// When this shell's screen was last read while the phone had it held.
    ///
    /// **A frozen view is not worth a keeper round trip twenty times a second.** While held nothing
    /// is built, but the tick still read the screen on every pass — with `screen()` taking the
    /// daemon-wide mutex, which a desk client also needs. A held shell's screen is read once a
    /// second: enough to notice a dead shell, cheap enough not to contend.
    held_read_at: Option<Instant>,
    /// A `Resync` the phone asked for while records were still in flight.
    ///
    /// **Asking for the screen while it is already on its way is not a reason to paint it twice.**
    /// A reset while a frame is queued makes the next tick build a second full frame, and on a slow
    /// link the phone asks again before the first one has landed: that is the repaint loop that
    /// spent about 20 s of air a minute on an attached shell nobody was typing in. The request is
    /// remembered and honoured as soon as the pipe is empty, so a genuine resync still happens and
    /// a repeat costs nothing.
    resync_wanted: bool,
    /// **The grid this phone displaced, put back when it lets go.**
    ///
    /// A phone that attaches takes the shell and fits the pty to its own screen, which is right —
    /// the shell follows the viewer. What was missing is the other half: when the phone leaves, the
    /// pty stayed at the phone's width, so a desk window 158 columns wide was left drawing a 46
    /// column shell and the reader's own shell looked broken (reported 2026-11-05, after a phone
    /// attached to `meshmore-sns` and detached). The bridge records the size the host had before
    /// the first `Resize` and restores it on detach.
    original: Option<(u8, u8)>,
    /// Whether the pane asks for bracketed paste (`CSI ? 2004 h`), read from the last screen.
    ///
    /// **Only the host can answer this, and it decides what a paste means.** A phone sends the
    /// text of a paste with its newlines intact; a pane that negotiated bracketed paste is owed
    /// the `CSI 200 ~` / `CSI 201 ~` markers so every line lands at the prompt as text, and one
    /// that did not is owed carriage returns, because a newline is the only way to give it a line
    /// and a newline is Enter. The phone cannot make this call — the mode lives in the pane — and
    /// without it every pasted line ran as a command (reported 2026-11-07: a multi-line paste
    /// executed its first line and the rest went to whatever that line started).
    bracketed_paste: bool,
}

/// What the pane is owed for one `Input` record: a paste, encoded for its own paste mode.
///
/// Typed text passes through unchanged. Text carrying a newline is a paste, and there are only
/// two honest answers for it:
///
///  - **the pane negotiated bracketed paste** — wrap it in `CSI 200 ~` … `CSI 201 ~`, and the
///    program inserts every line at the prompt instead of running them. ESC is stripped first:
///    a clipboard that contains `ESC [ 2 0 1 ~` would otherwise end the paste early and the rest
///    would run as commands (the classic paste injection);
///  - **it did not** — give it carriage returns. There is no way to hand such a program a
///    newline without its being Enter, so the paste is typed, line by line, which is what every
///    terminal does when a program has not asked for the markers.
///
/// `CRLF` is folded to `LF` first: a paste copied on a system that writes both would otherwise
/// arrive as a carriage return followed by a newline, i.e. a blank line after every line.
///
/// **A newline is the paste marker, and a carriage return is not.** Typed Enter arrives as `\r`,
/// so treating `\r` as a paste would wrap the person's own Enter in the markers — the shell would
/// insert a newline into the line instead of running it. A clipboard whose lines end in bare `\r`
/// therefore still types itself line by line, which is what a terminal does when the markers are
/// absent; anything that ends lines the ordinary way is a paste and is inserted.
#[must_use]
pub fn paste_bytes(bytes: &[u8], bracketed_paste: bool) -> Vec<u8> {
    if !bytes.contains(&b'\n') {
        return bytes.to_vec();
    }
    let text = String::from_utf8_lossy(bytes);
    let text = text.replace("\r\n", "\n");
    if bracketed_paste {
        let mut out = Vec::with_capacity(text.len() + 12);
        out.extend_from_slice(b"\x1b[200~");
        out.extend(text.bytes().filter(|b| *b != 0x1b));
        out.extend_from_slice(b"\x1b[201~");
        out
    } else {
        text.replace('\n', "\r").into_bytes()
    }
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
        // **An attach waits a moment for the phone's grid.** The pane's pty is whatever the desk
        // left it at — measured 158x82 — and a full frame of that is about eighty records, which at
        // the public tuple is a minute and a half of nothing on screen before the phone's `Resize`
        // is even read. The phone now sends its grid with the attach; this gives those two records
        // time to arrive before the first frame is drawn, so the frame that is *actually sent* is
        // the one at the phone's size (about seven records). A phone that sends no resize waits this
        // out and gets its screen a second later, which is what it always did.
        if self.hold.is_some_and(|until| now < until) {
            return false;
        }
        // A synced line's echo waits a moment for the answer it belongs with (see `defer_until`).
        if self.defer_until.is_some_and(|until| now < until) {
            return false;
        }
        if self.echo {
            // **One echo per record, at best.** A keystroke's echo cannot reach the phone sooner
            // than a record takes to send, so the echo follows the link's pace: a person typing
            // continuously then gets one frame per airtime carrying the whole line so far, instead
            // of one frame per key racing the next key. On a fast link this is 200 ms; on the
            // public preset it is one airtime.
            let gap = self.echo_gap();
            return self
                .last_push
                .is_none_or(|t| now.saturating_duration_since(t) >= gap);
        }
        if !self.changed_last_tick {
            return true;
        }
        self.last_push
            .is_none_or(|t| now.saturating_duration_since(t) >= self.burst_gap())
    }

    /// How long an echo waits: one record's airtime, floored so a fast link still feels live.
    /// The coalescing window for a *burst*: how often a screen that keeps changing may go out.
    ///
    /// **A fixed second is a bench number.** It exists so a build log is one update a second rather
    /// than twenty; on a 75 ms bench tuple that is thirteen records of coalescing where two would
    /// do, and the user waits a second for a screen that could have been drawn in 150 ms. Two
    /// airtimes is the same economy expressed in the link's own unit: 1.2 s at the public preset
    /// (capped at the old second, so the slow link is unchanged) and 300 ms on the bench. A slow
    /// link is never *made* slower by this — the cap is the old constant.
    fn burst_gap(&self) -> Duration {
        self.airtime
            .map(|a| a.saturating_mul(2))
            .unwrap_or(BURST_GAP)
            .clamp(Duration::from_millis(300), BURST_GAP)
    }

    fn echo_gap(&self) -> Duration {
        // A link that did not report its airtime is assumed fast: being wrong there costs a record
        // or two, where assuming slow costs a second of lag on every keystroke.
        self.airtime
            .unwrap_or(Duration::from_millis(200))
            .clamp(Duration::from_millis(200), BURST_GAP)
    }

    /// Start the screen over: the differ forgets what the phone holds, and so does the pacing.
    /// A `Resync` and an accepted `Resize` are each "the phone's screen is not the one the last
    /// push left", so the next update is due the moment it exists - holding it would be pacing a
    /// burst that has already ended. A refused send is not one of these any more: it keeps the
    /// exact messages it failed to deliver and re-offers them (see `retry`).
    fn resync(&mut self) {
        self.differ.reset();
        // A resize is the answer the grace was waiting for, so it ends the hold too.
        self.hold = None;
        self.last_push = None;
        self.changed_last_tick = false;
        self.echo = false;
        self.resync_wanted = false;
    }

    /// Rebuild the screen once the link owes nothing.
    ///
    /// A `Resync` that arrives while records are in flight is the phone asking again while the
    /// answer is still going out. Resetting now queues a second full frame behind the first, and
    /// because the phone re-asks on its own timer the second frame is built before the first has
    /// landed: that is the loop that spent about 20 s of air a minute on an attached shell nobody
    /// was typing in. The wanted flag makes the request cost one repaint instead of two, and the
    /// next tick with an empty pipe honours it, so a real resync still happens.
    fn defer_resync(&mut self, pending: usize) {
        if pending == 0 {
            self.resync();
        } else {
            self.resync_wanted = true;
        }
    }

    /// Stop or resume drawing this shell, on the phone's word.
    ///
    /// The differ is deliberately **not** reset: it still records the screen the phone holds, so the
    /// release sends what changed while the view was frozen rather than the whole pane. The pacing is
    /// cleared because the change the release exists for is due now.
    fn hold(&mut self, on: bool, now: Instant) {
        if on {
            self.held_since.get_or_insert(now);
            return;
        }
        self.held_since = None;
        self.held_read_at = None;
        self.defer_until = None;
        // The line that follows this release is a synced line: its echo is not worth a record.
        self.released_by_sync = true;
        self.last_push = None;
        self.changed_last_tick = false;
        self.echo = false;
    }

    fn held(&self) -> bool {
        self.held_since.is_some()
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
    /// The grid the phone last asked for, per shell, kept across a detach.
    ///
    /// **The desk's pty is not the phone's, and waiting for a resize is a race.** A shell left at a
    /// desk window's geometry was drawn at that geometry for the first second of every attach — a
    /// 158x82 or 255x82 pane is twenty thousand cells and twenty-odd records — unless the phone's
    /// `Resize` won. Remembering the grid here means a re-attach fits the pane *before* the first
    /// frame is built, so there is nothing to race.
    grids: HashMap<u8, (u8, u8)>,
    generation: u32,
    /// When the holder of each attached shell was last checked.
    ///
    /// **Ownership can leave without the geometry moving.** A desk window that takes a shell may keep
    /// the same grid, so the resize signal never fires; the phone then drew a shell it no longer
    /// owned and nothing said so until it typed. One listing call every few seconds is a keeper round
    /// trip, not airtime.
    owner_check: Option<Instant>,
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

/// The mark table, cut into `Marks` messages that each fit one record.
///
/// **A message has a body ceiling and the table does not.** `MAX_ROWS_BODY` is 134 bytes and an
/// entry is its id plus two bytes, so a table of a dozen shells already overflows one message — and
/// `reply` drops a message it cannot encode, which would have meant "no marks at all" exactly when
/// an operator had many shells marked. Pages are cheap: the phone's decoder is a stream decoder, so
/// several messages cost the records they already cost and nothing more.
fn marks_pages(marks: Vec<(String, u8)>) -> Vec<Message> {
    use ldmux_protocol::mesh_term::{MAX_ROWS_BODY, MAX_SELECTOR};
    // The flags byte is part of the body, so a page carries one byte less than the ceiling.
    let ceiling = MAX_ROWS_BODY - 1;
    let mut pages: Vec<Message> = Vec::new();
    let mut page: Vec<MarkEntry> = Vec::new();
    let mut used = 0usize;
    for (id, kind) in marks {
        // An id the wire cannot carry is not silently shortened into a mark for another shell: it is
        // dropped, and only a bug could produce one (session ids are ten characters).
        if id.len() > MAX_SELECTOR {
            continue;
        }
        let cost = 2 + id.len();
        if !page.is_empty() && used + cost > ceiling {
            pages.push(Message::Marks {
                first: pages.is_empty(),
                last: false,
                entries: std::mem::take(&mut page),
            });
            used = 0;
        }
        used += cost;
        page.push(MarkEntry { id, kind });
    }
    pages.push(Message::Marks {
        first: pages.is_empty(),
        last: true,
        entries: page,
    });
    pages
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
            grids: HashMap::new(),
            decoders: HashMap::new(),
            versions: HashMap::new(),
            generation: 0,
            owner_check: None,
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

/// Replace a frame's `Rows`/`Commit` run with **one compressed `Frame`**, keeping a leading
/// `Profile` (and anything after the rows) outside it.
///
/// Below [`ldmux_protocol::mesh_term::FRAME_COMPRESS_MIN`] nothing is compressed: deflate's own
/// header and the length field cost more than the text repeats, and a one-row echo must stay one
/// record. A non-contiguous run is left alone too — a message that must be read in order after the
/// rows (a `Closed`, say) would change meaning inside a frame.
/// The encoded size of a run of messages, or `usize::MAX` when one will not encode.
fn plain_len(messages: &[Message]) -> usize {
    messages
        .iter()
        .map(|m| m.encode().map_or(usize::MAX, |b| b.len()))
        .fold(0usize, |a, b| a.saturating_add(b))
}

fn pack_frame(messages: &[Message], with_dict: bool) -> Vec<Message> {
    use ldmux_protocol::mesh_term::Message as M;
    let is_screen = |m: &Message| matches!(m, M::Rows { .. } | M::Commit { .. });
    // **A run of messages is a frame, not only a screen.** The launcher is the other place the host
    // spends several records on one answer: a dozen shells is about 700 bytes of repeated words
    // (`bash`, `~/Documents/`, the ids) and deflates to a fraction of that. A frame's inner bytes
    // are fed back through the phone's own decoder, so nothing about a page's meaning changes.
    let (start, end) = match messages.iter().position(&is_screen) {
        Some(start) => {
            let end = messages
                .iter()
                .rposition(&is_screen)
                .map_or(start, |i| i + 1);
            if messages[start..end].iter().any(|m| !is_screen(m)) {
                return messages.to_vec();
            }
            (start, end)
        }
        // No screen at all: the whole answer, when there is enough of it to pay for the frame.
        None => (0, messages.len()),
    };
    let mut out: Vec<Message> = messages[..start].to_vec();
    out.extend(Self::frame_run(&messages[start..end], with_dict));
    out.extend_from_slice(&messages[end..]);
    out
}

/// Compress a run of messages into **as few frames as its encoded size allows**.
///
/// **One frame is one *message*, and a message body has a ceiling.** A forty-eight shell list is
/// about 2.6 KB of text, and it deflates to more than the 598-byte body a frame may carry: a single
/// frame then cannot encode, and the caller fell back to one record per 137-byte page — twenty-odd
/// records for a list that is two. The run is therefore split, in halves, until each piece fits; a
/// frame is `Rows`/`Commit`/`Sessions` among other messages and each is complete in itself, and the
/// phone's mirror accumulates an open frame until its `Commit` whatever record it arrived in.
fn frame_run(run: &[Message], with_dict: bool) -> Vec<Message> {
    use ldmux_protocol::mesh_term::{FRAME_COMPRESS_MIN, FRAME_MAX_INFLATED, Message as M};
    if run.is_empty() {
        return Vec::new();
    }
    let mut inner = Vec::new();
    for m in run {
        let Ok(bytes) = m.encode() else {
            return run.to_vec();
        };
        inner.extend_from_slice(&bytes);
    }
    if inner.len() < FRAME_COMPRESS_MIN {
        return run.to_vec();
    }
    if inner.len() <= FRAME_MAX_INFLATED {
        // **The dictionary frame first, the plain one as the fallback.** A phone that reported
        // version 12 carries [`FRAME_DICTIONARY`], and against it the same screen deflates ~20 %
        // smaller — 419 -> 325 bytes on a real status screen, which is the difference between three
        // records and two.
        let dict_frame = with_dict.then_some(M::FrameDict {
            inner: inner.clone(),
        });
        let plain_frame = M::Frame { inner };
        // **The frame must encode, and it must be smaller than what it replaces.** Both are measured
        // here rather than assumed: on text that does not repeat (a hex dump, a table of digits)
        // deflate can come out larger than the input, and an encode refusal used to fail the whole
        // reply — the bridge resynced, rebuilt the identical frame and refused it again, so a screen
        // that could not be compressed was a screen that never painted.
        let plain = Self::plain_len(run);
        let frame = match dict_frame {
            Some(f) if f.encode().is_ok_and(|b| b.len() < plain) => f,
            _ => plain_frame,
        };
        if let Ok(bytes) = frame.encode()
            && bytes.len() < plain
        {
            return vec![frame];
        }
    }
    // Either too big for one body, or not worth a frame as a whole: split and try each half. A run
    // of one message that will not frame is sent as it is.
    if run.len() == 1 {
        return run.to_vec();
    }
    let mid = run.len() / 2;
    let mut out = Self::frame_run(&run[..mid], with_dict);
    out.extend(Self::frame_run(&run[mid..], with_dict));
    out
}

    fn reply(&mut self, shell: u8, messages: &[Message]) -> bool {
        self.reply_upto(shell, messages).is_ok()
    }

    /// As [`Self::reply`], saying **how far it got**.
    ///
    /// The return value is the number of messages at the front the phone is done with: sent, or
    /// skipped because they will never encode. A caller that keeps its messages for a retry needs
    /// this, because a record can carry several messages and a refusal takes the whole record: the
    /// prefix already on the link must not be offered again, and on a link priced per record that
    /// duplicate is a whole record of air.
    fn reply_upto(&mut self, shell: u8, messages: &[Message]) -> Result<(), usize> {
        // **Messages share a record when they fit.** The phone's decoder is a stream decoder
        // (`TermDecoder` in MeshTerm.kt), so a record may carry several messages, and the
        // protocol's own `RECORD_PAYLOAD` is the only limit. Sending one record per message
        // cost a whole 176-byte frame per message: a one-row echo paid two frames (`Rows` and
        // its `Commit`) where the pair is 30-40 bytes. Packed, the same echo is one frame.
        //
        // **The version is the phone's, not the attach's.** A `List` is answered *because* nothing
        // is attached: the launcher asks before it opens anything, and a `Close` asks for the list
        // after the attach was removed. Reading the version from the live attach therefore made
        // every launcher answer one record per message and unframed — a nineteen-shell list was
        // eleven records, about 6 s of air, when the same bytes framed are two or three. The
        // version the phone attached with is already remembered in `versions`, refused or accepted,
        // and the `List` handler twenty lines away uses it for exactly this reason.
        let version = self
            .shells
            .get(&shell)
            .map(|s| s.version)
            .or_else(|| self.versions.get(&shell).copied())
            .unwrap_or(0);
        let streamed = version >= STREAM_VERSION;
        // **A big frame is compressed as one frame, not one record at a time.** A record is 143 B
        // and one screen is 900, so compressing a record saves nothing (the record still goes out
        // whole). Compressing the frame is 1.7x to 8x on real terminal text — measured, raw deflate:
        // a build log 936 → 554, a TUI 971 → 413, a code listing 894 → 112 — which is seven records
        // becoming three to one. `Profile` stays outside it: the phone must know the grid before the
        // rows mean anything, and it is 16 bytes.
        let framed: Vec<Message> = if streamed && version >= ldmux_protocol::mesh_term::FRAME_VERSION {
            Self::pack_frame(messages, version >= ldmux_protocol::mesh_term::FRAME_DICT_VERSION)
        } else {
            messages.to_vec()
        };
        let messages: &[Message] = &framed;
        let payload = self.link.record_payload();
        let mut record: Vec<u8> = Vec::with_capacity(payload);
        // Messages inside `record` that the link has not taken yet.
        let mut carried: usize = 0;
        // Messages at the front that are finished with: on the link, or unencodable.
        let mut delivered: usize = 0;
        let mut skipped: usize = 0;
        for m in messages {
            let Ok(bytes) = m.encode() else {
                // **An unencodable message is consumed, not retried.** It will never encode, so a
                // retry re-fails on every tick and holds everything behind it for ever.
                delivered += 1;
                skipped += 1;
                continue;
            };
            if !streamed {
                if self.link.send(shell, &bytes).is_ok() {
                    delivered += 1;
                } else {
                    self.stats.refused_sends += 1;
                    return Err(delivered);
                }
                continue;
            }
            if !record.is_empty() && record.len() + bytes.len() > payload {
                if self.link.send(shell, &record).is_ok() {
                    self.stats.records_sent += 1;
                    delivered += carried;
                } else {
                    self.stats.refused_sends += 1;
                    return Err(delivered);
                }
                record.clear();
                carried = 0;
            }
            // **A message bigger than a record is carried whole and split below.** The phone's
            // decoder reassembles by length, so nothing about a message's meaning depends on it
            // fitting one record — and a compressed frame (a few hundred bytes for a screen that
            // was seven records) is exactly such a message. The session chunks it into records at
            // this same ceiling, so one `send` here is one message, however many records it becomes.
            record.extend_from_slice(&bytes);
            carried += 1;
        }
        if !record.is_empty() {
            if self.link.send(shell, &record).is_ok() {
                self.stats.records_sent += 1;
                delivered += carried;
            } else {
                self.stats.refused_sends += 1;
                return Err(delivered);
            }
        }
        // **Counted after the send, not before it.** A counter incremented as messages were packed
        // reported messages that never left, which is the one number a user reads as "the phone
        // got it".
        self.stats.messages_sent = self.stats.messages_sent.saturating_add(
            u32::try_from(delivered.saturating_sub(skipped)).unwrap_or(u32::MAX),
        );
        if timing() {
            eprintln!("ldmuxd: mesh timing: reply queued at {:?}", stamp());
        }
        Ok(())
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

        // **Who holds each attached shell, checked on a cadence.** The desk may take a shell without
        // changing its geometry, and the write path only learns that when the phone types; until
        // then the phone draws a pane that is no longer its own and looks broken.
        let owner_due = self
            .owner_check
            .is_none_or(|t| (self.clock)().saturating_duration_since(t) >= OWNER_CHECK);
        if owner_due && !self.shells.is_empty() {
            self.owner_check = Some((self.clock)());
            if let Some(rows) = self.host.listing().await {
                let taken: Vec<u8> = self
                    .shells
                    .iter()
                    .filter(|(_, a)| {
                        rows.iter()
                            .any(|r| r.id == a.shell && !r.owner.is_empty() && r.owner != "mesh")
                    })
                    .map(|(shell, _)| *shell)
                    .collect();
                for shell in taken {
                    self.refuse_held_write(shell).await;
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
            // **A whole frame when the window is empty, a cautious slice when it is not.** The
            // differ's argument is in messages and the window is in records, and a compressed frame
            // is one message per screen — so a message budget of 8 split the phone's own grid across
            // two ticks and, because a partial frame carries no `Commit`, the phone could paint
            // nothing until the second one. With nothing in flight a whole frame is built at once
            // (2 records once compressed, well inside the window); with the pipe busy, the old
            // record-for-message budget stands, because that is what keeps a keyboard echo from
            // queueing behind a repaint.
            // **A held shell is looked at once a second.** Nothing is built for it, and the tick
            // otherwise reads the keeper's snapshot on every pass for a picture nobody draws.
            if let Some(a) = self.shells.get(&shell)
                && a.held()
            {
                let now = (self.clock)();
                let due = a
                    .held_read_at
                    .is_none_or(|t| now.saturating_duration_since(t) >= HELD_SCREEN_MS);
                if !due {
                    continue;
                }
                if let Some(a) = self.shells.get_mut(&shell) {
                    a.held_read_at = Some(now);
                }
            }
            let pending = self.link.pending();
            // **A resync asked for while the link owed records is honoured now.** It was deferred so
            // the screen it asked for was not queued twice; the pipe is empty, so this is the one
            // repaint the request should cost.
            if pending == 0
                && let Some(a) = self.shells.get_mut(&shell)
                && a.resync_wanted
            {
                a.resync();
            }
            let room = if pending == 0 {
                FRAME_MESSAGES
            } else {
                PENDING_LIMIT.saturating_sub(pending)
            };
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
            // The pane's paste mode, refreshed with the screen it arrived on: a program turns
            // bracketed paste on when it starts and off when it exits, so this changes under the
            // phone and a value read once at attach would be the shell's, not the program's.
            if let Some(a) = self.shells.get_mut(&shell) {
                a.bracketed_paste = screen.bracketed_paste;
            }
            // Before this phone has fitted anything, the pty's size is whatever the desk left it
            // at: that is the size to put back. Recorded once, and only before our own `Resize`
            // has been applied, so a later screen cannot overwrite it with the phone's grid.
            if let Some(a) = self.shells.get_mut(&shell)
                && a.original.is_none()
                && a.geometry.is_none()
            {
                a.original = Some((screen.cols, screen.rows));
            }
            // **A held shell builds nothing.** The screen is still read (that is how a dead shell is
            // noticed) and every control message still goes, but no frame is built, because the
            // phone is not drawing it. The hold expires so a phone that never releases cannot leave
            // a shell permanently unlit.
            let now = (self.clock)();
            if let Some(a) = self.shells.get_mut(&shell)
                && a
                    .held_since
                    .is_some_and(|t| now.saturating_duration_since(t) >= HOLD_MAX)
            {
                a.hold(false, now);
            }
            if self.shells.get(&shell).is_some_and(Attached::held) {
                if screen.dead {
                    self.reply(
                        shell,
                        &[Message::Closed {
                            reason: closed::EXITED,
                        }],
                    );
                    self.shells.remove(&shell);
                }
                continue;
            }
            // **What a refusal left unsent goes first.** The differ already counts those rows as
            // delivered, so they are only recoverable here; anything built after them would be the
            // *next* change, and the phone would keep a gap for ever.
            let retry = self
                .shells
                .get(&shell)
                .map(|a| a.retry.clone())
                .unwrap_or_default();
            if !retry.is_empty() {
                // **Only what did not go out is offered again.** A record can carry several
                // messages, so a refusal at the third record means the first two are on the link:
                // re-offering the whole list would put them there twice, and on this link a
                // duplicate is a whole record of air.
                match self.reply_upto(shell, &retry) {
                    Ok(()) => {
                        if let Some(a) = self.shells.get_mut(&shell) {
                            a.retry.clear();
                            a.last_push = Some((self.clock)());
                        }
                    }
                    Err(unsent) => {
                        if let Some(a) = self.shells.get_mut(&shell) {
                            a.retry = retry[unsent.min(retry.len())..].to_vec();
                        }
                        self.stalled = true;
                    }
                }
                continue;
            }
            if self.shells.get(&shell).is_some_and(|a| a.differ.up_to_date(&screen)) {
                let now = (self.clock)();
                // **Tell the phone the answer has finished arriving.** It holds the screen already,
                // so this is one record with no rows; the phone ends its "waiting for the answer"
                // on it instead of guessing from a quiet timer.
                let due = self.shells.get(&shell).is_some_and(|a| {
                    a.answer_owed
                        && !a.idle_sent
                        // One frame answered it: that frame ended the wait already.
                        && a.pushes_since_input >= 2
                        && a.last_push
                            .is_some_and(|t| now.saturating_duration_since(t).as_millis() >= IDLE_AFTER_MS)
                });
                if due {
                    let marker = self.shells.get(&shell).and_then(|a| a.differ.idle_marker(&screen));
                    if let Some(marker) = marker {
                        self.stats.updates += 1;
                        if let Some(a) = self.shells.get_mut(&shell) {
                            a.idle_sent = true;
                            a.answer_owed = false;
                        }
                        self.reply(shell, &[marker]);
                    }
                }
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
            let was_echo = self.shells.get(&shell).is_some_and(|a| a.echo);
            if let Err(unsent) = self.reply_upto(shell, &messages) {
                // **Keep them, do not start the screen over.** `reply` fails when the ARQ queue is
                // full, which is a moment: the differ has already advanced over the rows it handed
                // out, so the same messages are the only way to deliver them. Resyncing made the
                // next frame a full screen into a queue that was already full.
                if let Some(a) = self.shells.get_mut(&shell) {
                    a.retry = messages[unsent.min(messages.len())..].to_vec();
                }
                self.stalled = true;
                continue;
            }
            if let Some(a) = self.shells.get_mut(&shell) {
                a.retry.clear();
                a.last_push = Some(now);
                // **An echo does not open a burst.** The keystroke is sent at once, which is right,
                // and then the answer it produces is the *next* change: marking this push as a
                // burst held that answer for a whole coalescing window, so every command paid up
                // to a second of nothing after its own echo had already gone out. A burst is what
                // follows a burst, and an echo is not one.
                a.changed_last_tick = !was_echo;
                a.idle_sent = false;
                a.echo = false;
                a.pushes_since_input = a.pushes_since_input.saturating_add(1);
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

    /// Tell the phone a shell it is still attached to is another device's, and let go of it.
    ///
    /// **The write path obeys the same rule as the attach path.** A desk window that takes a
    /// shell displaces the phone by ADR-0010, but `Input` and `Key` used to write straight
    /// through: the desk believed it owned the shell, the phone kept typing into the same pty,
    /// and neither was told. `Closed { HELD }` is the vocabulary the attach path already uses,
    /// and the claim plus the displaced grid are given back exactly as `Detach` gives them back,
    /// so the desk window that took the shell is not left with a pty fitted to a phone that is
    /// no longer attached.
    async fn refuse_held_write(&mut self, shell: u8) {
        self.stats.held_writes = self.stats.held_writes.saturating_add(1);
        self.reply(shell, &[Message::Closed { reason: closed::HELD }]);
        if let Some(a) = self.shells.remove(&shell) {
            let resized = a.geometry.is_some();
            self.release(&a.session).await;
            if let Some((cols, rows)) = a.original.filter(|_| resized) {
                let _ = self.host.resize(&a.session, cols, rows).await;
            }
        }
        self.decoders.remove(&shell);
    }

    /// Whether the shell this phone attached to has moved to another device since.
    async fn displaced(&mut self, session: &str) -> bool {
        match self.host.holder(session).await {
            Some(holder) => !holder.is_empty() && holder != "mesh",
            // Unknown is not "someone else": a host that cannot answer must not cut the phone
            // off mid-word. The attach path already refused the shell if it was held.
            None => false,
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
                    rows.iter().map(|r| r.id.clone()).collect::<Vec<_>>()
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
                        // **The host decides whether the holder is in use.** A shell a desk window
                        // has open but nobody has typed into is displaced here, silently, and that
                        // window is told and stops: the refusal with `take it back` is for the case
                        // the rule exists for, somebody typing at the desk right now. The check
                        // used to live here as "the holder is not the mesh link", which refused the
                        // phone for every desk window left open on the machine.
                        match self.host.claim(one, takeover).await {
                            Ok(ClaimOutcome::Claimed) => {}
                            Ok(ClaimOutcome::Held { .. }) => {
                                self.reply(shell, &[Message::Closed { reason: closed::HELD }]);
                                return;
                            }
                            Err(_) => {
                                self.reply(
                                    shell,
                                    &[Message::Closed {
                                        reason: closed::UNAVAILABLE,
                                    }],
                                );
                                return;
                            }
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
                        // **Fit the pane to the grid this phone used last time, before reading it.**
                        // Otherwise the first frame is the desk's geometry and the phone's `Resize`
                        // is a race the desk usually wins: measured 2026-11-09, a 255x82 desk pane
                        // was drawn and committed before a 46x17 grid arrived, twenty-odd records
                        // for a screen nobody would see. The remembered grid is a hint, not a
                        // promise: the phone resizes on its first relayout and corrects it.
                        let remembered = self.grids.get(&shell).copied();
                        if let Some((cols, rows)) = remembered {
                            let _ = self.host.resize(one, cols, rows).await;
                        }
                        let original = self.host.screen(one).await.ok();
                        // The pane's own paste mode rides the same read: the first keystroke can
                        // beat the next tick, and a paste that arrived before it would execute
                        // every line.
                        let bracketed_paste = original.as_ref().is_some_and(|s| s.bracketed_paste);
                        let original = original.map(|s| (s.cols, s.rows));
                        self.shells.insert(
                            shell,
                            Attached {
                                session: (*one).clone(),
                                shell: (*one).clone(),
                                // Answered in the version asked for, naming the shell the host
                                // chose, so a phone that sent an empty selector learns which.
                                differ: Differ::for_attach(version, one),
                                answer_owed: false,
                                idle_sent: false,
                                pushes_since_input: 0,
                                version,
                                geometry: remembered,
                                retry: Vec::new(),
                                last_push: None,
                                // **Only when the grid is unknown.** The grace exists so the
                                // first frame is not drawn at the desk's geometry while the
                                // phone's own grid is in flight. When this phone has attached
                                // before, the pane was already fitted to that grid above, so
                                // there is nothing to wait for: 1.4 s of blank screen per
                                // re-attach was being spent on a record that had arrived.
                                hold: remembered
                                    .is_none()
                                    .then(|| Instant::now() + ATTACH_GRACE),
                                resync_wanted: false,
                                held_since: None,
                                released_by_sync: false,
                                defer_until: None,
                                airtime: self
                                    .link
                                    .airtime_ms()
                                    .map(|ms| Duration::from_millis(u64::from(ms))),
                                held_read_at: None,
                                changed_last_tick: false,
                                echo: false,
                                original,
                                bracketed_paste,
                            },
                        );
                        // **The profile goes out at once, before any row.** A phone asks for its own
                        // grid only once it knows the pane's, so withholding the profile until the
                        // rows are due is a deadlock: measured 2026-11-09, the host drew a 255x82
                        // desk pane (twenty thousand cells, twenty-odd records) because the profile
                        // that would have made the phone ask for 46x17 was held with the rows behind
                        // the grace. Eighteen bytes now, and the phone's `Resize` follows one round
                        // trip later.
                        let profile = self.shells.get(&shell).map(|a| Message::Profile {
                            version: a.version,
                            cols: original.map_or(0, |(c, _)| c),
                            rows: original.map_or(0, |(_, r)| r),
                            unsupported: if a.version >= RESIZE_VERSION {
                                ldmux_protocol::mesh_term::unsupported::ALL_V1
                                    & !ldmux_protocol::mesh_term::unsupported::RESIZE
                            } else {
                                ldmux_protocol::mesh_term::unsupported::ALL_V1
                            },
                            shell: a.shell.clone(),
                        });
                        let Some(profile) = profile else { return };
                        // **What this phone missed, sent once.** The marks that are true now go
                        // with the attach, because the radio cannot reach a phone that is not
                        // connected and this is the only moment delivery is possible. A phone on a
                        // version below [MARKS_VERSION] is not sent the tag at all.
                        //
                        // **Sent even when it is empty.** An empty page is how the phone learns that
                        // a mark it still shows has expired or been read elsewhere; skipping the
                        // message left the stale row on the launcher until the app restarted.
                        //
                        // **In the same record as the profile.** They are a few dozen bytes together
                        // and two separate replies were two records: 0.6 s of air on this link for a
                        // page that is usually empty. The profile stays first in the record, so the
                        // phone still learns the grid before it reads a row.
                        if version >= MARKS_VERSION {
                            let marks = self.host.marks().await;
                            let mut together = vec![profile];
                            together.extend(marks_pages(marks));
                            self.reply(shell, &together);
                        } else {
                            self.reply(shell, &[profile]);
                        }
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
                // The shell rides the list like the folder and the holder: one flag, one string per
                // entry, and only for a phone that speaks it.
                let with_shell = version.is_some_and(|v| v >= SHELL_VERSION);
                let mut answer =
                    session_pages(&listing, with_cwd, with_origin, with_owner, with_shell);
                // **The marks ride with the list.** The launcher is the one screen that shows every
                // shell, so it is the one screen that needs the table — and it lists without
                // attaching, which is why sending the table only on an attach left every row
                // unmarked until a shell was opened (measured on the device, 2026-11-06).
                //
                // A phone below the marks version is not sent it; one that has not attached yet has
                // said no version at all, and that is the launcher's case. It gets the table anyway:
                // an unknown tag is skipped by every phone this app has ever shipped, and a version
                // the host has not learned yet is not worth another round trip. The table costs one
                // record per list, empty included: an empty page is how the phone learns that a mark
                // it still draws has expired, or was read on the desk.
                //
                // **In the same record as the first page.** Two replies were two records, and a
                // launcher screen the user opens and closes all day is not worth a second one.
                let marks_version_ok = self.versions.get(&shell).is_none_or(|v| *v >= MARKS_VERSION);
                if marks_version_ok {
                    let marks = self.host.marks().await;
                    answer.extend(marks_pages(marks));
                }
                self.reply(shell, &answer);
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
                // **A claim-table question, not a listing.** `listing` asks the keeper about every
                // shell, and this answer is in the daemon's memory: the same read was made twice
                // in this handler (here and for the reply below), so a close cost two keeper round
                // trips before the shell was even ended.
                let holder = self.host.holder(&id).await.unwrap_or_default();
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
                        &session_pages(
                            &listing,
                            with_cwd,
                            with_origin,
                            with_owner,
                            version >= SHELL_VERSION,
                        ),
                    );
                }
            }
            // **A page up on a screen the phone never saw.** The keeper has the rows; the phone
            // asks for them by absolute line number, joining what it holds rather than overlapping
            // it. Answered only when asked, so it costs nothing until the operator scrolls.
            Message::WantHistory { from, rows } => {
                let Some(session) = self.shells.get(&shell).map(|a| a.session.clone()) else {
                    return;
                };
                let Some((start, older, lines)) = self.host.history(&session, from, rows).await
                else {
                    return;
                };
                // One line per history request, only when the operator scrolls: the numbers a "the
                // page did not arrive" report needs (asked, served, how many).
                eprintln!(
                    "ldmuxd: history {from}+{rows} -> {start}, {} line(s), older={older}",
                    lines.len()
                );
                // **Paged by bytes, not by line count.** A body is at most 600 bytes and a 46-column
                // row is ~46, so a screenful is two messages. Each page states the absolute line of
                // its first row, so the phone places it exactly rather than assuming the page joins
                // what it holds. `last` ends the answer; `older` rides every page because it is a
                // fact about the answer, not about one page.
                let mut pages: Vec<Message> = Vec::new();
                let mut page: Vec<String> = Vec::new();
                let mut used = 5usize;
                // `at` is the absolute line of `page[0]`: a page advances by its **line count**,
                // not by its byte count.
                let mut at = start;
                for line in lines {
                    // **A row is clamped here, at the wire, because a row that cannot be encoded
                    // takes the whole page with it.** The host clamps too; this is the guarantee
                    // that no host can make a page silently disappear, which is what a `reply`
                    // skips when a message will not encode.
                    let line = ldmux_protocol::mesh_term::clamp_row(&line);
                    let cost = 1 + line.len();
                    if !page.is_empty() && used + cost > 560 {
                        let from = at;
                        at += page.len() as u32;
                        pages.push(Message::HistoryPage {
                            from,
                            last: false,
                            older,
                            lines: std::mem::take(&mut page),
                        });
                        used = 5;
                    }
                    used += cost;
                    page.push(line);
                }
                pages.push(Message::HistoryPage {
                    from: at,
                    last: true,
                    older,
                    lines: page,
                });
                self.reply(shell, &pages);
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
                if self.displaced(&session).await {
                    self.refuse_held_write(shell).await;
                    return;
                }
                self.stats.inputs += 1;
                let synced = self.shells.get(&shell).is_some_and(|a| a.released_by_sync);
                // **One record's airtime, no more.** The deferral exists to merge the echo into the
                // answer's frame; paying longer than the record it saves is a latency loss dressed
                // as an airtime win, so the pace is the link's own airtime (600 ms at the public
                // preset, 75 ms on the bench) capped by the burst window.
                let defer = self
                    .link
                    .airtime_ms()
                    .map(|ms| Duration::from_millis(u64::from(ms)))
                    .unwrap_or_default()
                    .min(BURST_GAP);
                let now = (self.clock)();
                if let Some(a) = self.shells.get_mut(&shell) {
                    // A line that arrived with its own release from a hold is a synced line: its
                    // echo rides the answer's frame instead of costing a record first.
                    a.echo = !synced;
                    a.released_by_sync = false;
                    a.defer_until = synced.then(|| now + defer);
                    a.answer_owed = true;
                    a.idle_sent = false;
                    a.pushes_since_input = 0;
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
                // **A newline in `Input` is a paste, and the pane decides what to do with it.**
                // Typed text never carries one (Enter crosses as a `Key` name, so the host's own
                // key table encodes it for the pane's mode); pasted text does, because the phone
                // sends it verbatim. Wrapping happens here rather than on the phone for one
                // reason: whether the pane asked for bracketed paste is a fact only the host can
                // read, and a phone that guessed "no" turned every pasted line into a command.
                let bracketed = self.shells.get(&shell).is_some_and(|a| a.bracketed_paste);
                let bytes = paste_bytes(&bytes, bracketed);
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
                if self.displaced(&session).await {
                    self.refuse_held_write(shell).await;
                    return;
                }
                self.stats.keys += 1;
                let synced = self.shells.get(&shell).is_some_and(|a| a.released_by_sync);
                let defer = self
                    .link
                    .airtime_ms()
                    .map(|ms| Duration::from_millis(u64::from(ms)))
                    .unwrap_or_default()
                    .min(BURST_GAP);
                let now = (self.clock)();
                if let Some(a) = self.shells.get_mut(&shell) {
                    a.echo = !synced;
                    a.released_by_sync = false;
                    a.defer_until = synced.then(|| now + defer);
                    a.answer_owed = true;
                    a.idle_sent = false;
                    a.pushes_since_input = 0;
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
                if accepted {
                    let pending = self.link.pending();
                    if let Some(a) = self.shells.get_mut(&shell) {
                        a.geometry = Some((cols, rows));
                        a.defer_resync(pending);
                    }
                    self.grids.insert(shell, (cols, rows));
                }
            }
            Message::Resync => {
                let pending = self.link.pending();
                if let Some(a) = self.shells.get_mut(&shell) {
                    a.defer_resync(pending);
                }
            }
            // **The phone's view is frozen, so nothing is built for it.** A version-12 host ignores
            // the tag and behaves exactly as before, which is why this needs no version gate.
            Message::Hold { on } => {
                let now = (self.clock)();
                if let Some(a) = self.shells.get_mut(&shell) {
                    a.hold(on, now);
                    self.stats.holds += u32::from(on);
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

/// What the host said about a claim.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClaimOutcome {
    /// The shell is this link's now. Whoever held it was idle, or was displaced by consent.
    Claimed,
    /// An in-use holder refuses, and the user is offered `take it back`.
    Held { holder: String },
}

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

    async fn listing(&mut self) -> Option<Vec<ldmux_protocol::mesh_term::SessionSummary>> {
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
                        // The shell the session was created with (`bash`), for the row's second
                        // line: the folder and the shell, always.
                        let shell = s.get("shell").and_then(|x| x.as_str()).unwrap_or("");
                        Some(ldmux_protocol::mesh_term::SessionSummary {
                            id: id.to_owned(),
                            label: label.to_owned(),
                            cwd: cwd.to_owned(),
                            origin: origin.to_owned(),
                            owner: holder.to_owned(),
                            shell: shell.to_owned(),
                        })
                    })
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        Some(rows)
    }

    async fn marks(&mut self) -> Vec<(String, u8)> {
        let v = self
            .0
            .lock()
            .await
            .handle("marks", serde_json::json!({}))
            .await;
        let Ok(v) = v else { return Vec::new() };
        v.get("marks")
            .and_then(|m| m.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|m| {
                        let id = m.get("session").and_then(|x| x.as_str())?.to_owned();
                        // A kind this build does not know is skipped, not folded into `done`: a
                        // word added later would otherwise arrive as the wrong mark instead of as
                        // no mark.
                        let kind = match m.get("kind").and_then(|x| x.as_str()) {
                            Some("done") => 0,
                            Some("attention") => 1,
                            _ => return None,
                        };
                        Some((id, kind))
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    async fn holder(&mut self, id: &str) -> Option<String> {
        let v = self
            .0
            .lock()
            .await
            .handle("holder", serde_json::json!({ "id": id }))
            .await
            .ok()?;
        Some(
            v.get("holder")
                .and_then(|h| h.as_str())
                .unwrap_or_default()
                .to_owned(),
        )
    }

    async fn history(&mut self, id: &str, from: u32, rows: u8) -> Option<(u32, bool, Vec<String>)> {
        // **Two reads, because the keeper's `historyLines` counts back from the live screen.**
        // Asking for "a page plus a margin" is wrong the moment the operator scrolls further back
        // than the margin: the keeper would return the newest N lines above live, the page would
        // not join what the client holds, and the missing rows would go unnoticed. The first read
        // is the geometry (`totalRows`, `rows`, `historySize`), which the keeper reports whether or
        // not it sends any history; the second asks for exactly the lines between `from` and the
        // live screen. Both are local socket calls, so the extra one costs microseconds and not a
        // single byte of airtime.
        let want = u32::from(rows.max(1));
        let info: SnapshotResult = serde_json::from_value(
            self.0
                .lock()
                .await
                .handle("snapshot", serde_json::json!({ "id": id }))
                .await
                .ok()?,
        )
        .ok()?;
        let total = u32::try_from(info.total_rows).unwrap_or(u32::MAX);
        let live_top = total.saturating_sub(u32::try_from(info.rows).unwrap_or(0));
        let available = u32::try_from(info.history_size).unwrap_or(0);
        // **Nothing to serve, but two different reasons, and `older` tells them apart.** A request
        // at or past the live screen never receives live rows, and whether the pane still holds
        // history above that point is exactly what the caller needs to know: `older` false means
        // "you are at the oldest row this shell kept", true means "ask again lower down".
        let above = live_top > total.saturating_sub(available);
        if from >= live_top {
            return Some((live_top, above, Vec::new()));
        }
        let need = live_top.saturating_sub(from).min(available);
        if need == 0 {
            return Some((live_top, above, Vec::new()));
        }
        let snap: SnapshotResult = serde_json::from_value(
            self.0
                .lock()
                .await
                .handle(
                    "snapshot",
                    serde_json::json!({ "id": id, "historyLines": need }),
                )
                .await
                .ok()?,
        )
        .ok()?;
        // **Second answer, second geometry.** The pane may have scrolled between the two reads, and
        // this snapshot is the one the text belongs to, so every count is taken from it.
        let all: Vec<String> = snap
            .text
            .split('\n')
            .map(ldmux_protocol::mesh_term::plain_row)
            .collect();
        Some(history_page(
            u32::try_from(snap.total_rows).unwrap_or(u32::MAX),
            u32::try_from(snap.rows).unwrap_or(0),
            u32::try_from(snap.history_lines).unwrap_or(0),
            u32::try_from(snap.history_size).unwrap_or(0),
            from,
            want,
            &all,
        ))
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
        )
        // The pane's own answer to "may a paste be given newlines": see [`Screen`].
        .with_paste_mode(s.bracketed_paste))
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

    async fn claim(&mut self, id: &str, takeover: bool) -> Result<ClaimOutcome, String> {
        let v = self
            .0
            .lock()
            .await
            .handle(
                "claim",
                serde_json::json!({
                    "id": id,
                    "device": { "id": MESH_DEVICE, "name": "the mesh link", "kind": "mesh" },
                    "takeover": takeover,
                }),
            )
            .await?;
        if v.get("ok").and_then(|b| b.as_bool()).unwrap_or(false) {
            Ok(ClaimOutcome::Claimed)
        } else {
            Ok(ClaimOutcome::Held {
                holder: v
                    .get("holder")
                    .and_then(|h| h.as_str())
                    .unwrap_or("")
                    .to_owned(),
            })
        }
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

/// Slice a history page out of a keeper snapshot, in the keeper's own line numbering.
///
/// **Three counts, and confusing them is a silently empty page.** `total` is every line the pane
/// has (history first, live last); `available` is how many history lines exist; `captured` is how
/// many of them the keeper put in `text`, which is `available` clipped to what was asked for. The
/// live screen is the last `rows` lines of `total`.
///
/// A request above the live screen serves only history; one that reaches the live screen is clamped
/// back so a page never re-sends rows the phone already has; one older than what was fetched serves
/// from the oldest line in hand, so the phone's next press walks up from there rather than skipping.
///
/// A pure function because this is where the bug was: with the measured pane (`total 9912`,
/// `available 9895`, `rows 17`) the old arithmetic used `available`'s slot for `captured` and
/// answered every request with nothing.
fn history_page(
    total: u32,
    rows: u32,
    captured: u32,
    available: u32,
    from: u32,
    want: u32,
    lines: &[String],
) -> (u32, bool, Vec<String>) {
    let live_top = total.saturating_sub(rows);
    let oldest_captured = total.saturating_sub(captured);
    let oldest_available = total.saturating_sub(available);
    let start = from.max(oldest_captured).min(live_top);
    let end = start.saturating_add(want).min(live_top);
    // **A row is clamped to what a message can carry.** The wire's line field is one byte and the
    // protocol's segment bound is 120, so a wider pane would have its page silently shortened by a
    // failed encode; the phone's own grid is never wider than this anyway.
    let out = if end > start {
        let a = start.saturating_sub(oldest_captured) as usize;
        let b = end.saturating_sub(oldest_captured) as usize;
        lines
            .get(a..b)
            .map(|slice| {
                slice
                    .iter()
                    .map(|l| ldmux_protocol::mesh_term::clamp_row(l))
                    .collect()
            })
            .unwrap_or_default()
    } else {
        Vec::new()
    };
    (start, start > oldest_available, out)
}

/// How often the bridge looks at the screen. Interactive echo wants this small, the airtime
/// bound does not care: an update waits for an empty queue regardless.
pub const TICK_MS: u64 = 50;

/// How long a pane must be unchanged before the host says "nothing more is coming".
///
/// Above one round trip on this link (a record plus its ack is a few hundred milliseconds at the
/// deployed tuple) and well under the phone's own four-second quiet timer, so the marker is what
/// ends a wait in practice and the timer only covers a host that does not send one.
pub const IDLE_AFTER_MS: u128 = 700;

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
        + u64::from(stats.holds)
}

/// How many unacked records may be in flight before the bridge stops looking at the screen. It is
/// the session's own window, so the queue can never grow past what the link can carry.
pub const PENDING_LIMIT: usize = 8;

/// The message budget a frame is built within, which is **not** [`PENDING_LIMIT`].
///
/// `PENDING_LIMIT` is a window of **records**; the differ spends its argument in **messages**. They
/// were the same number until a frame was compressed, and then they were not: a 46x17 screen is
/// `Profile` + 9 `Rows` + `Commit` = 11 messages, so an 8-message budget split it into two ticks,
/// and — because a partial frame deliberately carries no `Commit` — the phone could paint nothing
/// until the second tick. Twelve messages let the phone's own grid go out whole, which is 2 records
/// once compressed: one record fewer and one tick sooner than the split, for the same airtime.
/// A desk-sized pane (43 messages) still splits, which is the pacing doing its job.
pub const FRAME_MESSAGES: usize = 12;

/// The most often a screen that keeps changing is sent, unless the change is the echo of input.
///
/// This is a coalescing window, not a delay added to every update: a change that arrives after a
/// quiet tick goes at once, so a command's output is as fast as the link allows. Continuous
/// output (a build, `yes`, a log) is one update a second instead of twenty, which on a link
/// where a short frame's preamble and header are a third of its airtime is the difference
/// between the screen being usable and the queue never draining. It is also the hard bound on
/// staleness: nothing coalesced waits longer than this.
pub const BURST_GAP: Duration = Duration::from_millis(1000);

/// How long the first frame after an attach waits for the phone's grid.
///
/// **Measured, not derived from the attach alone.** The grid rides inside the attach record, but that
/// record leaves *after* the handshake: hint ~0.19 s, msg1 ~0.46 s, msg2 ~0.25 s, attach ~0.19 s, so
/// the phone's `Resize` lands about 1.1 s after the attach begins. A 700 ms grace expired first and
/// the phone was drawn a 255x82 desk pane — about twenty thousand cells, twenty-odd records — before
/// its own grid arrived, which is exactly the case this constant exists to prevent. A `Resize` still
/// clears the hold outright, so this bounds only the fallback path.
const ATTACH_GRACE: Duration = Duration::from_millis(1_400);

/// How often the holder of an attached shell is re-checked.
///
/// Ownership can leave without the pane's geometry moving, so this is the only reliable signal. One
/// listing call every few seconds is a keeper round trip; it costs no airtime and it is what stops
/// the phone drawing a shell the desk has taken.
const OWNER_CHECK: Duration = Duration::from_millis(5_000);

/// How long a phone may keep a shell unlit before the host starts drawing it again.
///
/// **A hold must not outlive the phone that asked for it.** The normal release is the phone's own
/// `Hold { on: false }` when the operator presses sync or show, and a phone that goes away ends the
/// session with it. This is the backstop for the case neither happens — a client that froze the
/// view and then stopped sending — because a shell nobody draws and nobody releases is a shell the
/// next viewer finds blank for a reason nothing records.
const HOLD_MAX: Duration = Duration::from_secs(600);

/// How often a held shell's screen is read.
///
/// Enough to notice a shell that ended, and a sixtieth of the tick's own cadence: a held shell
/// builds nothing, so the only reason to read it is to see whether it is still there.
const HELD_SCREEN_MS: Duration = Duration::from_millis(1_000);

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
        /// Refuse every send once this many records have been handed over: a link that takes
        /// part of a reply, which is how a full queue really behaves.
        refuse_after: Option<usize>,
        /// What one record costs on this link, when a test sets it.
        airtime: Option<u32>,
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
            if s.refuse || s.refuse_after.is_some_and(|n| s.sent.len() >= n) {
                return Err("full".into());
            }
            s.sent.push((shell, bytes.to_vec()));
            Ok(())
        }
        fn take_inbound(&self) -> Vec<(u8, Vec<u8>)> {
            std::mem::take(&mut self.0.lock().unwrap().inbound)
        }
        fn airtime_ms(&self) -> Option<u32> {
            self.0.lock().unwrap().airtime
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
        /// Shells whose holder has typed recently: what a claim refuses over.
        engaged: std::collections::HashSet<String>,
        /// The marks table the host would send on attach.
        marks: Vec<(String, u8)>,
        /// The scrollback above the live screen, oldest first (see `history`).
        history: Vec<String>,
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
        async fn listing(&mut self) -> Option<Vec<ldmux_protocol::mesh_term::SessionSummary>> {
            let s = self.0.lock().unwrap();
            if s.fail_list {
                return None;
            }
            Some(
                s.running
                    .iter()
                    .map(|id| ldmux_protocol::mesh_term::SessionSummary {
                        id: id.clone(),
                        label: format!("label of {id}"),
                        cwd: format!("/work/{id}"),
                        origin: "phone".to_string(),
                        owner: s.holders.get(id).cloned().unwrap_or_default(),
                        shell: "bash".to_string(),
                    })
                    .collect(),
            )
        }
        async fn marks(&mut self) -> Vec<(String, u8)> {
            self.0.lock().unwrap().marks.clone()
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

        async fn holder(&mut self, id: &str) -> Option<String> {
            Some(
                self.0
                    .lock()
                    .unwrap()
                    .holders
                    .get(id)
                    .cloned()
                    .unwrap_or_default(),
            )
        }

        async fn history(&mut self, _id: &str, from: u32, rows: u8) -> Option<(u32, bool, Vec<String>)> {
            let s = self.0.lock().unwrap();
            // **The fake answers like a pane with nothing dropped**: every history line is still
            // there (`available` = all of it), so its numbering starts at 0 and `older` is "does a
            // line exist above this page". A request at or past the live screen serves nothing and
            // still reports the history above it, which is what the real host does.
            let live_top = s.history.len() as u32;
            if from >= live_top {
                return Some((live_top, live_top > 0, Vec::new()));
            }
            let start = from;
            let end = (start + u32::from(rows)).min(live_top);
            let lines = s.history[start as usize..end as usize].to_vec();
            Some((start, start > 0, lines))
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
        /// The double holds a shell the way the daemon does: an *engaged* holder refuses, an idle
        /// one is displaced, and the shell is the link's afterwards.
        async fn claim(&mut self, id: &str, takeover: bool) -> Result<ClaimOutcome, String> {
            let mut s = self.0.lock().unwrap();
            s.claimed.push(("claim", id.to_owned()));
            let holder = s.holders.get(id).cloned().unwrap_or_default();
            if holder.is_empty() || holder == "mesh" {
                s.holders.insert(id.to_owned(), "mesh".into());
                return Ok(ClaimOutcome::Claimed);
            }
            if !takeover && s.engaged.contains(id) {
                return Ok(ClaimOutcome::Held { holder });
            }
            s.holders.insert(id.to_owned(), "mesh".into());
            Ok(ClaimOutcome::Claimed)
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
            bracketed_paste: false,
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

    // ── a paste is not a keystroke ────────────────────────────────────────────

    #[test]
    fn a_paste_is_wrapped_only_where_the_pane_asked_for_bracketed_paste() {
        // What the pane negotiated decides, and only the host can read it.
        assert_eq!(paste_bytes(b"one\ntwo", true), b"\x1b[200~one\ntwo\x1b[201~");
        assert_eq!(paste_bytes(b"one\ntwo", false), b"one\rtwo");
        // Typed text has no newline and is owed nothing: a keystroke is not a paste.
        assert_eq!(paste_bytes(b"ls", true), b"ls");
        assert_eq!(paste_bytes(b"ls", false), b"ls");
        // CRLF folded first, or every pasted line gains a blank line after it.
        assert_eq!(paste_bytes(b"one\r\ntwo", true), b"\x1b[200~one\ntwo\x1b[201~");
        assert_eq!(paste_bytes(b"one\r\ntwo", false), b"one\rtwo");
        // A clipboard holding the closing marker must not end the paste early: ESC goes with it,
        // and only where a wrap happened — there is nothing to close early in a typed record.
        assert_eq!(
            paste_bytes(b"a\x1b[201~\nrm -rf /", true),
            b"\x1b[200~a[201~\nrm -rf /\x1b[201~"
        );
        assert_eq!(paste_bytes(b"a\x1b[201~", true), b"a\x1b[201~");
    }

    #[tokio::test]
    async fn a_multiline_paste_arrives_as_text_where_the_pane_asked_for_it() {
        let (mut b, host, link) = setup();
        host.0.lock().unwrap().screens.insert(
            "abcd-1".into(),
            Screen {
                bracketed_paste: true,
                ..screen(&["$", ""])
            },
        );
        phone_says(
            &link,
            3,
            &Message::Attach {
                version: VERSION,
                selector: String::new(),
            },
        );
        b.tick().await;
        phone_says(&link, 3, &Message::Input(b"one\ntwo\n".to_vec()));
        b.tick().await;
        assert_eq!(
            host.0.lock().unwrap().typed,
            vec![("abcd-1".to_owned(), b"\x1b[200~one\ntwo\n\x1b[201~".to_vec())],
            "every pasted line must be inserted, not run"
        );
    }

    #[tokio::test]
    async fn a_multiline_paste_is_carriage_returns_where_it_did_not() {
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
        phone_says(&link, 3, &Message::Input(b"one\ntwo\n".to_vec()));
        b.tick().await;
        assert_eq!(
            host.0.lock().unwrap().typed,
            vec![("abcd-1".to_owned(), b"one\rtwo\r".to_vec())],
            "a pane with no bracketed paste can only be given lines, which is what every terminal does"
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

    /// **The host says when the answer has finished arriving.** After input, once the pane has
    /// stopped changing, one `Commit` with the idle flag goes out and no more; and a repaint nobody
    /// typed for sends no marker at all.
    #[tokio::test]
    async fn the_host_says_when_a_pane_has_settled_after_input() {
        let (mut b, host, link) = setup();
        let clock = FakeClock::new();
        b.clock = clock.reader();
        phone_says(
            &link,
            3,
            &Message::Attach {
                version: VERSION,
                selector: "abcd".into(),
            },
        );
        b.tick().await;
        // Nobody typed: a settled pane is not worth a record.
        b.tick().await;
        clock.advance(2_000);
        let quiet = link.0.lock().unwrap().sent.len();

        phone_says(&link, 3, &Message::Input(b"ls\r".to_vec()));
        host.0
            .lock()
            .unwrap()
            .screens
            .insert("abcd-1".into(), screen(&["$ ls", "file"]));
        clock.advance(60);
        b.tick().await;
        clock.advance(60);
        b.tick().await;
        clock.advance(900);
        b.tick().await;
        b.tick().await;
        // **One frame answered it, so no marker.** The frame that carries the answer already ends
        // the phone's wait; a second record to say "and nothing more" was a whole record (0.6 s of a
        // 1 % duty cycle) per keystroke, a third of the airtime of typing.
        let after = link.0.lock().unwrap().sent[quiet..].to_vec();
        let mut decoded: Vec<Message> = Vec::new();
        for (_, bytes) in &after {
            let mut d = ldmux_protocol::mesh_term::Decoder::default();
            if let Ok(items) = d.push(bytes) {
                decoded.extend(items.into_iter().flatten());
            }
        }
        let markers: Vec<_> = decoded
            .iter()
            .filter(|m| {
                matches!(m, Message::Commit { flags, .. }
                    if flags & ldmux_protocol::mesh_term::commit::IDLE != 0)
            })
            .collect();
        assert_eq!(markers.len(), 0, "a one-frame answer needs no marker");

        // An answer that takes two frames does need one: the phone cannot tell a slow answer from a
        // finished one, so the host says it once the pane has settled.
        phone_says(&link, 3, &Message::Input(b"x".to_vec()));
        let n = link.0.lock().unwrap().sent.len();
        host.0
            .lock()
            .unwrap()
            .screens
            .insert("abcd-1".into(), screen(&["$ ls", "file", "more"]));
        clock.advance(60);
        b.tick().await;
        // A second frame for the same answer, past the burst gap so it is its own frame: now the
        // phone cannot tell whether more is coming.
        clock.advance(1_100);
        host.0
            .lock()
            .unwrap()
            .screens
            .insert("abcd-1".into(), screen(&["$ ls", "file", "more", "again"]));
        b.tick().await;
        clock.advance(900);
        b.tick().await;
        b.tick().await;
        let mut decoded: Vec<Message> = Vec::new();
        for (_, bytes) in &link.0.lock().unwrap().sent[n..] {
            let mut d = ldmux_protocol::mesh_term::Decoder::default();
            if let Ok(items) = d.push(bytes) {
                decoded.extend(items.into_iter().flatten());
            }
        }
        let markers: Vec<_> = decoded
            .iter()
            .filter(|m| {
                matches!(m, Message::Commit { flags, .. }
                    if flags & ldmux_protocol::mesh_term::commit::IDLE != 0)
            })
            .collect();
        assert_eq!(markers.len(), 1, "one marker for a two-frame answer, in {:?}", decoded);

        // Nothing more changes: no second marker.
        let n = link.0.lock().unwrap().sent.len();
        b.tick().await;
        b.tick().await;
        assert_eq!(link.0.lock().unwrap().sent.len(), n, "one marker per burst");
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

    /// **The launcher needs the table too.** It lists without attaching, so a table sent only with
    /// the attach left every row unmarked until a shell was opened (measured on the device,
    /// 2026-11-06).
    #[tokio::test]
    async fn a_list_answer_carries_the_marks_table() {
        let (mut b, host, link) = setup();
        host.0.lock().unwrap().marks = vec![("abcd-1".into(), 0)];
        phone_says(&link, 1, &Message::List);
        b.tick().await;
        assert!(
            sent(&link, 1).iter().any(|m| matches!(
                m,
                Message::Marks { entries, .. } if entries.len() == 1 && entries[0].id == "abcd-1"
            )),
            "the table rides with the list: {:?}",
            sent(&link, 1)
        );
    }

    /// **The table is paged, because a message has a body ceiling the table does not.** A dozen
    /// marked shells already overflow `MAX_ROWS_BODY`, and `reply` drops a message it cannot encode,
    /// which would have been "no marks at all" exactly when there were many.
    ///
    /// The page order is asserted here too: `first` opens the run and `last` closes it, which is what
    /// lets the phone append the pages and replace its table once. Without them it replaced the table
    /// per page and showed only the last one.
    #[test]
    fn a_large_mark_table_is_paged_into_messages_that_fit() {
        let marks: Vec<(String, u8)> = (0..40).map(|i| (format!("s{i:09}"), (i % 2) as u8)).collect();
        let pages = marks_pages(marks.clone());
        assert!(pages.len() >= 2, "forty marks do not fit one message");
        let mut seen = 0;
        for (i, p) in pages.iter().enumerate() {
            let bytes = p.encode().expect("every page encodes");
            assert!(
                bytes.len() <= ldmux_protocol::mesh_term::MAX_MESSAGE,
                "a page is one message"
            );
            match p {
                Message::Marks {
                    first,
                    last,
                    entries,
                } => {
                    assert_eq!(*first, i == 0, "exactly the first page opens the run");
                    assert_eq!(*last, i + 1 == pages.len(), "only the last page closes it");
                    seen += entries.len();
                }
                other => panic!("a mark page is a Marks message: {other:?}"),
            }
        }
        assert_eq!(seen, marks.len(), "no mark is lost in the paging");
    }

    /// An empty table is still a complete run: the phone must be told to clear what it had.
    #[test]
    fn an_empty_mark_table_is_one_page_that_opens_and_closes() {
        let pages = marks_pages(Vec::new());
        assert_eq!(pages.len(), 1);
        assert!(matches!(
            &pages[0],
            Message::Marks {
                first: true,
                last: true,
                entries
            } if entries.is_empty()
        ));
    }

    /// **The marks a phone missed go out with the attach.** The radio cannot reach a phone that is
    /// not connected, so this is the only moment delivery is possible; a phone on a version below
    /// `MARKS_VERSION` is not sent the tag at all.
    #[tokio::test]
    async fn an_attach_carries_the_marks_table() {
        let (mut b, host, link) = setup();
        host.0.lock().unwrap().marks = vec![("abcd-1".into(), 1), ("zz99".into(), 0)];
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
            sent(&link, 1).iter().any(|m| matches!(
                m,
                Message::Marks { entries, .. }
                    if entries.len() == 2 && entries[0].id == "abcd-1" && entries[0].kind == 1
            )),
            "the table goes with the attach: {:?}",
            sent(&link, 1)
        );
    }

    /// **An empty table is still a message.** It is how the phone learns that a mark it still draws
    /// has expired on the host, or was read on the desk: skipping the message left the stale row on
    /// the launcher until the app restarted.
    #[tokio::test]
    async fn an_empty_mark_table_is_still_sent_on_attach() {
        let (mut b, _host, link) = setup();
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
            sent(&link, 1).iter().any(|m| matches!(
                m,
                Message::Marks { first: true, last: true, entries } if entries.is_empty()
            )),
            "the empty table travels too: {:?}",
            sent(&link, 1)
        );
    }

    #[tokio::test]
    async fn a_phone_below_the_marks_version_is_not_sent_the_table() {
        let (mut b, host, link) = setup();
        host.0.lock().unwrap().marks = vec![("abcd-1".into(), 1)];
        phone_says(
            &link,
            1,
            &Message::Attach {
                version: MARKS_VERSION - 1,
                selector: "abcd-1".into(),
            },
        );
        b.tick().await;
        assert!(
            !sent(&link, 1)
                .iter()
                .any(|m| matches!(m, Message::Marks { .. })),
            "an older phone is not spent a record on: {:?}",
            sent(&link, 1)
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
                version: VERSION + 1,
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
                    shell: _,
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
                shell: _,
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
        // The profile, not "the first record": a version-9 phone is also sent the mark table (empty
        // here), and that page is a record of its own.
        let first = |shell: u8| {
            sent.iter()
                .filter(|(s, _)| *s == shell)
                .flat_map(|(_, bytes)| Decoder::new().push(bytes).unwrap())
                .find_map(|m| match m {
                    Ok(Message::Profile { .. }) => Some(m.unwrap()),
                    _ => None,
                })
                .expect("a profile was sent")
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
        // A length over the cap ends the attach. The cap is a message bound now (600), so the
        // oversize is one past it rather than a fixed 301.
        let over = (ldmux_protocol::mesh_term::MAX_BODY + 1) as u16;
        link.0.lock().unwrap().inbound.push((
            1,
            vec![0x02, (over & 0x7f) as u8 | 0x80, (over >> 7) as u8],
        ));
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

    /// **A burst coalesces for two airtimes, not for a fixed second.**
    ///
    /// The second was written on a 75 ms bench tuple, where it is thirteen records of coalescing
    /// where two would do — the user waits a second for a screen that could have been drawn in
    /// 150 ms. On the public preset two airtimes is 1.2 s, capped at the old second so the slow
    /// link is unchanged.
    #[tokio::test]
    async fn a_burst_on_a_fast_link_coalesces_for_less_than_a_second() {
        // The airtime is read **at attach**, so it is set before the attach: a link whose pace
        // changes under a live attach keeps the pace the attach began with.
        let (mut b, host, link) = setup();
        let clock = FakeClock::new();
        b.clock = clock.reader();
        link.0.lock().unwrap().airtime = Some(75);
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
        clock.advance(ATTACH_GRACE.as_millis() as u64 + 50);
        b.tick().await;
        clock.advance(2_000);
        b.tick().await;

        let base = sent_records(&link);
        clock.advance(500);
        paced(&host, &["a", ""]);
        b.tick().await;
        assert_eq!(sent_records(&link) - base, 1, "the first change after quiet goes");
        clock.advance(100);
        paced(&host, &["a", "b"]);
        b.tick().await;
        assert_eq!(
            sent_records(&link) - base,
            1,
            "a change inside the window is coalesced"
        );
        clock.advance(250);
        b.tick().await;
        assert_eq!(
            sent_records(&link) - base,
            2,
            "and goes at two airtimes, not at a whole second"
        );
    }

    /// **A resync asked for while the screen is already on its way costs one repaint.** The phone
    /// re-asks on a timer, and a reset with a frame in flight queued a second full frame behind the
    /// first: that is the loop that spent about 20 s of air a minute on an attached shell nobody
    /// was typing in. The request is remembered and honoured when the pipe is empty.
    #[tokio::test]
    async fn a_resync_while_the_screen_is_in_flight_costs_one_repaint() {
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
        let base = sent_records(&link);
        // The phone asks again while records are still on their way.
        link.0.lock().unwrap().pending = 2;
        phone_says(&link, 1, &Message::Resync);
        b.tick().await;
        assert_eq!(
            sent_records(&link),
            base,
            "nothing is built into a pipe that already owes records"
        );
        // The pipe empties: the request is honoured, once.
        link.0.lock().unwrap().pending = 0;
        b.tick().await;
        let after = link.0.lock().unwrap().sent[base..].to_vec();
        assert!(!after.is_empty(), "the deferred resync is honoured");
        let mut d = Decoder::new();
        let first = d.push(&after[0].1).unwrap().remove(0).unwrap();
        assert!(
            matches!(first, Message::Profile { .. }),
            "a full screen starts with Profile"
        );
        let settled = sent_records(&link);
        b.tick().await;
        assert_eq!(sent_records(&link), settled, "one repaint, not a loop");
    }

    /// **A phone can ask for the shell's own scrollback, and gets lines it never saw.**
    ///
    /// The link carries one screen; the keeper carries the pane's history. Pressing the scroll pad's
    /// up key on a screen the phone joined late used to answer "no history yet on this link" — true
    /// and useless, because the rows exist on the host. This asks for them by absolute line number
    /// so a page joins what the phone holds.
    /// **The three counts of a keeper snapshot, at the numbers the deployed pane reported.**
    ///
    /// `totalRows 9912`, `historySize 9895`, `rows 17`, and `historyLines` = whatever was asked
    /// for. The bug this pins: the old code treated the *requested* history count as the captured
    /// one, so `oldest` became `total` and every page was empty. The measured pane is also the
    /// proof that `historySize` is the number a page is clamped against.
    #[test]
    fn a_history_page_is_sliced_in_the_keepers_numbering() {
        // 64 history lines captured (lines 9848..9912) plus the 17 live rows (9912..9929).
        let text: Vec<String> = (9848..9929).map(|n| format!("line {n}")).collect();
        assert_eq!(text.len(), 81, "64 history + 17 live");
        // One page just above the live screen: lines 9879..9895.
        let (start, older, page) = history_page(9912, 17, 64, 9895, 9879, 16, &text);
        assert_eq!(start, 9879);
        assert!(older, "the pane kept 9895 lines, so there is more above this page");
        assert_eq!(page.len(), 16);
        assert_eq!(page[0], "line 9879");

        // A request that reaches into the live screen is clamped back to its first row.
        let (start, _, page) = history_page(9912, 17, 64, 9895, 9890, 16, &text);
        assert_eq!(start, 9890);
        assert_eq!(page.len(), 5, "9890..9895 is all the history left");

        // Older than what this snapshot captured: serve from the oldest line in hand, and say so,
        // so the phone's next press walks up from there instead of skipping a gap.
        let (start, older, page) = history_page(9912, 17, 64, 9895, 100, 16, &text);
        assert_eq!(start, 9848, "the oldest captured line");
        assert!(older, "there is more history above this page");
        assert_eq!(page[0], "line 9848");

        // Nothing above the live screen at all (a fresh pane): an empty page, not a wrong one.
        let (start, older, page) = history_page(17, 17, 0, 0, 0, 16, &text[..17]);
        assert_eq!(start, 0);
        assert!(!older);
        assert!(page.is_empty());
    }

    /// Collect the history pages of one answer: their lines in order, and the flags the *final*
    /// page carries (which is where `last` lives; `older` is on every page).
    fn history_answer(link: &FakeLink, base: usize) -> (Vec<String>, bool, bool) {
        let sent = link.0.lock().unwrap().sent[base..].to_vec();
        let mut d = Decoder::new();
        let pages: Vec<(u32, bool, bool, Vec<String>)> = sent
            .iter()
            .flat_map(|(_, bytes)| d.push(bytes).unwrap())
            .filter_map(|m| m.ok())
            .filter_map(|m| match m {
                Message::HistoryPage {
                    from,
                    last,
                    older,
                    lines,
                } => Some((from, last, older, lines)),
                _ => None,
            })
            .collect();
        assert!(!pages.is_empty(), "an answer, even an empty one");
        let mut lines = Vec::new();
        let mut older = true;
        let mut last = false;
        for (i, (from, l, o, ls)) in pages.iter().enumerate() {
            // **Each page says where it starts**, so a caller can place it rather than assume it
            // joins. Rows are consecutive: page n+1 starts where page n ended.
            assert_eq!(*from, pages[0].0 + lines.len() as u32, "page {i} is misplaced");
            lines.extend(ls.iter().cloned());
            last = *l;
            older = *o;
            assert_eq!(*l, i + 1 == pages.len(), "only the final page ends the answer");
        }
        (lines, last, older)
    }

    /// **The scrollback contract, end to end through the wire.**
    ///
    /// The phone asks for an absolute line of the keeper's numbering; the answer is the rows from
    /// there, `last` when the answer is complete, and `older` when the pane holds history above it.
    /// The distinction is the whole point: an answer that ends without saying whether anything is
    /// above forces the phone to spend a record discovering it, and one that says "no more" too
    /// early strands the operator at the wrong top.
    #[tokio::test]
    async fn a_phone_can_ask_for_the_shells_own_scrollback() {
        let (mut b, host, link) = setup();
        host.0.lock().unwrap().history = (0..10).map(|i| format!("line {i}")).collect();
        phone_says(
            &link,
            1,
            &Message::Attach {
                version: VERSION,
                selector: String::new(),
            },
        );
        b.tick().await;

        // The middle of the history: four rows, and there is more above them.
        let base = sent_records(&link);
        phone_says(&link, 1, &Message::WantHistory { from: 6, rows: 4 });
        b.tick().await;
        let (lines, last, older) = history_answer(&link, base);
        assert_eq!(lines, vec!["line 6", "line 7", "line 8", "line 9"]);
        assert!(last, "one page answers the whole request");
        assert!(older, "lines 0..5 are above this page");

        // The oldest row the pane kept: nothing above it, so the phone can say "start of history"
        // without spending a record to find out.
        let base = sent_records(&link);
        phone_says(&link, 1, &Message::WantHistory { from: 0, rows: 4 });
        b.tick().await;
        let (lines, last, older) = history_answer(&link, base);
        assert_eq!(lines, vec!["line 0", "line 1", "line 2", "line 3"]);
        assert!(last);
        assert!(!older, "this is the oldest row the shell kept");

        // A request that reaches the live screen is clamped to the history: the phone already has
        // those rows, and re-sending them would cost a record to say nothing.
        let base = sent_records(&link);
        phone_says(&link, 1, &Message::WantHistory { from: 8, rows: 4 });
        b.tick().await;
        let (lines, last, older) = history_answer(&link, base);
        assert_eq!(lines, vec!["line 8", "line 9"]);
        assert!(last);
        assert!(older);

        // A request at or past the live screen is answered with an empty page, not with silence:
        // silence is indistinguishable from a lost request, and the phone would wait for ever.
        let base = sent_records(&link);
        phone_says(&link, 1, &Message::WantHistory { from: 99, rows: 4 });
        b.tick().await;
        let (lines, last, older) = history_answer(&link, base);
        assert!(lines.is_empty());
        assert!(last);
        assert!(older, "history exists above the live screen, so ask again lower down");
    }

    /// **A page too big for one message is split by bytes, and every page says where it starts.**
    ///
    /// A body is 600 bytes, so a screenful of wide rows is several pages. The absolute line of
    /// each page is what makes them reassemble; `older` rides every page, and only the last ends
    /// the answer. The rows here are box drawing, like a pane's own frames: three bytes each, which
    /// is why the clamp has to count bytes (`clamp_row`) and not characters — a row that will not
    /// encode takes its whole page with it, and the operator would press up into silence.
    #[tokio::test]
    async fn a_history_answer_pages_by_bytes_and_keeps_the_numbering() {
        let (mut b, host, link) = setup();
        host.0.lock().unwrap().history = (0..6)
            .map(|i| format!("{i}{}", "─".repeat(120)))
            .collect();
        phone_says(
            &link,
            1,
            &Message::Attach {
                version: VERSION,
                selector: String::new(),
            },
        );
        b.tick().await;
        let base = sent_records(&link);
        phone_says(&link, 1, &Message::WantHistory { from: 0, rows: 6 });
        b.tick().await;
        let (lines, last, older) = history_answer(&link, base);
        assert_eq!(lines.len(), 6, "every row arrives");
        // 120 bytes of three-byte characters is 40 of them, and the row still encodes. A clamp
        // that counted characters would have passed 360 bytes to the encoder and lost the page.
        assert_eq!(lines[0].chars().count(), 40, "clamped by bytes, never split mid-character");
        assert_eq!(lines[0].len(), 118, "one ASCII digit plus thirty-nine dashes");
        assert!(lines[0].len() <= ldmux_protocol::mesh_term::MAX_SEGMENT);
        assert!(last);
        assert!(!older);
        // Six rows of 118 bytes cannot fit one body, so the answer pages — and still reassembles
        // in order, which is what the per-page `from` guarantees (checked in `history_answer`).
        // Records are not the unit here: identical rows compress into one frame, and that is a win,
        // not a page.
        assert!(lines[0].starts_with('0'), "the first row of the page");
        assert!(lines[5].starts_with('5'), "and the last, five rows later");
    }

    /// **A list answered with nothing attached is framed too.**
    ///
    /// The launcher asks *because* it is not attached, and a `Close` asks for the list after the
    /// attach was removed: reading the version from the live attach made every such answer one
    /// record per page and unframed — eleven records for a nineteen-shell list, about 6 s of air.
    /// The host's own decoder inflates a frame, so "framed" is visible as pages arriving in fewer
    /// records than there are pages.
    #[tokio::test]
    async fn a_list_answered_without_an_attach_is_framed() {
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
        // The phone closes the page it opened: the version is remembered, the attach is gone.
        phone_says(&link, 1, &Message::Detach);
        b.tick().await;
        host.0.lock().unwrap().running = (0..12).map(|i| format!("shell-{i:02}")).collect();
        let base = sent_records(&link);
        phone_says(&link, 1, &Message::List);
        b.tick().await;
        let sent = link.0.lock().unwrap().sent[base..].to_vec();
        let pages = count_pages(&sent);
        assert!(pages >= 3, "twelve shells are paged: {pages} pages");
        assert!(
            sent.len() < pages,
            "{pages} pages must travel in fewer than {pages} records, got {}",
            sent.len()
        );
    }

    /// **A list too big for one frame body is split between frames, not pages.**
    ///
    /// A frame is one message and a message body has a ceiling; forty-eight shells deflate past it.
    /// Falling back to one record per page would spend twenty-odd records on a list that is four.
    #[tokio::test]
    async fn a_full_list_is_split_between_frames_not_pages() {
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
        host.0.lock().unwrap().running = (0..48).map(|i| format!("shell-{i:02}")).collect();
        let base = sent_records(&link);
        phone_says(&link, 1, &Message::List);
        b.tick().await;
        let sent = link.0.lock().unwrap().sent[base..].to_vec();
        let pages = count_pages(&sent);
        assert!(pages >= 8, "forty-eight shells are paged: {pages} pages");
        assert!(
            sent.len() < pages / 2,
            "{pages} pages in fewer than {} records, got {}",
            pages / 2,
            sent.len()
        );
    }

    /// **A synced line's echo waits one airtime for its answer, and they leave together.**
    ///
    /// The phone draws the line it sent, so the echo is not news; the answer's frame carries the
    /// echoed row anyway. Waiting one record's airtime merges them into one frame — and waiting
    /// longer than a record would be a latency loss dressed as an airtime win, so the wait is the
    /// link's own airtime, not a constant.
    #[tokio::test]
    async fn a_synced_lines_echo_rides_its_answer() {
        let (mut b, host, link, clock) = attached_paced().await;
        // The host reports a 600 ms record, as the public preset does.
        link.0.lock().unwrap().airtime = Some(600);
        let base = sent_records(&link);
        // The phone releases the hold and sends the line in one turn (two messages, two records
        // only because this link does not pack; the host's own answers are what is counted).
        phone_says(&link, 1, &Message::Hold { on: false });
        phone_says(&link, 1, &Message::Input(b"echo hi".to_vec()));
        b.tick().await;
        // The pty echoes the line; nothing may go out yet.
        paced(&host, &["$ echo hi", ""]);
        clock.advance(50);
        b.tick().await;
        assert_eq!(
            sent_records(&link),
            base,
            "the echo waits for the answer it belongs with"
        );
        // The answer lands inside the same airtime: one frame, not two.
        paced(&host, &["$ echo hi", "hi", ""]);
        clock.advance(600);
        b.tick().await;
        assert_eq!(
            sent_records(&link) - base,
            1,
            "echo and answer are one frame"
        );
    }

    /// **A full launcher list travels in fewer records than its pages.**
    ///
    /// The pages are the other answer the host spends several records on: a dozen shells is ~700
    /// bytes of repeated words, and a frame's inner bytes are fed back through the phone's own
    /// decoder, so the paging and its `last` flag are unchanged.
    #[tokio::test]
    async fn a_full_launcher_list_is_framed_into_fewer_records() {
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
        // A dozen shells, and the attached one still among them: this is a launcher refresh.
        host.0.lock().unwrap().running = (0..12).map(|i| format!("shell-{i:02}")).collect();
        let base = sent_records(&link);
        phone_says(&link, 1, &Message::List);
        b.tick().await;
        let sent = link.0.lock().unwrap().sent[base..].to_vec();
        let mut pages = 0usize;
        let mut d = Decoder::new();
        for (_, bytes) in &sent {
            for item in d.push(bytes).unwrap() {
                match item.unwrap() {
                    Message::Frame { inner } | Message::FrameDict { inner } => {
                        let mut inner_decoder = Decoder::new();
                        for m in inner_decoder.push(&inner).unwrap() {
                            if matches!(m.unwrap(), Message::Sessions { .. }) {
                                pages += 1;
                            }
                        }
                    }
                    Message::Sessions { .. } => pages += 1,
                    _ => {}
                }
            }
        }
        assert!(pages >= 3, "twelve shells are paged: {pages} pages");
        assert!(
            sent.len() < pages,
            "{pages} pages must travel in fewer than {pages} records, got {}",
            sent.len()
        );
    }

    /// **The profile and the marks are one record.** Two replies were two records, and the attach
    /// page is usually an empty one: 0.6 s of air at the public preset for nothing.
    #[tokio::test]
    async fn the_profile_and_the_marks_share_a_record() {
        let (mut b, host, link) = setup();
        host.0.lock().unwrap().marks = vec![("abcd-1".into(), 1)];
        phone_says(
            &link,
            1,
            &Message::Attach {
                version: VERSION,
                selector: String::new(),
            },
        );
        b.tick().await;
        let first = link.0.lock().unwrap().sent[0].1.clone();
        let items = Decoder::new().push(&first).unwrap();
        assert!(matches!(
            items[0].as_ref().unwrap(),
            Message::Profile { .. }
        ));
        assert_eq!(items.len(), 2, "the marks ride the same record");
        assert!(matches!(items[1].as_ref().unwrap(), Message::Marks { .. }));
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
        // **The attach holds its first frame briefly** so the phone's own grid can arrive (see
        // [`ATTACH_GRACE`]), and then its full screen is a push like any other. Start the test after
        // both windows have closed, so "a step after quiet" is what the first change really is.
        clock.advance(ATTACH_GRACE.as_millis() as u64 + 50);
        b.tick().await;
        clock.advance(2_000);
        b.tick().await;
        (b, host, link, clock)
    }

    /// How many `Sessions` pages a list answer carried, whether framed or plain.
    ///
    /// The host's decoder inflates a frame into its inner messages, so a page is a page however it
    /// travelled; the record count against the page count is what says whether it was framed.
    fn count_pages(sent: &[(u8, Vec<u8>)]) -> usize {
        let mut pages = 0usize;
        let mut d = Decoder::new();
        for (_, bytes) in sent {
            for item in d.push(bytes).unwrap() {
                if matches!(item.unwrap(), Message::Sessions { .. }) {
                    pages += 1;
                }
            }
        }
        pages
    }

    fn sent_records(link: &FakeLink) -> usize {
        link.0.lock().unwrap().sent.len()
    }

    /// **A big frame goes compressed, and the phone still sees the same screen.**
    ///
    /// One full screen is about 900 bytes of text on a 143-byte record — seven records — and raw
    /// deflate on real terminal text is 1.7x to 8x, which is three records or fewer. A one-row echo
    /// must stay uncompressed: below [`ldmux_protocol::mesh_term::FRAME_COMPRESS_MIN`] the deflate
    /// header costs more than the text repeats.
    #[tokio::test]
    async fn a_big_frame_is_compressed_and_decodes_to_the_same_screen() {
        let (mut b, host, link) = setup();
        let clock = FakeClock::new();
        b.clock = clock.reader();
        let wide: Vec<String> = (0..17)
            .map(|i| format!("row {i:02} {}", "x".repeat(24)))
            .collect();
        let refs: Vec<&str> = wide.iter().map(String::as_str).collect();
        paced(&host, &refs);
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
        clock.advance(ATTACH_GRACE.as_millis() as u64 + 50);
        // A full screen is paced: the differ hands out a window of messages per tick, so the paint
        // finishes over several ticks. Ten is more than this screen needs and still bounded.
        for _ in 0..10 {
            b.tick().await;
            clock.advance(1_100);
        }
        // **Records, not messages: the decoder makes a frame transparent**, so the assertion is on
        // what the air carried. This screen is ~527 bytes of rows (615 with the messages' own
        // headers), which uncompressed is five records; compressed it is two or three.
        let records = link
            .0
            .lock()
            .unwrap()
            .sent
            .iter()
            .filter(|(s, _)| *s == 1)
            .count();
        assert!(
            records <= 3,
            "a 527-byte screen took {records} records; compression did not happen"
        );
        // The phone's decode of what was sent is still the screen itself.
        assert_eq!(phone_view(&link, 1).lines, wide);
    }

    /// **The phone's own grid paints in one tick.** A 46x17 screen is 11 messages — Profile, nine
    /// `Rows`, Commit — and a message budget of 8 split it across two ticks, where the partial frame
    /// deliberately carries no Commit, so the phone could paint nothing until the second one. The
    /// compressed frame is 2 records, well inside the window.
    #[tokio::test]
    async fn a_phone_sized_frame_paints_in_one_tick() {
        let (mut b, host, link) = setup();
        let clock = FakeClock::new();
        b.clock = clock.reader();
        let lines: Vec<String> = (0..17)
            .map(|i| format!("{i:>2} · {}", "cargo build --release".repeat(2)))
            .map(|l| l.chars().take(46).collect())
            .collect();
        let refs: Vec<&str> = lines.iter().map(String::as_str).collect();
        paced(&host, &refs);
        b.tick().await;
        phone_says(
            &link,
            1,
            &Message::Attach {
                version: VERSION,
                selector: "abcd-1".into(),
            },
        );
        phone_says(&link, 1, &Message::Resize { cols: 46, rows: 17 });
        b.tick().await;
        clock.advance(50);
        b.tick().await;
        let records = link.0.lock().unwrap().sent.iter().filter(|(s, _)| *s == 1).count();
        // The attach answers with the pane's geometry at once (so the phone can ask for its own),
        // then the frame: one profile record, then the compressed frame in two or three.
        assert!(
            records <= 4,
            "a 46x17 first paint is one profile plus one compressed frame: {records} records"
        );
        assert_eq!(
            phone_view(&link, 1).lines,
            lines,
            "and it is paintable after one tick"
        );
    }

    /// **A re-attach is drawn at the phone's grid, not the desk's.** The pty keeps whatever a desk
    /// window left it at, and waiting for the phone's `Resize` is a race: measured 2026-11-09, a
    /// 255x82 desk pane was committed before a 46x17 grid arrived. The grid from the last attach is
    /// remembered per shell and applied *before* the first frame is read.
    #[tokio::test]
    async fn a_reattach_fits_the_pane_before_the_first_frame() {
        let (mut b, host, link) = setup();
        let clock = FakeClock::new();
        b.clock = clock.reader();
        // The desk has left this shell at a wide geometry.
        host.0
            .lock()
            .unwrap()
            .screens
            .insert("abcd-1".into(), screen(&["a desk frame"]));
        phone_says(
            &link,
            1,
            &Message::Attach {
                version: VERSION,
                selector: "abcd-1".into(),
            },
        );
        b.tick().await;
        // The phone says its own grid and later detaches.
        phone_says(&link, 1, &Message::Resize { cols: 46, rows: 17 });
        b.tick().await;
        phone_says(&link, 1, &Message::Detach);
        b.tick().await;
        // Re-attach: the pane is fitted to 46x17 before a frame is built, so the desk geometry is
        // never what the phone is drawn with.
        phone_says(
            &link,
            1,
            &Message::Attach {
                version: VERSION,
                selector: "abcd-1".into(),
            },
        );
        b.tick().await;
        clock.advance(200);
        b.tick().await;
        let profile = link
            .0
            .lock()
            .unwrap()
            .sent
            .iter()
            .filter(|(s, _)| *s == 1)
            .flat_map(|(_, bytes)| Decoder::new().push(bytes).unwrap())
            .filter_map(|m| m.ok())
            .filter_map(|m| match m {
                Message::Profile { cols, rows, .. } => Some((cols, rows)),
                _ => None,
            })
            .next_back()
            .expect("a profile");
        assert_eq!(profile, (46, 17), "the remembered grid, not the desk's");
    }

    /// **A re-attach does not wait for a grid it already remembers.** The grace exists because the
    /// pane's first size is whatever the desk left it at. When this phone has attached before, the
    /// pane is fitted to its remembered grid before the first frame is read, so the wait has nothing
    /// to wait for: spending it was 1.4 s of blank screen per re-attach.
    #[tokio::test]
    async fn a_reattach_does_not_wait_again_for_the_remembered_grid() {
        let (mut b, _host, link) = setup();
        let clock = FakeClock::new();
        b.clock = clock.reader();
        phone_says(
            &link,
            1,
            &Message::Attach {
                version: VERSION,
                selector: "abcd-1".into(),
            },
        );
        b.tick().await;
        phone_says(&link, 1, &Message::Resize { cols: 46, rows: 17 });
        b.tick().await;
        phone_says(&link, 1, &Message::Detach);
        b.tick().await;

        let base = sent_records(&link);
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
            sent_records(&link) - base >= 2,
            "the profile and the first frame go in the attach's own tick"
        );
    }

    /// **A refused send re-offers the same messages, and does not start the screen over.**
    ///
    /// The differ advances its record of the phone's screen as it hands rows out, so a record the
    /// radio refused is a row the phone never sees unless it is offered again as *that* message.
    /// Resyncing reset the record and made the next frame a full screen — measured 2026-11-09, a
    /// full screen plus a profile every ten seconds on an idle link.
    #[tokio::test]
    async fn a_refused_send_is_offered_again_rather_than_reset() {
        let (mut b, host, link) = setup();
        let clock = FakeClock::new();
        b.clock = clock.reader();
        paced(&host, &["$ one", "$ two"]);
        phone_says(
            &link,
            1,
            &Message::Attach {
                version: VERSION,
                selector: "abcd-1".into(),
            },
        );
        b.tick().await;
        clock.advance(1_200);
        b.tick().await;
        // The radio refuses everything for one tick.
        link.0.lock().unwrap().refuse = true;
        paced(&host, &["$ one", "$ two", "$ three"]);
        clock.advance(1_100);
        b.tick().await;
        let refusals = b.stats.refused_sends;
        assert!(refusals > 0, "the send was refused");
        // The link takes records again: the refused rows arrive without a full repaint.
        link.0.lock().unwrap().refuse = false;
        clock.advance(1_100);
        b.tick().await;
        assert_eq!(
            phone_view(&link, 1).lines,
            ["$ one", "$ two", "$ three"],
            "the refused frame is delivered, not abandoned"
        );
    }

    /// **Ownership can leave without the geometry moving, and the phone is told anyway.**
    #[tokio::test]
    async fn a_shell_taken_at_the_same_grid_is_still_a_takeover() {
        let (mut b, host, link) = setup();
        let clock = FakeClock::new();
        b.clock = clock.reader();
        paced(&host, &["a phone frame"]);
        phone_says(
            &link,
            1,
            &Message::Attach {
                version: VERSION,
                selector: "abcd-1".into(),
            },
        );
        b.tick().await;
        phone_says(&link, 1, &Message::Resize { cols: 46, rows: 17 });
        b.tick().await;
        let before = link.0.lock().unwrap().sent.len();
        // The desk takes the shell. The pane keeps the phone's grid, so nothing about the screen
        // says so; only the holder does.
        host.0
            .lock()
            .unwrap()
            .holders
            .insert("abcd-1".into(), "desktop".into());
        clock.advance(6_000);
        b.tick().await;
        let sent: Vec<Message> = link
            .0
            .lock()
            .unwrap()
            .sent[before..]
            .iter()
            .flat_map(|(_, bytes)| Decoder::new().push(bytes).unwrap())
            .filter_map(Result::ok)
            .collect();
        assert!(
            sent.iter().any(|m| matches!(m, Message::Closed { reason } if *reason == closed::HELD)),
            "the phone is told: {sent:?}"
        );
        assert!(!b.attached(1), "and it is detached");
    }

    /// **A screen that does not compress still paints.**
    ///
    /// Deflate can come out *larger* than its input on text that does not repeat — a hex dump, a
    /// column of random digits. The first version of the frame path measured nothing: it built the
    /// frame, `encode` refused it when the deflated bytes passed the message bound, the reply failed,
    /// the bridge resynced and rebuilt the identical frame, for ever — a screen that never arrived,
    /// with no counter to show it. So the packer must fall back to plain messages whenever the frame
    /// would not be *smaller*, and the screen must still paint.
    #[test]
    fn a_frame_carries_every_message_it_replaces() {
        use ldmux_protocol::mesh_term::{Differ, Screen};
        // Compressible on purpose: real terminal text repeats (paths, prompts, log prefixes).
        let lines: Vec<String> = (0..17)
            .map(|i| format!("   Compiling ldmux-protocol v0.1.0 (/home/ldeen/Documents/ldmux/crates/{i})"))
            .collect();
        let screen = Screen {
            cols: 80,
            rows: 17,
            cursor_x: 0,
            cursor_y: 0,
            alternate: false,
            dead: false,
            bracketed_paste: false,
            first_line: 0,
            lines,
        };
        let mut differ = Differ::new();
        // Enough room that the whole screen is one frame, which is the case the packer must get right.
        let messages = differ.update_within(&screen, 40);
        let packed = Bridge::<FakeHost, FakeLink>::pack_frame(&messages, true);
        let replaced = messages.len() - 1; // the profile stays outside the frame
        let frame = packed
            .iter()
            .find(|m| matches!(m, Message::Frame { .. } | Message::FrameDict { .. }))
            .cloned()
            .expect("a 17-row screen of real text must be compressed");
        let stream = match &frame {
            Message::Frame { inner } | Message::FrameDict { inner } => inner.clone(),
            other => unreachable!("just matched a frame: {other:?}"),
        };
        let mut d = Decoder::new();
        let inner = d.push(&stream).expect("the inner stream decodes");
        assert_eq!(inner.len(), replaced, "every message the frame replaced is inside it");
        assert!(inner.iter().all(Result::is_ok), "and none of them is malformed");
        assert!(
            matches!(inner.last(), Some(Ok(Message::Commit { .. }))),
            "the commit travels inside the frame too, or the phone never sees the cursor"
        );
        // And the frame is smaller than what it replaces, which is the only reason to send it.
        let plain: usize = messages[1..]
            .iter()
            .filter_map(|m| m.encode().ok())
            .map(|b| b.len())
            .sum();
        let framed = frame.encode().expect("the frame encodes").len();
        assert!(framed < plain, "frame {framed} vs plain {plain}");
    }

    #[tokio::test]
    async fn a_screen_that_does_not_compress_still_paints() {
        let (mut b, host, link) = setup();
        let clock = FakeClock::new();
        b.clock = clock.reader();
        // Noise: every row is a different random-looking string, so there is nothing to repeat.
        let mut seed = 0x2545_f491_4f6c_dd1du64;
        let wide: Vec<String> = (0..17)
            .map(|_| {
                (0..44)
                    .map(|_| {
                        seed ^= seed << 13;
                        seed ^= seed >> 7;
                        seed ^= seed << 17;
                        char::from(b'!' + (seed % 90) as u8)
                    })
                    .collect()
            })
            .collect();
        let refs: Vec<&str> = wide.iter().map(String::as_str).collect();
        paced(&host, &refs);
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
        clock.advance(ATTACH_GRACE.as_millis() as u64 + 50);
        // A full screen is paced: the differ hands out a window of messages per tick, so the paint
        // finishes over several ticks. Ten is more than this screen needs and still bounded.
        for _ in 0..10 {
            b.tick().await;
            clock.advance(1_100);
        }
        {
            // What is inside the frame itself, before any splicing.
            for (_, bytes) in link.0.lock().unwrap().sent.iter() {
                let mut d = Decoder::new();
                if let Ok(items) = d.push(bytes) {
                    for i in items.into_iter().flatten() {
                        if let Message::Frame { inner } = i {
                            let mut dd = Decoder::new();
                            let mut kinds: Vec<String> = Vec::new();
                            for it in dd.push(&inner).unwrap() {
                                kinds.push(match it { Ok(m) => format!("{:?}", std::mem::discriminant(&m)), Err(e) => format!("ERR{e:?}") });
                            }
                        }
                    }
                }
            }
            let mut d = Decoder::new();
            let mut names: Vec<String> = Vec::new();
            for (_, bytes) in link.0.lock().unwrap().sent.iter() {
                match d.push(bytes) {
                    Ok(items) => for i in items {
                        names.push(match i { Ok(m) => format!("{:?}", std::mem::discriminant(&m)), Err(e) => format!("ERR {e:?}") });
                    },
                    Err(e) => names.push(format!("STREAM ERR {e:?}")),
                }
            }
        }
        assert_eq!(
            phone_view(&link, 1).lines,
            wide,
            "an incompressible screen is sent plain, not dropped"
        );
        assert!(b.stats.records_sent > 0, "something reached the air");
    }

    /// **A phone below version 11 is never sent a compressed frame**, because it cannot read one.
    #[tokio::test]
    async fn a_version_10_phone_is_never_sent_a_frame() {
        let (mut b, host, link) = setup();
        let clock = FakeClock::new();
        b.clock = clock.reader();
        let wide: Vec<String> = (0..17).map(|i| format!("row {i:02} {}", "x".repeat(24))).collect();
        let refs: Vec<&str> = wide.iter().map(String::as_str).collect();
        paced(&host, &refs);
        b.tick().await;
        phone_says(
            &link,
            1,
            &Message::Attach {
                version: ldmux_protocol::mesh_term::FRAME_VERSION - 1,
                selector: "abcd-1".into(),
            },
        );
        b.tick().await;
        clock.advance(ATTACH_GRACE.as_millis() as u64 + 50);
        b.tick().await;
        clock.advance(100);
        b.tick().await;
        // Every record of that screen decodes without a frame in it: the phone reads plain Rows.
        for (_, bytes) in link.0.lock().unwrap().sent.iter() {
            let items = Decoder::new().push(bytes).unwrap();
            for item in items {
                let m = item.expect("a version-10 phone's stream stays readable");
                assert!(
                    !matches!(m, Message::Frame { .. }),
                    "a frame would be unreadable to this phone"
                );
            }
        }
        assert_eq!(phone_view(&link, 1).lines, wide);
    }

    /// **An attach does not draw the desk's grid.** The pane's pty is whatever a desk window left
    /// it at — measured 158x82 — and a full frame of that is about eighty records, which at the
    /// public tuple is a minute and a half of blank screen before the phone's `Resize` is even read.
    /// The first frame is held for [`ATTACH_GRACE`] so the grid can arrive first, and a phone that
    /// sends no resize still gets its screen when the grace ends.
    #[tokio::test]
    async fn an_attach_waits_for_the_phone_grid_before_drawing_the_desk_one() {
        let (mut b, host, link) = setup();
        let clock = FakeClock::new();
        b.clock = clock.reader();
        b.tick().await;
        paced(&host, &["a desk frame", ""]);
        phone_says(
            &link,
            1,
            &Message::Attach {
                version: VERSION,
                selector: String::new(),
            },
        );
        b.tick().await;
        // Inside the grace: the pane is attached, the phone has been told the pane's geometry (so it
        // can ask for its own), and **no row** has been sent yet.
        clock.advance(200);
        b.tick().await;
        let rows_sent = link
            .0
            .lock()
            .unwrap()
            .sent
            .iter()
            .filter(|(s, _)| *s == 1)
            .flat_map(|(_, bytes)| Decoder::new().push(bytes).unwrap())
            .filter(|m| matches!(m, Ok(Message::Rows { .. })))
            .count();
        assert_eq!(rows_sent, 0, "no rows before the phone's grid can arrive");
        // The phone's grid, then the frame: at the grid it asked for, not the desk's.
        phone_says(&link, 1, &Message::Resize { cols: 46, rows: 17 });
        b.tick().await;
        clock.advance(100);
        b.tick().await;
        assert_eq!(phone_view(&link, 1).lines.first().map(String::as_str), Some("a desk frame"));
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

    /// **An echo does not open a coalescing window.** The echo goes at once, which is right, but
    /// marking that push as a burst held the answer it produced for a whole `BURST_GAP`: every
    /// command paid up to a second of nothing after its own echo had already gone out.
    #[tokio::test]
    async fn an_answer_is_not_held_behind_its_own_echo() {
        let (mut b, host, link, clock) = attached_paced().await;
        let base = sent_records(&link);

        clock.advance(50);
        phone_says(&link, 1, &Message::Input(b"ls".to_vec()));
        paced(&host, &["$ ls", ""]);
        b.tick().await;
        let echoed = sent_records(&link) - base;
        assert!(echoed >= 1, "the echo is sent");

        // The answer appears 100 ms later, well inside the window the echo used to open.
        clock.advance(100);
        paced(&host, &["$ ls", "a.txt", "b.txt", ""]);
        b.tick().await;
        assert!(
            sent_records(&link) - base > echoed,
            "the answer is not held for a window its own echo opened"
        );
    }

    /// **A refused reply re-offers only what did not go out.** A record can carry several messages,
    /// so the prefix already on the link must not be offered again: on a link priced per record that
    /// duplicate is a whole record of air.
    #[tokio::test]
    async fn a_refusal_keeps_only_the_messages_it_did_not_send() {
        let (mut b, _host, link) = setup();
        let marks: Vec<(String, u8)> = (0..20).map(|i| (format!("session-{i:02}"), 0)).collect();
        let messages = marks_pages(marks);
        assert_eq!(messages.len(), 2, "the table needs two pages");
        let base = sent_records(&link);
        link.0.lock().unwrap().refuse_after = Some(base + 1);
        let outcome = b.reply_upto(1, &messages);
        assert_eq!(
            outcome,
            Err(1),
            "the first message went, the second did not"
        );
        assert_eq!(sent_records(&link) - base, 1);
    }

    /// **An echo follows the link's record pace, not the tick.**
    ///
    /// A keystroke's echo cannot reach the phone before the record carrying that keystroke has left,
    /// so pushing one echo per keystroke spends records on frames that lose the race against the
    /// next keystroke. Measured on the airtime harness at 62.5 kHz: a 40-character line cost **38
    /// records down** at one echo per key, and **6** at one per record. The frame that does go
    /// carries every character typed so far, so nothing about the picture changes.
    #[tokio::test]
    async fn an_echo_follows_the_links_record_pace() {
        let (mut b, host, link, clock) = attached_paced().await;
        let base = sent_records(&link);

        // **The first echo of a burst is immediate** — that is the keystroke the user is watching.
        clock.advance(2_000);
        phone_says(&link, 1, &Message::Input(b"x".to_vec()));
        paced(&host, &["$ x", ""]);
        b.tick().await;
        assert_eq!(
            sent_records(&link) - base,
            1,
            "the first echo of a burst goes now, not at a window boundary"
        );

        // **The next one waits for the record it would race.** A keystroke's echo cannot reach the
        // phone before the record carrying the keystroke has left, so one echo per record is the
        // most the link can deliver; the frame carries the whole line typed so far.
        clock.advance(50);
        phone_says(&link, 1, &Message::Input(b"y".to_vec()));
        paced(&host, &["$ xy", ""]);
        b.tick().await;
        assert_eq!(
            sent_records(&link) - base,
            1,
            "a second echo inside one record's time waits for it"
        );
        clock.advance(200);
        b.tick().await;
        assert_eq!(
            sent_records(&link) - base,
            2,
            "and goes when the record's time is up"
        );
    }

    /// **A held shell is not drawn, and its release is the difference.**
    ///
    /// Batch mode freezes the picture until the operator presses sync, and the host had no way to
    /// know: a shell printing a build log cost a record a second for screens the phone kept and
    /// dropped. The release must not repaint — the differ still records what the phone holds.
    #[tokio::test]
    async fn a_held_shell_is_not_drawn_until_it_is_released() {
        let (mut b, host, link, clock) = attached_paced().await;
        phone_says(&link, 1, &Message::Hold { on: true });
        b.tick().await;
        let base = sent_records(&link);
        // The shell prints, over two coalescing windows.
        // The same grid as the attach, so the release is a row change and not a new geometry.
        clock.advance(2_000);
        paced(&host, &["$ ls", "one"]);
        b.tick().await;
        clock.advance(2_000);
        paced(&host, &["$ ls", "two"]);
        b.tick().await;
        assert_eq!(sent_records(&link), base, "nothing is built for a frozen view");

        phone_says(&link, 1, &Message::Hold { on: false });
        b.tick().await;
        let after = link.0.lock().unwrap().sent[base..].to_vec();
        assert!(!after.is_empty(), "the release draws what changed while held");
        let mut d = Decoder::new();
        let msgs: Vec<Message> = after
            .iter()
            .flat_map(|(_, bytes)| d.push(bytes).unwrap())
            .filter_map(|m| m.ok())
            .collect();
        assert!(
            !msgs.iter().any(|m| matches!(m, Message::Profile { .. })),
            "the release is a difference, not a repaint: {msgs:?}"
        );
    }

    /// **A hold nobody releases expires.** A client that froze the view and then stopped sending
    /// must not leave a shell unlit for the next viewer, with nothing recording why.
    #[tokio::test]
    async fn a_hold_that_outlives_the_phone_starts_drawing_again() {
        let (mut b, host, link, clock) = attached_paced().await;
        phone_says(&link, 1, &Message::Hold { on: true });
        b.tick().await;
        let base = sent_records(&link);
        clock.advance(2_000);
        paced(&host, &["$ ls", "one"]);
        b.tick().await;
        assert_eq!(sent_records(&link), base);

        clock.advance(601_000);
        b.tick().await;
        assert!(
            sent_records(&link) > base,
            "the hold expires and the present screen goes out"
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
        {
            let mut h = host.0.lock().unwrap();
            h.holders.insert("abcd-1".into(), "desktop".into());
            h.engaged.insert("abcd-1".into());
        }
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

    /// **A shell somebody is typing in at the desk is refused, not stolen.** The phone is told
    /// `Closed { HELD }`, so it can offer `take it back`; the desk window keeps the shell.
    #[tokio::test]
    async fn an_attach_on_a_shell_the_desk_is_using_asks_first() {
        let (mut b, host, link) = setup();
        {
            let mut h = host.0.lock().unwrap();
            h.holders.insert("abcd-1".into(), "desktop".into());
            h.engaged.insert("abcd-1".into());
        }
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
            host.0.lock().unwrap().holders.get("abcd-1").map(String::as_str) == Some("desktop"),
            "an in-use holder keeps the shell: {:?}",
            host.0.lock().unwrap().holders
        );
    }

    /// **An idle desk window does not hold a shell.** Nine windows left open on this machine made
    /// every phone attach a `take it back` dialog for a shell nobody had typed into in hours. The
    /// claim is displaced instead, which is how a takeover already works: the window is told and
    /// stops, and the operator can press `r` there to take it back.
    #[tokio::test]
    async fn an_attach_on_a_shell_the_desk_left_open_is_taken_silently() {
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
            !sent(&link, 1)
                .iter()
                .any(|m| matches!(m, Message::Closed { reason } if *reason == closed::HELD)),
            "an idle window is not a refusal"
        );
        assert_eq!(
            host.0.lock().unwrap().holders.get("abcd-1").map(String::as_str),
            Some("mesh"),
            "the phone holds it now"
        );
    }

    /// **The write path obeys the same rule as the attach path.** A desk window that takes the
    /// shell displaces the phone (ADR-0010), but `Input` and `Key` used to write straight
    /// through: two devices typed into one pty and neither was told. The phone is now refused in
    /// the vocabulary it already understands, and the claim is handed back so the desk keeps it.
    #[tokio::test]
    async fn a_displaced_phone_stops_writing_and_is_told() {
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
            [("claim", "abcd-1".to_string())]
        );
        // The desk takes the shell back: the daemon's claim now names the desktop.
        host.0
            .lock()
            .unwrap()
            .holders
            .insert("abcd-1".into(), "desktop".into());
        phone_says(&link, 3, &Message::Input(b"rm -rf /\n".to_vec()));
        phone_says(&link, 3, &Message::Key(vec!["Enter".into()]));
        b.tick().await;
        assert!(
            host.0.lock().unwrap().typed.is_empty(),
            "a keystroke for a shell this phone no longer holds must not reach it: {:?}",
            host.0.lock().unwrap().typed
        );
        assert!(host.0.lock().unwrap().keyed.is_empty());
        assert!(
            sent(&link, 3)
                .iter()
                .any(|m| matches!(m, Message::Closed { reason } if *reason == closed::HELD)),
            "the phone must be told it lost the shell, not left typing into a screen"
        );
        assert_eq!(b.stats.held_writes, 1);
        assert!(
            host.0
                .lock()
                .unwrap()
                .claimed
                .contains(&("release", "abcd-1".to_string())),
            "the claim is given back so the desk window keeps what it took"
        );
    }

    /// **A held shell must not answer a question about no shell in particular.** With two
    /// shells running and the first held by the desk, an `Attach` with an empty selector used to
    /// consult `matches.first()` before the match, so the phone was told `HELD` about a shell it
    /// never named instead of being told the choice was ambiguous.
    #[tokio::test]
    async fn an_unnamed_attach_with_two_shells_is_ambiguous_not_held() {
        let (mut b, host, link) = setup();
        {
            let mut h = host.0.lock().unwrap();
            h.running.push("abcd-2".into());
            h.screens.insert("abcd-2".into(), screen(&["$", ""]));
            h.holders.insert("abcd-1".into(), "desktop".into());
        }
        phone_says(
            &link,
            2,
            &Message::Attach {
                version: VERSION,
                selector: String::new(),
            },
        );
        b.tick().await;
        let sent = sent(&link, 2);
        assert!(
            sent.iter()
                .any(|m| matches!(m, Message::Closed { reason } if *reason == closed::AMBIGUOUS)),
            "the phone must be asked which shell, not told one is held: {sent:?}"
        );
        assert!(
            !sent
                .iter()
                .any(|m| matches!(m, Message::Closed { reason } if *reason == closed::HELD)),
            "no shell was named, so no shell can be held against this attach"
        );
    }

    /// And `Takeover` is that consent: it claims, and the screen follows.
    #[tokio::test]
    async fn a_takeover_takes_the_shell_the_desk_held() {
        let (mut b, host, link) = setup();
        {
            let mut h = host.0.lock().unwrap();
            h.holders.insert("abcd-1".into(), "desktop".into());
            // Somebody is typing at the desk, so the attach is refused and the takeover is the
            // only thing that can claim it.
            h.engaged.insert("abcd-1".into());
        }
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
        // Two asks, one grant: the attach was refused because the desk is in use, and the
        // takeover is the ask that claims it. Recording both is what shows the refusal happened
        // at the claim table rather than being decided here.
        assert_eq!(
            *host.0.lock().unwrap().claimed,
            [("claim", "abcd-1".to_string()), ("claim", "abcd-1".to_string())],
            "the attach asks and is refused, the takeover asks and succeeds"
        );
        assert_eq!(
            host.0.lock().unwrap().holders.get("abcd-1").map(String::as_str),
            Some("mesh"),
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
