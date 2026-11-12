//! The 1-round-trip bootstrap: one initiation, one answer, and a responder that answers a repeated
//! initiation without rebuilding anything.
//!
//! [`handshake`](crate::handshake) is the Noise exchange. This module is what a *link* does with it:
//! it holds one attempt across every retry rung, fragments the initiation when the carrier cannot
//! hold it whole, and remembers the answer it gave so that a repeated initiation costs a repeat of
//! that answer and nothing more.
//!
//! # Why the attempt owns the ephemeral
//!
//! `RC-5`: the phone abandoned its handshake when a rung timed out and started the next rung with a
//! fresh ephemeral. At range a crossing takes seconds, so the first rung's answer routinely arrived
//! *after* the second rung's initiation had gone out, and that answer no longer decrypted —
//! `HandshakeError::Crypto` on a link that was working. [`Attempt`] therefore builds the initiation
//! once and keeps it: every retry re-sends the **same** Noise content, so whichever answer lands,
//! it lands on the right ephemeral.
//!
//! # Why every send carries a nonce
//!
//! `SimpleMeshTables::wasSeen` (`src/Mesh.cpp:97`, hash from `src/Packet.cpp:22-45`) hashes
//! `payload_type || payload` into a 160-entry FIFO with no timeout, at every repeater and at the
//! receiver. A byte-identical retry is therefore discarded as a duplicate *before* anything looks at
//! it, which makes a retry worthless exactly when it is needed. So each transmission appends
//! `tx_nonce: u16`, which changes per send and is not part of the Noise content, so the responder's
//! cache still matches.
//!
//! A flood is additionally wrapped in `LDMR` with its own fresh nonce per transmission, so the
//! fragment forms are protected twice; the nonce on the fragment is what protects the *direct*
//! fragment case, which has no wrapper of its own.
//!
//! # Layouts
//!
//! ```text
//! KIND_MSG1 (0x11)         [kind][filter tag 16][noise 104]                      121 B
//! KIND_MSG1 + nonce        [kind][filter tag 16][noise 104][tx_nonce u16]        123 B
//! KIND_MSG1_FRAG (0x14)    [kind][init_id u16][part_byte][slice 61/60][nonce u16] 67/66 B
//! KIND_MSG2 (0x12)         [kind][noise 49]                                       50 B
//! KIND_MSG2 + nonce        [kind][noise 49][tx_nonce u16]                         52 B
//! ```
//!
//! The session index is **inside** message 2's sealed payload (`[index]` under the tag), so the
//! clear part of an answer is the kind plus the 49-byte Noise message; it is not a separate clear
//! field. `docs/RADIO-PLAN.md` §5.1 draws it as a clear byte and still totals 52 with the nonce; the
//! total is right and the field breakdown is not, and the code here is what the firmware sees.

use hmac::{Hmac, Mac};
use sha2::Sha256;

use crate::direction::{KIND_MSG1, KIND_MSG1_FRAG, KIND_MSG2};
use crate::handshake::{self, Established, HandshakeError, Initiator, PairingEntry, Responder};

/// Bytes appended to every transmission so a retry is never byte-identical to the copy before it.
pub const NONCE_LEN: usize = 2;

/// The initiation's Noise content: `[kind][filter tag 16][noise 104]`.
pub const MSG1_CONTENT_LEN: usize = handshake::MSG1_LEN;
/// The initiation as sent on a direct path: the content and its nonce.
pub const MSG1_NONCED_LEN: usize = MSG1_CONTENT_LEN + NONCE_LEN;
/// The answer's Noise content: `[kind][noise 49]`.
pub const MSG2_CONTENT_LEN: usize = handshake::MSG2_LEN;
/// The answer as sent: the content and its nonce.
pub const MSG2_NONCED_LEN: usize = MSG2_CONTENT_LEN + NONCE_LEN;

/// How many parts an initiation is split into when it does not fit whole.
pub const FRAG_PARTS: usize = 2;
/// `[kind][init_id u16][part_byte]` in front of every part's slice.
pub const FRAG_HEADER: usize = 4;
/// The first part's slice, which is one byte longer because 121 is odd.
pub const FRAG_SLICE0: usize = 61;
/// The second part's slice.
pub const FRAG_SLICE1: usize = MSG1_CONTENT_LEN - FRAG_SLICE0;
/// The first part as sent.
pub const FRAG_LEN0: usize = FRAG_HEADER + FRAG_SLICE0 + NONCE_LEN;
/// The second part as sent.
pub const FRAG_LEN1: usize = FRAG_HEADER + FRAG_SLICE1 + NONCE_LEN;

