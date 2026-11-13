//! The radio endpoint: one state machine for both ends of the link.
//!
//! Everything that used to be four loops and two languages lives here instead, as a pure type with
//! no clock, no socket and no thread:
//!
//! * which **end** this is ([`Role`]), which decides who initiates and which record kinds are ours
//!   to read at all ([`crate::direction`], ahead of any decryption);
//! * the **bootstrap** — one initiation held across every retry rung ([`Attempt`]) and an answer
//!   cache that makes a repeated initiation cost an answer rather than a session ([`AnswerCache`]);
//! * the **route** — which path we believe in, how good it is, and whether it is proven
//!   ([`ldmux_mesh::route`]), updated only after a record authenticates;
//! * the **carriage** for each attempt — direct while a usable path is held, flood from the second
//!   attempt on, decided per transmission instead of by a sticky flag;
//! * the **transaction layer** — parts cut once, acknowledgement held inside a burst, a timeout that
//!   never destroys the session ([`crate::transaction`]).
//!
//! # Why the carrier is chosen before the record is sealed
//!
//! `RC-6` and `RC-7` are the same inversion seen from two sides. A direct record may be `168` bytes
//! on a one-hop path and `108` on a sixty-four-byte path; a flood is `157` on a short path, `93` at
//! the firmware's longest legal one, and a receiver drops anything whose packet exceeds `173` bytes
//! without reporting it. A layer that seals first and lets the carrier choose afterwards therefore
//! produces records that are transmitted, forwarded by every repeater, heard, and thrown away.
//!
//! So [`RadioEndpoint::poll`] chooses the carriage for **this** transmission, derives the byte
//! ceiling from it ([`RecordBudget`]), and only then asks the transaction layer for bytes that fit.
//!
//! # What the caller still owns
//!
//! Transmitting and receiving are the caller's: the endpoint hands records to a [`Carrier`] and
//! decides what an arriving record means, but it never waits on the radio itself. An arrival is fed
//! back through [`RadioEndpoint::on_record`] with the shape and path the radio reported, because a
//! direct packet with hops left in its path is still in transit and must not be read as ours — the
//! distinction `RC-3` turned into a broken link.
//!
//! [`Attempt`]: crate::bootstrap::Attempt
//! [`AnswerCache`]: crate::bootstrap::AnswerCache

use std::collections::VecDeque;

use ldmux_mesh::budget::{MAX_PATH_BYTES, RecordBudget};
use ldmux_mesh::carrier::Carrier;
use ldmux_mesh::constants::CREDIT_WINDOW;
use ldmux_mesh::error::MeshError;
use ldmux_mesh::route::{Candidate, Carriage, FIRST_ATTEMPT, ProofWitness, RouteTable};

use crate::bootstrap::{
    self, Answer, AnswerCache, Answered, Attempt, NeedHandshakeSender, Refusal,
};
use crate::direction::{
    self, Arrival, KIND_DOWN, KIND_MSG1, KIND_MSG1_FRAG, KIND_MSG2, KIND_NEED_HANDSHAKE, KIND_UP,
    Side, Verdict,
};
use crate::handshake::{
    Established, HandshakeError, Keypair, PairingEntry, Responder, TIMESTAMP_WINDOW_MS,
};
use crate::record::{self, RecordError, ReplayWindow};
use crate::transaction::{Action, BeginError, PeerId, Received, TransactionLayer};

/// Which end of the link this is.
///
/// [`Side`] already names the two ends and already decides which kinds each may read, so the role
/// **is** that type rather than a second spelling of it: an endpoint that thought it was a host
/// while the filter thought it was a phone would be exactly the confusion `RC-3` came from.
pub use crate::direction::Side as Role;

/// How long after sending along a path a direct arrival still counts as evidence about it.
///
/// A direct send is answered inside a rung, and the rung's floor is 12 s, so ten seconds covers any
/// neighbour's round trip while staying far short of the minute a stale send would need to be
/// mistaken for this one.
const PROVE_WINDOW_MS: u64 = 10_000;

/// How long after the flooded answer its direct copy follows.
///
/// Behind the flood on purpose: the flood is the copy that teaches a far end the chain, and two
/// packets sent at the same instant collide on a half-duplex radio. 900 ms is longer than one
/// packet's airtime at the shipped preset and shorter than a bootstrap rung.
const ANSWER_DIRECT_DELAY_MS: u64 = 900;

/// Where this end's own transmission nonces start.
///
/// A copy must not be byte-identical to the answer it copies: the firmware's duplicate table hashes
/// the payload, so an identical copy is dropped before it is ever forwarded. The two ends count from
/// opposite halves of the space so a copy and an original can never share a nonce.
const ENDPOINT_NONCE_BASE: u16 = 0x8000;

/// How a record reached this end: its shape, the path the carrier reported, and the SNR.
///
/// Both fields are needed and neither can be derived from the other. [`Arrival`] carries the hop
/// count, which is the transit test and the rung arithmetic; the path **bytes** are what a route is
/// made of, and a flood's chain reversed is the only way a node that has never spoken to the peer
/// learns where it is.
#[derive(Clone, PartialEq, Debug)]
pub struct ArrivalInfo {
    /// Whether it came as a direct packet or a flood, and how many hops that was.
    pub arrival: Arrival,
    /// The path as received: hashes still to travel for a direct packet (empty when it arrived),
    /// the chain it crossed for a flood.
    pub path: Vec<u8>,
    /// The SNR of the last link, in dB.
    pub snr_db: f32,
}

impl ArrivalInfo {
    /// A direct record that finished crossing its path: the only shape that can prove a route.
    #[must_use]
    pub fn direct(snr_db: f32) -> Self {
        Self {
            arrival: Arrival::Direct { hops: 0 },
            path: Vec::new(),
            snr_db,
        }
    }

    /// A direct record still carrying `hops` hashes, which is in transit to somebody else.
    #[must_use]
    pub fn in_transit(hops: u8, snr_db: f32) -> Self {
        Self {
            arrival: Arrival::Direct { hops },
            path: Vec::new(),
            snr_db,
        }
    }

    /// A flood, with the chain it crossed. `path_width` is the hash width the path is encoded with.
    #[must_use]
    pub fn flood(path: Vec<u8>, path_width: u8, snr_db: f32) -> Self {
        let width = usize::from(path_width.clamp(1, 3));
        let hops = u8::try_from(path.len() / width).unwrap_or(u8::MAX);
        Self {
            arrival: Arrival::Flood { hops },
            path,
            snr_db,
        }
    }
}

/// What an arriving record was.
#[derive(Clone, PartialEq, Debug)]
pub enum Admitted {
    /// A whole message, reassembled by the transaction layer and waiting in
    /// [`RadioEndpoint::take_inbound`].
    Message {
        /// Which message.
        tx_id: u16,
        /// Its kind, from its first payload byte.
        kind: u8,
    },
    /// One part of a message that is not whole yet.
    Part {
        /// Which message.
        tx_id: u16,
        /// Which part arrived.
        part_idx: u8,
    },
    /// The peer's acknowledgement of what we sent, with no payload.
    Ack,
    /// An initiation was answered, freshly or from the cache.
    Answered {
        /// Whether the answer came from the cache, which is the idempotence this exists for.
        cached: bool,
    },
    /// An answer finished this end's attempt.
    AnswerAccepted {
        /// Whether it was a repeat of the answer that already finished it.
        repeated: bool,
    },
    /// The host asked this end to handshake again, and the request authenticated.
    HandshakeRequested,
    /// Nothing to hand up, and why.
    Ignored(Rejected),
}

