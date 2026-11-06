//! The seam between the session loop and whatever carries its records.
//!
//! The daemon's session loop used to name [`MeshClient`] directly: it dialled a
//! loopback gateway, handshook, and then read [`Frame`](crate::frame::Frame)s.
//! That made the gateway the only possible carrier, so a host with a radio on its
//! own serial port still needed a second process — and that process is written in
//! Dart, in another repository.
//!
//! A carrier is whatever can hand one record to the air, deliver one record upward,
//! and report the radio's state. The gateway is one implementation; the daemon's
//! serial companion (`crates/companion`) is the other. Everything above this trait
//! — the L3 session, the record budget, the retry ladder — is written once and is
//! unaware of which one it has.

use crate::client::MeshClient;
use crate::constants::{CAP_STATUS_PUSH, FrameType, RadioState};
use crate::frame::MeshError;
use crate::messages::{self, SendResult, Status};

/// What a carrier reports upward.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Event {
    /// A status report: radio state, signal, battery, airtime and the budget left.
    Status(Status),
    /// One downlink record. Opaque: the session layer decides what it means.
    Recv(Vec<u8>),
    /// The answer to one [`Carrier::send_record`], naming the sequence it carried.
    SendResult(SendResult),
}

/// What a carrier promised at connect time.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Bound {
    /// The largest record this peer's path allows. Records are sized to it.
    pub uplink_record_max: u8,
    pub radio_state: RadioState,
    /// Who decides how much may be sent this hour.
    pub budget: Budget,
}

/// Where the hourly record budget comes from.
///
/// The session loop gates what it sends on a number, and there are two honest sources for it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Budget {
    /// The far end (a gateway) reports how many records are left, and a gateway that has said
    /// nothing has allowed nothing - so an unknown number means zero.
    Reported,
    /// The radio we own governs its own airtime: its firmware runs the transmitter's duty cycle
    /// and refuses a record it will not send. There is no second meter to apply here.
    ///
    /// Reading the absent gateway number as zero is what left a serial host able to send one
    /// probe record a minute and answer nothing else (measured 2026-11-05: `ldmux mesh status`
    /// read `budget 0 records left this hour` while a paired phone retried `attach` in a loop).
    RadioGoverned,
}

/// What the session loop needs from a carriage.
///
/// One command at a time, no retries and no reconnection: those belong to the
/// layer that owns the session, because only it knows whether a record was already
/// handed to the radio.
#[allow(async_fn_in_trait)]
pub trait Carrier {
    /// Hands one record to the carrier, tagged with the sequence its answer will
    /// carry. Success means "accepted for transmission", never "delivered".
    async fn send_record(&mut self, seq: u8, record: &[u8]) -> Result<(), MeshError>;

    /// Tune the radio to a tuple, where the carriage is a radio this process owns.
    ///
    /// A gateway does not answer this: its radio belongs to another process, and a command it
    /// cannot carry must say so rather than pretend. The default is that refusal.
    async fn set_preset(&mut self, freq_khz: u32, bw_hz: u32, sf: u8, cr: u8) -> Result<(), MeshError> {
        let _ = (freq_khz, bw_hz, sf, cr);
        Err(MeshError::Unsupported("this carriage does not own its radio".to_owned()))
    }

    /// The next thing the carrier has to say. Waits; wrap in a deadline if the
    /// caller needs one.
    async fn recv(&mut self) -> Result<Event, MeshError>;
}

/// The loopback gateway, as a carrier.
pub struct GatewayCarrier {
    client: MeshClient,
}

impl GatewayCarrier {
    /// Connect the socket. No frame is sent yet.
    ///
    /// Separate from [`Self::bind`] so the caller keeps one deadline per phase, as
    /// it had when the handshake lived in the session loop: a gateway that accepts
    /// the socket and then goes quiet is a different failure from one that refuses
    /// to answer `HELLO`, and the two names are what `mesh status` shows.
    pub async fn open(addr: &str) -> Result<Self, MeshError> {
        Ok(Self {
            client: MeshClient::connect(addr).await?,
        })
    }

    /// Complete `HELLO`/`PAIR`/`BIND` and learn what this peer allows.
    pub async fn bind(&mut self, token: &[u8], peer: &[u8]) -> Result<Bound, MeshError> {
        let handshake = self.client.handshake(token, peer, CAP_STATUS_PUSH).await?;
        Ok(Bound {
            uplink_record_max: handshake.bind.uplink_record_max,
            radio_state: handshake.hello.radio_state,
            // A gateway is the one that knows how much airtime it will grant.
            budget: Budget::Reported,
        })
    }

    /// `CLOSE(shutdown)`, as courtesy: the gateway treats an abrupt drop the same.
    pub async fn close(self) {
        self.client.close().await;
    }
}

impl Carrier for GatewayCarrier {
    async fn send_record(&mut self, seq: u8, record: &[u8]) -> Result<(), MeshError> {
        self.client.send_record(seq, record).await
    }

    async fn recv(&mut self) -> Result<Event, MeshError> {
        // Frames that carry nothing for the session — a late `PAIR_OK`, an unknown
        // type from a newer gateway — are skipped here rather than surfaced, so the
        // loop above sees only the three events it acts on. `CLOSE` is the one
        // frame that is an error, because it ends the connection.
        loop {
            let frame = self.client.recv().await?;
            match frame.kind {
                FrameType::Status => return Ok(Event::Status(messages::read_status(&frame)?)),
                FrameType::Recv => return Ok(Event::Recv(messages::read_recv(&frame)?.0)),
                FrameType::SendResult => {
                    return Ok(Event::SendResult(messages::read_send_result(&frame)?));
                }
                FrameType::Close => {
                    return Err(MeshError::Refused("close", messages::read_close(&frame)?.code()));
                }
                _ => {}
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::constants::SendOutcome;

    #[test]
    fn bound_carries_the_two_things_the_loop_needs() {
        let bound = Bound { uplink_record_max: 172, radio_state: RadioState::Connected, budget: Budget::Reported };
        assert_eq!(bound.uplink_record_max, 172);
        assert_eq!(bound.radio_state, RadioState::Connected);
    }

    #[test]
    fn an_event_can_name_a_received_record_and_a_send_result() {
        // The two values the loop routes on, built the way a carrier builds them, so
        // a change to either type shows up here rather than in the loop.
        let recv = Event::Recv(vec![1, 2, 3]);
        assert_eq!(recv, Event::Recv(vec![1, 2, 3]));
        let result = Event::SendResult(SendResult { seq: 7, outcome: SendOutcome::SentDirect });
        match result {
            Event::SendResult(r) => assert_eq!(r.seq, 7),
            other => panic!("wrong event: {other:?}"),
        }
    }
}
