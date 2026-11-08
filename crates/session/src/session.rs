//! An established session: [`record`](crate::record) sealing and replay protection composed
//! with [`arq`](crate::arq). Still no I/O: bytes in, bytes out.

use crate::arq::{self, Arq, Config, Delivered, Incoming, Limits, Outgoing};
use crate::handshake::Established;
use crate::record::{self, RecordError, ReplayWindow};

/// ADR-0020: a new handshake after 2^16 records or 12 hours.
pub const REKEY_RECORDS: u32 = 1 << 16;
pub const REKEY_AGE_MS: u64 = 12 * 60 * 60 * 1000;

#[derive(Debug, PartialEq, Eq)]
pub enum SessionError {
    Record(RecordError),
    Decode(arq::DecodeError),
    /// The record limit leaves no room for even one payload byte.
    RecordLimitTooSmall,
}

/// One sealed record, and the ARQ sequence number that names it on the wire.
///
/// The sequence number has to travel with the bytes: a `SEND_RESULT` from the radio names the
/// record by the number the *caller* wrote into the send frame, and the only way to say "this one
/// never reached the air" is to know which ARQ slot that number belongs to. Handing back bare
/// bytes forced every caller to keep its own counter and guess the mapping, which is wrong as soon
/// as a retransmission or a handshake reply shares the tunnel.
#[derive(Debug, PartialEq, Eq, Clone)]
pub struct Sealed {
    /// The ARQ slot this record belongs to. `None` for control traffic the session did not
    /// sequence itself (a handshake reply queued by the caller), which has no slot to give back.
    pub seq: Option<u16>,
    /// The sealed bytes, ready to hand to the tunnel.
    pub bytes: Vec<u8>,
    /// **New application data, not a retransmission, an ack or a close.**
    ///
    /// A caller that meters airtime must charge only this: the ARQ lets retransmissions and acks
    /// past its budget (they keep the link alive), so a caller counting every record would spend
    /// the budget on traffic the session did not consider new.
    pub new_data: bool,
}

/// What a received record produced.
#[derive(Debug, PartialEq, Eq)]
pub enum Inbound {
    /// In-order application bytes (possibly several, if one record closed a gap).
    Data(Vec<Delivered>),
    /// Nothing for the caller (a pure ack, or a held out-of-order record).
    Quiet,
    /// The peer ended the session.
    Closed,
}

pub struct Session {
    est: Established,
    counter: u32,
    replay: ReplayWindow,
    arq: Arq,
    record_limit: usize,
    airtime_ms: Option<f64>,
    started_ms: u64,
    pub dropped: u32,
}

impl Session {
    /// `record_limit` is `BIND_OK`'s advertised size, so the layout is sized to what the radio
    /// will actually accept.
    pub fn new(est: Established, record_limit: usize, now_ms: u64) -> Result<Self, SessionError> {
        Self::with_airtime(est, record_limit, now_ms, None)
    }

    /// The same session, with its retransmission timing scaled to the radio's own airtime.
    ///
    /// **A fixed 2 s retry is wrong on a long link.** One record at 62.5 kHz and SF7 is 553 ms,
    /// and every repeater holds a flood or a direct packet for a random delay of up to two
    /// airtimes before it forwards; a round trip over three hops is therefore several seconds. A
    /// timer shorter than the round trip retransmits records the peer already has, which on a 1 %
    /// duty cycle spends the hour's allowance on duplicates. `airtime_ms` is the same figure the
    /// budget is computed from (`time_on_air_ms`), and `None` keeps the interactive defaults for
    /// a link whose airtime nobody measured.
    pub fn with_airtime(
        est: Established,
        record_limit: usize,
        now_ms: u64,
        airtime_ms: Option<f64>,
    ) -> Result<Self, SessionError> {
        let app = record::application_bytes(record_limit.min(ldmux_mesh::constants::RECORD_MAX));
        if app <= arq::HEADER {
            return Err(SessionError::RecordLimitTooSmall);
        }
        Ok(Self {
            est,
            counter: 0,
            replay: ReplayWindow::default(),
            arq: Arq::new(Config::for_payload(app - arq::HEADER).with_airtime(airtime_ms)),
            record_limit,
            airtime_ms,
            started_ms: now_ms,
            dropped: 0,
        })
    }

    /// Re-scale the retransmission timers for the radio's airtime.
    ///
    /// A session is created when the handshake completes, which on the phone happens before the
    /// preset is known to this side; this lets the caller hand over the figure afterwards without
    /// rebuilding the session (which would throw away the keys and the queue).
    pub fn set_airtime(&mut self, airtime_ms: Option<f64>) {
        self.airtime_ms = airtime_ms;
        self.arq.set_config(self.arq.config().with_airtime(airtime_ms));
    }

