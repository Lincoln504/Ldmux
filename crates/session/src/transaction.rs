//! Message-oriented reliable transactions, above the AEAD record.
//!
//! This replaces the byte-stream [`crate::arq`] with a layer whose unit of transfer is a **whole
//! message**, not a byte range. The difference is not stylistic: an ordered byte stream can only be
//! retransmitted by number, so a record that no longer fitted the narrowed carrier had to be split
//! mid-flight, the head re-sealed under the same sequence number and the tail pushed back to the
//! front of the queue (`session.rs:259-271`, `arq.rs:755`). When the original arrived and its ack did
//! not, the peer's bytes were delivered twice and out of order — `RC-7`, "typed, pressed sync, went
//! blank".
//!
//! ## Wire
//!
//! Inside the AEAD plaintext, after the two inner header bytes the record layer owns:
//!
//! ```text
//! [tx_id: u16 LE][part_byte: u8][part_count: u8][ack_tx_id: u16 LE][ack_mask: u16 LE][tx_nonce: u16 LE][payload]
//! ```
//!
//! * `tx_id` is a monotonic message id (`1..=u16::MAX`). `0` means "this record is a pure
//!   acknowledgement" and carries no payload.
//! * `part_byte` bits `0..=5` are `part_idx` (`0..=15`); bits `6..=7` are the [`SubHalf`].
//! * `ack_tx_id` is the newest message the sender holds **any** part of, and `ack_mask` names which
//!   of its parts have arrived. A full mask means the message is whole, which is the only thing that
//!   promotes acknowledgement to delivery.
//! * `tx_nonce` is fresh on every transmission and ignored on arrival.
//!
//! **The nonce is not decoration, and the plan's original header lacked it.** MeshCore suppresses a
//! duplicate by `SHA256(payload_type || payload)` at every receiver and repeater (`Packet.cpp`), so a
//! retransmitted part whose bytes are unchanged is **dropped on arrival and the sender retries for
//! ever**. The byte-stream ARQ solved the same problem with one `mark` byte; the `msg1`/`msg2` boot
//! messages solve it with a trailing `tx_nonce`. `VirtualMesh` reproduces the loss exactly — 292
//! `DroppedDedup` entries in one eight-hop run — which is why this header is ten bytes and not eight.
//!
//! ## The five rules, and why each one exists
//!
//! 1. **A part index never changes meaning.** Parts are cut once, at [`TransactionLayer::begin`], and
//!    the vector is immutable afterwards. Retransmission re-sends the same index with the same bytes.
//! 2. **A part always fits the narrower carrier.** Parts are sized to the smaller of the direct
//!    capacity and twice the flood capacity, so any unacknowledged part can be carried as `SUB_A` +
//!    `SUB_B` under the same `(tx_id, part_idx)` when the route shrinks from direct to flood.
//! 3. **Acknowledgement is suppressed inside a burst.** An ack for every part collides at the first
//!    repeater with the data queued behind it, because the radio is half duplex. The ack is held until
//!    the message is whole, or until [`Config::burst_tail_ms`] after the burst began.
//! 4. **A newer message supersedes an older one of the same kind, cleanly.** The superseded message's
//!    parts are dropped, never merged: a partial `Snapshot` cannot contribute bytes to the next one.
//!    A message is also delivered the moment it is whole, not in `tx_id` order, so a newer snapshot is
//!    never held behind an older one that is still missing a part.
//! 5. **A timeout ends a transaction, never the session.** [`Action::TimedOut`] leaves every key and
//!    every peer's state intact, so the next record still decrypts and a later `tx_id` still works.
//!    The byte-stream ARQ's `Failure::RetriesExhausted` dropped the whole transport state instead.
//!
//! ## What this layer is not
//!
//! It does not know a radio, a path, a hop count, a preset or a shell. It is handed a [`RecordBudget`]
//! to size one record for, a `records_left` allowance, and a peer identity, and it returns bytes for
//! the caller to seal. That is what lets it be tested without a radio.

use std::collections::BTreeMap;

use ldmux_mesh::budget::RecordBudget;
use ldmux_mesh::constants::RECORD_MAX;

use crate::direction::{KIND_DOWN, KIND_UP, Side};
use crate::record;

/// The transaction header, inside the record layer's application bytes.
pub const TX_HEADER: usize = 10;

/// The most parts one message may have: `ACK_MASK_BITS`, because `ack_mask` is one `u16`.
pub const MAX_PARTS: usize = 16;

/// Bits available in `ack_mask`, which is also the most parts a peer can name at once.
pub const ACK_MASK_BITS: usize = 16;

/// How the halves of one part travel when the carrier is narrower than the part.
///
/// Two bits, three meanings. `Whole` is the normal case: one part, one record. `A` and `B` are the
/// halves of the same `(tx_id, part_idx)`, which is what makes a retry over a flood carrier a retry
/// rather than a new transaction.
#[derive(Clone, Copy, PartialEq, Eq, Debug, PartialOrd, Ord)]
pub enum SubHalf {
    /// The part fits the carrier and is carried in one record.
    Whole = 0,
    /// The low half of a part that had to be split for a narrower carrier.
    A = 1,
    /// The high half of the same part.
    B = 2,
}

impl SubHalf {
    const fn code(self) -> u8 {
        self as u8
    }

    const fn from_code(code: u8) -> Option<Self> {
        match code {
            0 => Some(Self::Whole),
            1 => Some(Self::A),
            2 => Some(Self::B),
            _ => None,
        }
    }
}

/// Who a transaction is addressed to.
///
/// A Noise session is one peer, so in practice there is one of these per layer. It is a value rather
/// than an assumption so that a peer's acknowledgement can only ever complete its **own** message,
/// which is asserted in the tests below.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug, Hash)]
pub struct PeerId(pub u64);

/// The kind of a message, which is the first byte of its payload.
///
/// This is the terminal protocol's message tag (`T_SEND`, `T_SNAPSHOT`, …), not a new field: the
/// transaction header is full, and the payload already begins with something that says what it is.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug, Hash)]
pub struct MessageKind(pub u8);

