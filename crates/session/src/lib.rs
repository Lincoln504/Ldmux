//! The L3 session: what runs end to end between the phone and the daemon, on top of the
//! opaque-record tunnel (`ldmux-mesh`). Nothing here knows a radio, a preset, a path, a
//! contact or an airtime figure; the only numbers it is given are the record size the peer
//! allows and the count of records the carrier says may still be sent.
//!
//! Four layers, each testable without the others:
//!
//! - [`direction`]: which end may send which record kind, and which arriving packets are ours to
//!   read at all — the self-echo filter (`RC-3`), ahead of any decryption.
//! - [`handshake`]: Noise `IKpsk2_25519_ChaChaPoly_SHA256` plus the PSK filter tag.
//! - [`bootstrap`]: one initiation held across every retry, its fragments, the responder's
//!   idempotent answer cache, and the `KIND_NEED_HANDSHAKE` request.
//! - [`record`]: the 6 + 2 + 16 byte record layout and the 64-record replay window.
//! - [`arq`]: ordering, acknowledgement and bounded retransmission over a byte stream. A pure state
//!   machine, **superseded by [`transaction`]** and removed with the daemon's mesh loop.
//! - [`transaction`]: the message-oriented reliable layer that replaces it: parts cut once, a part
//!   always fittable on the narrower carrier, acknowledgement held inside a burst, and a timeout that
//!   never destroys the session.
//! - [`endpoint`]: the state machine that composes all of the above for one end of the link — the
//!   carriage chosen before a record is sealed, the route updated only after a record authenticates,
//!   and one place for the bootstrap, the transactions and the deadlines.
//! - [`client`]: the phone's whole app protocol in one value — pages, screens and the answers to
//!   commands — so the app stops reimplementing the protocol in Kotlin (`docs/RADIO-PLAN.md` `A2`).
//!
//! [`session::Session`] composes [`record`] and [`arq`] over an [`handshake::Established`] keypair;
//! [`transaction::TransactionLayer`] is the layer a caller should reach for instead.
#![forbid(unsafe_code)]

pub mod arq;
pub mod bootstrap;
pub mod client;
pub mod direction;
pub mod endpoint;
pub mod handshake;
pub mod radio;
pub mod record;
pub mod session;
pub mod transaction;

pub use client::{
    Applied, Client, ClientConfig, ClientError, ClientEvent, ClientStats, MAX_OPEN_PAGES, PageMirror,
};
pub use direction::{Arrival, Side, Verdict, admit};
pub use endpoint::{ArrivalInfo, CarriageSink, EndpointConfig, RadioEndpoint, Role};
pub use handshake::{Established, HandshakeError, Initiator, Keypair, PairingEntry, Responder};
pub use session::{Inbound, Sealed, Session, SessionError};