// The layouts above are arithmetic, so they are checked at compile time rather than in a test that
// can be skipped. A change to `handshake::MSG1_LEN` that this module did not follow would otherwise
// show up as a truncated initiation on the air.
const _: () = {
    assert!(MSG1_CONTENT_LEN == 121);
    assert!(MSG1_NONCED_LEN == 123);
    assert!(MSG2_CONTENT_LEN == 50);
    assert!(MSG2_NONCED_LEN == 52);
    assert!(FRAG_SLICE0 + FRAG_SLICE1 == MSG1_CONTENT_LEN);
    assert!(FRAG_LEN0 == 67);
    assert!(FRAG_LEN1 == 66);
};

/// The smallest answer window the ladder allows, so a neighbour does not need a retry to succeed.
pub const FIRST_RUNG_MIN_MS: u64 = 12_000;
/// How long the last rung waits for an answer before the attempt is called dead.
///
/// The firmware's own figure for a multi-hop reply ("observed multi-hop replies can take over a
/// minute", `Mesh.cpp::calcDirectTimeoutMillisFor` and `openhop_core`'s `timing.py`): the plan
/// takes the documented ceiling rather than a smaller number that a field test happened to beat.
pub const ANSWER_WINDOW_MS: u64 = 120_000;
/// One answer per peer per this window; see [`AnswerCache`].
pub const ANSWER_RATE_MS: u64 = 5_000;

/// The mean forward delay a repeater adds, as a multiple of one packet's airtime.
///
/// `getRetransmitDelay` (`examples/simple_repeater/MyMesh.cpp:597-605`) draws
/// `nextInt(0, 5 * airtime * tx_delay_factor + 1)` with `tx_delay_factor = 0.5` for a flood and
/// `direct_tx_delay_factor = 0.3` for a direct packet, i.e. a mean of 1.25 and 0.75 airtimes. The
/// larger figure is used here because a bootstrap is retried on both carriers and a rung that is
/// too short wastes the whole rung.
const FORWARD_DELAY_FACTOR: f64 = 1.25;

/// How long the last rung waits for an answer.
#[must_use]
pub const fn answer_window_ms() -> u64 {
    ANSWER_WINDOW_MS
}

/// How long one rung waits before the next retry, from the route's own round trip.
///
/// Derived, not chosen: a rung has to cover the packet's flight to the peer
/// (`hops × airtime`), one forward delay per hop, and the same back for the answer, so
/// `rung = 2 × hops × airtime × (1 + FORWARD_DELAY_FACTOR)`. The floor of
/// [`FIRST_RUNG_MIN_MS`] is what a neighbour gets, because at one hop the arithmetic lands under
/// it and a rung shorter than that spends air on duplicates rather than on a second attempt.
#[must_use]
pub fn rung_ms(hops: u8, airtime_ms: f64) -> u64 {
    let hops = f64::from(hops.max(1));
    let round_trip = 2.0 * hops * airtime_ms * (1.0 + FORWARD_DELAY_FACTOR);
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let ms = round_trip.ceil().max(0.0) as u64;
    ms.max(FIRST_RUNG_MIN_MS)
}

/// How long the second part waits before it is sent after the first.
///
/// A repeater cannot hear while it forwards, so a second packet sent into the same window is lost at
/// the first hop. The gap is the forward window of the part that just went out: `2.5 × airtime` is
/// the ceiling of the draw in `getRetransmitDelay`. `airtime_ms` is that part's own airtime, so the
/// gap follows the fragment size rather than a full record's.
#[must_use]
pub fn part_gap_ms(airtime_ms: f64) -> u64 {
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let ms = (2.5 * airtime_ms).ceil() as u64;
    ms
}

/// What a received answer did to an [`Attempt`].
pub enum Answer {
    /// This attempt is complete.
    Finished(Established),
    /// The same answer arrived again — the responder retransmitted its cached reply. Harmless: the
    /// session established by the first copy is the one in use, and nothing is rebuilt.
    Repeated,
    /// A *different* answer arrived after this attempt had already finished. It belongs to no
    /// attempt of ours; the established session stands and the record is ignored.
    Ignored,
}