impl MessageKind {
    /// The kind of a message from its payload, or `None` when there are no bytes.
    #[must_use]
    pub fn of(payload: &[u8]) -> Option<Self> {
        payload.first().copied().map(Self)
    }
}

/// The transaction header on the wire.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Header {
    /// The message this record is part of, or `0` for a pure acknowledgement.
    pub tx_id: u16,
    /// Which part of `tx_id` this record carries, `0..=15`.
    pub part_idx: u8,
    /// How many parts `tx_id` has, `1..=16`; `0` when `tx_id` is `0`.
    pub part_count: u8,
    /// The newest message the sender holds any part of, or `0`.
    pub ack_tx_id: u16,
    /// Which parts of `ack_tx_id` the sender holds.
    pub ack_mask: u16,
    /// Whether this record is a whole part or one half of one.
    pub sub: SubHalf,
    /// Fresh on every transmission, ignored on arrival: it exists so no two transmissions of one part
    /// are the same bytes, which is what stops the mesh's duplicate suppression dropping a retry.
    pub tx_nonce: u16,
}

impl Header {
    /// Write the eight header bytes.
    pub fn encode(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(&self.tx_id.to_le_bytes());
        out.push(self.part_idx | (self.sub.code() << 6));
        out.push(self.part_count);
        out.extend_from_slice(&self.ack_tx_id.to_le_bytes());
        out.extend_from_slice(&self.ack_mask.to_le_bytes());
        out.extend_from_slice(&self.tx_nonce.to_le_bytes());
    }

    /// Read the eight header bytes, leaving the payload.
    #[must_use]
    pub fn decode(app: &[u8]) -> Option<(Self, &[u8])> {
        if app.len() < TX_HEADER {
            return None;
        }
        let tx_id = u16::from_le_bytes([app[0], app[1]]);
        let sub = SubHalf::from_code(app[2] >> 6)?;
        let part_idx = app[2] & 0x3F;
        let part_count = app[3];
        let ack_tx_id = u16::from_le_bytes([app[4], app[5]]);
        let ack_mask = u16::from_le_bytes([app[6], app[7]]);
        let tx_nonce = u16::from_le_bytes([app[8], app[9]]);
        Some((
            Self {
                tx_id,
                part_idx,
                part_count,
                ack_tx_id,
                ack_mask,
                sub,
                tx_nonce,
            },
            &app[TX_HEADER..],
        ))
    }

    /// Whether this record is a pure acknowledgement.
    #[must_use]
    pub const fn is_pure_ack(&self) -> bool {
        self.tx_id == 0
    }
}

/// How many payload bytes a transaction record of `record_limit` bytes leaves.
///
/// Derived from the record layer's own overhead rather than restated, because that layer is the only
/// place allowed to decide how much of a record is application.
///
/// **The plan says 141 at a 171-byte record; the arithmetic says 137.** The record layer's overhead is
/// 24 bytes, not 22 — its two inner header bytes (flags and shell) came out of the plan's count — and
/// the transaction header is ten bytes rather than eight, because a part that is retransmitted must
/// not be byte-identical to the copy that preceded it.
#[must_use]
pub fn payload_capacity(record_limit: usize) -> usize {
    record::application_bytes(record_limit.min(RECORD_MAX)).saturating_sub(TX_HEADER)
}

/// The largest part that can always be carried, whole on a direct route and halved on a flood.
///
/// Twice the flood capacity, because [`SubHalf`] offers exactly two halves. A part cut larger than
/// this could never be retried over the narrower carrier without renumbering it, which is the defect
/// this layer exists to remove.
///
/// At the public preset this is 118 bytes: `min(137, 2 x 59)`.
#[must_use]
pub fn max_part_payload(direct_record_limit: usize, flood_record_limit: usize) -> usize {
    let direct = payload_capacity(direct_record_limit);
    let halved = 2 * payload_capacity(flood_record_limit);
    if direct < halved { direct } else { halved }
}

/// What the layer needs to know that is not on the wire.
#[derive(Clone, Debug)]
pub struct Config {
    /// Which end this is: it decides the record kind a part is carried in.
    pub side: Side,
    /// One full record's time on the air at the carrier's own tuple, in milliseconds.
    ///
    /// Used for [`Config::burst_tail_ms`] and nothing else: retransmission timing is the caller's,
    /// because only the caller knows the route.
    pub airtime_ms: f64,
    /// The largest record a direct carrier allows here, in bytes.
    pub direct_record_limit: usize,
    /// The largest record a flood is guaranteed to have reported at the far end, in bytes.
    pub flood_record_limit: usize,
    /// How many passes over a message's unacknowledged parts before it is given up on.
    pub max_attempts: u8,
    /// The first retransmission deadline, in milliseconds.
    pub rto_ms: u64,
}

impl Config {
    /// How long a burst may stay open before its acknowledgement must go, in milliseconds.
    ///
    /// `2.5 x airtime`: the longest a repeater holds a flood packet before forwarding it
    /// (`getRetransmitDelay`, `0..5 x airtime x 0.5`), so a burst's tail has left the first repeater
    /// by then and an ack will not collide with it.
    #[must_use]
    pub fn burst_tail_ms(&self) -> u64 {
        let ms = (2.5 * self.airtime_ms).round();
        if ms < 1.0 { 1 } else { ms as u64 }
    }

    /// The largest part this configuration can cut.
    #[must_use]
    pub fn part_payload(&self) -> usize {
        max_part_payload(self.direct_record_limit, self.flood_record_limit)
    }

    /// How long one part must stand alone on the air before the next may follow it.
    ///
    /// The same `2.5 x airtime` as [`Config::burst_tail_ms`], because it is the same physical fact:
    /// a repeater is **half duplex**, so while it is forwarding the record it just heard it cannot
    /// hear anything else. `ldmux_mesh::sim::VirtualMesh` measures this directly — sixteen parts
    /// handed over back to back lose all but the first at the first repeater, and then the retry
    /// machinery spends the rest of the allowance rediscovering that.
    #[must_use]
    pub fn part_pace_ms(&self) -> u64 {
        self.burst_tail_ms()
    }
}