    /// Re-size the record, because **the path changed**.
    ///
    /// The packet is `[header][path][record]` inside a 176-byte frame, so a repeater's hops come out
    /// of the record budget: a 172-byte record is refused by the builder at the first path byte. The
    /// size is therefore a property of the route, not of the link, and a session that outlives a
    /// route change has to be told. Chunks already queued keep their size — the split happened when
    /// they were queued — so a *shorter* limit only governs what is queued next; a chunk that no
    /// longer fits is refused by the carrier and its attempt given back, which ends in a reconnect
    /// with the right limit.
    pub fn set_record_limit(&mut self, record_limit: usize) {
        let app = record::application_bytes(record_limit.min(ldmux_mesh::constants::RECORD_MAX));
        if app <= arq::HEADER {
            return;
        }
        self.record_limit = record_limit;
        self.arq
            .set_config(Config::for_payload(app - arq::HEADER).with_airtime(self.airtime_ms));
    }

    /// The record limit this session was sized to.
    #[must_use]
    pub fn record_limit(&self) -> usize {
        self.record_limit
    }

    #[must_use]
    pub fn peer_key(&self) -> [u8; 32] {
        self.est.remote_static
    }

    #[must_use]
    pub fn arq(&self) -> &Arq {
        &self.arq
    }

    pub fn arq_mut(&mut self) -> &mut Arq {
        &mut self.arq
    }

    /// Queue bytes for a shell. Sent by [`Self::poll`], within the airtime budget.
    pub fn send(&mut self, shell: u8, bytes: &[u8]) -> Result<(), arq::QueueError> {
        self.arq.send(shell, bytes)
    }

    #[must_use]
    pub fn needs_rekey(&self, now_ms: u64) -> bool {
        self.counter >= REKEY_RECORDS || now_ms.saturating_sub(self.started_ms) >= REKEY_AGE_MS
    }

    /// Process one record from the tunnel. A record that fails any check is counted and
    /// returned as an error; it changes no state.
    pub fn receive(&mut self, now_ms: u64, record: &[u8]) -> Result<Inbound, SessionError> {
        let opened = record::open(
            &self.est.transport,
            self.est.index,
            &mut self.replay,
            record,
        )
        .inspect_err(|_| self.dropped += 1)
        .map_err(SessionError::Record)?;
        let inc = Incoming::decode(opened.flags, opened.shell, &opened.app)
            .inspect_err(|_| self.dropped += 1)
            .map_err(SessionError::Decode)?;
        let kind = inc.kind;
        let delivered = self.arq.on_incoming(now_ms, &inc);
        if kind == arq::Kind::Close {
            return Ok(Inbound::Closed);
        }
        Ok(if delivered.is_empty() {
            Inbound::Quiet
        } else {
            Inbound::Data(delivered)
        })
    }

    /// Sealed records to hand to the tunnel now.
    ///
    /// At most `l2_slots` in total and at most `records_left` of them new; a retransmission rides
    /// on top of the budget because it is already-paid traffic. See [`arq::Arq::poll`].
    ///
    /// **Every call seals afresh, retransmissions included.** The ARQ hands back the same plaintext
    /// when it retries, and sealing it again with the next nonce produces different bytes on the
    /// wire. That is what keeps a retry alive on this mesh: MeshCore radios deduplicate by payload,
    /// so a byte-identical retry is dropped by the first relay that forwarded the original and a
    /// multi-hop transfer stalls at its tail. Caching the sealed bytes to "retransmit cheaply"
    /// would save one AEAD operation and break delivery.
    pub fn poll(&mut self, now_ms: u64, limits: Limits) -> Result<Vec<Vec<u8>>, SessionError> {
        Ok(self
            .poll_with_seq(now_ms, limits)?
            .into_iter()
            .map(|s| s.bytes)
            .collect())
    }

    /// As [`Session::poll`], but each record keeps the ARQ sequence number that names it.
    ///
    /// A caller that receives a send result from the radio must be able to say which record it
    /// refers to, and a refused record must be handed back to [`arq::Arq::on_refused`] by that
    /// number. [`Sealed`] carries it; the plain [`Session::poll`] is the convenience wrapper for
    /// callers that only need the bytes.
    pub fn poll_with_seq(
        &mut self,
        now_ms: u64,
        limits: Limits,
    ) -> Result<Vec<Sealed>, SessionError> {
        let mut sealed = Vec::new();
        for o in self.arq.poll(now_ms, limits) {
            let seq = o.seq;
            sealed.push(Sealed {
                seq: Some(seq),
                bytes: self.seal(&o)?,
                new_data: o.kind == crate::arq::Kind::Data && !o.retransmit,
            });
        }
        Ok(sealed)
    }

    /// Seal one outgoing record under the next nonce. See [`Session::poll`] for why a retry is
    /// sealed again rather than replayed.
    fn seal(&mut self, o: &Outgoing) -> Result<Vec<u8>, SessionError> {
        let rec = record::seal(
            &self.est.transport,
            self.est.index,
            self.counter,
            o.kind.flags(),
            o.shell,
            &o.app_bytes(),
            self.record_limit,
        )
        .map_err(SessionError::Record)?;
        self.counter += 1;
        Ok(rec)
    }
}