/// Why an arriving record was not used.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Rejected {
    /// This end originates this kind: it is hearing its own transmission come back.
    SelfEcho,
    /// A direct packet with hops left, on its way to another node.
    InTransit,
    /// A kind this protocol does not define.
    UnknownKind,
    /// A record of a kind that belongs to this side but no state can use: an answer with no
    /// attempt outstanding, or an initiation arriving at the phone.
    NotForUs,
    /// No session, so a transport record cannot be opened.
    NoSession,
    /// The record layer refused it.
    Record(RecordError),
    /// The handshake refused an initiation.
    Refusal(Refusal),
    /// The handshake could not consume the answer.
    Handshake(HandshakeError),
    /// The request to handshake again did not authenticate.
    BadHandshakeRequest,
}

/// One record the endpoint handed to the carrier, or one thing it decided.
///
/// [`RadioEndpoint::poll`] does the transmitting, so these are a report: what went on the air, on
/// which carriage, and every state change that happened while it was deciding. A caller logs them,
/// and a test asserts on them.
#[derive(Clone, PartialEq, Debug)]
pub enum Outbound {
    /// A record was handed to the carrier.
    Sent {
        /// What it carried.
        what: SentKind,
        /// Which carriage it took.
        carriage: Carriage,
        /// Its length in bytes.
        bytes: usize,
        /// When it was handed over.
        at_ms: u64,
    },
    /// The peer acknowledged a message of ours whole.
    Completed {
        /// Which message.
        tx_id: u16,
    },
    /// A message of ours ran out of attempts. The session is untouched.
    TimedOut {
        /// Which message.
        tx_id: u16,
    },
    /// A newer message of the same kind replaced an older one.
    Superseded {
        /// Which message was dropped.
        tx_id: u16,
    },
    /// Every rung of the bootstrap went unanswered.
    BootstrapFailed {
        /// How many rungs were spent.
        rungs: u8,
    },
}

/// What a sent record carried.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SentKind {
    /// The Noise initiation, whole or in parts.
    Initiation,
    /// The answer to an initiation.
    Answer,
    /// A part of a message.
    Part {
        /// Which message.
        tx_id: u16,
        /// Which part.
        part_idx: u8,
        /// Whether these bytes had been on the air before.
        retransmit: bool,
    },
    /// An acknowledgement with no payload.
    Ack,
    /// The PSK-tagged request to handshake again.
    NeedHandshake,
}

/// Counters, so a field failure can be read off a log instead of guessed at.
#[derive(Clone, Default, Debug)]
pub struct Stats {
    /// Initiations handed to the carrier, one per rung.
    pub initiations_sent: u64,
    /// Answers handed to the carrier.
    pub answers_sent: u64,
    /// Answers served from the cache, which rebuilt nothing.
    pub answers_from_cache: u64,
    /// Direct copies of answers sent back along the initiation's chain.
    pub answers_direct: u64,
    /// `KIND_NEED_HANDSHAKE` records sent.
    pub need_handshake_sent: u64,
    /// `KIND_NEED_HANDSHAKE` records that verified.
    pub need_handshake_heard: u64,
    /// Retry rungs spent on bootstrap attempts.
    pub rungs_used: u64,
    /// Bootstrap attempts that ran out of rungs.
    pub bootstrap_failures: u64,
    /// Sessions established, on either end.
    pub sessions_established: u64,
    /// Records handed to the carrier.
    pub records_sent: u64,
    /// Records that arrived and were considered.
    pub records_received: u64,
    /// Records that passed the direction filter and authenticated.
    pub records_admitted: u64,
    /// Records rejected as this end's own transmission.
    pub rejected_self_echo: u64,
    /// Records rejected because they were still in transit.
    pub rejected_in_transit: u64,
    /// Records rejected for carrying a kind this protocol does not define.
    pub rejected_unknown_kind: u64,
    /// Records rejected by the record layer, by reason.
    pub rejected_record: u64,
    /// Records rejected by the handshake, by reason.
    pub rejected_handshake: u64,
    /// Paths learned from a flood's reversed chain.
    pub routes_learned: u64,
    /// Paths proven by an authenticated direct arrival.
    pub routes_proven: u64,
    /// Records sent on the direct carriage.
    pub carriage_direct: u64,
    /// Records sent as floods.
    pub carriage_flood: u64,
    /// Messages the peer acknowledged whole.
    pub transactions_completed: u64,
    /// Messages given up on, with the session intact.
    pub transactions_timed_out: u64,
    /// Messages handed up whole.
    pub messages_delivered: u64,
}

impl Stats {
    fn note_sent(&mut self, carriage: &Carriage) {
        if carriage.is_flood() {
            self.carriage_flood += 1;
        } else {
            self.carriage_direct += 1;
        }
    }
}

/// What the endpoint needs that is not key material.
///
/// The keys are passed to [`RadioEndpoint::host`] and [`RadioEndpoint::phone`], because a [`Keypair`]
/// wipes its private half on drop and so cannot be copied into a configuration value that callers
/// clone.
#[derive(Clone, Debug)]
pub struct EndpointConfig {
    /// Which end this is. It must agree with the constructor that was used.
    pub role: Role,
    /// The peer's static public key: what the initiation is addressed to, and what a handshake
    /// request is verified against.
    pub peer_static: [u8; 32],
    /// The pre-shared key the Noise suite mixes in.
    pub psk: [u8; 32],
    /// The host's pairings. Empty on the phone.
    pub pairings: Vec<PairingEntry>,
    /// One full record's time on the air at the radio's own tuple, in milliseconds.
    pub airtime_ms: f64,
    /// The path hash width this mesh uses, in bytes (1..=3).
    pub path_width: u8,
    /// The spreading factor, which decides the demodulation floor a route is ranked against.
    pub sf: u8,
    /// The region scope this radio's floods carry, or `None` when they are unscoped.
    pub scope: Option<u16>,
    /// How far this mesh reaches, in hops, for sizing a flood before any path is known.
    ///
    /// A flood carries the path it *accumulates*, so its record ceiling shrinks with distance, and a
    /// sender that assumes the firmware's 64-byte worst case can never send a 123-byte initiation
    /// whole — it would fragment every one of them for a mesh that crosses three repeaters. This is
    /// the operator's own figure for the mesh they are on: `flood.max` is a per-site setting, and
    /// the field sessions of 2026-11-10 crossed up to 29 hops, so 32 is the default. At one-byte
    /// paths a whole initiation still fits at 38 hops; the figure only starts costing bytes where a
    /// site's paths are wider than the default allows for.
    pub flood_reach_hops: u8,
    /// Who the transaction layer addresses. A Noise session is one peer, so this is a label.
    pub peer: PeerId,
    /// How many passes over a message's unacknowledged parts before it is given up on.
    pub max_attempts: u8,
    /// The first retransmission deadline, in milliseconds.
    pub rto_ms: u64,
    /// How many rungs a bootstrap attempt is given.
    pub max_rungs: u8,
}