/// One thing the caller should do, in the order [`TransactionLayer::poll`] produced it.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Action {
    /// Seal `bytes` as a record and send it to `peer`.
    Send {
        /// Who it is for.
        peer: PeerId,
        /// The encoded [`Header`] and payload, ready for the record layer.
        bytes: Vec<u8>,
        /// The message it belongs to.
        tx_id: u16,
        /// Which part.
        part_idx: u8,
        /// Whether it is a whole part or one half.
        sub: SubHalf,
        /// Whether these bytes have been on the air before.
        retransmit: bool,
    },
    /// Seal `bytes` as a record and send it to `peer`: an acknowledgement with no payload.
    StandaloneAck {
        /// Who it is for.
        peer: PeerId,
        /// The encoded [`Header`], ready for the record layer.
        bytes: Vec<u8>,
    },
    /// The peer acknowledged the whole of `tx_id`.
    Complete {
        /// Which peer.
        peer: PeerId,
        /// Which message.
        tx_id: u16,
        /// What kind of message it was.
        kind: MessageKind,
    },
    /// `tx_id` ran out of attempts. The session is untouched.
    TimedOut {
        /// Which peer.
        peer: PeerId,
        /// Which message.
        tx_id: u16,
        /// What kind of message it was.
        kind: MessageKind,
    },
    /// `tx_id` was abandoned because a newer message of the same kind replaced it.
    Superseded {
        /// Which peer.
        peer: PeerId,
        /// Which message.
        tx_id: u16,
        /// What kind of message it was.
        kind: MessageKind,
    },
}

/// What arrived in one record.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Received {
    /// A whole message, reassembled and complete.
    Message {
        /// Which peer sent it.
        peer: PeerId,
        /// Which message.
        tx_id: u16,
        /// What kind it is, from its first payload byte.
        kind: MessageKind,
        /// The message's bytes.
        payload: Vec<u8>,
        /// The peer's acknowledgement of what we sent them.
        ack_tx_id: u16,
        /// Which parts of `ack_tx_id` they hold.
        ack_mask: u16,
    },
    /// One part of a message that is not whole yet.
    Part {
        /// Which peer sent it.
        peer: PeerId,
        /// Which message.
        tx_id: u16,
        /// Which part arrived.
        part_idx: u8,
        /// Whether it was a whole part or one half.
        sub: SubHalf,
    },
    /// The peer's acknowledgement of what we sent them, with no payload.
    Ack {
        /// Which peer.
        peer: PeerId,
        /// The newest message they hold any part of.
        tx_id: u16,
        /// Which of its parts.
        mask: u16,
    },
    /// Nothing to hand up, and why.
    Ignored(Ignored),
}

/// Why a record carried nothing this layer could use.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Ignored {
    /// Shorter than the transaction header, or a `part_byte` with a reserved sub-half value.
    Malformed,
    /// `part_count` is zero or past [`MAX_PARTS`], `part_idx` is past `part_count`, or a message
    /// already being reassembled arrived with a different part count.
    BadPartCount,
    /// A part of a message that was already delivered, or one this layer never began. Nothing is
    /// joined to anything.
    StalePart,
}

/// Counters, so a test or a log line can say what happened without reading the bytes.
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub struct Stats {
    /// Records carrying a part that had not been on the air before.
    pub parts_sent: u64,
    /// Records carrying a part that had been on the air before.
    pub parts_retransmitted: u64,
    /// Records that carried only an acknowledgement.
    pub standalone_acks: u64,
    /// Records carrying half of a part, because the carrier was narrower than the part.
    pub halves_sent: u64,
    /// Messages the peer acknowledged whole.
    pub messages_completed: u64,
    /// Messages given up on.
    pub messages_timed_out: u64,
    /// Messages replaced by a newer one of the same kind.
    pub messages_superseded: u64,
    /// A part that this layer already held, arriving again.
    pub duplicate_parts: u64,
    /// Parts dropped because their message was already delivered.
    pub stale_parts: u64,
    /// Acknowledgements held inside a burst instead of being sent at once.
    pub acks_suppressed: u64,
    /// Messages handed up whole.
    pub messages_delivered: u64,
}

/// Which of a part's three forms have been emitted in the current pass.
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
struct Emitted {
    whole: bool,
    a: bool,
    b: bool,
}

impl Emitted {
    /// The next form this part still owes the air, if any.
    fn next(&self, needs_halves: bool) -> Option<SubHalf> {
        if !needs_halves {
            return if self.whole { None } else { Some(SubHalf::Whole) };
        }
        if !self.a {
            return Some(SubHalf::A);
        }
        if !self.b {
            return Some(SubHalf::B);
        }
        None
    }

    /// Whether any form of this part has taken the air in this pass.
    fn any(&self) -> bool {
        self.whole || self.a || self.b
    }

    fn mark(&mut self, sub: SubHalf) {
        match sub {
            SubHalf::Whole => self.whole = true,
            SubHalf::A => self.a = true,
            SubHalf::B => self.b = true,
        }
    }
}

/// One outbound message, cut into parts.
struct Outbound {
    peer: PeerId,
    tx_id: u16,
    kind: MessageKind,
    /// Immutable once cut: the whole point of the layer.
    parts: Vec<Vec<u8>>,
    /// Bit `i` set means part `i` is whole at the peer.
    acked: u16,
    /// Passes over the parts so far.
    attempts: u8,
    /// When the current pass began, for the retransmission deadline.
    pass_started_ms: u64,
    /// What each part owes this pass.
    emitted: Vec<Emitted>,
    /// Whether each part must go as two halves, decided when that part first takes the air in a
    /// pass.
    ///
    /// Per part, not per pass: a route that narrows mid-pass must not leave a part unsendable, and
    /// a part that has already been emitted keeps the form it went out in, so a peer never sees one
    /// index in two forms at once.
    halves: Vec<bool>,
    /// When the last part of this message was put on the air, for the inter-part pacing.
    last_sent_ms: Option<u64>,
    /// When the current pass must be considered lost.
    ///
    /// Derived from the pass's **own airtime**, never from `rto_ms` alone: sixteen parts paced
    /// `2.5 x airtime` apart take 40 airtimes to put on the air, and a 2.4 s retransmission deadline
    /// would declare the last of them lost before it had been sent, then send it again for ever.
    pass_deadline_ms: u64,
}