/// One handshake attempt, from the phone's side, held for the whole ladder.
///
/// Built once and kept: [`Attempt::direct`] and [`Attempt::parts`] may be called any number of times
/// and always carry the same Noise content with a fresh nonce, and [`Attempt::answer`] finishes it.
pub struct Attempt {
    inner: Option<Initiator>,
    content: [u8; MSG1_CONTENT_LEN],
    /// Bumped by every send. Not random on purpose: a retry sequence is reproducible, which is what
    /// lets a test assert the retry rule rather than sample it.
    nonce: u16,
    answered_with: Option<Vec<u8>>,
}

impl Attempt {
    /// Begin an attempt. `timestamp_ms` is wall-clock Unix milliseconds and is part of the sealed
    /// payload the responder checks.
    pub fn start(
        local: &handshake::Keypair,
        remote_public: &[u8; 32],
        psk: &[u8; 32],
        timestamp_ms: u64,
    ) -> Result<Self, HandshakeError> {
        let (inner, msg1) = Initiator::start(local, remote_public, psk, timestamp_ms)?;
        let content: [u8; MSG1_CONTENT_LEN] =
            msg1.as_slice().try_into().map_err(|_| HandshakeError::Malformed)?;
        Ok(Self {
            inner: Some(inner),
            content,
            nonce: 0,
            answered_with: None,
        })
    }

    /// The 121 bytes every form of this initiation carries, and what the responder matches on.
    #[must_use]
    pub fn content(&self) -> &[u8; MSG1_CONTENT_LEN] {
        &self.content
    }

    /// The identifier the fragments are joined by: the first two bytes of the filter tag.
    #[must_use]
    pub fn init_id(&self) -> u16 {
        u16::from_le_bytes([self.content[1], self.content[2]])
    }

    /// Whether an answer has already finished this attempt.
    #[must_use]
    pub fn spent(&self) -> bool {
        self.answered_with.is_some()
    }

    fn next_nonce(&mut self) -> u16 {
        self.nonce = self.nonce.wrapping_add(1);
        self.nonce
    }

    /// The whole initiation with a fresh nonce, for a path that can carry [`MSG1_NONCED_LEN`].
    pub fn direct(&mut self) -> Vec<u8> {
        let nonce = self.next_nonce();
        let mut out = Vec::with_capacity(MSG1_NONCED_LEN);
        out.extend_from_slice(&self.content);
        out.extend_from_slice(&nonce.to_le_bytes());
        out
    }

    /// The initiation in two parts, with a fresh nonce on each, for a carrier that cannot hold it.
    pub fn parts(&mut self) -> [Vec<u8>; FRAG_PARTS] {
        let init_id = self.init_id();
        // Copied out so the loop below can bump the nonce, which borrows `self` mutably.
        let content = self.content;
        let slices = [&content[..FRAG_SLICE0], &content[FRAG_SLICE0..]];
        let mut out = [Vec::new(), Vec::new()];
        for (idx, (part, slice)) in out.iter_mut().zip(slices.iter()).enumerate() {
            let nonce = self.next_nonce();
            part.reserve(FRAG_LEN0);
            part.push(KIND_MSG1_FRAG);
            part.extend_from_slice(&init_id.to_le_bytes());
            part.push(part_byte(idx, FRAG_PARTS));
            part.extend_from_slice(slice);
            part.extend_from_slice(&nonce.to_le_bytes());
        }
        out
    }

    /// Consume an answer.
    ///
    /// A repeated answer is not a failure, and neither is an answer that arrives after this attempt
    /// finished: the first one established the session and the later copies carry nothing new. A
    /// **crypto failure or a malformed answer leaves this attempt unusable** — `snow` has consumed
    /// its handshake state by then — so the caller must replace the attempt rather than retry the
    /// rung with it. What must never happen is the old behaviour: a *timeout* discarding the
    /// ephemeral, so that the answer which finally lands cannot be decrypted.
    pub fn answer(&mut self, msg2: &[u8]) -> Result<Answer, HandshakeError> {
        let content = msg2_content(msg2)?;
        if let Some(previous) = self.answered_with.as_deref() {
            return Ok(if previous == content {
                Answer::Repeated
            } else {
                Answer::Ignored
            });
        }
        let inner = self.inner.take().ok_or(HandshakeError::Malformed)?;
        let established = inner.finish(content)?;
        self.answered_with = Some(content.to_vec());
        Ok(Answer::Finished(established))
    }
}