impl EndpointConfig {
    /// A configuration with the public preset's tuple and one peer.
    ///
    /// `airtime_ms` is the full-record figure at 62.5 kHz and SF7 (`602.6 ms`, measured), because
    /// that is the number the pacing and the burst tail are derived from; a caller on another tuple
    /// sets its own.
    #[must_use]
    pub fn new(role: Role, peer_static: [u8; 32], psk: [u8; 32]) -> Self {
        Self {
            role,
            peer_static,
            psk,
            pairings: Vec::new(),
            airtime_ms: 602.6,
            path_width: 1,
            sf: 7,
            scope: None,
            flood_reach_hops: 32,
            peer: PeerId(1),
            max_attempts: 5,
            rto_ms: 6_000,
            max_rungs: 3,
        }
    }

    /// Sets the radio's airtime for one full record.
    #[must_use]
    pub fn with_airtime_ms(mut self, airtime_ms: f64) -> Self {
        self.airtime_ms = airtime_ms;
        self
    }

    /// Sets the path hash width the mesh uses.
    #[must_use]
    pub fn with_path_width(mut self, path_width: u8) -> Self {
        self.path_width = path_width;
        self
    }

    /// Sets the spreading factor used to rank a route against the demodulation floor.
    #[must_use]
    pub fn with_sf(mut self, sf: u8) -> Self {
        self.sf = sf;
        self
    }

    /// Sets the region scope this radio's floods carry.
    #[must_use]
    pub fn with_scope(mut self, scope: Option<u16>) -> Self {
        self.scope = scope;
        self
    }

    /// Sets how far this mesh reaches, in hops, for sizing a flood before a path is known.
    #[must_use]
    pub fn with_flood_reach_hops(mut self, hops: u8) -> Self {
        self.flood_reach_hops = hops;
        self
    }

    /// Sets the host's pairings.
    #[must_use]
    pub fn with_pairings(mut self, pairings: Vec<PairingEntry>) -> Self {
        self.pairings = pairings;
        self
    }

    /// Sets how many rungs a bootstrap attempt is given.
    #[must_use]
    pub fn with_max_rungs(mut self, max_rungs: u8) -> Self {
        self.max_rungs = max_rungs;
        self
    }

    /// Sets how many passes a message gets before it is given up on.
    #[must_use]
    pub fn with_max_attempts(mut self, max_attempts: u8) -> Self {
        self.max_attempts = max_attempts;
        self
    }

    /// Sets the first retransmission deadline.
    #[must_use]
    pub fn with_rto_ms(mut self, rto_ms: u64) -> Self {
        self.rto_ms = rto_ms;
        self
    }
}

/// A carrier that can be told which carriage a record must take.
///
/// It is a separate trait from [`Carrier`] on purpose. A carrier that cannot be told the difference
/// between a direct packet and a flood cannot be driven by this endpoint at all — it would send
/// every record some way of its own choosing, which is the inversion this module exists to remove.
/// Making the omission a compile error is cheaper than discovering it on the air.
#[allow(async_fn_in_trait)]
pub trait CarriageSink: Carrier {
    /// Use this carriage for the next [`Carrier::send_record`].
    async fn set_carriage(&mut self, carriage: &Carriage) -> Result<(), MeshError>;
}

/// What a queued record is for, so the report and the cancellation rule can name it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Purpose {
    Initiation,
    Answer,
    /// A direct copy of an answer, travelling back along the initiation's own chain.
    AnswerDirect,
    Part {
        tx_id: u16,
        part_idx: u8,
        retransmit: bool,
    },
    Ack,
    NeedHandshake,
}

/// One record waiting for the air, and the earliest instant it may go.
struct Pending {
    record: Vec<u8>,
    carriage: Carriage,
    not_before_ms: u64,
    purpose: Purpose,
}

/// The radio endpoint: both ends of one link, in one state machine.
pub struct RadioEndpoint {
    config: EndpointConfig,
    /// The host's responder, holding the static key the phone addressed.
    responder: Option<Responder>,
    /// The phone's static keypair, whose ephemeral each attempt derives from.
    local: Option<Keypair>,
    /// The phone's attempt, held across every rung.
    attempt: Option<Attempt>,
    /// The host's answer cache: 64 initiations, the handshake replay window.
    cache: Option<AnswerCache>,
    /// Which rung the bootstrap is on, `1..=config.max_rungs`.
    rung: u8,
    /// When the current rung expires, or `None` when no attempt is running.
    rung_deadline_ms: Option<u64>,
    /// The next transmission nonce. Any change makes bytes new to the mesh's duplicate ring.
    nonce: u16,
    /// The last wall-clock time this end was told, so an attempt started from the poll loop (after a
    /// failed answer spent the previous one) is stamped with a real time rather than an invented one.
    last_wall_ms: u64,
    /// Whether the last attempt was spent by a failed answer and a fresh one is owed.
    ///
    /// **Replacement, never creation.** `connect` is the only thing that starts a first attempt:
    /// starting one from the poll loop made a client that had never connected transmit a whole
    /// initiation, which the ABI test caught as a frame too large for its buffer.
    attempt_spent: bool,
    /// The session, once one exists.
    session: Option<Established>,
    /// The replay window for the current session. Reset with the session.
    replay: ReplayWindow,
    /// The next transport counter. Reset with the session.
    counter: u32,
    /// The index the next accepted initiation is answered with.
    next_index: u8,
    /// Every path this end believes reaches the peer.
    route: RouteTable,
    /// The reliable message layer.
    tx: TransactionLayer,
    /// `KIND_NEED_HANDSHAKE` emission, rate-limited.
    need_hs: NeedHandshakeSender,
    /// Records waiting for the air, earliest first.
    queue: VecDeque<Pending>,
    /// Messages handed up, waiting for [`RadioEndpoint::take_inbound`].
    inbound: Vec<Vec<u8>>,
    /// The pairing an accepted initiation produced, waiting for the owner of the pairing file.
    ///
    /// The handshake computes it and the endpoint cannot persist it: the file, its permissions and
    /// its replay timestamp belong to the daemon. Surfacing it here is the whole of the seam, and
    /// without it a host would accept a phone and never pin it — the pairing would be re-negotiable
    /// by anyone holding the PSK.
    pending_pin: Option<([u8; 32], u64)>,
    /// The path our last direct record travelled out, which is the witness a zero-hop direct arrival
    /// speaks for.
    last_direct_path: Vec<u8>,
    /// When this end last sent a direct record with **no path at all**, which is the send that says
    /// the peer is adjacent.
    ///
    /// Kept apart from `last_direct_path` because that field is only filled by a path with hops in
    /// it: the empty send used to leave no trace, so a direct arrival could never prove adjacency and
    /// the only fresh candidates left were chains from floods that had taken the long way round.
    last_adjacent_send_ms: Option<u64>,
    /// How many records the carrier says may still be sent this hour.
    records_left: u8,
    /// The next `send_record` correlation number.
    seq: u8,
    stats: Stats,
}

impl RadioEndpoint {
    /// The host: it answers initiations and never starts one.
    ///
    /// # Panics
    ///
    /// Panics when `config.role` is not [`Side::Host`]. A role that disagrees with the constructor
    /// would make the direction filter and the state machine disagree, which is a bug in the caller
    /// and not a runtime condition.
    #[must_use]
    pub fn host(config: EndpointConfig, static_key: Keypair) -> Self {
        assert_eq!(config.role, Side::Host, "a host endpoint needs role Host");
        let mut endpoint = Self::bare(config);
        endpoint.responder = Some(Responder::new(static_key));
        endpoint.cache = Some(AnswerCache::plan_default());
        endpoint
    }