impl Outbound {
    /// Every part, as a mask.
    fn full_mask(&self) -> u16 {
        complete_mask(self.parts.len())
    }

    fn is_complete(&self) -> bool {
        self.acked & self.full_mask() == self.full_mask()
    }
}

/// The two halves of one part, as they arrive.
type Halves = (Option<Vec<u8>>, Option<Vec<u8>>);

/// One inbound message being reassembled.
struct Partial {
    part_count: usize,
    /// A whole part, once one has arrived.
    parts: Vec<Option<Vec<u8>>>,
    /// The two halves of a part that arrived split; joined into `parts` when both are present.
    halves: Vec<Halves>,
    /// The kind, from the first part that arrived.
    kind: Option<MessageKind>,
}

impl Partial {
    fn new(part_count: usize) -> Self {
        Self {
            part_count,
            parts: vec![None; part_count],
            halves: vec![(None, None); part_count],
            kind: None,
        }
    }

    fn mask(&self) -> u16 {
        let mut mask = 0u16;
        for (i, slot) in self.parts.iter().enumerate() {
            if slot.is_some() && i < ACK_MASK_BITS {
                mask |= 1u16 << i;
            }
        }
        mask
    }

    fn is_complete(&self) -> bool {
        self.parts.iter().all(Option::is_some)
    }

    fn join(self) -> Vec<u8> {
        let mut out = Vec::new();
        for part in self.parts.into_iter().flatten() {
            out.extend_from_slice(&part);
        }
        out
    }
}

/// One peer's inbound state.
#[derive(Default)]
struct PeerState {
    /// The newest message we hold any part of.
    ack_tx_id: u16,
    /// Which of its parts we hold.
    ack_mask: u16,
    /// The highest message already delivered whole.
    highest_delivered: u16,
    /// Messages being reassembled.
    partial: BTreeMap<u16, Partial>,
    /// An acknowledgement is owed to this peer.
    ack_owed: bool,
    /// When the earliest unacknowledged part of the current burst arrived.
    burst_started_ms: Option<u64>,
}

/// The message-oriented reliable transaction layer.
///
/// One of these sits on each end of a session. It is pure: it holds no clock, opens no socket and
/// seals nothing. [`TransactionLayer::poll`] returns work; the caller does it.
pub struct TransactionLayer {
    config: Config,
    next_tx_id: u16,
    outbound: BTreeMap<u16, Outbound>,
    peers: BTreeMap<PeerId, PeerState>,
    /// Kinds whose newer instance replaces an older, still-incomplete one.
    superseding: Vec<MessageKind>,
    stats: Stats,
    /// Actions that happened without a caller asking (a supersession), drained first by `poll`.
    events: Vec<Action>,
    /// The next per-transmission nonce. Any change makes the bytes new to the mesh.
    next_nonce: u16,
}

impl TransactionLayer {
    /// A layer with no messages in flight.
    #[must_use]
    pub fn new(config: Config) -> Self {
        Self {
            config,
            next_tx_id: 1,
            outbound: BTreeMap::new(),
            peers: BTreeMap::new(),
            superseding: Vec::new(),
            stats: Stats::default(),
            events: Vec::new(),
            next_nonce: 1,
        }
    }

    /// A nonce for one transmission: never the same twice in a row, and never zero.
    fn nonce(&mut self) -> u16 {
        let nonce = self.next_nonce;
        self.next_nonce = if nonce == u16::MAX { 1 } else { nonce + 1 };
        nonce
    }

    /// Mark kinds whose newer instance replaces an older incomplete one.
    ///
    /// A screen snapshot is the case this exists for: the newer picture makes the older one
    /// pointless, and waiting for it delays the only thing the operator is waiting for.
    pub fn set_superseding(&mut self, kinds: &[MessageKind]) {
        self.superseding = kinds.to_vec();
    }

    /// The counters.
    #[must_use]
    pub fn stats(&self) -> Stats {
        self.stats
    }

