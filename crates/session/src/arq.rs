//! Ordering, acknowledgement and bounded retransmission, above the AEAD record.
//!
//! A pure state machine: no sockets, no clock, no randomness. Time is an argument (`now_ms`,
//! any monotonic millisecond count), and everything it wants to send is *returned* by
//! [`Arq::poll`], never sent. That is what makes "no silent retry storm" provable.
//!
//! # Inner layout (inside the AEAD plaintext)
//!
//! ```text
//! flags (1)  shell (1)  | seq u16 LE | ack u16 LE | mark u8 | payload (0..=max_payload)
//!  ADR inner header     |   5-byte ARQ header, taken out of the application bytes
//! ```
//!
//! # Why `mark` exists
//!
//! MeshCore suppresses duplicates by **packet hash**, first copy wins, at every receiver and
//! repeater (`Mesh::onRecvPacket` -> `wasSeen`/`markSeen`). A record retransmitted with the same
//! bytes is therefore dropped on arrival: the sender waits for an ack the peer never had a chance
//! to send, retries until `max_attempts`, and the session dies. That is exactly "host stopped
//! answering" with both radios healthy.
//!
//! `mark` makes every transmission a distinct packet. It counts up for each record emitted, so a
//! retransmission, a repeated ack and two heartbeats carrying the same ack are all new to the
//! mesh. The receiver ignores it - duplicates are recognised by `seq` - so it costs one byte and
//! buys the whole retry mechanism.
//!
//! `flags` bits 0-1 are the [`Kind`]; every other bit is reserved and must be zero (a record
//! with one set is dropped, so a future flag is not half-understood by an old peer).
//!
//! # Rules
//!
//! - One ordered stream per session. `seq` is a `u16` compared with wrap-around arithmetic;
//!   the window is at most [`MAX_WINDOW`] (the L2 `CREDIT_WINDOW`), far below half the space.
//! - `ack` is cumulative: "I have everything before this". Every outgoing record carries it.
//! - **Budget.** [`Arq::poll`] takes `records_left` and returns at most that many **new data**
//!   records. Retransmissions, pure acks and the close are not charged against it: they are
//!   traffic already paid for or the exchange that keeps the link alive, and gating them turned a
//!   spent allowance into a dead session. With the budget at zero new data waits and
//!   [`Stats::budget_stalls`] rises.
//! - **Retransmission** is one record at a time (the oldest unacknowledged), on a timeout that
//!   doubles with each attempt up to a cap. After `max_attempts` the session is
//!   [`Failure::RetriesExhausted`]; it does not keep trying.
//! - **Duplicates** are dropped, but they do mark an ack as owed (the sender's retransmission
//!   usually means our ack was lost). That is one pending flag, not one reply per duplicate.

use std::collections::VecDeque;

use ldmux_mesh::constants::CREDIT_WINDOW;

/// ARQ header inside the application bytes.
pub const HEADER: usize = 5;
/// The most records ever in flight: the L2 credit window.
pub const MAX_WINDOW: u8 = CREDIT_WINDOW;

const FLAG_KIND_MASK: u8 = 0b0000_0011;

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum Kind {
    Data,
    /// A pure acknowledgement: no payload, not itself acknowledged.
    Ack,
    /// The sender is ending the session. Not acknowledged.
    Close,
}

impl Kind {
    #[must_use]
    pub fn flags(self) -> u8 {
        match self {
            Self::Data => 0,
            Self::Ack => 1,
            Self::Close => 2,
        }
    }

