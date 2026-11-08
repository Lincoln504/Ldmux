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

/// The **flood scope** this node's broadcasts are proved with, as the carrier found it.
///
/// MeshCore scopes a flood to a region by attaching a 16-bit HMAC of the region's key over the
/// payload, so a repeater that does not carry that region drops the packet silently. The key is
/// `SHA256("#" + name)` cut to 16 bytes for the public regions everybody uses, but a private one
/// (a name beginning `$`) has a stored key, which is why the key travels with the name.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct ScopeInfo {
    /// The region name as the radio stores it, without the leading `#`.
    pub name: String,
    /// The 16-byte transport key.
    pub key: [u8; 16],
}

impl ScopeInfo {
    /// The key as 32 lower-case hex characters, for a pairing code and for `mesh status`.
    #[must_use]
    pub fn key_hex(&self) -> String {
        self.key.iter().map(|b| format!("{b:02x}")).collect()
    }
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
