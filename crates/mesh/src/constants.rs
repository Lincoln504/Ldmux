//! The numbers and the words the radio route shares.
//!
//! These are not a wire format: the host's frames are the companion protocol's
//! (`crates/companion`), and the phone's are the terminal protocol's
//! (`crates/protocol/src/mesh_term.rs`). What is here is what both ends of the session loop must
//! agree on — how large a record may be, how many may be in flight, and what a radio state is
//! called. `mesh_term.rs` publishes the first two to the app, so a change here that the app does
//! not hear about is caught by that fixture rather than by a phone retrying `attach`.

/// Records held while the radio is unavailable, and the credit window.
///
/// Eight is the number of records the ARQ may have outstanding at once; it is also the L2 slot
/// count the daemon reports, so the two cannot disagree.
pub const CREDIT_WINDOW: u8 = 8;

/// Absolute ceiling for an opaque record: 172 bytes with an empty path.
///
/// A raw send is `[0x41][priority][header][path_len][path][record]` inside the companion frame's 176
/// bytes. **The real limit is per peer and follows the path**, because the packet carries the path
/// too: 172 − path bytes, capped by [`RECORD_RECEIVED_MAX`]. It is advertised on bind and revised
/// when the path changes ([`crate::carrier::Event::Limit`]), which is what keeps a full record
/// buildable through a repeater — a fixed 172 was refused by our own packet builder at the first
/// path byte.
pub const RECORD_MAX: usize = 172;

/// The largest record a radio will **hand up as a receive-log push**: 171, one less than the wire
/// budget.
///
/// `MyMesh::logRxRaw` refuses a frame it cannot fit in `MAX_FRAME_SIZE` 176 —
/// `if (_serial->isConnected() && len + 3 <= MAX_FRAME_SIZE)` (`examples/companion_radio/MyMesh.cpp:287`)
/// — where `len` is the received packet: 1 header + 1 path byte + the record (a direct packet's
/// path has been consumed by the repeaters that carried it). So a 172-byte record is transmitted,
/// received, and **never reported upward**, which is silent and repeats on every full record.
///
/// This is the receiver's ceiling. The sender's is `RECORD_MAX - path`, because the packet it builds
/// carries the path too; the limit that is advertised to the session is therefore
/// `min(RECORD_RECEIVED_MAX, RECORD_MAX - path)`.
pub const RECORD_RECEIVED_MAX: usize = 171;

/// The smallest record limit ldmux will try to send on, and it is a **sanity check, not a path
/// test**: the firmware's longest legal path is 64 bytes, so the limit can never legitimately fall
/// below `RECORD_MAX - 64` = 108. It used to be 160, which is what a path of 12 bytes leaves —
/// six two-byte hops — so every path past six hops made the phone refuse to send anything at all
/// and report "no usable path to host" on a link that was working.
pub const RECORD_FLOOR: usize = 108;

/// `rssi` meaning "no reading" (`0x80` on the wire, signed).
pub const UNKNOWN_RSSI: i8 = -128;

/// `batteryPercent` meaning "not reported".
pub const UNKNOWN_BATTERY: u8 = 0xFF;

/// The radio's own state, as the carrier reports it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum RadioState {
    Connected,
    Disconnected,
    RadioLost,
    NoContact,
    Unknown(u8),
}

impl RadioState {
    #[must_use]
    pub fn code(self) -> u8 {
        match self {
            Self::Connected => 0,
            Self::Disconnected => 1,
            Self::RadioLost => 2,
            Self::NoContact => 3,
            Self::Unknown(code) => code,
        }
    }

    #[must_use]
    pub fn from_code(code: u8) -> Self {
        match code {
            0 => Self::Connected,
            1 => Self::Disconnected,
            2 => Self::RadioLost,
            3 => Self::NoContact,
            other => Self::Unknown(other),
        }
    }
}

/// What the radio did with a record. Not delivery.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SendOutcome {
    SentDirect,
    SentFlood,
    ErrNotFound,
    ErrBusy,
    ErrTooLong,
    LocalTimeout,
    Unknown(u8),
}

impl SendOutcome {
    #[must_use]
    pub fn code(self) -> u8 {
        match self {
            Self::SentDirect => 1,
            Self::SentFlood => 2,
            Self::ErrNotFound => 3,
            Self::ErrBusy => 4,
            Self::ErrTooLong => 5,
            Self::LocalTimeout => 6,
            Self::Unknown(code) => code,
        }
    }

    #[must_use]
    pub fn from_code(code: u8) -> Self {
        match code {
            1 => Self::SentDirect,
            2 => Self::SentFlood,
            3 => Self::ErrNotFound,
            4 => Self::ErrBusy,
            5 => Self::ErrTooLong,
            6 => Self::LocalTimeout,
            other => Self::Unknown(other),
        }
    }

    /// True only when the radio accepted the record.
    #[must_use]
    pub fn accepted(self) -> bool {
        matches!(self, Self::SentDirect | Self::SentFlood)
    }
}