    #[must_use]
    pub fn from_flags(flags: u8) -> Option<Self> {
        if flags & !FLAG_KIND_MASK != 0 {
            return None;
        }
        match flags & FLAG_KIND_MASK {
            0 => Some(Self::Data),
            1 => Some(Self::Ack),
            2 => Some(Self::Close),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Config {
    /// Records allowed in flight. Clamped to [`MAX_WINDOW`] and to at least 1.
    pub window: u8,
    /// First retransmission timeout. A mesh reply taking tens of seconds is normal.
    pub rto_initial_ms: u64,
    pub rto_max_ms: u64,
    /// Total transmissions of one record (first send included) before the session fails.
    pub max_attempts: u8,
    /// How long an owed acknowledgement may wait for outgoing data to ride on.
    pub ack_delay_ms: u64,
    /// One record's airtime, when the caller knows it; **0 means unknown**.
    ///
    /// Used only as the floor for a fast retransmission: a record that has demonstrably not arrived
    /// is re-sent early, but never sooner than it could still be in the air on its first
    /// transmission, because that would put two copies on the air for nothing.
    pub airtime_ms: u64,
    /// How long an idle link may stay silent before it says something; **0 means never**.
    ///
    /// Silence is how both ends decide the other is gone, but only while something is
    /// outstanding: with an empty queue and nothing in flight, nobody is waiting on an answer, and
    /// the ARQ's own ladder detects a dead peer as soon as there is anything to send. mosh
    /// heartbeats every 3 s because its link is the internet, where a middlebox that forgot the
    /// flow is the usual failure; on a LoRa link the usual failure is the record budget, and a
    /// heartbeat is a record. 3 s here was 1200 records an hour spent to say nothing.
    pub heartbeat_ms: u64,
    /// Chunks waiting to be sent. A full queue refuses, it never grows.
    pub queue_max: usize,
    /// Largest payload in one record: `application_bytes - HEADER`.
    pub max_payload: usize,
}

impl Config {
    /// Defaults are judgement calls (ADR-0020 says as much of its own); revisit with
    /// measurements. `max_payload` must come from the record limit the peer advertised.
    #[must_use]
    pub fn for_payload(max_payload: usize) -> Self {
        // Tuned for **interactive typing on a link we own**, measured 2026-10-30. The old
        // numbers (3 s ack delay, 20 s first retry, 300 s cap, window 4) were chosen when a
        // reply taking tens of seconds was normal; on a private 500 kHz channel that is a
        // 70 ms frame, and they were the whole of the delay a keystroke saw. Air is not the
        // constraint here: one echo is two records, ≈140 ms.
        //
        //  - `ack_delay_ms` still lets an owed ack ride on outgoing data first; it only bounds
        //    how long a *pure* ack waits, and at 150 ms that frame is not worth avoiding.
        //  - `rto_initial_ms` is ~10x the measured round trip at 62.5 kHz, so a retry fires long
        //    before a user would notice, and a duplicate is dropped and only marks an ack owed.
        //  - `window` 8 is the L2 credit, so a repaint pipelines instead of stalling at 4.
        Self {
            window: 8,
            rto_initial_ms: 2_000,
            rto_max_ms: 30_000,
            max_attempts: 5,
            ack_delay_ms: 150,
            airtime_ms: 0,
            heartbeat_ms: 0,
            queue_max: 64,
            max_payload,
        }
    }

    /// Retransmission timing for a radio whose records take `airtime_ms` on the air.
    ///
    /// A record is on air for that long, the ack for as long again, and each repeater adds a
    /// random forward delay of up to two airtimes, so a first retry at four airtimes is the
    /// earliest that cannot be a duplicate of a record still in flight; two hops plus the ack is
    /// ≈6. The interactive defaults (2 s) are kept when that is larger, so a fast private
    /// channel still retries quickly, and the cap grows with the airtime so a slow preset does not
    /// exhaust its attempts inside one round trip.
    #[must_use]
    pub fn with_airtime(self, airtime_ms: Option<f64>) -> Self {
        self.with_path(airtime_ms, 1)
    }

    /// Retransmission timing for a route of `hops` repeaters, at `airtime_ms` per record.
    ///
    /// **MeshCore's own direct-send budget is six airtimes *per hop*** (`DIRECT_SEND_PERHOP_FACTOR`,
    /// `Dispatcher.cpp`). A flat six airtimes is right for a direct neighbour and too short for a
    /// route: at 0.6 s a record and ten hops, one copy is still travelling fifteen seconds one way,
    /// so the timer fires twice over and the retransmissions cost the air the timer exists to save.
    /// The measured round trip (`srtt`) corrects this after the first exchange, which is exactly why
    /// the *first* one has to be right — it is the one that is wrong today.
    #[must_use]
    pub fn with_path(mut self, airtime_ms: Option<f64>, hops: u8) -> Self {
        let Some(airtime) = airtime_ms.filter(|a| *a > 0.0) else {
            return self;
        };
        // **The firmware's own ceiling, not a guess.** A path is at most 64 bytes, so at one byte a
        // hop there can be 63 repeaters; a number above that is a misread, and letting it through
        // would set a retransmission timer of hours.
        let hops = f64::from(hops.clamp(1, 63));
        // **Six airtimes: a record, its ack, and two forward delays per hop.** The burst is no longer
        // a reason to stretch this, because a slot's timer starts at its own air slot (see `poll`):
        // the eighth record of a window is timed from two seconds later, not from the hand-over.
        // Stretching it to ten airtimes to cover the burst doubled the time a genuinely lost record
        // took to come back — measured 2026-11-09, a first paint waited eleven seconds for one lost
        // record on a noisy link — and a loss is the common case this timer exists for.
        let scaled = (airtime * 6.0 * hops).round() as u64;
        // **Assigned, not raised, so this is a function of the route.** `max` made it one-way: a
        // session that had once seen a 28-hop chain kept a 101-second timer after the phone came
        // home, and every retry on a neighbour's link then waited a hundred seconds.
        self.rto_initial_ms = scaled.clamp(2_000, 600_000);
        // **A pure ack must not cost its own record when a little patience avoids it.** An owed ack
        // used to wait a fixed 150 ms for outgoing data and then go as a record of its own, which at
        // 0.6 s per record is 0.6 s of air for a 5-byte body — one record in every exchange where the
        // host has nothing to send. Waiting a little still fires it long before anything is
        // retransmitted, so the window never stalls on it, and in the common case the next frame's
        // data carries the ack instead.
        //
        // **Two airtimes, not a third of the timer.** `rto_initial` is six airtimes *per hop*, so a
        // third of it was 24 s on a twenty-hop route: a host with nothing to say held every
        // acknowledgement for twenty-four seconds and the sender's window advanced one record per
        // window of it. Patience that scales with the route is not patience, it is latency — and the
        // case the delay exists for, an ack riding the next data record, is decided by whether data
        // is going out in the next couple of records, not by how far the route is.
        // **The cap has to be above the route's own round trip, or the timer is the bug.** The
        // doubling ladder is useless if its ceiling is below the first rung: at the public tuple
        // `40 × airtime` is 24 s, so `max(30 s, 24 s)` left the cap at **30 s for every route of
        // nine hops or more** and every retransmission was scheduled before its first copy could
        // have crossed. Twice the route's own budget is the floor the cap must clear, and the
        // ladder still has four rungs above it before the firmware's ten-minute bound.
        let cap = (airtime * 40.0).round() as u64;
        self.rto_max_ms = cap
            .max(30_000)
            .max(self.rto_initial_ms.saturating_mul(2))
            .min(600_000);
        // The airtime this call was given, not the field: that is assigned below.
        let ack = (airtime * 2.0).round().max(1.0) as u64;
        self.ack_delay_ms = ack.max(150).min(self.rto_max_ms / 2);
        self.airtime_ms = airtime.round() as u64;
        self
    }
}

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum Failure {
    /// Every attempt went out and none was answered.
    RetriesExhausted,
    /// The radio kept refusing the record without ever transmitting it: a full transmit table that
    /// never drained, a record the firmware will not parse, or a radio that is not really there.
    RadioRefused,
    /// The queue had no room for the tail of a record that had to be split, so bytes would have
    /// been lost. A caller that sees this has outrun the link, not lost the link.
    QueueFull,
}

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum QueueError {
    /// The queue is at `queue_max`: the caller must slow down, not the layer grow.
    Full,
    /// Closed or failed: nothing more is accepted.
    NotOpen,
}

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum DecodeError {
    TooShort,
    UnknownFlags,
}

/// A record the machine wants sent.
#[derive(Debug, PartialEq, Eq, Clone)]
pub struct Outgoing {
    pub kind: Kind,
    pub seq: u16,
    pub ack: u16,
    pub shell: u8,
    /// A per-record mark, so no two transmissions are byte-identical (see the module docs).
    pub mark: u8,
    pub payload: Vec<u8>,
    pub retransmit: bool,
}

impl Outgoing {
    /// The application bytes to put in a transport record (after the 2 inner bytes).
    #[must_use]
    pub fn app_bytes(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(HEADER + self.payload.len());
        out.extend_from_slice(&self.seq.to_le_bytes());
        out.extend_from_slice(&self.ack.to_le_bytes());
        out.push(self.mark);
        out.extend_from_slice(&self.payload);
        out
    }
}

/// A record the peer sent, decoded.
#[derive(Debug, PartialEq, Eq, Clone)]
pub struct Incoming {
    pub kind: Kind,
    pub seq: u16,
    pub ack: u16,
    pub shell: u8,
    /// The sender's per-record mark. Unused here: duplicates are recognised by `seq`.
    pub mark: u8,
    pub payload: Vec<u8>,
}

impl Incoming {
    pub fn decode(flags: u8, shell: u8, app: &[u8]) -> Result<Self, DecodeError> {
        let kind = Kind::from_flags(flags).ok_or(DecodeError::UnknownFlags)?;
        if app.len() < HEADER {
            return Err(DecodeError::TooShort);
        }
        Ok(Self {
            kind,
            seq: u16::from_le_bytes([app[0], app[1]]),
            ack: u16::from_le_bytes([app[2], app[3]]),
            shell,
            mark: app[4],
            payload: app[HEADER..].to_vec(),
        })
    }
}

/// In-order data handed up.
#[derive(Debug, PartialEq, Eq, Clone)]
pub struct Delivered {
    pub shell: u8,
    pub payload: Vec<u8>,
}

/// What the caller allows this poll to emit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    /// The radio's `recordsLeftThisHour`, as the carrier reported it. Never exceeded.
    pub records_left: u8,
    /// Free L2 slots: `CREDIT_WINDOW` minus records still awaiting `SEND_RESULT`.
    pub l2_slots: u8,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Stats {
    pub sent: u32,
    pub retransmits: u32,
    /// Retransmissions pulled early by the peer's own non-advancing ack, rather than by a timer.
    pub fast_retransmits: u32,
    pub delivered_bytes: u64,
    pub duplicates: u32,
    pub out_of_window: u32,
    pub bad_acks: u32,
    pub budget_stalls: u32,
}

/// How long to wait before retrying a record the radio refused without transmitting.
///
/// A refusal is almost always the firmware's transmit table being full, which happens when records
/// are handed over faster than the radio can put them on the air. meshcore-sar measured that wall at
/// ≈350 ms per fragment and paces for it, and that is the number here: the retry *is* the pace,
/// learned from the radio's own answer instead of guessed from a preset. It is deliberately far
/// shorter than the interval for a packet that really went out and went unanswered — a refusal says
/// "not yet", not "lost".
pub const REFUSED_RETRY_MS: u64 = 350;

impl Arq {
    /// How long to wait before re-offering a record the radio refused.
    ///
    /// [`REFUSED_RETRY_MS`] is the firmware's measured drain at a bench preset; on a slower one a
    /// record occupies the transmit table for longer than that, so asking again at 350 ms is asking
    /// again before a slot can possibly have freed — a serial round trip per attempt for an answer
    /// the radio has already given. The airtime is the honest floor and the constant is the floor
    /// for a fast link.
    fn refused_retry_ms(&self) -> u64 {
        REFUSED_RETRY_MS.max(self.cfg.airtime_ms)
    }
}

/// How long one record may keep being refused before the session says the radio is not taking it.
///
/// A refusal is "not yet", so it must not consume attempts — but a radio whose transmit table is
/// permanently full (wrong preset, wrong baud, wedged firmware, or a record the firmware will never
/// parse) would otherwise be retried every 350 ms for as long as the process runs, with a live
/// session and no error. Thirty seconds is long enough for a real burst to drain and short enough
/// that a wedged radio becomes a named failure instead of silence.
pub const REFUSAL_DEADLINE_MS: u64 = 30_000;

struct Slot {
    seq: u16,
    shell: u8,
    /// The plaintext application bytes. The session seals them again on every transmission, so a
    /// retransmission leaves as a different packet — MeshCore radios drop byte-identical repeats.
    payload: Vec<u8>,
    /// Transmissions so far (>= 1 once sent).
    attempts: u8,
    sent_at: u64,
    /// The radio refused the last transmission without putting it on the air. The retry then
    /// waits [`REFUSED_RETRY_MS`], the firmware's transmit-table pace, instead of a retry interval.
    refused: bool,
    /// When this slot's refusal run began, so a radio that never takes the record fails the
    /// session after [`REFUSAL_DEADLINE_MS`] instead of retrying it forever.
    refused_since: Option<u64>,
    /// The peer's own acknowledgement proved this record has not arrived, so it may be re-sent
    /// ahead of its timer.
    fast_due: bool,
}

pub struct Arq {
    cfg: Config,
    queue: VecDeque<(u8, Vec<u8>)>,
    inflight: VecDeque<Slot>,
    next_seq: u16,
    next_expected: u16,
    /// Out-of-order data held until the gap fills, keyed by distance from `next_expected`.
    held: Vec<Option<(u8, Vec<u8>)>>,
    ack_owed_since: Option<u64>,
    /// The owed ack comes from a **gap**, so it goes out at once rather than riding outgoing data.
    ack_urgent: bool,
    /// When something was last sent, for the heartbeat. `None` before the first record.
    last_sent_at: Option<u64>,
    /// Smoothed round-trip estimate, in milliseconds, from first-transmission acks only.
    /// `None` until an ack has taught one.
    srtt_ms: Option<u64>,
    /// The next per-record mark. Bumped for every record emitted, so no two are identical.
    next_mark: u8,
    close_wanted: bool,
    close_sent: bool,
    peer_closed: bool,
    failed: Option<Failure>,
    stats: Stats,
}

impl Arq {
    /// The live configuration, for a caller that needs to re-scale it.
    #[must_use]
    pub fn config(&self) -> &Config {
        &self.cfg
    }

    /// Replace the configuration. Window and attempt bounds are re-applied, as in [`Self::new`].
    pub fn set_config(&mut self, mut cfg: Config) {
        cfg.window = cfg.window.clamp(1, MAX_WINDOW);
        cfg.max_attempts = cfg.max_attempts.max(1);
        self.cfg = cfg;
    }

    pub fn new(mut cfg: Config) -> Self {
        cfg.window = cfg.window.clamp(1, MAX_WINDOW);
        cfg.max_attempts = cfg.max_attempts.max(1);
        Self {
            cfg,
            queue: VecDeque::new(),
            inflight: VecDeque::new(),
            next_seq: 0,
            next_expected: 0,
            held: vec![None; usize::from(MAX_WINDOW)],
            ack_owed_since: None,
            ack_urgent: false,
            last_sent_at: None,
            srtt_ms: None,
            next_mark: 0,
            close_wanted: false,
            close_sent: false,
            peer_closed: false,
            failed: None,
            stats: Stats::default(),
        }
    }

    #[must_use]
    pub fn failure(&self) -> Option<Failure> {
        self.failed
    }

    #[must_use]
    pub fn peer_closed(&self) -> bool {
        self.peer_closed
    }

    #[must_use]
    pub fn stats(&self) -> Stats {
        self.stats
    }

    /// The smoothed round trip, once an ack has taught one. For `mesh timing` lines and tests,
    /// never for control flow.
    #[must_use]
    pub fn smoothed_rtt_ms(&self) -> Option<u64> {
        self.srtt_ms
    }

    /// Chunks queued plus records in flight.
    #[must_use]
    pub fn pending(&self) -> usize {
        self.queue.len() + self.inflight.len()
    }

    /// The application bytes this queue is still holding, queued and in flight.
    ///
    /// **The only honest number for "how much is outstanding".** A caller that counted its own
    /// `send` calls and only reset when the queue was empty never subtracted what had been
    /// delivered: on a link that always has something in flight the count climbed until it hit a
    /// ceiling, and from then on every send was refused with "the outbound queue is full" while the
    /// session was perfectly healthy — the screen simply froze with no error the user could act on
    /// (audited 2026-11-05). The bytes live here, so the arithmetic belongs here.
    #[must_use]
    pub fn queued_bytes(&self) -> usize {
        self.queue.iter().map(|(_, b)| b.len()).sum::<usize>()
            + self.inflight.iter().map(|s| s.payload.len()).sum::<usize>()
    }

    /// Queue bytes for `shell`. Split into records of at most `max_payload` bytes; all or
    /// nothing, so a refused call leaves the queue as it was.
    pub fn send(&mut self, shell: u8, bytes: &[u8]) -> Result<(), QueueError> {
        if self.failed.is_some() || self.peer_closed || self.close_wanted {
            return Err(QueueError::NotOpen);
        }
        let chunk = self.cfg.max_payload.max(1);
        let chunks = bytes
            .len()
            .div_ceil(chunk)
            .max(usize::from(bytes.is_empty()));
        if self.queue.len() + chunks > self.cfg.queue_max {
            return Err(QueueError::Full);
        }
        if bytes.is_empty() {
            self.queue.push_back((shell, Vec::new()));
        }
        for c in bytes.chunks(chunk) {
            self.queue.push_back((shell, c.to_vec()));
        }
        Ok(())
    }

    /// **Re-split what is still queued, after the record size shrank.**
    ///
    /// A repeater's hops come out of the same radio frame as the record, so a route that grows
    /// shrinks each record by one byte a hop. Chunks are split when they are queued, so a queue
    /// split at 172 bytes keeps chunks that no longer fit — the carrier refuses them, gives the
    /// attempt back, and they are refused again: a loop that used to end in a reconnect. Merging
    /// adjacent chunks of the *same shell* is lossless, so the tail can be re-split at the new size
    /// with nothing lost. Records already in flight cannot be re-split; the caller handles those.
    ///
    /// Returns whether the queue changed.
    pub fn resize_pending(&mut self, chunk: usize) -> bool {
        let chunk = chunk.max(1);
        if self.queue.is_empty() {
            return false;
        }
        // Kept so an overflow can put the queue back exactly as it was: re-splitting into a queue
        // that no longer fits its bound must not silently drop bytes.
        let original: Vec<(u8, Vec<u8>)> = self.queue.iter().cloned().collect();
        let mut merged: Vec<(u8, Vec<u8>)> = Vec::with_capacity(self.queue.len());
        for (shell, bytes) in self.queue.drain(..) {
            match merged.last_mut() {
                Some((s, acc)) if *s == shell => acc.extend_from_slice(&bytes),
                _ => merged.push((shell, bytes)),
            }
        }
        let count: usize = merged
            .iter()
            .map(|(_, bytes)| bytes.len().div_ceil(chunk).max(1))
            .sum();
        // A queue that would no longer fit its own bound is put back exactly as it was: the carrier
        // refusing a record is a slower failure than a queue that silently dropped bytes.
        if count > self.cfg.queue_max {
            self.queue.extend(original);
            return false;
        }
        for (shell, bytes) in merged {
            if bytes.is_empty() {
                self.queue.push_back((shell, Vec::new()));
            }
            for c in bytes.chunks(chunk) {
                self.queue.push_back((shell, c.to_vec()));
            }
        }
        true
    }

    /// End the session. The close record is emitted once, after everything queued has been
    /// sent and acknowledged, budget permitting.
    pub fn close(&mut self) {
        self.close_wanted = true;
    }

    /// Feed one decoded record. Returns data now deliverable, in order.
    pub fn on_incoming(&mut self, now_ms: u64, inc: &Incoming) -> Vec<Delivered> {
        let mut out = Vec::new();
        if self.failed.is_some() {
            return out;
        }
        if inc.kind == Kind::Close {
            self.peer_closed = true;
            return out;
        }
        self.apply_ack(inc.ack, now_ms);
        if inc.kind != Kind::Data {
            return out;
        }
        let ahead = inc.seq.wrapping_sub(self.next_expected);
        if ahead == 0 {
            self.deliver(inc.shell, &inc.payload, &mut out);
            // Drain whatever the gap was hiding.
            while let Some(slot) = self.held.first_mut().and_then(Option::take) {
                self.deliver(slot.0, &slot.1, &mut out);
            }
        } else if ahead >= 0x8000 {
            self.stats.duplicates += 1;
        } else if usize::from(ahead) < self.held.len() {
            let slot = &mut self.held[usize::from(ahead)];
            if slot.is_some() {
                self.stats.duplicates += 1;
            } else {
                *slot = Some((inc.shell, inc.payload.clone()));
            }
        } else {
            self.stats.out_of_window += 1;
        }
        // **A gap's ack does not wait.** An ack owed by in-order data rides the next outgoing record,
        // which is what `ack_delay` is for. An ack owed because a record was *held* is the one ack
        // that carries information the sender is waiting on, so it goes at once: same record, sent
        // earlier, and it is what lets the sender retransmit the gap in half the time.
        if ahead != 0 && ahead < 0x8000 {
            self.ack_urgent = true;
        }
        // New data, a duplicate or a gap all leave an ack owed; one flag, not one per record.
        self.ack_owed_since.get_or_insert(now_ms);
        out
    }

    fn deliver(&mut self, shell: u8, payload: &[u8], out: &mut Vec<Delivered>) {
        self.next_expected = self.next_expected.wrapping_add(1);
        // `held[i]` is keyed by distance from `next_expected`; advancing it shifts the table.
        self.held.remove(0);
        self.held.push(None);
        self.stats.delivered_bytes += payload.len() as u64;
        out.push(Delivered {
            shell,
            payload: payload.to_vec(),
        });
    }

    /// Apply one cumulative ack and learn from it.
    ///
    /// **A round-trip sample is taken only from a first transmission.** A retransmitted slot
    /// cannot say which of its transmissions was acknowledged (Karn's rule), so it teaches the
    /// estimator nothing and is skipped.
    ///
    /// The estimate matters because a fixed first timeout is wrong in both directions on a mesh:
    /// at 500 kHz SF8 a record is ≈138 ms on air, and the firmware's own direct timeout is
    /// `500 + ((airtime * 6 + 250) * (hops + 1))`, so two hops take 3.7 s and three hops 4.8 s,
    /// while a direct neighbour answers in well under a second.
    fn apply_ack(&mut self, ack: u16, now_ms: u64) {
        let base = self.inflight.front().map_or(self.next_seq, |s| s.seq);
        let acked = ack.wrapping_sub(base);
        if usize::from(acked) > self.inflight.len() || acked >= 0x8000 {
            self.stats.bad_acks += 1;
            return;
        }
        if acked == 0 {
            // **A cumulative ack that does not advance is evidence, and it used to be thrown away.**
            //
            // The peer only owes an ack because a record arrived. If that record had been the front,
            // this ack would have advanced. So the front was absent when the ack was built, and
            // waiting out the retransmission timer afterwards is waiting on a die that already fell:
            // measured at the public preset, that wait was 0.7 s of the 4.3 s a gap cost.
            //
            // The floor is what keeps a merely *late* record from being sent twice: `srtt/2` is the
            // measured one-way time including every repeater's forward delay, so a slow path raises
            // its own floor, and one airtime is the hard minimum before any RTT exists.
            let floor = self.fast_retx_floor();
            if let Some(front) = self
                .inflight
                .front_mut()
                .filter(|s| !s.refused && now_ms.saturating_sub(s.sent_at) >= floor)
            {
                front.fast_due = true;
            }
            return;
        }
        if acked > 0 {
            let sample = self
                .inflight
                .front()
                .filter(|s| s.attempts == 1)
                .map(|s| now_ms.saturating_sub(s.sent_at));
            if let Some(sample) = sample {
                self.srtt_ms = Some(match self.srtt_ms {
                    None => sample,
                    // 1/8 weight, the same smoothing TCP gives its RTT estimate.
                    Some(srtt) => (srtt.saturating_mul(7) + sample) / 8,
                });
            }
        }
        for _ in 0..acked {
            self.inflight.pop_front();
        }
    }

    /// The radio refused a record without transmitting it.
    ///
    /// A refusal is `busy`, `not found` or `too long`: the packet never reached the air, so it is
    /// **not** a transmission attempt. Counting it as one used to fail the session after five
    /// refusals, and a refusal is exactly what a radio that has spent its hourly budget sends —
    /// so a full budget killed the session and forced a handshake it did not need.
    ///
    /// The slot's timer restarts from the refusal rather than firing immediately: the radio has
    /// just said no, and asking again in the same breath is how a retry loop turns into a flood.
    /// The next try is [`REFUSED_RETRY_MS`] away — the firmware's transmit-table pace — not a full
    /// retry interval, because a refusal means "not yet" rather than "lost". The attempt ladder
    /// only advances when a packet really went out and really went unanswered.
    ///
    /// Returns `true` when a slot with that sequence number was in flight.
    pub fn on_refused(&mut self, seq: u16, now_ms: u64) -> bool {
        let Some(i) = self.inflight.iter().position(|s| s.seq == seq) else {
            return false;
        };
        // The interval depends only on the attempt count, so it is computed before the slot is
        // borrowed mutably and the borrow checker is not asked to prove anything about ordering.
        let attempts = self.inflight[i].attempts.saturating_sub(1).max(1);
        let slot = &mut self.inflight[i];
        slot.attempts = attempts;
        slot.sent_at = now_ms;
        slot.refused = true;
        // The run starts at the first refusal and is not extended by later ones: the deadline
        // bounds "this radio is not taking this record", not "the radio is busy right now".
        slot.refused_since.get_or_insert(now_ms);
        true
    }

    /// How long a record must have been outstanding before a non-advancing ack may pull it early.
    ///
    /// `max(one airtime, srtt/2)`. The half-round-trip term is the honest one: it is the measured
    /// time for a record to reach the peer by *this* path, repeaters included, so a long path
    /// raises its own floor instead of being punished for being long. One airtime is the floor
    /// before any sample exists, and also the floor for a link so lossy that only first
    /// transmissions are sampled (Karn's rule), which is exactly a link with many gaps.
    fn fast_retx_floor(&self) -> u64 {
        let rtt = self.srtt_ms.map_or(0, |s| s / 2);
        self.cfg.airtime_ms.max(rtt).max(200)
    }

    /// The retry interval for a slot that has been sent `attempts` times.
    ///
    /// The floor is the larger of the configured first timeout and the measured round trip, so a
    /// path that really takes 3.7 s is not retried at 2 s, which used to put one duplicate
    /// record on the air per expiry. The doubling across attempts is unchanged.
    fn rto(&self, attempts: u8) -> u64 {
        let base = self.cfg.rto_initial_ms.max(self.srtt_ms.unwrap_or(0));
        let shift = u32::from(attempts.saturating_sub(1)).min(20);
        base.saturating_mul(1u64 << shift)
            .min(self.cfg.rto_max_ms)
    }

    /// Put the tail of a record that no longer fits back at the head of the queue.
    ///
    /// **A route that grows mid-flight must not end the session.** Slots keep the payload they were
    /// split at, so a smaller record limit makes one of them unsealable; refusing it used to cost
    /// the whole batch (acks included) and one attempt per poll until the session died. The sender
    /// keeps the front slot's sequence number and a prefix that fits, and the remainder is queued as
    /// the next thing that shell sends — same order, same bytes, one extra record.
    ///
    /// The slot's own attempt count is untouched: it was already charged for this transmission.
    pub fn requeue_remainder(&mut self, shell: u8, remainder: Vec<u8>) {
        if remainder.is_empty() {
            return;
        }
        if self.queue.len() >= self.cfg.queue_max {
            // No room to hold it: the honest failure is a session error, which the caller reports,
            // rather than bytes silently lost.
            self.failed = Some(Failure::QueueFull);
            return;
        }
        self.queue.push_front((shell, remainder));
    }

    /// The earliest time something becomes due, for the caller's timer.
    #[must_use]
    pub fn next_deadline(&self) -> Option<u64> {
        let retx = self.inflight.front().map(|s| {
            if s.refused {
                s.sent_at + self.refused_retry_ms()
            } else {
                s.sent_at + self.rto(s.attempts)
            }
        });
        let ack = self.ack_owed_since.map(|t| t + self.cfg.ack_delay_ms);
        // `heartbeat_ms == 0` means never, which `poll` already honours. Without the guard here
        // the deadline resolved to `last_sent_at`, an instant in the past, so the caller's wait
        // was zero after the first record and the session loop spun at full CPU forever.
        let beat = self
            .last_sent_at
            .filter(|_| self.cfg.heartbeat_ms > 0)
            .map(|t| t.saturating_add(self.cfg.heartbeat_ms));
        let (retx, ack) = match (retx, ack) {
            (Some(a), Some(b)) => (Some(a.min(b)), None),
            (a, b) => (a, b),
        };
        let retx = match (retx, beat) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        };
        match (retx, ack) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        }
    }

    /// Everything the machine wants sent now, bounded by the caller's room.
    ///
    /// At most `l2_slots` records in total, and at most `records_left` of them *new*: a
    /// retransmission is already-paid traffic and is allowed through on top of the budget, which is
    /// why the count can be `min(records_left, l2_slots) + 1`. The slot bound is absolute, because
    /// a record the caller has no room for cannot be sent at all.
    pub fn poll(&mut self, now_ms: u64, limits: Limits) -> Vec<Outgoing> {
        let mut out = Vec::new();
        if self.failed.is_some() {
            return out;
        }
        let mut budget = limits.records_left.min(limits.l2_slots);
        let mut wanted = false;

        // 1. At most one retransmission: the oldest record, when its timer has run out.
        let refused_retry = self.refused_retry_ms();
        let front_due = self.inflight.front().map(|s| {
            if s.refused {
                s.sent_at + refused_retry
            } else if s.fast_due {
                // The peer proved this record is missing, so the timer is not what decides.
                s.sent_at
            } else {
                s.sent_at + self.rto(s.attempts)
            }
        });
        if let Some(due) = front_due
            && now_ms >= due
        {
            // A refusal run that outlives the deadline is not "yet": the radio is not taking this
            // record at all, so the session says so instead of asking every 350 ms forever.
            if self
                .inflight
                .front()
                .is_some_and(|s| s.refused && s.refused_since.is_some_and(|t| now_ms >= t + REFUSAL_DEADLINE_MS))
            {
                self.failed = Some(Failure::RadioRefused);
                return out;
            }
            if self.inflight.front().is_some_and(|s| s.attempts >= self.cfg.max_attempts) {
                self.failed = Some(Failure::RetriesExhausted);
                return out;
            }
            wanted = true;
            // A retransmission is not new traffic: it is the record already paid for. Gating it
            // on the voluntary cap turns a busy minute into a dead session, and the cap exists to
            // bound what the operator chooses to send, not to throw away what was sent.
            if usize::from(limits.l2_slots) > out.len() {
                let ack = self.next_expected;
                let front = self.inflight.front_mut().expect("checked above");
                // **A fast retransmission is a transmission.** It counts an attempt, so five of them
                // still end the session instead of looping for ever, and it clears the flag so the
                // next non-advancing ack has to clear the floor again before pulling it a second time.
                let fast = front.fast_due;
                front.attempts += 1;
                front.sent_at = now_ms;
                front.refused = false;
                front.refused_since = None;
                front.fast_due = false;
                if fast {
                    self.stats.fast_retransmits += 1;
                }
                out.push(Outgoing {
                    kind: Kind::Data,
                    seq: front.seq,
                    ack,
                    shell: front.shell,
                    mark: 0,
                    payload: front.payload.clone(),
                    retransmit: true,
                });
                self.stats.retransmits += 1;
            }
        }

        // 2. New data, while the window, the budget and the L2 slot count all allow.
        //
        // The slot bound matters as much as the budget: the caller has that many slots of room in
        // its credit window, and records beyond it cannot be sent at all. Filling the window past
        // them would mark eight records in flight while only one reached the radio, and every one
        // of the seven would then be retransmitted as a duplicate.
        //
        // `wanted` is decided before the gates, so a stall is counted even when there is no slot to
        // send into: "something is waiting and it did not go" is exactly what the caller reports.
        if !self.queue.is_empty() && self.inflight.len() < usize::from(self.cfg.window) {
            wanted = true;
        }
        while self.inflight.len() < usize::from(self.cfg.window)
            && !self.queue.is_empty()
            && out.len() < usize::from(limits.l2_slots)
        {
            if budget == 0 {
                break;
            }
            budget -= 1;
            let (shell, payload) = self.queue.pop_front().expect("checked non-empty");
            let seq = self.next_seq;
            self.next_seq = self.next_seq.wrapping_add(1);
            // **The timer starts when this record's turn on the air begins, not when the window was
            // handed over.** The caller writes a whole poll's records to the radio one after
            // another, and a radio at 0.6 s a record does not start the fourth until two seconds
            // after the first: stamping them all `now` made a record's timeout expire before it had
            // been transmitted, and the front of every full burst was retransmitted for nothing.
            // The estimate is the slot's position in this poll times the airtime, which is what
            // lets the first timeout stay at six airtimes (fast recovery on a lossy link) instead of
            // being stretched to cover the whole window.
            let slot_air = u64::try_from(out.len()).unwrap_or(0).saturating_mul(self.cfg.airtime_ms);
            self.inflight.push_back(Slot {
                seq,
                shell,
                payload: payload.clone(),
                attempts: 1,
                sent_at: now_ms.saturating_add(slot_air),
                refused: false,
                refused_since: None,
                fast_due: false,
            });
            out.push(Outgoing {
                kind: Kind::Data,
                seq,
                ack: self.next_expected,
                shell,
                mark: 0,
                payload,
                retransmit: false,
            });
        }

        // 3. A pure ack, only if nothing else is carrying it and it has waited long enough.
        if out.is_empty()
            && self
                .ack_owed_since
                .is_some_and(|t| now_ms >= t + if self.ack_urgent { 0 } else { self.cfg.ack_delay_ms })
        {
            wanted = true;
            if limits.l2_slots > 0 {
                out.push(self.control(Kind::Ack));
            }
        }

        // 3b. A heartbeat when the link has gone quiet — only if one is configured. On the mesh
        //     carrier it is not: a record is minutes of budget, and an idle session has nothing to
        //     say that anyone is waiting for. Silence is how each end decides the other
        //     is gone, so an idle session that says nothing is indistinguishable from a dead one;
        //     a pure ack costs ≈22 bytes and carries the receive state.
        if self.cfg.heartbeat_ms > 0
            && out.is_empty()
            && self
                .last_sent_at
                .is_some_and(|t| now_ms >= t.saturating_add(self.cfg.heartbeat_ms))
            && limits.l2_slots > 0
        {
            out.push(self.control(Kind::Ack));
        }

        // 4. The close, last, once.
        // Only once everything sent has been acknowledged: a close that overtook lost data would
        // end the session with bytes the peer never got.
        //
        // **Not budget-gated.** `budget` counts new *data*; a close is control traffic like an ack
        // or a retransmission. Gating it on the budget meant a session that asked to close while
        // the hour's allowance was spent never sent the close at all — and it counted a stall on
        // every poll for the rest of the hour. The slot bound still applies, because a record the
        // caller has no room for cannot be sent.
        if self.close_wanted
            && !self.close_sent
            && self.queue.is_empty()
            && self.inflight.is_empty()
            && limits.l2_slots > 0
        {
            wanted = true;
            out.push(self.control(Kind::Close));
            self.close_sent = true;
        }

        if !out.is_empty() {
            for o in &mut out {
                o.mark = self.next_mark;
                self.next_mark = self.next_mark.wrapping_add(1);
            }
            self.ack_owed_since = None;
            self.ack_urgent = false;
            self.last_sent_at = Some(now_ms);
            self.stats.sent += out.len() as u32;
        }
        if wanted && out.is_empty() {
            self.stats.budget_stalls += 1;
        }
        out
    }

    fn control(&self, kind: Kind) -> Outgoing {
        Outgoing {
            kind,
            seq: self.next_seq,
            ack: self.next_expected,
            shell: 0,
            mark: 0,
            payload: Vec::new(),
            retransmit: false,
        }
    }
}

#[cfg(test)]
mod ack_timing_tests {
    use super::*;

