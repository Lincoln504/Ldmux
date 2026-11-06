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

/// Absolute ceiling for an opaque record.
///
/// A raw send is `[0x41][path_len][path][payload]` inside the companion frame's 176 bytes, which
/// leaves 172 for the sealed record. The real limit is advertised per peer, because a longer path
/// leaves less room; this is the ceiling no path may exceed.
pub const RECORD_MAX: usize = 172;

/// The conservative floor: a long path still fits a record this size.
pub const RECORD_FLOOR: usize = 160;

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