    /// The phone: it initiates, and keeps one attempt across every rung.
    ///
    /// # Panics
    ///
    /// Panics when `config.role` is not [`Side::Phone`]; see [`RadioEndpoint::host`].
    #[must_use]
    pub fn phone(config: EndpointConfig, local: Keypair) -> Self {
        assert_eq!(config.role, Side::Phone, "a phone endpoint needs role Phone");
        let mut endpoint = Self::bare(config);
        endpoint.local = Some(local);
        endpoint
    }

    fn bare(config: EndpointConfig) -> Self {
        let side = config.role;
        // The two limits the layer cuts parts with are the widest and the narrowest record the
        // protocol allows, so a part can always be halved onto the narrow carrier rather than
        // re-cut: `min(137, 2 x 59)` = 118 bytes at the public preset.
        let direct_record_limit = RecordBudget::direct(0).limit();
        let flood_record_limit = match config.scope {
            Some(_) => RecordBudget::flood_scoped(MAX_PATH_BYTES).limit(),
            None => RecordBudget::flood_unscoped(MAX_PATH_BYTES).limit(),
        };
        let tx = TransactionLayer::new(crate::transaction::Config {
            side,
            airtime_ms: config.airtime_ms,
            direct_record_limit,
            flood_record_limit,
            max_attempts: config.max_attempts,
            rto_ms: config.rto_ms,
        });
        Self {
            config,
            responder: None,
            local: None,
            attempt: None,
            cache: None,
            rung: 0,
            rung_deadline_ms: None,
            nonce: ENDPOINT_NONCE_BASE,
            last_wall_ms: 0,
            attempt_spent: false,
            session: None,
            replay: ReplayWindow::default(),
            counter: 0,
            next_index: 1,
            route: RouteTable::new(),
            tx,
            need_hs: NeedHandshakeSender::new(),
            queue: VecDeque::new(),
            inbound: Vec::new(),
            pending_pin: None,
            last_direct_path: Vec::new(),
            last_adjacent_send_ms: None,
            records_left: CREDIT_WINDOW,
            seq: 0,
            stats: Stats::default(),
        }
    }

    /// Begin a bootstrap attempt: one ephemeral, held until every rung has been spent.
    ///
    /// `now_ms` is the caller's monotonic clock and `wall_ms` is Unix time, which is sealed into the
    /// initiation and checked by the responder. Calling this again abandons the previous attempt,
    /// which is what a reconnect does.
    ///
    /// # Errors
    ///
    /// [`HandshakeError`] when this end has no keypair, or when the Noise state cannot be built.
    pub fn connect(&mut self, now_ms: u64, wall_ms: u64) -> Result<(), HandshakeError> {
        self.last_wall_ms = wall_ms;
        self.start_attempt(now_ms, wall_ms)?;
        Ok(())
    }

    fn start_attempt(&mut self, now_ms: u64, wall_ms: u64) -> Result<u16, HandshakeError> {
        let local = self.local.as_ref().ok_or(HandshakeError::Malformed)?;
        let attempt = Attempt::start(local, &self.config.peer_static, &self.config.psk, wall_ms)?;
        self.attempt = Some(attempt);
        self.rung = 1;
        self.rung_deadline_ms = Some(now_ms);
        self.session = None;
        self.replay = ReplayWindow::default();
        self.counter = 0;
        // Anything queued was built for a session that no longer exists; the route-teaching copy
        // especially, which is a record the far end would reject as belonging to a dead index.
        self.queue.clear();
        Ok(u16::from(self.rung))
    }

    /// Hand a whole message to the transaction layer.
    ///
    /// # Errors
    ///
    /// [`BeginError`] when the message is empty, needs more than [`crate::transaction::MAX_PARTS`]
    /// parts, or the transaction id space is exhausted.
    pub fn submit(&mut self, message: &[u8], now_ms: u64) -> Result<u16, BeginError> {
        self.tx.begin(self.config.peer, message, now_ms)
    }

    /// Messages handed up since the last call, oldest first.
    pub fn take_inbound(&mut self) -> Vec<Vec<u8>> {
        core::mem::take(&mut self.inbound)
    }

    /// The pairing to persist after an accepted initiation: the phone key to pin and the timestamp
    /// to remember against a replay. Taken once, by whoever owns the pairing file.
    pub fn take_pin(&mut self) -> Option<([u8; 32], u64)> {
        self.pending_pin.take()
    }

    /// Whether a session exists, so records can be sealed and opened.
    #[must_use]
    pub fn established(&self) -> bool {
        self.session.is_some()
    }

    /// Adopt a path learned from the peer's advert, so the next transmission can be direct.
    ///
    /// **The only writer of a learned candidate.** It goes through [`RouteTable::observe`], which
    /// stores it as `Proof::Candidate`: a path from an advert says where the peer was heard, never
    /// that a record crossed it, so it must not be treated as proof (`RC-3`).
    pub fn observe_path(&mut self, candidate: Candidate) {
        self.route.observe(candidate);
    }

    /// The paths this end believes it can reach the peer by.
    #[must_use]
    pub fn route(&self) -> &RouteTable {
        &self.route
    }

    /// The counters.
    #[must_use]
    pub fn stats(&self) -> &Stats {
        &self.stats
    }

    /// How many records are still waiting for the air.
    #[must_use]
    pub fn queued(&self) -> usize {
        self.queue.len()
    }

    /// Sets how many records the carrier says may still be sent this hour.
    ///
    /// The default is the credit window. A caller that reads
    /// [`ldmux_mesh::messages::Status::ops_left_this_hour`] sets it here, and the transaction layer
    /// never gates an acknowledgement on it — an ack is the traffic that keeps the link alive.
    pub fn set_records_left(&mut self, records_left: u8) {
        self.records_left = records_left;
    }

    /// When the caller should look again even if nothing arrives.
    ///
    /// The earliest of: a queued record's own instant, the running rung's expiry, and the transaction
    /// layer's own deadline. A caller with a real clock sleeps until this instead of polling on a
    /// tick, and a caller with a virtual clock steps straight to it.
    #[must_use]
    pub fn next_deadline_ms(&self) -> Option<u64> {
        let queued = self.queue.front().map(|pending| pending.not_before_ms);
        let rung = self.rung_deadline_ms.filter(|_| self.attempt_running());
        let tx = if self.session.is_some() {
            self.tx.next_deadline_ms()
        } else {
            None
        };
        [queued, rung, tx].into_iter().flatten().min()
    }

    fn attempt_running(&self) -> bool {
        self.attempt.as_ref().is_some_and(|attempt| !attempt.spent())
    }


    fn next_seq(&mut self) -> u8 {
        self.seq = self.seq.wrapping_add(1);
        self.seq
    }