    /// **Patience that scales with the route is latency.** The owed-ack delay is decided by whether
    /// data is going out in the next couple of records, not by how far the route is: a third of the
    /// retransmission timer was 24 s on a twenty-hop path.
    #[test]
    fn an_ack_never_waits_more_than_a_couple_of_records() {
        let near = Config::for_payload(137).with_path(Some(603.0), 1);
        assert_eq!(near.ack_delay_ms, 1_206, "two airtimes at the public tuple");
        let far = Config::for_payload(137).with_path(Some(603.0), 20);
        assert_eq!(far.ack_delay_ms, 1_206, "and the same twenty hops out");
        assert!(far.rto_initial_ms > 60_000, "while the retransmission timer scales");
        // An unknown airtime keeps the interactive default rather than inventing one.
        let unknown = Config::for_payload(137).with_path(None, 20);
        assert_eq!(unknown.ack_delay_ms, 150);
    }
}

#[cfg(test)]
mod resize_tests {
    use super::*;

    /// **A shrink re-splits what is queued, and loses nothing.** A route that grows takes one byte a
    /// hop out of every record, so a queue split at 148 bytes holds chunks the radio then refuses;
    /// merging adjacent chunks of one shell is lossless, so the tail can be re-split at the new size.
    #[test]
    fn a_smaller_record_resplits_the_queue_without_losing_bytes() {
        let mut arq = Arq::new(Config::for_payload(148));
        let body: Vec<u8> = (0..300u32).map(|i| (i % 251) as u8).collect();
        arq.send(3, &body).expect("queues");
        let before = arq.pending();
        assert!(arq.resize_pending(90));
        assert!(arq.pending() > before, "smaller chunks means more of them");
        let mut rejoined = Vec::new();
        for (shell, bytes) in arq.queue.iter() {
            assert_eq!(*shell, 3);
            assert!(bytes.len() <= 90);
            rejoined.extend_from_slice(bytes);
        }
        assert_eq!(rejoined, body, "byte for byte, in order");
        // Two shells are never merged into one chunk.
        let mut arq = Arq::new(Config::for_payload(148));
        arq.send(1, &[7u8; 10]).unwrap();
        arq.send(2, &[8u8; 10]).unwrap();
        assert!(arq.resize_pending(60));
        let shells: Vec<u8> = arq.queue.iter().map(|(s, _)| *s).collect();
        assert_eq!(shells, vec![1, 2]);
    }
}
