//! The shared vocabulary of the radio route.
//!
//! The daemon owns the host's MeshCore radio over USB serial (`crates/companion`), and the
//! phone owns its own over Bluetooth LE. Nothing in between is a gateway: there is no loopback
//! process, no pairing token and no second copy of the frame format. What is left in this crate
//! is the vocabulary both sides of the daemon's session loop speak:
//!
//!   * [`carrier`] — the seam between the session loop and whatever carries its records, so the
//!     loop never names a device;
//!   * [`constants`] — the record limits and the radio-state vocabulary, the numbers the phone
//!     must agree with (`crates/protocol/src/mesh_term.rs` publishes them to the app); and
//!   * [`messages`] — the two reports a carrier hands upward, a send result and a status.
//!
//! It deliberately contains **no radio knowledge**. No frequency, bandwidth, spreading factor,
//! coding rate, path hash, contact or preset arithmetic appears here; the companion crate owns
//! that. A grep for `lora`, `spread` or `airtime` in this crate finds only comments explaining
//! the absence.

#![forbid(unsafe_code)]

pub mod carrier;
pub mod constants;
pub mod frame;
pub mod messages;

pub use carrier::{Bound, Budget, Carrier, Event};
pub use constants::{
    CREDIT_WINDOW, RECORD_FLOOR, RECORD_MAX, RadioState, SendOutcome, UNKNOWN_BATTERY,
    UNKNOWN_RSSI,
};
pub use frame::MeshError;
pub use messages::{SendResult, Status};