    /// When the caller should look at this layer again even if nothing arrives.
    ///
    /// The earliest instant at which something becomes possible without new input: a retransmission
    /// pass's deadline, or the end of a burst's tail when an acknowledgement is owed. A caller with a
    /// real clock schedules its next wake-up from this instead of polling on a fixed tick.
    #[must_use]
    pub fn next_deadline_ms(&self) -> Option<u64> {
        let pass = self.outbound.values().map(|o| o.pass_deadline_ms).min();
        let ack = self
            .peers
            .values()
            .filter(|p| p.ack_owed)
            .filter_map(|p| p.burst_started_ms.map(|at| at.saturating_add(self.config.burst_tail_ms())))
            .min();
        match (pass, ack) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        }
    }

    /// How long a caller must leave between two parts, in milliseconds.
    ///
    /// A caller driving a real clock needs this to know when to poll again; the layer enforces the
    /// same number itself, so this is a convenience and not a second rule.
    #[must_use]
    pub fn pace_ms(&self) -> u64 {
        self.config.part_pace_ms()
    }

    /// The record kind a part of this end's messages is carried in.
    #[must_use]
    pub fn record_kind(&self) -> u8 {
        match self.config.side {
            Side::Phone => KIND_UP,
            Side::Host => KIND_DOWN,
        }
    }

    /// Whether the next thing this layer sends to `peer` is a retransmission.
    ///
    /// A message that has already had one full pass over its unacknowledged parts is retransmitting,
    /// and a retransmission is evidence that the carriage the first pass used did not work. A caller
    /// chooses the carriage **before** asking for bytes, so it has to be able to ask this first: a
    /// part cut for a 171-byte direct record does not fit a 93-byte flood, and a caller that sent it
    /// as one would have it dropped at the far end without an error (`RC-6`).
    #[must_use]
    pub fn in_retransmit_pass(&self, peer: PeerId) -> bool {
        self.outbound
            .values()
            .filter(|o| o.peer == peer)
            .any(|o| o.attempts > 0)
    }

    /// The number of messages still in flight to `peer`.
    #[must_use]
    pub fn in_flight(&self, peer: PeerId) -> usize {
        self.outbound.values().filter(|o| o.peer == peer).count()
    }

    /// Hand a whole message to the layer, and get its transaction id.
    ///
    /// Parts are cut here from [`Config::part_payload`] and never re-cut, so a retransmission cannot
    /// change what an index means. The message's **kind is its first byte**, which is what makes the
    /// supersession rule at both ends agree: there is no second field that could disagree with it.
    ///
    /// # Errors
    ///
    /// [`BeginError::Empty`] for a zero-length message, [`BeginError::TooLarge`] when it needs more
    /// than [`MAX_PARTS`] parts, and [`BeginError::IdSpace`] when the transaction id would wrap onto a
    /// message still in flight.
    pub fn begin(&mut self, peer: PeerId, data: &[u8], now_ms: u64) -> Result<u16, BeginError> {
        // The kind **is** the message's first byte, on both ends, so it is read rather than declared:
        // a caller cannot label a snapshot as something else, and the receiver's supersession rule
        // sees exactly what the sender did.
        let kind = MessageKind::of(data).ok_or(BeginError::Empty)?;
        let capacity = self.config.part_payload();
        if capacity == 0 {
            return Err(BeginError::TooLarge);
        }
        let part_count = data.len().div_ceil(capacity);
        if part_count > MAX_PARTS {
            return Err(BeginError::TooLarge);
        }
        let tx_id = self.allocate_tx_id()?;

        // A newer message of a superseding kind replaces an older one from the same peer.
        if self.superseding.contains(&kind) {
            let replaced: Vec<u16> = self
                .outbound
                .values()
                .filter(|o| o.peer == peer && o.kind == kind && !o.is_complete())
                .map(|o| o.tx_id)
                .collect();
            for old in replaced {
                self.outbound.remove(&old);
                self.stats.messages_superseded += 1;
                self.events.push(Action::Superseded {
                    peer,
                    tx_id: old,
                    kind,
                });
            }
        }

        let parts: Vec<Vec<u8>> = data.chunks(capacity).map(<[u8]>::to_vec).collect();
        let parts_len = parts.len();
        let emitted = vec![Emitted::default(); parts.len()];
        self.outbound.insert(
            tx_id,
            Outbound {
                peer,
                tx_id,
                kind,
                parts,
                acked: 0,
                attempts: 0,
                pass_started_ms: now_ms,
                emitted,
                halves: vec![false; parts_len],
                last_sent_ms: None,
                pass_deadline_ms: now_ms.saturating_add(pass_deadline_ms(&self.config, 0, parts_len)),
            },
        );
        Ok(tx_id)
    }

    fn allocate_tx_id(&mut self) -> Result<u16, BeginError> {
        for _ in 0..=u16::MAX {
            let candidate = self.next_tx_id.max(1);
            self.next_tx_id = if candidate == u16::MAX {
                1
            } else {
                candidate + 1
            };
            if !self.outbound.contains_key(&candidate) {
                return Ok(candidate);
            }
        }
        Err(BeginError::IdSpace)
    }

    /// The next thing to put on the air, or `None` when there is nothing to do.
    ///
    /// Order is deliberate: a completion or a timeout first, so the caller learns about it before
    /// spending airtime; then an acknowledgement, which is never charged against the allowance
    /// because it is the traffic that keeps the link alive; then data, which is charged.
    #[must_use]
    pub fn poll(&mut self, now_ms: u64, carrier: RecordBudget, records_left: u8) -> Option<Action> {
        if let Some(event) = self.events.pop() {
            return Some(event);
        }
        if let Some(action) = self.take_completion() {
            return Some(action);
        }
        if let Some(action) = self.take_timeout(now_ms) {
            return Some(action);
        }
        if let Some(action) = self.readied_ack(now_ms) {
            return Some(action);
        }
        if records_left == 0 {
            return None;
        }
        self.take_part(now_ms, carrier)
    }

    /// Drain everything that would go out from `now_ms` onwards, pacing the parts.
    ///
    /// The parts come back with the caller expected to put each one on the air
    /// [`Config::part_pace_ms`] after the one before it, so this walks its own copy of the clock
    /// forward as it goes. A caller with a real clock loops on [`TransactionLayer::poll`] instead.
    #[must_use]
    pub fn poll_all(&mut self, now_ms: u64, carrier: RecordBudget, records_left: u8) -> Vec<Action> {
        let mut left = records_left;
        let mut out = Vec::new();
        let mut at = now_ms;
        loop {
            // One pass, not a replay of it: stop before the deadline that would start the next.
            // The first poll always happens, so an expiry that is already due is reported rather
            // than swallowed by the deadline it is due at.
            if !out.is_empty()
                && self
                    .next_deadline_ms()
                    .is_some_and(|deadline| at >= deadline)
            {
                break;
            }
            let Some(action) = self.poll(at, carrier, left) else {
                break;
            };
            if matches!(action, Action::Send { .. }) {
                at = at.saturating_add(self.config.part_pace_ms());
            }
            if matches!(
                action,
                Action::Send {
                    retransmit: false,
                    ..
                }
            ) {
                left = left.saturating_sub(1);
            }
            out.push(action);
            if out.len() > 4 * MAX_PARTS + 8 {
                break;
            }
        }
        out
    }

    fn take_completion(&mut self) -> Option<Action> {
        let (tx_id, peer, kind) = self
            .outbound
            .values()
            .find(|o| o.is_complete())
            .map(|o| (o.tx_id, o.peer, o.kind))?;
        self.outbound.remove(&tx_id);
        self.stats.messages_completed += 1;
        Some(Action::Complete { peer, tx_id, kind })
    }

    fn take_timeout(&mut self, now_ms: u64) -> Option<Action> {
        let attempts = self.config.max_attempts;
        let (tx_id, peer, kind) = self
            .outbound
            .values()
            .find(|o| o.attempts >= attempts && now_ms >= o.pass_deadline_ms)
            .map(|o| (o.tx_id, o.peer, o.kind))?;
        // Only the transaction goes. Every key, every replay window and every other peer stay.
        self.outbound.remove(&tx_id);
        self.stats.messages_timed_out += 1;
        Some(Action::TimedOut { peer, tx_id, kind })
    }

    /// Whether the owed acknowledgement may go now: the message is whole, or the burst's tail has
    /// left the first repeater.
    fn readied_ack(&mut self, now_ms: u64) -> Option<Action> {
        let tail = self.config.burst_tail_ms();
        let ready = self.peers.iter().find_map(|(peer, state)| {
            if !state.ack_owed {
                return None;
            }
            let whole = state
                .partial
                .get(&state.ack_tx_id)
                .is_none_or(Partial::is_complete);
            let elapsed = state
                .burst_started_ms
                .map_or(u64::MAX, |at| now_ms.saturating_sub(at));
            if whole || elapsed >= tail {
                Some(*peer)
            } else {
                None
            }
        })?;
        let (ack_tx_id, ack_mask) = self
            .peers
            .get_mut(&ready)
            .map(|state| {
                state.ack_owed = false;
                state.burst_started_ms = None;
                (state.ack_tx_id, state.ack_mask)
            })
            .expect("the peer was just found");
        let nonce = self.nonce();
        let header = Header {
            tx_id: 0,
            part_idx: 0,
            part_count: 0,
            ack_tx_id,
            ack_mask,
            sub: SubHalf::Whole,
            tx_nonce: nonce,
        };
        let mut bytes = Vec::with_capacity(TX_HEADER);
        header.encode(&mut bytes);
        self.stats.standalone_acks += 1;
        Some(Action::StandaloneAck { peer: ready, bytes })
    }

    fn take_part(&mut self, now_ms: u64, carrier: RecordBudget) -> Option<Action> {
        let capacity = payload_capacity(carrier.limit());
        if capacity == 0 {
            return None;
        }
        let tx_id = *self.outbound.keys().next()?;
        let (ack_tx_id, ack_mask) = self.outbound.get(&tx_id).map_or((0, 0), |o| {
            self.peers
                .get(&o.peer)
                .map_or((0, 0), |s| (s.ack_tx_id, s.ack_mask))
        });

        let max_attempts = self.config.max_attempts;
        let pace = self.config.part_pace_ms();
        // Taken before the message is borrowed mutably. A nonce spent on a call that sends nothing
        // is wasted and harmless: what matters is that two transmissions of one part never match.
        let nonce = self.nonce();
        let out = self.outbound.get_mut(&tx_id)?;

        // One part at a time, `part_pace_ms` apart: a repeater forwarding the previous part cannot
        // hear this one, so a burst handed over back to back loses its own tail.
        if let Some(last) = out.last_sent_ms
            && now_ms.saturating_sub(last) < pace
        {
            return None;
        }

        // A pass that owes nothing starts the next one, but only once the deadline has passed.
        let still_owed = out
            .parts
            .iter()
            .enumerate()
            .any(|(i, _)| out.acked & (1u16 << i) == 0 && out.emitted[i].next(out.halves[i]).is_some());
        if !still_owed {
            if now_ms < out.pass_deadline_ms || out.attempts >= max_attempts {
                return None;
            }
            out.attempts += 1;
            out.pass_started_ms = now_ms;
            out.pass_deadline_ms = now_ms.saturating_add(pass_deadline_ms(
                &self.config,
                out.attempts,
                out.parts.len(),
            ));
            for e in &mut out.emitted {
                *e = Emitted::default();
            }
        }

        let retransmit = out.attempts > 0;
        let part_count = u8::try_from(out.parts.len()).unwrap_or(u8::MAX);
        let mut chosen: Option<(usize, SubHalf, Vec<u8>)> = None;
        for (i, part) in out.parts.iter().enumerate() {
            if out.acked & (1u16 << i) != 0 {
                continue;
            }
            // The form is decided when this part first takes the air in this pass, from the
            // carrier in hand at that moment, and fixed for the rest of the pass.
            if !out.emitted[i].any() {
                out.halves[i] = part.len() > capacity;
            }
            let Some(sub) = out.emitted[i].next(out.halves[i]) else {
                continue;
            };
            let slice = match sub {
                SubHalf::Whole => part.clone(),
                SubHalf::A => {
                    let split = part.len().div_ceil(2);
                    part[..split].to_vec()
                }
                SubHalf::B => {
                    let split = part.len().div_ceil(2);
                    part[split..].to_vec()
                }
            };
            assert!(
                slice.len() <= capacity,
                "a {}-byte slice does not fit a {capacity}-byte record: parts are cut to {} and \
                 halved at most, so this is a bug in the form decision",
                slice.len(),
                self.config.part_payload()
            );
            chosen = Some((i, sub, slice));
            break;
        }

        let (part_idx, sub, slice) = chosen?;
        out.emitted[part_idx].mark(sub);
        out.last_sent_ms = Some(now_ms);
        let header = Header {
            tx_id,
            part_idx: u8::try_from(part_idx).unwrap_or(0),
            part_count,
            ack_tx_id,
            ack_mask,
            sub,
            tx_nonce: nonce,
        };
        let mut bytes = Vec::with_capacity(TX_HEADER + slice.len());
        header.encode(&mut bytes);
        bytes.extend_from_slice(&slice);

        if sub == SubHalf::Whole {
            if retransmit {
                self.stats.parts_retransmitted += 1;
            } else {
                self.stats.parts_sent += 1;
            }
        } else {
            self.stats.halves_sent += 1;
        }
        Some(Action::Send {
            peer: out.peer,
            bytes,
            tx_id,
            part_idx: u8::try_from(part_idx).unwrap_or(0),
            sub,
            retransmit,
        })
    }

    /// Take one record's application bytes from `peer`.
    ///
    /// Nothing is joined across messages: a part is placed at its own index of its own `tx_id`, and a
    /// message is handed up the moment every one of its parts is present.
    pub fn on_app(&mut self, peer: PeerId, app: &[u8], now_ms: u64) -> Received {
        let Some((header, payload)) = Header::decode(app) else {
            return Received::Ignored(Ignored::Malformed);
        };
        // The acknowledgement rides on **every** record, not only on a pure one: that is the whole
        // reason `ack_tx_id` and `ack_mask` are in the header rather than in a message of their own.
        // Applying it only when `tx_id == 0` leaves a link where the peer always has data to send
        // without any acknowledgement at all — every message is delivered and its sender still
        // retries until it gives up, because the ack arrived inside a record this branch never
        // reached. The record is authenticated by the time anything here sees it, so the fields are
        // as trustworthy as the payload beside them, and a mask wider than the peer's message is
        // harmless: completion is `acked & full_mask == full_mask`.
        if header.ack_tx_id != 0 {
            self.apply_ack(peer, header.ack_tx_id, header.ack_mask);
        }
        if header.is_pure_ack() {
            let (tx_id, mask) = (header.ack_tx_id, header.ack_mask);
            return Received::Ack { peer, tx_id, mask };
        }
        if header.part_count == 0
            || usize::from(header.part_count) > MAX_PARTS
            || header.part_idx >= header.part_count
        {
            return Received::Ignored(Ignored::BadPartCount);
        }
        if payload.is_empty() && header.part_count > 1 {
            return Received::Ignored(Ignored::Malformed);
        }

        let superseding = MessageKind::of(payload)
            .is_some_and(|kind| self.superseding.contains(&kind));
        let stale = {
            let state = self.peers.entry(peer).or_default();
            header.tx_id <= state.highest_delivered && !state.partial.contains_key(&header.tx_id)
        };
        if stale {
            let state = self.peers.entry(peer).or_default();
            self.stats.stale_parts += 1;
            // Still owe an acknowledgement: the peer believes this part is missing, and saying
            // otherwise costs nothing.
            state.ack_owed = true;
            state.burst_started_ms.get_or_insert(now_ms);
            return Received::Ignored(Ignored::StalePart);
        }

        let part_count = usize::from(header.part_count);
        let idx = usize::from(header.part_idx);
        let state = self.peers.entry(peer).or_default();

        if superseding {
            let older: Vec<u16> = state
                .partial
                .keys()
                .copied()
                .filter(|id| *id < header.tx_id)
                .collect();
            for old in older {
                state.partial.remove(&old);
                self.stats.messages_superseded += 1;
            }
        }

        let entry = state
            .partial
            .entry(header.tx_id)
            .or_insert_with(|| Partial::new(part_count));
        if entry.part_count != part_count {
            // A different part count for a message already being reassembled: refuse rather than
            // resize, because a resize would silently renumber what the peer already sent.
            return Received::Ignored(Ignored::BadPartCount);
        }
        if entry.kind.is_none() {
            entry.kind = MessageKind::of(payload);
        }

        let already = match header.sub {
            SubHalf::Whole => entry.parts[idx].is_some(),
            SubHalf::A => entry.halves[idx].0.is_some(),
            SubHalf::B => entry.halves[idx].1.is_some(),
        };
        if already {
            self.stats.duplicate_parts += 1;
        } else {
            match header.sub {
                SubHalf::Whole => entry.parts[idx] = Some(payload.to_vec()),
                SubHalf::A => entry.halves[idx].0 = Some(payload.to_vec()),
                SubHalf::B => entry.halves[idx].1 = Some(payload.to_vec()),
            }
            join_halves(entry, idx);
        }
        state.ack_tx_id = header.tx_id;
        state.ack_mask = state.partial[&header.tx_id].mask();
        state.ack_owed = true;
        state.burst_started_ms.get_or_insert(now_ms);

        if !state.partial[&header.tx_id].is_complete() {
            if header.part_idx + 1 < header.part_count {
                // Parts are still queued behind this one, so the ack waits for the burst's tail.
                self.stats.acks_suppressed += 1;
            }
            return Received::Part {
                peer,
                tx_id: header.tx_id,
                part_idx: header.part_idx,
                sub: header.sub,
            };
        }

        let partial = state
            .partial
            .remove(&header.tx_id)
            .expect("the entry was just inspected");
        let kind = partial.kind.unwrap_or(MessageKind(0));
        let payload = partial.join();
        let whole_mask = complete_mask(part_count);
        state.ack_mask = whole_mask;
        state.highest_delivered = state.highest_delivered.max(header.tx_id);
        self.stats.messages_delivered += 1;
        Received::Message {
            peer,
            tx_id: header.tx_id,
            kind,
            payload,
            ack_tx_id: state.ack_tx_id,
            ack_mask: state.ack_mask,
        }
    }

    fn apply_ack(&mut self, peer: PeerId, tx_id: u16, mask: u16) {
        if tx_id == 0 {
            return;
        }
        if let Some(out) = self.outbound.get_mut(&tx_id) {
            // A peer's acknowledgement may only ever complete its own message.
            if out.peer == peer {
                out.acked |= mask;
            }
        }
    }
}

