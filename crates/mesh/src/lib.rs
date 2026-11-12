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
//!     must agree with (`crates/protocol/src/mesh_term.rs` publishes them to the app);
//!   * [`messages`] — the reports a carrier hands upward: a send result, a status, and the flood
//!     scope this node broadcasts under;
//!   * [`airtime`] — how long a packet occupies the channel, which is the arithmetic every budget,
//!     deadline and forward delay in the route is derived from;
//!   * [`budget`] — how many record bytes fit in one packet on a given carrier, derived from the
//!     companion's two frame ceilings rather than remembered;
//!   * [`route`] — the peer's path and the ranking that chooses between copies of it; and
//!   * [`companion`] — the command window: one command in flight, settled only by its own answer.
//!
//! It still contains **no device knowledge**: no frequency, no preset register, no path hash, no
//! contact. Those belong to the companion crate, which owns a radio. What is here is what *both*
//! ends must compute identically — a record limit, a radio state, and a time on air — because a
//! host and a phone that disagreed about any of them would disagree about how long a retry may take
//! and what an hour buys, and neither would be able to tell.

#![forbid(unsafe_code)]

pub mod airtime;
pub mod budget;
pub mod carrier;
pub mod companion;
pub mod constants;
pub mod error;
pub mod messages;
pub mod route;

/// A deterministic simulated MeshCore mesh, so radio behaviour is proved without a radio.
///
/// Behind a feature because it is a test instrument: a shipping binary must not carry a simulator.
/// It lives here because it models this crate's own wire — the packet layout, the deduplication
/// hash, the airtime arithmetic — and a copy anywhere else would drift from the real thing.
#[cfg(any(test, feature = "sim"))]
pub mod sim;

pub use airtime::{airtime_ms, Modulation};
pub use budget::RecordBudget;
pub use carrier::{Bound, Budget, Carrier, Event};
pub use constants::{
    CREDIT_WINDOW, RECORD_FLOOR, RECORD_MAX, RadioState, SendOutcome, UNKNOWN_BATTERY,
    UNKNOWN_RSSI,
};
pub use error::MeshError;
pub use messages::{ScopeInfo, SendResult, Status};
pub use route::{Candidate, Carriage, Proof, ProofWitness, RouteTable};