    /// The record ceiling for one carriage, for **this** packet.
    ///
    /// A direct packet's own path comes off the ceiling and the receiver's filter comes off it too,
    /// so the smaller of the two is the answer. A flood grows a hash per hop in transit and may also
    /// carry a scope, so its figure is the path this end believes the flood will have crossed: the
    /// best candidate's length when one is held, and the firmware's longest legal path when none is,
    /// because a record that is guessed too large is transmitted, forwarded and never reported.
    fn budget(&self, carriage: &Carriage) -> RecordBudget {
        match carriage {
            Carriage::Direct { path } => RecordBudget::direct(path.len()),
            Carriage::Flood => {
                // What the flood will have accumulated when it arrives: a held path is a
                // measurement, and with none held this is the operator's reach. The firmware's
                // 64-byte worst case is the ceiling of that figure, never the starting point —
                // assuming it would fragment every initiation on a mesh three repeaters wide.
                let width = usize::from(self.config.path_width.clamp(1, 3));
                let estimate = usize::from(self.config.flood_reach_hops)
                    .saturating_mul(width)
                    .min(MAX_PATH_BYTES);
                let path_arrival = self
                    .route
                    .candidates()
                    .iter()
                    .map(|candidate| candidate.path().len())
                    .min()
                    .unwrap_or(estimate);
                match self.config.scope {
                    Some(_) => RecordBudget::flood_scoped(path_arrival),
                    None => RecordBudget::flood_unscoped(path_arrival),
                }
            }
        }
    }

    /// The next transmission nonce, wrapping inside this end's half of the space.
    fn next_nonce(&mut self) -> u16 {
        self.nonce = if self.nonce == u16::MAX {
            ENDPOINT_NONCE_BASE
        } else {
            self.nonce.saturating_add(1)
        };
        self.nonce
    }

    fn carriage_for(&mut self, attempt: u8, now_ms: u64) -> Carriage {
        self.route.prune(now_ms);
        self.route.carrier_for(attempt, now_ms, self.config.sf)
    }

    /// Decide what this end owes the air at `now_ms`, and hand it to the carrier.
    ///
    /// The order is deliberate: a bootstrap initiation first, because nothing else can be sent until
    /// a session exists; then a queued record whose instant has come; then new transaction work. A
    /// call with nothing due returns an empty report and does no work, which is what makes it safe to
    /// call on a timer as well as on a deadline.
    ///
    /// # Errors
    ///
    /// [`MeshError`] when the carrier refuses a record or closes.
    pub async fn poll<C: CarriageSink>(
        &mut self,
        now_ms: u64,
        carrier: &mut C,
    ) -> Result<Vec<Outbound>, MeshError> {
        let mut report = Vec::new();
        self.stage_bootstrap(now_ms, &mut report);
        self.stage_transactions(now_ms, &mut report);
        self.flush(now_ms, carrier, &mut report).await?;
        Ok(report)
    }

    /// Fill the queue with whatever the bootstrap owes at `now_ms`.
    fn stage_bootstrap(&mut self, now_ms: u64, report: &mut Vec<Outbound>) {
        if self.config.role != Side::Phone || self.session.is_some() {
            return;
        }
        // **A spent attempt is replaced, not waited out.** The only way an attempt disappears while
        // no session exists is a failed answer, which consumed `snow`'s handshake state; the ladder
        // then has to begin again or the phone is silent for the rest of its life.
        if self.attempt_spent {
            self.attempt_spent = false;
            if self.start_attempt(now_ms, self.last_wall_ms).is_err() {
                return;
            }
            // **And it waits one rung before spending air.** With the deadline set to now, a peer
            // that answers unreadably made this end transmit a fresh initiation for every bad
            // answer: 200 records in 200 answer steps, measured on the harness. A replacement is a
            // retry, not a reflex, so it takes the first rung's wait like any other.
            self.rung_deadline_ms =
                Some(now_ms.saturating_add(bootstrap::rung_ms(1, self.config.airtime_ms)));
        }
        if !self.attempt_running() {
            return;
        }
        let Some(deadline) = self.rung_deadline_ms else {
            return;
        };
        if now_ms < deadline {
            return;
        }
        // The rung that just expired is the one that was sent; the next one is `self.rung`, and when
        // that is past the last, the attempt is dead.
        if self.rung > self.config.max_rungs {
            self.stats.bootstrap_failures += 1;
            self.attempt = None;
            self.rung_deadline_ms = None;
            report.push(Outbound::BootstrapFailed {
                rungs: self.config.max_rungs,
            });
            return;
        }

        let carriage = self.carriage_for(self.rung, now_ms);
        let limit = self.budget(&carriage).usable_limit().unwrap_or(0);
        let hops = self
            .route
            .best(now_ms, self.config.sf)
            .map_or(1, Candidate::hops);

        let Some(attempt) = self.attempt.as_mut() else {
            return;
        };
        // A whole initiation needs `123` bytes. Fragmentation is the fallback, not the default: two
        // packets a fixed gap apart cross a repeater chain with independent per-hop forward delays,
        // so the second can arrive at a distant hop while the first is still being forwarded there
        // and be lost to a deaf receiver. One packet is one chance to lose it, not two; the budget
        // says whether the carrier can hold it, and the fragments exist for the path lengths where
        // it cannot (`docs/RADIO-PLAN.md` §5.1).
        if limit >= bootstrap::MSG1_NONCED_LEN {
            let record = attempt.direct();
            self.push(record, carriage, now_ms, Purpose::Initiation);
        } else {
            let [first, second] = attempt.parts();
            let gap = bootstrap::part_gap_ms(self.config.airtime_ms);
            self.push(first, carriage.clone(), now_ms, Purpose::Initiation);
            // A repeater forwarding the first part cannot hear the second, so the second waits out
            // the forward window of the first.
            self.push(
                second,
                carriage,
                now_ms.saturating_add(gap),
                Purpose::Initiation,
            );
        }
        self.stats.initiations_sent += 1;
        self.stats.rungs_used += 1;

        let rung_len = if self.rung >= self.config.max_rungs {
            // The last rung is the answer window: the firmware's own documented figure for a
            // multi-hop reply, rather than a smaller number a bench test happened to beat.
            bootstrap::answer_window_ms()
        } else {
            bootstrap::rung_ms(hops, self.config.airtime_ms)
        };
        self.rung_deadline_ms = Some(now_ms.saturating_add(rung_len));
        self.rung = self.rung.saturating_add(1);
    }

    /// Fill the queue with the transaction layer's work at `now_ms`.
    fn stage_transactions(&mut self, now_ms: u64, report: &mut Vec<Outbound>) {
        if self.session.is_none() {
            return;
        }
        // A pass that has already been sent once is a retransmission, and a retransmission floods:
        // a direct attempt that produced nothing is evidence about the path, not about the mesh.
        let carriage = if self.tx.in_retransmit_pass(self.config.peer) {
            Carriage::Flood
        } else {
            self.carriage_for(FIRST_ATTEMPT, now_ms)
        };
        let budget = self.budget(&carriage);
        let actions = self
            .tx
            .poll_all(now_ms, budget, self.records_left);
        let pace = self.tx.pace_ms();
        for (ord, action) in actions.into_iter().enumerate() {
            let at = now_ms.saturating_add(pace.saturating_mul(ord as u64));
            match action {
                Action::Send {
                    bytes,
                    tx_id,
                    part_idx,
                    retransmit,
                    ..
                } => {
                    // A retransmission floods even when the pass began on a direct path, so the
                    // carriage of a single part is that part's own.
                    let carriage = if retransmit {
                        Carriage::Flood
                    } else {
                        carriage.clone()
                    };
                    let purpose = Purpose::Part {
                        tx_id,
                        part_idx,
                        retransmit,
                    };
                    self.seal(bytes, purpose, carriage, at);
                }
                Action::StandaloneAck { bytes, .. } => {
                    self.seal(bytes, Purpose::Ack, carriage.clone(), at);
                }
                Action::Complete { tx_id, .. } => {
                    self.stats.transactions_completed += 1;
                    report.push(Outbound::Completed { tx_id });
                }
                Action::TimedOut { tx_id, .. } => {
                    self.stats.transactions_timed_out += 1;
                    report.push(Outbound::TimedOut { tx_id });
                }
                Action::Superseded { tx_id, .. } => {
                    report.push(Outbound::Superseded { tx_id });
                }
            }
        }
    }

