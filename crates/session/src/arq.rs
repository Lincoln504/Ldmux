//! Ordering, acknowledgement and bounded retransmission, above the AEAD record.
//!
//! A pure state machine: no sockets, no clock, no randomness. Time is an argument (`now_ms`,
//! any monotonic millisecond count), and everything it wants to send is *returned* by
//! [`Arq::poll`], never sent. That is what makes "no silent retry storm" provable.
//!
//! # Inner layout (inside the AEAD plaintext)
//!
//! ```text
//! flags (1)  shell (1)  | seq u16 LE | ack u16 LE | payload (0..=max_payload)
//!  ADR inner header     |   4-byte ARQ header, taken out of the application bytes
//! ```
//!
//! `flags` bits 0-1 are the [`Kind`]; every other bit is reserved and must be zero (a record
//! with one set is dropped, so a future flag is not half-understood by an old peer).
//!
//! # Rules
//!
//! - One ordered stream per session. `seq` is a `u16` compared with wrap-around arithmetic;
//!   the window is at most [`MAX_WINDOW`] (the L2 `CREDIT_WINDOW`), far below half the space.
//! - `ack` is cumulative: "I have everything before this". Every outgoing record carries it.
//! - **Budget.** [`Arq::poll`] takes `records_left` and returns at most that many records.
//!   Every record the gateway transmits counts: first sends, retransmissions, pure acks and
//!   close. When the budget is zero nothing is emitted and [`Stats::budget_stalls`] rises.
//! - **Retransmission** is one record at a time (the oldest unacknowledged), on a timeout that
//!   doubles with each attempt up to a cap. After `max_attempts` the session is
//!   [`Failure::RetriesExhausted`]; it does not keep trying.
//! - **Duplicates** are dropped, but they do mark an ack as owed (the sender's retransmission
//!   usually means our ack was lost). That is one pending flag, not one reply per duplicate.

use std::collections::VecDeque;

use ldmux_mesh::constants::CREDIT_WINDOW;

/// ARQ header inside the application bytes.
pub const HEADER: usize = 4;
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
    /// Chunks waiting to be sent. A full queue refuses, it never grows.
    pub queue_max: usize,
    /// Largest payload in one record: `application_bytes - HEADER`.
    pub max_payload: usize,
}