/// Which part of the initiation this is, and how many there are, in one byte.
///
/// `(part_idx << 4) | part_count`, so a two-part initiation is `0x02` then `0x12`. The low nibble is
/// the count rather than the index so that a part arriving alone still says how many to expect.
#[must_use]
pub const fn part_byte(part_idx: usize, part_count: usize) -> u8 {
    ((part_idx as u8) << 4) | (part_count as u8 & 0x0F)
}

/// A parsed initiation fragment.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Part<'a> {
    /// The identifier both parts of one initiation share.
    pub init_id: u16,
    /// Which part this is, from the low nibble's sibling: `part_byte >> 4`.
    pub index: u8,
    /// How many parts the initiation has.
    pub count: u8,
    /// This part's slice of the 121 bytes.
    pub slice: &'a [u8],
}

/// Parse one `KIND_MSG1_FRAG` record, with or without its trailing nonce.
///
/// Both lengths are accepted on purpose: a peer that predates the nonce on fragments is still
/// readable, and the nonce is not part of what is reassembled.
#[must_use]
pub fn parse_part(record: &[u8]) -> Option<Part<'_>> {
    if record.first().copied() != Some(KIND_MSG1_FRAG) {
        return None;
    }
    let init_id = u16::from_le_bytes([*record.get(1)?, *record.get(2)?]);
    let byte = *record.get(3)?;
    let index = byte >> 4;
    let count = byte & 0x0F;
    let body = record.get(FRAG_HEADER..)?;
    // The trailing nonce, when present, is the last two bytes and is not part of the slice.
    let with_nonce = body.len().checked_sub(NONCE_LEN)?;
    let slice = if with_nonce == FRAG_SLICE0 || with_nonce == FRAG_SLICE1 {
        &body[..with_nonce]
    } else {
        body
    };
    Some(Part {
        init_id,
        index,
        count,
        slice,
    })
}

/// The 50-byte Noise content of an answer, with or without its trailing nonce.
fn msg2_content(msg2: &[u8]) -> Result<&[u8], HandshakeError> {
    if msg2.len() == MSG2_NONCED_LEN || msg2.len() == MSG2_CONTENT_LEN {
        let content = &msg2[..MSG2_CONTENT_LEN];
        if content.first().copied() == Some(KIND_MSG2) {
            return Ok(content);
        }
    }
    Err(HandshakeError::Malformed)
}

/// Append a fresh nonce to an answer the cache can only produce once.
fn answer_with_nonce(content: &[u8], nonce: u16) -> Vec<u8> {
    let mut out = Vec::with_capacity(MSG2_NONCED_LEN);
    out.extend_from_slice(content);
    out.extend_from_slice(&nonce.to_le_bytes());
    out
}

/// What the cache did with an arriving initiation.
pub enum Answered {
    /// A new initiation: a session was built and this is its answer.
    Fresh {
        /// The answer, with a nonce.
        reply: Vec<u8>,
        /// The established session.
        ///
        /// Boxed because a `StatelessTransportState` is a few hundred bytes and every other variant
        /// of this type is two small vectors: without the indirection, the whole enum — and so the
        /// answer a caller holds while it decides — costs every caller the session's size.
        established: Box<Established>,
        /// The phone key to pin.
        remote_static: [u8; 32],
        /// The timestamp to remember against replay.
        timestamp_ms: u64,
        /// Which pairing matched.
        pairing: usize,
        /// The 121 bytes that were accepted, which is what a repeat will match on.
        initiation: [u8; MSG1_CONTENT_LEN],
    },
    /// A repeat of an initiation already answered: the same answer, a fresh nonce, nothing rebuilt.
    Cached {
        /// The answer, with a nonce.
        reply: Vec<u8>,
    },
    /// One part of a two-part initiation; the other has not arrived yet.
    Held,
}

impl core::fmt::Debug for Answered {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Fresh { reply, initiation, .. } => f
                .debug_struct("Fresh")
                .field("reply_len", &reply.len())
                .field("init_id", &u16::from_le_bytes([initiation[1], initiation[2]]))
                .finish_non_exhaustive(),
            Self::Cached { reply } => f
                .debug_struct("Cached")
                .field("reply_len", &reply.len())
                .finish(),
            Self::Held => f.write_str("Held"),
        }
    }
}

