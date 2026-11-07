//! The host's own MeshCore companion radio, over USB serial.
//!
//! This crate exists because the daemon used to reach the host's radio through a
//! separate **Dart** gateway process on loopback, so the radio route needed a second
//! language, a second repository and a hand-configured process before it could carry a
//! byte. That gateway is gone: the daemon now opens the serial port itself.
//!
//! Here instead: the daemon opens the serial port itself, speaks the companion
//! protocol, and owns the radio. No gateway, no socket, no token, no state file,
//! no Dart.
//!
//! Layout, bottom up:
//!
//! - [`framing`] — `0x3C`/`0x3E` + `u16` little-endian, and a decoder that
//!   resynchronises over the firmware's console output instead of reading it as a
//!   length.
//! - [`link`] — the byte pipe: a real serial port, and a scripted one for tests.
//! - [`codec`] — `SELF_INFO` and `DEVICE_INFO`, plus the raw packet this crate
//!   builds for a send (`[0x3E][path byte][path][record]`).
//! - [`session`] — one command at a time, with a deadline, and pushes kept apart
//!   from replies.
//!
//! Everything here is synchronous on purpose. The protocol is a serial
//! conversation with one question in flight; an async wrapper would add a state
//! machine to something whose correct behaviour is "send, wait, answer". The
//! daemon can call it from a blocking task.

pub mod codec;
pub mod framing;
pub mod link;
pub mod session;

pub use codec::{
    APP_TARGET_VERSION, AdvertPath, DeviceInfo, RAW_CUSTOM_DIRECT_HEADER, SelfInfo, build_raw_direct,
    codes, commands, parse_advert_path,
};
pub use framing::{FrameReader, MAX_PAYLOAD, encode};
pub use link::{ByteLink, MemoryLink, SerialLink};
pub use session::{Companion, SessionError, error_meaning};
