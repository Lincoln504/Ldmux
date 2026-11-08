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
use crate::messages::{ScopeInfo, SendResult, Status};

/// What a carrier reports upward.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Event {
    /// A status report: radio state, signal, battery, airtime and the budget left.
    Status(Status),
    /// One downlink record. Opaque: the session layer decides what it means.
    Recv(Vec<u8>),
    /// The answer to one [`Carrier::send_record`], naming the sequence it carried.
    SendResult(SendResult),
    /// The **largest record this link can carry now**, after the path it is using.
    ///
    /// The record budget is not a constant: a raw send is `[0x41][priority][header][path][record]`
    /// in a 176-byte frame, so every path byte costs a record byte, and a 172-byte record is refused
    /// by the builder as soon as one repeater is in the path. The session's record size is fixed at
    /// the handshake, so a carrier that learns a longer path must say so: this event re-sizes it.
    ///
    /// Sent when the path is learned and whenever it changes materially, and it carries the hop
    /// count beside the size because the two are one fact: a field test is read as "12 hops, so 148
    /// bytes", and a status that printed only the size left the operator guessing whether the route
    /// was a metre or a county away.
    Limit { record_max: u8, hops: u8 },
    /// The **flood scope** this node's broadcasts are scoped to, or `None` when they are unscoped.
    ///
    /// Sent at open and whenever it changes — which on this carrier happens once, when a radio with
    /// no scope of its own overhears a scoped flood and recognises the region. Nothing else in the
    /// system can report it: the phone compares its own radio against this one, and two ends on
    /// different scopes hear each other's *direct* records but cannot discover each other at all,
    /// because the discovery floods are dropped by the first repeater that does not carry the scope.
    Scope(Option<ScopeInfo>),
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
/// One answer today, and it is written out rather than assumed: **the carrier computes the hour and
/// publishes it as [`Status::ops_left_this_hour`], and the loop spends what it is told.** For the
/// serial radio that figure is derived from the tuple (`mesh_serial::budget_per_hour`), which is what
/// makes the 1 % a European band requires enforceable at all: the firmware's own limiter is a 50 %
/// duty cycle (`Dispatcher::getAirtimeBudgetFactor()` returns 1.0), so it is a ceiling far above the
/// band rather than the meter.
///
/// **The loop must never gate on a budget that was never reported.** Reading silence as zero is what
/// once left a serial host able to send one probe record a minute and answer nothing (measured
/// 2026-11-05: `ldmux mesh status` read `budget 0 records left this hour` while a paired phone
/// retried `attach` in a loop).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Budget {
    /// The carrier is the meter: it computes the hour and reports what is left, and
    /// [`Status::ops_left_this_hour`](crate::messages::Status::ops_left_this_hour) is what the session
    /// loop hands to the ARQ.
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

    /// Set the radio's path hash width, in **bytes** (1..=3), and report the firmware's answer.
    ///
    /// A channel is not only its four numbers. On MeshCore's public presets the width is the
    /// network's own (one byte, so a repeater's hash is one byte), and a node that names its
    /// neighbours in two bytes cannot read a path a public repeater built. The default is the same
    /// refusal as [`Carrier::set_preset`]: a carriage that does not own its radio must say so.
    /// Set the radio's transmit power, in dBm. `Err(Unsupported)` when this carriage is not the
    /// radio's owner.
    async fn set_tx_power(&mut self, dbm: i8) -> Result<(), MeshError> {
        let _ = dbm;
        Err(MeshError::Unsupported(
            "this carriage does not own its radio".to_owned(),
        ))
    }

    async fn set_path_width(&mut self, bytes: u8) -> Result<(), MeshError> {
        let _ = bytes;
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