impl core::fmt::Debug for Answer {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Finished(established) => f
                .debug_struct("Finished")
                .field("index", &established.index)
                .finish_non_exhaustive(),
            Self::Repeated => f.write_str("Repeated"),
            Self::Ignored => f.write_str("Ignored"),
        }
    }
}

/// Why an initiation was not answered.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Refusal {
    /// The shape is not an initiation at all.
    Malformed,
    /// This peer was answered too recently.
    RateLimited {
        /// How long until it may be answered again.
        retry_after_ms: u64,
    },
    /// A replayed initiation that is not the live one. Ignored **without touching the live
    /// session**, which is the whole point: a captured initiation must not be able to end a
    /// session that is working.
    Superseded,
    /// The cache is full of initiations still inside the replay window, so a new one cannot be
    /// remembered and is therefore not answered either.
    CacheFull,
    /// No pairing's PSK produced the filter tag.
    BadFilter,
    /// The timestamp is outside the window around now.
    Stale,
    /// The timestamp is older than the last accepted one for this pairing, by more than the retry
    /// tolerance.
    Replay,
    /// The PSK matched but a different phone key presented it.
    PinMismatch,
    /// The exchange failed in the cryptography itself.
    Crypto,
}

impl From<HandshakeError> for Refusal {
    fn from(e: HandshakeError) -> Self {
        match e {
            HandshakeError::Malformed => Self::Malformed,
            HandshakeError::BadFilter => Self::BadFilter,
            HandshakeError::Stale => Self::Stale,
            HandshakeError::Replay => Self::Replay,
            HandshakeError::PinMismatch => Self::PinMismatch,
            HandshakeError::Crypto => Self::Crypto,
        }
    }
}

impl Refusal {
    /// One sentence, for the journal line an operator reads.
    #[must_use]
    pub fn describe(self) -> String {
        match self {
            Self::Malformed => "not an initiation".to_owned(),
            Self::RateLimited { retry_after_ms } => {
                format!("answered too recently, {retry_after_ms} ms to wait")
            }
            Self::Superseded => "replay of an initiation the live session replaced".to_owned(),
            Self::CacheFull => "too many initiations inside the replay window".to_owned(),
            Self::BadFilter => "unknown or wrong pre-shared key".to_owned(),
            Self::Stale => "timestamp outside the window (is the phone's clock right?)".to_owned(),
            Self::Replay => "replayed or out-of-order timestamp".to_owned(),
            Self::PinMismatch => "a different phone key presented the pairing's key".to_owned(),
            Self::Crypto => "cryptographic failure".to_owned(),
        }
    }
}

/// One answered initiation, kept so a repeat costs an answer and not a session.
struct Entry {
    content: [u8; MSG1_CONTENT_LEN],
    answer: Vec<u8>,
    remote_static: [u8; 32],
    /// Only the initiation behind the live session may be answered from the cache. An older one is
    /// a replay, and answering it would tell a captured packet where the phone is.
    live: bool,
}

/// The parts of one unfinished initiation, joined by `init_id`.
struct Pending {
    init_id: u16,
    count: u8,
    first_seen_ms: u64,
    slices: [Option<Vec<u8>>; FRAG_PARTS],
}

/// The responder's memory: what it answered, and what it must not answer again.
///
/// Three jobs, each of them a defect that was found in the field:
///
/// * **Idempotence.** A duplicate initiation returns the cached answer and touches nothing else: no
///   new session, no index advance, no outbox clear, no `fsync`. Before this, three copies of one
///   initiation built three sessions with three indices, and the phone and the host agreed only
///   when the arrival order happened to line up (`RC-5`).
/// * **Fragments.** A `KIND_MSG1_FRAG` pair is joined by `init_id` and never mixed with another
///   attempt's part.
/// * **Rate.** One answer per peer per [`ANSWER_RATE_MS`], so a captured initiation cannot be
///   replayed as a transmission amplifier.
pub struct AnswerCache {
    entries: Vec<(Entry, u64)>,
    pending: Vec<Pending>,
    peers: Vec<([u8; 32], u64)>,
    capacity: usize,
    window_ms: u64,
    rate_ms: u64,
    nonce: u16,
}