fn join_halves(entry: &mut Partial, idx: usize) {
    if entry.parts[idx].is_some() {
        return;
    }
    let (a, b) = &entry.halves[idx];
    if let (Some(low), Some(high)) = (a, b) {
        let mut joined = Vec::with_capacity(low.len() + high.len());
        joined.extend_from_slice(low);
        joined.extend_from_slice(high);
        entry.parts[idx] = Some(joined);
    }
}

/// How long one pass over `parts` may take before it is considered lost.
///
/// The larger of the caller's retransmission timeout, doubled per attempt, and the **airtime the
/// pass itself needs**: a pass cannot be late before it has finished being sent.
fn pass_deadline_ms(config: &Config, attempts: u8, parts: usize) -> u64 {
    let by_timeout = config.rto_ms.saturating_mul(1u64 << attempts.min(6));
    // Two emissions a part at most: a part that must be halved takes two records, and the deadline
    // has to cover the widest pass this message could produce.
    let by_airtime = config
        .part_pace_ms()
        .saturating_mul(2 * u64::try_from(parts).unwrap_or(u64::MAX));
    if by_timeout > by_airtime {
        by_timeout
    } else {
        by_airtime
    }
}

fn complete_mask(part_count: usize) -> u16 {
    if part_count >= ACK_MASK_BITS {
        u16::MAX
    } else {
        (1u16 << part_count) - 1
    }
}