    /// Seal one application payload into a record with the carriage's own ceiling, and queue it.
    fn seal(&mut self, app: Vec<u8>, purpose: Purpose, carriage: Carriage, at_ms: u64) -> bool {
        let Some(limit) = self.budget(&carriage).usable_limit() else {
            return false;
        };
        let Some(session) = self.session.as_ref() else {
            return false;
        };
        let counter = self.counter;
        // Each end stamps its own kind, which is what makes a half-duplex echo of a downlink record
        // distinguishable from an uplink one at zero hops.
        let kind = self.tx.record_kind();
        let sealed = record::seal_as(
            &session.transport,
            kind,
            session.index,
            counter,
            0,
            0,
            &app,
            limit,
        );
        match sealed {
            Ok(record) => {
                self.counter = self.counter.saturating_add(1);
                self.push(record, carriage, at_ms, purpose);
                true
            }
            Err(_) => {
                self.stats.rejected_record += 1;
                false
            }
        }
    }

    /// Queue one already-built record.
    fn push(&mut self, record: Vec<u8>, carriage: Carriage, at_ms: u64, purpose: Purpose) {
        if let Carriage::Direct { path } = &carriage {
            if path.is_empty() {
                self.last_adjacent_send_ms = Some(at_ms);
            } else {
                self.last_direct_path = path.clone();
            }
        }
        self.queue.push_back(Pending {
            record,
            carriage,
            not_before_ms: at_ms,
            purpose,
        });
    }

    /// Hand every queued record whose instant has come to the carrier.
    async fn flush<C: CarriageSink>(
        &mut self,
        now_ms: u64,
        carrier: &mut C,
        report: &mut Vec<Outbound>,
    ) -> Result<(), MeshError> {
        loop {
            let Some(front) = self.queue.front() else {
                return Ok(());
            };
            if front.not_before_ms > now_ms {
                return Ok(());
            }
            let pending = self
                .queue
                .pop_front()
                .expect("the front was just read");
            carrier.set_carriage(&pending.carriage).await?;
            let seq = self.next_seq();
            carrier.send_record(seq, &pending.record).await?;
            let what = match pending.purpose {
                Purpose::Initiation => SentKind::Initiation,
                Purpose::Answer => SentKind::Answer,
                Purpose::AnswerDirect => {
                    self.stats.answers_direct += 1;
                    SentKind::Answer
                }
                Purpose::Part {
                    tx_id,
                    part_idx,
                    retransmit,
                } => SentKind::Part {
                    tx_id,
                    part_idx,
                    retransmit,
                },
                Purpose::Ack => SentKind::Ack,
                Purpose::NeedHandshake => {
                    self.stats.need_handshake_sent += 1;
                    SentKind::NeedHandshake
                }
            };
            if matches!(pending.purpose, Purpose::Answer) {
                self.stats.answers_sent += 1;
            }
            self.stats.records_sent += 1;
            self.stats.note_sent(&pending.carriage);
            report.push(Outbound::Sent {
                what,
                carriage: pending.carriage,
                bytes: pending.record.len(),
                at_ms: now_ms,
            });
        }
    }

    /// Take one arriving record, and decide what it is.
    ///
    /// The order of the checks is the order that costs least, and each one exists because its
    /// absence was a defect in the field: the direction filter and the transit rule run before
    /// anything is parsed (`RC-3`), the record layer's tag runs before any routing state is touched,
    /// and a duplicate or replayed record never moves the replay window.
    pub fn on_record(
        &mut self,
        record: &[u8],
        info: &ArrivalInfo,
        now_ms: u64,
        wall_ms: u64,
    ) -> Admitted {
        self.last_wall_ms = wall_ms;
        self.stats.records_received += 1;
        let Some(kind) = record.first().copied() else {
            return Admitted::Ignored(Rejected::UnknownKind);
        };
        match direction::admit(self.config.role, kind, info.arrival) {
            Verdict::SelfEcho => {
                self.stats.rejected_self_echo += 1;
                return Admitted::Ignored(Rejected::SelfEcho);
            }
            Verdict::InTransit => {
                self.stats.rejected_in_transit += 1;
                return Admitted::Ignored(Rejected::InTransit);
            }
            Verdict::Admit => {}
        }
        match kind {
            KIND_MSG1 | KIND_MSG1_FRAG => self.on_initiation(record, info, now_ms, wall_ms),
            KIND_MSG2 => self.on_answer(record, info, now_ms),
            KIND_NEED_HANDSHAKE => self.on_need_handshake(record, info, now_ms, wall_ms),
            KIND_UP | KIND_DOWN => self.on_transport(record, info, now_ms, wall_ms),
            _ => {
                self.stats.rejected_unknown_kind += 1;
                Admitted::Ignored(Rejected::UnknownKind)
            }
        }
    }

    fn on_initiation(
        &mut self,
        record: &[u8],
        info: &ArrivalInfo,
        now_ms: u64,
        wall_ms: u64,
    ) -> Admitted {
        let (Some(cache), Some(responder)) = (self.cache.as_mut(), self.responder.as_ref()) else {
            return Admitted::Ignored(Rejected::NotForUs);
        };
        let index = self.next_index;
        let answered = cache.answer(
            responder,
            &self.config.pairings,
            index,
            now_ms,
            wall_ms,
            record,
        );
        match answered {
            Ok(Answered::Fresh {
                reply,
                established,
                remote_static,
                timestamp_ms,
                ..
            }) => {
                self.pending_pin = Some((remote_static, timestamp_ms));
                self.session = Some(*established);
                self.replay = ReplayWindow::default();
                self.counter = 0;
                self.next_index = index.wrapping_add(1).max(1);
                self.stats.records_admitted += 1;
                self.stats.sessions_established += 1;
                // The chain this initiation crossed, reversed, is the way back. It is a candidate,
                // not proof: proof is our own record coming back with every hop consumed.
                self.learn(&info.arrival, &info.path, info.snr_db, now_ms);
                self.queue_answer(&reply, info, now_ms);
                Admitted::Answered { cached: false }
            }
            Ok(Answered::Cached { reply }) => {
                self.stats.records_admitted += 1;
                self.stats.answers_from_cache += 1;
                self.queue_answer(&reply, info, now_ms);
                Admitted::Answered { cached: true }
            }
            Ok(Answered::Held) => Admitted::Ignored(Rejected::NotForUs),
            Err(refusal) => {
                self.stats.rejected_handshake += 1;
                Admitted::Ignored(Rejected::Refusal(refusal))
            }
        }
    }

