//! The ldmux wire contract: frames, messages, keeper, rpc, pairing, net,
//! paths, socket.
//!
//! Port of `packages/protocol` (the TS source of record). During the port's
//! A/B window the TS tree is unchanged and is the reference; each Rust
//! module names its TS source in its own doc comment. The keeper messages
//! (`keeper`) are new (plan §3.5) and have no TS source.
//!
//! See `docs/RUST-PORT-PLAN.md` (§4 module map) and `docs/RUST-PORT-LOG.md`
//! (phase state).

pub mod frames;
pub mod keeper;
pub mod keeper_call;
pub mod messages;
pub mod net;
pub mod paths;
pub mod pairing;
pub mod rpc;
pub mod socket;
