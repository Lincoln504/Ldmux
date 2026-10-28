//! The L3 session: what runs end to end between the phone and the daemon, on top of the
//! opaque-record tunnel (`ldmux-mesh`). Nothing here knows a radio, a preset, a path, a
//! contact or an airtime figure; the only numbers it is given are the record size the
//! gateway advertised and the count of records the gateway says may still be sent.
//!
//! Three layers, each testable without the others:
//!
//! - [`handshake`]: Noise `IKpsk2_25519_ChaChaPoly_SHA256` plus the PSK filter tag.
//! - [`record`]: the 6 + 2 + 16 byte record layout and the 64-record replay window.
//! - [`arq`]: ordering, acknowledgement and bounded retransmission. A pure state machine.
//!
//! [`session::Session`] composes the last two over an [`handshake::Established`] keypair.
#![forbid(unsafe_code)]

pub mod arq;
pub mod handshake;
pub mod record;
pub mod session;

pub use handshake::{Established, HandshakeError, Initiator, Keypair, PairingEntry, Responder};
pub use session::{Inbound, Session, SessionError};