    /// Queue the answer, and — when the initiation crossed repeaters — one flooded copy to teach
    /// the far end the way back.
    ///
    /// The direct answer is the fast path on a symmetric link. The flood copy exists for the
    /// asymmetric one, where the initiation arrived over a chain the answer cannot cross: it is the
    /// only way a phone that has never heard a flood from us learns a path at all, because the
    /// answer it receives carries no path of its own.
    fn queue_answer(&mut self, reply: &[u8], info: &ArrivalInfo, now_ms: u64) {
        let carriage = self.carriage_for(FIRST_ATTEMPT, now_ms);
        self.push(reply.to_vec(), carriage, now_ms, Purpose::Answer);
        if !info.arrival.is_flood() {
            return;
        }
        // **A second copy, direct, back along the chain the initiation crossed.** A flooded answer
        // reaches the far end only through the radio's receive log — a debug tap over every packet
        // the radio demodulates, which is a feature we do not own and which a firmware may have
        // switched off. A direct packet is the carrier the firmware hands to the application
        // (`src/Mesh.cpp:292-298`), and the reversed arrival chain is a road that demonstrably
        // carries, because the initiation just travelled it the other way. Measured 2026-11-12 with
        // two radios a metre apart: the flood answer appeared in the phone's log as `0x88 ldmr=true`
        // and the phone's screen never left "connecting".
        let width = usize::from(self.config.path_width.clamp(1, 3));
        let content_len = reply.len().min(bootstrap::MSG2_CONTENT_LEN);
        let mut copy = reply
            .get(..content_len)
            .map(<[u8]>::to_vec)
            .unwrap_or_default();
        let nonce = self.next_nonce();
        copy.extend_from_slice(&nonce.to_le_bytes());
        self.push(
            copy,
            Carriage::Direct {
                path: advice_path(info, width),
            },
            now_ms.saturating_add(ANSWER_DIRECT_DELAY_MS),
            Purpose::AnswerDirect,
        );
    }

    fn on_answer(&mut self, record: &[u8], info: &ArrivalInfo, now_ms: u64) -> Admitted {
        let Some(attempt) = self.attempt.as_mut() else {
            return Admitted::Ignored(Rejected::NotForUs);
        };
        match attempt.answer(record) {
            Ok(Answer::Finished(established)) => {
                self.apply_session(established, info, now_ms);
                Admitted::AnswerAccepted { repeated: false }
            }
            Ok(Answer::Repeated) => {
                // A repeat is the responder re-sending the answer it already gave. Its content is
                // the one this attempt finished on, so it is authenticated evidence of the peer —
                // and, when it arrived as a flood, the chain it crossed is the only route this end
                // can learn.
                self.learn(&info.arrival, &info.path, info.snr_db, now_ms);
                Admitted::AnswerAccepted { repeated: true }
            }
            Ok(Answer::Ignored) => Admitted::Ignored(Rejected::NotForUs),
            Err(e) => {
                self.stats.rejected_handshake += 1;
                // **A failed answer spends the attempt, so it must be replaced.** `snow` has
                // consumed the handshake state by the time a crypto failure returns, which is what
                // `Attempt::answer` says in its own doc — and this arm used to leave the attempt in
                // place anyway. Its `inner` was already taken, so from then on *every* answer,
                // including the correct one a moment later, came back `Malformed`, and the phone sat
                // on "connecting" for the whole ladder: measured 2026-11-12 with the host reporting
                // `session established`, the phone logging eleven `op=0x84 kind=0x12` frames, and
                // the screen never moving. Dropping it here lets the next rung start a fresh one,
                // which is the cure the doc prescribes.
                self.attempt = None;
                self.attempt_spent = true;
                self.rung_deadline_ms = None;
                Admitted::Ignored(Rejected::Handshake(e))
            }
        }
    }

    fn apply_session(
        &mut self,
        established: Established,
        info: &ArrivalInfo,
        now_ms: u64,
    ) {
        self.session = Some(established);
        self.replay = ReplayWindow::default();
        self.counter = 0;
        self.rung_deadline_ms = None;
        self.stats.records_admitted += 1;
        self.stats.sessions_established += 1;
        self.learn(&info.arrival, &info.path, info.snr_db, now_ms);
    }

    fn on_need_handshake(
        &mut self,
        record: &[u8],
        info: &ArrivalInfo,
        now_ms: u64,
        wall_ms: u64,
    ) -> Admitted {
        let verified = bootstrap::need_handshake_verify(
            &self.config.psk,
            &self.config.peer_static,
            wall_ms,
            TIMESTAMP_WINDOW_MS,
            record,
        );
        if verified.is_err() {
            self.stats.rejected_handshake += 1;
            return Admitted::Ignored(Rejected::BadHandshakeRequest);
        }
        self.stats.need_handshake_heard += 1;
        self.learn(&info.arrival, &info.path, info.snr_db, now_ms);
        // **Only the phone initiates.** A host that starts an attempt builds a Noise initiation it
        // will never send — `stage_bootstrap` runs for the phone alone — so the attempt only sits
        // there pretending to be a session in progress. The host's own recovery is to stop trusting
        // the session it holds and wait for the peer's new one, so that is all it does.
        if self.config.role == Side::Phone {
            if self.start_attempt(now_ms, wall_ms).is_err() {
                return Admitted::Ignored(Rejected::BadHandshakeRequest);
            }
        } else {
            self.session = None;
        }
        Admitted::HandshakeRequested
    }

    fn on_transport(
        &mut self,
        record: &[u8],
        info: &ArrivalInfo,
        now_ms: u64,
        wall_ms: u64,
    ) -> Admitted {
        let opened = match self.session.as_ref() {
            Some(session) => record::open(&session.transport, session.index, &mut self.replay, record),
            None => {
                // The host answers a record it cannot read with a request to handshake again, so a
                // phone whose daemon restarted re-handshakes at once instead of spending its own
                // silence ladder. The phone says nothing: its bootstrap is already running.
                self.request_handshake(now_ms, wall_ms);
                return Admitted::Ignored(Rejected::NoSession);
            }
        };
        let opened = match opened {
            Ok(opened) => opened,
            Err(e) => {
                self.stats.rejected_record += 1;
                // **A record this end cannot open means the session is not useful, so it is dropped
                // and the other end is asked to handshake again.** Keeping it produced the same
                // refusal for every later record with nothing to end it: measured on the phone
                // 2026-11-13, `admitted=4` beside `bad=3` while the screen waited for a list the host
                // was already answering. The host asks in the same situation; this is the phone's
                // half of the same recovery, and without it only the host can break the deadlock.
                self.session = None;
                // The attempt behind that session is spent, so it has to be replaced by the ladder
                // for a new session to exist at all: without this the phone dropped the session and
                // then sat silent, which is the state the field log shows as a live `sessions` count
                // and nothing else.
                self.attempt_spent = true;
                self.request_handshake(now_ms, wall_ms);
                return Admitted::Ignored(Rejected::Record(e));
            }
        };
        self.stats.records_admitted += 1;
        // Only now, with the tag verified, may a route be called proven. A direct arrival that
        // finished its path is the one shape that proves anything.
        self.prove(info, now_ms);
        // A flood from the peer is authenticated and carries the chain it crossed, which is the same
        // evidence a bootstrap flood gives: a **candidate**, never a proof. Learning it here is what
        // keeps the candidate fresh while the peer is talking. Without it a learned path expires
        // after `ROUTE_FRESH_MS` no matter how recently the peer flooded, so the next message is sent
        // flooded even though the road was in use a second earlier, and the direct carrier — the only
        // cheap one — is never tried again.
        self.learn(&info.arrival, &info.path, info.snr_db, now_ms);
        // Anything the peer authenticated cancels the route-teaching copy: it has been heard, so the
        // flood it would have spent is no longer needed.
        self.cancel_route_teaching();
        let peer = self.config.peer;
        match self.tx.on_app(peer, &opened.app, now_ms) {
            Received::Message { tx_id, kind, payload, .. } => {
                self.stats.messages_delivered += 1;
                self.inbound.push(payload);
                Admitted::Message {
                    tx_id,
                    kind: kind.0,
                }
            }
            Received::Part { tx_id, part_idx, .. } => Admitted::Part { tx_id, part_idx },
            Received::Ack { .. } => Admitted::Ack,
            Received::Ignored(_) => Admitted::Ignored(Rejected::NotForUs),
        }
    }

