//! The seam between the session loop and the radio that carries its records.
//!
//! A carrier is whatever can hand one record to the air, deliver one record upward, and report
//! the radio's state. Today there is exactly one: the daemon's serial companion
//! (`crate::mesh_serial`, on `crates/companion`), which owns a MeshCore radio on a USB port.
//! Everything above this trait — the L3 session, the credit window, the retry ladder, the
//! bridge — is written once and is unaware of the device.
//!
//! It was two once: a loopback MeshMore gateway was the other, and this seam was introduced so
//! the daemon could stop needing a second process written in another language. The gateway is
//! gone; the seam stayed because it is what keeps the loop testable and the device out of it.

use crate::constants::RadioState;
use crate::error::MeshError;
use crate::messages::{SendResult, Status};

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
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Bound {
    /// The largest record this peer's path allows. Records are sized to it.
    pub uplink_record_max: u8,
    pub radio_state: RadioState,
    /// Who decides how much may be sent this hour.
    pub budget: Budget,
    /// How long one full record takes on the air at the carrier's own tuple, when it knows.
    ///
    /// The session scales its retransmission timers by this: a record at 62.5 kHz and SF7 is
    /// 553 ms, and each repeater adds a random forward delay of up to two airtimes, so a fixed
    /// two-second retry fires while the first copy is still travelling and spends the hour's
    /// allowance on duplicates.
    pub airtime_ms: Option<f64>,
}

/// Where the hourly record budget comes from.
///
/// One answer today, and it is written out rather than assumed: the radio the daemon owns runs
/// its own transmitter duty cycle in firmware and refuses a record it will not send, so the
/// session loop must not apply a second meter on top. Applying one — reading a budget the radio
/// never reported as zero — is what once left a serial host able to send one probe record a
/// minute and answer nothing (measured 2026-11-05: `ldmux mesh status` read `budget 0 records
/// left this hour` while a paired phone retried `attach` in a loop).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Budget {
    /// The radio is the meter. What the loop may send is not gated here.
    RadioGoverned,
}

/// What the session loop needs from a carriage.
///
/// One command at a time, no retries and no reconnection: those belong to the layer that owns
/// the session, because only it knows whether a record was already handed to the radio.
#[allow(async_fn_in_trait)]
pub trait Carrier {
    /// Hands one record to the carrier, tagged with the sequence its answer will carry.
    /// Success means "accepted for transmission", never "delivered".
    async fn send_record(&mut self, seq: u8, record: &[u8]) -> Result<(), MeshError>;

    /// Tune the radio to a tuple.
    ///
    /// A carriage that does not own its radio must say so rather than pretend. The default is
    /// that refusal.
    async fn set_preset(&mut self, freq_khz: u32, bw_hz: u32, sf: u8, cr: u8) -> Result<(), MeshError> {
        let _ = (freq_khz, bw_hz, sf, cr);
        Err(MeshError::Unsupported(
            "this carriage does not own its radio".to_owned(),
        ))
    }

    /// The next thing the carrier has to say. Waits; wrap in a deadline if the caller needs one.
    async fn recv(&mut self) -> Result<Event, MeshError>;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::constants::SendOutcome;

    #[test]
    fn bound_carries_the_two_things_the_loop_needs() {
        let bound = Bound {
            uplink_record_max: 172,
            radio_state: RadioState::Connected,
            budget: Budget::RadioGoverned,
            airtime_ms: Some(69.2),
        };
        assert_eq!(bound.uplink_record_max, 172);
        assert_eq!(bound.radio_state, RadioState::Connected);
        // Timing follows the radio, so the carrier states it once and every layer reads it here.
        assert_eq!(bound.airtime_ms, Some(69.2));
    }

    #[test]
    fn an_event_can_name_a_received_record_and_a_send_result() {
        // The two values the loop routes on, built the way a carrier builds them, so
        // a change to either type shows up here rather than in the loop.
        let recv = Event::Recv(vec![1, 2, 3]);
        assert_eq!(recv, Event::Recv(vec![1, 2, 3]));
        let result = Event::SendResult(SendResult {
            seq: 7,
            outcome: SendOutcome::SentDirect,
        });
        match result {
            Event::SendResult(r) => assert_eq!(r.seq, 7),
            other => panic!("wrong event: {other:?}"),
        }
    }
}