impl AnswerCache {
    /// A cache of `capacity` initiations, each remembered for `window_ms`.
    #[must_use]
    pub fn new(capacity: usize, window_ms: u64) -> Self {
        Self {
            entries: Vec::new(),
            pending: Vec::new(),
            peers: Vec::new(),
            capacity: capacity.max(1),
            window_ms,
            rate_ms: ANSWER_RATE_MS,
            nonce: 0,
        }
    }

    /// The plan's own size: 64 initiations, five minutes — the handshake replay window.
    #[must_use]
    pub fn plan_default() -> Self {
        Self::new(64, handshake::TIMESTAMP_WINDOW_MS)
    }

    /// Change how often one peer may be answered. Tests drive the edges with it; a deployment has
    /// no reason to.
    #[must_use]
    pub fn with_rate(mut self, rate_ms: u64) -> Self {
        self.rate_ms = rate_ms;
        self
    }

    /// Forget everything, as a new pairing grant must.
    pub fn clear(&mut self) {
        self.entries.clear();
        self.pending.clear();
        self.peers.clear();
    }

    /// How many initiations are remembered, for a status line.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether nothing is remembered.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    fn next_nonce(&mut self) -> u16 {
        self.nonce = self.nonce.wrapping_add(1);
        self.nonce
    }

    /// Answer an arriving initiation record, of any of the three forms.
    ///
    /// `now_ms` is monotonic (rate limits and pending fragments); `wall_ms` is Unix time (the
    /// handshake timestamp window). `index` is the session index to assign a **fresh** initiation.
    pub fn answer(
        &mut self,
        responder: &Responder,
        pairings: &[PairingEntry],
        index: u8,
        now_ms: u64,
        wall_ms: u64,
        record: &[u8],
    ) -> Result<Answered, Refusal> {
        let content = match record.first().copied() {
            Some(KIND_MSG1) => initiation_content(record).ok_or(Refusal::Malformed)?,
            Some(KIND_MSG1_FRAG) => match self.join_fragment(now_ms, record)? {
                Some(content) => content,
                None => return Ok(Answered::Held),
            },
            _ => return Err(Refusal::Malformed),
        };
        self.answer_content(responder, pairings, index, now_ms, wall_ms, content)
    }

    /// Join one fragment into its attempt, returning the initiation once both parts are in.
    fn join_fragment(
        &mut self,
        now_ms: u64,
        record: &[u8],
    ) -> Result<Option<[u8; MSG1_CONTENT_LEN]>, Refusal> {
        let part = parse_part(record).ok_or(Refusal::Malformed)?;
        if usize::from(part.count) != FRAG_PARTS || part.index as usize >= FRAG_PARTS {
            return Err(Refusal::Malformed);
        }
        // Drop attempts whose other half is old enough to belong to a different ladder.
        self.pending
            .retain(|p| now_ms.saturating_sub(p.first_seen_ms) <= self.window_ms);
        if self.pending.len() >= self.capacity {
            return Err(Refusal::CacheFull);
        }
        let at = match self.pending.iter().position(|p| p.init_id == part.init_id) {
            Some(at) => at,
            None => {
                self.pending.push(Pending {
                    init_id: part.init_id,
                    count: part.count,
                    first_seen_ms: now_ms,
                    slices: [None, None],
                });
                self.pending.len() - 1
            }
        };
        let pending = self.pending.get_mut(at).ok_or(Refusal::Malformed)?;
        if pending.count != part.count {
            // The same id from an attempt that disagrees about its own length is not a duplicate.
            return Err(Refusal::Malformed);
        }
        let slot = pending
            .slices
            .get_mut(part.index as usize)
            .ok_or(Refusal::Malformed)?;
        // A repeated part is harmless: the first copy is as good as the second.
        if slot.is_none() {
            *slot = Some(part.slice.to_vec());
        }
        if pending.slices.iter().any(Option::is_none) {
            return Ok(None);
        }
        let joined = self.pending.swap_remove(at);
        let mut content = [0u8; MSG1_CONTENT_LEN];
        let mut at = 0;
        for slice in joined.slices.iter().flatten() {
            let end = at + slice.len();
            content
                .get_mut(at..end)
                .ok_or(Refusal::Malformed)?
                .copy_from_slice(slice);
            at = end;
        }
        if at != MSG1_CONTENT_LEN {
            return Err(Refusal::Malformed);
        }
        Ok(Some(content))
    }

    fn answer_content(
        &mut self,
        responder: &Responder,
        pairings: &[PairingEntry],
        index: u8,
        now_ms: u64,
        wall_ms: u64,
        content: [u8; MSG1_CONTENT_LEN],
    ) -> Result<Answered, Refusal> {
        self.entries
            .retain(|(_, seen)| wall_ms.abs_diff(*seen) <= self.window_ms);
        self.peers
            .retain(|(_, at)| now_ms.saturating_sub(*at) <= self.rate_ms);

        if let Some(at) = self.entries.iter().position(|(e, _)| e.content == content) {
            let entry = self.entries.get(at).ok_or(Refusal::Malformed)?;
            if !entry.0.live {
                return Err(Refusal::Superseded);
            }
            let peer = entry.0.remote_static;
            if let Some(retry_after_ms) = self.rate_limited(now_ms, &peer) {
                return Err(Refusal::RateLimited { retry_after_ms });
            }
            let answer = entry.0.answer.clone();
            let nonce = self.next_nonce();
            self.note_answer(now_ms, peer);
            // The entry's age is when it was **accepted**, not when it was last answered: refreshing
            // it here would let a captured initiation keep itself remembered forever.
            return Ok(Answered::Cached {
                reply: answer_with_nonce(&answer, nonce),
            });
        }

        // A fresh initiation. The replay window and the pin are the handshake's own rules and are
        // not re-implemented here: `accept` is the one place that may refuse for either.
        let accepted = responder
            .accept(pairings, wall_ms, &content, index)
            .map_err(Refusal::from)?;
        let peer = accepted.remote_static;
        if let Some(retry_after_ms) = self.rate_limited(now_ms, &peer) {
            return Err(Refusal::RateLimited { retry_after_ms });
        }
        if self.entries.len() >= self.capacity && !self.evict_one() {
            return Err(Refusal::CacheFull);
        }
        for (entry, _) in &mut self.entries {
            entry.live = false;
        }
        let answer = accepted.reply.clone();
        self.entries.push((
            Entry {
                content,
                answer,
                remote_static: peer,
                live: true,
            },
            wall_ms,
        ));
        let nonce = self.next_nonce();
        self.note_answer(now_ms, peer);
        Ok(Answered::Fresh {
            reply: answer_with_nonce(&accepted.reply, nonce),
            established: Box::new(accepted.established),
            remote_static: peer,
            timestamp_ms: accepted.timestamp_ms,
            pairing: accepted.pairing,
            initiation: content,
        })
    }

    /// Drop the oldest entry that is not the live session's. Returns whether one was dropped.
    ///
    /// The live entry is the one an attacker would most like evicted — its removal would make the
    /// next replay look fresh and rebuild the session — so it is never the one chosen.
    fn evict_one(&mut self) -> bool {
        let Some(at) = self.entries.iter().position(|(e, _)| !e.live) else {
            return false;
        };
        self.entries.remove(at);
        true
    }

    fn rate_limited(&self, now_ms: u64, peer: &[u8; 32]) -> Option<u64> {
        let (_, at) = self.peers.iter().find(|(p, _)| p == peer)?;
        let elapsed = now_ms.saturating_sub(*at);
        (elapsed < self.rate_ms).then(|| self.rate_ms - elapsed)
    }

    fn note_answer(&mut self, now_ms: u64, peer: [u8; 32]) {
        match self.peers.iter_mut().find(|(p, _)| *p == peer) {
            Some((_, at)) => *at = now_ms,
            None => self.peers.push((peer, now_ms)),
        }
    }

    /// Whether an accepted initiation is the live session's, for a status line.
    #[must_use]
    pub fn is_live(&self, content: &[u8]) -> bool {
        self.entries
            .iter()
            .any(|(e, _)| e.live && e.content.as_slice() == content)
    }
}