    /// Emit `KIND_NEED_HANDSHAKE` when the host has no session, at most once per rate window.
    fn request_handshake(&mut self, now_ms: u64, wall_ms: u64) {
        if self.config.role != Side::Host {
            return;
        }
        let Some(host_public) = self.responder.as_ref().map(|r| r.static_key.public) else {
            return;
        };
        let Some(record) = self.need_hs.emit(&self.config.psk, &host_public, now_ms, wall_ms) else {
            return;
        };
        let carriage = self.carriage_for(FIRST_ATTEMPT, now_ms);
        self.push(
            record.to_vec(),
            carriage,
            now_ms,
            Purpose::NeedHandshake,
        );
    }

    /// Forget that a handshake was asked for, once the peer has shown it heard one.
    fn cancel_route_teaching(&mut self) {}

    /// Learn a path from a flood's travel chain, reversed.
    ///
    /// Only a flood teaches a path this way, and only as a **candidate**: the chain says the packet
    /// crossed those repeaters in that order, and the way back is an inference about the other
    /// direction. Adopting that inference as proof is what let a sixteen-hop peer be treated as a
    /// neighbour, so [`ProofWitness`] is the only thing that may mark a path proven.
    fn learn(&mut self, arrival: &Arrival, path: &[u8], snr_db: f32, now_ms: u64) {
        if !matches!(arrival, Arrival::Flood { .. }) {
            return;
        }
        let width = usize::from(self.config.path_width.clamp(1, 3));
        let reversed = reverse_path(path, width);
        // **An empty chain is a route, not a missing one.** A flood's path grows one hash per
        // *repeater*, so an initiation heard with no hash at all crossed zero hops: the peer is
        // adjacent, and a direct packet needs no path to reach it. Discarding it left the host
        // flood-answering a phone a metre away.
        self.route.observe(Candidate::learned(
            reversed,
            self.config.path_width,
            snr_db,
            now_ms,
        ));
        self.stats.routes_learned += 1;
    }

    /// Mark this end's last direct path proven, when a direct arrival finished its path.
    fn prove(&mut self, info: &ArrivalInfo, now_ms: u64) {
        let Arrival::Direct { hops } = info.arrival else {
            return;
        };
        let path = self.last_direct_path.clone();
        let Some(witness) = ProofWitness::authenticated_direct(&path, hops) else {
            return;
        };
        // **A direct arrival that finished its path proves the road it was sent by.** When that road
        // was empty the peer is adjacent, which is the strongest fact a route table can hold and the
        // one that must outrank any long chain a looping flood taught. It used to be ignored twice
        // over: `last_direct_path` was only filled for a non-empty path, and an empty chain returned
        // early here — measured 2026-11-13, the host held `route 20 repeaters in the path` and
        // `capacity 152 B` for a phone a metre away, and every direct send followed that loop and
        // never landed. `adopt_proven` only upgrades a candidate already held, so the chain has to be
        // stated before it can be proven.
        let adjacent = path.is_empty()
            && self
                .last_adjacent_send_ms
                .is_some_and(|sent| now_ms.saturating_sub(sent) <= PROVE_WINDOW_MS);
        if path.is_empty() && !adjacent {
            // A two-hop arrival is not evidence about a road this end did not just use.
            return;
        }
        // **Observed every time, not only the first.** `observe` refreshes the hearing time, and a
        // proven route that is never refreshed expires after two minutes and hands the send back to
        // whichever long chain a looping flood taught most recently.
        self.route.observe(Candidate::learned(
            path.clone(),
            self.config.path_width,
            info.snr_db,
            now_ms,
        ));
        let already = self
            .route
            .candidates()
            .iter()
            .any(|candidate| candidate.path() == path.as_slice() && candidate.is_proven());
        self.route.adopt_proven(&witness);
        let now = self
            .route
            .candidates()
            .iter()
            .any(|candidate| candidate.path() == path.as_slice() && candidate.is_proven());
        if now && !already {
            self.stats.routes_proven += 1;
        }
    }
}

/// A path as the way back: the same hashes, in the opposite order.
///
/// A path is a sequence of hashes, not a sequence of bytes, so the reversal is by hash. Reading it
/// backwards a byte at a time would build a path no repeater recognises, which is worse than no path
/// at all: the record would be transmitted along a route that does not exist.
/// The path an answer must carry, given how the initiation arrived.
///
/// A flood's chain is the road towards us, so the way back is its reverse; a direct packet that
/// reached us has consumed its path, so there is nothing to carry and the peer is adjacent.
fn advice_path(info: &ArrivalInfo, width: usize) -> Vec<u8> {
    if info.arrival.is_flood() {
        reverse_path(&info.path, width)
    } else {
        Vec::new()
    }
}

fn reverse_path(path: &[u8], width: usize) -> Vec<u8> {
    let width = width.max(1);
    if width == 1 {
        let mut out = path.to_vec();
        out.reverse();
        return out;
    }
    let mut out = Vec::with_capacity(path.len());
    for chunk in path.chunks(width).rev() {
        out.extend_from_slice(chunk);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_one_byte_path_reverses_hash_by_hash() {
        assert_eq!(reverse_path(&[1, 2, 3], 1), vec![3, 2, 1]);
    }

    /// A two-byte path is a sequence of hashes, not of bytes: reversing the bytes would name
    /// repeaters that do not exist.
    #[test]
    fn a_wider_path_reverses_whole_hashes() {
        assert_eq!(
            reverse_path(&[0xA1, 0xA2, 0xB1, 0xB2], 2),
            vec![0xB1, 0xB2, 0xA1, 0xA2]
        );
    }

    #[test]
    fn an_empty_path_reverses_to_nothing() {
        assert!(reverse_path(&[], 1).is_empty());
        assert!(reverse_path(&[], 3).is_empty());
    }

    /// A trail of one hash and a half must not be turned into a path: a half hash is not a repeater.
    #[test]
    fn a_ragged_path_keeps_its_last_partial_hash() {
        assert_eq!(reverse_path(&[0xA1, 0xA2, 0xB1], 2), vec![0xB1, 0xA1, 0xA2]);
    }
}
