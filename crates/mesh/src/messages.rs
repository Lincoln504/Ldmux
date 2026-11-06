//! The two reports a carrier hands upward.
//!
//! A carrier answers a record with a [`SendResult`] and describes the radio with a [`Status`].
//! Both are plain values: nothing here parses bytes, because the bytes belong to the carriage
//! (the companion protocol on the host, the terminal protocol's own envelope on the phone).

use crate::constants::RadioState;

/// What the radio did with one record. Not delivery.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct SendResult {
    pub seq: u8,
    pub outcome: crate::constants::SendOutcome,
}

/// A report from the radio: its state, signal, charge, airtime and remaining budget.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Status {
    pub radio_state: RadioState,
    /// Signed; [`crate::constants::UNKNOWN_RSSI`] means no reading.
    pub rssi: i8,
    /// [`crate::constants::UNKNOWN_BATTERY`] means not reported.
    pub battery_percent: u8,
    /// The radio's own transmit airtime this hour, in 0.1 s units.
    pub tx_airtime_tenths_this_hour: u16,
    /// Records of the current uplink size that still fit this hour, when the radio says.
    ///
    /// The host's serial carrier computes it from the tuple the radio reports, and the one-byte
    /// field caps it at 255; `None` means this carrier does not meter at all.
    pub ops_left_this_hour: Option<u8>,
}
