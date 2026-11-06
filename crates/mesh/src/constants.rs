//! Wire constants. The authority is `docs/ARCHITECTURE.md#4-the-wire`; this file is the
//! transcription, and the Dart package `meshcore_gateway` is the reference
//! implementation of it.

/// Frame magic, ASCII "MG". Identifies the service before any field is read.
pub const MAGIC: [u8; 2] = [0x4D, 0x47];

/// Contract version this codec implements.
pub const VERSION: u8 = 1;

/// Bytes before the payload: magic 2 + version 1 + type 1 + flags 1 + length 2.
pub const HEADER_BYTES: usize = 7;

/// Largest payload. Above the largest record, below anything that would make a
/// desynchronised length allocate wildly.
pub const MAX_PAYLOAD: usize = 512;

/// Records held while the radio is unavailable, and the credit window.
pub const CREDIT_WINDOW: u8 = 8;

/// Absolute ceiling for an opaque record. The real limit is advertised per
/// peer in `BIND_OK`, because a raw send is `[0x19][path_len][path][payload]`
/// inside a 176-byte companion frame.
pub const RECORD_MAX: usize = 172;

/// The conservative floor: a long path still fits a record this size.
pub const RECORD_FLOOR: usize = 160;

/// Largest record on the flooded discovery path.
pub const DISCOVERY_RECORD_MAX: usize = 142;

/// Pairing token size in bytes.
pub const PAIRING_TOKEN_BYTES: usize = 16;

/// `rssi` meaning "no reading" (`0x80` on the wire, signed).
pub const UNKNOWN_RSSI: i8 = -128;

/// `batteryPercent` meaning "not reported".
pub const UNKNOWN_BATTERY: u8 = 0xFF;

/// Capability bit: the gateway enforces the credit window.
pub const CAP_RECV_CREDIT: u8 = 1 << 0;

/// Capability bit: the gateway pushes `STATUS` when the radio state changes.
pub const CAP_STATUS_PUSH: u8 = 1 << 1;

/// L2 frame types. Values are frozen; an unknown type is ignored, not fatal.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum FrameType {
    Hello,
    HelloOk,
    HelloErr,
    Pair,
    PairOk,
    PairErr,
    Bind,
    BindOk,
    BindErr,
    Record,
    Recv,
    SendResult,
    Status,
    Close,
    Unknown(u8),
}

impl FrameType {
    #[must_use]
    pub fn code(self) -> u8 {
        match self {
            Self::Hello => 0x01,
            Self::HelloOk => 0x02,
            Self::HelloErr => 0x03,
            Self::Pair => 0x10,
            Self::PairOk => 0x11,
            Self::PairErr => 0x12,
            Self::Bind => 0x20,
            Self::BindOk => 0x21,
            Self::BindErr => 0x22,
            Self::Record => 0x30,
            Self::Recv => 0x31,
            Self::SendResult => 0x32,
            Self::Status => 0x40,
            Self::Close => 0x60,
            Self::Unknown(code) => code,
        }
    }

    #[must_use]
    pub fn from_code(code: u8) -> Self {
        match code {
            0x01 => Self::Hello,
            0x02 => Self::HelloOk,
            0x03 => Self::HelloErr,
            0x10 => Self::Pair,
            0x11 => Self::PairOk,
            0x12 => Self::PairErr,
            0x20 => Self::Bind,
            0x21 => Self::BindOk,
            0x22 => Self::BindErr,
            0x30 => Self::Record,
            0x31 => Self::Recv,
            0x32 => Self::SendResult,
            0x40 => Self::Status,
            0x60 => Self::Close,
            other => Self::Unknown(other),
        }
    }

    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::Hello => "hello",
            Self::HelloOk => "hello_ok",
            Self::HelloErr => "hello_err",
            Self::Pair => "pair",
            Self::PairOk => "pair_ok",
            Self::PairErr => "pair_err",
            Self::Bind => "bind",
            Self::BindOk => "bind_ok",
            Self::BindErr => "bind_err",
            Self::Record => "record",
            Self::Recv => "recv",
            Self::SendResult => "send_result",
            Self::Status => "status",
            Self::Close => "close",
            Self::Unknown(_) => "unknown",
        }
    }
}

/// Radio state carried by `STATUS` and `HELLO_OK`.
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

/// Why a connection is being closed.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum CloseReason {
    Protocol,
    Version,
    Busy,
    PreAuthTimeout,
    Shutdown,
    Unknown(u8),
}

impl CloseReason {
    #[must_use]
    pub fn code(self) -> u8 {
        match self {
            Self::Protocol => 1,
            Self::Version => 2,
            Self::Busy => 3,
            Self::PreAuthTimeout => 4,
            Self::Shutdown => 5,
            Self::Unknown(code) => code,
        }
    }

    #[must_use]
    pub fn from_code(code: u8) -> Self {
        match code {
            1 => Self::Protocol,
            2 => Self::Version,
            3 => Self::Busy,
            4 => Self::PreAuthTimeout,
            5 => Self::Shutdown,
            other => Self::Unknown(other),
        }
    }
}

/// Why a `HELLO` was refused.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum HelloError {
    VersionMismatch,
    Busy,
    Unknown(u8),
}

impl HelloError {
    #[must_use]
    pub fn from_code(code: u8) -> Self {
        match code {
            1 => Self::VersionMismatch,
            2 => Self::Busy,
            other => Self::Unknown(other),
        }
    }
}

/// Why a `PAIR` was refused.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PairError {
    BadToken,
    AlreadyPaired,
    Revoked,
    Unknown(u8),
}

impl PairError {
    #[must_use]
    pub fn from_code(code: u8) -> Self {
        match code {
            1 => Self::BadToken,
            2 => Self::AlreadyPaired,
            3 => Self::Revoked,
            other => Self::Unknown(other),
        }
    }
}

/// Why a `BIND` was refused.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum BindError {
    /// The peer is not a MeshCore contact on the gateway's radio.
    NotAContact,
    /// There is no direct path to the peer yet.
    NoPath,
    Unknown(u8),
}

impl BindError {
    #[must_use]
    pub fn from_code(code: u8) -> Self {
        match code {
            1 => Self::NotAContact,
            2 => Self::NoPath,
            other => Self::Unknown(other),
        }
    }
}