impl Config {
    /// Defaults are judgement calls (ADR-0020 says as much of its own); revisit with
    /// measurements. `max_payload` must come from the record limit the gateway advertised.
    #[must_use]
    pub fn for_payload(max_payload: usize) -> Self {
        // Tuned for **interactive typing on a link we own**, measured 2026-10-30. The old
        // numbers (3 s ack delay, 20 s first retry, 300 s cap, window 4) were chosen when a
        // reply taking tens of seconds was normal; on a private 500 kHz channel that is a
        // 70 ms frame, and they were the whole of the delay a keystroke saw. Air is not the
        // constraint here: one echo is two records, about 140 ms.
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
            queue_max: 64,
            max_payload,
        }
    }
}

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum Failure {
    RetriesExhausted,
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
    /// The gateway's `recordsLeftThisHour`. Never exceeded.
    pub records_left: u8,
    /// Free L2 slots: `CREDIT_WINDOW` minus records still awaiting `SEND_RESULT`.
    pub l2_slots: u8,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Stats {
    pub sent: u32,
    pub retransmits: u32,
    pub delivered_bytes: u64,
    pub duplicates: u32,
    pub out_of_window: u32,
    pub bad_acks: u32,
    pub budget_stalls: u32,
}

struct Slot {
    seq: u16,
    shell: u8,
    payload: Vec<u8>,
    /// Transmissions so far (>= 1 once sent).
    attempts: u8,
    sent_at: u64,
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
    close_wanted: bool,
    close_sent: bool,
    peer_closed: bool,
    failed: Option<Failure>,
    stats: Stats,
}

impl Arq {
    #[must_use]
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

    /// Chunks queued plus records in flight.
    #[must_use]
    pub fn pending(&self) -> usize {
        self.queue.len() + self.inflight.len()
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
        self.apply_ack(inc.ack);
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

    fn apply_ack(&mut self, ack: u16) {
        let base = self.inflight.front().map_or(self.next_seq, |s| s.seq);
        let acked = ack.wrapping_sub(base);
        if usize::from(acked) > self.inflight.len() || acked >= 0x8000 {
            self.stats.bad_acks += 1;
            return;
        }
        for _ in 0..acked {
            self.inflight.pop_front();
        }
    }

    fn rto(&self, attempts: u8) -> u64 {
        let shift = u32::from(attempts.saturating_sub(1)).min(20);
        self.cfg
            .rto_initial_ms
            .saturating_mul(1u64 << shift)
            .min(self.cfg.rto_max_ms)
    }

    /// The earliest time something becomes due, for the caller's timer.
    #[must_use]
    pub fn next_deadline(&self) -> Option<u64> {
        let retx = self
            .inflight
            .front()
            .map(|s| s.sent_at + self.rto(s.attempts));
        let ack = self.ack_owed_since.map(|t| t + self.cfg.ack_delay_ms);
        match (retx, ack) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        }
    }

    /// Everything the machine wants sent now, at most `min(records_left, l2_slots)` records.
    pub fn poll(&mut self, now_ms: u64, limits: Limits) -> Vec<Outgoing> {
        let mut out = Vec::new();
        if self.failed.is_some() {
            return out;
        }
        let mut budget = limits.records_left.min(limits.l2_slots);
        let mut wanted = false;

        // 1. At most one retransmission: the oldest record, when its timer has run out.
        if let Some(front) = self.inflight.front()
            && now_ms >= front.sent_at + self.rto(front.attempts)
        {
            if front.attempts >= self.cfg.max_attempts {
                self.failed = Some(Failure::RetriesExhausted);
                return out;
            }
            wanted = true;
            if budget > 0 {
                budget -= 1;
                let ack = self.next_expected;
                let front = self.inflight.front_mut().expect("checked above");
                front.attempts += 1;
                front.sent_at = now_ms;
                out.push(Outgoing {
                    kind: Kind::Data,
                    seq: front.seq,
                    ack,
                    shell: front.shell,
                    payload: front.payload.clone(),
                    retransmit: true,
                });
                self.stats.retransmits += 1;
            }
        }

        // 2. New data, while the window and the budget allow.
        while self.inflight.len() < usize::from(self.cfg.window) && !self.queue.is_empty() {
            wanted = true;
            if budget == 0 {
                break;
            }
            budget -= 1;
            let (shell, payload) = self.queue.pop_front().expect("checked non-empty");
            let seq = self.next_seq;
            self.next_seq = self.next_seq.wrapping_add(1);
            self.inflight.push_back(Slot {
                seq,
                shell,
                payload: payload.clone(),
                attempts: 1,
                sent_at: now_ms,
            });
            out.push(Outgoing {
                kind: Kind::Data,
                seq,
                ack: self.next_expected,
                shell,
                payload,
                retransmit: false,
            });
        }

        // 3. A pure ack, only if nothing else is carrying it and it has waited long enough.
        if out.is_empty()
            && self
                .ack_owed_since
                .is_some_and(|t| now_ms >= t + self.cfg.ack_delay_ms)
        {
            wanted = true;
            if budget > 0 {
                budget -= 1;
                out.push(self.control(Kind::Ack));
            }
        }

        // 4. The close, last, once.
        // Only once everything sent has been acknowledged: a close that overtook lost data would
        // end the session with bytes the peer never got.
        if self.close_wanted
            && !self.close_sent
            && self.queue.is_empty()
            && self.inflight.is_empty()
        {
            wanted = true;
            if budget > 0 {
                out.push(self.control(Kind::Close));
                self.close_sent = true;
            }
        }

        if !out.is_empty() {
            self.ack_owed_since = None;
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
            payload: Vec::new(),
            retransmit: false,
        }
    }
}
