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
    started_ms: u64,
    pub dropped: u32,
}

impl Session {
    /// `record_limit` is `BIND_OK`'s advertised size, so the layout is sized to what the radio
    /// will actually accept.
    pub fn new(est: Established, record_limit: usize, now_ms: u64) -> Result<Self, SessionError> {
        let app = record::application_bytes(record_limit.min(ldmux_mesh::constants::RECORD_MAX));
        if app <= arq::HEADER {
            return Err(SessionError::RecordLimitTooSmall);
        }
        Ok(Self {
            est,
            counter: 0,
            replay: ReplayWindow::default(),
            arq: Arq::new(Config::for_payload(app - arq::HEADER)),
            record_limit,
            started_ms: now_ms,
            dropped: 0,
        })
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

    /// Sealed records to hand to the tunnel now: at most `min(records_left, l2_slots)`.
    ///
    /// **Every call seals afresh, retransmissions included.** The ARQ hands back the same plaintext
    /// when it retries, and sealing it again with the next nonce produces different bytes on the
    /// wire. That is what keeps a retry alive on this mesh: MeshCore radios deduplicate by payload,
    /// so a byte-identical retry is dropped by the first relay that forwarded the original and a
    /// multi-hop transfer stalls at its tail. Caching the sealed bytes to "retransmit cheaply"
    /// would save one AEAD operation and break delivery.
    pub fn poll(&mut self, now_ms: u64, limits: Limits) -> Result<Vec<Vec<u8>>, SessionError> {
        let mut sealed = Vec::new();
        for o in self.arq.poll(now_ms, limits) {
            sealed.push(self.seal(&o)?);
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