/// The 121-byte content of an initiation, with or without its trailing nonce.
fn initiation_content(record: &[u8]) -> Option<[u8; MSG1_CONTENT_LEN]> {
    if record.len() != MSG1_CONTENT_LEN && record.len() != MSG1_NONCED_LEN {
        return None;
    }
    record[..MSG1_CONTENT_LEN].try_into().ok()
}

/// `KIND_NEED_HANDSHAKE` — the host asking the phone to start again.
///
/// ```text
/// [kind 0x15][wall_ms: u64 LE][HMAC-SHA-256(psk, "NEED_HS" || host_pub || wall_ms)[..16]]
/// ```
///
/// It exists for one situation: the daemon restarted, its sessions are gone, and the phone still
/// believes it holds one. Without it the phone sends transport records into nothing until its own
/// silence ladder expires — minutes of air spent discovering that the peer is ready to talk.
///
/// The tag is keyed by the pairing's PSK and covers the host's static public key, so a stranger can
/// neither forge the request nor replay one from another host.
pub const NEED_HANDSHAKE_LEN: usize = 1 + 8 + 16;
const NEED_HANDSHAKE_LABEL: &[u8] = b"NEED_HS";

type HmacSha256 = Hmac<Sha256>;

/// Why a `KIND_NEED_HANDSHAKE` record was refused.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum NeedHandshakeError {
    /// The wrong length, or not this kind.
    Malformed,
    /// The tag did not verify under this pairing's PSK.
    BadTag,
    /// The timestamp is further from now than the window allows.
    Stale,
}