/// Why a message could not be handed to the layer.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum BeginError {
    /// A message with no bytes has nothing to send.
    Empty,
    /// It needs more than [`MAX_PARTS`] parts at this capacity.
    TooLarge,
    /// Every transaction id is in use, which cannot happen with [`MAX_PARTS`] in flight and means
    /// something is holding them.
    IdSpace,
}

impl std::fmt::Display for BeginError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Empty => write!(f, "a message with no bytes has nothing to send"),
            Self::TooLarge => write!(
                f,
                "the message needs more than {MAX_PARTS} parts at this record size"
            ),
            Self::IdSpace => write!(f, "no transaction id is free"),
        }
    }
}

impl std::error::Error for BeginError {}

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> Config {
        Config {
            side: Side::Phone,
            airtime_ms: 402.0,
            direct_record_limit: 171,
            flood_record_limit: 93,
            max_attempts: 5,
            rto_ms: 2_400,
        }
    }

    fn layer() -> TransactionLayer {
        TransactionLayer::new(config())
    }

    fn direct() -> RecordBudget {
        RecordBudget::direct(0)
    }

    fn flood() -> RecordBudget {
        RecordBudget::flood_scoped(64)
    }

    #[test]
    fn the_record_layer_overhead_decides_the_payload_not_a_literal() {
        // 171 - 24 (6 clear + 2 inner + 16 tag) - 10 transaction header.
        assert_eq!(payload_capacity(171), 137);
        assert_eq!(payload_capacity(93), 59);
        // Twice the flood capacity, because a part has two halves.
        assert_eq!(max_part_payload(171, 93), 118);
        assert_eq!(config().part_payload(), 118);
        // The two record sizes the plan promises, straight from the budget module.
        assert_eq!(flood().limit(), 93);
        assert_eq!(direct().limit(), 171);
    }

    #[test]
    fn the_burst_tail_is_two_and_a_half_airtimes() {
        assert_eq!(config().burst_tail_ms(), 1_005);
    }

    #[test]
    fn a_header_round_trips_every_sub_half_and_index() {
        for sub in [SubHalf::Whole, SubHalf::A, SubHalf::B] {
            for idx in 0..MAX_PARTS as u8 {
                let header = Header {
                    tx_id: 4096,
                    part_idx: idx,
                    part_count: 16,
                    ack_tx_id: 4095,
                    ack_mask: 0xBEEF,
                    sub,
                    tx_nonce: 0x1234,
                };
                let mut bytes = Vec::new();
                header.encode(&mut bytes);
                bytes.extend_from_slice(b"payload");
                let (back, rest) = Header::decode(&bytes).expect("decodes");
                assert_eq!(back, header);
                assert_eq!(rest, b"payload");
            }
        }
    }

    #[test]
    fn a_reserved_sub_half_is_refused_rather_than_guessed() {
        let mut bytes = vec![1, 0, 0b11_000000, 1, 0, 0, 0, 0];
        bytes.extend_from_slice(b"x");
        assert_eq!(Header::decode(&bytes), None);
    }

    #[test]
    fn a_message_is_cut_once_and_never_renumbered() {
        let mut tx = layer();
        let peer = PeerId(7);
        let mut data: Vec<u8> = (0..600u32).map(|i| (i % 251) as u8).collect();
        data[0] = 0x0E;
        let tx_id = tx.begin(peer, &data, 0).unwrap();
        let actions = tx.poll_all(0, direct(), 255);
        let mut reconstructed: Vec<(u8, Vec<u8>)> = Vec::new();
        for action in actions {
            if let Action::Send {
                tx_id: id,
                part_idx,
                sub,
                bytes,
                ..
            } = action
            {
                assert_eq!(id, tx_id);
                assert_eq!(sub, SubHalf::Whole);
                reconstructed.push((part_idx, bytes[TX_HEADER..].to_vec()));
            }
        }
        reconstructed.sort_by_key(|(idx, _)| *idx);
        let joined: Vec<u8> = reconstructed.into_iter().flat_map(|(_, b)| b).collect();
        assert_eq!(joined, data, "the parts are the message, in order, once");
    }

    #[test]
    fn a_message_too_large_to_number_is_refused_rather_than_truncated() {
        let mut tx = layer();
        let peer = PeerId(1);
        assert_eq!(tx.begin(peer, &[], 0), Err(BeginError::Empty));
        // Sixteen parts of 118 bytes is the ceiling, and one byte more is refused.
        let mut too_big = vec![1u8; 118 * 16 + 2];
        too_big[0] = 0x0E;
        assert_eq!(tx.begin(peer, &too_big, 0), Err(BeginError::TooLarge));
        // The largest message that can be numbered is accepted.
        let mut biggest = vec![1u8; 118 * 16];
        biggest[0] = 0x0E;
        assert!(tx.begin(peer, &biggest, 0).is_ok());
    }

    #[test]
    fn a_peers_acknowledgement_can_only_complete_its_own_message() {
        let mut tx = layer();
        let mine = PeerId(1);
        let stranger = PeerId(2);
        let tx_id = tx.begin(mine, b"\x0Ehello", 0).unwrap();
        // The stranger acknowledges everything.
        assert_eq!(
            tx.on_app(stranger, &ack_bytes(tx_id, u16::MAX), 0),
            Received::Ack {
                peer: stranger,
                tx_id,
                mask: u16::MAX
            }
        );
        // It completes nothing: the message is still in flight to its own peer.
        assert_eq!(tx.in_flight(mine), 1);
    }

    fn ack_bytes(tx_id: u16, mask: u16) -> Vec<u8> {
        let mut bytes = Vec::new();
        Header {
            tx_id: 0,
            part_idx: 0,
            part_count: 0,
            ack_tx_id: tx_id,
            ack_mask: mask,
            sub: SubHalf::Whole,
            tx_nonce: 1,
        }
        .encode(&mut bytes);
        bytes
    }
}