/// Build a `KIND_NEED_HANDSHAKE` record.
#[must_use]
pub fn need_handshake_encode(
    psk: &[u8; 32],
    host_public: &[u8; 32],
    wall_ms: u64,
) -> [u8; NEED_HANDSHAKE_LEN] {
    let mut mac = HmacSha256::new_from_slice(psk).expect("hmac accepts any key length");
    mac.update(NEED_HANDSHAKE_LABEL);
    mac.update(host_public);
    mac.update(&wall_ms.to_le_bytes());
    let tag = mac.finalize().into_bytes();
    let mut out = [0u8; NEED_HANDSHAKE_LEN];
    out[0] = crate::direction::KIND_NEED_HANDSHAKE;
    out[1..9].copy_from_slice(&wall_ms.to_le_bytes());
    out[9..].copy_from_slice(&tag[..16]);
    out
}

/// Check a `KIND_NEED_HANDSHAKE` record, returning the timestamp it carried.
pub fn need_handshake_verify(
    psk: &[u8; 32],
    host_public: &[u8; 32],
    now_ms: u64,
    window_ms: u64,
    record: &[u8],
) -> Result<u64, NeedHandshakeError> {
    if record.len() != NEED_HANDSHAKE_LEN
        || record.first().copied() != Some(crate::direction::KIND_NEED_HANDSHAKE)
    {
        return Err(NeedHandshakeError::Malformed);
    }
    let wall_ms = u64::from_le_bytes(
        record[1..9]
            .try_into()
            .map_err(|_| NeedHandshakeError::Malformed)?,
    );
    let mut mac = HmacSha256::new_from_slice(psk).expect("hmac accepts any key length");
    mac.update(NEED_HANDSHAKE_LABEL);
    mac.update(host_public);
    mac.update(&wall_ms.to_le_bytes());
    mac.verify_truncated_left(&record[9..])
        .map_err(|_| NeedHandshakeError::BadTag)?;
    if now_ms.abs_diff(wall_ms) > window_ms {
        return Err(NeedHandshakeError::Stale);
    }
    Ok(wall_ms)
}

/// Emits at most one `KIND_NEED_HANDSHAKE` per [`ANSWER_RATE_MS`].
///
/// The host calls this when a transport record arrives with no session behind it. It is deliberately
/// not a timer: a host with nobody talking to it says nothing, which is what keeps an idle link free.
#[derive(Debug, Default)]
pub struct NeedHandshakeSender {
    last_ms: Option<u64>,
}

impl NeedHandshakeSender {
    /// A sender that has not emitted yet.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The record to send now, or `None` when one already went out inside the window.
    pub fn emit(
        &mut self,
        psk: &[u8; 32],
        host_public: &[u8; 32],
        now_ms: u64,
        wall_ms: u64,
    ) -> Option<[u8; NEED_HANDSHAKE_LEN]> {
        if let Some(last) = self.last_ms
            && now_ms.saturating_sub(last) < ANSWER_RATE_MS
        {
            return None;
        }
        self.last_ms = Some(now_ms);
        Some(need_handshake_encode(psk, host_public, wall_ms))
    }
}

